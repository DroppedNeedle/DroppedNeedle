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
    http::StatusCode,
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

/// User-facing 404 message, kept from v2.
pub const NOT_FOUND_MESSAGE: &str = "Not found";
/// Fixed 405 message.
pub const METHOD_NOT_ALLOWED_MESSAGE: &str = "Method not allowed";
/// Fixed 5xx message. This string is the whole body contract for server
/// faults: no cause text, hosts, or paths may join it.
pub const FIXED_INTERNAL_MESSAGE: &str = "Internal server error";

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
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: INTERNAL_ERROR.to_owned(),
                message: FIXED_INTERNAL_MESSAGE.to_owned(),
                details: Some(json!({ "error_id": error_id })),
            },
        };
        (status, Json(body)).into_response()
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
    fn not_found_maps_to_404_envelope_without_details() {
        let envelope = ErrorEnvelope::from(ApiError::NotFound);
        assert_eq!(envelope.error.code, NOT_FOUND);
        assert_eq!(envelope.error.message, NOT_FOUND_MESSAGE);
        assert_eq!(envelope.error.details, None);
    }

    #[test]
    fn method_not_allowed_maps_to_405_envelope_without_details() {
        let envelope = ErrorEnvelope::from(ApiError::MethodNotAllowed);
        assert_eq!(envelope.error.code, METHOD_NOT_ALLOWED);
        assert_eq!(envelope.error.message, METHOD_NOT_ALLOWED_MESSAGE);
        assert_eq!(envelope.error.details, None);
    }

    #[test]
    fn internal_maps_to_fixed_500_envelope_with_error_id() {
        let envelope = ErrorEnvelope::from(ApiError::Internal {
            error_id: "abc".to_owned(),
        });
        assert_eq!(envelope.error.code, INTERNAL_ERROR);
        assert_eq!(envelope.error.message, FIXED_INTERNAL_MESSAGE);
        assert_eq!(envelope.error.details, Some(json!({ "error_id": "abc" })));
    }
}
