//! Playback-reporting failures and their one mapping onto [`ApiError`].
//!
//! Services return domain failures that handlers convert; the `From` impl
//! below is the only place a status is chosen. Reporting itself almost
//! never errors: a failed forward is logged and swallowed (v2 never fails
//! the player for a report), so these errors cover only auth, unknown
//! tracks, and malformed bodies.

use axum::http::StatusCode;
use droppedneedle_kernel::error::ApiError;

/// Every failure the playback routes can return to a caller.
#[derive(Debug)]
pub enum PlaybackError {
    /// No valid session. Carries the `WWW-Authenticate` challenge.
    Unauthorized {
        /// User-safe reason, e.g. "Authentication required".
        message: String,
    },
    /// Unknown track id. Always the same fixed message.
    NotFound,
    /// Bad input. The message is shown to the user.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Downstream failure: a 502 whose cause reaches the log only.
    Upstream {
        /// Log-only cause.
        cause: String,
    },
    /// Server fault. The cause reaches the log only.
    Internal {
        /// Log-only cause.
        cause: String,
    },
}

impl PlaybackError {
    /// A 500 whose cause the request boundary logs.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }

    /// A 502 whose cause the request boundary logs.
    pub fn upstream(cause: impl std::fmt::Display) -> Self {
        Self::Upstream {
            cause: cause.to_string(),
        }
    }
}

impl From<PlaybackError> for ApiError {
    fn from(error: PlaybackError) -> Self {
        match error {
            PlaybackError::Unauthorized { message } => ApiError::unauthorized(message),
            PlaybackError::NotFound => ApiError::not_found(),
            PlaybackError::InvalidInput { message } => ApiError::invalid_input(message),
            PlaybackError::Upstream { cause } => ApiError::fault(StatusCode::BAD_GATEWAY, cause),
            PlaybackError::Internal { cause } => ApiError::internal(cause),
        }
    }
}
