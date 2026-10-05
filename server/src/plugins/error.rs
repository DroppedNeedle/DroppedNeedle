//! Slice errors rendered into the shared envelope.
//!
//! Status mapping for the plugin and scrobble-settings routes lives here
//! in the handler layer. Services return domain failures that handlers
//! convert; anything unexpected becomes a fixed 5xx body naming an error
//! id, with the cause going to the log only.

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
/// Authenticated but not allowed.
pub const FORBIDDEN: &str = "FORBIDDEN";
/// Unknown plugin, route, or link.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// The request itself is wrong. The message is user-facing.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// The request body is over the cap.
pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
/// The caller is over the route's per-minute budget.
pub const RATE_LIMITED: &str = "RATE_LIMITED";
/// A plugin route failed. Fixed body, like v2.
pub const ROUTE_FAILED: &str = "EXTERNAL_SERVICE_UNAVAILABLE";

/// Fixed plugin-route failure message. Never carries cause text.
pub const FIXED_ROUTE_FAILED_MESSAGE: &str = "Plugin route failed";
/// Fixed oversize-body message.
pub const FIXED_TOO_LARGE_MESSAGE: &str = "Request body too large";
/// Fixed rate-limit message.
pub const FIXED_RATE_LIMITED_MESSAGE: &str = "Too many requests";

/// Challenge sent on every 401.
pub const WWW_AUTHENTICATE_BEARER: &str = "Bearer";

/// Every failure the plugin routes can return to a caller.
#[derive(Debug)]
pub enum PluginError {
    /// No valid session. Carries the `WWW-Authenticate` challenge.
    Unauthorized {
        /// User-safe reason, e.g. "Authentication required".
        message: String,
    },
    /// Valid session, wrong role.
    Forbidden {
        /// User-safe reason, e.g. "Admin access required".
        message: String,
    },
    /// Unknown plugin, route, or link. Always the same fixed message, so
    /// disabled plugins are indistinguishable from missing ones.
    NotFound,
    /// Bad input. The message is shown to the user.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Request body over the cap.
    PayloadTooLarge,
    /// Over the route's per-minute budget.
    RateLimited {
        /// Seconds the caller should wait.
        retry_after: u64,
    },
    /// A plugin route failed. Fixed body, never the plugin's error text.
    RouteFailed,
    /// Server fault. Fixed body plus the request-tied id.
    Internal {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
}

impl PluginError {
    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "plugins request failed");
        Self::Internal { error_id }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidInput { .. } => StatusCode::BAD_REQUEST,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::RouteFailed => StatusCode::BAD_GATEWAY,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn envelope(&self) -> ErrorEnvelope {
        let (code, message, details) = match self {
            Self::Unauthorized { message } | Self::Forbidden { message } => (
                if matches!(self, Self::Unauthorized { .. }) {
                    UNAUTHORIZED.to_owned()
                } else {
                    FORBIDDEN.to_owned()
                },
                message.clone(),
                None,
            ),
            Self::NotFound => (
                NOT_FOUND.to_owned(),
                crate::error::NOT_FOUND_MESSAGE.to_owned(),
                None,
            ),
            Self::InvalidInput { message } => (INVALID_INPUT.to_owned(), message.clone(), None),
            Self::PayloadTooLarge => (
                PAYLOAD_TOO_LARGE.to_owned(),
                FIXED_TOO_LARGE_MESSAGE.to_owned(),
                None,
            ),
            Self::RateLimited { .. } => (
                RATE_LIMITED.to_owned(),
                FIXED_RATE_LIMITED_MESSAGE.to_owned(),
                None,
            ),
            Self::RouteFailed => (
                ROUTE_FAILED.to_owned(),
                FIXED_ROUTE_FAILED_MESSAGE.to_owned(),
                None,
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

impl IntoResponse for PluginError {
    fn into_response(self) -> Response {
        let status = self.status();
        let retry_after = match &self {
            Self::RateLimited { retry_after } => Some(*retry_after),
            _ => None,
        };
        let mut response = (status, Json(self.envelope())).into_response();
        if status == StatusCode::UNAUTHORIZED
            && let Ok(challenge) = HeaderValue::from_str(WWW_AUTHENTICATE_BEARER)
        {
            response
                .headers_mut()
                .insert(axum::http::header::WWW_AUTHENTICATE, challenge);
        }
        if let Some(seconds) = retry_after
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert("retry-after", value);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_carries_retry_after() {
        let response = PluginError::RateLimited { retry_after: 7 }.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let header = response.headers().get("retry-after");
        assert_eq!(header.and_then(|value| value.to_str().ok()), Some("7"));
    }

    #[test]
    fn server_faults_use_fixed_bodies_with_error_id() {
        let envelope = PluginError::Internal {
            error_id: "a".to_owned(),
        }
        .envelope();
        assert!(envelope.error.details.is_some());
        assert!(!envelope.error.message.contains('/'));
    }
}
