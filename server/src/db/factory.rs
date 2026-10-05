//! Runtime factory: one call from a path to a serving database.
//!
//! [`open_runtime`] runs the boot checks in order: create the parent
//! directory, refuse symlinks and network filesystems, open the reader pool
//! with the full pragma set, run the sibling schema migrations and assert
//! `user_version`, then start the writer lane and wire the checkpoint,
//! wakeup, and backup services.
//!
//! Connection budget: the pool carries 7 reader connections and the writer
//! lane owns 1 dedicated connection outside the pool, for 8 handles total
//! with exactly one writer. The pool keeps the spec'd shape while the writer
//! stays a true single connection, which is what lets the lane enforce the
//! no-await-inside-write-transaction rule structurally.
//!
//! Schema hooks used (owned by the sibling slice, called but never edited):
//! `crate::schema::apply_migrations(&SqlitePool) -> Result<(), SchemaError>`
//! and `crate::schema::assert_migrated(&SqlitePool) -> Result<(), SchemaError>`.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use sqlx::{
    SqlitePool,
    pool::PoolConnectionMetadata,
    sqlite::{
        SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePoolOptions,
        SqliteSynchronous,
    },
};
use tokio::sync::Notify;

use super::{
    backup::BackupService,
    checkpoint::CheckpointService,
    durable::DurableWorkWakeups,
    error::DbError,
    fs::{reject_remote_filesystem, reject_symlink},
    writer::WriteLane,
};

/// Reader connections in the pool. The writer lane owns one more outside.
pub const MAX_READERS: u32 = 7;
/// Warm connections kept ready.
pub const MIN_CONNECTIONS: u32 = 1;
/// Pool checkout horizon, shared with the busy timeout.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// Lock wait on every connection. The v1 AUD-7 value, unchanged.
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
/// Idle connections older than this are reaped. No max lifetime: SQLite
/// connections do not go stale.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Memory-mapped I/O window. The v1 catalog-validation value, unchanged.
pub const MMAP_SIZE: i64 = 67_108_864;
/// Page cache per connection, in KiB. Negative means KiB in SQLite units.
/// Stage-13 fix F cut this from 16 MiB: heavy reads held tens of MB of
/// cache across connections after the workload, and 2 MiB keeps every
/// read-latency budget green (seeded 100k artists p95 9.5 ms vs 10 ms).
pub const CACHE_SIZE_KIB: i64 = -2048;
/// Frames between automatic checkpoints during bulk writes.
pub const WAL_AUTOCHECKPOINT: i64 = 1000;

/// What [`open_runtime`] needs: the database file and its tuning.
#[derive(Debug, Clone)]
pub struct DbConfig {
    /// Live database file, created with its parent directory when missing.
    pub path: PathBuf,
    /// Reader connections in the pool. Defaults to [`MAX_READERS`].
    pub readers: u32,
    /// Lock wait on pool connections. Defaults to [`BUSY_TIMEOUT`].
    pub busy_timeout: Duration,
}

impl DbConfig {
    /// Boot with defaults against one database file.
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            readers: MAX_READERS,
            busy_timeout: BUSY_TIMEOUT,
        }
    }
}

/// The serving database: pool, writer lane, and services in one handle.
#[derive(Debug)]
pub struct DbRuntime {
    path: PathBuf,
    pool: SqlitePool,
    lane: WriteLane,
    checkpoint: CheckpointService,
    wakeups: DurableWorkWakeups,
    backups: BackupService,
    checkpoint_stop: Arc<Notify>,
}

/// Open the runtime against one database file, running every boot check.
///
/// Fails closed: symlink paths, network filesystems, pragma drift, and
/// schema mismatches each refuse to serve with a typed error.
pub async fn open_runtime(config: &DbConfig) -> Result<DbRuntime, DbError> {
    let path = config.path.clone();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    reject_symlink(&path)?;
    reject_remote_filesystem(&path)?;

    let reads_frozen = Arc::new(AtomicBool::new(false));
    let pool = open_pool(
        &path,
        config.readers,
        config.busy_timeout,
        Arc::clone(&reads_frozen),
    )
    .await?;
    crate::schema::apply_migrations(&pool).await?;
    crate::schema::assert_migrated(&pool).await?;
    reads_frozen.store(true, Ordering::Release);

    let lane = WriteLane::open(&path)?;
    let checkpoint = CheckpointService::new(&path, pool.clone(), lane.idle_state());
    let wakeups = DurableWorkWakeups::new(pool.clone());
    let backup_dir = path
        .parent()
        .map(|parent| parent.join("backups"))
        .unwrap_or_else(|| PathBuf::from("backups"));
    let backups = BackupService::new(&path, &backup_dir);

    Ok(DbRuntime {
        path,
        pool,
        lane,
        checkpoint,
        wakeups,
        backups,
        checkpoint_stop: Arc::new(Notify::new()),
    })
}

impl DbRuntime {
    /// Live database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reader pool. Reads only after boot, enforced by `query_only=ON` on
    /// every checkout once migrations complete; every write goes through
    /// [`DbRuntime::lane`].
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// The single writer.
    pub fn lane(&self) -> &WriteLane {
        &self.lane
    }

    /// Checkpoint runner with the latest-record export.
    pub fn checkpoint(&self) -> &CheckpointService {
        &self.checkpoint
    }

    /// Worker wakeups and job registry.
    pub fn wakeups(&self) -> &DurableWorkWakeups {
        &self.wakeups
    }

    /// Backup and restore service.
    pub fn backups(&self) -> &BackupService {
        &self.backups
    }

    /// Run the steady-state checkpoint loop until [`DbRuntime::shutdown`].
    /// Spawn once per process; pass failures are recorded, never fatal.
    pub async fn run_checkpoint_loop(&self) {
        self.checkpoint
            .run_forever(Arc::clone(&self.checkpoint_stop))
            .await;
    }

    /// Clean shutdown in order: stop the checkpoint loop, drain the writer
    /// lane, reclaim the WAL, then close the pool. Draining first means no
    /// write lands after the reclaim; the reclaim itself is best effort (a
    /// held lock leaves the WAL in place), so a quiet shutdown leaves no
    /// `-wal` or `-shm` behind.
    pub async fn shutdown(self) {
        self.checkpoint_stop.notify_waiters();
        self.lane.shutdown().await;
        let checkpoint = self.checkpoint.clone();
        let _ = tokio::task::spawn_blocking(move || checkpoint.shutdown_truncate()).await;
        self.pool.close().await;
    }
}

/// Open the reader pool with the pragma set from the stage-0 table.
/// `after_connect` applies every pragma and fails the connection loudly on
/// drift; `before_acquire` re-verifies journal mode and FK enforcement at
/// each checkout and recycles strays. Once `reads_frozen` flips (after boot
/// migrations), both hooks pin `query_only=ON` so the pool can no longer
/// write, whether a checkout reuses an idle connection or opens a new one;
/// boot migrations run before the flip.
async fn open_pool(
    path: &Path,
    readers: u32,
    busy_timeout: Duration,
    reads_frozen: Arc<AtomicBool>,
) -> Result<SqlitePool, DbError> {
    let frozen_at_connect = Arc::clone(&reads_frozen);
    let connect = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(busy_timeout)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(readers.max(1))
        .min_connections(MIN_CONNECTIONS.min(readers.max(1)))
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(Some(IDLE_TIMEOUT))
        .max_lifetime(None)
        .after_connect(
            move |connection: &mut SqliteConnection, _meta: PoolConnectionMetadata| {
                let reads_frozen = Arc::clone(&frozen_at_connect);
                Box::pin(async move {
                    apply_pragmas(connection, busy_timeout).await?;
                    // A connection opened on demand skips `before_acquire`,
                    // so it has to pick up the read-only pin here.
                    if reads_frozen.load(Ordering::Acquire) {
                        sqlx::query("PRAGMA query_only=ON")
                            .execute(&mut *connection)
                            .await?;
                    }
                    Ok(())
                })
            },
        )
        .before_acquire(
            move |connection: &mut SqliteConnection, _meta: PoolConnectionMetadata| {
                let reads_frozen = Arc::clone(&reads_frozen);
                Box::pin(async move {
                    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
                        .fetch_one(&mut *connection)
                        .await?;
                    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                        .fetch_one(&mut *connection)
                        .await?;
                    let healthy = mode.eq_ignore_ascii_case("wal") && foreign_keys == 1;
                    if !healthy {
                        tracing::warn!(
                            journal_mode = mode,
                            foreign_keys,
                            "recycling pooled connection with drifted pragmas"
                        );
                    }
                    if healthy && reads_frozen.load(Ordering::Acquire) {
                        sqlx::query("PRAGMA query_only=ON")
                            .execute(&mut *connection)
                            .await?;
                    }
                    Ok(healthy)
                })
            },
        )
        .connect_with(connect)
        .await?;
    Ok(pool)
}

/// The full pragma set, applied to every pooled connection at open.
async fn apply_pragmas(
    connection: &mut SqliteConnection,
    busy_timeout: Duration,
) -> Result<(), sqlx::Error> {
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode=WAL")
        .fetch_one(&mut *connection)
        .await?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(sqlx::Error::Configuration(
            format!("database refused WAL mode, got {mode}").into(),
        ));
    }
    sqlx::query("PRAGMA synchronous=NORMAL")
        .execute(&mut *connection)
        .await?;
    sqlx::query(&format!("PRAGMA busy_timeout={}", busy_timeout.as_millis()))
        .execute(&mut *connection)
        .await?;
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *connection)
        .await?;
    sqlx::query(&format!("PRAGMA mmap_size={MMAP_SIZE}"))
        .execute(&mut *connection)
        .await?;
    sqlx::query("PRAGMA temp_store=MEMORY")
        .execute(&mut *connection)
        .await?;
    sqlx::query(&format!("PRAGMA cache_size={CACHE_SIZE_KIB}"))
        .execute(&mut *connection)
        .await?;
    sqlx::query(&format!("PRAGMA wal_autocheckpoint={WAL_AUTOCHECKPOINT}"))
        .execute(&mut *connection)
        .await?;
    Ok(())
}
