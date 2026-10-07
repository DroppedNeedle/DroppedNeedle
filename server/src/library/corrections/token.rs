//! Preview tokens. A token is `<issued_at>.<mac>`, where the MAC (keyed
//! with the server's data key) covers the person who previewed, the
//! request, the state the preview read, and the result it computed.
//! Applying recomputes all of it: anything that changed in between (a
//! scan, another correction, a different selection or choice, another
//! person) changes the MAC, so an apply only ever does what was shown to
//! the person applying it.

use super::reasons;
use crate::library::operations::reasons::Reason;
use crate::runtime_config::ConfigStore;

/// How long a preview stays good, as in v2.
pub const TTL_SECS: i64 = 15 * 60;

/// Keeps catalog correction tokens apart from anything else the server
/// signs.
const PURPOSE: &str = "catalog-correction-preview";

fn mac(signer: &ConfigStore, actor: &str, material: &str, issued_at: i64) -> String {
    let message = format!("{actor}\0{issued_at}\0{material}");
    signer
        .mac(PURPOSE, message.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A token for `material`, previewed by `actor` at `issued_at` (unix
/// seconds).
pub fn issue(signer: &ConfigStore, actor: &str, material: &str, issued_at: i64) -> String {
    format!("{issued_at}.{}", mac(signer, actor, material, issued_at))
}

/// Check a token against the material recomputed now.
pub fn verify(
    signer: &ConfigStore,
    actor: &str,
    token: &str,
    material: &str,
    now: i64,
) -> Result<(), TokenFault> {
    let issued: i64 = token
        .split_once('.')
        .and_then(|(issued, _)| issued.parse().ok())
        .ok_or(TokenFault::Invalid)?;
    if issued > now + 60 || now - issued > TTL_SECS {
        return Err(TokenFault::Expired);
    }
    if token != issue(signer, actor, material, issued) {
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
