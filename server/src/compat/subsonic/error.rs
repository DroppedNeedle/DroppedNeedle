//! Subsonic error codes and the binary-vs-envelope split.
//!
//! v2: `backend/api/compat/subsonic/errors.py`, `router.py` (`_BINARY`,
//! `_AUTH_CODES`, `_binary_error`, `_dispatch`).

use std::fmt;

/// Generic error.
pub const GENERIC: u8 = 0;
/// Required parameter missing (also: invalid parameter value).
pub const PARAM_MISSING: u8 = 10;
/// Wrong username or password.
pub const WRONG_CREDENTIALS: u8 = 40;
/// Multiple conflicting authentication mechanisms.
pub const CONFLICTING_AUTH: u8 = 43;
/// Invalid apiKey.
pub const INVALID_APIKEY: u8 = 44;
/// User not authorized for the operation.
pub const NOT_AUTHORIZED: u8 = 50;
/// Requested data not found.
pub const NOT_FOUND: u8 = 70;

/// Default message per code, v2 `errors.py` verbatim.
pub fn default_message(code: u8) -> &'static str {
    match code {
        PARAM_MISSING => "Required parameter is missing.",
        WRONG_CREDENTIALS => "Wrong username or password.",
        CONFLICTING_AUTH => "Multiple conflicting authentication mechanisms provided.",
        INVALID_APIKEY => "Invalid API key.",
        NOT_AUTHORIZED => "User is not authorized for the given operation.",
        NOT_FOUND => "The requested data was not found.",
        _ => "An error occurred.",
    }
}

/// Protocol-native failure. This always renders as the Subsonic
/// `subsonic-response` failed envelope (or `text/plain` on the binary
/// path), never as the native `{"error":{...}}` envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicError {
    /// One of the codes above (41/42 reserved for future auth codes).
    pub code: u8,
    /// Wire message. v2 surfaces our own exception text and falls back
    /// to the static default for unexpected failures.
    pub message: String,
}

impl SubsonicError {
    /// Error with an explicit message.
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Bare-code error (auth paths in v2 raise bare codes).
    pub fn code_only(code: u8) -> Self {
        Self {
            code,
            message: default_message(code).to_owned(),
        }
    }

    /// Invalid-parameter error naming the parameter, v2 style.
    pub fn invalid(name: &str) -> Self {
        Self::new(PARAM_MISSING, format!("Invalid parameter '{name}'"))
    }

    /// Missing-parameter error naming the parameter, v2 style.
    pub fn missing(name: &str) -> Self {
        Self::new(
            PARAM_MISSING,
            format!("Required parameter '{name}' is missing"),
        )
    }
}

impl fmt::Display for SubsonicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "subsonic error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for SubsonicError {}
