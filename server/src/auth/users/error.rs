//! The users domain error.
//!
//! Services return [`UsersError`]; the users handlers map it to HTTP
//! ([`super::handlers::UsersHttpError`]). Anything unexpected carries an
//! error id whose cause went to the log, never to the caller.

use crate::ids::IdGenerator;

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
    /// (503, the federated outage posture). Fixed body plus the id.
    Unavailable {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
    /// Downstream failure. Fixed body plus the request-tied id.
    Upstream {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
    /// Server fault. Fixed body plus the request-tied id.
    Internal {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
}

impl UsersError {
    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "users request failed");
        Self::Internal { error_id }
    }

    /// Build a 503, logging the real cause with its id.
    pub fn unavailable(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "users request dependency unavailable");
        Self::Unavailable { error_id }
    }

    /// Build a 502, logging the real cause with its id.
    pub fn upstream(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "users request upstream failed");
        Self::Upstream { error_id }
    }
}
