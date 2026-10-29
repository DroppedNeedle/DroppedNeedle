//! One-time app connect codes, held in memory as their SHA-256.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::session::tokens::{TokenError, hash_token};

/// How long a code waits for its trade, in seconds.
pub const CODE_TTL_SECS: i64 = 120;
/// Random bytes per code.
pub const CODE_BYTES: usize = 32;
/// Codes kept per user; the oldest goes first.
pub const CODES_PER_USER: usize = 5;
/// Codes kept in all; the oldest goes first.
pub const MAX_CODES: usize = 1024;

/// What a code was issued for. The token step trades it only for exactly
/// this app, return address and verifier, and mints for this user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The user who confirmed it.
    pub user_id: String,
    /// The app it was issued to.
    pub client_id: String,
    /// The return address it was sent to.
    pub redirect_uri: String,
    /// The PKCE `S256` challenge of the request.
    pub code_challenge: String,
    /// The app password's name.
    pub password_name: String,
}

#[derive(Debug)]
struct Pending {
    grant: Grant,
    issued_at: i64,
}

/// Codes waiting for their trade. A restart drops them; the app asks again.
#[derive(Debug, Default)]
pub struct PendingCodes {
    codes: Mutex<HashMap<String, Pending>>,
}

/// Fresh code: [`CODE_BYTES`] OS-random bytes, base64url without padding
/// (43 characters, nothing to escape in a URL).
fn mint_code() -> Result<String, TokenError> {
    let mut bytes = [0u8; CODE_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| TokenError::RngUnavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn live(pending: &Pending, now: i64) -> bool {
    now >= pending.issued_at && now - pending.issued_at < CODE_TTL_SECS
}

impl PendingCodes {
    /// Issue a code for `grant` at `now` (unix seconds) and return it.
    /// Expired codes go first; past [`CODES_PER_USER`] for the user or
    /// [`MAX_CODES`] in all, so do the oldest.
    pub fn issue(&self, grant: Grant, now: i64) -> Result<String, TokenError> {
        let code = mint_code()?;
        let mut codes = self
            .codes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        codes.retain(|_, pending| live(pending, now));
        let oldest = |codes: &HashMap<String, Pending>, user: Option<&str>| {
            codes
                .iter()
                .filter(|(_, pending)| user.is_none_or(|user| pending.grant.user_id == user))
                .min_by_key(|(_, pending)| pending.issued_at)
                .map(|(hash, _)| hash.clone())
        };
        while codes
            .values()
            .filter(|pending| pending.grant.user_id == grant.user_id)
            .count()
            >= CODES_PER_USER
        {
            let Some(hash) = oldest(&codes, Some(&grant.user_id)) else {
                break;
            };
            codes.remove(&hash);
        }
        while codes.len() >= MAX_CODES {
            let Some(hash) = oldest(&codes, None) else {
                break;
            };
            codes.remove(&hash);
        }
        codes.insert(
            hash_token(&code),
            Pending {
                grant,
                issued_at: now,
            },
        );
        Ok(code)
    }

    /// Take `code` out, whatever happens next: a code trades once. `None`
    /// when it is unknown, already taken, or expired at `now`.
    pub fn take(&self, code: &str, now: i64) -> Option<Grant> {
        let mut codes = self
            .codes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let pending = codes.remove(&hash_token(code))?;
        live(&pending, now).then_some(pending.grant)
    }

    /// Codes held, expired ones included until the next issue.
    pub fn len(&self) -> usize {
        self.codes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len()
    }

    /// True when no code is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(user: &str) -> Grant {
        Grant {
            user_id: user.to_owned(),
            client_id: "app".to_owned(),
            redirect_uri: "com.example.app:/connect".to_owned(),
            code_challenge: "c".repeat(43),
            password_name: "App".to_owned(),
        }
    }

    #[test]
    fn a_code_is_43_url_characters_and_kept_only_as_its_hash() {
        let codes = PendingCodes::default();
        let code = codes.issue(grant("u1"), 1_000).unwrap();
        assert_eq!(code.len(), 43);
        assert!(
            code.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
        let held = codes.codes.lock().unwrap();
        assert!(held.contains_key(&hash_token(&code)));
        assert!(!held.contains_key(&code));
    }

    #[test]
    fn a_code_trades_once_and_only_within_its_lifetime() {
        let codes = PendingCodes::default();
        let code = codes.issue(grant("u1"), 1_000).unwrap();
        assert_eq!(
            codes.take(&code, 1_000 + CODE_TTL_SECS - 1),
            Some(grant("u1"))
        );
        assert_eq!(codes.take(&code, 1_001), None, "second trade");

        let late = codes.issue(grant("u1"), 1_000).unwrap();
        assert_eq!(codes.take(&late, 1_000 + CODE_TTL_SECS), None, "expired");
        assert_eq!(
            codes.take(&late, 1_000),
            None,
            "an expired take still burns it"
        );

        assert_eq!(codes.take("unknown", 1_000), None);
        let early = codes.issue(grant("u1"), 1_000).unwrap();
        assert_eq!(codes.take(&early, 999), None, "a clock that went back");
    }

    #[test]
    fn issuing_drops_expired_codes_and_caps_per_user_and_in_all() {
        let codes = PendingCodes::default();
        let stale = codes.issue(grant("u1"), 0).unwrap();
        let _ = codes.issue(grant("u2"), CODE_TTL_SECS);
        assert_eq!(codes.len(), 1, "the expired code went");
        assert_eq!(codes.take(&stale, 0), None);

        let first = codes.issue(grant("u1"), 200).unwrap();
        let mut rest = Vec::new();
        for at in 201..201 + CODES_PER_USER as i64 {
            rest.push(codes.issue(grant("u1"), at).unwrap());
        }
        assert_eq!(codes.take(&first, 210), None, "the oldest of u1 went");
        for code in &rest {
            assert!(codes.take(code, 210).is_some());
        }

        let codes = PendingCodes::default();
        let first = codes.issue(grant("u0"), 0).unwrap();
        for n in 1..=MAX_CODES {
            let _ = codes.issue(grant(&format!("u{n}")), 1).unwrap();
        }
        assert_eq!(codes.len(), MAX_CODES);
        assert_eq!(codes.take(&first, 2), None, "the oldest in all went");
    }
}
