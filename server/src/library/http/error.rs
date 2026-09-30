//! Library typed errors rendering the shared envelope.
//!
//! Shapes are byte-compatible with the crate `error` module plus the
//! 4xx codes the sibling slices use: every failure renders as
//! `{"error": {code, message, details}}`. Server faults carry the
//! fixed generic message plus an error id; the cause goes to the log
//! only.

use axum::{
    Json,
    extract::{FromRequest, Query, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// Missing or invalid credential.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Valid credential lacking rights.
pub const FORBIDDEN: &str = "FORBIDDEN";
/// Missing resource.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// Bad input. Safe to show the message.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// Valid input against the wrong state.
pub const CONFLICT: &str = "CONFLICT";
/// Unspecified server fault.
pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";

/// User-facing 404 message, kept from v2.
pub const NOT_FOUND_MESSAGE: &str = "Not found";
/// Fixed 5xx message. The whole body contract for server faults.
pub const FIXED_INTERNAL_MESSAGE: &str = "Internal server error";

/// The `error` object inside every error response.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    /// SCREAMING_SNAKE machine code.
    pub code: String,
    /// Human message. Fixed for 5xx, descriptive for 4xx.
    pub message: String,
    /// Extra detail, or null when there is none.
    pub details: Option<Value>,
}

/// Top-level error envelope.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    /// The single error payload.
    pub error: ErrorBody,
}

/// Handler-layer errors with their status mapping.
#[derive(Debug)]
pub enum LibraryError {
    /// No usable credential.
    Unauthorized { message: String },
    /// Credential lacks rights.
    Forbidden { message: String },
    /// Missing resource (also used to hide private resources).
    NotFound,
    /// Bad input with a user-safe message.
    InvalidInput { message: String },
    /// Valid input against the wrong state.
    Conflict { message: String },
    /// Server fault. The id matches the log line.
    Internal { error_id: String },
}

impl LibraryError {
    /// Build a server fault, logging the real cause with a fresh id.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        let error_id = uuid::Uuid::new_v4().to_string();
        tracing::error!(error_id, %cause, "library request failed");
        Self::Internal { error_id }
    }
}

impl From<LibraryError> for ErrorEnvelope {
    fn from(error: LibraryError) -> Self {
        let body = match error {
            LibraryError::Unauthorized { message } => ErrorBody {
                code: UNAUTHORIZED.to_owned(),
                message,
                details: None,
            },
            LibraryError::Forbidden { message } => ErrorBody {
                code: FORBIDDEN.to_owned(),
                message,
                details: None,
            },
            LibraryError::NotFound => ErrorBody {
                code: NOT_FOUND.to_owned(),
                message: NOT_FOUND_MESSAGE.to_owned(),
                details: None,
            },
            LibraryError::InvalidInput { message } => ErrorBody {
                code: INVALID_INPUT.to_owned(),
                message,
                details: None,
            },
            LibraryError::Conflict { message } => ErrorBody {
                code: CONFLICT.to_owned(),
                message,
                details: None,
            },
            LibraryError::Internal { error_id } => ErrorBody {
                code: INTERNAL_ERROR.to_owned(),
                message: FIXED_INTERNAL_MESSAGE.to_owned(),
                details: Some(json!({ "error_id": error_id })),
            },
        };
        Self { error: body }
    }
}

impl IntoResponse for LibraryError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized { .. } => (
                StatusCode::UNAUTHORIZED,
                [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
                Json(ErrorEnvelope::from(self)),
            )
                .into_response(),
            Self::Forbidden { .. } => {
                (StatusCode::FORBIDDEN, Json(ErrorEnvelope::from(self))).into_response()
            }
            Self::NotFound => {
                (StatusCode::NOT_FOUND, Json(ErrorEnvelope::from(self))).into_response()
            }
            Self::InvalidInput { .. } => {
                (StatusCode::BAD_REQUEST, Json(ErrorEnvelope::from(self))).into_response()
            }
            Self::Conflict { .. } => {
                (StatusCode::CONFLICT, Json(ErrorEnvelope::from(self))).into_response()
            }
            Self::Internal { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorEnvelope::from(self)),
            )
                .into_response(),
        }
    }
}

/// JSON body extractor that keeps malformed input inside the shared envelope
/// instead of Axum's default plain-text 400.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = LibraryError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| LibraryError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}

/// Query extractor that keeps malformed input inside the shared envelope
/// instead of Axum's default plain-text 400.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = LibraryError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| LibraryError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}
