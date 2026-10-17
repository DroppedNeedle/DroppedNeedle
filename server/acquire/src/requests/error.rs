//! Requests domain errors.
//!
//! Services and stores return [`RequestsError`]; only its one mapping onto
//! `ApiError` in [`super::http`] chooses a status.

use serde_json::Value;

/// Rolling request-count quota spent. Maps to 429; details carry the window.
pub const QUOTA_EXCEEDED: &str = "QUOTA_EXCEEDED";
/// Library or personal storage budget full. Maps to 403; details carry usage.
pub const STORAGE_FULL: &str = "STORAGE_FULL";

/// What a requests operation can fail with.
#[derive(Debug)]
pub enum RequestsError {
    /// No usable credential.
    Unauthorized { message: String },
    /// Credential lacks rights.
    Forbidden { message: String },
    /// Missing resource.
    NotFound,
    /// Bad input with a user-safe message.
    InvalidInput { message: String },
    /// Valid input against the wrong state.
    Conflict { message: String },
    /// Request-count quota spent.
    QuotaExceeded { message: String, details: Value },
    /// Storage budget full.
    StorageFull { message: String, details: Value },
    /// The database stayed locked past its busy timeout. Retryable.
    Busy { operation: String },
    /// Server fault. The cause is for the log only.
    Internal { cause: String },
}

impl RequestsError {
    /// A server fault with its cause for the log.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }
}
