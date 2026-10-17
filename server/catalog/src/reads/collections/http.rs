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
            CollectionsError::Forbidden { message } => ApiError::forbidden(message),
            CollectionsError::NotFound => ApiError::not_found(),
            CollectionsError::InvalidInput { message } => ApiError::invalid_input(message),
            CollectionsError::Conflict { message } => ApiError::conflict(message),
            CollectionsError::Busy { operation } => ApiError::unavailable(
                Unavailable::ServerBusy,
                format!("collections database busy during {operation}"),
            ),
            CollectionsError::Internal { cause } => {
                ApiError::internal(format!("collections failed: {cause}"))
            }
        }
    }
}

/// Handler result type.
pub type HttpResult<T> = Result<T, ApiError>;
