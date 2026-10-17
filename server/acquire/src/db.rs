//! Database handle for acquisition state.
//!
//! [`AcquireDb`] carries the database the acquisition stores share: the
//! reader pool for queries and the writer lane for every write. Stores hold
//! a clone; nothing in acquisition opens its own connection.

use std::path::Path;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Arc;

use rusqlite::Transaction;
use sqlx::SqlitePool;

use droppedneedle_kernel::db::{Db, DbError, Lane};

/// The application database, for the acquisition stores.
#[derive(Clone, Debug)]
pub struct AcquireDb {
    db: Db,
    #[cfg(any(test, feature = "test-support"))]
    _scratch: Option<Arc<droppedneedle_kernel::db::scratch::ScratchDir>>,
}

impl AcquireDb {
    /// Handle over the serving runtime's database.
    pub fn new(db: Db) -> Self {
        Self {
            db,
            #[cfg(any(test, feature = "test-support"))]
            _scratch: None,
        }
    }

    /// The sqlx reader pool.
    pub fn pool(&self) -> &SqlitePool {
        self.db.sqlx()
    }

    /// The database, for a write that is not one of the two below.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// One request-path write transaction.
    pub async fn write<F, R>(&self, name: &'static str, op: F) -> Result<R, DbError>
    where
        F: FnOnce(&Transaction) -> Result<R, DbError> + Send + 'static,
        R: Send + 'static,
    {
        self.db.write(Lane::Foreground, name, op).await
    }

    /// One background-job write transaction.
    pub async fn write_background<F, R>(&self, name: &'static str, op: F) -> Result<R, DbError>
    where
        F: FnOnce(&Transaction) -> Result<R, DbError> + Send + 'static,
        R: Send + 'static,
    {
        self.db.write(Lane::Background, name, op).await
    }

    /// Scratch database for tests: a private copy of the migrated template
    /// in the temp dir, opened through [`Db::open_for_tests`]. The directory
    /// is removed when the last clone drops.
    #[cfg(any(test, feature = "test-support"))]
    pub fn scratch() -> Result<Self, String> {
        let scratch = Arc::new(
            droppedneedle_kernel::db::scratch::ScratchDir::new("acquire-db")
                .map_err(|error| format!("scratch dir: {error}"))?,
        );
        let dir = scratch.to_path_buf();
        let path = dir.join("app.db");
        droppedneedle_kernel::schema::template::copy_to(&path)?;
        let db = Db::open_for_tests(&path).map_err(|error| error.to_string())?;
        Ok(Self {
            db,
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
