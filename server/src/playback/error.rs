//! Playback-reporting slice errors rendered into the shared envelope.
//!
//! Status mapping for this slice lives here in the handler layer. Services
//! return domain failures that handlers convert; anything unexpected becomes
//! a fixed 5xx body naming an error id, with the cause going to the log only.
//! Reporting itself almost never errors: a failed forward is logged and
//! swallowed (v2 never fails the player for a report), so these errors cover
//! only auth, unknown tracks, and malformed bodies.

use axum::{
    Json,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    error::{ErrorBody, ErrorEnvelope},
    ids::IdGenerator,
};

/// Missing or invalid session.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Unknown track id.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// The request itself is wrong. The message is user-facing.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// A downstream provider failed. Body is fixed; the cause stays in the log.
pub const UPSTREAM_ERROR: &str = "UPSTREAM_ERROR";

/// Fixed 502 message. Never carries cause text, hosts, or paths.
pub const FIXED_UPSTREAM_MESSAGE: &str = "Upstream service error";

/// Challenge sent on every 401.
pub const WWW_AUTHENTICATE_BEARER: &str = "Bearer";

/// Every failure this slice can return to a caller.
#[derive(Debug)]
pub enum PlaybackError {
    /// No valid session. Carries the `WWW-Authenticate` challenge.
    Unauthorized {
        /// User-safe reason, e.g. "Authentication required".
        message: String,
    },
    /// Unknown track id. Always the same fixed message.
    NotFound,
    /// Bad input. The message is shown to the user.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Downstream failure. Fixed body plus the request-tied id.
    Upstream {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
    /// Server fault. Fixed body plus the request-tied id.
    Internal {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
}

impl PlaybackError {
    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "playback slice failed");
        Self::Internal { error_id }
    }

    /// Build a 502, logging the real cause with its id.
    pub fn upstream(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "playback slice upstream failed");
        Self::Upstream { error_id }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidInput { .. } => StatusCode::BAD_REQUEST,
            Self::Upstream { .. } => StatusCode::BAD_GATEWAY,
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
            Self::InvalidInput { message } => (INVALID_INPUT.to_owned(), message.clone(), None),
            Self::Upstream { error_id } => (
                UPSTREAM_ERROR.to_owned(),
                FIXED_UPSTREAM_MESSAGE.to_owned(),
                Some(json!({ "error_id": error_id })),
            ),
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

impl IntoResponse for PlaybackError {
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
    fn unauthorized_carries_bearer_challenge() {
        let response = PlaybackError::Unauthorized {
            message: "Authentication required".to_owned(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let challenge = response.headers().get(axum::http::header::WWW_AUTHENTICATE);
        assert_eq!(
            challenge.and_then(|value| value.to_str().ok()),
            Some(WWW_AUTHENTICATE_BEARER)
        );
    }

    #[test]
    fn server_faults_use_fixed_bodies_with_error_id() {
        for error in [
            PlaybackError::Internal {
                error_id: "a".to_owned(),
            },
            PlaybackError::Upstream {
                error_id: "b".to_owned(),
            },
        ] {
            let envelope = error.envelope();
            assert!(envelope.error.details.is_some());
            assert!(!envelope.error.message.contains('/'));
        }
    }
}
