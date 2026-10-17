//! HTTP mapping for the collections routes.
//!
//! `From<CollectionsError> for ApiError` is the only place a collections
//! failure meets a status code. Lock contention answers the closed-list
//! `SERVER_BUSY`; any other fault is the fixed internal error.

use droppedneedle_kernel::error::{ApiError, Unavailable};

use super::error::CollectionsError;

impl From<CollectionsError> for ApiError {
    fn from(error: CollectionsError) -> Self {
        match error {
            CollectionsError::Unauthorized { message } => ApiError::unauthorized(message),
            CollectionsError::Forbidden { message } => ApiError::forbidden(message),
            CollectionsError::NotFound => ApiError::not_found(),
            CollectionsError::InvalidInput { message } => ApiError::invalid_input(message),
            CollectionsError::Conflict { message } => ApiError::conflict(message),
            CollectionsError::Busy { error_id } => ApiError::unavailable(
                Unavailable::ServerBusy,
                format!("collections database busy (error id {error_id})"),
            ),
            CollectionsError::Internal { error_id } => {
                ApiError::internal(format!("collections failed (error id {error_id})"))
            }
        }
    }
}

/// Handler result type.
pub type HttpResult<T> = Result<T, ApiError>;
