//! Runtime factory: one call from a path to a serving database.
//!
//! [`open_runtime`] runs the boot checks in order: create the parent
//! directory, refuse symlinks and network filesystems, work out the schema
//! plan from the stamp (a stamp the runner refuses stops boot here, before
//! any backup), take a verified backup when the plan changes the file, run
//! the schema runner on one connection with the full pragma set (a file
//! that refuses WAL mode stops boot here), then start the writer lane, open
//! the read pools and wire the checkpoint, wakeup, and backup services.
//!
//! Backups live in `backups/` next to the database file, so they share its
//! volume and filesystem (the backup stages and renames in place).
//!
//! Connection budget: the writer lane owns 1 dedicated connection and the
//! read pools hold 7 request connections and 2 long ones, all open from
//! boot, for 10 handles with exactly one writer. The writer stays a true
//! single connection, which is what lets the lane enforce the
//! no-await-inside-write-transaction rule structurally.
//!
//! The schema comes from [`crate::schema::CHAIN`], applied by
//! [`super::migrate`].

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use rusqlite::Connection;

use crate::clock::Clock;

use super::{
    analyze::AnalyzeService,
    backup::BackupService,
    checkpoint::{ACTIVE_HIGH_WATER_BYTES, ACTIVE_LOW_WATER_BYTES, CheckpointService},
    error::DbError,
    fs::{reject_remote_filesystem, reject_symlink},
    handle::Db,
    migrate::{Stamp, migrate},
    read_pool::{ReadActivity, ReadPools},
    sql,
    writer::{CancelFlag, LaneLimits, LaneWatch, ReliefPass, Ticker, WriteLane, open_connection},
};

/// Request connections in the read pool, which is also how many request
/// reads run at once. Measured on a 12-core host beside a busy writer: with
/// 10 or more running readers the writer commits a third less and the heavy
/// reads slow down, and with 4 the cheap reads wait about twice as long.
pub const MAX_READERS: u32 = 7;
/// How long a request read waits for a pooled connection before `Busy`,
/// the same as the busy timeout.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the boot migration waits on a busy database, and how long a
/// boot step keeps trying while the database answers busy. Boot serves no
/// request, so a slow or swapping disk is waited out, not refused.
pub const BOOT_WAIT: Duration = Duration::from_secs(60);
/// Lock wait on every connection. The v2 value, unchanged.
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
/// Memory-mapped I/O window. The v2 catalog-validation value, unchanged.
pub const MMAP_SIZE: i64 = 67_108_864;
/// Page cache per connection, in KiB. Negative means KiB in SQLite units.
/// Cut from 16 MiB after measurement: heavy reads held tens of MB of
/// cache across connections after the workload, and 2 MiB keeps every
/// read-latency budget green (seeded 100k artists p95 9.5 ms vs 10 ms).
pub const CACHE_SIZE_KIB: i64 = -2048;
/// Frames between automatic checkpoints during bulk writes.
pub const WAL_AUTOCHECKPOINT: i64 = 1000;

/// What [`open_runtime`] needs: the database file, its tuning and the
/// clock its services date their work by.
#[derive(Clone)]
pub struct DbConfig {
    /// Live database file, created with its parent directory when missing.
    pub path: PathBuf,
    /// Wall time for backups, checkpoints and planner statistics: the
    /// platform's, in the server.
    clock: Arc<dyn Clock>,
    /// Request connections in the read pool. Defaults to [`MAX_READERS`].
    pub readers: u32,
    /// Lock wait on pool connections. Defaults to [`BUSY_TIMEOUT`].
    pub busy_timeout: Duration,
    /// How long shutdown lets queued writes finish. Defaults to
    /// [`crate::config::DEFAULT_SHUTDOWN_GRACE`].
    pub shutdown_grace: Duration,
    /// The lane's time limits. Not a setting: only a test changes them.
    pub(in crate::db) lane_limits: LaneLimits,
    /// The WAL sizes, high then low, at which bulk writes are held and let
    /// go. Not a setting: only a test changes them.
    pub(in crate::db) wal_water: (i64, i64),
    /// How long a request read waits for a pooled connection before `Busy`.
    /// Not a setting: only a test changes it.
    pub(in crate::db) read_wait: Duration,
}

impl DbConfig {
    /// Boot with defaults against one database file, dating backups,
    /// checkpoints and planner statistics by `clock`.
    pub fn new(path: &Path, clock: Arc<dyn Clock>) -> Self {
        Self {
            path: path.to_owned(),
            clock,
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
    backups: Arc<BackupService>,
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
    let backups = Arc::new(BackupService::new(
        &path,
        &backup_dir,
        Arc::clone(&config.clock),
    ));
    let upgrade = backup_before_upgrade(&path, &backups).await?;
    migrate_at_boot(&path, upgrade).await?;

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
    let db = Db::new(lane, Arc::clone(&pools));
    let lane = db.lane();
    let (high_water, low_water) = config.wal_water;
    let checkpoint = CheckpointService::new(
        &path,
        reads.clone(),
        lane.idle_state(),
        lane.wal_hold(),
        Arc::clone(&config.clock),
    )
    .with_water(high_water, low_water);
    // Cloned before the statistics service is attached: the relief thread
    // shares the policy, the hold and the latest record, and owns nothing
    // that keeps the lane or the read pools alive. A `Db` there would keep
    // both alive from the watch it belongs to, and a runtime dropped
    // without shutdown would leak the watch's threads.
    let relief = checkpoint.clone();
    let oldest = reads.clone();
    let checkpoint =
        checkpoint.with_analyze(AnalyzeService::new(db.clone(), Arc::clone(&config.clock)));
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
    Ok(DbRuntime {
        path,
        db,
        reads,
        watch,
        shutdown_grace: config.shutdown_grace,
        checkpoint,
        backups,
    })
}

/// Work out the schema plan from the stamp, read without writing, before
/// the runtime opens a connection or migrates. A stamp the runner refuses
/// stops boot here, before any backup: backups rotate, and a refusal
/// repeated at every restart must not rotate out the backups it tells the
/// user to restore. Otherwise one verified backup is taken whenever the
/// plan changes the file. A missing or empty database has nothing to lose
/// and skips. A failed backup stops the boot: migrating without a way back
/// risks the catalog. True when the plan changes a database that holds
/// data.
async fn backup_before_upgrade(path: &Path, backups: &BackupService) -> Result<bool, DbError> {
    if !path.exists() {
        return Ok(false);
    }
    let probe = path.to_owned();
    let stamp = tokio::task::spawn_blocking(move || Stamp::read_file(&probe))
        .await
        .map_err(|_| DbError::WriteFailed {
            operation: "pre-upgrade check".to_owned(),
            cause: "schema check task failed to join".to_owned(),
        })??;
    let plan = crate::schema::CHAIN.plan(stamp)?;
    if !plan.needs_backup() {
        return Ok(false);
    }
    tracing::info!(
        found = stamp.version,
        expected = crate::schema::latest_version(),
        "schema upgrade pending; taking a backup first"
    );
    let report = backups.backup(None, &CancelFlag::never()).await?;
    tracing::info!(backup = %report.path.display(), "pre-upgrade backup complete");
    Ok(true)
}

/// How long opening the boot connection may take before the log says boot
/// is waiting on the database.
const SLOW_BOOT: Duration = BUSY_TIMEOUT;

/// Run the schema runner on a connection of its own, closed before the
/// writer and the read pools open. Boot serves nothing yet, so it waits out
/// a busy database for up to [`BOOT_WAIT`], and says so once when the
/// connection is slow to open; the migration itself may take as long as it
/// needs. A file that refuses WAL mode stops boot at once.
///
/// After an `upgrade` the last planner statistics run is forgotten: a
/// migration may have added indexes or rebuilt tables that SQLite holds no
/// statistics for, and the next check takes them again.
async fn migrate_at_boot(path: &Path, upgrade: bool) -> Result<(), DbError> {
    let joined = |_| DbError::WriteFailed {
        operation: "schema migration".to_owned(),
        cause: "migration task failed to join".to_owned(),
    };
    let path = path.to_owned();
    let mut open = tokio::task::spawn_blocking(move || -> Result<Connection, DbError> {
        let connection = open_connection(&path)?;
        let mode: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(DbError::WalRefused { mode });
        }
        connection.busy_timeout(BOOT_WAIT)?;
        Ok(connection)
    });
    let opened = match tokio::time::timeout(SLOW_BOOT, &mut open).await {
        Ok(opened) => opened,
        Err(_) => {
            tracing::warn!(
                waited_ms = SLOW_BOOT.as_millis() as u64,
                "the database is slow to open; boot is still waiting on it"
            );
            open.await
        }
    };
    let connection = opened.map_err(joined)??;
    tokio::task::spawn_blocking(move || -> Result<(), DbError> {
        let connection = migrate(connection, &crate::schema::CHAIN)?;
        if upgrade {
            connection.execute(sql::FORGET_ANALYZE.text(), [])?;
        }
        Ok(())
    })
    .await
    .map_err(joined)?
}

/// Percent-encode the characters a SQLite URI path cannot carry raw.
#[must_use]
pub fn sqlite_uri_path(path: &Path) -> String {
    let mut out = String::new();
    for ch in path.to_string_lossy().chars() {
        match ch {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            other => out.push(other),
        }
    }
    out
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

    /// Checkpoint runner with the latest-record export.
    pub fn checkpoint(&self) -> &CheckpointService {
        &self.checkpoint
    }

    /// The one backup and restore service: boot's pre-upgrade backup and
    /// the admin route share it, so their runs take distinct names and
    /// never sweep each other's staging files.
    pub fn backups(&self) -> &Arc<BackupService> {
        &self.backups
    }

    /// Clean shutdown in order: drain the writer lane, stop the watch
    /// threads, close the read pools, then reclaim the WAL. Draining first
    /// means no write lands after the reclaim; the reclaim itself is best
    /// effort (a held lock leaves the WAL in place), so a quiet shutdown
    /// leaves no `-wal` or `-shm` behind. The drain is bounded by the
    /// configured grace: a write that has not returned by then is logged and
    /// left. Closing the lane lifts the WAL hold, so a held bulk write runs
    /// in the drain like any other and does not use up the grace.
    ///
    /// The read pools get what is left of that same grace, not one of their
    /// own: a read still running when it ends is named in the log and left,
    /// the reclaim then fails fast and the WAL stays for the next start. The
    /// watch stop and the reclaim are bounded by that grace too: a step still
    /// running when it ends is left to finish in the background.
    pub async fn shutdown(self) {
        let grace = self.shutdown_grace;
        self.shutdown_within(grace).await;
    }

    /// [`DbRuntime::shutdown`] bounded by `grace` instead of the configured
    /// grace: the server gives it what is left of its one shutdown budget.
    /// The watch stop, the read pool close and the WAL reclaim get what is
    /// left of that bound, and a step still running past it is left to finish
    /// in the background. A step that panics is logged.
    pub async fn shutdown_within(self, grace: Duration) {
        // A grace too long to add to the clock is no bound at all.
        let deadline = Instant::now().checked_add(grace);
        self.db.lane().shutdown(grace).await;
        let (watch, checkpoint) = (self.watch, self.checkpoint.clone());
        let pools = Arc::clone(self.db.reads());
        // Stopping the watch can wait out a relief pass in flight, so it
        // happens off the runtime, and before the reclaim that pass would
        // otherwise block.
        let blocking = tokio::task::spawn_blocking(move || {
            watch.stop();
            // Before the reclaim: it needs every reader gone, and SQLite
            // removes `-wal` and `-shm` only when the last handle to close
            // is a read-write one, which a pooled connection is not.
            pools.close(deadline);
            checkpoint.shutdown_truncate()
        });
        let joined = match deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                tokio::time::timeout(left, blocking).await.ok()
            }
            None => Some(blocking.await),
        };
        // The reclaim logs its own outcome; a step that panicked is logged
        // here.
        match joined {
            None => {
                tracing::warn!("database shutdown steps still running at the shutdown bound");
            }
            Some(Err(error)) => tracing::error!(%error, "a database shutdown step failed"),
            Some(Ok(_)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::scratch::ScratchDir, test_support::template::copy_to};

    /// Shutdown returns at its grace while a watch pass is still running.
    /// The pass holds until the test releases it, and shutdown must not wait
    /// for it.
    #[tokio::test]
    async fn shutdown_gives_up_on_a_watch_pass_at_the_grace() {
        let dir = ScratchDir::new("factory-shutdown").unwrap();
        let db = dir.join("library.db");
        copy_to(&db).unwrap();
        let mut runtime = open_runtime(&DbConfig::new(
            &db,
            std::sync::Arc::new(crate::clock::SystemClock),
        ))
        .await
        .unwrap();

        let shared = Arc::new(());
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let held = Ticker::start(
            "factory-held-pass",
            Duration::from_millis(5),
            &shared,
            move |_| {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
            },
        )
        .unwrap();
        runtime.watch.add(held);
        entered.recv_timeout(Duration::from_secs(10)).unwrap();

        let grace = Duration::from_millis(200);
        let finished = tokio::time::timeout(
            grace + Duration::from_secs(2),
            runtime.shutdown_within(grace),
        )
        .await;
        let _ = release.send(());
        assert!(
            finished.is_ok(),
            "shutdown waited on a watch pass past its grace"
        );
    }
}
