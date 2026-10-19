//! Import route errors and their one mapping onto [`ApiError`].
//!
//! 4xx answers keep their codes and user-safe messages; 401 carries the
//! `Bearer` challenge. An upstream failure keeps its 502 with the fixed
//! body, a busy database is `SERVER_BUSY`, and every cause goes to the log
//! only, written once by the request boundary.

use axum::http::StatusCode;
use droppedneedle_kernel::error::ApiError;

/// No connection or settings stored for this import (Lidarr/Spotify).
pub const IMPORT_NOT_CONFIGURED: &str = "IMPORT_NOT_CONFIGURED";

/// Every failure the imports routes can return to a caller.
#[derive(Debug)]
pub enum ImportsError {
    /// Unknown id. Fixed message, no details.
    NotFound,
    /// Bad input. The message is shown to the user.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Valid input against the wrong state.
    Conflict {
        /// What was wrong.
        message: String,
    },
    /// Import has no stored connection.
    NotConfigured {
        /// Which import is missing, e.g. "Lidarr is not connected".
        message: String,
    },
    /// Upstream rejected the stored credential (502). Reconfigure, do not
    /// retry. The caller is signed in, so no Bearer challenge.
    AuthFailed {
        /// Which import rejected, for the log only.
        cause: String,
    },
    /// Upstream error or unreachable (502).
    Unavailable {
        /// What failed, for the log only.
        cause: String,
    },
    /// The database was too busy, or stopping, to take a write (503
    /// `SERVER_BUSY`).
    Busy {
        /// What waited, for the log only.
        cause: String,
    },
    /// Server fault.
    Internal {
        /// What went wrong, for the log only.
        cause: String,
    },
}

impl ImportsError {
    /// A 500 with its cause for the log.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }
}

impl From<ImportsError> for ApiError {
    fn from(error: ImportsError) -> Self {
        match error {
            ImportsError::NotFound => ApiError::not_found(),
            ImportsError::InvalidInput { message } => ApiError::invalid_input(message),
            ImportsError::Conflict { message } => ApiError::conflict(message),
            ImportsError::NotConfigured { message } => {
                ApiError::new(StatusCode::BAD_REQUEST, IMPORT_NOT_CONFIGURED, message)
            }
            ImportsError::AuthFailed { cause } | ImportsError::Unavailable { cause } => {
                ApiError::fault(StatusCode::BAD_GATEWAY, cause)
            }
            ImportsError::Busy { cause } => {
                ApiError::unavailable(droppedneedle_kernel::error::Unavailable::ServerBusy, cause)
            }
            ImportsError::Internal { cause } => ApiError::internal(cause),
        }
    }
}
