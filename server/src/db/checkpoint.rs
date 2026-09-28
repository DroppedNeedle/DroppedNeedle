//! WAL checkpoint policy: GH-293 calibration with TRUNCATE reclaim.
//!
//! Steady state is a `PASSIVE` checkpoint every 30 s from a dedicated
//! connection that fails fast (`busy_timeout=0`): lock contention becomes a
//! `busy` pass that keeps its progress baseline (the v1 F-181 rule), while
//! non-lock errors are recorded distinctly and leave backpressure alone.
//! Backpressure suspends background producers only, never foreground writes,
//! when active WAL crosses 64 MiB or a checkpoint makes no measurable
//! progress for 60 s.
//!
//! v1 never reclaimed: `PASSIVE` leaves checkpointed frames allocated, so a
//! dead `-wal` of several MB survived graceful restarts. v3 adds `TRUNCATE`
//! reclaim when three gates hold: the last `PASSIVE` reported zero active
//! frames, the writer lane is idle and every reader connection is back in
//! the pool, and an hour passed since the last reclaim. A final `TRUNCATE`
//! runs at clean shutdown, so a stopped database leaves no `-wal` behind.
//!
//! Every pass records its outcome for the log and keeps the latest one for
//! the admin health endpoint (stage 10 reads [`CheckpointService::latest`]).

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::{Notify, RwLock};

use super::writer::LaneIdle;

/// `PASSIVE` checkpoint cadence. The GH-293 owner calibration, unchanged.
pub const CHECKPOINT_CADENCE: Duration = Duration::from_secs(30);
/// Stall bound: no measurable `PASSIVE` progress for this long suspends
/// background producers. The GH-293 owner calibration, unchanged.
pub const CHECKPOINT_READER_BLOCKED_BOUND: Duration = Duration::from_secs(60);
/// Active-WAL high water: above this, background producers suspend.
pub const ACTIVE_HIGH_WATER_BYTES: i64 = 64 * 1024 * 1024;
/// Active-WAL low water: at or below this, suspended producers resume.
pub const ACTIVE_LOW_WATER_BYTES: i64 = 16 * 1024 * 1024;
/// Minimum gap between live `TRUNCATE` reclaims. Shutdown reclaim ignores it.
pub const TRUNCATE_MIN_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Which checkpoint mode produced an outcome record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CheckpointMode {
    /// Steady-state bounded pass.
    Passive,
    /// Allocation reclaim under the three gates, or at shutdown.
    Truncate,
}

/// One recorded checkpoint pass. `active_bytes` counts uncheckpointed frames
/// times the page size; `wal_file_bytes` is the `-wal` allocation on disk,
/// which may hold dead frames the next `TRUNCATE` reclaims. A `busy` pass
/// with `active_bytes == -1` carried no frame evidence (F-181) and never
/// moves the progress baseline.
#[derive(Debug, Clone, Serialize)]
pub struct CheckpointOutcome {
    /// Wall-clock time of the pass.
    pub at: SystemTime,
    /// Mode that ran.
    pub mode: CheckpointMode,
    /// True when a lock contender blocked the pass.
    pub busy: bool,
    /// Total frames in the log, or 0 on an unmeasured pass.
    pub log_frames: i64,
    /// Frames checkpointed, or 0 on an unmeasured pass.
    pub checkpointed_frames: i64,
    /// Uncheckpointed bytes, or -1 on an unmeasured pass or error.
    pub active_bytes: i64,
    /// `-wal` file size in bytes.
    pub wal_file_bytes: u64,
    /// How long the pass took.
    pub duration: Duration,
    /// Non-lock failure text, if the pass failed.
    pub error: Option<String>,
    /// Backpressure state after the pass.
    pub suspended: bool,
    /// How long the reader-blocked stall guard has been held, if at all.
    pub reader_blocked: Duration,
    /// True when the pass made measurable progress over the last one.
    pub progress: bool,
}

/// Pure backpressure machine: the v1 `_update_state` rule. Suspension needs
/// active WAL over the high water or a stall past the reader bound; resume
/// needs active WAL at/below the low water or any measurable progress.
/// Unmeasured passes never feed the baseline and never clear suspension.
#[derive(Debug)]
pub struct BackpressurePolicy {
    suspended: bool,
    busy_since: Option<Instant>,
    last_log_frames: Option<i64>,
    last_checkpointed_frames: Option<i64>,
}

/// What one observation changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// Backpressure state after the observation.
    pub suspended: bool,
    /// How long the stall guard has been held, if at all.
    pub reader_blocked: Duration,
    /// True when the pass advanced over the previous baseline.
    pub progress: bool,
}

impl BackpressurePolicy {
    /// A fresh machine: producers running, no baseline, no stall.
    pub fn new() -> Self {
        Self {
            suspended: false,
            busy_since: None,
            last_log_frames: None,
            last_checkpointed_frames: None,
        }
    }

    /// Current suspension state.
    pub fn suspended(&self) -> bool {
        self.suspended
    }

    /// Fold one pass into the machine.
    pub fn observe(
        &mut self,
        busy: bool,
        log_frames: i64,
        checkpointed_frames: i64,
        active_bytes: i64,
        now: Instant,
        measured: bool,
    ) -> Observation {
        let progress = if measured {
            let advanced = self.last_log_frames.is_some_and(|last| log_frames < last)
                || self
                    .last_checkpointed_frames
                    .is_some_and(|last| checkpointed_frames > last);
            self.last_log_frames = Some(log_frames);
            self.last_checkpointed_frames = Some(checkpointed_frames);
            advanced
        } else {
            false
        };
        let reader_blocked = if busy {
            let since = *self.busy_since.get_or_insert(now);
            now.saturating_duration_since(since)
        } else {
            self.busy_since = None;
            Duration::ZERO
        };
        if active_bytes > ACTIVE_HIGH_WATER_BYTES
            || (self.busy_since.is_some() && reader_blocked >= CHECKPOINT_READER_BLOCKED_BOUND)
        {
            if !self.suspended {
                tracing::warn!(
                    active_bytes,
                    busy,
                    reader_blocked_secs = reader_blocked.as_secs_f64(),
                    "WAL backpressure: suspending background producers"
                );
            }
            self.suspended = true;
        } else if self.suspended
            && ((0..=ACTIVE_LOW_WATER_BYTES).contains(&active_bytes) || progress)
        {
            tracing::info!(
                active_bytes,
                "WAL backpressure cleared: resuming background producers"
            );
            self.suspended = false;
        }
        Observation {
            suspended: self.suspended,
            reader_blocked,
            progress,
        }
    }
}

impl Default for BackpressurePolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Checkpoint runner. Cloneable handle over shared state; blocking passes
/// run through `spawn_blocking` so the runtime never stalls.
#[derive(Clone, Debug)]
pub struct CheckpointService {
    db_path: PathBuf,
    pool: SqlitePool,
    lane_idle: Arc<LaneIdle>,
    policy: Arc<Mutex<BackpressurePolicy>>,
    latest: Arc<RwLock<Option<CheckpointOutcome>>>,
    last_truncate: Arc<Mutex<Option<Instant>>>,
    resume: Arc<Notify>,
}

impl CheckpointService {
    /// Wire the service. The pool is read for reader-idle checks only; the
    /// lane handle feeds the writer-idle half of the `TRUNCATE` gate.
    pub fn new(db_path: &Path, pool: SqlitePool, lane_idle: Arc<LaneIdle>) -> Self {
        Self {
            db_path: db_path.to_owned(),
            pool,
            lane_idle,
            policy: Arc::new(Mutex::new(BackpressurePolicy::new())),
            latest: Arc::new(RwLock::new(None)),
            last_truncate: Arc::new(Mutex::new(None)),
            resume: Arc::new(Notify::new()),
        }
    }

    /// True when background producers must yield. Never gates foreground.
    pub fn background_suspended(&self) -> bool {
        self.policy
            .lock()
            .map(|policy| policy.suspended())
            .unwrap_or(false)
    }

    /// Latest recorded pass, for the admin health endpoint.
    pub async fn latest(&self) -> Option<CheckpointOutcome> {
        self.latest.read().await.clone()
    }

    /// Latest recorded pass without awaiting, for non-async callers.
    pub fn try_latest(&self) -> Option<CheckpointOutcome> {
        self.latest.try_read().ok().and_then(|guard| guard.clone())
    }

    /// Wait until backpressure clears. Background producers call this per
    /// unit of work; it returns at once when nothing is suspended. The
    /// waiter registers before reading the flag, so a resume landing
    /// between the check and the sleep still wakes this call.
    pub async fn wait_for_resume(&self) {
        loop {
            let notified = self.resume.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.background_suspended() {
                break;
            }
            notified.await;
        }
    }

    /// One bounded `PASSIVE` pass on a dedicated fail-fast connection.
    /// Blocking; callers offload it with `spawn_blocking`.
    pub fn run_once(&self) -> CheckpointOutcome {
        self.run_pass(CheckpointMode::Passive)
    }

    /// Reclaim dead `-wal` allocation when the three gates hold: the last
    /// `PASSIVE` saw zero active frames, writer and readers are idle, and an
    /// hour passed since the last reclaim. Returns the `TRUNCATE` outcome,
    /// or `None` when a gate held the reclaim back. Blocking like `run_once`.
    pub fn maybe_truncate(&self, now: Instant) -> Option<CheckpointOutcome> {
        let quiet = self
            .try_latest()
            .is_some_and(|latest| latest.active_bytes == 0 && latest.error.is_none());
        if !quiet {
            return None;
        }
        if !self.lane_idle.is_idle() || !self.readers_idle() {
            return None;
        }
        let due = self
            .last_truncate
            .lock()
            .map(|guard| {
                guard
                    .is_none_or(|last| now.saturating_duration_since(last) >= TRUNCATE_MIN_INTERVAL)
            })
            .unwrap_or(false);
        if !due {
            return None;
        }
        let outcome = self.run_pass(CheckpointMode::Truncate);
        if outcome.error.is_none()
            && !outcome.busy
            && let Ok(mut guard) = self.last_truncate.lock()
        {
            *guard = Some(now);
        }
        Some(outcome)
    }

    /// Final reclaim at clean shutdown. Best effort: a busy lock is recorded
    /// and the shutdown proceeds. Blocking like `run_once`.
    pub fn shutdown_truncate(&self) -> CheckpointOutcome {
        let outcome = self.run_pass(CheckpointMode::Truncate);
        if outcome.error.is_none() && !outcome.busy {
            tracing::info!("shutdown checkpoint reclaimed the WAL file");
        } else {
            tracing::warn!(
                busy = outcome.busy,
                error = outcome.error.as_deref().unwrap_or("none"),
                "shutdown checkpoint could not reclaim the WAL file"
            );
        }
        outcome
    }

    /// Steady-state loop: a `PASSIVE` pass plus a reclaim check every 30 s
    /// until `stop` fires. Pass failures are recorded, never fatal.
    pub async fn run_forever(&self, stop: Arc<Notify>) {
        let mut interval = tokio::time::interval(CHECKPOINT_CADENCE);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.notified() => break,
                _ = interval.tick() => {
                    let service = self.clone();
                    let pass = tokio::task::spawn_blocking(move || {
                        service.run_once();
                        service.maybe_truncate(Instant::now());
                    })
                    .await;
                    if pass.is_err() {
                        tracing::error!("checkpoint pass task failed to join");
                    }
                }
            }
        }
    }

    /// True when every pooled connection is back in the pool.
    fn readers_idle(&self) -> bool {
        self.pool.num_idle() as u32 == self.pool.size()
    }

    /// Run one pass in the given mode and record its outcome.
    fn run_pass(&self, mode: CheckpointMode) -> CheckpointOutcome {
        let started = Instant::now();
        let at = SystemTime::now();
        let pragma = match mode {
            CheckpointMode::Passive => "PRAGMA wal_checkpoint(PASSIVE)",
            CheckpointMode::Truncate => "PRAGMA wal_checkpoint(TRUNCATE)",
        };
        let outcome = self.checkpoint_once(pragma, mode, at, started);
        tracing::info!(
            mode = ?mode,
            busy = outcome.busy,
            log_frames = outcome.log_frames,
            checkpointed_frames = outcome.checkpointed_frames,
            active_bytes = outcome.active_bytes,
            wal_file_bytes = outcome.wal_file_bytes,
            duration_ms = outcome.duration.as_millis() as u64,
            error = outcome.error.as_deref().unwrap_or("none"),
            suspended = outcome.suspended,
            "checkpoint pass recorded"
        );
        outcome
    }

    /// One pragma round-trip with v1's error split: lock errors become an
    /// unmeasured busy pass, other errors are recorded and leave
    /// backpressure untouched. Only genuine lock contention reports
    /// `busy`; every error pass reports `busy: false` with its cause.
    ///
    /// The checkpoint connection is the one pragma exception in the
    /// runtime: it opens read-write without create (it must never conjure
    /// a missing database into being) and sets only `busy_timeout=0` for
    /// its fail-fast contract. It deliberately applies no other stage-0
    /// pragma: an observer must not flip journal mode, sync, or FK
    /// enforcement on the live database.
    fn checkpoint_once(
        &self,
        pragma: &str,
        mode: CheckpointMode,
        at: SystemTime,
        started: Instant,
    ) -> CheckpointOutcome {
        let connection = match rusqlite::Connection::open_with_flags(
            &self.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        ) {
            Ok(connection) => connection,
            Err(error) => {
                return self.record(
                    mode,
                    at,
                    started,
                    false,
                    0,
                    0,
                    -1,
                    false,
                    Some(short_error(&error)),
                );
            }
        };
        if connection.busy_timeout(Duration::from_millis(0)).is_err() {
            return self.record(
                mode,
                at,
                started,
                false,
                0,
                0,
                -1,
                false,
                Some("checkpoint connection setup failed".to_owned()),
            );
        }
        let (busy, log_frames, checkpointed_frames, measured) =
            match connection.query_row(pragma, [], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            }) {
                Ok((busy, log, checkpointed)) => (busy != 0, log, checkpointed, true),
                Err(error) => {
                    if super::error::rusqlite_is_busy(&error) {
                        tracing::warn!("WAL checkpoint busy while a lock is held");
                        (true, 0, 0, false)
                    } else {
                        tracing::error!("WAL checkpoint failed with a non-lock error");
                        return self.record(
                            mode,
                            at,
                            started,
                            false,
                            0,
                            0,
                            -1,
                            false,
                            Some(short_error(&error)),
                        );
                    }
                }
            };
        let page_size: i64 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap_or(4096);
        let active_bytes = if measured {
            (log_frames - checkpointed_frames).max(0) * page_size.max(1)
        } else {
            -1
        };
        let wal_file_bytes = wal_allocation(&self.db_path);
        let was_suspended = self.background_suspended();
        let observation = self.policy.lock().map(|mut policy| {
            policy.observe(
                busy,
                log_frames,
                checkpointed_frames,
                active_bytes,
                Instant::now(),
                measured,
            )
        });
        let (suspended, reader_blocked, progress) = match observation {
            Ok(observation) => (
                observation.suspended,
                observation.reader_blocked,
                observation.progress,
            ),
            Err(_) => (was_suspended, Duration::ZERO, false),
        };
        if was_suspended && !suspended {
            self.resume.notify_waiters();
        }
        let outcome = CheckpointOutcome {
            at,
            mode,
            busy,
            log_frames,
            checkpointed_frames,
            active_bytes,
            wal_file_bytes,
            duration: started.elapsed(),
            error: None,
            suspended,
            reader_blocked,
            progress,
        };
        self.store_latest(outcome.clone());
        outcome
    }

    /// Record an error pass without touching backpressure, and keep it.
    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        mode: CheckpointMode,
        at: SystemTime,
        started: Instant,
        busy: bool,
        log_frames: i64,
        checkpointed_frames: i64,
        active_bytes: i64,
        progress: bool,
        error: Option<String>,
    ) -> CheckpointOutcome {
        let outcome = CheckpointOutcome {
            at,
            mode,
            busy,
            log_frames,
            checkpointed_frames,
            active_bytes,
            wal_file_bytes: wal_allocation(&self.db_path),
            duration: started.elapsed(),
            error,
            suspended: self.background_suspended(),
            reader_blocked: Duration::ZERO,
            progress,
        };
        self.store_latest(outcome.clone());
        outcome
    }

    /// Keep the latest outcome for health export. A poisoned lock keeps the
    /// previous record rather than blocking the loop.
    fn store_latest(&self, outcome: CheckpointOutcome) {
        if let Ok(mut guard) = self.latest.try_write() {
            *guard = Some(outcome);
        }
    }
}

/// `-wal` allocation in bytes, or 0 when no WAL file exists.
fn wal_allocation(db_path: &Path) -> u64 {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::metadata(Path::new(&wal))
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// One line of failure without hosts or paths.
fn short_error(error: &rusqlite::Error) -> String {
    let text = match error {
        rusqlite::Error::SqliteFailure(code, _) => format!("sqlite error {code:?}"),
        other => other.to_string(),
    };
    text.chars().take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe(policy: &mut BackpressurePolicy, active: i64, log: i64, ckpt: i64) -> Observation {
        policy.observe(false, log, ckpt, active, Instant::now(), true)
    }

    #[test]
    fn high_water_suspends_and_low_water_or_progress_resumes() {
        let mut policy = BackpressurePolicy::new();
        let first = observe(&mut policy, ACTIVE_HIGH_WATER_BYTES + 1, 100, 0);
        assert!(first.suspended);
        let still = observe(&mut policy, ACTIVE_LOW_WATER_BYTES + 1, 100, 0);
        assert!(still.suspended, "no progress, still above low water");
        let resumed = observe(&mut policy, ACTIVE_LOW_WATER_BYTES, 100, 0);
        assert!(!resumed.suspended);

        let mut policy = BackpressurePolicy::new();
        observe(&mut policy, ACTIVE_HIGH_WATER_BYTES + 1, 100, 0);
        let stuck = observe(&mut policy, ACTIVE_HIGH_WATER_BYTES + 1, 90, 10);
        assert!(stuck.suspended, "above high water, progress is not enough");
        let resumed = observe(&mut policy, ACTIVE_HIGH_WATER_BYTES, 80, 20);
        assert!(!resumed.suspended, "at/below high water, progress resumes");
    }

    #[test]
    fn stall_bound_suspends_without_water_and_clears_on_quiet() {
        let mut policy = BackpressurePolicy::new();
        let start = Instant::now();
        let early = policy.observe(true, 50, 50, 0, start, true);
        assert!(!early.suspended);
        let late = policy.observe(
            true,
            50,
            50,
            0,
            start + CHECKPOINT_READER_BLOCKED_BOUND,
            true,
        );
        assert!(late.suspended);
        assert!(late.reader_blocked >= CHECKPOINT_READER_BLOCKED_BOUND);
        let quiet = policy.observe(
            false,
            50,
            50,
            0,
            start + CHECKPOINT_READER_BLOCKED_BOUND + Duration::from_secs(1),
            true,
        );
        assert!(!quiet.suspended);
        assert_eq!(quiet.reader_blocked, Duration::ZERO);
    }

    #[test]
    fn unmeasured_passes_never_feed_the_baseline() {
        let mut policy = BackpressurePolicy::new();
        observe(&mut policy, 0, 100, 100);
        let unmeasured = policy.observe(true, 0, 0, -1, Instant::now(), false);
        assert!(!unmeasured.progress);
        let next = observe(&mut policy, 0, 100, 100);
        assert!(!next.progress, "fabricated zeros must not read as progress");
    }

    #[test]
    fn suspension_survives_unmeasured_passes() {
        let mut policy = BackpressurePolicy::new();
        observe(&mut policy, ACTIVE_HIGH_WATER_BYTES + 1, 100, 0);
        assert!(policy.suspended());
        let pass = policy.observe(true, 0, 0, -1, Instant::now(), false);
        assert!(pass.suspended, "active -1 is evidence-free, not drained");
    }
}
