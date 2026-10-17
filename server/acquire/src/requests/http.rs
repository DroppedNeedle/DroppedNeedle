//! The requests routes' one mapping onto [`ApiError`].

use axum::http::StatusCode;
use droppedneedle_kernel::error::{ApiError, Unavailable};

use super::error::{QUOTA_EXCEEDED, RequestsError, STORAGE_FULL};

/// Status, code and details for each requests error. Quota and storage
/// refusals keep their details for the Requests page, a busy database is
/// `SERVER_BUSY`, and a fault's cause goes to the log only.
impl From<RequestsError> for ApiError {
    fn from(error: RequestsError) -> Self {
        match error {
            RequestsError::Unauthorized { message } => ApiError::unauthorized(message),
            RequestsError::Forbidden { message } => ApiError::forbidden(message),
            RequestsError::NotFound => ApiError::not_found(),
            RequestsError::InvalidInput { message } => ApiError::invalid_input(message),
            RequestsError::Conflict { message } => ApiError::conflict(message),
            RequestsError::QuotaExceeded { message, details } => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, QUOTA_EXCEEDED, message)
                    .with_details(details)
            }
            RequestsError::StorageFull { message, details } => {
                ApiError::new(StatusCode::FORBIDDEN, STORAGE_FULL, message).with_details(details)
            }
            RequestsError::Busy { operation } => ApiError::unavailable(
                Unavailable::ServerBusy,
                format_args!("database busy: {operation}"),
            ),
            RequestsError::Internal { cause } => ApiError::internal(cause),
        }
    }
}
