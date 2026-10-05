//! Typed errors rendered into the shared envelope.
//!
//! Every failure renders as `{"error": {code, message, details}}` with a
//! SCREAMING_SNAKE code. Statuses match the other modules: 401 carries
//! the `Bearer` challenge, 4xx messages are user-safe, and 5xx bodies stay
//! fixed with only an error id while the cause goes to the structured log.

use crate::error::{
    ApiError, CONFLICT, FORBIDDEN, INVALID_INPUT, NOT_FOUND, NOT_FOUND_MESSAGE, envelope_response,
    unauthorized_response,
};
use crate::ids::IdGenerator;
use axum::{
    extract::{FromRequest, Query, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

/// No connection or settings stored for this import (Lidarr/Spotify).
pub const IMPORT_NOT_CONFIGURED: &str = "IMPORT_NOT_CONFIGURED";
/// The stored credential was rejected upstream (Lidarr 401/403).
pub const IMPORT_AUTH_FAILED: &str = "IMPORT_AUTH_FAILED";
/// The upstream answered with an error or garbage.
pub const IMPORT_UNAVAILABLE: &str = "IMPORT_UNAVAILABLE";

/// Every failure the imports routes can return to a caller.
#[derive(Debug)]
pub enum ImportsError {
    /// No valid session.
    Unauthorized {
        /// User-safe reason, e.g. "Authentication required".
        message: String,
    },
    /// Authenticated but not an admin.
    Forbidden {
        /// User-safe reason, e.g. "Admin access required".
        message: String,
    },
    /// Unknown id. Fixed message, no details.
    NotFound,
    /// Bad input. The message is shown to the user.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Valid input against the wrong state.
    Conflict {
        /// What was wrong.
        message: String,
    },
    /// Import has no stored connection.
    NotConfigured {
        /// Which import is missing, e.g. "Lidarr is not connected".
        message: String,
    },
    /// Upstream rejected the stored credential. Reconfigure, do not retry.
    AuthFailed {
        /// Which import rejected, without credential detail.
        message: String,
    },
    /// Upstream error or unreachable. User-safe summary only.
    Unavailable {
        /// Short summary, e.g. "Lidarr answered with an error".
        message: String,
    },
    /// Server fault. Fixed body plus the request-tied id.
    Internal {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
}

impl ImportsError {
    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "imports request failed");
        Self::Internal { error_id }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidInput { .. } | Self::NotConfigured { .. } => StatusCode::BAD_REQUEST,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            // Upstream credential rejection is a gateway failure, not a
            // caller-auth failure: the caller IS authenticated here, so no
            // Bearer challenge must fire.
            Self::AuthFailed { .. } => StatusCode::BAD_GATEWAY,
            Self::Unavailable { .. } => StatusCode::BAD_GATEWAY,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ImportsError {
    fn into_response(self) -> Response {
        let status = self.status();
        match self {
            Self::Unauthorized { message } => unauthorized_response(message),
            Self::Forbidden { message } => envelope_response(status, FORBIDDEN, message, None),
            Self::NotFound => envelope_response(status, NOT_FOUND, NOT_FOUND_MESSAGE, None),
            Self::InvalidInput { message } => {
                envelope_response(status, INVALID_INPUT, message, None)
            }
            Self::Conflict { message } => envelope_response(status, CONFLICT, message, None),
            Self::NotConfigured { message } => {
                envelope_response(status, IMPORT_NOT_CONFIGURED, message, None)
            }
            Self::AuthFailed { message } => {
                envelope_response(status, IMPORT_AUTH_FAILED, message, None)
            }
            Self::Unavailable { message } => {
                envelope_response(status, IMPORT_UNAVAILABLE, message, None)
            }
            Self::Internal { error_id } => ApiError::server_error_response(status, &error_id),
        }
    }
}

/// Query extractor that renders failures in the shared envelope instead of
/// Axum's default plain-text 400.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = ImportsError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| ImportsError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}

/// JSON body extractor with the same envelope guarantee.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = ImportsError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(req, state)
            .await
            .map(|axum::Json(value)| Self(value))
            .map_err(|cause| ImportsError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}
