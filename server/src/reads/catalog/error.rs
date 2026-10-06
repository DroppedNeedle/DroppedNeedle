//! Catalog errors and their HTTP mapping.
//!
//! [`CatalogError`] is the service-layer error; [`CatalogHttpError`] is the
//! only place it becomes a status. Client faults carry a safe message;
//! server and upstream faults carry the fixed body plus an error id, with
//! the cause in the structured log only.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::error::{
    FIXED_UPSTREAM_MESSAGE, INVALID_INPUT, UPSTREAM_ERROR, envelope_response, fault_response,
};

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

/// Handler-layer wrapper that owns the status mapping.
#[derive(Debug)]
pub struct CatalogHttpError {
    error: CatalogError,
    error_id: String,
}

impl CatalogHttpError {
    /// Pair a service error with the id its log line and body share.
    pub fn new(error: CatalogError, ids: &dyn crate::ids::IdGenerator) -> Self {
        Self {
            error,
            error_id: ids.new_id(),
        }
    }

    /// Reject a bad request parameter.
    pub fn invalid(message: impl Into<String>, ids: &dyn crate::ids::IdGenerator) -> Self {
        Self::new(CatalogError::Invalid(message.into()), ids)
    }
}

impl IntoResponse for CatalogHttpError {
    fn into_response(self) -> Response {
        match self.error {
            CatalogError::Invalid(message) => {
                envelope_response(StatusCode::BAD_REQUEST, INVALID_INPUT, message, None)
            }
            CatalogError::NotFound => crate::error::ApiError::NotFound.into_response(),
            CatalogError::Missing(message) => envelope_response(
                StatusCode::NOT_FOUND,
                crate::error::NOT_FOUND,
                message,
                None,
            ),
            CatalogError::Forbidden => envelope_response(
                StatusCode::FORBIDDEN,
                crate::error::FORBIDDEN,
                "Curator access required",
                None,
            ),
            CatalogError::Conflict(message) => {
                envelope_response(StatusCode::CONFLICT, crate::error::CONFLICT, message, None)
            }
            CatalogError::Unavailable(cause) => {
                tracing::warn!(error_id = %self.error_id, %cause, "catalog upstream unavailable");
                fault_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    UPSTREAM_ERROR,
                    FIXED_UPSTREAM_MESSAGE,
                    &self.error_id,
                )
            }
            CatalogError::Internal(cause) => {
                tracing::error!(error_id = %self.error_id, %cause, "catalog request failed");
                crate::error::ApiError::internal_response(&self.error_id)
            }
        }
    }
}
