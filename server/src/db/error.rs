//! Typed persistence errors and busy detection.
//!
//! Lock contention is transient by construction: the writer lane serializes
//! writes and every connection waits out the 5 s busy timeout before SQLite
//! reports busy. When busy still escapes, the data path maps it to the typed
//! retryable [`DbError::Busy`]. The lane answers the same way when a
//! foreground write finds its queue full or has waited 5 s for its turn.
//! Handlers render that as HTTP 503 with
//! `Retry-After: 1` through [`crate::error::busy_response`]. The client
//! retries; this process never spins.

use std::path::PathBuf;

use thiserror::Error;

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
    /// Lock contention outlived the busy timeout, or a write was refused by
    /// the lane: its queue was full, or it was a foreground write that
    /// waited past its limit and never ran. Also a request read that waited
    /// past the limit for a pooled connection. Retryable by the caller.
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
        /// Rows this transaction changed before the abort fired.
        changes: u64,
    },
    /// A write closure ended its own transaction: a raw `COMMIT` or
    /// `ROLLBACK`, or a failure that made SQLite roll the whole transaction
    /// back. Nothing the closure did was committed.
    #[error("write {operation} ended its own transaction; nothing was committed")]
    TransactionEnded {
        /// Operation name for the log line. Never SQL text or a path.
        operation: String,
    },
    /// A write was issued from inside a write closure on the same database.
    /// It could only wait behind the closure that issued it, so it is
    /// refused at once.
    #[error("write {operation} was issued inside a write closure on the same database")]
    NestedWrite {
        /// Name of the refused write. Never SQL text or a path.
        operation: String,
    },
    /// Synchronous code tried to wait on a future from inside a write
    /// closure, where the lane would be held for as long as the wait lasts.
    #[error("refusing to wait on {operation} inside a write closure")]
    WaitInsideWrite {
        /// What the wait was for. Never SQL text, a host or a path.
        operation: String,
    },
    /// A pooled read was issued from inside a write closure on the same
    /// database. It would see the state from before the transaction, so it
    /// is refused at once: read on the transaction instead.
    #[error("read {operation} was issued inside a write closure on the same database")]
    ReadInsideWrite {
        /// Name of the refused read. Never SQL text or a path.
        operation: String,
    },
    /// A write, another pooled read or a wait on a future was issued from
    /// inside a read closure, which holds a connection and pins the WAL for
    /// as long as it runs. Refused at once.
    #[error("{operation} was issued inside a read closure on the same database")]
    InsideRead {
        /// Name of the refused call. Never SQL text, a host or a path.
        operation: String,
    },
    /// A long read's cancel flag tripped while it waited for a connection.
    #[error("read {operation} was cancelled while it waited for a connection")]
    ReadCancelled {
        /// Operation name for the log line. Never SQL text or a path.
        operation: String,
    },
    /// The closure of an async read panicked. Its connection went back to
    /// the pool.
    #[error("read {operation} panicked")]
    ReadPanicked {
        /// Operation name for the log line. Never SQL text or a path.
        operation: String,
    },
    /// Cooperative cancellation stopped a chunked bulk write.
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
    /// The database is shutting down: the writer lane is closed, went away
    /// before it answered or gave up before this write's turn came, or a
    /// read pool is closed.
    #[error("the database is shutting down")]
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

#[cfg(test)]
mod tests {
    use super::*;

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
