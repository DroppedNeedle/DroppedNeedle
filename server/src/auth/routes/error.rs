//! Shared error shape for the auth HTTP routes.
//!
//! Every failure renders the shared `{"error": {code, message, details}}`
//! envelope. Codes and fixed messages mirror the sibling users slice so one
//! route never disagrees with the next; the one deliberate difference is the
//! upstream outage status: federated IdP outages are 503 here (the federated
//! slice contract and v2 both say 503), while the users slice uses 502 for
//! its Last.fm calls.

use crate::auth::users::error::UsersError;
use crate::error::{ErrorBody, ErrorEnvelope};
use crate::ids::IdGenerator;
use axum::{
    Json,
    extract::{FromRequest, Query, Request},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

/// Missing or invalid credential.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Valid credential lacking rights (and Plex poll rejections, v2 parity).
pub const FORBIDDEN: &str = "FORBIDDEN";
/// Unknown id.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// The request itself is wrong. The message is user-facing.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// The request conflicts with current state. The message is user-facing.
pub const CONFLICT: &str = "CONFLICT";
/// A body larger than the route allows.
pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
/// An upstream IdP failed. Body is fixed; the cause stays in the log.
pub const UPSTREAM_ERROR: &str = "UPSTREAM_ERROR";

/// Fixed 503 message. Never carries cause text, hosts, or paths.
pub const FIXED_UPSTREAM_MESSAGE: &str = "Upstream service error";
/// Fixed 413 message.
pub const FIXED_TOO_LARGE_MESSAGE: &str = "Request body too large";

/// Challenge sent on login-route 401s.
pub const WWW_AUTHENTICATE_BEARER: &str = "Bearer";

/// Every failure these routes can return.
#[derive(Debug)]
pub enum AuthRouteError {
    /// No valid credential. Login routes add the Bearer challenge header; the
    /// browser-redirect OIDC steps omit it (v2 parity).
    Unauthorized {
        /// User-safe reason.
        message: String,
        /// Whether to send `WWW-Authenticate: Bearer`.
        challenge: bool,
    },
    /// Valid credential lacking rights, or a rejected Plex poll.
    Forbidden {
        /// User-safe reason.
        message: String,
    },
    /// Unknown id. Always the same fixed message.
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
    /// Upstream IdP failure. Fixed body plus the request-tied id.
    Unavailable {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
    /// Server fault. Fixed body plus the request-tied id.
    Internal {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
}

impl AuthRouteError {
    /// 401 with the Bearer challenge header (credential-carrying routes).
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized {
            message: message.into(),
            challenge: true,
        }
    }

    /// 401 without a challenge (browser-redirect steps, v2 parity).
    pub fn auth_failed(message: impl Into<String>) -> Self {
        Self::Unauthorized {
            message: message.into(),
            challenge: false,
        }
    }

    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "auth route failed");
        Self::Internal { error_id }
    }

    /// Build a 503, logging the real cause with its id.
    pub fn unavailable(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "auth route upstream failed");
        Self::Unavailable { error_id }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidInput { .. } => StatusCode::BAD_REQUEST,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn envelope(&self) -> ErrorEnvelope {
        let (code, message, details) = match self {
            Self::Unauthorized { message, .. } => (UNAUTHORIZED.to_owned(), message.clone(), None),
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
            Self::Unavailable { error_id } => (
                UPSTREAM_ERROR.to_owned(),
                FIXED_UPSTREAM_MESSAGE.to_owned(),
                Some(serde_json::json!({ "error_id": error_id })),
            ),
            Self::Internal { error_id } => (
                crate::error::INTERNAL_ERROR.to_owned(),
                crate::error::FIXED_INTERNAL_MESSAGE.to_owned(),
                Some(serde_json::json!({ "error_id": error_id })),
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

impl From<UsersError> for AuthRouteError {
    fn from(error: UsersError) -> Self {
        match error {
            UsersError::Unauthorized { message } => Self::unauthorized(message),
            UsersError::Forbidden { message } => Self::Forbidden { message },
            UsersError::NotFound => Self::NotFound,
            UsersError::InvalidInput { message } => Self::InvalidInput { message },
            UsersError::Conflict { message } => Self::Conflict { message },
            UsersError::TooLarge => Self::TooLarge,
            UsersError::Upstream { error_id } => Self::Unavailable { error_id },
            UsersError::Internal { error_id } => Self::Internal { error_id },
        }
    }
}

impl IntoResponse for AuthRouteError {
    fn into_response(self) -> Response {
        let status = self.status();
        let challenge = matches!(
            &self,
            Self::Unauthorized {
                challenge: true,
                ..
            }
        );
        let mut response = (status, Json(self.envelope())).into_response();
        if challenge && let Ok(value) = HeaderValue::from_str(WWW_AUTHENTICATE_BEARER) {
            response
                .headers_mut()
                .insert(axum::http::header::WWW_AUTHENTICATE, value);
        }
        response
    }
}

/// Query extractor that renders failures in the shared envelope.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = AuthRouteError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| AuthRouteError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}
