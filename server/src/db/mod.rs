//! SQLite runtime: pool factory, checkpoint, writer lanes, backup, durable fabric.
//!
//! This module owns every SQLite handle in the process. Reads go through the
//! [`factory::DbRuntime`] pool; every write goes through the
//! [`writer::WriteLane`] as one synchronous closure per transaction. There is
//! no async API on the write path, so holding a write transaction across an
//! await is unrepresentable.
//!
//! Writes take turns on the lane's one connection. Async code calls
//! `write`, whose closure runs on the lane's own thread; synchronous code on
//! a blocking thread calls `write_blocking` and runs its closure itself, so
//! it may borrow. Neither needs a runtime to make progress. A closure that
//! writes to its own lane is refused with `NestedWrite`. Synchronous code
//! that must wait on a future goes through [`block_on`], which refuses
//! inside a write closure.
//!
//! Layout: [`factory`] builds the runtime, [`checkpoint`] runs the GH-293
//! checkpoint policy with TRUNCATE reclaim, [`writer`] serializes writes with
//! foreground-burst-8 fairness, [`backup`] copies the live database out with
//! the online backup API, [`durable`] carries the worker wakeups and job
//! registry, [`analyze`] keeps the planner statistics fresh, [`fs`]
//! enforces the local-filesystem rule, [`fold`] keeps the accent-insensitive
//! search function, `scope` records which write closures a thread is
//! inside, and [`error`] holds the typed errors.

pub mod analyze;
pub mod backup;
pub mod checkpoint;
pub mod durable;
pub mod error;
pub mod factory;
pub mod fold;
pub mod fs;
mod scope;
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
pub use durable::{DurableWorkWakeups, JobKind, JobRecord, JobState, WakeupChannel};
pub use error::{DbError, map_sqlx_busy, rusqlite_is_busy, sqlx_is_busy};
pub use factory::{
    ACQUIRE_TIMEOUT, BUSY_TIMEOUT, CACHE_SIZE_KIB, DbConfig, DbRuntime, IDLE_TIMEOUT, MAX_READERS,
    MIN_CONNECTIONS, MMAP_SIZE, WAL_AUTOCHECKPOINT, open_runtime,
};
pub use fold::{fold_text, register_fold};
pub use fs::{filesystem_is_local, reject_remote_filesystem, reject_symlink, same_filesystem};
pub use scope::block_on;
pub use writer::{
    BACKGROUND_CHUNK_ROWS, CancelFlag, FOREGROUND_BURST, FOREGROUND_SOFT_BUDGET,
    LANE_QUEUE_CAPACITY, Lane, LaneIdle, OpError, WAIT_BUCKET_BOUNDS_MS, WRITE_HARD_BUDGET,
    WaitTimes, WriteLane, apply_connection_pragmas, decide_lane, open_connection,
    open_journal_connection,
};
