//! One-time app connect codes, held in memory as their SHA-256.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use super::request::verifier_matches;
use crate::session::tokens::{TokenError, hash_token, secret_eq};

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
pub(crate) struct Grant {
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

/// A code traded for exactly the app, address and verifier it was issued
/// for. Only [`PendingCodes::redeem`] makes one, and minting an app
/// password through app connect takes one.
#[derive(Debug)]
pub(crate) struct Redeemed {
    user_id: String,
    password_name: String,
}

impl Redeemed {
    /// The user who allowed the code.
    pub(crate) fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The app password's name, checked when the code was issued.
    pub(crate) fn password_name(&self) -> &str {
        &self.password_name
    }
}

#[derive(Debug)]
struct Pending {
    grant: Grant,
    issued_at: i64,
}

/// Codes waiting for their trade. A restart drops them; the app asks again.
#[derive(Debug, Default)]
pub(crate) struct PendingCodes {
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
    pub(crate) fn issue(&self, grant: Grant, now: i64) -> Result<String, TokenError> {
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

    /// Trade `code` for its grant when the app, address and verifier are
    /// exactly the ones it was issued for. The code is gone after this,
    /// whatever the answer: a code trades once. `None` when it is unknown,
    /// already taken, expired at `now`, or does not fit.
    pub(crate) fn redeem(
        &self,
        code: &str,
        client_id: &str,
        redirect_uri: &str,
        verifier: &str,
        now: i64,
    ) -> Option<Redeemed> {
        let pending = self
            .codes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&hash_token(code))?;
        let grant = live(&pending, now).then_some(pending.grant)?;
        let fits = secret_eq(&grant.client_id, client_id)
            & secret_eq(&grant.redirect_uri, redirect_uri)
            & verifier_matches(verifier, &grant.code_challenge);
        fits.then_some(Redeemed {
            user_id: grant.user_id,
            password_name: grant.password_name,
        })
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

    fn held(codes: &PendingCodes, code: &str) -> bool {
        codes.codes.lock().unwrap().contains_key(&hash_token(code))
    }

    fn count(codes: &PendingCodes) -> usize {
        codes.codes.lock().unwrap().len()
    }

    #[test]
    fn issuing_drops_expired_codes_and_caps_per_user_and_in_all() {
        let codes = PendingCodes::default();
        let stale = codes.issue(grant("u1"), 0).unwrap();
        let _ = codes.issue(grant("u2"), CODE_TTL_SECS);
        assert_eq!(count(&codes), 1, "the expired code went");
        assert!(!held(&codes, &stale));

        let first = codes.issue(grant("u1"), 200).unwrap();
        let mut rest = Vec::new();
        for at in 201..201 + CODES_PER_USER as i64 {
            rest.push(codes.issue(grant("u1"), at).unwrap());
        }
        assert!(!held(&codes, &first), "the oldest of u1 went");
        assert!(rest.iter().all(|code| held(&codes, code)));

        let codes = PendingCodes::default();
        let first = codes.issue(grant("u0"), 0).unwrap();
        for n in 1..=MAX_CODES {
            let _ = codes.issue(grant(&format!("u{n}")), 1).unwrap();
        }
        assert_eq!(count(&codes), MAX_CODES);
        assert!(!held(&codes, &first), "the oldest in all went");
    }
}
