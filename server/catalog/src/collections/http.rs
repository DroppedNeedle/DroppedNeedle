//! HTTP mapping for the collections routes.
//!
//! `From<CollectionsError> for ApiError` is the only place a collections
//! failure meets a status code. Lock contention answers the closed-list
//! `SERVER_BUSY`; any other fault is the fixed internal error.

use axum::http::StatusCode;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use serde_json::json;

use super::error::CollectionsError;
use super::models::RedactedPlaylist;

/// Opening a playlist the caller may not read but their list shows.
const PLAYLIST_PRIVATE: &str = "PLAYLIST_PRIVATE";

/// The 403 for such a playlist: `details` is the stub, and nothing more.
pub(super) fn playlist_private(stub: &RedactedPlaylist) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        PLAYLIST_PRIVATE,
        "This playlist is private",
    )
    .with_details(json!(stub))
}

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
