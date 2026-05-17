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

/// Which caller is opening the store — used to tailor the
/// [`Error::DatabaseBusy`] remediation hint when sled reports a lock
/// failure. The CLI typically loses the race to a running MCP server;
/// the MCP server typically loses the race to a CLI command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCaller {
    /// Default. Hint points the user at quitting Claude Code or killing
    /// the MCP subprocess.
    Cli,
    /// Hint points the user at the offending CLI command / second MCP
    /// instance.
    Mcp,
}

impl Store {
    /// Open the database at the default XDG data-local path, with CLI
    /// remediation hints on lock failure.
    pub fn open() -> Result<Self> {
        Self::open_with_caller(OpenCaller::Cli)
    }

    /// Open the database at the default XDG data-local path, attributing
    /// any lock failure to the calling context (CLI vs. MCP server).
    pub fn open_with_caller(caller: OpenCaller) -> Result<Self> {
        Self::open_at_with_caller(db_path(), caller)
    }

    /// Open the database at an explicit path. Creates the directory if needed.
    /// Primarily useful in tests; uses CLI remediation messaging on lock
    /// failure (call [`Store::open_at_with_caller`] to override).
    ///
    /// If sled reports a lock failure (another concordance process holds the
    /// DB), this returns [`Error::DatabaseBusy`] with the CLI-flavoured
    /// remediation message.
    pub fn open_at(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_at_with_caller(path, OpenCaller::Cli)
    }

    /// Open the database at an explicit path with explicit lock-error
    /// attribution. The `caller` parameter only affects the wording of the
    /// [`Error::DatabaseBusy`] message on failure; the happy path is
    /// identical for both variants.
    pub fn open_at_with_caller(path: impl AsRef<Path>, caller: OpenCaller) -> Result<Self> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)?;
        match sled::open(path) {
            Ok(db) => Ok(Self { db }),
            Err(e) if is_lock_error(&e) => {
                let msg = match caller {
                    OpenCaller::Cli => database_busy_message_cli(path),
                    OpenCaller::Mcp => database_busy_message_mcp(path),
                };
                Err(Error::DatabaseBusy(msg))
            }
            Err(e) => Err(e.into()),
        }
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

/// Detect sled's "DB is already locked by another process" failure.
///
/// Sled 0.34 wraps the underlying `fs2::FileExt::try_lock_exclusive`
/// WouldBlock error in `sled::Error::Io` with `io::ErrorKind::Other` (NOT
/// `ErrorKind::WouldBlock` as one might expect — sled re-wraps the inner
/// error before surfacing it; see sled-0.34.7/src/config.rs around the
/// `try_lock` helper). We therefore match on the message body, which sled
/// always renders as `"could not acquire lock on ..."` regardless of OS,
/// since that string is the portable invariant across macOS (EAGAIN/35),
/// Linux (EWOULDBLOCK), and Windows.
fn is_lock_error(e: &sled::Error) -> bool {
    match e {
        sled::Error::Io(io) => {
            // Belt-and-suspenders: accept either the (theoretical)
            // WouldBlock kind or the (actual, sled-0.34) message-based form.
            io.kind() == std::io::ErrorKind::WouldBlock
                || io.to_string().contains("could not acquire lock")
        }
        _ => false,
    }
}

/// Remediation message shown when the CLI hits the lock — i.e. the MCP
/// server (long-running, spawned by Claude Code) is the likely culprit.
pub(crate) fn database_busy_message_cli(path: &Path) -> String {
    format!(
        "concordance is already running with the database open (likely the MCP\n\
         server spawned by Claude Code / your MCP client). The on-disk store is\n\
         single-writer per process, so the CLI can't run while another process\n\
         holds the lock at {}.\n\
         Either:\n  \
           - quit Claude Code (or `/mcp` -> disable the `concordance` server),\n    \
             then retry the command\n  \
           - or kill the MCP subprocess and retry:\n        pkill -f 'concordance mcp'\n\
         See https://github.com/adamrusch/concordance/issues/2 for the\n\
         architectural fix that will let CLI and MCP coexist.",
        path.display()
    )
}

/// Remediation message shown when the MCP server hits the lock — i.e. a
/// CLI command (or a second MCP instance) is the likely culprit.
pub(crate) fn database_busy_message_mcp(path: &Path) -> String {
    format!(
        "concordance can't start the MCP server: the database at {} is\n\
         already locked by another concordance process (probably a\n\
         CLI command still running, or another MCP server instance).\n\
         Either wait for the other process to finish, or:\n    \
             pkill -f 'concordance'\n\
         See https://github.com/adamrusch/concordance/issues/2 for the\n\
         architectural fix that will let CLI and MCP coexist.",
        path.display()
    )
}

/// Path the default-location store opens at. Exposed so callers (the
/// MCP-side error remap, tests) can compose remediation messages with
/// the exact path users see in CLI output.
pub fn default_db_path() -> PathBuf {
    db_path()
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

    // ── Lock-failure handling (issue #4) ─────────────────────────────────

    /// When a second `Store::open_at` runs against a path already opened
    /// by another live `Store`, sled fails the lock acquisition and the
    /// CLI gets the [`Error::DatabaseBusy`] variant with the
    /// quit-Claude-Code remediation hint.
    #[test]
    fn double_open_returns_database_busy_with_cli_hint() {
        let dir = TempDir::new().unwrap();
        let _first = Store::open_at(dir.path()).expect("first open succeeds");
        let result = Store::open_at(dir.path());
        let err = match result {
            Ok(_) => panic!("second open must fail while first is alive"),
            Err(e) => e,
        };
        match err {
            Error::DatabaseBusy(msg) => {
                assert!(msg.contains("Claude Code"), "CLI hint missing: {msg}");
                assert!(msg.contains("pkill"), "remediation hint missing: {msg}");
                assert!(msg.contains("issues/2"), "fixme link missing: {msg}");
            }
            other => panic!("expected Error::DatabaseBusy, got {other:?}"),
        }
    }

    /// Same scenario but the second caller is the MCP server: the
    /// message must call out that a CLI command is the likely culprit.
    #[test]
    fn double_open_with_mcp_caller_returns_mcp_flavoured_hint() {
        let dir = TempDir::new().unwrap();
        let _first = Store::open_at(dir.path()).expect("first open succeeds");
        let result = Store::open_at_with_caller(dir.path(), OpenCaller::Mcp);
        let err = match result {
            Ok(_) => panic!("second open must fail while first is alive"),
            Err(e) => e,
        };
        match err {
            Error::DatabaseBusy(msg) => {
                assert!(msg.contains("MCP server"), "MCP hint missing: {msg}");
                assert!(msg.contains("CLI command"), "CLI culprit hint missing: {msg}");
                assert!(msg.contains("issues/2"), "fixme link missing: {msg}");
            }
            other => panic!("expected Error::DatabaseBusy, got {other:?}"),
        }
    }

    /// The lock-error message must be printed cleanly via the `Display`
    /// impl on `Error` — i.e. without the `store error:` prefix that
    /// raw `sled::Error` would carry. This is the user-visible bit:
    /// `eprintln!("error: {e}")` in main() should yield the multi-line
    /// remediation, not a tangled `error: store error: IO error: ...`.
    #[test]
    fn database_busy_displays_without_store_error_prefix() {
        let dir = TempDir::new().unwrap();
        let _first = Store::open_at(dir.path()).expect("first open succeeds");
        let result = Store::open_at(dir.path());
        let err = match result {
            Ok(_) => panic!("second open must fail while first is alive"),
            Err(e) => e,
        };
        let rendered = err.to_string();
        assert!(
            !rendered.starts_with("store error:"),
            "got store-error-prefixed message: {rendered}"
        );
        assert!(rendered.contains("concordance is already running"));
    }
}
