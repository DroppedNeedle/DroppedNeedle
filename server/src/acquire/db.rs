//! Database handle for acquisition state.
//!
//! [`AcquireDb`] carries the two halves of the shared SQLite runtime the
//! acquisition stores need: the reader pool for queries and the writer lane
//! for every write. Stores hold a clone; nothing in acquisition opens its
//! own connection.

use std::path::Path;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Arc;

use rusqlite::Transaction;
use sqlx::SqlitePool;

use crate::db::{DbError, DbRuntime, Lane, OpError, WriteLane};

/// Reader pool plus writer lane over the application database.
#[derive(Clone, Debug)]
pub struct AcquireDb {
    pool: SqlitePool,
    lane: WriteLane,
    #[cfg(any(test, feature = "test-support"))]
    _scratch: Option<Arc<crate::tooling::scratch::ScratchDir>>,
}

impl AcquireDb {
    /// Handle over the serving runtime.
    pub fn from_runtime(runtime: &DbRuntime) -> Self {
        Self::new(runtime.pool().clone(), runtime.lane().clone())
    }

    /// Handle over an explicit pool and lane on the same database file.
    pub fn new(pool: SqlitePool, lane: WriteLane) -> Self {
        Self {
            pool,
            lane,
            #[cfg(any(test, feature = "test-support"))]
            _scratch: None,
        }
    }

    /// Reader pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Writer lane.
    pub fn lane(&self) -> &WriteLane {
        &self.lane
    }

    /// Database file the lane writes to.
    pub fn path(&self) -> &Path {
        self.lane.db_path()
    }

    /// One request-path write transaction.
    pub async fn write<F, R>(&self, name: &'static str, op: F) -> Result<R, DbError>
    where
        F: FnOnce(&Transaction) -> Result<R, OpError> + Send + 'static,
        R: Send + 'static,
    {
        self.lane.write(Lane::Foreground, name, op).await
    }

    /// One background-job write transaction.
    pub async fn write_background<F, R>(&self, name: &'static str, op: F) -> Result<R, DbError>
    where
        F: FnOnce(&Transaction) -> Result<R, OpError> + Send + 'static,
        R: Send + 'static,
    {
        self.lane.write(Lane::Background, name, op).await
    }

    /// Scratch database for tests: a fresh file in the temp dir with every
    /// migration applied, a lazy reader pool and a writer lane. The
    /// directory is removed when the last clone drops. Call from inside a
    /// tokio runtime (the lane spawns its scheduler there).
    #[cfg(any(test, feature = "test-support"))]
    pub fn scratch() -> Result<Self, String> {
        let scratch = Arc::new(
            crate::tooling::scratch::ScratchDir::new("acquire-db")
                .map_err(|error| format!("scratch dir: {error}"))?,
        );
        let dir = scratch.to_path_buf();
        let path = dir.join("app.db");
        let conn = rusqlite::Connection::open(&path).map_err(|error| error.to_string())?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|error| error.to_string())?;
        for migration in crate::schema::MIGRATOR.iter() {
            conn.execute_batch(&migration.sql)
                .map_err(|error| format!("migration {}: {error}", migration.version))?;
        }
        drop(conn);
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(4)
            .connect_lazy_with(options);
        let lane = WriteLane::open(&path).map_err(|error| error.to_string())?;
        Ok(Self {
            pool,
            lane,
            _scratch: Some(scratch),
        })
    }

    /// Insert one user row so foreign keys to `auth_users` hold in tests.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn add_user(&self, id: &str, display_name: &str, role: &str) -> Result<(), String> {
        let (id, display_name, role) = (id.to_owned(), display_name.to_owned(), role.to_owned());
        self.write("test.add_user", move |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO auth_users (id, display_name, role, created_at) \
                 VALUES (?1, ?2, ?3, '2024-01-01T00:00:00Z')",
                rusqlite::params![id, display_name, role],
            )?;
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())
    }
}

/// Current unix time in whole seconds. A clock before the epoch reads as
/// zero, which only ever makes rows look old.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}

/// Convert a stored integer to `u64`, clamping negatives to zero.
pub fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Convert a `u64` for storage, clamping to the SQLite integer range.
pub fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
