//! Unified error type for the concordance library and CLI.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },

    #[error("JWT expired — fetch a new token from the browser cookie at {instance}")]
    JwtExpired { instance: String },

    #[error("JWT invalid: {0}")]
    JwtInvalid(String),

    #[error(
        "no JWT configured for instance '{0}' — store one with:\n  \
         pbpaste | concordance --instance {0} auth set --jwt -\n  \
         or set $CONCORDANCE_JWT (env), or use `--jwt-file <path>`. See\n  \
         `concordance auth set --help` for the full source-resolution list."
    )]
    NoToken(String),

    #[error("instance '{0}' not found — run: concordance instances add <url> --name {0}")]
    UnknownInstance(String),

    /// Kept for backwards compatibility; as of v0.3.1
    /// `Store::default_instance` always resolves to the built-in fallback
    /// when no DB pointer is set, so this variant is effectively unused
    /// from inside the store itself. Other call sites may still produce
    /// it in the future, so we keep the message accurate.
    #[error("no default instance configured — run: concordance instances add <url>")]
    NoDefaultInstance,

    #[error(
        "no identity configured — set one via the `set_identity` MCP tool, or write {}",
        crate::identity::Identity::default_path().display()
    )]
    NoIdentity,

    /// Generic store error. Kept on the variant list for backwards
    /// compatibility with callers that may have matched on this name;
    /// the v0.3.x sled `#[from]` impl is gone, but a future store
    /// backend may want to emit this.
    #[error("store error: {0}")]
    Store(String),

    /// The on-disk store is locked by another concordance process for
    /// longer than the retry window. With the v0.3.2+ file-based store
    /// this is rare (microsecond locks), but the variant is kept so
    /// callers — CLI vs. MCP server — can render a context-tailored
    /// remediation hint without the `store error:` prefix.
    #[error("{0}")]
    DatabaseBusy(String),

    #[error("bincode error: {0}")]
    Bincode(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
