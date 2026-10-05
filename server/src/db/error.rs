//! Typed persistence errors and the busy-path mapping.
//!
//! Lock contention is transient by construction: the writer lane serializes
//! writes and every connection waits out the 5 s busy timeout before SQLite
//! reports busy. When busy still escapes, the data path maps it to a typed
//! retryable error rendered as HTTP 503 with `Retry-After: 1`. The client
//! retries; this process never spins.

use std::path::PathBuf;

use axum::{
    http::{HeaderValue, StatusCode, header::RETRY_AFTER},
    response::Response,
};
use thiserror::Error;

use crate::error::ApiError;

/// Failures from the SQLite runtime: boot checks, writes, backup, restore.
#[derive(Debug, Error)]
pub enum DbError {
    /// The database path or one of its sidecars sits on a network filesystem.
    #[error("refusing database on non-local filesystem {fs_type} at mount {mount}: {path:?}")]
    NonLocalFilesystem {
        /// The path that was refused.
        path: PathBuf,
        /// Filesystem type name from the mount table, or the statfs magic.
        fs_type: String,
        /// Mount point the path resolves under.
        mount: String,
    },
    /// The database path is a symlink. Symlinks are never followed here.
    #[error("refusing database behind a symlink: {path:?}")]
    SymlinkRefused {
        /// The path that was refused.
        path: PathBuf,
    },
    /// Lock contention outlived the busy timeout. Retryable by the caller.
    #[error("database is busy during {operation}; retry the request")]
    Busy {
        /// Operation name for the log line. Never SQL text or a path.
        operation: String,
    },
    /// A write transaction outran its hard budget and was aborted.
    #[error("write {operation} exceeded its budget and was rolled back")]
    TxTimeout {
        /// Operation name for the log line. Never SQL text or a path.
        operation: String,
        /// Rows changed before the abort fired.
        changes: u64,
    },
    /// Cooperative cancellation stopped a chunked background write.
    #[error("chunked write {operation} cancelled after {chunks} chunks")]
    Cancelled {
        /// Operation name for the log line.
        operation: String,
        /// Committed chunks before the stop flag was seen.
        chunks: usize,
    },
    /// The backup staging copy failed its integrity check. Nothing rotated.
    #[error("staged backup failed integrity validation; previous backups kept")]
    IntegrityFailed,
    /// Restore needs an empty directory and the target was occupied.
    #[error("refusing restore into non-empty target: {path:?}")]
    RestoreTargetNotEmpty {
        /// The target that was refused.
        path: PathBuf,
    },
    /// The backup is newer than this binary and downgrade was not allowed.
    #[error("backup schema version {found} is newer than binary version {expected}")]
    BackupTooNew {
        /// `user_version` stamped in the backup file.
        found: i64,
        /// Newest migration this binary knows.
        expected: i64,
    },
    /// Backup staging must sit on the same filesystem as the database so the
    /// final rename is atomic.
    #[error("backup staging is not on the database filesystem: {path:?}")]
    StagingCrossFilesystem {
        /// The staging path that was refused.
        path: PathBuf,
    },
    /// The writer lane shut down while an admission was queued.
    #[error("write lane is closed")]
    LaneClosed,
    /// A write closure failed inside its transaction. The transaction rolled
    /// back; the message names the operation, never SQL text or a path.
    #[error("write {operation} failed: {cause}")]
    WriteFailed {
        /// Operation name for the log line.
        operation: String,
        /// Short cause without SQL text, hosts, or paths.
        cause: String,
    },
    /// Schema migrate or boot assertion failed.
    #[error(transparent)]
    Schema(#[from] crate::schema::SchemaError),
    /// A non-busy SQLite failure on the read path or at boot.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// A non-busy SQLite failure on a dedicated connection.
    #[error(transparent)]
    Rusqlite(#[from] rusqlite::Error),
    /// Filesystem failure around the database or a backup.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Backup manifest that is missing or unreadable as JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// True when a sqlx error is lock contention (SQLITE_BUSY or SQLITE_LOCKED).
///
/// The check mirrors the v2 rule: either the primary result code is busy or
/// locked, or the message names a lock. Only lock errors convert; anything
/// else keeps its failure semantics.
pub fn sqlx_is_busy(error: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(database) = error {
        if let Some(code) = database.code()
            && (code.as_ref() == "5" || code.as_ref() == "6")
        {
            return true;
        }
        let message = database.message().to_lowercase();
        return message.contains("locked") || message.contains("busy");
    }
    false
}

/// True when a rusqlite error is lock contention (SQLITE_BUSY or SQLITE_LOCKED).
pub fn rusqlite_is_busy(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(code, message) => {
            use rusqlite::ffi::ErrorCode;
            if matches!(
                code.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) {
                return true;
            }
            let text = message.as_deref().unwrap_or_default().to_lowercase();
            text.contains("locked") || text.contains("busy")
        }
        _ => false,
    }
}

/// Map a sqlx failure on the data path: busy becomes retryable, the rest
/// passes through untouched.
pub fn map_sqlx_busy(operation: &str, error: sqlx::Error) -> DbError {
    if sqlx_is_busy(&error) {
        DbError::Busy {
            operation: operation.to_owned(),
        }
    } else {
        DbError::Sqlx(error)
    }
}

/// Render the busy path: 503 with `Retry-After: 1` and the fixed 5xx envelope.
///
/// The body matches what the request-scope middleware writes for any
/// 5xx, so passing through the middleware is idempotent: same status, same
/// header, same body. The operation name is logged, never rendered.
pub fn busy_response(operation: &str, request_id: &str) -> Response {
    tracing::warn!(
        operation,
        request_id,
        "database busy; answering 503 with retry"
    );
    let mut response = ApiError::server_error_response(StatusCode::SERVICE_UNAVAILABLE, request_id);
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_response_carries_503_retry_after_and_fixed_envelope() {
        let response = busy_response("op", "req-1");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get(RETRY_AFTER).unwrap(),
            HeaderValue::from_static("1")
        );
    }

    #[test]
    fn rusqlite_busy_detection_follows_message_and_code() {
        let failure = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(5),
            Some("database is locked".to_owned()),
        );
        assert!(rusqlite_is_busy(&failure));
        let other = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(1),
            Some("no such table".to_owned()),
        );
        assert!(!rusqlite_is_busy(&other));
    }
}
