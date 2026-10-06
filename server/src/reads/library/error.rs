//! Typed errors rendered into the shared envelope.
//!
//! Status mapping for the library reads lives here in the handler layer.
//! Services return [`LibraryFailure`](super::services::LibraryFailure);
//! handlers convert. Server faults carry the fixed generic body plus an
//! error id; the cause goes to the structured log only.

use crate::error::{ErrorBody, ErrorEnvelope};
use crate::ids::IdGenerator;
use axum::{
    Json,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;

/// Missing or invalid session.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Unknown id. Always the same fixed message.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// The request itself is wrong. The message is user-facing.
pub const INVALID_INPUT: &str = "INVALID_INPUT";

/// Challenge sent on every 401.
pub const WWW_AUTHENTICATE_BEARER: &str = "Bearer";

/// Every failure the library reads can return to a caller.
#[derive(Debug)]
pub enum LibraryError {
    /// No valid session. Carries the `WWW-Authenticate` challenge.
    Unauthorized {
        /// User-safe reason, e.g. "Authentication required".
        message: String,
    },
    /// Unknown id. Fixed message, no details.
    NotFound,
    /// The caller's role may not do this, or the file left the library
    /// roots. The message is shown to the user.
    Forbidden {
        /// Why.
        message: String,
    },
    /// Bad input. The message is shown to the user.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Server fault. Fixed body plus the request-tied id.
    Internal {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
}

impl LibraryError {
    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "library request failed");
        Self::Internal { error_id }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::InvalidInput { .. } => StatusCode::BAD_REQUEST,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn envelope(&self) -> ErrorEnvelope {
        let (code, message, details) = match self {
            Self::Unauthorized { message } => (UNAUTHORIZED.to_owned(), message.clone(), None),
            Self::NotFound => (
                NOT_FOUND.to_owned(),
                crate::error::NOT_FOUND_MESSAGE.to_owned(),
                None,
            ),
            Self::Forbidden { message } => {
                (crate::error::FORBIDDEN.to_owned(), message.clone(), None)
            }
            Self::InvalidInput { message } => (INVALID_INPUT.to_owned(), message.clone(), None),
            Self::Internal { error_id } => (
                crate::error::INTERNAL_ERROR.to_owned(),
                crate::error::FIXED_INTERNAL_MESSAGE.to_owned(),
                Some(json!({ "error_id": error_id })),
            ),
        };
        ErrorEnvelope {
            error: ErrorBody {
                code,
                message,
                details,
            },
        }
    }
}

impl IntoResponse for LibraryError {
    fn into_response(self) -> Response {
        let status = self.status();
        let mut response = (status, Json(self.envelope())).into_response();
        if status == StatusCode::UNAUTHORIZED
            && let Ok(challenge) = HeaderValue::from_str(WWW_AUTHENTICATE_BEARER)
        {
            response
                .headers_mut()
                .insert(axum::http::header::WWW_AUTHENTICATE, challenge);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_maps_to_401_with_challenge_header() {
        let response = LibraryError::Unauthorized {
            message: "Authentication required".to_owned(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(axum::http::header::WWW_AUTHENTICATE),
            Some(&HeaderValue::from_static("Bearer")),
        );
    }

    #[test]
    fn internal_body_is_fixed_with_error_id_only() {
        let error = LibraryError::Internal {
            error_id: "e1".to_owned(),
        };
        let envelope = error.envelope();
        assert_eq!(envelope.error.code, crate::error::INTERNAL_ERROR);
        assert_eq!(envelope.error.message, crate::error::FIXED_INTERNAL_MESSAGE);
        assert_eq!(envelope.error.details, Some(json!({ "error_id": "e1" })));
    }
}
