//! The users domain error.
//!
//! Services return [`UsersError`]; the users handlers map it onto
//! `ApiError`. Anything unexpected carries its cause for the log, never
//! for the caller.

/// Every failure the users routes can return to a caller.
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
    /// A directory or IdP this route needs is unconfigured or unreachable
    /// (503, the federated outage posture).
    Unavailable {
        /// What went wrong, for the log only.
        cause: String,
    },
    /// Downstream failure (502).
    Upstream {
        /// What went wrong, for the log only.
        cause: String,
    },
    /// The database was too busy to answer in time.
    Busy,
    /// Server fault.
    Internal {
        /// What went wrong, for the log only.
        cause: String,
    },
}

impl UsersError {
    /// A 500 with its cause for the log.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }

    /// A 503 with its cause for the log.
    pub fn unavailable(cause: &dyn std::fmt::Display) -> Self {
        Self::Unavailable {
            cause: cause.to_string(),
        }
    }

    /// A 502 with its cause for the log.
    pub fn upstream(cause: &dyn std::fmt::Display) -> Self {
        Self::Upstream {
            cause: cause.to_string(),
        }
    }
}
