//! Typed errors rendered into the shared envelope.
//!
//! Every failure renders as `{"error": {code, message, details}}` with a
//! SCREAMING_SNAKE code. Statuses match the reads routes: 401 carries the
//! `Bearer` challenge, 4xx messages are user-safe, and 5xx bodies stay fixed
//! with only an error id while the cause goes to the structured log.

use crate::error::{ErrorBody, ErrorEnvelope};
use crate::ids::IdGenerator;
use axum::{
    Json,
    extract::{FromRequest, Query, Request},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;
use serde_json::json;

/// Missing or invalid session.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Unknown id. Always the same fixed message.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// The request itself is wrong. The message is user-facing.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// Valid input against the wrong state (duplicate import, bad folder mode).
pub const CONFLICT: &str = "CONFLICT";
/// No credential stored for this source, or the source is disabled.
pub const REMOTE_NOT_CONFIGURED: &str = "REMOTE_NOT_CONFIGURED";
/// The stored credential was rejected upstream (HTTP 401/403, Subsonic 40/41).
pub const REMOTE_AUTH_FAILED: &str = "REMOTE_AUTH_FAILED";
/// The upstream answered with an error or garbage.
pub const REMOTE_UNAVAILABLE: &str = "REMOTE_UNAVAILABLE";
/// The source has no such capability (Plex similar, Jellyfin top songs).
pub const REMOTE_UNSUPPORTED: &str = "REMOTE_UNSUPPORTED";

/// Challenge sent on every 401.
pub const WWW_AUTHENTICATE_BEARER: &str = "Bearer";

/// Every failure the remotes routes can return to a caller.
#[derive(Debug)]
pub enum RemotesError {
    /// No valid session.
    Unauthorized {
        /// User-safe reason, e.g. "Authentication required".
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
    /// Source has no stored credential.
    NotConfigured {
        /// Which source is missing, e.g. "Plex is not connected".
        message: String,
    },
    /// Upstream rejected the stored credential. Relink, do not retry.
    AuthFailed {
        /// Which source rejected, without credential detail.
        message: String,
    },
    /// Upstream error or unreachable. User-safe summary only.
    Unavailable {
        /// Short summary, e.g. "Jellyfin answered with an error".
        message: String,
    },
    /// Capability the source lacks.
    Unsupported {
        /// What is missing where, e.g. "Plex has no similar-tracks endpoint".
        message: String,
    },
    /// Server fault. Fixed body plus the request-tied id.
    Internal {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
}

impl RemotesError {
    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "remotes request failed");
        Self::Internal { error_id }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidInput { .. }
            | Self::Conflict { .. }
            | Self::Unsupported { .. }
            | Self::NotConfigured { .. } => StatusCode::BAD_REQUEST,
            Self::AuthFailed { .. } => StatusCode::UNAUTHORIZED,
            Self::Unavailable { .. } => StatusCode::BAD_GATEWAY,
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
            Self::Conflict { message } => (CONFLICT.to_owned(), message.clone(), None),
            Self::NotConfigured { message } => {
                (REMOTE_NOT_CONFIGURED.to_owned(), message.clone(), None)
            }
            Self::AuthFailed { message } => (REMOTE_AUTH_FAILED.to_owned(), message.clone(), None),
            Self::Unavailable { message } => (REMOTE_UNAVAILABLE.to_owned(), message.clone(), None),
            Self::Unsupported { message } => (REMOTE_UNSUPPORTED.to_owned(), message.clone(), None),
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

impl IntoResponse for RemotesError {
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

/// Query extractor that renders failures in the shared envelope instead of
/// Axum's default plain-text 400.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = RemotesError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| RemotesError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}

/// JSON body extractor with the same envelope guarantee.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = RemotesError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(req, state)
            .await
            .map(|axum::Json(value)| Self(value))
            .map_err(|cause| RemotesError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}
