//! HTTP errors for the auth routes.
//!
//! Every failure renders through the shared envelope in [`crate::error`].
//! Codes and fixed messages match the users routes; the one intended
//! difference is the upstream outage status: federated IdP outages are 503
//! here (the federated contract and v2 both say 503), while the users routes
//! use 502 for their Last.fm calls.

use crate::auth::users::error::UsersError;
use crate::error::{
    CONFLICT, FIXED_INTERNAL_MESSAGE, FIXED_TOO_LARGE_MESSAGE, FIXED_UPSTREAM_MESSAGE, FORBIDDEN,
    INTERNAL_ERROR, INVALID_INPUT, NOT_FOUND, NOT_FOUND_MESSAGE, PAYLOAD_TOO_LARGE, UNAUTHORIZED,
    UPSTREAM_ERROR, envelope_response, fault_response, unauthorized_response,
};
use crate::ids::IdGenerator;
use axum::{
    extract::{FromRequest, Query, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

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
            UsersError::Unavailable { error_id } | UsersError::Upstream { error_id } => {
                Self::Unavailable { error_id }
            }
            UsersError::Internal { error_id } => Self::Internal { error_id },
        }
    }
}

impl IntoResponse for AuthRouteError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized {
                message,
                challenge: true,
            } => unauthorized_response(message),
            Self::Unauthorized {
                message,
                challenge: false,
            } => envelope_response(StatusCode::UNAUTHORIZED, UNAUTHORIZED, message, None),
            Self::Forbidden { message } => {
                envelope_response(StatusCode::FORBIDDEN, FORBIDDEN, message, None)
            }
            Self::NotFound => {
                envelope_response(StatusCode::NOT_FOUND, NOT_FOUND, NOT_FOUND_MESSAGE, None)
            }
            Self::InvalidInput { message } => {
                envelope_response(StatusCode::BAD_REQUEST, INVALID_INPUT, message, None)
            }
            Self::Conflict { message } => {
                envelope_response(StatusCode::CONFLICT, CONFLICT, message, None)
            }
            Self::TooLarge => envelope_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                PAYLOAD_TOO_LARGE,
                FIXED_TOO_LARGE_MESSAGE,
                None,
            ),
            Self::Unavailable { error_id } => fault_response(
                StatusCode::SERVICE_UNAVAILABLE,
                UPSTREAM_ERROR,
                FIXED_UPSTREAM_MESSAGE,
                &error_id,
            ),
            Self::Internal { error_id } => fault_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL_ERROR,
                FIXED_INTERNAL_MESSAGE,
                &error_id,
            ),
        }
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
