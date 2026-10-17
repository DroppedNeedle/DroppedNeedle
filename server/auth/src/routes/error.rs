//! HTTP errors for the auth routes and their one mapping onto [`ApiError`].
//!
//! Codes and messages match the users routes. Federated IdP outages answer
//! 503 here (the federated contract and v2 both say 503), while the users
//! routes use 502 for their Last.fm calls. A busy database is the closed-list
//! `SERVER_BUSY`; every other fault keeps its status with the fixed body, its
//! cause logged once by the request boundary.

use crate::users::error::UsersError;
use axum::http::StatusCode;
use droppedneedle_kernel::error::{ApiError, Unavailable};

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
    /// Upstream IdP failure (503).
    Unavailable {
        /// What went wrong, for the log only.
        cause: String,
    },
    /// The database was too busy to answer in time.
    Busy {
        /// What was busy, for the log only.
        cause: String,
    },
    /// Server fault.
    Internal {
        /// What went wrong, for the log only.
        cause: String,
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

    /// A 500 with its cause for the log.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }

    /// A busy database, with what was busy for the log.
    pub fn busy(cause: &dyn std::fmt::Display) -> Self {
        Self::Busy {
            cause: cause.to_string(),
        }
    }

    /// An upstream 503 with its cause for the log.
    pub fn unavailable(cause: &dyn std::fmt::Display) -> Self {
        Self::Unavailable {
            cause: cause.to_string(),
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
            UsersError::Unavailable { cause } | UsersError::Upstream { cause } => {
                Self::Unavailable { cause }
            }
            UsersError::Busy => Self::busy(&"users store busy"),
            UsersError::Internal { cause } => Self::Internal { cause },
        }
    }
}

impl From<AuthRouteError> for ApiError {
    fn from(error: AuthRouteError) -> Self {
        match error {
            AuthRouteError::Unauthorized {
                message,
                challenge: true,
            } => ApiError::unauthorized(message),
            AuthRouteError::Unauthorized {
                message,
                challenge: false,
            } => ApiError::unauthorized_without_challenge(message),
            AuthRouteError::Forbidden { message } => ApiError::forbidden(message),
            AuthRouteError::NotFound => ApiError::not_found(),
            AuthRouteError::InvalidInput { message } => ApiError::invalid_input(message),
            AuthRouteError::Conflict { message } => ApiError::conflict(message),
            AuthRouteError::TooLarge => ApiError::too_large(),
            AuthRouteError::Unavailable { cause } => {
                ApiError::fault(StatusCode::SERVICE_UNAVAILABLE, cause)
            }
            AuthRouteError::Busy { cause } => ApiError::unavailable(Unavailable::ServerBusy, cause),
            AuthRouteError::Internal { cause } => ApiError::internal(cause),
        }
    }
}
