//! Runtime factory: one call from a path to a serving database.
//!
//! [`open_runtime`] runs the boot checks in order: create the parent
//! directory, refuse symlinks and network filesystems, take a verified
//! backup when the schema is behind, run the schema migrations and assert
//! `user_version` on a boot pool, open the read-only reader pool with the
//! full pragma set, then start the writer lane, open the read pools and
//! wire the checkpoint, wakeup, and backup services.
//!
//! Backups live in `backups/` next to the database file, so they share its
//! volume and filesystem (the backup stages and renames in place).
//!
//! Connection budget: the sqlx pool carries up to 7 reader connections, the
//! writer lane owns 1 dedicated connection, and the read pools hold 7
//! request connections and 2 long ones, all open from boot, for up to 17
//! handles with exactly one writer. The sqlx pool goes once the last area
//! reads through [`Db`]. The writer stays a true single connection, which
//! is what lets the lane enforce the no-await-inside-write-transaction rule
//! structurally.
//!
//! Schema hooks used (from [`crate::schema`]):
//! `crate::schema::apply_migrations(&SqlitePool) -> Result<(), SchemaError>`
//! and `crate::schema::assert_migrated(&SqlitePool) -> Result<(), SchemaError>`.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use sqlx::{
    SqlitePool,
    pool::PoolConnectionMetadata,
    sqlite::{
        SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePoolOptions,
        SqliteSynchronous,
    },
};

use super::{
    analyze::AnalyzeService,
    backup::BackupService,
    checkpoint::{ACTIVE_HIGH_WATER_BYTES, ACTIVE_LOW_WATER_BYTES, CheckpointService},
    durable::DurableWorkWakeups,
    error::DbError,
    fs::{reject_remote_filesystem, reject_symlink},
    handle::Db,
    read_pool::{ReadActivity, ReadPools},
    writer::{CancelFlag, LaneLimits, LaneWatch, ReliefPass, Ticker, WriteLane},
};

/// Reader connections in the sqlx pool, and request connections in the read
/// pool, which is also how many request reads run at once. Measured on a
/// 12-core host beside a busy writer: with 10 or more running readers the
/// writer commits a third less and the heavy reads slow down, and with 4
/// the cheap reads wait about twice as long.
pub const MAX_READERS: u32 = 7;
/// Warm connections kept ready.
pub const MIN_CONNECTIONS: u32 = 1;
/// Pool checkout horizon, shared with the busy timeout.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// Lock wait on every connection. The v2 value, unchanged.
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
/// Idle connections older than this are reaped. No max lifetime: SQLite
/// connections do not go stale.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Memory-mapped I/O window. The v2 catalog-validation value, unchanged.
pub const MMAP_SIZE: i64 = 67_108_864;
/// Page cache per connection, in KiB. Negative means KiB in SQLite units.
/// Cut from 16 MiB after measurement: heavy reads held tens of MB of
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
    /// Reader connections in the sqlx pool and request connections in the
    /// read pool. Defaults to [`MAX_READERS`].
    pub readers: u32,
    /// Lock wait on pool connections. Defaults to [`BUSY_TIMEOUT`].
    pub busy_timeout: Duration,
    /// How long shutdown lets queued writes finish. Defaults to
    /// [`crate::config::DEFAULT_SHUTDOWN_GRACE`].
    pub shutdown_grace: Duration,
    /// The lane's time limits. Not a setting: only a test changes them.
    lane_limits: LaneLimits,
    /// The WAL sizes, high then low, at which bulk writes are held and let
    /// go. Not a setting: only a test changes them.
    wal_water: (i64, i64),
    /// How long a request read waits for a pooled connection before `Busy`,
    /// in the read pool and the sqlx reader pool. Not a setting: only a test
    /// changes it.
    read_wait: Duration,
}

impl DbConfig {
    /// Boot with defaults against one database file.
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            readers: MAX_READERS,
            busy_timeout: BUSY_TIMEOUT,
            shutdown_grace: crate::config::DEFAULT_SHUTDOWN_GRACE,
            lane_limits: LaneLimits::default(),
            wal_water: (ACTIVE_HIGH_WATER_BYTES, ACTIVE_LOW_WATER_BYTES),
            read_wait: ACQUIRE_TIMEOUT,
        }
    }

    /// Bound shutdown's wait for queued writes by the deployment's grace.
    pub fn with_shutdown_grace(mut self, grace: Duration) -> Self {
        self.shutdown_grace = grace;
        self
    }

    /// Other lane time limits, so a test can reach them without waiting out
    /// the real ones.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_lane_limits(mut self, limits: LaneLimits) -> Self {
        self.lane_limits = limits;
        self
    }

    /// Lower water marks for the WAL hold, so a test fills the WAL with
    /// kilobytes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_wal_water(mut self, high: i64, low: i64) -> Self {
        self.wal_water = (high, low);
        self
    }

    /// A shorter wait for a pooled connection, so a test reaches `Busy`
    /// without waiting out the real one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_read_wait(mut self, wait: Duration) -> Self {
        self.read_wait = wait;
        self
    }
}

/// The serving database: pool, writer lane, and services in one handle.
#[derive(Debug)]
pub struct DbRuntime {
    path: PathBuf,
    db: Db,
    reads: ReadActivity,
    /// The lane's watch and relief threads and the read pools' pass. They
    /// end with this handle, so a runtime that is dropped without
    /// [`DbRuntime::shutdown`] leaves no thread behind. Never owned by
    /// [`Db`], whose clones outlive the runtime.
    watch: LaneWatch,
    shutdown_grace: Duration,
    checkpoint: CheckpointService,
    wakeups: DurableWorkWakeups,
    backups: BackupService,
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

    let backup_dir = path
        .parent()
        .map(|parent| parent.join("backups"))
        .unwrap_or_else(|| PathBuf::from("backups"));
    let backups = BackupService::new(&path, &backup_dir);
    backup_before_upgrade(&path, &backups).await?;

    // Migrations run on a writable pool of their own, closed before the
    // reader pool opens, so every reader connection is read-only from the
    // moment it opens. The boot pool connects eagerly with the same pragma
    // set, so a database that refuses it still fails boot; the reader pool
    // then opens its connections as reads need them.
    let boot = open_pool(
        &path,
        config.readers,
        config.busy_timeout,
        ACQUIRE_TIMEOUT,
        false,
    )
    .await?;
    let migrated = crate::schema::apply_migrations(&boot).await;
    boot.close().await;
    migrated?;
    let pool = open_pool(
        &path,
        config.readers,
        config.busy_timeout,
        config.read_wait,
        true,
    )
    .await?;

    let lane = WriteLane::open_limited(&path, config.lane_limits)?;
    // Read-only connections, so they open after the writer: it is the
    // writer that makes sure the WAL and its index exist.
    let pools = ReadPools::open(
        &path,
        lane.database(),
        config.readers as usize,
        config.busy_timeout,
        config.read_wait,
    )?;
    let reads = pools.activity();
    let (high_water, low_water) = config.wal_water;
    let checkpoint =
        CheckpointService::new(&path, pool.clone(), lane.idle_state(), lane.wal_hold())
            .with_water(high_water, low_water)
            .with_pooled_reads(reads.clone());
    // Cloned before the statistics service is attached: the relief thread
    // shares the policy, the hold and the latest record, and owns nothing
    // that keeps the lane or the read pools alive.
    let relief = checkpoint.clone();
    let oldest = reads.clone();
    let checkpoint = checkpoint.with_analyze(AnalyzeService::new(lane.clone(), pool.clone()));
    let mut watch = lane.watch(move || ReliefPass {
        error: relief.run_relief().error,
        oldest_read: oldest.oldest(),
    })?;
    // The pools' pass has a thread of its own, so a slow pass over the lane
    // never delays a reader's `Busy` and the other way round. It holds the
    // pools weakly and stops with the rest of the watch.
    watch.add(Ticker::start(
        "droppedneedle-db-readers",
        config.lane_limits.watch_tick,
        &pools,
        |pools| pools.pass(),
    )?);
    let wakeups = DurableWorkWakeups::new(pool.clone());

    Ok(DbRuntime {
        path,
        db: Db::new(lane, pools, pool),
        reads,
        watch,
        shutdown_grace: config.shutdown_grace,
        checkpoint,
        wakeups,
        backups,
    })
}

/// Take one verified backup when the stamp is behind the binary, before
/// the runtime opens a connection or migrates. A missing or fresh database
/// (stamp 0, no tables) has nothing to lose and skips; a stamp-0 file with
/// tables is a real database and gets its backup. A failure stops the
/// boot: migrating without a way back risks the catalog.
async fn backup_before_upgrade(path: &Path, backups: &BackupService) -> Result<(), DbError> {
    if !path.exists() {
        return Ok(());
    }
    let probe = path.to_owned();
    let (found, tables) = tokio::task::spawn_blocking(move || schema_stamp(&probe))
        .await
        .map_err(|_| DbError::WriteFailed {
            operation: "pre-upgrade check".to_owned(),
            cause: "schema check task failed to join".to_owned(),
        })??;
    let expected = crate::schema::latest_version();
    if found >= expected || (found == 0 && tables == 0) {
        return Ok(());
    }
    tracing::info!(
        found,
        expected,
        "schema upgrade pending; taking a backup first"
    );
    let report = backups.backup(None, &CancelFlag::never()).await?;
    tracing::info!(backup = %report.path.display(), "pre-upgrade backup complete");
    Ok(())
}

/// `user_version` and the user table count, read without writing.
fn schema_stamp(path: &Path) -> Result<(i64, i64), DbError> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let found = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let tables = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok((found, tables))
}

impl DbRuntime {
    /// Live database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The handle areas read and write through.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// What the read pools are doing: connections out, callers waiting, the
    /// oldest open read.
    pub fn read_activity(&self) -> &ReadActivity {
        &self.reads
    }

    /// Reader pool. Reads only, enforced by `query_only=ON` on every
    /// connection it opens; every write goes through
    /// [`DbRuntime::lane`].
    pub fn pool(&self) -> &SqlitePool {
        self.db.sqlx()
    }

    /// The single writer.
    pub fn lane(&self) -> &WriteLane {
        self.db.lane()
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

    /// Clean shutdown in order: drain the writer lane, stop the watch
    /// threads, close the read pools, reclaim the WAL, then close the sqlx
    /// pool. Draining first means no write lands after the reclaim; the
    /// reclaim itself is best effort (a held lock leaves the WAL in place),
    /// so a quiet shutdown leaves no `-wal` or `-shm` behind. The drain is
    /// bounded by the configured grace: a write that has not returned by
    /// then is logged and left. Closing the lane lifts the WAL hold, so a
    /// held bulk write runs in the drain like any other and does not use up
    /// the grace.
    ///
    /// The read pools get what is left of that same grace, not one of their
    /// own: a read still running when it ends is named in the log and left,
    /// the reclaim then fails fast and the WAL stays for the next start.
    pub async fn shutdown(self) {
        // A grace too long to add to the clock is no bound at all.
        let deadline = Instant::now().checked_add(self.shutdown_grace);
        self.db.lane().shutdown(self.shutdown_grace).await;
        let (watch, checkpoint) = (self.watch, self.checkpoint.clone());
        let pools = Arc::clone(self.db.reads());
        // Stopping the watch can wait out a relief pass in flight, so it
        // happens off the runtime, and before the reclaim that pass would
        // otherwise block.
        let _ = tokio::task::spawn_blocking(move || {
            watch.stop();
            // Before the reclaim: it needs every reader gone, and SQLite
            // removes `-wal` and `-shm` only when the last handle to close
            // is a read-write one, which a pooled connection is not.
            pools.close(deadline);
            checkpoint.shutdown_truncate()
        })
        .await;
        self.db.sqlx().close().await;
    }
}

/// Open a pool with the full connection pragma set. `after_connect` applies
/// every pragma when a connection opens and fails it loudly on drift; with
/// `read_only` it also pins `query_only=ON`, so the pool can never write,
/// and opens no connection until a read asks for one. `acquire_wait` is how
/// long a caller waits for a connection.
///
/// A checkout does no work on the connection: no pragma check and no ping.
/// The pragmas are per connection and nothing in the process changes them
/// after open (WAL is a property of the file, and a read-only connection
/// has no writes for foreign keys to check), and a ping cannot tell an
/// in-process SQLite connection anything.
async fn open_pool(
    path: &Path,
    readers: u32,
    busy_timeout: Duration,
    acquire_wait: Duration,
    read_only: bool,
) -> Result<SqlitePool, DbError> {
    let connect = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(busy_timeout)
        .foreign_keys(true);
    let options = SqlitePoolOptions::new()
        .max_connections(readers.max(1))
        .min_connections(MIN_CONNECTIONS.min(readers.max(1)))
        .acquire_timeout(acquire_wait)
        .test_before_acquire(false)
        .idle_timeout(Some(IDLE_TIMEOUT))
        .max_lifetime(None)
        .after_connect(
            move |connection: &mut SqliteConnection, _meta: PoolConnectionMetadata| {
                Box::pin(async move {
                    let ready = ready_connection(connection, busy_timeout, read_only).await;
                    // Without this line a pool that can never open a
                    // connection would only show as callers told "busy".
                    if let Err(error) = &ready {
                        tracing::error!(%error, "database connection could not be made ready");
                    }
                    ready
                })
            },
        );
    if read_only {
        return Ok(options.connect_lazy_with(connect));
    }
    Ok(options.connect_with(connect).await?)
}

/// Apply the pragma set to a new connection, then the read-only pin when
/// the pool is the reader pool.
async fn ready_connection(
    connection: &mut SqliteConnection,
    busy_timeout: Duration,
    read_only: bool,
) -> Result<(), sqlx::Error> {
    apply_pragmas(connection, busy_timeout).await?;
    if read_only {
        sqlx::query("PRAGMA query_only=ON")
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
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
