//! The library reads' one mapping onto [`ApiError`].
//!
//! Services return [`LibraryFailure`]; this is the only place it meets a
//! status. A busy database answers the closed-list `SERVER_BUSY`; any other
//! fault is the fixed internal error, its cause going to the log only.

use droppedneedle_kernel::error::{ApiError, Unavailable};

use super::services::LibraryFailure;

impl From<LibraryFailure> for ApiError {
    fn from(failure: LibraryFailure) -> Self {
        match failure {
            LibraryFailure::NotFound => ApiError::not_found(),
            LibraryFailure::InvalidInput(message) => ApiError::invalid_input(message),
            LibraryFailure::Forbidden(message) => ApiError::forbidden(message),
            LibraryFailure::Busy(operation) => {
                ApiError::unavailable(Unavailable::ServerBusy, operation)
            }
            LibraryFailure::Internal(cause) => ApiError::internal(cause),
        }
    }
}
