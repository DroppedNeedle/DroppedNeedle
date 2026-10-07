//! Preview tokens. A token is `<issued_at>.<digest>`, where the digest
//! covers the request, the state the preview read, and the result it
//! computed. Applying recomputes all three: anything that changed in
//! between (a scan, another correction, a different selection) changes
//! the digest, so an apply only ever does what the person saw.

use sha2::{Digest as _, Sha256};

use super::reasons;
use crate::library::operations::reasons::Reason;

/// How long a preview stays good, as in v2.
pub const TTL_SECS: i64 = 15 * 60;

fn digest(material: &str, issued_at: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(issued_at.to_string().as_bytes());
    hasher.update([0u8]);
    hasher.update(material.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A token for `material`, issued at `issued_at` (unix seconds).
pub fn issue(material: &str, issued_at: i64) -> String {
    format!("{issued_at}.{}", digest(material, issued_at))
}

/// Check a token against the material recomputed now.
pub fn verify(token: &str, material: &str, now: i64) -> Result<(), TokenFault> {
    let issued: i64 = token
        .split_once('.')
        .and_then(|(issued, _)| issued.parse().ok())
        .ok_or(TokenFault::Invalid)?;
    if issued > now + 60 || now - issued > TTL_SECS {
        return Err(TokenFault::Expired);
    }
    if token != issue(material, issued) {
        return Err(TokenFault::Stale);
    }
    Ok(())
}

/// Why a token does not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenFault {
    Invalid,
    Expired,
    Stale,
}

impl TokenFault {
    pub fn reason(self) -> Reason {
        match self {
            Self::Invalid => reasons::TOKEN_INVALID,
            Self::Expired => reasons::PREVIEW_EXPIRED,
            Self::Stale => reasons::PREVIEW_STALE,
        }
    }
}
