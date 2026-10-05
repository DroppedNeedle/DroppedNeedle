//! Registered ephemeral loops: wanted watcher, follow poll, upgrade sweep,
//! request-status-sync.
//!
//! Each loop is a small cadence contract: a `tick` function that runs one
//! pass when due (pure over an explicit `now`, so tests drive it with a
//! [`ManualClock`](super::seams::ManualClock) and never sleep), plus a
//! `spawn_*` constructor (single-flight registry, log-and-continue passes,
//! shutdown through the sleeper) that registers the loop as
//! [`JobKind::Ephemeral`](crate::db::JobKind) and returns a handle the
//! caller awaits at shutdown.
//!
//! Cadences match v2's task schedule; each constant cites its source.
//! Loops that v2 jitters (wanted, follow) take a [`Jitter`]; loops v2
//! sleeps plainly (sync, upgrade) sleep the exact interval.

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::db::{DurableWorkWakeups, JobKind, JobState, WriteLane};

use super::seams::{
    CandidateSearch, Clock, DispatchKind, DispatchRequest, DownloadDispatch, ReleasePoll, TickSink,
};
use super::stores::{
    AdminDirectory, FollowCursor, FollowStore, LibraryPresence, PendingRelease, UpgradePolicy,
    UpgradeWorklist,
};
use crate::acquire::requests::error::RequestsError;
use crate::acquire::requests::ledger::{RequestRecord, WATCH_WATCHING, WantedWatch};
use crate::acquire::requests::models::RequestKind;
use crate::acquire::requests::sqlite::{RequestStore, WantedStore};

/// Wanted-watcher sweep cadence in seconds (v2 `_WANTED_WATCHER_INTERVAL`).
pub const WANTED_INTERVAL: Duration = Duration::from_secs(900);
/// Wanted-watcher startup delay in seconds (v2 `_WANTED_WATCHER_INITIAL_DELAY`).
pub const WANTED_INITIAL_DELAY: Duration = Duration::from_secs(240);
/// Follow new-release poll cadence in seconds (v2 `_FOLLOW_POLL_INTERVAL`).
pub const FOLLOW_INTERVAL: Duration = Duration::from_secs(60);
/// Follow poll startup delay in seconds (v2 `_FOLLOW_POLL_INITIAL_DELAY`).
pub const FOLLOW_INITIAL_DELAY: Duration = Duration::from_secs(300);
/// Follow poll due-driven cap: provider work happens only for due artists,
/// at most this many per tick (v2 `_run_due_poll`, `attempted < 10`).
pub const FOLLOW_MAX_ARTISTS_PER_TICK: usize = 10;
/// Upgrade-scan startup delay in seconds (v2 `scan_for_upgrades_periodically`).
pub const UPGRADE_INITIAL_DELAY: Duration = Duration::from_secs(900);
/// Upgrade-scan default cadence in hours (v2 `interval_hours = 12`).
pub const UPGRADE_DEFAULT_INTERVAL_HOURS: u64 = 12;
/// Request-status-sync cadence in seconds (v2 `_REQUEST_SYNC_INTERVAL`).
pub const SYNC_INTERVAL: Duration = Duration::from_secs(60);
/// Status-sync startup delay in seconds (v2 `_REQUEST_SYNC_INITIAL_DELAY`).
pub const SYNC_INITIAL_DELAY: Duration = Duration::from_secs(15);

/// Registry name for the wanted-watcher loop (v2 `TaskRegistry` key).
pub const WANTED_JOB: &str = "wanted-watcher";
/// Registry name for the follow new-release poll (v2 `TaskRegistry` key).
pub const FOLLOW_JOB: &str = "follow-new-release-poll";
/// Registry name for the background upgrade scan (v2 `TaskRegistry` key).
pub const UPGRADE_JOB: &str = "background-upgrade-scan";
/// Registry name for the request-status sync (v2 `TaskRegistry` key).
pub const SYNC_JOB: &str = "request-status-sync";

/// Sleep seam so loops test without real waits.
pub trait Sleeper: Send + Sync {
    /// Sleep `duration`, returning false when shutdown won the race.
    fn sleep(&self, duration: Duration) -> impl Future<Output = bool> + Send;
}

/// Production sleeper over tokio time and a shutdown watch.
#[derive(Debug, Clone)]
pub struct TokioSleeper {
    shutdown: tokio::sync::watch::Receiver<bool>,
}

impl TokioSleeper {
    /// Build a sleeper that wakes when `shutdown` flips true.
    pub fn new(shutdown: tokio::sync::watch::Receiver<bool>) -> Self {
        Self { shutdown }
    }
}

impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) -> bool {
        let mut shutdown = self.shutdown.clone();
        tokio::select! {
            () = tokio::time::sleep(duration) => !*shutdown.borrow(),
            _ = shutdown.changed() => false,
        }
    }
}

/// Manual sleeper for tests: `wake` releases one waiter, `shut_down`
/// releases all with false. Requested durations are recorded so tests can
/// assert the intervals.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub struct ManualSleeper {
    inner: Arc<ManualSleeperInner>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
struct ManualSleeperInner {
    state: Mutex<ManualSleepState>,
    wake: tokio::sync::Notify,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
struct ManualSleepState {
    waits: usize,
    shutdown: bool,
    requested: Vec<Duration>,
}

#[cfg(any(test, feature = "test-support"))]
impl ManualSleeper {
    /// Fresh manual sleeper.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ManualSleeperInner {
                state: Mutex::new(ManualSleepState::default()),
                wake: tokio::sync::Notify::new(),
            }),
        }
    }

    /// Release one waiter to continue the loop.
    pub fn wake(&self) {
        self.inner.wake.notify_one();
    }

    /// Shut the loop down: waiters return false.
    pub fn shut_down(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.shutdown = true;
        }
        self.inner.wake.notify_waiters();
    }

    /// Durations the loop requested, in order.
    pub fn requested(&self) -> Vec<Duration> {
        self.inner
            .state
            .lock()
            .map(|state| state.requested.clone())
            .unwrap_or_default()
    }

    /// Waiters currently parked in `sleep`.
    pub fn waits(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|state| state.waits)
            .unwrap_or(0)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Default for ManualSleeper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Sleeper for ManualSleeper {
    async fn sleep(&self, duration: Duration) -> bool {
        if let Ok(mut state) = self.inner.state.lock() {
            if state.shutdown {
                return false;
            }
            state.requested.push(duration);
            state.waits += 1;
        }
        self.inner.wake.notified().await;
        if let Ok(mut state) = self.inner.state.lock() {
            state.waits = state.waits.saturating_sub(1);
            if state.shutdown {
                return false;
            }
        }
        true
    }
}

/// Jitter seam. v2 desynchronizes same-second ticks across instances with
/// mean-preserving ±20% uniform jitter (`_jittered_sleep`); tests pin
/// [`NoJitter`] so requested durations assert exactly.
pub trait Jitter: Send + Sync {
    /// Multiplicative factor for one sleep (0.8..=1.2 in production).
    fn factor(&self) -> f64;
}

/// No jitter: every sleep runs its exact interval. For tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoJitter;

#[cfg(any(test, feature = "test-support"))]
impl Jitter for NoJitter {
    fn factor(&self) -> f64 {
        1.0
    }
}

/// Production jitter: uniform over ±20%, drawn from a nanos seed.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreadJitter;

impl Jitter for ThreadJitter {
    fn factor(&self) -> f64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|span| span.subsec_nanos())
            .unwrap_or(0);
        0.8 + f64::from(nanos % 401) / 1000.0
    }
}

/// Apply one jitter factor to a base interval.
pub fn jittered(base: Duration, jitter: &dyn Jitter) -> Duration {
    let scaled = base.as_secs_f64() * jitter.factor();
    Duration::from_secs_f64(scaled.max(1.0))
}

/// Single-flight registry: one live pass per loop key. `begin` returns
/// false when the loop already runs; holders must call `finish`.
#[derive(Debug, Default)]
pub struct FlowRegistry {
    live: Mutex<HashSet<String>>,
}

impl FlowRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the loop. False means another pass holds it.
    pub fn begin(&self, job: &str) -> bool {
        self.live
            .lock()
            .map(|mut live| live.insert(job.to_owned()))
            .unwrap_or(false)
    }

    /// Release the loop.
    pub fn finish(&self, job: &str) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(job);
        }
    }

    /// True while the loop runs.
    pub fn is_live(&self, job: &str) -> bool {
        self.live
            .lock()
            .map(|live| live.contains(job))
            .unwrap_or(false)
    }
}

/// Per-loop cadence cursor. The first tick always runs; later ticks run
/// once the interval has elapsed since the last run.
#[derive(Debug, Default, Clone)]
pub struct LoopState {
    /// Unix seconds of the last run, if any.
    pub last_run: Option<i64>,
    /// Runs so far.
    pub runs: u64,
}

impl LoopState {
    /// Fresh cursor.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when a tick at `now` should run given `interval` seconds.
    pub fn due(&self, now: i64, interval_secs: i64) -> bool {
        self.last_run.is_none_or(|last| now - last >= interval_secs)
    }

    /// Record a run at `now`.
    pub fn ran(&mut self, now: i64) {
        self.last_run = Some(now);
        self.runs += 1;
    }
}

/// Wanted-watcher settings, re-read every sweep so flipping the toggle
/// needs no restart (v2 `run_sweep`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedSettings {
    /// The watcher runs at all.
    pub enabled: bool,
    /// Also enrol `incomplete` (partial-album) rows, not just `failed`.
    pub watch_partial_albums: bool,
    /// Due watches checked per sweep, at most.
    pub max_checks_per_sweep: usize,
    /// Dispatch on find. Off means badge-only: candidates tick
    /// `wanted.found` and wait for a hand dispatch instead.
    pub auto_download_on_find: bool,
}

impl Default for WantedSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            watch_partial_albums: false,
            max_checks_per_sweep: 25,
            auto_download_on_find: true,
        }
    }
}

/// What one wanted sweep accomplished (v2 `WantedSweepSummary`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WantedSummary {
    /// Requests newly enrolled as watches.
    pub enrolled: usize,
    /// Due watches checked.
    pub checked: usize,
    /// Watches that auto-dispatched a download.
    pub dispatched: usize,
    /// Watches satisfied from the library.
    pub fulfilled: usize,
    /// Watches that errored (each isolated, the sweep continued).
    pub errors: usize,
}

/// Wanted-watcher dependencies.
pub struct WantedDeps {
    /// Settings source, read fresh every sweep.
    pub settings: Arc<dyn Fn() -> WantedSettings + Send + Sync>,
    /// Watch registry (shared with the wanted view).
    pub watches: WantedStore,
    /// Request ledger for enrolment.
    pub ledger: RequestStore,
    /// Candidate search.
    pub search: Arc<dyn CandidateSearch>,
    /// Download dispatch.
    pub downloads: Arc<dyn DownloadDispatch>,
    /// Library presence for satisfaction checks.
    pub library: Arc<LibraryPresence>,
    /// Durable ticks.
    pub ticks: Arc<dyn TickSink>,
}

/// Epoch seconds as the watch store keeps them.
fn epoch(now: i64) -> u64 {
    u64::try_from(now).unwrap_or(0)
}

/// Run one wanted sweep when due: enrol availability-dead requests, then
/// check due watches. Disabled reads as an empty pass. One bad watch never
/// kills the sweep (v2 `run_sweep` isolation).
pub async fn wanted_tick(now: i64, state: &mut LoopState, deps: &WantedDeps) -> WantedSummary {
    if !state.due(now, WANTED_INTERVAL.as_secs() as i64) {
        return WantedSummary::default();
    }
    state.ran(now);
    let settings = (deps.settings)();
    if !settings.enabled {
        return WantedSummary::default();
    }
    let mut summary = WantedSummary {
        enrolled: enrol_watches(now, &settings, deps).await,
        ..WantedSummary::default()
    };
    let due = match deps
        .watches
        .list_due(epoch(now), settings.max_checks_per_sweep)
        .await
    {
        Ok(due) => due,
        Err(error) => {
            tracing::warn!(?error, "wanted sweep could not list due watches");
            summary.errors += 1;
            return summary;
        }
    };
    for watch in due {
        summary.checked += 1;
        if let Err(error) = check_watch(now, &settings, deps, &watch, &mut summary).await {
            tracing::warn!(key = %watch.key, ?error, "wanted check failed");
            summary.errors += 1;
        }
    }
    summary
}

/// Check one due watch: satisfied from the library, dispatched on a find,
/// badged on a find when auto-download is off, or rescheduled quietly.
async fn check_watch(
    now: i64,
    settings: &WantedSettings,
    deps: &WantedDeps,
    watch: &WantedWatch,
    summary: &mut WantedSummary,
) -> Result<(), RequestsError> {
    let at = epoch(now);
    if deps.library.contains(&watch.key) {
        deps.watches
            .mark_fulfilled(&watch.key, "in_library", at)
            .await?;
        deps.ledger
            .update_status(RequestKind::Album, &watch.key, "imported", Some(at), None)
            .await?;
        deps.ticks.emit(
            "request_fulfilled",
            &format!("wanted {} satisfied from the library", watch.key),
            now,
        );
        summary.fulfilled += 1;
        return Ok(());
    }
    let next = |streak: u32| {
        at + u64::try_from(interval_seconds(
            watch.first_release_date.as_deref(),
            streak,
            now,
        ))
        .unwrap_or(0)
    };
    match deps
        .search
        .search_album(&watch.artist_name, &watch.album_title)
        .await
    {
        Ok(candidates) if !candidates.is_empty() => {
            if !settings.auto_download_on_find {
                deps.watches
                    .record_check(
                        &watch.key,
                        watch.quiet_streak,
                        next(watch.quiet_streak),
                        at,
                        "found",
                    )
                    .await?;
                deps.ticks.emit(
                    "wanted.found",
                    &format!(
                        "wanted {} has {} candidate(s) (badge-only)",
                        watch.key,
                        candidates.len()
                    ),
                    now,
                );
                return Ok(());
            }
            let dispatch = DispatchRequest {
                user_id: watch.user_id.clone(),
                kind: DispatchKind::Album,
                mbid: watch.key.clone(),
                artist: watch.artist_name.clone(),
                title: watch.album_title.clone(),
                origin: "wanted".to_owned(),
                // One dispatch per check slot: a crash between the dispatch
                // and the reschedule answers the same task next time.
                idempotency_key: Some(format!("wanted:{}:{}", watch.key, watch.check_count)),
            };
            match deps.downloads.dispatch(&dispatch).await {
                Ok(task_id) => {
                    deps.ledger
                        .link_task(RequestKind::Album, &watch.key, &task_id, None)
                        .await?;
                    deps.watches
                        .record_check(&watch.key, 0, next(0), at, "dispatched")
                        .await?;
                    deps.ticks.emit(
                        "wanted.dispatched",
                        &format!("wanted {} auto-dispatched as {task_id}", watch.key),
                        now,
                    );
                    summary.dispatched += 1;
                }
                Err(cause) => {
                    tracing::warn!(key = %watch.key, %cause, "wanted dispatch failed");
                    deps.watches
                        .record_check(&watch.key, 0, next(0), at, "error")
                        .await?;
                    summary.errors += 1;
                }
            }
        }
        Ok(_) => {
            let streak = watch.quiet_streak.saturating_add(1);
            deps.watches
                .record_check(&watch.key, streak, next(streak), at, "quiet")
                .await?;
        }
        Err(cause) => {
            // A per-want failure reschedules normally with a reset streak:
            // one bad want never kills the sweep (v2 `_record_error_cycle`).
            tracing::warn!(key = %watch.key, %cause, "wanted search failed");
            deps.watches
                .record_check(&watch.key, 0, next(0), at, "error")
                .await?;
            summary.errors += 1;
        }
    }
    Ok(())
}

/// Enrol availability-dead album requests as watches: `failed` rows, plus
/// `incomplete` rows when the partial toggle is on. Never auto-revives a
/// stopped watch, skips rows with no requester, and skips rows whose
/// linked task is still active (v2 `_maybe_enrol` guards).
async fn enrol_watches(now: i64, settings: &WantedSettings, deps: &WantedDeps) -> usize {
    let mut statuses = vec![("failed", "missing")];
    if settings.watch_partial_albums {
        statuses.push(("incomplete", "partial"));
    }
    let mut enrolled = 0;
    for (status, kind) in statuses {
        let rows = match deps.ledger.with_status(RequestKind::Album, status).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(status, ?error, "wanted enrolment could not read requests");
                continue;
            }
        };
        for row in rows {
            let Some(user_id) = row.user_id.clone().filter(|user| !user.is_empty()) else {
                continue;
            };
            if deps.library.contains(&row.key) {
                continue;
            }
            if let Some(task_id) = row.task_id.as_deref() {
                match deps.downloads.task_status(task_id).await {
                    Ok(Some(status)) if matches!(status.as_str(), "downloading" | "processing") => {
                        continue;
                    }
                    Ok(_) => {}
                    Err(cause) => {
                        tracing::warn!(task_id, %cause, "wanted enrolment task read failed");
                        continue;
                    }
                }
            }
            let watch = WantedWatch {
                key: row.key.clone(),
                user_id,
                user_name: None,
                artist_name: row.artist_name.clone(),
                album_title: row.album_title.clone(),
                artist_mbid: row.artist_mbid.clone(),
                year: row.year,
                cover_url: None,
                kind: kind.to_owned(),
                state: WATCH_WATCHING.to_owned(),
                created_at: epoch(now),
                first_release_date: None,
                check_count: 0,
                quiet_streak: 0,
                next_check_at: epoch(now + interval_seconds(None, 0, now)),
                new_candidate_count: 0,
            };
            match deps.watches.enrol(watch).await {
                Ok(true) => {
                    deps.ticks.emit(
                        "wanted.enrolled",
                        &format!("wanted {} enrolled from {status} request", row.key),
                        now,
                    );
                    enrolled += 1;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(key = %row.key, ?error, "wanted enrolment write failed");
                }
            }
        }
    }
    enrolled
}

/// Quiet streak at which old releases back off from 14 to 28 days (v2
/// `_QUIET_DOUBLING_STREAK = 10`).
pub const QUIET_DOUBLING_STREAK: u32 = 10;

/// Age-based recheck cadence in seconds (v2 `_interval_days`):
/// under 30 days old → 2 days; under 90 → 4; under a year → 7; older or
/// unknown → 14, or 28 past the quiet-doubling streak. Unknown release
/// date reads as old. Day granularity (the table bands are days wide).
pub fn interval_seconds(first_release_date: Option<&str>, quiet_streak: u32, now: i64) -> i64 {
    const DAY: i64 = 86_400;
    let age_days = first_release_date
        .and_then(parse_partial_date)
        .map(|(year, month, day)| now.div_euclid(DAY) - days_from_civil(year, month, day))
        .map(|age| age.max(0));
    let days: i64 = match age_days {
        Some(age) if age < 30 => 2,
        Some(age) if age < 90 => 4,
        Some(age) if age < 365 => 7,
        _ if quiet_streak >= QUIET_DOUBLING_STREAK => 28,
        _ => 14,
    };
    days * DAY
}

/// Parse a partial MusicBrainz date (`YYYY`, `YYYY-MM`, or `YYYY-MM-DD`)
/// to the start of its period (v2 `_parse_partial_date`).
fn parse_partial_date(value: &str) -> Option<(i64, u32, u32)> {
    let mut parts = value.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = match parts.next() {
        None => 1,
        Some(part) => part.parse::<u32>().ok()?,
    };
    let day: u32 = match parts.next() {
        None => 1,
        Some(part) => part.parse::<u32>().ok()?,
    };
    if parts.next().is_some() || month == 0 || month > 12 || day == 0 || day > 31 {
        return None;
    }
    Some((year, month, day))
}

/// Days from the unix epoch to a civil date (Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400) as u64;
    let mp = ((i64::from(month) + 9) % 12) as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// What one follow poll accomplished (v2 `PollSummary`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FollowSummary {
    /// Artists polled this tick.
    pub artists_polled: usize,
    /// Artists baselined (first poll records, never emits).
    pub baselined: usize,
    /// New releases observed.
    pub new_releases: usize,
    /// Releases enqueued for followers.
    pub enqueued: usize,
    /// Artists that errored (each isolated).
    pub errors: usize,
}

/// Follow-poll dependencies.
pub struct FollowDeps {
    /// Cursor store.
    pub follows: FollowStore,
    /// Release-page provider seam.
    pub poll: Arc<dyn ReleasePoll>,
    /// Download dispatch.
    pub downloads: Arc<dyn DownloadDispatch>,
    /// Durable ticks.
    pub ticks: Arc<dyn TickSink>,
    /// Accepted primary types (`Album`, `Single`, `EP`, ...); empty takes all.
    pub include_types: Vec<String>,
    /// Today provider, so tests pin the cursor date.
    pub today: Arc<dyn Fn() -> String + Send + Sync>,
}

/// Run one follow poll when due: poll at most
/// [`FOLLOW_MAX_ARTISTS_PER_TICK`] due artists. A first poll (or any poll
/// with no cursor yet) records every observed release group as a
/// no-feed/no-task baseline; normal polls only emit complete, valid dates
/// on or after the prior cursor, holding future matches dispatch-pending
/// until their date (v2 `NewReleaseService` module contract).
pub async fn follow_tick(now: i64, state: &mut LoopState, deps: &FollowDeps) -> FollowSummary {
    if !state.due(now, FOLLOW_INTERVAL.as_secs() as i64) {
        return FollowSummary::default();
    }
    state.ran(now);
    let mut summary = FollowSummary::default();
    let today = (deps.today)();
    let due = match deps
        .follows
        .list_due(now, FOLLOW_MAX_ARTISTS_PER_TICK)
        .await
    {
        Ok(due) => due,
        Err(error) => {
            tracing::warn!(?error, "follow poll could not list due artists");
            summary.errors += 1;
            return summary;
        }
    };
    for mut cursor in due {
        summary.artists_polled += 1;
        match deps.poll.poll_releases(&cursor.artist_mbid).await {
            Ok(releases) => {
                if !cursor.baselined {
                    cursor.known = releases
                        .iter()
                        .map(|row| row.rg_mbid.to_lowercase())
                        .collect();
                    cursor.baselined = true;
                    cursor.cursor_date = Some(today.clone());
                    summary.baselined += 1;
                } else {
                    let seen = follow_new_releases(&cursor, &releases, &today, &deps.include_types);
                    summary.new_releases += seen.emitted.len() + seen.pending.len();
                    for release in &seen.emitted {
                        summary.enqueued += enqueue_for_followers(
                            deps,
                            &cursor,
                            &release.rg_mbid,
                            &release.title,
                            now,
                        )
                        .await;
                        cursor.known.insert(release.rg_mbid.to_lowercase());
                    }
                    for held in seen.pending {
                        if !cursor.pending.iter().any(|row| row.rg_mbid == held.rg_mbid) {
                            cursor.pending.push(held);
                        }
                    }
                    let (ready, waiting): (Vec<PendingRelease>, Vec<PendingRelease>) =
                        cursor.pending.drain(..).partition(|row| row.date <= today);
                    cursor.pending = waiting;
                    for release in ready {
                        summary.enqueued += enqueue_for_followers(
                            deps,
                            &cursor,
                            &release.rg_mbid,
                            &release.title,
                            now,
                        )
                        .await;
                        cursor.known.insert(release.rg_mbid.to_lowercase());
                        summary.new_releases += 1;
                    }
                    cursor.cursor_date = Some(today.clone());
                }
            }
            Err(cause) => {
                tracing::warn!(artist = %cursor.artist_mbid, %cause, "follow poll failed");
                summary.errors += 1;
            }
        }
        cursor.next_poll_at = now + FOLLOW_INTERVAL.as_secs() as i64;
        if let Err(error) = deps.follows.record_poll(&cursor).await {
            tracing::warn!(artist = %cursor.artist_mbid, ?error, "follow cursor write failed");
            summary.errors += 1;
        }
    }
    summary
}

/// Dispatch one new release for every approved follower; answers how many
/// dispatched. The key pins one task per (follower, release).
async fn enqueue_for_followers(
    deps: &FollowDeps,
    cursor: &FollowCursor,
    rg_mbid: &str,
    title: &str,
    now: i64,
) -> usize {
    let mut enqueued = 0;
    for follower in &cursor.followers {
        let dispatch = DispatchRequest {
            user_id: follower.clone(),
            kind: DispatchKind::Album,
            mbid: rg_mbid.to_owned(),
            artist: String::new(),
            title: title.to_owned(),
            origin: "follow".to_owned(),
            idempotency_key: Some(format!("follow:{follower}:{}", rg_mbid.to_lowercase())),
        };
        match deps.downloads.dispatch(&dispatch).await {
            Ok(_) => {
                enqueued += 1;
                deps.ticks.emit(
                    "follow.enqueued",
                    &format!("follow {rg_mbid} enqueued for {follower}"),
                    now,
                );
            }
            Err(cause) => {
                tracing::warn!(rg_mbid, follower, %cause, "follow dispatch failed");
            }
        }
    }
    enqueued
}

/// Releases split into those emitting now and those held pending.
struct FollowSeen {
    /// Complete dates on/after the cursor and not before today: emit.
    emitted: Vec<super::seams::ObservedRelease>,
    /// Complete future dates: hold dispatch-pending.
    pending: Vec<PendingRelease>,
}

/// Observed page rows worth acting on: complete `YYYY-MM-DD` dates on or
/// after the prior cursor, unknown to the inventory, within the accepted
/// types. Malformed rows never emit (v2 `_observe_artist` guards).
fn follow_new_releases(
    cursor: &FollowCursor,
    releases: &[super::seams::ObservedRelease],
    today: &str,
    include_types: &[String],
) -> FollowSeen {
    let mut seen = FollowSeen {
        emitted: Vec::new(),
        pending: Vec::new(),
    };
    for row in releases {
        if cursor.known.contains(&row.rg_mbid.to_lowercase()) {
            continue;
        }
        if !include_types.is_empty()
            && row
                .primary_type
                .as_deref()
                .is_none_or(|kind| !include_types.iter().any(|want| want == kind))
        {
            continue;
        }
        let Some(date) = row.first_release_date.as_deref() else {
            continue;
        };
        if !is_complete_date(date) {
            continue;
        }
        if cursor
            .cursor_date
            .as_deref()
            .is_some_and(|cursor| date < cursor)
        {
            continue;
        }
        if date > today {
            seen.pending.push(PendingRelease {
                rg_mbid: row.rg_mbid.clone(),
                title: row.title.clone(),
                date: date.to_owned(),
            });
        } else {
            seen.emitted.push(row.clone());
        }
    }
    seen
}

/// True for complete `YYYY-MM-DD` dates (v2 `_COMPLETE_DATE_RE`).
fn is_complete_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && date[..4].parse::<u32>().is_ok()
        && date[5..7]
            .parse::<u32>()
            .is_ok_and(|month| (1..=12).contains(&month))
        && date[8..10]
            .parse::<u32>()
            .is_ok_and(|day| (1..=31).contains(&day))
}

/// What one upgrade sweep accomplished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepSummary {
    /// Upgrade grabs enqueued.
    pub enqueued: usize,
    /// Items skipped because no admin could own them (whole sweep).
    pub skipped_no_admin: bool,
    /// Items that errored (each isolated from the rest).
    pub errors: usize,
}

/// Upgrade-sweep dependencies.
pub struct SweepDeps {
    /// Policy source, re-read every pass so enabling needs no restart.
    pub policy: Arc<dyn Fn() -> UpgradePolicy + Send + Sync>,
    /// Cutoff-unmet worklist.
    pub worklist: UpgradeWorklist,
    /// Admin directory for sweep ownership.
    pub admins: Arc<AdminDirectory>,
    /// Download dispatch.
    pub downloads: Arc<dyn DownloadDispatch>,
    /// Durable ticks.
    pub ticks: Arc<dyn TickSink>,
}

/// Run one background-upgrade sweep when due: while upgrades are allowed
/// and the scan is enabled, walk the cutoff-unmet worklist and enqueue at
/// most `max_per_run` origin-`upgrade` grabs owned by the oldest admin.
/// `AlreadyInLibrary` answers never count as enqueued; a poison item never
/// starves its siblings (v2 `run_background_upgrade_sweep`).
pub async fn sweep_tick(now: i64, state: &mut LoopState, deps: &SweepDeps) -> SweepSummary {
    let policy = (deps.policy)();
    let interval = (policy.interval_hours.max(1) * 3600) as i64;
    if !state.due(now, interval) {
        return SweepSummary::default();
    }
    state.ran(now);
    if !(policy.upgrade_allowed && policy.scan_enabled) {
        return SweepSummary::default();
    }
    let Some(owner) = deps.admins.oldest_admin() else {
        return SweepSummary {
            skipped_no_admin: true,
            ..SweepSummary::default()
        };
    };
    let mut summary = SweepSummary::default();
    let items = match deps.worklist.list_cutoff_unmet().await {
        Ok(items) => items,
        Err(error) => {
            tracing::warn!(?error, "upgrade sweep could not read the worklist");
            summary.errors += 1;
            return summary;
        }
    };
    for item in items {
        if summary.enqueued >= policy.max_per_run {
            break;
        }
        let dispatch = DispatchRequest {
            user_id: owner.clone(),
            kind: DispatchKind::Album,
            mbid: item.rg_mbid.clone(),
            artist: item.artist.clone(),
            title: item.title.clone(),
            origin: "upgrade".to_owned(),
            idempotency_key: None,
        };
        match deps.downloads.dispatch_upgrade(&dispatch).await {
            Ok(super::seams::UpgradeDispatch::Enqueued(task_id)) => {
                deps.ticks.emit(
                    "upgrade.enqueued",
                    &format!("upgrade {} enqueued as {task_id}", item.rg_mbid),
                    now,
                );
                summary.enqueued += 1;
            }
            Ok(super::seams::UpgradeDispatch::AlreadyInLibrary) => {}
            Err(cause) => {
                tracing::warn!(rg_mbid = %item.rg_mbid, %cause, "upgrade dispatch failed");
                summary.errors += 1;
            }
        }
    }
    summary
}

/// What one status-sync pass accomplished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncSummary {
    /// Rows reconciled (status moved).
    pub reconciled: usize,
    /// Rows that reached `imported`.
    pub imported: usize,
    /// Rows that errored (each isolated).
    pub errors: usize,
}

/// Status-sync dependencies.
pub struct SyncDeps {
    /// Request ledger.
    pub ledger: RequestStore,
    /// Download dispatch for task lookups.
    pub downloads: Arc<dyn DownloadDispatch>,
    /// Library presence for the album fallback.
    pub library: Arc<LibraryPresence>,
    /// Durable ticks.
    pub ticks: Arc<dyn TickSink>,
}

/// Map a download task status onto a request status (v2
/// `_TASK_TO_REQUEST_STATUS`, `requests_page_service.py`).
pub fn map_task_status(task_status: &str) -> Option<&'static str> {
    match task_status {
        "downloading" | "processing" => Some("downloading"),
        "completed" => Some("imported"),
        "partial" => Some("incomplete"),
        "failed" => Some("failed"),
        "cancelled" => Some("cancelled"),
        _ => None,
    }
}

/// Terminal request statuses: nothing left to reconcile.
pub fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "imported" | "incomplete" | "failed" | "cancelled" | "ignored"
    )
}

/// Run one request-status-sync pass when due: reconcile every active row
/// with its native download task. Track rows resolve only through their
/// linked task id (a recording MBID is not a library key), while album
/// rows without a task fall back to library presence (v2 `_reconcile_request`
/// quirk). Terminal moves stamp `completed_at`; a won race to `imported`
/// ticks the notify. One bad row never stops the sweep.
pub async fn sync_tick(now: i64, state: &mut LoopState, deps: &SyncDeps) -> SyncSummary {
    if !state.due(now, SYNC_INTERVAL.as_secs() as i64) {
        return SyncSummary::default();
    }
    state.ran(now);
    let mut summary = SyncSummary::default();
    let rows = match deps.ledger.active(None, None).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(?error, "status sync could not read live requests");
            summary.errors += 1;
            return summary;
        }
    };
    for row in rows {
        if let Err(cause) = sync_one(now, deps, &row, &mut summary).await {
            tracing::warn!(key = %row.key, %cause, "status sync failed for one request");
            summary.errors += 1;
        }
    }
    summary
}

/// Reconcile one live request row.
async fn sync_one(
    now: i64,
    deps: &SyncDeps,
    row: &RequestRecord,
    summary: &mut SyncSummary,
) -> Result<(), String> {
    let at = epoch(now);
    let mut task_status = None;
    if let Some(task_id) = row.task_id.as_deref() {
        task_status = deps.downloads.task_status(task_id).await?;
    }
    if task_status.is_none() && row.kind == RequestKind::Album {
        task_status = deps
            .downloads
            .active_task_for_album(&row.key)
            .await?
            .map(|view| view.status);
    }
    if let Some(task_status) = task_status {
        let Some(mapped) = map_task_status(&task_status) else {
            return Ok(());
        };
        if mapped == row.status {
            return Ok(());
        }
        let terminal = is_terminal(mapped) && mapped != "incomplete";
        let won = deps
            .ledger
            .update_status(
                row.kind,
                &row.key,
                mapped,
                terminal.then_some(at),
                Some(row.generation),
            )
            .await
            .map_err(|error| format!("{error:?}"))?;
        if won {
            summary.reconciled += 1;
            if mapped == "imported" {
                summary.imported += 1;
                deps.ticks.emit(
                    "request_fulfilled",
                    &format!("request {} imported (task {task_status})", row.key),
                    now,
                );
            }
        }
        return Ok(());
    }
    if row.kind == RequestKind::Track || !deps.library.contains(&row.key) {
        return Ok(());
    }
    let won = deps
        .ledger
        .update_status(
            row.kind,
            &row.key,
            "imported",
            Some(at),
            Some(row.generation),
        )
        .await
        .map_err(|error| format!("{error:?}"))?;
    if won {
        summary.reconciled += 1;
        summary.imported += 1;
        deps.ticks.emit(
            "request_fulfilled",
            &format!("request {} imported (library presence)", row.key),
            now,
        );
    }
    Ok(())
}

/// Handle for one spawned loop: its registry name plus the task `main`
/// awaits after serve.
pub struct FlowHandle {
    /// Registry name.
    pub name: &'static str,
    /// Loop task.
    pub task: tokio::task::JoinHandle<()>,
}

/// Register one loop as an ephemeral job. Idempotent across restarts.
pub async fn register_ephemeral_loop(
    wakeups: &DurableWorkWakeups,
    lane: &WriteLane,
    job: &str,
) -> Result<(), String> {
    wakeups
        .register_job(lane, job, JobKind::Ephemeral, None)
        .await
        .map_err(|error| error.to_string())
}

/// One loop pass behind the shared run skeleton.
pub trait FlowPass: Send {
    /// Run one pass when due. Per-item failures stay inside the tick.
    fn run(&mut self, now: i64, state: &mut LoopState) -> impl Future<Output = ()> + Send;
}

/// Wanted-watcher pass over shared deps.
pub struct WantedPass {
    /// Tick deps.
    pub deps: Arc<WantedDeps>,
}

impl FlowPass for WantedPass {
    async fn run(&mut self, now: i64, state: &mut LoopState) {
        let summary = wanted_tick(now, state, &self.deps).await;
        if summary.errors > 0 {
            tracing::warn!(?summary, "wanted sweep finished with errors");
        }
    }
}

/// Follow-poll pass over shared deps.
pub struct FollowPass {
    /// Tick deps.
    pub deps: Arc<FollowDeps>,
}

impl FlowPass for FollowPass {
    async fn run(&mut self, now: i64, state: &mut LoopState) {
        let summary = follow_tick(now, state, &self.deps).await;
        if summary.errors > 0 {
            tracing::warn!(?summary, "follow poll finished with errors");
        }
    }
}

/// Upgrade-sweep pass over shared deps.
pub struct SweepPass {
    /// Tick deps.
    pub deps: Arc<SweepDeps>,
}

impl FlowPass for SweepPass {
    async fn run(&mut self, now: i64, state: &mut LoopState) {
        let summary = sweep_tick(now, state, &self.deps).await;
        if summary.errors > 0 {
            tracing::warn!(?summary, "upgrade sweep finished with errors");
        }
    }
}

/// Status-sync pass over shared deps.
pub struct SyncPass {
    /// Tick deps.
    pub deps: Arc<SyncDeps>,
}

impl FlowPass for SyncPass {
    async fn run(&mut self, now: i64, state: &mut LoopState) {
        let summary = sync_tick(now, state, &self.deps).await;
        if summary.errors > 0 {
            tracing::warn!(?summary, "status sync finished with errors");
        }
    }
}

/// Spawn the wanted-watcher loop: startup delay, then a jittered 15-minute
/// cadence until shutdown. Registers `wanted-watcher` as ephemeral first.
pub async fn spawn_wanted_loop<S, J>(
    sleeper: S,
    jitter: J,
    wakeups: DurableWorkWakeups,
    lane: WriteLane,
    clock: Arc<dyn Clock>,
    deps: Arc<WantedDeps>,
) -> Result<(FlowHandle, Arc<FlowRegistry>), String>
where
    S: Sleeper + Clone + Send + 'static,
    J: Jitter + Clone + Send + 'static,
{
    spawn_flow(
        WANTED_JOB,
        sleeper,
        Some(Box::new(jitter)),
        WANTED_INITIAL_DELAY,
        WANTED_INTERVAL,
        wakeups,
        lane,
        clock,
        WantedPass { deps },
    )
    .await
}

/// Spawn the follow new-release poll: startup delay, then a jittered
/// 1-minute due-driven cadence until shutdown.
pub async fn spawn_follow_loop<S, J>(
    sleeper: S,
    jitter: J,
    wakeups: DurableWorkWakeups,
    lane: WriteLane,
    clock: Arc<dyn Clock>,
    deps: Arc<FollowDeps>,
) -> Result<(FlowHandle, Arc<FlowRegistry>), String>
where
    S: Sleeper + Clone + Send + 'static,
    J: Jitter + Clone + Send + 'static,
{
    spawn_flow(
        FOLLOW_JOB,
        sleeper,
        Some(Box::new(jitter)),
        FOLLOW_INITIAL_DELAY,
        FOLLOW_INTERVAL,
        wakeups,
        lane,
        clock,
        FollowPass { deps },
    )
    .await
}

/// Spawn the background upgrade sweep: startup delay, then the policy
/// cadence (default 12h, plain sleep like v2) until shutdown.
pub async fn spawn_sweep_loop<S>(
    sleeper: S,
    wakeups: DurableWorkWakeups,
    lane: WriteLane,
    clock: Arc<dyn Clock>,
    deps: Arc<SweepDeps>,
) -> Result<(FlowHandle, Arc<FlowRegistry>), String>
where
    S: Sleeper + Clone + Send + 'static,
{
    let hours = (deps.policy)().interval_hours.max(1);
    spawn_flow(
        UPGRADE_JOB,
        sleeper,
        None,
        UPGRADE_INITIAL_DELAY,
        Duration::from_secs(hours * 3600),
        wakeups,
        lane,
        clock,
        SweepPass { deps },
    )
    .await
}

/// Spawn the request-status sync: startup delay, then a plain 1-minute
/// cadence until shutdown.
pub async fn spawn_sync_loop<S>(
    sleeper: S,
    wakeups: DurableWorkWakeups,
    lane: WriteLane,
    clock: Arc<dyn Clock>,
    deps: Arc<SyncDeps>,
) -> Result<(FlowHandle, Arc<FlowRegistry>), String>
where
    S: Sleeper + Clone + Send + 'static,
{
    spawn_flow(
        SYNC_JOB,
        sleeper,
        None,
        SYNC_INITIAL_DELAY,
        SYNC_INTERVAL,
        wakeups,
        lane,
        clock,
        SyncPass { deps },
    )
    .await
}

/// Register one loop and spawn its task.
#[allow(clippy::too_many_arguments)]
async fn spawn_flow<S, P>(
    job: &'static str,
    sleeper: S,
    jitter: Option<Box<dyn Jitter>>,
    initial_delay: Duration,
    interval: Duration,
    wakeups: DurableWorkWakeups,
    lane: WriteLane,
    clock: Arc<dyn Clock>,
    mut pass: P,
) -> Result<(FlowHandle, Arc<FlowRegistry>), String>
where
    S: Sleeper + Clone + Send + 'static,
    P: FlowPass + 'static,
{
    register_ephemeral_loop(&wakeups, &lane, job).await?;
    let registry = Arc::new(FlowRegistry::new());
    let task_registry = registry.clone();
    let task = tokio::spawn(async move {
        run_loop(
            &task_registry,
            job,
            sleeper,
            jitter,
            initial_delay,
            interval,
            &wakeups,
            &lane,
            clock.as_ref(),
            &mut pass,
        )
        .await;
    });
    Ok((FlowHandle { name: job, task }, registry))
}

/// Shared loop body: sleep the startup delay, run one guarded pass, then
/// repeat on the (optionally jittered) interval until shutdown. Failures
/// inside a pass are already isolated per item; the loop itself only exits
/// on shutdown (v2 loop contract).
#[allow(clippy::too_many_arguments)]
async fn run_loop<S, P>(
    registry: &FlowRegistry,
    job: &'static str,
    sleeper: S,
    jitter: Option<Box<dyn Jitter>>,
    initial_delay: Duration,
    interval: Duration,
    wakeups: &DurableWorkWakeups,
    lane: &WriteLane,
    clock: &dyn Clock,
    pass: &mut P,
) where
    S: Sleeper,
    P: FlowPass,
{
    if sleeper.sleep(initial_delay).await {
        let mut state = LoopState::new();
        loop {
            let now = clock.now_unix();
            if registry.begin(job) {
                pass.run(now, &mut state).await;
                registry.finish(job);
                if let Err(error) = wakeups.heartbeat(lane, job).await {
                    tracing::warn!(job, %error, "flow heartbeat failed");
                }
            }
            let wait = jitter
                .as_deref()
                .map_or(interval, |jitter| jittered(interval, jitter));
            if !sleeper.sleep(wait).await {
                break;
            }
        }
    }
    if let Err(error) = wakeups.set_job_state(lane, job, JobState::Stopped).await {
        tracing::warn!(job, %error, "flow stop state write failed");
    }
}
