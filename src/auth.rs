//! JWT inspection and validation.
//!
//! Ekklesia tokens are standard JWTs carried in the `Authorization: Bearer`
//! header and the `token` cookie. We only decode the payload to read the `exp`
//! claim — signature verification is intentionally omitted because the server
//! enforces validity; we just want to detect expiry before making a request.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};

use crate::error::{Error, Result};

/// Decoded metadata from a JWT payload.
#[derive(Debug)]
pub struct TokenInfo {
    /// The `exp` claim decoded as a UTC timestamp.
    pub expires_at: DateTime<Utc>,
    /// True if `exp` is in the past at the time of inspection.
    pub is_expired: bool,
    /// Seconds until expiry; negative if already expired.
    pub seconds_remaining: i64,
}

impl TokenInfo {
    /// Human-readable one-liner for CLI output.
    pub fn status_line(&self) -> String {
        if self.is_expired {
            format!(
                "EXPIRED at {}",
                self.expires_at.format("%Y-%m-%d %H:%M:%S UTC")
            )
        } else {
            let hours = self.seconds_remaining / 3600;
            let mins = (self.seconds_remaining % 3600) / 60;
            format!(
                "valid — expires {} ({}h {}m remaining)",
                self.expires_at.format("%Y-%m-%d %H:%M:%S UTC"),
                hours,
                mins
            )
        }
    }
}

/// Decode a JWT and return expiry metadata without verifying the signature.
///
/// Returns an error if the token is structurally invalid (wrong number of
/// parts, non-base64 payload, missing `exp` claim).
pub fn inspect_jwt(token: &str) -> Result<TokenInfo> {
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() != 3 {
        return Err(Error::JwtInvalid("expected 3 dot-separated parts".into()));
    }

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| Error::JwtInvalid(format!("base64 decode failed: {e}")))?;

    let claims: serde_json::Value = serde_json::from_slice(&payload_bytes)?;

    let exp = claims["exp"]
        .as_i64()
        .ok_or_else(|| Error::JwtInvalid("missing 'exp' claim".into()))?;

    let expires_at = DateTime::from_timestamp(exp, 0)
        .ok_or_else(|| Error::JwtInvalid("invalid exp timestamp".into()))?;

    let now = Utc::now();
    let seconds_remaining = (expires_at - now).num_seconds();

    Ok(TokenInfo {
        expires_at,
        is_expired: seconds_remaining <= 0,
        seconds_remaining,
    })
}

/// Like [`inspect_jwt`] but returns an error if the token is expired.
pub fn require_valid_jwt(token: &str, instance_name: &str) -> Result<()> {
    let info = inspect_jwt(token)?;
    if info.is_expired {
        return Err(Error::JwtExpired {
            instance: instance_name.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal structurally valid JWT with the given `exp` timestamp.
    /// The signature segment is a placeholder — we never verify it.
    fn make_jwt(exp: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"sub":"test","exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.fakesig")
    }

    #[test]
    fn valid_future_token_is_not_expired() {
        let future = Utc::now().timestamp() + 3600;
        let info = inspect_jwt(&make_jwt(future)).unwrap();
        assert!(!info.is_expired);
        assert!(info.seconds_remaining > 0);
    }

    #[test]
    fn past_token_is_expired() {
        let past = Utc::now().timestamp() - 1;
        let info = inspect_jwt(&make_jwt(past)).unwrap();
        assert!(info.is_expired);
        assert!(info.seconds_remaining <= 0);
    }

    #[test]
    fn wrong_number_of_parts_errors() {
        assert!(inspect_jwt("onlyone").is_err());
        assert!(inspect_jwt("two.parts").is_err());
        // four dots should still work — splitn(3) collapses the rest into the third segment
        // but a real four-part token has an invalid payload; we just check the two-part case
    }

    #[test]
    fn non_base64_payload_errors() {
        let bad = "header.!!!not_base64!!!.sig";
        assert!(inspect_jwt(bad).is_err());
    }

    #[test]
    fn missing_exp_claim_errors() {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"no-exp-here"}"#);
        let token = format!("{header}.{payload}.sig");
        let err = inspect_jwt(&token).unwrap_err();
        assert!(err.to_string().contains("exp"));
    }

    #[test]
    fn non_json_payload_errors() {
        let header = URL_SAFE_NO_PAD.encode(b"header");
        let payload = URL_SAFE_NO_PAD.encode(b"not json at all");
        let token = format!("{header}.{payload}.sig");
        assert!(inspect_jwt(&token).is_err());
    }

    #[test]
    fn require_valid_jwt_passes_for_future_token() {
        let future = Utc::now().timestamp() + 3600;
        assert!(require_valid_jwt(&make_jwt(future), "test-instance").is_ok());
    }

    #[test]
    fn require_valid_jwt_errors_for_expired_token() {
        let past = Utc::now().timestamp() - 1;
        let err = require_valid_jwt(&make_jwt(past), "my-instance").unwrap_err();
        assert!(err.to_string().contains("my-instance"));
    }

    #[test]
    fn status_line_shows_expired() {
        let past = Utc::now().timestamp() - 1;
        let info = inspect_jwt(&make_jwt(past)).unwrap();
        assert!(info.status_line().contains("EXPIRED"));
    }

    #[test]
    fn status_line_shows_remaining_time() {
        let future = Utc::now().timestamp() + 7200;
        let info = inspect_jwt(&make_jwt(future)).unwrap();
        let line = info.status_line();
        assert!(line.contains("valid"));
        assert!(line.contains('h'));
        assert!(line.contains('m'));
    }

    #[test]
    fn token_expiring_exactly_now_is_expired() {
        // exp == floor(now) → seconds_remaining is 0 or -1, both satisfy <= 0.
        // Time only moves forward between these two calls, so is_expired is always true.
        let now_ts = Utc::now().timestamp();
        let info = inspect_jwt(&make_jwt(now_ts)).unwrap();
        assert!(info.is_expired);
        assert!(info.seconds_remaining <= 0);
    }
}
