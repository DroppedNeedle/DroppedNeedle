//! Opaque session tokens: 32 random bytes, urlsafe-b64 on the wire, SHA-256 at rest.
//!
//! v2 format preserved byte for byte: `urlsafe_b64encode(os.urandom(32))`
//! (padded, 44 chars) with `sha256(raw).hexdigest()` stored in
//! `auth_tokens.token_hash`. Tokens are never logged and never rendered
//! except once at mint (Bearer [REDACTED] Set-Cookie value).

use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Random bytes per token (v2 `TOKEN_BYTES`).
pub const TOKEN_BYTES: usize = 32;
/// Absolute session lifetime in days; no sliding refresh (spec D5).
pub const SESSION_LIFETIME_DAYS: i64 = 30;
/// Cookie max-age matching the token lifetime, in seconds.
pub const SESSION_MAX_AGE_SECS: i64 = SESSION_LIFETIME_DAYS * 24 * 60 * 60;

/// Token minting failure (entropy unavailable).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TokenError {
    /// The OS random source failed; the request must fail, never fall back.
    #[error("random source unavailable")]
    RngUnavailable,
}

/// Mint one opaque token: 32 OS-random bytes, urlsafe-b64 with padding.
pub fn mint_token() -> Result<String, TokenError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| TokenError::RngUnavailable)?;
    Ok(URL_SAFE.encode(bytes))
}

/// Storage hash for a raw token: lowercase SHA-256 hex.
pub fn hash_token(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

/// Absolute expiry for a session issued at `now_unix`.
pub fn expires_at(now_unix: i64) -> i64 {
    now_unix + SESSION_MAX_AGE_SECS
}

/// Fixed-time equality for stored hashes. Lengths are constant in practice
/// (64 hex chars); a length mismatch still returns without comparing.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    if x.len() != y.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..x.len() {
        diff |= x[i] ^ y[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn mint_format_and_uniqueness_match_v2() {
        let first = mint_token().unwrap();
        assert_eq!(first.len(), 44);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '=')
        );
        let rest: HashSet<_> = (0..50).map(|_| mint_token().unwrap()).collect();
        assert_eq!(rest.len(), 50);
        assert!(!rest.contains(&first));
    }

    #[test]
    fn hash_is_sha256_hex_and_compare_is_exact() {
        let hash = hash_token("abc");
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(constant_time_eq(&hash, &hash));
        assert!(!constant_time_eq(&hash, &hash_token("abd")));
        assert!(!constant_time_eq(&hash, "short"));
    }
}
