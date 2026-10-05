//! Auth seam: principal shape plus credential classification.
//!
//! Secret verification (app passwords only, never account passwords) is
//! owned by `auth::compat_auth::subsonic`. This module only classifies
//! which scheme the caller attempted and enforces the mutual-exclusion
//! rules, so the HTTP adapter can hand the
//! extracted [`Credentials`] to the real verifier and adapt the result
//! into a [`Principal`].
//!
//! v2: the Subsonic compat auth and the app password service's
//! `verify_subsonic`.

use super::error::{CONFLICTING_AUTH, PARAM_MISSING, SubsonicError};
use super::params::SubsonicParameters;

/// Authenticated caller facts the handlers need.
pub trait Principal: Clone + Send + Sync {
    /// Owning user id.
    fn user_id(&self) -> &str;
    /// Login username.
    fn username(&self) -> &str;
    /// Display username (for avatar self-match).
    fn username_display(&self) -> &str {
        self.username()
    }
    /// Display name (for now-playing attribution).
    fn display_name(&self) -> &str;
    /// Admin role (startScan gate, getUser roles).
    fn is_admin(&self) -> bool;
}

/// Which auth scheme the request attempted (verified elsewhere).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credentials {
    /// `u` + `t=md5(secret+s)` + `s`.
    Token {
        /// Username.
        username: String,
        /// Hex token.
        token: String,
        /// Salt.
        salt: String,
        /// Client name (`c`).
        client: Option<String>,
    },
    /// `u` + `p` (hex-or-plain app password).
    Password {
        /// Username.
        username: String,
        /// Raw `p` value.
        password: String,
        /// Client name (`c`).
        client: Option<String>,
    },
    /// Lone `apiKey` (OpenSubsonic extension).
    ApiKey {
        /// Raw key.
        key: String,
    },
}

/// Classify the auth params: duplicates of the auth keys, `t`/`s`
/// asymmetry, `p`+token, and apiKey+token/password mixes are all code 10
/// (v2 `resolve_subsonic_user`). A non-empty apiKey next to a non-empty
/// `u` is code 43 (v2 `verify_subsonic`). Anything else is returned for
/// the verifier; missing credentials surface as code 10 there.
pub fn classify(params: &SubsonicParameters) -> Result<Option<Credentials>, SubsonicError> {
    for key in ["u", "t", "s", "p", "apiKey", "c"] {
        if params.all(key).len() > 1 {
            return Err(SubsonicError::code_only(PARAM_MISSING));
        }
    }
    let first = |key: &str| params.all(key).into_iter().next().map(str::to_owned);
    let u = first("u");
    let t = first("t");
    let s = first("s");
    let p = first("p");
    let api_key = first("apiKey");
    let client = first("c");

    if t.is_some() != s.is_some() {
        return Err(SubsonicError::code_only(PARAM_MISSING));
    }
    if p.is_some() && t.is_some() {
        return Err(SubsonicError::code_only(PARAM_MISSING));
    }
    if api_key.is_some() && (t.is_some() || p.is_some()) {
        return Err(SubsonicError::code_only(PARAM_MISSING));
    }
    if api_key.as_deref().is_some_and(|key| !key.is_empty())
        && u.as_deref().is_some_and(|name| !name.is_empty())
    {
        return Err(SubsonicError::code_only(CONFLICTING_AUTH));
    }
    if api_key.as_deref().is_some_and(|key| !key.is_empty()) {
        return Ok(Some(Credentials::ApiKey {
            key: api_key.unwrap_or_default(),
        }));
    }
    match (u, t, s, p) {
        (Some(username), Some(token), Some(salt), None) => Ok(Some(Credentials::Token {
            username,
            token,
            salt,
            client,
        })),
        (Some(username), None, None, Some(password)) => Ok(Some(Credentials::Password {
            username,
            password,
            client,
        })),
        _ => Ok(None),
    }
}

/// Length caps from v2 `app_password_service` (kept verbatim; the real
/// verifier enforces them, listed here so goldens stay aligned).
pub mod caps {
    /// Max username length.
    pub const MAX_USERNAME_LENGTH: usize = 256;
    /// Max auth value length.
    pub const MAX_AUTH_VALUE_LENGTH: usize = 1024;
    /// Max encoded password length (hex doubles plus `enc:`).
    pub const MAX_ENCODED_PASSWORD_LENGTH: usize = 2 * MAX_AUTH_VALUE_LENGTH + 4;
    /// Max salt length.
    pub const MAX_SALT_LENGTH: usize = 128;
    /// Max token length.
    pub const MAX_TOKEN_LENGTH: usize = 128;
    /// Max client name length.
    pub const MAX_CLIENT_NAME_LENGTH: usize = 256;
}
