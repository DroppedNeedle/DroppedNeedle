//! Admin-UX typed errors rendering the shared envelope.
//!
//! Shapes are byte-compatible with the crate `error` module: every failure
//! renders as `{"error": {code, message, details}}`. Server faults carry the
//! fixed generic message plus an error id; the cause goes to the log only.

use axum::{
    Json,
    extract::{FromRequest, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

/// Missing or invalid credential.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Valid credential lacking rights.
pub const FORBIDDEN: &str = "FORBIDDEN";
/// Missing resource.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// Bad input. Safe to show the message.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// Live-state refusal (a run is already going). Safe to show the message.
pub const CONFLICT: &str = "CONFLICT";
/// Backend not wired (test states only; production always wires).
pub const UPSTREAM_ERROR: &str = "UPSTREAM_ERROR";
/// Unspecified server fault.
pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";

/// User-facing 404 message, kept from v2.
pub const NOT_FOUND_MESSAGE: &str = "Not found";
/// Fixed 5xx message. The whole body contract for server faults.
pub const FIXED_INTERNAL_MESSAGE: &str = "Internal server error";

/// Handler-layer errors with their status mapping.
#[derive(Debug)]
pub enum AdminError {
    /// No usable credential.
    Unauthorized {
        /// Why the credential failed.
        message: String,
    },
    /// Credential lacks rights.
    Forbidden {
        /// What the credential lacks.
        message: String,
    },
    /// Missing resource.
    NotFound,
    /// Bad input with a user-safe message.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Live-state refusal with a user-safe message.
    Conflict {
        /// What is already going.
        message: String,
    },
    /// A backend this route needs is unwired.
    Unavailable {
        /// Which backend is missing.
        message: String,
    },
    /// Server fault. The id matches the log line.
    Internal {
        /// Log correlation id.
        error_id: String,
    },
}

impl AdminError {
    /// Build a server fault, logging the real cause with a fresh id.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        let error_id = uuid::Uuid::new_v4().to_string();
        tracing::error!(error_id, %cause, "admin request failed");
        Self::Internal { error_id }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let crate_body = |code: &str, message: String, details: Option<serde_json::Value>| {
            crate::error::ErrorEnvelope {
                error: crate::error::ErrorBody {
                    code: code.to_owned(),
                    message,
                    details,
                },
            }
        };
        match self {
            Self::Unauthorized { message } => (
                StatusCode::UNAUTHORIZED,
                [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
                Json(crate_body(UNAUTHORIZED, message, None)),
            )
                .into_response(),
            Self::Forbidden { message } => (
                StatusCode::FORBIDDEN,
                Json(crate_body(FORBIDDEN, message, None)),
            )
                .into_response(),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                Json(crate_body(NOT_FOUND, NOT_FOUND_MESSAGE.to_owned(), None)),
            )
                .into_response(),
            Self::InvalidInput { message } => (
                StatusCode::BAD_REQUEST,
                Json(crate_body(INVALID_INPUT, message, None)),
            )
                .into_response(),
            Self::Conflict { message } => (
                StatusCode::CONFLICT,
                Json(crate_body(CONFLICT, message, None)),
            )
                .into_response(),
            Self::Unavailable { message } => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(crate_body(UPSTREAM_ERROR, message, None)),
            )
                .into_response(),
            Self::Internal { error_id } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate_body(
                    INTERNAL_ERROR,
                    FIXED_INTERNAL_MESSAGE.to_owned(),
                    Some(serde_json::json!({ "error_id": error_id })),
                )),
            )
                .into_response(),
        }
    }
}

/// JSON body extractor that keeps malformed input inside the shared envelope
/// instead of Axum's default plain-text 400.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = AdminError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| AdminError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}
