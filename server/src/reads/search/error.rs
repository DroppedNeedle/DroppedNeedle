//! Search errors and their envelope mapping.
//!
//! Status mapping lives here in the handler layer. Client faults (400,
//! 404) carry a message safe for users; server faults carry the fixed
//! generic body plus an error id, with the cause in the structured log.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Client sent a blank query, an out-of-range limit, or a bad bucket filter.
/// The code matches the other reads slices (`INVALID_INPUT` everywhere).
pub const INVALID_INPUT: &str = "INVALID_INPUT";

/// Handler-layer search errors.
#[derive(Debug)]
pub enum SearchError {
    /// Bad query text, limit, or bucket filter.
    InvalidQuery {
        /// User-safe reason.
        message: String,
    },
    /// Unknown drill-down bucket.
    UnknownBucket,
    /// Server fault. The id matches the structured log line.
    Internal {
        /// Correlation id for the log line.
        error_id: String,
    },
}

impl SearchError {
    /// Build a server fault, logging the real cause with its id. Call sites
    /// pass the id generator from the search deps.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn crate::ids::IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "search request failed");
        Self::Internal { error_id }
    }
}

impl IntoResponse for SearchError {
    fn into_response(self) -> Response {
        match self {
            SearchError::InvalidQuery { message } => {
                let body = crate::error::ErrorEnvelope {
                    error: crate::error::ErrorBody {
                        code: INVALID_INPUT.to_owned(),
                        message,
                        details: None,
                    },
                };
                (StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
            }
            SearchError::UnknownBucket => crate::error::ApiError::NotFound.into_response(),
            SearchError::Internal { error_id } => crate::error::ApiError::server_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &error_id,
            ),
        }
    }
}
