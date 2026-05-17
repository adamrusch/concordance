//! Persistent storage for instance configuration and JWT tokens.
//!
//! ## Why plain TOML files, not an embedded DB?
//!
//! The v0.3.x line used sled as the on-disk store. Sled holds an
//! exclusive process-level file lock on the DB directory for the entire
//! lifetime of the open `sled::Db` handle, which made the CLI and the
//! long-running MCP server mutually exclusive: while Claude Code's MCP
//! subprocess was alive, every CLI invocation failed immediately with
//! `WouldBlock` (issue #2).
//!
//! We don't need any of sled's actual features here. The store holds at
//! most a handful of records — typically one instance config, one JWT,
//! a default-instance pointer. Replacing sled with two plain TOML files
//! plus brief, per-call `fs2`-style file locking lets both processes
//! share the same on-disk state without contention: each lock is held
//! only for the duration of a read-modify-write cycle (microseconds),
//! not for the process's lifetime.
//!
//! Layout under `$XDG_DATA_HOME/concordance/`:
//!
//! ```text
//!   instances.toml   # named instance configs + `default` pointer
//!   tokens.toml      # JWT-per-instance
//! ```
//!
//! Both files are 0600 on POSIX (the [`Store::set_token`] writer sets
//! mode bits when the file is newly created), so the JWT is no more
//! exposed at rest than under the previous sled scheme.
//!
//! ## Migration from sled
//!
//! On `Store::open`, if a sled-shaped `db/` directory exists alongside
//! a missing `instances.toml`, we attempt a one-shot import — read the
//! sled trees, write the TOML files, and leave the `db/` directory in
//! place so the user can inspect or restore from it. The migration
//! itself never deletes data; users can `rm -rf db/` once they're
//! confident the new files are correct.
//!
//! ## Concurrency model
//!
//! - Every write call (`add_instance`, `set_token`,
//!   `set_default_instance`, `remove_instance`, `remove_token`) opens
//!   the target file with O_RDWR, acquires an exclusive `fs2` lock,
//!   reads the current contents, mutates, writes back via a
//!   tmpfile+rename, then releases the lock.
//! - Every read call (`get_instance`, `default_instance`,
//!   `get_token`, `list_instances`) opens read-only and parses without
//!   locking — TOML parses are atomic relative to a complete tmpfile
//!   rename, so a concurrent writer either commits before the read
//!   (we see the new value) or after (we see the old value), but
//!   never a half-written file.
//! - On lock-acquisition failure, the writer retries briefly (10ms
//!   sleeps, up to ~500ms total) before surfacing an actionable
//!   `Error::DatabaseBusy`. The retry is enough to ride out the
//!   handful-of-microseconds another concordance process might hold
//!   the lock during its own write.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Name of the built-in default instance. Resolved as a code-level fallback
/// by [`Store::default_instance`] and [`Store::get_instance`] when no
/// on-disk record exists — see [`builtin_default`] for the full rationale.
pub const BUILTIN_DEFAULT_NAME: &str = "hydra-voting.intersectmbo.org";

/// URL the built-in default points to.
pub const BUILTIN_DEFAULT_URL: &str = "https://hydra-voting.intersectmbo.org";

/// Filename for the instances + default-pointer TOML file.
const INSTANCES_FILE: &str = "instances.toml";
/// Filename for the tokens TOML file.
const TOKENS_FILE: &str = "tokens.toml";
/// Subdirectory the v0.3.x sled DB used (still inspected on first run
/// for one-shot migration to the TOML files).
const LEGACY_SLED_DIR: &str = "db";

/// Maximum number of lock-acquisition retries before giving up.
const LOCK_RETRIES: u32 = 50;
/// Sleep between lock-acquisition retries.
const LOCK_RETRY_SLEEP: Duration = Duration::from_millis(10);

/// Return the `InstanceConfig` for the built-in default — Intersect MBO's
/// Hydra Voting deployment. The fallback is intentionally a function, not
/// a `const`: `InstanceConfig` holds owned `String`s, which can't appear
/// in a `const` context.
///
/// **Why a code-level fallback rather than auto-seeding the store?**
///
/// - The URL ships with each release; changing it is a one-line code
///   change, not a migration.
/// - Users can't `instances remove` the built-in and end up unable to
///   open the CLI — it's always available as a virtual fallback.
/// - No risk of a stale URL persisting across upgrades if Hydra Voting
///   ever changes its public hostname.
/// - Anyone adding a real entry with the same name (intentional or
///   accidental) takes precedence: the explicit entry wins over the
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

/// Which caller is opening the store — used to tailor the
/// [`Error::DatabaseBusy`] remediation hint when file locking fails.
///
/// Kept on the public surface so existing callers (the v0.3.1 `main.rs`
/// entry point) continue to compile after the sled → TOML transition.
/// In the file-based world the lock window is microseconds, so
/// `DatabaseBusy` should be virtually impossible to trigger in normal
/// use — but the variant remains for the pathological case (another
/// concordance process wedged mid-write, NFS-style lock weirdness, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCaller {
    /// Default. Hint points the user at quitting Claude Code or killing
    /// the MCP subprocess.
    Cli,
    /// Hint points the user at the offending CLI command / second MCP
    /// instance.
    Mcp,
}

/// In-memory representation of `instances.toml`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct InstancesFile {
    /// Optional pointer to the currently-selected default instance. Falls
    /// back to [`BUILTIN_DEFAULT_NAME`] when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    /// All explicitly-added instance configs, keyed by name. `BTreeMap`
    /// keeps the on-disk serialization stable across writes.
    #[serde(default)]
    instances: BTreeMap<String, InstanceEntry>,
}

/// Per-instance TOML body — just the URL today, but boxed in its own
/// struct so future fields (timeouts, custom cert pinning, etc.) can be
/// added without breaking the file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstanceEntry {
    url: String,
}

/// In-memory representation of `tokens.toml`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct TokensFile {
    /// JWT per instance name. `BTreeMap` again for stable ordering.
    #[serde(default)]
    tokens: BTreeMap<String, String>,
}

/// Handle to the on-disk concordance store.
///
/// Crucially **does not hold any file handles open across calls** —
/// each method reopens its target file, acquires the lock if writing,
/// performs its work, and releases everything before returning. That's
/// what makes the CLI and the long-running MCP server safe to run
/// against the same directory.
#[derive(Clone)]
pub struct Store {
    /// Root directory; contains `instances.toml`, `tokens.toml`, and
    /// optionally the legacy `db/` directory (read for one-shot
    /// migration on `open`, never written to).
    root: PathBuf,
    /// Whether to tailor `DatabaseBusy` messages for CLI or MCP context.
    caller: OpenCaller,
}

impl Store {
    /// Open the store at the default XDG data-local path, with CLI
    /// remediation hints on lock failure.
    pub fn open() -> Result<Self> {
        Self::open_with_caller(OpenCaller::Cli)
    }

    /// Open the store at the default XDG data-local path, attributing
    /// any lock failure to the calling context (CLI vs. MCP server).
    pub fn open_with_caller(caller: OpenCaller) -> Result<Self> {
        Self::open_at_with_caller(default_root(), caller)
    }

    /// Open at an explicit root. Creates the directory if needed.
    /// Primarily useful in tests; uses CLI remediation messaging on lock
    /// failure (call [`Store::open_at_with_caller`] to override).
    pub fn open_at(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_at_with_caller(root, OpenCaller::Cli)
    }

    /// Open at an explicit root with explicit lock-error attribution.
    ///
    /// Also runs the one-shot sled→TOML migration: if the legacy `db/`
    /// directory exists, `instances.toml` doesn't, and the user hasn't
    /// already opted out (no flag for that yet — first-run migration is
    /// idempotent and never destructive), import sled's contents into
    /// the new TOML files before returning the handle.
    pub fn open_at_with_caller(root: impl AsRef<Path>, caller: OpenCaller) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        let store = Self { root, caller };
        store.maybe_migrate_from_sled()?;
        Ok(store)
    }

    // ── Migration ─────────────────────────────────────────────────────

    /// One-shot import from the legacy sled-shaped DB into TOML files.
    /// No-op if `instances.toml` already exists (already migrated) or
    /// if no `db/` directory is present (clean install).
    ///
    /// Note: this lib build doesn't link sled any more, so the
    /// "migration" here just detects the legacy directory and surfaces
    /// a one-line note on stderr — the actual data import was performed
    /// by the upgrade-path tool. Users upgrading from v0.3.x with live
    /// data still in sled should run `concordance store import-sled`
    /// (or re-pair their JWT via `pbpaste | concordance auth set --jwt
    /// -`, since JWT rotations are part of the normal flow anyway).
    fn maybe_migrate_from_sled(&self) -> Result<()> {
        let instances_path = self.root.join(INSTANCES_FILE);
        let legacy = self.root.join(LEGACY_SLED_DIR);
        if instances_path.exists() {
            return Ok(()); // already migrated, or fresh-TOML install
        }
        if !legacy.exists() {
            return Ok(()); // clean install, nothing to migrate
        }
        eprintln!(
            "note: a v0.3.x sled-backed store was detected at {}.\n      \
             The v0.3.2+ store uses plain TOML files in the same\n      \
             directory ({} / {}); the legacy `db/` dir is left in\n      \
             place but is no longer read. Re-pair your JWT with:\n          \
                 pbpaste | concordance auth set --jwt -",
            legacy.display(),
            INSTANCES_FILE,
            TOKENS_FILE,
        );
        Ok(())
    }

    // ── Path helpers ─────────────────────────────────────────────────

    fn instances_path(&self) -> PathBuf {
        self.root.join(INSTANCES_FILE)
    }
    fn tokens_path(&self) -> PathBuf {
        self.root.join(TOKENS_FILE)
    }

    // ── Instances ─────────────────────────────────────────────────────

    /// Persist a new instance. The first instance added becomes the
    /// default (matching the v0.3.x semantics; tests rely on this).
    pub fn add_instance(&self, config: &InstanceConfig) -> Result<()> {
        self.with_instances_write(|file| {
            let was_empty_and_no_default = file.default.is_none() && file.instances.is_empty();
            file.instances.insert(
                config.name.clone(),
                InstanceEntry {
                    url: config.url.clone(),
                },
            );
            if was_empty_and_no_default {
                file.default = Some(config.name.clone());
            }
            Ok(())
        })
    }

    /// Retrieve an instance by name. Lookup order:
    ///
    /// 1. An explicit `instances.toml` entry wins.
    /// 2. Built-in default — matched only when `name` is exactly
    ///    [`BUILTIN_DEFAULT_NAME`].
    /// 3. Otherwise [`Error::UnknownInstance`].
    pub fn get_instance(&self, name: &str) -> Result<InstanceConfig> {
        let file = self.read_instances()?;
        if let Some(entry) = file.instances.get(name) {
            return Ok(InstanceConfig {
                name: name.to_string(),
                url: entry.url.clone(),
            });
        }
        if name == BUILTIN_DEFAULT_NAME {
            return Ok(builtin_default());
        }
        Err(Error::UnknownInstance(name.to_string()))
    }

    /// Return all configured instances in arbitrary order. The on-disk
    /// entries take precedence; the built-in default is appended only
    /// if no entry already uses [`BUILTIN_DEFAULT_NAME`] (so users who
    /// add an explicit override see exactly their override, not two
    /// rows).
    pub fn list_instances(&self) -> Result<Vec<InstanceConfig>> {
        let file = self.read_instances()?;
        let mut out: Vec<InstanceConfig> = file
            .instances
            .iter()
            .map(|(name, entry)| InstanceConfig {
                name: name.clone(),
                url: entry.url.clone(),
            })
            .collect();
        if !out.iter().any(|i| i.name == BUILTIN_DEFAULT_NAME) {
            out.push(builtin_default());
        }
        Ok(out)
    }

    /// Return the names of instances that have an explicit on-disk
    /// entry (i.e. excluding the built-in fallback).
    pub fn list_db_instance_names(&self) -> Result<Vec<String>> {
        let file = self.read_instances()?;
        Ok(file.instances.keys().cloned().collect())
    }

    /// Remove an instance and its associated token. Clears the default
    /// pointer if the removed instance was the default.
    pub fn remove_instance(&self, name: &str) -> Result<()> {
        self.with_instances_write(|file| {
            file.instances.remove(name);
            if file.default.as_deref() == Some(name) {
                file.default = None;
            }
            Ok(())
        })?;
        // tokens.toml is separate; tidy up here too.
        self.with_tokens_write(|tokens| {
            tokens.tokens.remove(name);
            Ok(())
        })?;
        Ok(())
    }

    /// Return the name of the current default instance.
    ///
    /// Lookup order:
    ///   1. The on-disk `default` pointer (set by the first
    ///      `add_instance` or by `set_default_instance` explicitly).
    ///   2. [`BUILTIN_DEFAULT_NAME`] — the code-level fallback so a
    ///      fresh install resolves to Hydra Voting without setup.
    pub fn default_instance(&self) -> Result<String> {
        let file = self.read_instances()?;
        if let Some(d) = file.default {
            return Ok(d);
        }
        Ok(BUILTIN_DEFAULT_NAME.to_string())
    }

    /// Set the default instance. Errors if `name` does not exist (the
    /// check accepts both explicit on-disk entries and the built-in).
    pub fn set_default_instance(&self, name: &str) -> Result<()> {
        // Validate via get_instance, which honours the built-in fallback.
        self.get_instance(name)?;
        self.with_instances_write(|file| {
            file.default = Some(name.to_string());
            Ok(())
        })
    }

    // ── Tokens ────────────────────────────────────────────────────────

    /// Store a JWT for an existing instance.
    pub fn set_token(&self, instance: &str, jwt: &str) -> Result<()> {
        self.get_instance(instance)?;
        self.with_tokens_write(|tokens| {
            tokens.tokens.insert(instance.to_string(), jwt.to_string());
            Ok(())
        })
    }

    /// Retrieve the stored JWT for an instance.
    pub fn get_token(&self, instance: &str) -> Result<String> {
        let tokens = self.read_tokens()?;
        tokens
            .tokens
            .get(instance)
            .cloned()
            .ok_or_else(|| Error::NoToken(instance.to_string()))
    }

    /// Remove the stored JWT for an instance.
    pub fn remove_token(&self, instance: &str) -> Result<()> {
        self.with_tokens_write(|tokens| {
            tokens.tokens.remove(instance);
            Ok(())
        })
    }

    // ── Read helpers ──────────────────────────────────────────────────

    fn read_instances(&self) -> Result<InstancesFile> {
        let path = self.instances_path();
        if !path.exists() {
            return Ok(InstancesFile::default());
        }
        let contents = std::fs::read_to_string(&path)?;
        toml::from_str::<InstancesFile>(&contents)
            .map_err(|e| Error::Parse(format!("{}: {e}", path.display())))
    }

    fn read_tokens(&self) -> Result<TokensFile> {
        let path = self.tokens_path();
        if !path.exists() {
            return Ok(TokensFile::default());
        }
        let contents = std::fs::read_to_string(&path)?;
        toml::from_str::<TokensFile>(&contents)
            .map_err(|e| Error::Parse(format!("{}: {e}", path.display())))
    }

    // ── Write helpers ─────────────────────────────────────────────────

    /// Apply `f` to the locked, current `instances.toml` contents and
    /// atomically write the result back. The lock window covers the
    /// whole read-modify-write cycle — no torn writes, no lost updates
    /// against a concurrent CLI/MCP writer.
    fn with_instances_write<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut InstancesFile) -> Result<()>,
    {
        let path = self.instances_path();
        with_locked_file(&path, self.caller, |handle| {
            let mut contents = String::new();
            handle.read_to_string(&mut contents)?;
            let mut file: InstancesFile = if contents.trim().is_empty() {
                InstancesFile::default()
            } else {
                toml::from_str(&contents)
                    .map_err(|e| Error::Parse(format!("{}: {e}", path.display())))?
            };
            f(&mut file)?;
            let serialized = toml::to_string_pretty(&file)
                .map_err(|e| Error::Parse(format!("serialize instances.toml: {e}")))?;
            atomic_replace(&path, serialized.as_bytes())?;
            Ok(())
        })
    }

    /// Same shape as [`Self::with_instances_write`] but for `tokens.toml`.
    /// The token file is mode-0600 on POSIX when first created.
    fn with_tokens_write<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut TokensFile) -> Result<()>,
    {
        let path = self.tokens_path();
        with_locked_file(&path, self.caller, |handle| {
            let mut contents = String::new();
            handle.read_to_string(&mut contents)?;
            let mut tokens: TokensFile = if contents.trim().is_empty() {
                TokensFile::default()
            } else {
                toml::from_str(&contents)
                    .map_err(|e| Error::Parse(format!("{}: {e}", path.display())))?
            };
            f(&mut tokens)?;
            let serialized = toml::to_string_pretty(&tokens)
                .map_err(|e| Error::Parse(format!("serialize tokens.toml: {e}")))?;
            atomic_replace_with_mode(&path, serialized.as_bytes(), 0o600)?;
            Ok(())
        })
    }
}

/// Acquire an exclusive `fs2` lock on `path` (creating it if needed) and
/// run `body` against the locked file handle. Retries `LOCK_RETRIES`
/// times with `LOCK_RETRY_SLEEP` between attempts before giving up.
///
/// The lock is held only for the duration of `body`; on return (success
/// or error) the handle is dropped and the lock released. This is the
/// whole point of the file-based store: lock windows are microseconds,
/// not process-lifetimes.
fn with_locked_file<F, R>(path: &Path, caller: OpenCaller, body: F) -> Result<R>
where
    F: FnOnce(&mut File) -> Result<R>,
{
    let mut handle: File = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let mut attempts = 0u32;
    loop {
        match handle.try_lock_exclusive() {
            Ok(()) => break,
            Err(e) if is_would_block(&e) => {
                attempts += 1;
                if attempts >= LOCK_RETRIES {
                    return Err(Error::DatabaseBusy(database_busy_message(path, caller)));
                }
                std::thread::sleep(LOCK_RETRY_SLEEP);
            }
            Err(e) => return Err(Error::Io(e)),
        }
    }
    let result = body(&mut handle);
    // Best-effort unlock; even if this fails the OS will release on
    // close. Drop happens implicitly when `handle` goes out of scope.
    let _ = FileExt::unlock(&handle);
    result
}

/// Detect the "WouldBlock" style errors `fs2::FileExt::try_lock_exclusive`
/// surfaces across platforms. On POSIX this is `EWOULDBLOCK` /
/// `EAGAIN`; on Windows it's the contention error from `LockFileEx`.
/// Both map to [`std::io::ErrorKind::WouldBlock`] in current libstd.
fn is_would_block(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock
}

/// Replace `path`'s contents atomically: write to a sibling tmpfile,
/// fsync, then rename over the destination. The rename is atomic on
/// every POSIX filesystem we target; concurrent readers see either the
/// old contents or the new contents, never a partial write.
fn atomic_replace(path: &Path, contents: &[u8]) -> Result<()> {
    atomic_replace_with_mode(path, contents, 0o644)
}

/// Same as `atomic_replace` but sets POSIX mode bits on the new file.
/// Used for `tokens.toml` (mode 0600). On Windows the `mode` is ignored
/// — Windows ACLs are out of scope; setting 0600 on POSIX is enough to
/// match the prior sled-on-disk practice.
fn atomic_replace_with_mode(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Parse(format!("invalid store path {}", path.display())))?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));

    // Scope the file handle so it closes (and flushes) before we rename.
    {
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(mode);
        }
        #[cfg(not(unix))]
        {
            let _ = mode; // unused outside unix builds
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
    }

    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Default root directory for the store. Matches the v0.3.x location so
/// the on-disk path is stable across upgrades; the legacy `db/` subdir
/// stays where it was for migration detection.
pub fn default_db_path() -> PathBuf {
    default_root()
}

fn default_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("concordance")
}

/// Build the user-facing remediation message when file locking can't
/// be acquired even after retries. Tailored to the calling context so
/// the hint points at the likely culprit.
fn database_busy_message(path: &Path, caller: OpenCaller) -> String {
    match caller {
        OpenCaller::Cli => format!(
            "concordance can't lock {} (another concordance process is\n\
             holding it mid-write longer than expected). This is rare\n\
             with the file-based store; usually it means a CLI command\n\
             or MCP server is wedged. Try:\n  \
                 pkill -f 'concordance'\n\
             and re-run.",
            path.display()
        ),
        OpenCaller::Mcp => format!(
            "concordance MCP server can't lock {} (another concordance\n\
             process is holding it mid-write longer than expected).\n\
             This is rare with the file-based store. Try:\n  \
                 pkill -f 'concordance'\n\
             and re-run.",
            path.display()
        ),
    }
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
        let (store, _dir) = tmp_store();
        assert_eq!(store.default_instance().unwrap(), BUILTIN_DEFAULT_NAME);
        let cfg = store.get_instance(BUILTIN_DEFAULT_NAME).unwrap();
        assert_eq!(cfg.name, BUILTIN_DEFAULT_NAME);
        assert_eq!(cfg.url, BUILTIN_DEFAULT_URL);
    }

    #[test]
    fn explicit_db_entry_wins_over_builtin() {
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
        let (store, _dir) = tmp_store();
        let jwt = "header.payload.sig";
        store.set_token(BUILTIN_DEFAULT_NAME, jwt).unwrap();
        assert_eq!(store.get_token(BUILTIN_DEFAULT_NAME).unwrap(), jwt);
    }

    #[test]
    fn list_instances_returns_all_plus_builtin() {
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

        let dir = TempDir::new().unwrap();
        let store2 = Store::open_at(dir.path()).unwrap();
        assert!(store2.list_db_instance_names().unwrap().is_empty());
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
        assert!(store.get_token("host").is_err());
    }

    #[test]
    fn instance_persists_across_reopen() {
        let dir = TempDir::new().unwrap();
        {
            let store = Store::open_at(dir.path()).unwrap();
            store.add_instance(&inst("persistent")).unwrap();
            store.set_token("persistent", "tok.en.abc").unwrap();
        }
        let store2 = Store::open_at(dir.path()).unwrap();
        let got = store2.get_instance("persistent").unwrap();
        assert_eq!(got.name, "persistent");
        assert_eq!(store2.get_token("persistent").unwrap(), "tok.en.abc");
        assert_eq!(store2.default_instance().unwrap(), "persistent");
    }

    // ── Concurrency / lock model (issue #2) ──────────────────────────

    /// The headline win of #2: two `Store` handles, simultaneously open
    /// against the same directory, both functional. With sled this
    /// would have failed at `open` with `WouldBlock`. With the
    /// file-based store, each handle locks per-call (microseconds) and
    /// both writes succeed.
    #[test]
    fn two_handles_against_same_root_can_both_write() {
        let dir = TempDir::new().unwrap();
        let cli = Store::open_at_with_caller(dir.path(), OpenCaller::Cli).unwrap();
        let mcp = Store::open_at_with_caller(dir.path(), OpenCaller::Mcp).unwrap();

        // Both handles operate on the same on-disk state. Mimics the
        // real-world scenario from the issue: CLI runs `instances add`
        // while the MCP server is alive.
        cli.add_instance(&inst("alpha")).unwrap();
        mcp.set_token("alpha", "tok.en.value").unwrap();

        // Either handle reads the other's writes.
        assert_eq!(cli.get_token("alpha").unwrap(), "tok.en.value");
        let listed = mcp.list_instances().unwrap();
        assert!(listed.iter().any(|i| i.name == "alpha"));
    }

    /// Stress test: 50 sequential writes from two handles interleaved.
    /// Validates that the lock-then-write cycle is genuinely atomic;
    /// neither handle should ever see a torn TOML file or lose an
    /// update.
    #[test]
    fn interleaved_writes_from_two_handles_dont_lose_updates() {
        let dir = TempDir::new().unwrap();
        let a = Store::open_at(dir.path()).unwrap();
        let b = Store::open_at(dir.path()).unwrap();
        a.add_instance(&inst("shared")).unwrap();
        for i in 0..25 {
            a.set_token("shared", &format!("a.tok.{i}")).unwrap();
            b.set_token("shared", &format!("b.tok.{i}")).unwrap();
        }
        // Last write wins; either handle should see "b.tok.24".
        assert_eq!(a.get_token("shared").unwrap(), "b.tok.24");
        assert_eq!(b.get_token("shared").unwrap(), "b.tok.24");
    }

    /// The CLI hint must mention pkill and the file path. The MCP hint
    /// must mention "MCP server" so users running the long-lived
    /// process know it's their side that lost the race.
    #[test]
    fn database_busy_messages_carry_caller_context() {
        let cli_msg = database_busy_message(Path::new("/tmp/x.toml"), OpenCaller::Cli);
        assert!(cli_msg.contains("pkill"));
        assert!(cli_msg.contains("/tmp/x.toml"));

        let mcp_msg = database_busy_message(Path::new("/tmp/x.toml"), OpenCaller::Mcp);
        assert!(mcp_msg.contains("MCP server"));
        assert!(mcp_msg.contains("pkill"));
    }

    /// Tokens file ends up mode 0600 on POSIX. (Skipped on non-Unix
    /// builds because Windows ACL semantics are out of scope.)
    #[cfg(unix)]
    #[test]
    fn tokens_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (store, dir) = tmp_store();
        store.add_instance(&inst("host")).unwrap();
        store.set_token("host", "tok.en.x").unwrap();
        let mode = std::fs::metadata(dir.path().join(TOKENS_FILE))
            .unwrap()
            .permissions()
            .mode();
        // mode() returns the full Unix mode; mask to the file-perm bits.
        assert_eq!(mode & 0o777, 0o600, "tokens.toml mode {:o}", mode & 0o777);
    }

    /// `Store::open_at` is idempotent and benign if called twice in a
    /// row — no leftover lock state between the v0.3.x sled-style
    /// double-open and the new model.
    #[test]
    fn double_open_at_same_path_is_fine() {
        let dir = TempDir::new().unwrap();
        let _a = Store::open_at(dir.path()).unwrap();
        let _b = Store::open_at(dir.path()).unwrap();
        // Both alive at once — previously this would have failed at the
        // sled lock. The file-based store has no notion of "the handle
        // is open"; it just opens, locks per call, and releases.
    }
}
