//! Catalog errors and their one mapping onto [`ApiError`].
//!
//! [`CatalogError`] is the service-layer error; its `From` impl is the only
//! place it becomes a status. Client faults carry a safe message; server
//! and upstream faults keep their status with the fixed body, the cause
//! going to the log only.

use axum::http::StatusCode;

use droppedneedle_kernel::error::{ApiError, NOT_FOUND};

/// What can go wrong building a catalog page.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CatalogError {
    /// The id is not a MusicBrainz id, or a parameter is out of range.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// MusicBrainz has no such entity and the library has no copy.
    #[error("not found")]
    NotFound,
    /// Something named in the request does not exist; the message says
    /// what.
    #[error("missing: {0}")]
    Missing(String),
    /// The caller's role does not allow this.
    #[error("forbidden")]
    Forbidden,
    /// The request conflicts with the library's state.
    #[error("conflict: {0}")]
    Conflict(String),
    /// MusicBrainz could not answer and the library has no copy to show.
    #[error("musicbrainz unavailable: {0}")]
    Unavailable(String),
    /// A local read failed.
    #[error("internal: {0}")]
    Internal(String),
}

impl CatalogError {
    /// Wrap a local database failure.
    pub fn database(error: sqlx::Error) -> Self {
        Self::Internal(format!("catalog database read: {error}"))
    }
}

impl From<CatalogError> for ApiError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::Invalid(message) => ApiError::invalid_input(message),
            CatalogError::NotFound => ApiError::not_found(),
            CatalogError::Missing(message) => {
                ApiError::new(StatusCode::NOT_FOUND, NOT_FOUND, message)
            }
            CatalogError::Forbidden => ApiError::forbidden("Curator access required"),
            CatalogError::Conflict(message) => ApiError::conflict(message),
            CatalogError::Unavailable(cause) => {
                ApiError::fault(StatusCode::SERVICE_UNAVAILABLE, cause)
            }
            CatalogError::Internal(cause) => ApiError::internal(cause),
        }
    }
}
