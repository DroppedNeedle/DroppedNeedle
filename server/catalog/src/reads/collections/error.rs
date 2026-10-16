//! The collections domain error.
//!
//! Services return [`CollectionsError`]; the handlers map it to HTTP
//! ([`super::http::CollectionsHttpError`]). Unexpected failures carry an
//! error id whose cause went to the log, never to the caller.

use super::db::StoreError;

/// Every failure the collections routes can return to a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectionsError {
    /// No usable credential.
    Unauthorized {
        /// User-safe reason.
        message: String,
    },
    /// Credential lacks rights.
    Forbidden {
        /// User-safe reason.
        message: String,
    },
    /// Missing resource (also hides other users' private playlists).
    NotFound,
    /// Bad input with a user-safe message.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Valid input against the wrong state.
    Conflict {
        /// What conflicted.
        message: String,
    },
    /// The database stayed locked past its timeout. Retryable.
    Busy {
        /// Ties the wire response to the log line.
        error_id: String,
    },
    /// Server fault.
    Internal {
        /// Ties the wire response to the log line.
        error_id: String,
    },
}

impl CollectionsError {
    /// A server fault, logging the real cause with a fresh id.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        let error_id = uuid::Uuid::new_v4().to_string();
        tracing::error!(error_id, %cause, "collections request failed");
        Self::Internal { error_id }
    }

    /// Bad input with a user-safe message.
    pub fn invalid(message: &str) -> Self {
        Self::InvalidInput {
            message: message.to_owned(),
        }
    }
}

impl From<StoreError> for CollectionsError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy(operation) => {
                let error_id = uuid::Uuid::new_v4().to_string();
                tracing::warn!(error_id, operation, "collections database busy");
                Self::Busy { error_id }
            }
            StoreError::Internal(cause) => Self::internal(&cause),
        }
    }
}
