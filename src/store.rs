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

/// Name of the built-in default instance. Resolved as a code-level fallback
/// by [`Store::default_instance`] and [`Store::get_instance`] when no DB
/// record exists — see [`builtin_default`] for the full rationale.
pub const BUILTIN_DEFAULT_NAME: &str = "hydra-voting.intersectmbo.org";

/// URL the built-in default points to.
pub const BUILTIN_DEFAULT_URL: &str = "https://hydra-voting.intersectmbo.org";

/// Return the `InstanceConfig` for the built-in default — Intersect MBO's
/// Hydra Voting deployment. The fallback is intentionally a function, not
/// a `const`: `InstanceConfig` holds owned `String`s, which can't appear
/// in a `const` context.
///
/// **Why a code-level fallback rather than auto-seeding the sled DB?**
///
/// - The URL ships with each release; changing it is a one-line code
///   change, not a migration.
/// - Users can't `instances remove` the built-in and end up unable to
///   open the CLI — it's always available as a virtual fallback.
/// - No risk of a stale URL persisting across upgrades if Hydra Voting
///   ever changes its public hostname.
/// - Anyone adding a real DB entry with the same name (intentional or
///   accidental) takes precedence: the explicit DB entry wins over the
///   code-level default. See `get_instance` for the lookup order.
pub fn builtin_default() -> InstanceConfig {
    InstanceConfig {
        name: BUILTIN_DEFAULT_NAME.to_string(),
        url: BUILTIN_DEFAULT_URL.to_string(),
    }
}

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

    /// Retrieve an instance by name. Lookup order:
    ///
    /// 1. DB entry (an explicit `instances add` wins over the built-in).
    /// 2. Built-in default — matched only when `name` is exactly
    ///    [`BUILTIN_DEFAULT_NAME`].
    /// 3. Otherwise `Error::UnknownInstance`.
    ///
    /// The fallback lets a fresh install reach Hydra Voting with zero
    /// `instances add` calls, while still allowing testnet / staging
    /// users to override by adding a DB entry with the same name.
    pub fn get_instance(&self, name: &str) -> Result<InstanceConfig> {
        let tree = self.db.open_tree(INSTANCES_TREE)?;
        if let Some(v) = tree.get(name.as_bytes())? {
            return serde_json::from_slice(&v).map_err(Into::into);
        }
        if name == BUILTIN_DEFAULT_NAME {
            return Ok(builtin_default());
        }
        Err(Error::UnknownInstance(name.to_string()))
    }

    /// Return all configured instances in arbitrary order. The DB entries
    /// take precedence; the built-in default is appended only if no DB
    /// entry already uses [`BUILTIN_DEFAULT_NAME`] (so users who add an
    /// explicit override see exactly their override, not two rows).
    pub fn list_instances(&self) -> Result<Vec<InstanceConfig>> {
        let tree = self.db.open_tree(INSTANCES_TREE)?;
        let mut out: Vec<InstanceConfig> = tree
            .iter()
            .map(|r| {
                let (_, v) = r?;
                serde_json::from_slice(&v).map_err(Into::into)
            })
            .collect::<Result<Vec<_>>>()?;
        if !out.iter().any(|i| i.name == BUILTIN_DEFAULT_NAME) {
            out.push(builtin_default());
        }
        Ok(out)
    }

    /// Return the names of instances that have an explicit DB entry
    /// (i.e. excluding the built-in fallback). Used by the CLI to flag
    /// which `instances list` rows are user-added vs. built-in.
    pub fn list_db_instance_names(&self) -> Result<Vec<String>> {
        let tree = self.db.open_tree(INSTANCES_TREE)?;
        tree.iter()
            .map(|r| {
                let (k, _) = r?;
                Ok(String::from_utf8_lossy(&k).to_string())
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
    ///
    /// Lookup order:
    ///   1. The DB-level `default_instance` pointer (set by `instances
    ///      add` on first add, or `instances default <name>` explicitly).
    ///   2. [`BUILTIN_DEFAULT_NAME`] — the code-level fallback so a fresh
    ///      install resolves to Hydra Voting without any setup.
    ///
    /// This method therefore never returns `Error::NoDefaultInstance`,
    /// despite the variant being kept on `Error` for backwards
    /// compatibility (it could still surface from other call sites in
    /// the future). The error variant should be considered effectively
    /// dead code as of v0.3.1.
    pub fn default_instance(&self) -> Result<String> {
        let meta = self.db.open_tree(META_TREE)?;
        if let Some(v) = meta.get(DEFAULT_INSTANCE_KEY)? {
            return Ok(String::from_utf8_lossy(&v).to_string());
        }
        Ok(BUILTIN_DEFAULT_NAME.to_string())
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
    fn remove_instance_clears_default_pointer_falls_back_to_builtin() {
        // Prior to v0.3.1, removing the sole instance erased the default
        // pointer and `default_instance()` errored. With the built-in
        // fallback it now resolves to BUILTIN_DEFAULT_NAME so the binary
        // can never end up unconfigured.
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("only")).unwrap();
        store.remove_instance("only").unwrap();
        assert_eq!(store.default_instance().unwrap(), BUILTIN_DEFAULT_NAME);
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
    fn empty_store_resolves_to_builtin_default() {
        // The whole point of the v0.3.1 built-in: a fresh install reaches
        // Hydra Voting with zero configuration.
        let (store, _dir) = tmp_store();
        assert_eq!(store.default_instance().unwrap(), BUILTIN_DEFAULT_NAME);
        let cfg = store.get_instance(BUILTIN_DEFAULT_NAME).unwrap();
        assert_eq!(cfg.name, BUILTIN_DEFAULT_NAME);
        assert_eq!(cfg.url, BUILTIN_DEFAULT_URL);
    }

    #[test]
    fn explicit_db_entry_wins_over_builtin() {
        // If a user (or a migration) inserts a row with the same name as
        // the built-in but a different URL — e.g. testing a fork — the
        // explicit row should take effect, not the hard-coded URL.
        let (store, _dir) = tmp_store();
        store
            .add_instance(&InstanceConfig {
                name: BUILTIN_DEFAULT_NAME.to_string(),
                url: "https://staging.example.invalid".to_string(),
            })
            .unwrap();
        let cfg = store.get_instance(BUILTIN_DEFAULT_NAME).unwrap();
        assert_eq!(cfg.url, "https://staging.example.invalid");
    }

    #[test]
    fn list_instances_includes_builtin_when_empty() {
        let (store, _dir) = tmp_store();
        let listed = store.list_instances().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, BUILTIN_DEFAULT_NAME);
        assert_eq!(listed[0].url, BUILTIN_DEFAULT_URL);
    }

    #[test]
    fn list_instances_does_not_duplicate_builtin_when_db_has_one() {
        let (store, _dir) = tmp_store();
        store
            .add_instance(&InstanceConfig {
                name: BUILTIN_DEFAULT_NAME.to_string(),
                url: "https://override.example".to_string(),
            })
            .unwrap();
        let listed = store.list_instances().unwrap();
        assert_eq!(listed.len(), 1, "expected single row, got {listed:?}");
        assert_eq!(listed[0].url, "https://override.example");
    }

    #[test]
    fn set_token_works_for_builtin_name_without_explicit_add() {
        // The whole point of issue #1: `auth set --jwt -` against the
        // built-in name must succeed on a fresh install, without the user
        // having to run `instances add` first. set_token validates via
        // get_instance, which now resolves the built-in.
        let (store, _dir) = tmp_store();
        // A structurally valid JWT (signature placeholder); set_token
        // doesn't inspect, the CLI handler does that separately.
        let jwt = "header.payload.sig";
        store.set_token(BUILTIN_DEFAULT_NAME, jwt).unwrap();
        assert_eq!(store.get_token(BUILTIN_DEFAULT_NAME).unwrap(), jwt);
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
    fn list_instances_returns_all_plus_builtin() {
        // The built-in fallback is always present in the listing unless a
        // DB entry already uses its name (see
        // list_instances_does_not_duplicate_builtin_when_db_has_one).
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("a")).unwrap();
        store.add_instance(&inst("b")).unwrap();
        store.add_instance(&inst("c")).unwrap();
        let mut names: Vec<String> = store
            .list_instances()
            .unwrap()
            .into_iter()
            .map(|i| i.name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                BUILTIN_DEFAULT_NAME.to_string(),
            ]
        );
    }

    #[test]
    fn list_db_instance_names_excludes_builtin() {
        let (store, _dir) = tmp_store();
        store.add_instance(&inst("alpha")).unwrap();
        let names = store.list_db_instance_names().unwrap();
        assert_eq!(names, vec!["alpha".to_string()]);
        // Empty DB returns empty — the built-in is virtual.
        let dir = TempDir::new().unwrap();
        let store2 = Store::open_at(dir.path()).unwrap();
        assert!(store2.list_db_instance_names().unwrap().is_empty());
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
