//! Typed API errors and the shared error envelope.
//!
//! Every native error renders as `{"error": {code, message, details}}` with a
//! SCREAMING_SNAKE code. Client faults (4xx) carry a message safe for users.
//! Server faults (5xx) carry a fixed generic string plus the request's id in
//! `details.error_id`; the real cause goes to the structured log only, never
//! to the wire. Status mapping lives here in the handler layer; services
//! return domain errors that callers convert.

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

/// Resource or route not found.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// Known route hit with the wrong method.
pub const METHOD_NOT_ALLOWED: &str = "METHOD_NOT_ALLOWED";
/// Unspecified server fault. More 5xx codes arrive with their features.
pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";
/// Missing or invalid credential.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Valid credential lacking rights.
pub const FORBIDDEN: &str = "FORBIDDEN";
/// The request itself is wrong. The message is user-facing.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// The request conflicts with current state. The message is user-facing.
pub const CONFLICT: &str = "CONFLICT";
/// A body larger than the route allows.
pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
/// A downstream service failed. Body is fixed; the cause stays in the log.
pub const UPSTREAM_ERROR: &str = "UPSTREAM_ERROR";

/// User-facing 404 message, kept from v2.
pub const NOT_FOUND_MESSAGE: &str = "Not found";
/// Fixed 405 message.
pub const METHOD_NOT_ALLOWED_MESSAGE: &str = "Method not allowed";
/// Fixed 5xx message. This string is the whole body contract for server
/// faults: no cause text, hosts, or paths may join it.
pub const FIXED_INTERNAL_MESSAGE: &str = "Internal server error";
/// Fixed upstream-fault message. Never carries cause text, hosts, or paths.
pub const FIXED_UPSTREAM_MESSAGE: &str = "Upstream service error";
/// Fixed 413 message.
pub const FIXED_TOO_LARGE_MESSAGE: &str = "Request body too large";

/// The `error` object inside every error response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorBody {
    /// SCREAMING_SNAKE machine code.
    pub code: String,
    /// Human message. Fixed for 5xx, descriptive for 4xx.
    pub message: String,
    /// Extra detail, or null when there is none.
    pub details: Option<Value>,
}

/// Top-level error envelope.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorEnvelope {
    /// The single error payload.
    pub error: ErrorBody,
}

/// Render one error envelope. Native error bodies are built here and
/// nowhere else, so every module emits the same shape.
pub fn envelope_response(
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: code.to_owned(),
            message: message.into(),
            details,
        },
    };
    (status, Json(body)).into_response()
}

/// A server or upstream fault: fixed message, the error id in `details`.
pub fn fault_response(status: StatusCode, code: &str, message: &str, error_id: &str) -> Response {
    envelope_response(status, code, message, Some(json!({ "error_id": error_id })))
}

/// 401 in the shared envelope with the `WWW-Authenticate: Bearer` challenge.
pub fn unauthorized_response(message: impl Into<String>) -> Response {
    let mut response = envelope_response(StatusCode::UNAUTHORIZED, UNAUTHORIZED, message, None);
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

/// 503 with `Retry-After: 1` for lock contention that outlived the busy
/// timeout. The body is the fixed 5xx envelope, so the request-scope
/// rewrite leaves it unchanged. The operation name is logged, never sent.
pub fn busy_response(operation: &str, request_id: &str) -> Response {
    tracing::warn!(
        operation,
        request_id,
        "database busy; answering 503 with retry"
    );
    let mut response = ApiError::server_error_response(StatusCode::SERVICE_UNAVAILABLE, request_id);
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

/// Handler-layer errors with their status mapping.
#[derive(Debug)]
pub enum ApiError {
    /// Unknown route or missing resource.
    NotFound,
    /// Known route, wrong method.
    MethodNotAllowed,
    /// Server fault. The id matches the request id header and the log line.
    Internal { error_id: String },
}

impl ApiError {
    /// Build a server fault, logging the real cause with its id. Call sites
    /// pass the request id from the `RequestId` extension.
    pub fn internal(cause: &dyn std::fmt::Display, error_id: &str) -> Self {
        tracing::error!(error_id, %cause, "request failed");
        Self::Internal {
            error_id: error_id.to_owned(),
        }
    }

    /// Render the fixed 5xx envelope for one request id.
    pub fn internal_response(error_id: &str) -> Response {
        Self::server_error_response(StatusCode::INTERNAL_SERVER_ERROR, error_id)
    }

    /// Render the fixed 5xx envelope keeping the original status. Only the
    /// body is fixed; the status keeps its distinct 5xx meaning (a 503 stays
    /// a 503). Callers pass a server-error status.
    pub fn server_error_response(status: StatusCode, error_id: &str) -> Response {
        fault_response(status, INTERNAL_ERROR, FIXED_INTERNAL_MESSAGE, error_id)
    }
}

impl From<ApiError> for ErrorEnvelope {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::NotFound => Self {
                error: ErrorBody {
                    code: NOT_FOUND.to_owned(),
                    message: NOT_FOUND_MESSAGE.to_owned(),
                    details: None,
                },
            },
            ApiError::MethodNotAllowed => Self {
                error: ErrorBody {
                    code: METHOD_NOT_ALLOWED.to_owned(),
                    message: METHOD_NOT_ALLOWED_MESSAGE.to_owned(),
                    details: None,
                },
            },
            ApiError::Internal { error_id } => Self {
                error: ErrorBody {
                    code: INTERNAL_ERROR.to_owned(),
                    message: FIXED_INTERNAL_MESSAGE.to_owned(),
                    details: Some(json!({ "error_id": error_id })),
                },
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            ApiError::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(ErrorEnvelope::from(self))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_maps_to_fixed_500_envelope_with_error_id() {
        let envelope = ErrorEnvelope::from(ApiError::Internal {
            error_id: "abc".to_owned(),
        });
        assert_eq!(envelope.error.code, INTERNAL_ERROR);
        assert_eq!(envelope.error.message, FIXED_INTERNAL_MESSAGE);
        assert_eq!(envelope.error.details, Some(json!({ "error_id": "abc" })));
    }

    #[test]
    fn busy_response_carries_503_and_retry_after() {
        let response = busy_response("op", "req-1");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "1");
    }
}
