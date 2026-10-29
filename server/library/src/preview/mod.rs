//! Preview-and-confirm tokens, for every change a person previews and then
//! applies.
//!
//! A token is `<issued_at>.<mac>`. The MAC is an HMAC with the server's
//! data key under the flow's own [`Purpose`], over the person who
//! previewed, the time, and the material the preview read and computed.
//! Applying recomputes the material and checks the token against it: a
//! different person, a changed selection or state, a token past its
//! expiry, or one that was altered is refused. A token lasts [`TTL_SECS`]
//! from issue unless the flow keeps its own expiry ([`issue_until`]); the
//! expiry is part of the MAC either way. The comparison takes the same
//! time wherever the tokens differ.

use crate::operations::reasons::Reason;
use crate::settings::LibrarySettings;
use droppedneedle_auth::session::tokens::constant_time_eq;

/// How long a preview stays good.
pub const TTL_SECS: i64 = 15 * 60;

/// How far ahead of the server's clock a token's time may be.
const CLOCK_SLACK_SECS: i64 = 60;

/// Keeps one flow's tokens from being accepted by another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Purpose(&'static str);

/// Catalog corrections: split, merge, move, reset and artist merges.
pub const CORRECTION: Purpose = Purpose("catalog-correction-preview");
/// Bulk changes to identification reviews.
pub const REVIEW_BULK: Purpose = Purpose("review-bulk-preview");
/// Library Management previews and activation dry runs.
pub const MANAGEMENT: Purpose = Purpose("library-management-preview");

/// When a token stops confirming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expiry {
    /// [`TTL_SECS`] after issue.
    FromIssue,
    /// At this time (unix seconds), which the flow keeps itself.
    At(i64),
}

impl Expiry {
    /// The one text the MAC carries for it.
    fn canonical(self) -> String {
        match self {
            Self::FromIssue => format!("ttl:{TTL_SECS}"),
            Self::At(at) => format!("until:{at}"),
        }
    }
}

fn mac(
    signer: &LibrarySettings,
    purpose: Purpose,
    actor: &str,
    material: &str,
    issued_at: i64,
    expiry: Expiry,
) -> String {
    let message = format!("{actor}\0{issued_at}\0{}\0{material}", expiry.canonical());
    signer
        .mac(purpose.0, message.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn token(
    signer: &LibrarySettings,
    purpose: Purpose,
    actor: &str,
    material: &str,
    issued_at: i64,
    expiry: Expiry,
) -> String {
    format!(
        "{issued_at}.{}",
        mac(signer, purpose, actor, material, issued_at, expiry)
    )
}

fn issued_at(token: &str) -> Result<i64, TokenFault> {
    token
        .split_once('.')
        .and_then(|(issued, _)| issued.parse().ok())
        .ok_or(TokenFault::Invalid)
}

/// A token for `material`, previewed by `actor` at `issued_at` (unix
/// seconds), good for [`TTL_SECS`].
pub fn issue(
    signer: &LibrarySettings,
    purpose: Purpose,
    actor: &str,
    material: &str,
    issued_at: i64,
) -> String {
    token(
        signer,
        purpose,
        actor,
        material,
        issued_at,
        Expiry::FromIssue,
    )
}

/// Check a token `actor` sent against the material recomputed now.
pub fn verify(
    signer: &LibrarySettings,
    purpose: Purpose,
    actor: &str,
    presented: &str,
    material: &str,
    now: i64,
) -> Result<(), TokenFault> {
    let issued = issued_at(presented)?;
    if issued > now.saturating_add(CLOCK_SLACK_SECS) || now.saturating_sub(issued) > TTL_SECS {
        return Err(TokenFault::Expired);
    }
    let expected = token(signer, purpose, actor, material, issued, Expiry::FromIssue);
    if !constant_time_eq(presented, &expected) {
        return Err(TokenFault::Stale);
    }
    Ok(())
}

/// As [`issue`], for a flow that keeps its own expiry: the token is good
/// until `expires_at` (unix seconds).
pub fn issue_until(
    signer: &LibrarySettings,
    purpose: Purpose,
    actor: &str,
    material: &str,
    issued_at: i64,
    expires_at: i64,
) -> String {
    token(
        signer,
        purpose,
        actor,
        material,
        issued_at,
        Expiry::At(expires_at),
    )
}

/// Check a token from [`issue_until`] with the same `expires_at`. The MAC
/// is checked first, so `Expired` means the token itself is genuine.
pub fn verify_until(
    signer: &LibrarySettings,
    purpose: Purpose,
    actor: &str,
    presented: &str,
    material: &str,
    expires_at: i64,
    now: i64,
) -> Result<(), TokenFault> {
    let issued = issued_at(presented)?;
    let expected = token(
        signer,
        purpose,
        actor,
        material,
        issued,
        Expiry::At(expires_at),
    );
    if !constant_time_eq(presented, &expected) {
        return Err(TokenFault::Stale);
    }
    if issued > now.saturating_add(CLOCK_SLACK_SECS) || now > expires_at {
        return Err(TokenFault::Expired);
    }
    Ok(())
}

/// Why a token does not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenFault {
    /// Not a token at all.
    Invalid,
    /// Past its expiry, or dated in the future.
    Expired,
    /// Another person's, another flow's, altered, or the state it covers
    /// changed since.
    Stale,
}

impl TokenFault {
    /// The refusal a person reads; `stale` says what changed in this flow.
    pub fn reason(self, stale: Reason) -> Reason {
        match self {
            Self::Invalid => TOKEN_INVALID,
            Self::Expired => PREVIEW_EXPIRED,
            Self::Stale => stale,
        }
    }
}

const fn reason(code: &'static str, message: &'static str, action: &'static str) -> Reason {
    Reason {
        code,
        message,
        action,
    }
}

pub const TOKEN_INVALID: Reason = reason(
    "PREVIEW_TOKEN_INVALID",
    "This change was not previewed, or the preview could not be read.",
    "Preview the change, then apply it.",
);
pub const PREVIEW_EXPIRED: Reason = reason(
    "PREVIEW_EXPIRED",
    "The preview is more than 15 minutes old.",
    "Preview the change again, then apply it.",
);

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_kernel::db::scratch::ScratchDir;
    use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};

    /// A token confirms only the material it was issued for, for the
    /// person who previewed, in the flow that issued it, as issued, and
    /// only for 15 minutes.
    #[test]
    fn a_token_confirms_only_its_own_preview() {
        let dir = ScratchDir::new("preview-token").expect("scratch dir");
        let store = ConfigStore::scratch(dir.path(), Crypto::from_key_bytes(&[7u8; 32]).unwrap())
            .expect("settings open");
        let signer = LibrarySettings::new(&store, None);
        let at = 1_000_000;
        let token = issue(&signer, REVIEW_BULK, "ann", "material", at);
        let check = |purpose, actor, token: &str, material, now| {
            verify(&signer, purpose, actor, token, material, now)
        };

        assert_eq!(check(REVIEW_BULK, "ann", &token, "material", at), Ok(()));
        assert_eq!(
            check(REVIEW_BULK, "ann", &token, "material", at + TTL_SECS),
            Ok(())
        );
        assert_eq!(
            check(REVIEW_BULK, "bob", &token, "material", at),
            Err(TokenFault::Stale),
            "another person's token"
        );
        assert_eq!(
            check(CORRECTION, "ann", &token, "material", at),
            Err(TokenFault::Stale),
            "another flow's token"
        );
        assert_eq!(
            check(REVIEW_BULK, "ann", &token, "changed", at),
            Err(TokenFault::Stale),
            "the state changed"
        );
        let tampered = format!("{}.{}", at + 1, token.split_once('.').unwrap().1);
        assert_eq!(
            check(REVIEW_BULK, "ann", &tampered, "material", at),
            Err(TokenFault::Stale),
            "a token with its time moved"
        );
        assert_eq!(
            check(REVIEW_BULK, "ann", &token, "material", at + TTL_SECS + 1),
            Err(TokenFault::Expired)
        );
        assert_eq!(
            check(REVIEW_BULK, "ann", "not a token", "material", at),
            Err(TokenFault::Invalid)
        );

        // A flow with its own expiry: good past 15 minutes, until then.
        let until = at + 3 * TTL_SECS;
        let kept = issue_until(&signer, MANAGEMENT, "ann", "job", at, until);
        let check = |actor, token: &str, expires_at, now| {
            verify_until(&signer, MANAGEMENT, actor, token, "job", expires_at, now)
        };
        assert_eq!(check("ann", &kept, until, at + 2 * TTL_SECS), Ok(()));
        assert_eq!(
            check("ann", &kept, until, until + 1),
            Err(TokenFault::Expired)
        );
        assert_eq!(
            check("bob", &kept, until, at),
            Err(TokenFault::Stale),
            "another person's token"
        );
        assert_eq!(
            check("ann", &kept, until + 3600, at),
            Err(TokenFault::Stale),
            "the expiry is part of the MAC"
        );
        assert_eq!(
            verify(&signer, MANAGEMENT, "ann", &kept, "job", at),
            Err(TokenFault::Stale),
            "a kept-expiry token is not a 15-minute one"
        );
    }
}
