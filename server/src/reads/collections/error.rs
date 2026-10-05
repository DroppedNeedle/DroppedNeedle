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
/// Bad input. Safe to show the message. The code matches the other reads
/// slices (`INVALID_INPUT` everywhere).
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// State conflict (e.g. auto-download without a follow).
pub const CONFLICT: &str = "CONFLICT";
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
pub enum CollectionsError {
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

impl CollectionsError {
    /// Build a server fault, logging the real cause with a fresh id. The id
    /// is minted here because the slice router has no request-scope layer;
    /// wiring passes the request id instead.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        let error_id = uuid::Uuid::new_v4().to_string();
        tracing::error!(error_id, %cause, "collections request failed");
        Self::Internal { error_id }
    }

    /// Render the fixed 5xx envelope for one error id.
    pub fn internal_response(error_id: &str) -> Response {
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: INTERNAL_ERROR.to_owned(),
                message: FIXED_INTERNAL_MESSAGE.to_owned(),
                details: Some(json!({ "error_id": error_id })),
            },
        };
        (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
    }
}

impl From<CollectionsError> for ErrorEnvelope {
    fn from(error: CollectionsError) -> Self {
        let body = match error {
            CollectionsError::Unauthorized { message } => ErrorBody {
                code: UNAUTHORIZED.to_owned(),
                message,
                details: None,
            },
            CollectionsError::Forbidden { message } => ErrorBody {
                code: FORBIDDEN.to_owned(),
                message,
                details: None,
            },
            CollectionsError::NotFound => ErrorBody {
                code: NOT_FOUND.to_owned(),
                message: NOT_FOUND_MESSAGE.to_owned(),
                details: None,
            },
            CollectionsError::InvalidInput { message } => ErrorBody {
                code: INVALID_INPUT.to_owned(),
                message,
                details: None,
            },
            CollectionsError::Conflict { message } => ErrorBody {
                code: CONFLICT.to_owned(),
                message,
                details: None,
            },
            CollectionsError::Internal { error_id } => ErrorBody {
                code: INTERNAL_ERROR.to_owned(),
                message: FIXED_INTERNAL_MESSAGE.to_owned(),
                details: Some(json!({ "error_id": error_id })),
            },
        };
        Self { error: body }
    }
}

impl IntoResponse for CollectionsError {
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
    type Rejection = CollectionsError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| CollectionsError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}

/// Query extractor that keeps malformed input inside the shared envelope
/// instead of Axum's default plain-text 400.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = CollectionsError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| CollectionsError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}

/// 404 fallback for tests that mount the routes alone; the app has its own.
#[cfg(any(test, feature = "test-support"))]
pub async fn fallback_404() -> Response {
    CollectionsError::NotFound.into_response()
}

/// 405 fallback for tests that mount the routes alone; the app has its own.
#[cfg(any(test, feature = "test-support"))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_response_keeps_the_fixed_500_contract() {
        let response = CollectionsError::internal_response("req-1");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn variants_map_to_envelope_codes() {
        let cases = [
            (
                CollectionsError::Unauthorized {
                    message: "m".to_owned(),
                },
                UNAUTHORIZED,
            ),
            (
                CollectionsError::Forbidden {
                    message: "m".to_owned(),
                },
                FORBIDDEN,
            ),
            (CollectionsError::NotFound, NOT_FOUND),
            (
                CollectionsError::InvalidInput {
                    message: "m".to_owned(),
                },
                INVALID_INPUT,
            ),
            (
                CollectionsError::Conflict {
                    message: "m".to_owned(),
                },
                CONFLICT,
            ),
            (
                CollectionsError::Internal {
                    error_id: "e".to_owned(),
                },
                INTERNAL_ERROR,
            ),
        ];
        for (error, code) in cases {
            assert_eq!(ErrorEnvelope::from(error).error.code, code);
        }
        let internal = ErrorEnvelope::from(CollectionsError::Internal {
            error_id: "e".to_owned(),
        });
        assert_eq!(internal.error.message, FIXED_INTERNAL_MESSAGE);
    }
}
