//! Remote-source route errors and their one mapping onto [`ApiError`].
//!
//! 4xx messages are user-safe. A media server that is down answers the
//! closed-list `REMOTE_UNAVAILABLE`, a busy database `SERVER_BUSY`, and any
//! other fault the fixed internal error; their detail goes to the log only.

use axum::http::{HeaderValue, StatusCode, header};
use droppedneedle_kernel::error::{ApiError, CONFLICT, INVALID_INPUT, Unavailable};

/// No credential stored for this source, or the source is disabled.
pub const REMOTE_NOT_CONFIGURED: &str = "REMOTE_NOT_CONFIGURED";
/// The stored credential was rejected upstream (HTTP 401/403, Subsonic 40/41).
pub const REMOTE_AUTH_FAILED: &str = "REMOTE_AUTH_FAILED";
/// The source has no such capability (Plex similar, Jellyfin top songs).
pub const REMOTE_UNSUPPORTED: &str = "REMOTE_UNSUPPORTED";

/// Every failure the remotes routes can return to a caller.
#[derive(Debug)]
pub enum RemotesError {
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
    /// Source has no stored credential.
    NotConfigured {
        /// Which source is missing, e.g. "Plex is not connected".
        message: String,
    },
    /// Upstream rejected the stored credential. Relink, do not retry.
    AuthFailed {
        /// Which source rejected, without credential detail.
        message: String,
    },
    /// Upstream error or unreachable.
    Unavailable {
        /// What went wrong, for the log only.
        cause: String,
    },
    /// Capability the source lacks.
    Unsupported {
        /// What is missing where, e.g. "Plex has no similar-tracks endpoint".
        message: String,
    },
    /// Server fault.
    Internal {
        /// What went wrong, for the log only.
        cause: String,
    },
}

impl From<RemotesError> for ApiError {
    fn from(error: RemotesError) -> Self {
        match error {
            RemotesError::NotFound => ApiError::not_found(),
            RemotesError::InvalidInput { message } => {
                ApiError::new(StatusCode::BAD_REQUEST, INVALID_INPUT, message)
            }
            RemotesError::Conflict { message } => {
                ApiError::new(StatusCode::BAD_REQUEST, CONFLICT, message)
            }
            RemotesError::NotConfigured { message } => {
                ApiError::new(StatusCode::BAD_REQUEST, REMOTE_NOT_CONFIGURED, message)
            }
            RemotesError::AuthFailed { message } => {
                ApiError::new(StatusCode::UNAUTHORIZED, REMOTE_AUTH_FAILED, message)
                    .with_header(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))
            }
            RemotesError::Unavailable { cause } => {
                ApiError::unavailable(Unavailable::MediaServer, cause)
            }
            RemotesError::Unsupported { message } => {
                ApiError::new(StatusCode::BAD_REQUEST, REMOTE_UNSUPPORTED, message)
            }
            RemotesError::Internal { cause } => ApiError::internal(cause),
        }
    }
}
