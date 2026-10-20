//! Settings errors and their one mapping onto [`ApiError`].
//!
//! 4xx messages are user-safe and render verbatim: no variant carries
//! secret material. A 5xx keeps its status with the fixed body; its cause
//! goes to the log once, at the request boundary.
//!
//! Config failures map by kind: [`ConfigError::Validation`] and
//! [`ConfigError::SectionDecode`] are caller faults (400), a database too
//! busy to take the write is the busy 503, and other storage failures are
//! server faults (500).
//!
//! [`ConfigError::Validation`]: crate::runtime_config::ConfigError::Validation
//! [`ConfigError::SectionDecode`]: crate::runtime_config::ConfigError::SectionDecode

use crate::error::{ApiError, RATE_LIMITED, Unavailable};
use axum::http::StatusCode;

/// Expected revision does not match the stored one; refresh and retry.
pub const STALE_REVISION: &str = "STALE_REVISION";
/// A dropped config section was addressed. Gone, not moved.
pub const SECTION_DROPPED: &str = "SECTION_DROPPED";

/// Every failure the settings services can return to a caller.
#[derive(Debug)]
pub enum SettingsError {
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
    /// The upstream answered badly or not at all (502).
    Upstream {
        /// What went wrong, for the log only.
        cause: String,
    },
    /// An outside service is down, rate limiting or unreachable (503
    /// `PROVIDER_UNAVAILABLE`).
    ProviderUnavailable {
        /// The service's lowercase key.
        provider: &'static str,
        /// Seconds until it takes requests again, when it said.
        retry_after: Option<u64>,
        /// What went wrong, for the log only.
        cause: String,
    },
    /// A backend this route needs is unwired (503).
    Unavailable {
        /// Which backend is missing, for the log only.
        cause: String,
    },
    /// The database was too busy to answer in time (503 `SERVER_BUSY`).
    Busy {
        /// Which read or write waited, for the log only.
        cause: String,
    },
    /// Server fault.
    Internal {
        /// What went wrong, for the log only.
        cause: String,
    },
}

impl SettingsError {
    /// A 500 with its cause for the log.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }

    /// Map a config failure: validation and decode faults blame the caller,
    /// IO/lock/crypto faults blame the server. Reasons never carry secrets.
    pub fn from_config(error: crate::runtime_config::ConfigError) -> Self {
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
            Source::Busy(cause) => Self::Busy { cause },
            other => Self::internal(&other),
        }
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput { message }
            | Self::Conflict { message }
            | Self::StaleRevision { message }
            | Self::Dropped { message }
            | Self::RateLimited { message } => f.write_str(message),
            Self::Upstream { cause }
            | Self::ProviderUnavailable { cause, .. }
            | Self::Unavailable { cause }
            | Self::Busy { cause }
            | Self::Internal { cause } => f.write_str(cause),
        }
    }
}

impl From<SettingsError> for ApiError {
    fn from(error: SettingsError) -> Self {
        match error {
            SettingsError::InvalidInput { message } => ApiError::invalid_input(message),
            SettingsError::Conflict { message } => ApiError::conflict(message),
            SettingsError::StaleRevision { message } => {
                ApiError::new(StatusCode::CONFLICT, STALE_REVISION, message)
            }
            SettingsError::Dropped { message } => {
                ApiError::new(StatusCode::GONE, SECTION_DROPPED, message)
            }
            SettingsError::RateLimited { message } => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, RATE_LIMITED, message)
            }
            SettingsError::Upstream { cause } => ApiError::fault(StatusCode::BAD_GATEWAY, cause),
            SettingsError::ProviderUnavailable {
                provider,
                retry_after,
                cause,
            } => ApiError::unavailable(
                Unavailable::Provider {
                    provider,
                    retry_after,
                },
                cause,
            ),
            SettingsError::Unavailable { cause } => {
                ApiError::fault(StatusCode::SERVICE_UNAVAILABLE, cause)
            }
            SettingsError::Busy { cause } => ApiError::unavailable(Unavailable::ServerBusy, cause),
            SettingsError::Internal { cause } => ApiError::internal(cause),
        }
    }
}
