//! Users-slice errors rendered into the shared envelope.
//!
//! Status mapping for this slice lives here in the handler layer. Services
//! return domain failures that handlers convert; anything unexpected becomes
//! a fixed 5xx body naming an error id, with the cause going to the log only.

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
/// Valid session lacking the role (or the wrong session kind).
pub const FORBIDDEN: &str = "FORBIDDEN";
/// Unknown id, or a foreign id the caller must not learn about.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// The request itself is wrong. The message is user-facing.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// The request conflicts with current state. The message is user-facing.
pub const CONFLICT: &str = "CONFLICT";
/// A body larger than the route allows.
pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
/// A downstream service failed. Body is fixed; the cause stays in the log.
pub const UPSTREAM_ERROR: &str = "UPSTREAM_ERROR";

/// Fixed 502 message. Never carries cause text, hosts, or paths.
pub const FIXED_UPSTREAM_MESSAGE: &str = "Upstream service error";
/// Fixed 413 message.
pub const FIXED_TOO_LARGE_MESSAGE: &str = "Request body too large";

/// Challenge sent on every 401.
pub const WWW_AUTHENTICATE_BEARER: &str = "Bearer";

/// Every failure this slice can return to a caller.
#[derive(Debug)]
pub enum UsersError {
    /// No valid session. Carries the `WWW-Authenticate` challenge.
    Unauthorized {
        /// User-safe reason, e.g. "Authentication required".
        message: String,
    },
    /// Valid session lacking the role, or a companion token minting.
    Forbidden {
        /// User-safe reason.
        message: String,
    },
    /// Unknown or foreign id. Always the same fixed message.
    NotFound,
    /// Bad input. The message is shown to the user.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// State conflict. The message is shown to the user.
    Conflict {
        /// What conflicted.
        message: String,
    },
    /// Oversize body.
    TooLarge,
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

impl UsersError {
    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "users slice failed");
        Self::Internal { error_id }
    }

    /// Build a 502, logging the real cause with its id.
    pub fn upstream(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "users slice upstream failed");
        Self::Upstream { error_id }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidInput { .. } => StatusCode::BAD_REQUEST,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Upstream { .. } => StatusCode::BAD_GATEWAY,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn envelope(&self) -> ErrorEnvelope {
        let (code, message, details) = match self {
            Self::Unauthorized { message } => (UNAUTHORIZED.to_owned(), message.clone(), None),
            Self::Forbidden { message } => (FORBIDDEN.to_owned(), message.clone(), None),
            Self::NotFound => (
                NOT_FOUND.to_owned(),
                crate::error::NOT_FOUND_MESSAGE.to_owned(),
                None,
            ),
            Self::InvalidInput { message } => (INVALID_INPUT.to_owned(), message.clone(), None),
            Self::Conflict { message } => (CONFLICT.to_owned(), message.clone(), None),
            Self::TooLarge => (
                PAYLOAD_TOO_LARGE.to_owned(),
                FIXED_TOO_LARGE_MESSAGE.to_owned(),
                None,
            ),
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
                code: code.to_owned(),
                message,
                details,
            },
        }
    }
}

impl IntoResponse for UsersError {
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
        let response = UsersError::Unauthorized {
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
    fn forbidden_carries_no_challenge() {
        let response = UsersError::Forbidden {
            message: "Admins only".to_owned(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            response
                .headers()
                .get(axum::http::header::WWW_AUTHENTICATE)
                .is_none()
        );
    }

    #[test]
    fn upstream_body_is_fixed_with_error_id_only() {
        let error = UsersError::Upstream {
            error_id: "e1".to_owned(),
        };
        let envelope = error.envelope();
        assert_eq!(envelope.error.code, UPSTREAM_ERROR);
        assert_eq!(envelope.error.message, FIXED_UPSTREAM_MESSAGE);
        assert_eq!(envelope.error.details, Some(json!({ "error_id": "e1" })));
    }
}
