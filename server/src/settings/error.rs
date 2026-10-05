//! Slice errors rendered into the shared envelope.
//!
//! Every failure renders as `{"error": {code, message, details}}` with a
//! SCREAMING_SNAKE code. Statuses follow the sibling slices: 401 carries
//! the `Bearer` challenge, 4xx messages are user-safe, and 5xx bodies stay
//! fixed with only an error id while the cause goes to the structured log.
//!
//! Config failures map by kind: [`ConfigError::Validation`] and
//! [`ConfigError::SectionDecode`] are caller faults (400), lock and IO
//! failures are server faults (500). No variant carries secret material,
//! so validation reasons render verbatim.

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
/// Admin-only route hit by a lesser role.
pub const FORBIDDEN: &str = "FORBIDDEN";
/// Unknown id. Always the same fixed message.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// The request itself is wrong. The message is user-facing.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// Valid input against the wrong state (stale proposal, disabled feature).
pub const CONFLICT: &str = "CONFLICT";
/// Expected revision does not match the stored one; refresh and retry.
pub const STALE_REVISION: &str = "STALE_REVISION";
/// A dropped config section was addressed. Gone, not moved.
pub const SECTION_DROPPED: &str = "SECTION_DROPPED";
/// The upstream provider is rate-limiting this server.
pub const RATE_LIMITED: &str = "RATE_LIMITED";
/// The upstream answered badly or not at all.
pub const UPSTREAM_ERROR: &str = "UPSTREAM_ERROR";
/// A backend this route needs is unwired.
pub const SERVICE_UNAVAILABLE: &str = "SERVICE_UNAVAILABLE";

/// Challenge sent on every 401.
pub const WWW_AUTHENTICATE_BEARER: &str = "Bearer";

/// Every failure this slice can return to a caller.
#[derive(Debug)]
pub enum SettingsError {
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
    /// Expected revision missed the stored one.
    StaleRevision {
        /// What was wrong.
        message: String,
    },
    /// A dropped section was addressed by key.
    Dropped {
        /// Which section, and where it went.
        message: String,
    },
    /// The upstream provider is rate-limiting this server.
    RateLimited {
        /// User-safe reason.
        message: String,
    },
    /// The upstream answered badly or not at all.
    Upstream {
        /// User-safe reason.
        message: String,
    },
    /// A backend this route needs is unwired.
    Unavailable {
        /// Which backend is missing.
        message: String,
    },
    /// Server fault. Fixed body plus the request-tied id.
    Internal {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
}

impl SettingsError {
    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "settings slice failed");
        Self::Internal { error_id }
    }

    /// Map a config failure: validation and decode faults blame the caller,
    /// IO/lock/crypto faults blame the server. Reasons never carry secrets.
    pub fn from_config(error: crate::runtime_config::ConfigError, ids: &dyn IdGenerator) -> Self {
        use crate::runtime_config::ConfigError as Source;
        match error {
            Source::Validation {
                section,
                field,
                reason,
            } => Self::InvalidInput {
                message: format!("Invalid {section}.{field}: {reason}"),
            },
            Source::SectionDecode { section, reason } => Self::InvalidInput {
                message: format!("Stored {section} has the wrong shape: {reason}"),
            },
            other => Self::internal(&other, ids),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidInput { .. } => StatusCode::BAD_REQUEST,
            Self::Dropped { .. } => StatusCode::GONE,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Upstream { .. } => StatusCode::BAD_GATEWAY,
            Self::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Conflict { .. } | Self::StaleRevision { .. } => StatusCode::CONFLICT,
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
            Self::StaleRevision { message } => (STALE_REVISION.to_owned(), message.clone(), None),
            Self::Dropped { message } => (SECTION_DROPPED.to_owned(), message.clone(), None),
            Self::RateLimited { message } => (RATE_LIMITED.to_owned(), message.clone(), None),
            Self::Upstream { message } => (UPSTREAM_ERROR.to_owned(), message.clone(), None),
            Self::Unavailable { message } => {
                (SERVICE_UNAVAILABLE.to_owned(), message.clone(), None)
            }
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

impl IntoResponse for SettingsError {
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
    type Rejection = SettingsError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| SettingsError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}

/// JSON body extractor with the same envelope guarantee.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = SettingsError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(req, state)
            .await
            .map(|axum::Json(value)| Self(value))
            .map_err(|cause| SettingsError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}
