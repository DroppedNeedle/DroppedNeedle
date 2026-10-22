//! WAL checkpoint policy: GH-293 calibration with TRUNCATE reclaim.
//!
//! Steady state is a `PASSIVE` checkpoint every 30 s from a dedicated
//! connection that fails fast (`busy_timeout=0`): lock contention becomes a
//! `busy` pass that keeps its progress baseline (as in v2), while
//! non-lock errors are recorded distinctly and leave backpressure alone.
//! Backpressure holds bulk writes only, never a request's write or a job's
//! state write, when active WAL crosses 64 MiB or a checkpoint makes no
//! measurable progress for 60 s. The service sets the lane's
//! [`WalHold`] after every pass that did not fail. While a bulk write waits
//! behind it, the lane's relief thread runs
//! [`CheckpointService::run_relief`] once a second, so the hold ends a
//! second and one pass, at most, after the WAL can shrink and does not wait
//! for the 30 s loop.
//!
//! v2 never reclaimed: `PASSIVE` leaves checkpointed frames allocated, so a
//! dead `-wal` of several MB survived graceful restarts. v3 adds `TRUNCATE`
//! reclaim when three gates hold: the last `PASSIVE` reported zero active
//! frames, the writer lane is idle and every reader connection is back in
//! its pool, and an hour passed since the last reclaim. A final `TRUNCATE`
//! runs at clean shutdown, so a stopped database leaves no `-wal` behind.
//!
//! Every pass records its outcome for the log and keeps the latest one for
//! the admin health endpoint (which reads [`CheckpointService::latest`]).

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::clock::Clock;

use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use super::{
    read_pool::ReadActivity,
    writer::{LaneIdle, WalHold},
};

/// `PASSIVE` checkpoint cadence, as calibrated in GH-293.
pub const CHECKPOINT_CADENCE: Duration = Duration::from_secs(30);
/// Stall bound: no measurable `PASSIVE` progress for this long holds bulk
/// writes, as calibrated in GH-293.
pub const CHECKPOINT_READER_BLOCKED_BOUND: Duration = Duration::from_secs(60);
/// Active-WAL high water: above this, bulk writes are held.
pub const ACTIVE_HIGH_WATER_BYTES: i64 = 64 * 1024 * 1024;
/// Active-WAL low water: at or below this, held bulk writes run again.
pub const ACTIVE_LOW_WATER_BYTES: i64 = 16 * 1024 * 1024;
/// Minimum gap between live `TRUNCATE` reclaims. Shutdown reclaim ignores it.
pub const TRUNCATE_MIN_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Which checkpoint mode produced an outcome record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointMode {
    /// Steady-state bounded pass.
    Passive,
    /// Allocation reclaim under the three gates, or at shutdown.
    Truncate,
}

/// One recorded checkpoint pass. `active_bytes` counts uncheckpointed frames
/// times the page size; `wal_file_bytes` is the `-wal` allocation on disk,
/// which may hold dead frames the next `TRUNCATE` reclaims. A `busy` pass
/// with `active_bytes == -1` lost a lock, carried no frame evidence and
/// never moves the progress baseline or the hold.
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

/// Pure backpressure machine: the v2 `_update_state` rule. Suspension needs
/// active WAL over the high water or a stall past the reader bound; resume
/// needs active WAL at/below the low water or any measurable progress.
/// Unmeasured passes never feed the baseline and never clear suspension.
#[derive(Debug)]
pub struct BackpressurePolicy {
    suspended: bool,
    busy_since: Option<Instant>,
    last_log_frames: Option<i64>,
    last_checkpointed_frames: Option<i64>,
    high_water: i64,
    low_water: i64,
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
        Self::with_water(ACTIVE_HIGH_WATER_BYTES, ACTIVE_LOW_WATER_BYTES)
    }

    /// A fresh machine with other water marks. Production runs on the
    /// constants; a test lowers them to fill the WAL with kilobytes.
    pub(crate) fn with_water(high_water: i64, low_water: i64) -> Self {
        Self {
            suspended: false,
            busy_since: None,
            last_log_frames: None,
            last_checkpointed_frames: None,
            high_water,
            low_water,
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
        if active_bytes > self.high_water
            || (self.busy_since.is_some() && reader_blocked >= CHECKPOINT_READER_BLOCKED_BOUND)
        {
            if !self.suspended {
                tracing::warn!(
                    active_bytes,
                    busy,
                    reader_blocked_secs = reader_blocked.as_secs_f64(),
                    "WAL backpressure: holding bulk writes"
                );
            }
            self.suspended = true;
        } else if self.suspended && ((0..=self.low_water).contains(&active_bytes) || progress) {
            tracing::info!(
                active_bytes,
                "WAL backpressure cleared: bulk writes may run"
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
#[derive(Clone)]
pub struct CheckpointService {
    db_path: PathBuf,
    pool: SqlitePool,
    clock: Arc<dyn Clock>,
    lane_idle: Arc<LaneIdle>,
    policy: Arc<Mutex<BackpressurePolicy>>,
    latest: Arc<RwLock<Option<CheckpointOutcome>>>,
    last_truncate: Arc<Mutex<Option<Instant>>>,
    hold: Arc<WalHold>,
    /// The read pools' counts. Never the pools themselves or the handle
    /// that owns them: this service is cloned into the relief thread.
    pooled_reads: Option<ReadActivity>,
    analyze: Option<super::analyze::AnalyzeService>,
}

impl std::fmt::Debug for CheckpointService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointService")
            .field("db_path", &self.db_path)
            .finish_non_exhaustive()
    }
}

impl CheckpointService {
    /// Wire the service. The pool is read for reader-idle checks only; the
    /// lane handle feeds the writer-idle half of the `TRUNCATE` gate, and
    /// `hold` is the lane's hold on bulk writes, which every pass sets.
    /// `clock` dates each pass.
    pub fn new(
        db_path: &Path,
        pool: SqlitePool,
        lane_idle: Arc<LaneIdle>,
        hold: Arc<WalHold>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            db_path: db_path.to_owned(),
            pool,
            clock,
            lane_idle,
            policy: Arc::new(Mutex::new(BackpressurePolicy::new())),
            latest: Arc::new(RwLock::new(None)),
            last_truncate: Arc::new(Mutex::new(None)),
            hold,
            pooled_reads: None,
            analyze: None,
        }
    }

    /// Count the read pools in the reader-idle half of the `TRUNCATE` gate.
    #[must_use]
    pub(crate) fn with_pooled_reads(mut self, reads: ReadActivity) -> Self {
        self.pooled_reads = Some(reads);
        self
    }

    /// Other water marks for the backpressure rule.
    #[must_use]
    pub(crate) fn with_water(mut self, high_water: i64, low_water: i64) -> Self {
        self.policy = Arc::new(Mutex::new(BackpressurePolicy::with_water(
            high_water, low_water,
        )));
        self
    }

    /// Refresh planner statistics from the checkpoint loop (see
    /// [`super::analyze`]).
    #[must_use]
    pub fn with_analyze(mut self, analyze: super::analyze::AnalyzeService) -> Self {
        self.analyze = Some(analyze);
        self
    }

    /// The statistics refresher the checkpoint loop drives, when wired.
    pub fn analyze(&self) -> Option<&super::analyze::AnalyzeService> {
        self.analyze.as_ref()
    }

    /// True while bulk writes are held. Nothing else ever is.
    pub fn bulk_held(&self) -> bool {
        self.hold.is_on()
    }

    /// Latest recorded pass, for the admin health endpoint.
    pub async fn latest(&self) -> Option<CheckpointOutcome> {
        self.latest.read().await.clone()
    }

    /// Latest recorded pass without awaiting, for non-async callers.
    pub fn try_latest(&self) -> Option<CheckpointOutcome> {
        self.latest.try_read().ok().and_then(|guard| guard.clone())
    }

    /// One bounded `PASSIVE` pass on a dedicated fail-fast connection.
    /// Blocking; callers offload it with `spawn_blocking`.
    pub fn run_once(&self) -> CheckpointOutcome {
        self.run_pass(CheckpointMode::Passive, false)
    }

    /// [`CheckpointService::run_once`] for the lane's relief thread, which
    /// calls it once a second while a bulk write is held. Only the pass that
    /// ends the hold is logged at info; the rest would be a line a second
    /// for as long as a reader pins the WAL. A pass that fails says so in
    /// its outcome, and the lane puts that in its line about held writes.
    pub fn run_relief(&self) -> CheckpointOutcome {
        self.run_pass(CheckpointMode::Passive, true)
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
        let outcome = self.run_pass(CheckpointMode::Truncate, false);
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
        let outcome = self.run_pass(CheckpointMode::Truncate, false);
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

    /// True when every pooled connection is back in its pool: the sqlx
    /// pool's and the read pools'.
    fn readers_idle(&self) -> bool {
        self.pool.num_idle() as u32 == self.pool.size()
            && self.pooled_reads.as_ref().is_none_or(ReadActivity::is_idle)
    }

    /// Run one pass in the given mode and record its outcome. A relief pass
    /// that leaves the hold on is logged at debug.
    fn run_pass(&self, mode: CheckpointMode, relief: bool) -> CheckpointOutcome {
        let started = Instant::now();
        let at =
            UNIX_EPOCH + Duration::from_millis(u64::try_from(self.clock.now_ms()).unwrap_or(0));
        let pragma = match mode {
            CheckpointMode::Passive => "PRAGMA wal_checkpoint(PASSIVE)",
            CheckpointMode::Truncate => "PRAGMA wal_checkpoint(TRUNCATE)",
        };
        let outcome = self.checkpoint_once(pragma, mode, at, started, relief);
        macro_rules! recorded {
            ($level:ident) => {
                tracing::$level!(
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
                )
            };
        }
        if relief && outcome.suspended {
            recorded!(debug);
        } else {
            recorded!(info);
        }
        outcome
    }

    /// One pragma round-trip with v2's error split: lock errors become an
    /// unmeasured busy pass, other errors are recorded and leave
    /// backpressure untouched. Only genuine lock contention reports
    /// `busy`; every error pass reports `busy: false` with its cause.
    ///
    /// The checkpoint connection is the one pragma exception in the
    /// runtime: it opens read-write without create (it must never conjure
    /// a missing database into being) and sets only `busy_timeout=0` for
    /// its fail-fast contract. It applies no other connection pragma on
    /// purpose: an observer must not flip journal mode, sync, or FK
    /// enforcement on the live database.
    fn checkpoint_once(
        &self,
        pragma: &str,
        mode: CheckpointMode,
        at: SystemTime,
        started: Instant,
        relief: bool,
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
                // A pass that cannot take the checkpoint lock, because the
                // writer's auto-checkpoint or another pass has it, is not an
                // error: SQLite answers with the row (1, -1, -1). Verified
                // against SQLite 3.46.0. It measured nothing, so it must not
                // reach the policy as a WAL with no frames in it.
                Ok((busy, log, checkpointed)) if busy != 0 && (log < 0 || checkpointed < 0) => {
                    log_lost_lock(relief);
                    (true, 0, 0, false)
                }
                Ok((busy, log, checkpointed)) => (busy != 0, log, checkpointed, true),
                Err(error) => {
                    if super::error::rusqlite_is_busy(&error) {
                        log_lost_lock(relief);
                        (true, 0, 0, false)
                    } else {
                        // A relief pass comes round every second; the lane
                        // reports its error with the writes it left held.
                        if relief {
                            tracing::debug!("WAL checkpoint failed with a non-lock error");
                        } else {
                            tracing::error!("WAL checkpoint failed with a non-lock error");
                        }
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
        let observation = {
            // A poisoned lock is taken all the same: a pass that could no
            // longer set the hold would leave it as it was until shutdown.
            let mut policy = self.policy.lock().unwrap_or_else(PoisonError::into_inner);
            let observation = policy.observe(
                busy,
                log_frames,
                checkpointed_frames,
                active_bytes,
                Instant::now(),
                measured,
            );
            // Set under the policy's lock, so the flag is always what the
            // policy last decided.
            self.hold.set(observation.suspended);
            observation
        };
        let (suspended, reader_blocked, progress) = (
            observation.suspended,
            observation.reader_blocked,
            observation.progress,
        );
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
            suspended: self.bulk_held(),
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

/// A pass that lost a lock is worth a warning from the 30 s loop. From the
/// relief thread, which tries again within a second, it is not.
fn log_lost_lock(relief: bool) {
    if relief {
        tracing::debug!("WAL checkpoint busy while a lock is held");
    } else {
        tracing::warn!("WAL checkpoint busy while a lock is held");
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
