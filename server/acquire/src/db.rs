//! Database handle for acquisition state.
//!
//! [`AcquireDb`] carries the database the acquisition stores share: the
//! reader pool for queries and the writer lane for every write. Stores hold
//! a clone; nothing in acquisition opens its own connection.

use std::path::Path;
use std::sync::Arc;

use rusqlite::Transaction;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};

/// The application database and the platform clock, for the acquisition
/// stores. Rows are stamped with the clock's time, so a test's
/// `ManualClock` drives every stamp.
#[derive(Clone)]
pub struct AcquireDb {
    db: Db,
    clock: Arc<dyn Clock>,
    #[cfg(any(test, feature = "test-support"))]
    _scratch: Option<Arc<droppedneedle_kernel::db::scratch::ScratchDir>>,
}

impl std::fmt::Debug for AcquireDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcquireDb")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl AcquireDb {
    /// Handle over the serving runtime's database and clock.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self {
            db,
            clock,
            #[cfg(any(test, feature = "test-support"))]
            _scratch: None,
        }
    }

    /// The database, for a write that is not one of the two below.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The platform clock.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Now, as whole unix seconds.
    pub fn now_epoch(&self) -> u64 {
        epoch_secs(self.clock.as_ref())
    }

    /// Now, as float unix seconds.
    pub fn now_unix_f64(&self) -> f64 {
        unix_secs_f64(self.clock.as_ref())
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
            clock: Arc::new(droppedneedle_kernel::clock::SystemClock),
            _scratch: Some(scratch),
        })
    }

    /// Insert one user row so foreign keys to `auth_users` hold in tests.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn add_user(&self, id: &str, display_name: &str, role: &str) -> Result<(), String> {
        let (id, display_name, role) = (id.to_owned(), display_name.to_owned(), role.to_owned());
        self.write("test.add_user", move |tx| {
            tx.execute(
                crate::sql::INSERT_TEST_USER.text(),
                rusqlite::params![id, display_name, role],
            )?;
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())
    }
}

/// `clock`'s time in whole unix seconds. A clock before the epoch reads
/// as zero, which only ever makes rows look old.
pub fn epoch_secs(clock: &dyn Clock) -> u64 {
    u64::try_from(clock.now_unix()).unwrap_or(0)
}

/// `clock`'s time as float unix seconds, to the millisecond. A clock
/// before the epoch reads as zero.
pub fn unix_secs_f64(clock: &dyn Clock) -> f64 {
    clock.now_ms().max(0) as f64 / 1000.0
}

/// Float unix seconds as a time column stores them: integer milliseconds.
pub fn ms_from_secs_f64(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

/// A stored millisecond time as float unix seconds.
pub fn secs_f64_from_ms(millis: i64) -> f64 {
    millis as f64 / 1000.0
}

/// Convert a stored integer to `u64`, clamping negatives to zero.
pub fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Convert a `u64` for storage, clamping to the SQLite integer range.
pub fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
