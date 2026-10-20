//! The discover routes' one mapping onto [`ApiError`].
//!
//! Services return [`ServiceError`]; this is the only place it meets a
//! status. A feature that needs setting up answers 409 with its own code and
//! sentence, so the page can say what to do. A provider failure answers by
//! its kind (an outage is 503 `PROVIDER_UNAVAILABLE`). Other faults keep
//! their status with the fixed body; their cause goes to the log only.

use axum::http::StatusCode;
use droppedneedle_kernel::error::{ApiError, RATE_LIMITED};

use super::ports::BatchRefusal;
use super::services::ServiceError;

/// The feature needs setup first: a key, an account (409).
pub const NOT_CONFIGURED: &str = "NOT_CONFIGURED";
/// The feature is not built in this version yet (409).
pub const NOT_AVAILABLE: &str = "NOT_AVAILABLE";

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        match error {
            ServiceError::NotFound => ApiError::not_found(),
            ServiceError::InvalidInput(message) => ApiError::invalid_input(message),
            ServiceError::Provider(error) => error.into(),
            ServiceError::Upstream(cause) => ApiError::fault(StatusCode::BAD_GATEWAY, cause),
            ServiceError::Internal(cause) => ApiError::internal(cause),
            ServiceError::NotConfigured(message) => {
                ApiError::new(StatusCode::CONFLICT, NOT_CONFIGURED, message)
            }
            ServiceError::NotBuilt(message) => {
                ApiError::new(StatusCode::CONFLICT, NOT_AVAILABLE, message)
            }
            ServiceError::Exhausted(message) => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, RATE_LIMITED, message)
            }
            ServiceError::Refused(refusal) => refused(refusal),
        }
    }
}

/// A batch the request rules refused, answered with the status, code and
/// details the requests routes give.
fn refused(refusal: BatchRefusal) -> ApiError {
    let status = StatusCode::from_u16(refusal.status).unwrap_or(StatusCode::FORBIDDEN);
    let error = ApiError::new(status, refusal.code, refusal.message);
    match refusal.details {
        Some(details) => error.with_details(details),
        None => error,
    }
}
