//! Persistent storage for instance configuration and JWT tokens.
//!
//! Uses an embedded [sled] database at `$XDG_DATA_HOME/concordance/db`
//! (typically `~/.local/share/concordance/db`). Each logical group of data
//! (instances, tokens, metadata) lives in a separate sled tree so keys
//! never collide.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

const INSTANCES_TREE: &str = "instances";
const TOKENS_TREE: &str = "tokens";
const META_TREE: &str = "meta";
const DEFAULT_INSTANCE_KEY: &str = "default_instance";

/// Connection details for a single Ekklesia instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    /// Short name used as a key everywhere (e.g. `"hydra-voting.intersectmbo.org"`).
    pub name: String,
    /// Base URL including scheme (e.g. `"https://hydra-voting.intersectmbo.org"`).
    pub url: String,
}

/// Thread-safe handle to the local sled database.
pub struct Store {
    db: sled::Db,
}

impl Store {
    /// Open the database at the default XDG data-local path.
    pub fn open() -> Result<Self> {
        Self::open_at(db_path())
    }

    /// Open the database at an explicit path. Creates the directory if needed.
    /// Primarily useful in tests.
    pub fn open_at(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)?;
        Ok(Self {
            db: sled::open(path)?,
        })
    }

    // ── Instances ─────────────────────────────────────────────────────────────

    /// Persist a new instance. The first instance added becomes the default.
    pub fn add_instance(&self, config: &InstanceConfig) -> Result<()> {
        let tree = self.db.open_tree(INSTANCES_TREE)?;
        let val = serde_json::to_vec(config)?;
        tree.insert(config.name.as_bytes(), val)?;
        let meta = self.db.open_tree(META_TREE)?;
        if meta.get(DEFAULT_INSTANCE_KEY)?.is_none() {
            meta.insert(DEFAULT_INSTANCE_KEY, config.name.as_bytes())?;
        }
        Ok(())
    }

    /// Retrieve a stored instance by name.
    pub fn get_instance(&self, name: &str) -> Result<InstanceConfig> {
        let tree = self.db.open_tree(INSTANCES_TREE)?;
        tree.get(name.as_bytes())?
            .ok_or_else(|| Error::UnknownInstance(name.to_string()))
            .and_then(|v| serde_json::from_slice(&v).map_err(Into::into))
    }

    /// Return all stored instances in arbitrary order.
    pub fn list_instances(&self) -> Result<Vec<InstanceConfig>> {
        let tree = self.db.open_tree(INSTANCES_TREE)?;
        tree.iter()
            .map(|r| {
                let (_, v) = r?;
                serde_json::from_slice(&v).map_err(Into::into)
            })
            .collect()
    }

    /// Remove an instance and its associated token. Clears the default pointer
    /// if the removed instance was the default.
    pub fn remove_instance(&self, name: &str) -> Result<()> {
        let tree = self.db.open_tree(INSTANCES_TREE)?;
        tree.remove(name.as_bytes())?;
        let tokens = self.db.open_tree(TOKENS_TREE)?;
        tokens.remove(name.as_bytes())?;
        let meta = self.db.open_tree(META_TREE)?;
        if let Some(cur) = meta.get(DEFAULT_INSTANCE_KEY)? {
            if cur.as_ref() == name.as_bytes() {
                meta.remove(DEFAULT_INSTANCE_KEY)?;
            }
        }
        Ok(())
    }

    /// Return the name of the current default instance.
    pub fn default_instance(&self) -> Result<String> {
        let meta = self.db.open_tree(META_TREE)?;
        meta.get(DEFAULT_INSTANCE_KEY)?
            .ok_or(Error::NoDefaultInstance)
            .map(|v| String::from_utf8_lossy(&v).to_string())
    }

    /// Set the default instance. Errors if `name` does not exist.
    pub fn set_default_instance(&self, name: &str) -> Result<()> {
        self.get_instance(name)?;
        let meta = self.db.open_tree(META_TREE)?;
        meta.insert(DEFAULT_INSTANCE_KEY, name.as_bytes())?;
        Ok(())
    }

    // ── Tokens ────────────────────────────────────────────────────────────────

    /// Store a JWT for an existing instance.
    pub fn set_token(&self, instance: &str, jwt: &str) -> Result<()> {
        self.get_instance(instance)?;
        let tree = self.db.open_tree(TOKENS_TREE)?;
        tree.insert(instance.as_bytes(), jwt.as_bytes())?;
        Ok(())
    }

    /// Retrieve the stored JWT for an instance.
    pub fn get_token(&self, instance: &str) -> Result<String> {
        let tree = self.db.open_tree(TOKENS_TREE)?;
        tree.get(instance.as_bytes())?
            .ok_or_else(|| Error::NoToken(instance.to_string()))
            .map(|v| String::from_utf8_lossy(&v).to_string())
    }

    /// Remove the stored JWT for an instance.
    pub fn remove_token(&self, instance: &str) -> Result<()> {
        let tree = self.db.open_tree(TOKENS_TREE)?;
        tree.remove(instance.as_bytes())?;
        Ok(())
    }
}

fn db_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("concordance")
        .join("db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = Store::open_at(dir.path()).unwrap();
        (store, dir)
    }

    fn inst(name: &str) -> InstanceConfig {
        InstanceConfig {
            name: name.to_string(),
            url: format!("https://{name}.example.com"),
        }
    }

    #[test]
    fn add_get_round_trip() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("alpha")).unwrap();
        let got = store.get_instance("alpha").unwrap();
        assert_eq!(got.name, "alpha");
        assert_eq!(got.url, "https://alpha.example.com");
    }

    #[test]
    fn first_instance_becomes_default() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("first")).unwrap();
        store.add_instance(&inst("second")).unwrap();
        assert_eq!(store.default_instance().unwrap(), "first");
    }

    #[test]
    fn set_default_switches_correctly() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("alpha")).unwrap();
        store.add_instance(&inst("beta")).unwrap();
        store.set_default_instance("beta").unwrap();
        assert_eq!(store.default_instance().unwrap(), "beta");
    }

    #[test]
    fn set_default_unknown_errors() {
        let (store, _dir) = tmp_store();
        assert!(store.set_default_instance("ghost").is_err());
    }

    #[test]
    fn remove_instance_clears_default() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("only")).unwrap();
        store.remove_instance("only").unwrap();
        assert!(store.default_instance().is_err());
    }

    #[test]
    fn remove_non_default_leaves_default_intact() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("alpha")).unwrap();
        store.add_instance(&inst("beta")).unwrap();
        store.remove_instance("beta").unwrap();
        assert_eq!(store.default_instance().unwrap(), "alpha");
    }

    #[test]
    fn get_unknown_instance_errors() {
        let (store, _dir) = tmp_store();
        assert!(store.get_instance("ghost").is_err());
    }

    #[test]
    fn no_default_when_empty() {
        let (store, _dir) = tmp_store();
        assert!(store.default_instance().is_err());
    }

    #[test]
    fn token_round_trip() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("host")).unwrap();
        store.set_token("host", "tok.en.value").unwrap();
        assert_eq!(store.get_token("host").unwrap(), "tok.en.value");
    }

    #[test]
    fn set_token_for_unknown_instance_errors() {
        let (store, _dir) = tmp_store();
        assert!(store.set_token("ghost", "tok.en.x").is_err());
    }

    #[test]
    fn get_token_missing_errors() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("host")).unwrap();
        assert!(store.get_token("host").is_err());
    }

    #[test]
    fn remove_token_then_get_errors() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("host")).unwrap();
        store.set_token("host", "tok.en.x").unwrap();
        store.remove_token("host").unwrap();
        assert!(store.get_token("host").is_err());
    }

    #[test]
    fn remove_instance_also_removes_token() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("host")).unwrap();
        store.set_token("host", "tok.en.x").unwrap();
        store.remove_instance("host").unwrap();
        // token tree entry gone too
        let tree = store.db.open_tree(TOKENS_TREE).unwrap();
        assert!(tree.get(b"host").unwrap().is_none());
    }

    #[test]
    fn instance_persists_across_reopen() {
        let dir = TempDir::new().unwrap();
        {
            let store = Store::open_at(dir.path()).unwrap();
            store.add_instance(&inst("persistent")).unwrap();
            store.set_token("persistent", "tok.en.abc").unwrap();
        }
        // Reopen at the same path — data must survive the drop
        let store2 = Store::open_at(dir.path()).unwrap();
        let got = store2.get_instance("persistent").unwrap();
        assert_eq!(got.name, "persistent");
        assert_eq!(store2.get_token("persistent").unwrap(), "tok.en.abc");
        assert_eq!(store2.default_instance().unwrap(), "persistent");
    }

    #[test]
    fn list_instances_returns_all() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("a")).unwrap();
        store.add_instance(&inst("b")).unwrap();
        store.add_instance(&inst("c")).unwrap();
        let mut names: Vec<_> = store
            .list_instances()
            .unwrap()
            .into_iter()
            .map(|i| i.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}
