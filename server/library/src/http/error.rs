//! Library HTTP errors and their one mapping onto [`ApiError`].
//!
//! 4xx messages are user-safe and render verbatim. A refusal with a
//! [`Reason`] keeps its own code and sentence and names what to do in
//! `details.action`. A server fault keeps its status with the fixed body;
//! its cause goes to the log once, at the request boundary.

use axum::http::StatusCode;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use serde_json::json;

use crate::operations::reasons::Reason;

/// Handler-layer errors with their status mapping.
#[derive(Debug)]
pub enum LibraryError {
    /// Credential lacks rights.
    Forbidden { message: String },
    /// Missing resource (also used to hide private resources).
    NotFound,
    /// Bad input with a user-safe message.
    InvalidInput { message: String },
    /// Valid input against the wrong state.
    Conflict { message: String },
    /// The database was too busy to answer: the server-busy 503 with
    /// `Retry-After`.
    Busy {
        /// What was busy, for the log only.
        cause: String,
    },
    /// Server fault.
    Internal {
        /// What went wrong, for the log only.
        cause: String,
    },
}

impl LibraryError {
    /// A 500 with its cause for the log.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }
}

impl From<LibraryError> for ApiError {
    fn from(error: LibraryError) -> Self {
        match error {
            LibraryError::Forbidden { message } => ApiError::forbidden(message),
            LibraryError::NotFound => ApiError::not_found(),
            LibraryError::InvalidInput { message } => ApiError::invalid_input(message),
            LibraryError::Conflict { message } => ApiError::conflict(message),
            LibraryError::Busy { cause } => ApiError::unavailable(Unavailable::ServerBusy, cause),
            LibraryError::Internal { cause } => ApiError::internal(cause),
        }
    }
}

impl From<crate::service::ServiceError> for LibraryError {
    fn from(error: crate::service::ServiceError) -> Self {
        use crate::service::ServiceError;
        match error {
            ServiceError::NotFound => Self::NotFound,
            ServiceError::InvalidInput { message } => Self::InvalidInput { message },
            ServiceError::Conflict { message } => Self::Conflict { message },
            ServiceError::Busy { cause } => Self::Busy { cause },
            ServiceError::Internal { cause } => Self::internal(&cause),
        }
    }
}

impl From<crate::service::ServiceError> for ApiError {
    fn from(error: crate::service::ServiceError) -> Self {
        LibraryError::from(error).into()
    }
}

/// A refusal with its reason's code and sentence, and its action in
/// `details.action`.
pub(super) fn refused(status: StatusCode, reason: Reason) -> ApiError {
    ApiError::new(status, reason.code, reason.message)
        .with_details(json!({ "action": reason.action }))
}
