//! Subsonic auth contract: three exclusive schemes and exact error codes.
//!
//! Schemes (mutually exclusive): `u+t+s` token (`t=md5(secret+s)`),
//! `u+p` password (hex-or-plain app password), lone `apiKey`. Conflicts
//! and duplicated keys fail with 10; bad credentials with 40; bad apiKey
//! with 44; `apiKey` plus `u` with 43. Only
//! `getOpenSubsonicExtensions` is public; every other endpoint (binary
//! stream/download/cover included) needs auth. Which methods are public
//! or answer bytes is compat's endpoint table.

use sha2::{Digest, Sha256};

use super::prod::CompatPasswords;
use crate::session::tokens::secret_eq;

/// Generic error.
pub const GENERIC: u8 = 0;
/// Required parameter missing.
pub const PARAM_MISSING: u8 = 10;
/// Wrong username or password.
pub const WRONG_CREDENTIALS: u8 = 40;
/// Multiple conflicting authentication mechanisms.
pub const CONFLICTING_AUTH: u8 = 43;
/// Invalid apiKey.
pub const INVALID_APIKEY: u8 = 44;
/// v2 length caps, kept verbatim.
pub const MAX_USERNAME_LENGTH: usize = 256;
/// v2 length caps, kept verbatim.
pub const MAX_AUTH_VALUE_LENGTH: usize = 1024;
/// v2 length caps, kept verbatim (hex doubles the secret plus `enc:`).
pub const MAX_ENCODED_PASSWORD_LENGTH: usize = 2 * MAX_AUTH_VALUE_LENGTH + 4;
/// v2 length caps, kept verbatim.
pub const MAX_SALT_LENGTH: usize = 128;
/// v2 length caps, kept verbatim.
pub const MAX_TOKEN_LENGTH: usize = 128;
/// v2 length caps, kept verbatim.
pub const MAX_CLIENT_NAME_LENGTH: usize = 256;

/// Query-plus-form params, repeats preserved (duplicates are an error).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubsonicParams {
    entries: Vec<(String, String)>,
}

impl SubsonicParams {
    /// Build from pairs in arrival order.
    pub fn new(entries: Vec<(&str, &str)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        }
    }

    /// All values for a key, in order.
    pub fn all(&self, key: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// First value for a key, if present (even when empty).
    pub fn first(&self, key: &str) -> Option<&str> {
        self.all(key).into_iter().next()
    }
}

/// Auth denial: the Subsonic error code. Auth paths raise bare codes in
/// v2, so compat answers each with its default message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubsonicDenied {
    /// The Subsonic error code.
    pub code: u8,
}

impl SubsonicDenied {
    /// Deny with a code.
    pub fn new(code: u8) -> Self {
        Self { code }
    }
}

/// Authenticated principal: the owning user id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicPrincipal {
    /// `auth_users.id`.
    pub user_id: String,
}

/// One active app password: lookup hash plus recoverable plaintext (the
/// token scheme needs the secret to compute `md5(secret+s)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSecret {
    /// SHA-256 hex of the secret.
    pub sha256: String,
    /// The secret itself.
    pub plaintext: String,
}

/// Store failure. The router maps this to code 0 (generic), never to an
/// auth code: a broken store must not look like bad credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicStoreError;

/// Run the three schemes. Error precedence mirrors v2 exactly: duplicate
/// keys, presence conflicts, apiKey rules, presence of `u`, length caps,
/// user lookup, then per-scheme verification.
pub async fn authenticate(
    store: &CompatPasswords,
    params: &SubsonicParams,
) -> Result<SubsonicPrincipal, SubsonicDenied> {
    for key in ["u", "t", "s", "p", "apiKey", "c"] {
        if params.all(key).len() > 1 {
            return Err(SubsonicDenied::new(PARAM_MISSING));
        }
    }
    let u = params.first("u");
    let t = params.first("t");
    let s = params.first("s");
    let p = params.first("p");
    let api_key = params.first("apiKey");
    let client = params.first("c");

    if t.is_some() != s.is_some() {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }
    if p.is_some() && t.is_some() {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }
    if api_key.is_some() && (t.is_some() || p.is_some()) {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }

    if client.is_some_and(|value| value.len() > MAX_CLIENT_NAME_LENGTH) {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }
    if api_key.is_some_and(|value| value.len() > MAX_AUTH_VALUE_LENGTH) {
        return Err(SubsonicDenied::new(INVALID_APIKEY));
    }
    if api_key.is_some_and(|value| !value.is_empty()) && u.is_some_and(|value| !value.is_empty()) {
        return Err(SubsonicDenied::new(CONFLICTING_AUTH));
    }
    if let Some(key) = api_key
        && !key.is_empty()
    {
        let owner = store
            .owner_of_secret(&sha256_hex(key))
            .await
            .map_err(|_| SubsonicDenied::new(GENERIC))?;
        let Some(user_id) = owner else {
            return Err(SubsonicDenied::new(INVALID_APIKEY));
        };
        store.note_use(key, None).await;
        return Ok(SubsonicPrincipal { user_id });
    }

    let Some(username) = u else {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    };
    if username.is_empty() {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }
    if username.len() > MAX_USERNAME_LENGTH {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }
    if t.is_some_and(|value| value.len() > MAX_TOKEN_LENGTH) {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }
    if s.is_some_and(|value| value.len() > MAX_SALT_LENGTH) {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }
    if p.is_some_and(|value| value.len() > MAX_ENCODED_PASSWORD_LENGTH) {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }

    let user_id = store
        .user_id_for_username(&username.trim().to_lowercase())
        .await
        .map_err(|_| SubsonicDenied::new(GENERIC))?;
    let Some(user_id) = user_id else {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    };

    if let (Some(token), Some(salt)) = (t, s)
        && !token.is_empty()
        && !salt.is_empty()
    {
        let target = token.to_lowercase();
        let secrets = store
            .active_secrets(&user_id)
            .await
            .map_err(|_| SubsonicDenied::new(GENERIC))?;
        for secret in &secrets {
            if secret_eq(&md5_hex(&format!("{}{salt}", secret.plaintext)), &target) {
                store.note_use(&secret.plaintext, client).await;
                return Ok(SubsonicPrincipal { user_id });
            }
        }
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }

    if let Some(password) = p {
        let secret = decode_subsonic_password(password);
        // A blank secret proves nothing, so it is refused before the lookup.
        if secret.is_empty() || secret.len() > MAX_AUTH_VALUE_LENGTH {
            return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
        }
        let owner = store
            .owner_of_secret(&sha256_hex(&secret))
            .await
            .map_err(|_| SubsonicDenied::new(GENERIC))?;
        if owner.as_deref() == Some(user_id.as_str()) {
            store.note_use(&secret, client).await;
            return Ok(SubsonicPrincipal { user_id });
        }
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }

    Err(SubsonicDenied::new(PARAM_MISSING))
}

/// Decode the `p` param: `enc:<hex>` decodes to UTF-8, anything else
/// (bad hex, odd length, non-UTF-8, or no prefix) passes through raw.
pub fn decode_subsonic_password(p: &str) -> String {
    let Some(hex) = p.strip_prefix("enc:") else {
        return p.to_owned();
    };
    let digits: String = hex.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    hex::decode(digits)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| p.to_owned())
}

/// SHA-256 hex of a secret (lookup hash).
pub fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// MD5 hex digest (RFC 1321). Used only for the Subsonic token scheme
/// (`t=md5(secret+s)`), which mandates MD5 for interop; never for anything
/// security-sensitive. The RFC vectors and the v2 spot value in the server's
/// auth store tests pin the bytes across the crate swap.
pub fn md5_hex(input: &str) -> String {
    format!("{:x}", md5::compute(input.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_decode_matches_v2() {
        assert_eq!(decode_subsonic_password("enc:616263"), "abc");
        assert_eq!(decode_subsonic_password("plain"), "plain");
        assert_eq!(decode_subsonic_password("enc:zzz"), "enc:zzz");
        assert_eq!(decode_subsonic_password("enc:6162636"), "enc:6162636");
        assert_eq!(decode_subsonic_password("enc:ff"), "enc:ff");
        assert_eq!(decode_subsonic_password("enc:61 62"), "ab");
        assert_eq!(decode_subsonic_password("enc:"), "");
    }
}
