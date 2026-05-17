//! Local user identity for Cardano governance participation.
//!
//! Concordance asks the user for the identity they go by in the Cardano
//! community (name + X handle + Forum username) before any wallet step, and
//! later links their stake address to that identity once they've signed in
//! to a Hydra-Voting instance. This module owns the on-disk representation.
//!
//! Storage: TOML at `$XDG_CONFIG_HOME/concordance/identity.toml` (typically
//! `~/.config/concordance/identity.toml` on Linux,
//! `~/Library/Application Support/concordance/identity.toml` on macOS).
//! Plain text on purpose: the user can read, edit, or remove it without
//! involving Concordance.
//!
//! The signature appended to Hydra-Voting comments is built from the
//! identity fields; see [`Identity::signature`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// User-facing identity for Concordance. Captured at first-run, kept across
/// sessions, and referenced when posting comments via Hydra-Voting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The name the user goes by in the Cardano community.
    pub name: String,
    /// X (formerly Twitter) handle, without leading `@`. Use `"none"` if the
    /// user has no X account they want to associate.
    pub x_handle: String,
    /// Cardano Forum username. Use `"none"` if no Forum account.
    pub cardano_forum_name: String,
    /// Bech32 stake address from the JWT of the configured Hydra-Voting
    /// instance. `None` until the wallet step completes.
    #[serde(default)]
    pub stake_address: Option<String>,
}

impl Identity {
    /// Default on-disk path for the identity file. Creates the parent
    /// directory lazily on save.
    pub fn default_path() -> PathBuf {
        dirs::config_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("concordance")
            .join("identity.toml")
    }

    /// Load identity from the default path. Returns
    /// `Err(Error::NoIdentity)` if the file does not exist.
    pub fn load() -> Result<Self> {
        Self::load_from(Self::default_path())
    }

    /// Load identity from an explicit path. Primarily useful in tests.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NoIdentity);
            }
            Err(e) => return Err(e.into()),
        };
        let identity: Identity =
            toml::from_str(&raw).map_err(|e| Error::Parse(format!("identity.toml: {e}")))?;
        Ok(identity)
    }

    /// Save identity to the default path. Creates the parent directory if
    /// necessary.
    pub fn save(&self) -> Result<()> {
        self.save_to(Self::default_path())
    }

    /// Save identity to an explicit path.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let toml = toml::to_string_pretty(self)
            .map_err(|e| Error::Parse(format!("serialize identity: {e}")))?;
        std::fs::write(path, toml)?;
        Ok(())
    }

    /// The signature block appended to every Hydra-Voting comment posted via
    /// Concordance. The leading `--` is a long-standing email/Usenet
    /// convention indicating a signature delimiter; we keep that for
    /// readability and tool-friendly parsing.
    ///
    /// Each non-final line ends with two trailing spaces — CommonMark's
    /// hard-line-break syntax — so the signature renders as five stacked
    /// rows in Hydra-Voting's Markdown renderer. Without the trailing
    /// spaces, single `\n` newlines collapse to a single space inside a
    /// paragraph, which made the signature show as one run-on line.
    pub fn signature(&self) -> String {
        format!(
            "\n\n--  \n{}  \nX Handle: @{}  \nCardano Forum: {}  \nvia Concordance Feedback Tool",
            self.name, self.x_handle, self.cardano_forum_name,
        )
    }

    /// Suggested public verification post — copy-pasted to X or the Cardano
    /// Forum so others can link the signature back to a real human. The
    /// `{stake_address}` placeholder is substituted with the user's stake
    /// address; `{Hydra Voting Portal URL}` is substituted with the
    /// instance's base URL.
    ///
    /// Returns `Err(Error::NoIdentity)` (with a clarifying message) if the
    /// stake address isn't linked yet.
    pub fn verification_post(&self, portal_url: &str) -> Result<String> {
        let stake = self
            .stake_address
            .as_deref()
            .ok_or_else(|| Error::Parse(
                "stake address not yet linked — run `link_stake_address` after configuring an instance".into()
            ))?;
        Ok(format!(
            "I am providing feedback to the Cardano Budget Process through the Concordance Feedback Tool!  Look for my posts on the Hydra Voting portal associated with this stake address; {stake}\n\n{portal_url}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample(stake: Option<&str>) -> Identity {
        Identity {
            name: "Adam Rusch".to_string(),
            x_handle: "adamrusch".to_string(),
            cardano_forum_name: "adam_rusch".to_string(),
            stake_address: stake.map(str::to_string),
        }
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("identity.toml");
        let id = sample(Some("stake1abc"));
        id.save_to(&path).unwrap();
        let loaded = Identity::load_from(&path).unwrap();
        assert_eq!(loaded, id);
    }

    #[test]
    fn load_missing_file_returns_no_identity_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let err = Identity::load_from(&path).unwrap_err();
        assert!(matches!(err, Error::NoIdentity));
    }

    #[test]
    fn save_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        // nested path that doesn't exist yet
        let path = dir.path().join("a").join("b").join("identity.toml");
        let id = sample(None);
        id.save_to(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn signature_format_matches_spec() {
        let id = sample(None);
        let sig = id.signature();
        assert_eq!(
            sig,
            "\n\n--  \nAdam Rusch  \nX Handle: @adamrusch  \nCardano Forum: adam_rusch  \nvia Concordance Feedback Tool"
        );
    }

    #[test]
    fn signature_uses_markdown_hard_breaks_between_lines() {
        // Two trailing spaces before each interior newline is the CommonMark
        // hard-break syntax. Without this, Hydra-Voting's renderer collapses
        // the five signature rows into one — confirmed empirically in May 2026.
        let sig = sample(None).signature();
        for line in ["--", "Adam Rusch", "X Handle: @adamrusch", "Cardano Forum: adam_rusch"] {
            assert!(
                sig.contains(&format!("{line}  \n")),
                "signature line {line:?} should end with two trailing spaces"
            );
        }
        // The final line must NOT have trailing spaces — nothing follows it.
        assert!(sig.ends_with("via Concordance Feedback Tool"));
    }

    #[test]
    fn verification_post_substitutes_stake_and_portal() {
        let id = sample(Some("stake1u8td6l5sakfcpm6uz85v942xu5"));
        let post = id
            .verification_post("https://hydra-voting.intersectmbo.org")
            .unwrap();
        assert!(post.contains("stake1u8td6l5sakfcpm6uz85v942xu5"));
        assert!(post.contains("https://hydra-voting.intersectmbo.org"));
        assert!(post.contains("Concordance Feedback Tool"));
    }

    #[test]
    fn verification_post_errors_when_stake_unlinked() {
        let id = sample(None);
        let err = id.verification_post("https://example.org").unwrap_err();
        assert!(err.to_string().contains("stake address not yet linked"));
    }

    #[test]
    fn identity_file_is_human_readable_toml() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("identity.toml");
        let id = sample(Some("stake1xyz"));
        id.save_to(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        // Field names should be present unquoted (real TOML, not e.g. JSON)
        assert!(raw.contains("name = "));
        assert!(raw.contains("x_handle = "));
        assert!(raw.contains("cardano_forum_name = "));
        assert!(raw.contains("stake_address = "));
    }

    #[test]
    fn none_handles_are_serialized() {
        // "none" is the documented sentinel for users with no X / no Forum.
        let id = Identity {
            name: "Anon User".into(),
            x_handle: "none".into(),
            cardano_forum_name: "none".into(),
            stake_address: None,
        };
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("identity.toml");
        id.save_to(&path).unwrap();
        let loaded = Identity::load_from(&path).unwrap();
        assert_eq!(loaded.x_handle, "none");
        assert_eq!(loaded.cardano_forum_name, "none");
        assert_eq!(loaded.stake_address, None);
        // signature still renders, just with literal "none"
        let sig = loaded.signature();
        assert!(sig.contains("X Handle: @none"));
        assert!(sig.contains("Cardano Forum: none"));
    }
}
