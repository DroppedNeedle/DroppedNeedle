//! The admin domain error.
//!
//! Admin services return [`AdminError`]; the handlers give it a status and
//! the shared envelope. Server faults carry an error id whose cause went to
//! the log only.

/// Admin failures. The admin handlers map them to HTTP
/// ([`super::handlers::AdminHttpError`]).
#[derive(Debug)]
pub enum AdminError {
    /// No usable credential.
    Unauthorized {
        /// Why the credential failed.
        message: String,
    },
    /// Credential lacks rights.
    Forbidden {
        /// What the credential lacks.
        message: String,
    },
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
    /// A backend this route needs is unwired.
    Unavailable {
        /// Which backend is missing.
        message: String,
    },
    /// Server fault. The id matches the log line.
    Internal {
        /// Log correlation id.
        error_id: String,
    },
}

impl AdminError {
    /// Build a server fault, logging the real cause with a fresh id.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        let error_id = uuid::Uuid::new_v4().to_string();
        tracing::error!(error_id, %cause, "admin request failed");
        Self::Internal { error_id }
    }
}
