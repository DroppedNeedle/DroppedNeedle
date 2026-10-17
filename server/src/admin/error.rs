//! The admin domain error.
//!
//! Admin services return [`AdminError`]; its one mapping onto `ApiError`
//! in the handlers gives it a status. Server faults carry their cause for
//! the log only.

/// Admin failures.
#[derive(Debug)]
pub enum AdminError {
    /// Missing resource.
    NotFound,
    /// Bad input with a user-safe message.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Live-state refusal with a user-safe message.
    Conflict {
        /// What is already going.
        message: String,
    },
    /// A backend this route needs is unwired (503).
    Unavailable {
        /// Which backend is missing, for the log only.
        message: String,
    },
    /// Server fault.
    Internal {
        /// What went wrong, for the log only.
        cause: String,
    },
}

impl AdminError {
    /// A server fault with its cause for the log.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }
}
