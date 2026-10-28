//! Database handle for the collections stores.
//!
//! [`CollectionsDb`] carries the database: the reader pool for queries and
//! the writer lane for every write. The stores in `store/` hold a clone;
//! nothing in collections opens its own connection.
//!
//! Timestamps: playlist rows keep v2's ISO-8601 text columns. SQLite does
//! the conversion in the statements (`strftime`), so the stores trade in
//! epoch seconds and never parse dates themselves.

use std::sync::Arc;

use rusqlite::{Connection, Transaction};
use sqlx::SqlitePool;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane, map_sqlx_busy};

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

/// The application database and the clock rows are stamped by.
#[derive(Clone)]
pub struct CollectionsDb {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for CollectionsDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollectionsDb")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl CollectionsDb {
    /// Handle over the serving runtime's database and clock.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }

    /// Now, as epoch milliseconds.
    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Now, as epoch seconds. A clock before the epoch reads as zero.
    pub fn now_epoch(&self) -> u64 {
        u64::try_from(self.clock.now_unix()).unwrap_or(0)
    }

    /// Now, as fractional epoch seconds to the millisecond, for the REAL
    /// columns.
    pub fn now_real(&self) -> f64 {
        self.clock.now_ms().max(0) as f64 / 1000.0
    }

    /// The reader pool the sqlx stores read through.
    pub fn pool(&self) -> Result<&SqlitePool, StoreError> {
        Ok(self.db.sqlx())
    }

    /// One request-path read on a pooled connection.
    pub async fn read<F, R>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        F: FnOnce(&Connection) -> Result<R, DbError> + Send + 'static,
        R: Send + 'static,
    {
        self.db.read(name, op).await.map_err(StoreError::from_db)
    }

    /// One request-path write transaction.
    pub async fn write<F, R>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        F: FnOnce(&Transaction) -> Result<R, DbError> + Send + 'static,
        R: Send + 'static,
    {
        self.db
            .write(Lane::Foreground, name, op)
            .await
            .map_err(StoreError::from_db)
    }
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
