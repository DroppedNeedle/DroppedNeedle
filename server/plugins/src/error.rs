//! The plugin routes' failures and their one mapping onto [`ApiError`].
//!
//! Services return domain failures that handlers convert; the `From` impl
//! below is the only place a status is chosen. A server fault carries its
//! cause to the request boundary, which logs it and writes the fixed body.

use axum::http::StatusCode;
use droppedneedle_kernel::error::{ApiError, Unavailable};

/// How GitHub let an install or update down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFault {
    /// It refused the request.
    Refused,
    /// It is rate limiting this server.
    RateLimited {
        /// Seconds until it takes requests again, when it said.
        retry_after: Option<u64>,
    },
    /// It did not answer, or answered with a failure.
    Unreachable,
    /// Its answer could not be read.
    Unreadable,
}

/// Every failure the plugin routes can return to a caller.
#[derive(Debug)]
pub enum PluginError {
    /// Valid session, wrong role.
    Forbidden {
        /// User-safe reason, e.g. "Admin role required".
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
    /// The request no longer matches the current state. User-facing.
    Conflict {
        /// What changed.
        message: String,
    },
    /// Request body over the cap.
    PayloadTooLarge,
    /// Over the route's per-minute budget.
    RateLimited {
        /// Seconds the caller should wait.
        retry_after: u64,
    },
    /// GitHub got in the way of an install or update. Answers with the
    /// installer's fixed sentence, never GitHub's text.
    Source(SourceFault),
    /// A plugin route failed. A plain 502, never the plugin's error text.
    RouteFailed,
    /// Server fault. The cause reaches the log only.
    Internal {
        /// Log-only cause.
        cause: String,
    },
}

impl PluginError {
    /// A 500 whose cause the request boundary logs.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }
}

impl From<PluginError> for ApiError {
    fn from(error: PluginError) -> Self {
        match error {
            PluginError::Forbidden { message } => ApiError::forbidden(message),
            PluginError::NotFound => ApiError::not_found(),
            PluginError::InvalidInput { message } => ApiError::invalid_input(message),
            PluginError::Conflict { message } => ApiError::conflict(message),
            PluginError::PayloadTooLarge => ApiError::too_large(),
            PluginError::RateLimited { retry_after } => ApiError::rate_limited(retry_after),
            PluginError::Source(fault) => {
                let variant = match fault {
                    SourceFault::Refused => Unavailable::UpstreamRefused,
                    SourceFault::RateLimited { retry_after } => {
                        Unavailable::UpstreamRateLimited { retry_after }
                    }
                    SourceFault::Unreachable => Unavailable::UpstreamError,
                    SourceFault::Unreadable => Unavailable::UpstreamUnreadable,
                };
                ApiError::unavailable(
                    variant,
                    format_args!("GitHub stopped a plugin install: {fault:?}"),
                )
            }
            PluginError::RouteFailed => {
                ApiError::fault(StatusCode::BAD_GATEWAY, "plugin route failed")
            }
            PluginError::Internal { cause } => ApiError::internal(cause),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse as _;

    use super::*;

    #[test]
    fn rate_limited_carries_retry_after() {
        let response = ApiError::from(PluginError::RateLimited { retry_after: 7 }).into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let header = response.headers().get("retry-after");
        assert_eq!(header.and_then(|value| value.to_str().ok()), Some("7"));
    }
}
