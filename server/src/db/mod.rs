//! SQLite runtime: pool factory, checkpoint, writer lanes, backup, durable fabric.
//!
//! This module owns every SQLite handle in the process. Reads go through the
//! [`factory::DbRuntime`] pool; every write goes through the
//! [`writer::WriteLane`] as one synchronous closure per transaction. There is
//! no async API on the write path, so holding a write transaction across an
//! await is unrepresentable.
//!
//! Layout: [`factory`] builds the runtime, [`checkpoint`] runs the GH-293
//! checkpoint policy with TRUNCATE reclaim, [`writer`] serializes writes with
//! foreground-burst-8 fairness, [`backup`] copies the live database out with
//! the online backup API, [`durable`] carries the worker wakeups and job
//! registry, [`fs`] enforces the local-filesystem rule, [`fold`] keeps the
//! accent-insensitive search function, and [`error`] holds the typed errors.

pub mod backup;
pub mod checkpoint;
pub mod durable;
pub mod error;
pub mod factory;
pub mod fold;
pub mod fs;
pub mod writer;

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
pub use writer::{
    BACKGROUND_CHUNK_ROWS, CancelFlag, FOREGROUND_BURST, FOREGROUND_SOFT_BUDGET,
    LANE_QUEUE_CAPACITY, Lane, LaneIdle, OpError, WRITE_HARD_BUDGET, WriteLane,
    apply_connection_pragmas, decide_lane, open_connection,
};
