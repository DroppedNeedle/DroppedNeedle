//! Requests domain errors.
//!
//! Services and stores return [`RequestsError`]; only the handler layer
//! ([`super::http`]) maps it to a status and the shared error envelope.

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
    /// Server fault. The id matches the log line.
    Internal { error_id: String },
}

impl RequestsError {
    /// Build a server fault, logging the real cause with a fresh id. The id
    /// is minted here because these routes have no request-scope id of
    /// their own.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        let error_id = uuid::Uuid::new_v4().to_string();
        tracing::error!(error_id, %cause, "requests operation failed");
        Self::Internal { error_id }
    }
}
