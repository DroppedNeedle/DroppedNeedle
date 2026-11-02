//! Database handle for the collections stores.
//!
//! [`CollectionsDb`] carries the database: the reader pool for queries and
//! the writer lane for every write. The stores in `store/` hold a clone;
//! nothing in collections opens its own connection.
//!
//! Timestamps: playlist rows hold integer milliseconds. The statements turn
//! them into the epoch seconds the stores trade in.

use std::sync::Arc;

use rusqlite::{Connection, Transaction};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};

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

impl From<DbError> for StoreError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Busy { operation } => Self::Busy(operation),
            other => Self::Internal(other.to_string()),
        }
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

    /// The database, for the compat adapters' own statements.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// One request-path read on a pooled connection.
    pub async fn read<F, R>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        F: FnOnce(&Connection) -> Result<R, DbError> + Send + 'static,
        R: Send + 'static,
    {
        self.db.read(name, op).await.map_err(StoreError::from)
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
            .map_err(StoreError::from)
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
