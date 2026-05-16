//! Property-based tests using proptest.
//!
//! These tests verify invariants that should hold for *any* input, not just
//! the specific cases covered by unit tests.

use concordance::render::title_to_slug;
use proptest::prelude::*;

// ── title_to_slug invariants ──────────────────────────────────────────────────

proptest! {
    /// Slugs must only contain lowercase ASCII letters, digits, and hyphens.
    #[test]
    fn slug_charset_safe_for_any_input(title in ".*") {
        let slug = title_to_slug(&title);
        for ch in slug.chars() {
            prop_assert!(
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-',
                "unexpected char {:?} in slug {:?}", ch, slug
            );
        }
    }

    /// Slugs never start or end with a hyphen.
    #[test]
    fn slug_no_leading_or_trailing_hyphen(title in ".*") {
        let slug = title_to_slug(&title);
        if !slug.is_empty() {
            prop_assert!(!slug.starts_with('-'), "slug starts with hyphen: {:?}", slug);
            prop_assert!(!slug.ends_with('-'), "slug ends with hyphen: {:?}", slug);
        }
    }

    /// Slugs are at most 64 characters long.
    #[test]
    fn slug_max_length(title in ".{0,500}") {
        let slug = title_to_slug(&title);
        prop_assert!(slug.len() <= 64, "slug too long ({} chars): {:?}", slug.len(), slug);
    }

    /// Slugs never contain consecutive hyphens.
    #[test]
    fn slug_no_consecutive_hyphens(title in ".*") {
        let slug = title_to_slug(&title);
        prop_assert!(!slug.contains("--"), "consecutive hyphens in slug: {:?}", slug);
    }

    /// title_to_slug is idempotent: slug(slug(x)) == slug(x).
    #[test]
    fn slug_is_idempotent(title in "[a-zA-Z0-9 :-]{1,100}") {
        let once = title_to_slug(&title);
        let twice = title_to_slug(&once);
        prop_assert_eq!(&once, &twice, "slug not idempotent for input {:?}", title);
    }
}

// ── JWT expiry invariants ─────────────────────────────────────────────────────

proptest! {
    /// Any JWT with exp strictly in the past must be detected as expired.
    #[test]
    fn expired_jwt_always_detected(offset in 1i64..=365 * 24 * 3600) {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use concordance::auth::inspect_jwt;

        let past_exp = chrono::Utc::now().timestamp() - offset;
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(format!(r#"{{"exp":{past_exp}}}"#).as_bytes());
        let token = format!("{header}.{payload}.sig");

        let info = inspect_jwt(&token).unwrap();
        prop_assert!(info.is_expired, "token with past exp should be expired");
        prop_assert!(info.seconds_remaining <= 0);
    }

    /// Any JWT with exp strictly in the future must not be detected as expired.
    #[test]
    fn future_jwt_never_expired(offset in 1i64..=365 * 24 * 3600) {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use concordance::auth::inspect_jwt;

        let future_exp = chrono::Utc::now().timestamp() + offset;
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(format!(r#"{{"exp":{future_exp}}}"#).as_bytes());
        let token = format!("{header}.{payload}.sig");

        let info = inspect_jwt(&token).unwrap();
        prop_assert!(!info.is_expired, "token with future exp should not be expired");
        prop_assert!(info.seconds_remaining > 0);
    }
}
