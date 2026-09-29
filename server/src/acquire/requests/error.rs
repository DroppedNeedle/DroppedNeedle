//! Slice-local typed errors rendering the shared envelope.
//!
//! Shapes are byte-compatible with the crate `error` module: every failure
//! renders as `{"error": {code, message, details}}`. Server faults carry the
//! fixed generic message plus an error id; the cause goes to the log only.
//! Wiring note: when this slice mounts into the app router, swap these
//! variants for the crate-level error type and keep the codes.

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
/// Known route hit with the wrong method.
pub const METHOD_NOT_ALLOWED: &str = "METHOD_NOT_ALLOWED";
/// Bad input. Safe to show the message.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// Valid input against the wrong state.
pub const CONFLICT: &str = "CONFLICT";
/// Rolling request-count quota spent. Maps to 429; details carry the window.
pub const QUOTA_EXCEEDED: &str = "QUOTA_EXCEEDED";
/// Library or personal storage budget full. Maps to 403; details carry usage.
pub const STORAGE_FULL: &str = "STORAGE_FULL";
/// Unspecified server fault.
pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";

/// User-facing 404 message, kept from v2.
pub const NOT_FOUND_MESSAGE: &str = "Not found";
/// Fixed 405 message.
pub const METHOD_NOT_ALLOWED_MESSAGE: &str = "Method not allowed";
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
pub enum RequestsError {
    /// No usable credential.
    Unauthorized { message: String },
    /// Credential lacks rights.
    Forbidden { message: String },
    /// Missing resource.
    NotFound,
    /// Bad input with a user-safe message.
    InvalidInput { message: String },
    /// Valid input against the wrong state.
    Conflict { message: String },
    /// Request-count quota spent.
    QuotaExceeded { message: String, details: Value },
    /// Storage budget full.
    StorageFull { message: String, details: Value },
    /// Server fault. The id matches the log line.
    Internal { error_id: String },
}

impl RequestsError {
    /// Build a server fault, logging the real cause with a fresh id. The id
    /// is minted here because the slice router has no request-scope layer;
    /// wiring passes the request id instead.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        let error_id = uuid::Uuid::new_v4().to_string();
        tracing::error!(error_id, %cause, "requests request failed");
        Self::Internal { error_id }
    }
}

impl From<RequestsError> for ErrorEnvelope {
    fn from(error: RequestsError) -> Self {
        let body = match error {
            RequestsError::Unauthorized { message } => ErrorBody {
                code: UNAUTHORIZED.to_owned(),
                message,
                details: None,
            },
            RequestsError::Forbidden { message } => ErrorBody {
                code: FORBIDDEN.to_owned(),
                message,
                details: None,
            },
            RequestsError::NotFound => ErrorBody {
                code: NOT_FOUND.to_owned(),
                message: NOT_FOUND_MESSAGE.to_owned(),
                details: None,
            },
            RequestsError::InvalidInput { message } => ErrorBody {
                code: INVALID_INPUT.to_owned(),
                message,
                details: None,
            },
            RequestsError::Conflict { message } => ErrorBody {
                code: CONFLICT.to_owned(),
                message,
                details: None,
            },
            RequestsError::QuotaExceeded { message, details } => ErrorBody {
                code: QUOTA_EXCEEDED.to_owned(),
                message,
                details: Some(details),
            },
            RequestsError::StorageFull { message, details } => ErrorBody {
                code: STORAGE_FULL.to_owned(),
                message,
                details: Some(details),
            },
            RequestsError::Internal { error_id } => ErrorBody {
                code: INTERNAL_ERROR.to_owned(),
                message: FIXED_INTERNAL_MESSAGE.to_owned(),
                details: Some(json!({ "error_id": error_id })),
            },
        };
        Self { error: body }
    }
}

impl IntoResponse for RequestsError {
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
            Self::QuotaExceeded { .. } => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(ErrorEnvelope::from(self)),
            )
                .into_response(),
            Self::StorageFull { .. } => {
                (StatusCode::FORBIDDEN, Json(ErrorEnvelope::from(self))).into_response()
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
    type Rejection = RequestsError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| RequestsError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}

/// Query extractor that keeps malformed input inside the shared envelope
/// instead of Axum's default plain-text 400. All query access goes through
/// this; no bare `Query` appears in handlers.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = RequestsError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| RequestsError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}

/// 404 fallback for the slice test router. Wiring uses the app fallback.
pub async fn fallback_404() -> Response {
    RequestsError::NotFound.into_response()
}

/// 405 fallback for the slice test router. Wiring uses the app fallback.
pub async fn fallback_405() -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: METHOD_NOT_ALLOWED.to_owned(),
            message: METHOD_NOT_ALLOWED_MESSAGE.to_owned(),
            details: None,
        },
    };
    (StatusCode::METHOD_NOT_ALLOWED, Json(body)).into_response()
}
