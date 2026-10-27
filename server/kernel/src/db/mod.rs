//! SQLite runtime: pool factory, checkpoint, writer lanes, backup, durable fabric.
//!
//! This module owns every SQLite handle in the process. [`open_runtime`]
//! opens the database and hands out one [`Db`], the handle every area is
//! given and the only one it holds. Every write goes through the `Db`'s
//! writer lane as one synchronous closure per transaction; the lane itself
//! is private to this module. There is no async API on the write path, so
//! holding a write transaction across an await is unrepresentable.
//!
//! A `Db` carries the lane's writes, reads on two pools of read-only
//! connections, and the sqlx reader pool until the last area has moved its
//! reads off it. The [`DbRuntime`] that opened it outlives it, and whoever
//! opens a runtime shuts it down after the last use of its handles, or
//! exits.
//!
//! `read` and `read_blocking` share the request pool and its one
//! arrival-ordered queue, and are answered `Busy` after 5 s of waiting;
//! `read_long` has a small pool of its own and waits until served or
//! cancelled. One read closure is one
//! connection and one snapshot for its whole length, so a request read runs
//! one statement per closure unless its statements must agree. A read
//! closure calls nothing on `Db` and waits on nothing: a write, another
//! pooled read and [`block_on`] are refused inside one with `InsideRead`,
//! and a pooled read inside a write closure with `ReadInsideWrite`.
//!
//! Writes take turns on the lane's one connection. Async code calls
//! `write`, whose closure runs on the lane's own thread; synchronous code on
//! a blocking thread calls `write_blocking` and runs its closure itself, so
//! it may borrow. Neither needs a runtime to make progress. A closure that
//! writes to its own lane is refused with `NestedWrite`. Synchronous code
//! that must wait on a future goes through [`block_on`], which refuses
//! inside a write closure.
//!
//! A write closure fails with the caller's own error type, as a read
//! closure does, and the lane's failures reach the caller through
//! `From<DbError>`. An outcome a caller branches on is a variant of that
//! type, never the text of a database error.
//!
//! A background write waits for its turn. A foreground write is refused
//! with `Busy` when its queue is full or it has waited 5 s, so a request
//! never waits for ever behind a stuck write. Each class has its own time
//! budget, and the lane's watch thread, which the runtime starts and stops,
//! names in the log a write that holds the lane past it.
//!
//! `write_durable` is `write_blocking` with the commit synced to disk before
//! it returns, at the cost of one sync a call. It is for rows that must
//! outlive a power cut, and it waits for its turn without the 5 s limit.
//! Its closure gets the transaction as a [`Durable`], which only the lane
//! makes, so a write that must be durable can ask for one.
//!
//! After each commit that changed a row the lane moves a counter that
//! `Db::commits` hands out, so a listener waits for a change rather than
//! polling. Rolled-back, refused and timed-out writes leave it alone.
//!
//! Many rows go through `write_chunked` or `write_chunked_blocking`, one
//! `Lane::Bulk` turn per chunk. Bulk writes are the only ones that wait
//! while the checkpoint service reports the WAL too large or stalled, and
//! the runtime's relief thread checkpoints every second until they can go.
//! Each chunk commits on its own, so any prefix may be all that lands; a
//! chunked write that stops short answers [`ChunkedError`] with the rows
//! that committed, and a caller resumes from there.
//!
//! Layout: [`factory`] builds the runtime, [`handle`] is `Db`, [`read_pool`]
//! holds the two read pools, [`checkpoint`] runs the GH-293
//! checkpoint policy with TRUNCATE reclaim, [`writer`] serializes writes with
//! foreground-burst-8 fairness, [`backup`] copies the live database out with
//! the online backup API, [`durable`] reads and writes the job registry's
//! rows, [`analyze`] keeps the planner statistics fresh, [`fs`]
//! enforces the local-filesystem rule, [`fold`] folds text for
//! accent-insensitive search, `savepoint` runs a part of a write that may
//! fail alone, `scope` records which write and read closures a thread is
//! inside, [`datalock`] is the file lock the server holds and the offline
//! tool takes exclusively, `scratch` is the self-removing directory tests
//! write under, [`statement`] declares the SQL every area keeps in its
//! `sql.rs`, `sql` is this module's own, [`error`] holds the typed
//! errors, and [`cache`] is the provider cache's own throwaway file, with
//! its own writer and readers, and [`undo_dir`] is the folder of undo
//! copies beside the database, which backups carry with it.

pub mod analyze;
pub mod backup;
pub mod cache;
pub mod checkpoint;
pub mod datalock;
pub mod durable;
pub mod error;
pub mod factory;
pub mod fold;
pub mod fs;
pub mod handle;
#[cfg(test)]
mod lane_tests;
pub mod read_pool;
mod savepoint;
mod scope;
#[cfg(any(test, feature = "test-support"))]
pub mod scratch;
pub(crate) mod sql;
pub mod statement;
pub mod undo_dir;
pub mod writer;

pub use analyze::AnalyzeService;
pub use backup::{
    BACKUP_KEEP, BACKUP_STEP_PAGES, BackupManifest, BackupProgress, BackupReport, BackupService,
    RestoredBackup,
};
pub use checkpoint::{
    ACTIVE_HIGH_WATER_BYTES, ACTIVE_LOW_WATER_BYTES, BackpressurePolicy, CHECKPOINT_CADENCE,
    CHECKPOINT_READER_BLOCKED_BOUND, CheckpointMode, CheckpointOutcome, CheckpointService,
    Observation, TRUNCATE_MIN_INTERVAL,
};
pub use durable::{JobState, LoopRecord, list_loops};
pub use error::{ChunkedError, DbError, map_sqlx_busy, rusqlite_is_busy, sqlx_is_busy};
pub use factory::{
    ACQUIRE_TIMEOUT, BOOT_WAIT, BUSY_TIMEOUT, CACHE_SIZE_KIB, DbConfig, DbRuntime, IDLE_TIMEOUT,
    MAX_READERS, MIN_CONNECTIONS, MMAP_SIZE, WAL_AUTOCHECKPOINT, open_runtime,
};
pub use fold::fold_text;
pub use fs::{filesystem_is_local, reject_remote_filesystem, reject_symlink, same_filesystem};
pub use handle::Db;
pub use read_pool::{LONG_READERS, OldestRead, ReadActivity};
pub use savepoint::savepoint;
pub use scope::block_on;
pub use undo_dir::UndoDir;
#[cfg(any(test, feature = "test-support"))]
pub use writer::TEST_PATIENCE;
pub use writer::{
    BACKGROUND_CHUNK_ROWS, BACKGROUND_HARD_BUDGET, BACKGROUND_QUEUE_CAPACITY,
    BACKGROUND_SOFT_BUDGET, Budget, CancelFlag, Durable, DurableLane, FOREGROUND_BURST,
    FOREGROUND_HARD_BUDGET, FOREGROUND_QUEUE_CAPACITY, FOREGROUND_SOFT_BUDGET,
    FOREGROUND_WAIT_LIMIT, HOLDER_LOG_EVERY, Lane, LaneIdle, LaneLimits, LaneWatch, ReliefPass,
    WAIT_BUCKET_BOUNDS_MS, WATCH_TICK, WaitTimes, WalHold, apply_connection_pragmas, decide_lane,
    open_connection,
};
