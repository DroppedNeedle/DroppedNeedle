//! The collections domain error.
//!
//! Services return [`CollectionsError`]; `super::http` maps it to HTTP.
//! Unexpected failures carry their cause to the request boundary, which
//! logs it; it never reaches the caller.

use super::db::StoreError;

/// Every failure the collections routes can return to a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectionsError {
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
        /// The operation that waited, for the log only.
        operation: String,
    },
    /// Server fault.
    Internal {
        /// What went wrong, for the log only.
        cause: String,
    },
}

impl CollectionsError {
    /// A server fault whose cause the request boundary logs.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
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
            StoreError::Busy(operation) => Self::Busy { operation },
            StoreError::Internal(cause) => Self::internal(&cause),
        }
    }
}
