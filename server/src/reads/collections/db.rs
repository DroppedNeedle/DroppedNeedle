//! Database handle for the collections stores.
//!
//! [`CollectionsDb`] carries the reader pool for queries and the writer
//! lane for every write. The stores in `store/` hold a clone; nothing in
//! collections opens its own connection.
//!
//! Timestamps: playlist rows keep v2's ISO-8601 text columns. SQLite does
//! the conversion in the statements (`strftime`), so the stores trade in
//! epoch seconds and never parse dates themselves.

use rusqlite::Transaction;
use sqlx::SqlitePool;

use crate::db::{DbError, Lane, OpError, WriteLane, map_sqlx_busy};

/// Every way a store call can fail. The text goes to the log only.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Lock contention outlived the busy timeout. The caller may retry.
    #[error("database busy during {0}")]
    Busy(String),
    /// Anything else.
    #[error("store failure: {0}")]
    Internal(String),
}

impl StoreError {
    fn from_db(error: DbError) -> Self {
        match error {
            DbError::Busy { operation } => Self::Busy(operation),
            other => Self::Internal(other.to_string()),
        }
    }

    /// Map a reader-pool error, keeping busy distinct.
    pub fn read(operation: &str, error: sqlx::Error) -> Self {
        Self::from_db(map_sqlx_busy(operation, error))
    }
}

/// Reader pool plus writer lane over the application database.
#[derive(Clone, Debug, Default)]
pub struct CollectionsDb {
    live: Option<(SqlitePool, WriteLane)>,
}

impl CollectionsDb {
    /// Handle over the serving runtime's pool and lane.
    pub fn new(pool: SqlitePool, lane: WriteLane) -> Self {
        Self {
            live: Some((pool, lane)),
        }
    }

    /// A handle with no database: every call fails closed with a logged
    /// 5xx. Only bundles that never serve collections build this.
    pub fn unwired() -> Self {
        Self::default()
    }

    /// Reader pool, or the unwired failure.
    pub fn pool(&self) -> Result<&SqlitePool, StoreError> {
        self.live.as_ref().map(|(pool, _)| pool).ok_or_else(unwired)
    }

    /// One request-path write transaction.
    pub async fn write<F, R>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        F: FnOnce(&Transaction) -> Result<R, OpError> + Send + 'static,
        R: Send + 'static,
    {
        let (_, lane) = self.live.as_ref().ok_or_else(unwired)?;
        lane.write(Lane::Foreground, name, op)
            .await
            .map_err(StoreError::from_db)
    }
}

fn unwired() -> StoreError {
    StoreError::Internal("collections database is not wired".to_owned())
}

/// Current time as epoch seconds. A broken clock reads as zero rather than
/// failing the request.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}

/// Current time as fractional epoch seconds, for the REAL columns.
pub fn now_real() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// Epoch seconds from a REAL column. Negative or broken values read as zero.
pub fn epoch_from_real(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}

/// `?, ?, ...` placeholders for an IN-list. Callers skip empty lists.
pub fn placeholders(len: usize) -> String {
    vec!["?"; len].join(", ")
}
