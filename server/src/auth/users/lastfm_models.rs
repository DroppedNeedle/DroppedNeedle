//! Last.fm wire shapes for the account-link calls.
//!
//! Last.fm answers errors as `{"error": <code>, "message": ...}`, often
//! with a 4xx status, so the error shape is read before the status.

use serde::Deserialize;

/// An error answer.
#[derive(Debug, Clone, Deserialize)]
pub struct ErrorWire {
    /// Last.fm error code.
    pub error: i64,
    /// Human-readable reason (logged, never shown raw).
    #[serde(default)]
    pub message: String,
}

/// `auth.getToken` answer.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenWire {
    /// The token the user approves in the browser.
    pub token: String,
}

/// `auth.getSession` answer.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionWire {
    /// The session.
    pub session: SessionBody,
}

/// The linked session.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionBody {
    /// Last.fm username.
    pub name: String,
    /// Session key for signed calls (scrobbling).
    pub key: String,
}
