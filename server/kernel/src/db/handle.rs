//! `Db`: the one handle an area needs to read and write the database.
//!
//! It carries the writer lane, the two read pools and, until the last area
//! has moved its reads off it, the sqlx reader pool. The factory builds it
//! once; clones are cheap and share everything. Areas hold a `Db` and no
//! other database handle.
//!
//! The runtime that opened a `Db` outlives it: past its runtime a handle
//! has no `Busy` answer for a waiting read or write. So whoever opens a
//! [`super::DbRuntime`] calls its `shutdown` after the last use of its
//! handles, or exits.

use std::{path::Path, sync::Arc};

use rusqlite::{Connection, Transaction};
use sqlx::SqlitePool;

#[cfg(any(test, feature = "test-support"))]
use super::writer::LaneLimits;
use super::{
    error::{ChunkedError, DbError},
    read_pool::ReadPools,
    writer::{CancelFlag, Durable, DurableLane, Lane, WriteLane},
};

/// The database, for reads and writes. Cloneable; every clone shares the one
/// writer lane and the same read pools.
#[derive(Clone)]
pub struct Db {
    lane: WriteLane,
    reads: Arc<ReadPools>,
    sqlx: SqlitePool,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("lane", &self.lane)
            .finish_non_exhaustive()
    }
}

impl Db {
    pub(in crate::db) fn new(lane: WriteLane, reads: Arc<ReadPools>, sqlx: SqlitePool) -> Self {
        Self { lane, reads, sqlx }
    }

    /// A handle on an existing, migrated database file with no runtime
    /// behind it: the writer lane, both read pools and a lazy sqlx reader
    /// pool, opened on the calling thread. For tests only. It starts no
    /// watch, so a waiting write or read gets no `Busy` answer and a stuck
    /// write is not logged, and nothing holds bulk writes for the WAL.
    /// Writes run on [`LaneLimits::patient`], so a loaded host slows a
    /// test without aborting its writes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_for_tests(path: &Path) -> Result<Self, DbError> {
        Self::on_test_lane(path, WriteLane::open_limited(path, LaneLimits::patient())?)
    }

    /// [`Db::open_for_tests`] with other lane time limits, so a test can
    /// run a write past its budget without waiting out the real one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_for_tests_with(path: &Path, limits: LaneLimits) -> Result<Self, DbError> {
        Self::on_test_lane(path, WriteLane::open_limited(path, limits)?)
    }

    /// A test handle over `lane`: both read pools and a lazy sqlx pool.
    #[cfg(any(test, feature = "test-support"))]
    fn on_test_lane(path: &Path, lane: WriteLane) -> Result<Self, DbError> {
        let reads = ReadPools::open(
            path,
            lane.database(),
            super::MAX_READERS as usize,
            super::BUSY_TIMEOUT,
            super::ACQUIRE_TIMEOUT,
        )?;
        let sqlx = super::factory::lazy_reader_pool_for_tests(path);
        Ok(Self::new(lane, reads, sqlx))
    }

    pub(in crate::db) fn lane(&self) -> &WriteLane {
        &self.lane
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        self.lane.db_path()
    }

    pub(crate) fn reads(&self) -> &Arc<ReadPools> {
        &self.reads
    }

    /// One checkout of a request connection: the closure runs inside one
    /// read transaction and sees one snapshot. The connection is held for
    /// the whole closure, so on the request path a closure runs one
    /// statement; put statements in one closure only when they must agree.
    ///
    /// The read waits for a connection as a future, in arrival order with
    /// every other request read, and is answered `Busy` after 5 s. That
    /// answer comes from a thread the database runtime owns: on a handle
    /// that has outlived its runtime without a shutdown, a waiting read
    /// waits for a connection with no limit. Dropped while it waits, the
    /// read loses its place; dropped after the closure started, the closure
    /// finishes and its result is discarded. A closure that panics answers
    /// `ReadPanicked`.
    ///
    /// The closure runs on the tokio runtime's blocking pool. So a runtime
    /// that is being dropped waits for a read closure that has started, as
    /// for any `spawn_blocking`, and one that never returns holds that drop
    /// until it does. Reads still queued, or granted a connection and not
    /// yet started, go down with the runtime and their connections pass on.
    ///
    /// Inside the closure, call nothing on `Db` and wait on nothing: a
    /// write, another read and [`super::block_on`] are each refused with
    /// `InsideRead`. `name` is for the log and the errors, never SQL text
    /// or a path.
    pub async fn read<R, E, F>(&self, name: &'static str, op: F) -> Result<R, E>
    where
        F: FnOnce(&Connection) -> Result<R, E> + Send + 'static,
        R: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        self.reads.read(name, op).await
    }

    /// [`Db::read`] for synchronous code on a blocking thread. The closure
    /// runs on the calling thread and may borrow. Same pool and same queue
    /// as `read`; the calling thread is parked while it waits. A panic in
    /// the closure gives the connection back and continues up the caller's
    /// thread.
    pub fn read_blocking<R, E, F>(&self, name: &'static str, op: F) -> Result<R, E>
    where
        F: FnOnce(&Connection) -> Result<R, E>,
        E: From<DbError>,
    {
        self.reads.read_blocking(name, op)
    }

    /// A read that may run long (a scan window, an export), on the long
    /// pool. Waits for a connection for as long as it takes, checking
    /// `cancel`, and answers `ReadCancelled` once the flag trips. A read
    /// that is handed a connection in the instant its flag trips still runs
    /// once, so the closure checks the flag itself if that matters. The
    /// closure collects what it needs and returns: an open read keeps the
    /// WAL from shrinking, so it never waits on anything a writer feeds.
    pub fn read_long<R, E, F>(&self, name: &'static str, cancel: &CancelFlag, op: F) -> Result<R, E>
    where
        F: FnOnce(&Connection) -> Result<R, E>,
        E: From<DbError>,
    {
        self.reads.read_long(name, cancel, op)
    }

    /// One write transaction from async code: the closure runs on the
    /// writer lane's own thread when its turn comes. Who waits, who is
    /// answered `Busy` and the time budgets are in [`super::writer`].
    pub async fn write<R, E, F>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, E>
    where
        F: FnOnce(&Transaction) -> Result<R, E> + Send + 'static,
        R: Send + 'static,
        E: From<DbError> + std::fmt::Display + Send + 'static,
    {
        self.lane.write(lane, name, op).await
    }

    /// [`Db::write`] for synchronous code on a blocking thread: the closure
    /// runs on the calling thread, so it may borrow.
    pub fn write_blocking<R, E, F>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, E>
    where
        F: FnOnce(&Transaction) -> Result<R, E>,
        E: From<DbError> + std::fmt::Display,
    {
        self.lane.write_blocking(lane, name, op)
    }

    /// [`Db::write_blocking`] whose commit is on disk before it returns. It
    /// is never held by the WAL, so its lane has no bulk class. The closure
    /// gets the transaction as a [`Durable`].
    pub fn write_durable<R, E, F>(
        &self,
        lane: DurableLane,
        name: &'static str,
        op: F,
    ) -> Result<R, E>
    where
        F: FnOnce(&Durable<'_>) -> Result<R, E>,
        E: From<DbError> + std::fmt::Display,
    {
        self.lane.write_durable(lane, name, op)
    }

    /// Commit `items` in chunks, one bulk transaction each, so other writes
    /// get in between and a full WAL holds the next chunk back. Any prefix
    /// of the chunks may be all that lands; [`ChunkedError`] carries the
    /// rows that committed.
    pub async fn write_chunked<T, E, F>(
        &self,
        name: &'static str,
        items: Vec<T>,
        chunk_rows: usize,
        cancel: &CancelFlag,
        chunk: F,
    ) -> Result<usize, ChunkedError<E>>
    where
        T: Send + 'static,
        E: From<DbError> + std::fmt::Display + Send + 'static,
        F: Fn(&[T], &Transaction) -> Result<(), E> + Send + Sync + 'static,
    {
        self.lane
            .write_chunked(name, items, chunk_rows, cancel, chunk)
            .await
    }

    /// [`Db::write_chunked`] for synchronous code on a blocking thread.
    pub fn write_chunked_blocking<T, E, F>(
        &self,
        name: &'static str,
        items: &[T],
        chunk_rows: usize,
        cancel: &CancelFlag,
        chunk: F,
    ) -> Result<usize, ChunkedError<E>>
    where
        E: From<DbError> + std::fmt::Display,
        F: FnMut(&[T], &Transaction) -> Result<(), E>,
    {
        self.lane
            .write_chunked_blocking(name, items, chunk_rows, cancel, chunk)
    }

    /// Moves after each write that committed a changed row, so a listener
    /// can wait for the database to change instead of polling it. Closed
    /// once the lane has shut down.
    pub fn commits(&self) -> tokio::sync::watch::Receiver<u64> {
        self.lane.commits()
    }

    /// The sqlx reader pool, for the areas that have not moved their reads
    /// to [`Db::read`] yet.
    pub fn sqlx(&self) -> &SqlitePool {
        &self.sqlx
    }
}
