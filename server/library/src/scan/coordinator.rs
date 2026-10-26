//! Single durable entry point for every scan trigger.
//!
//! Port of v2's library scan coordinator plus the indexing and
//! reconciliation phases (v2's library indexer and reconciler). Tag reads
//! ride the [`TagReader`](super::seams::TagReader) seam with bounded
//! concurrency on the blocking pool; each index window commits its catalog
//! rows, inventory marks, counters, and identify offers in one transaction,
//! so a resumed run starts at the first unprocessed row.
//!
//! Pipeline per run: discover (walk) -> index (tag + catalog) ->
//! reconcile (missing detection) -> completed. Control (pause/stop) and
//! policy supersede flow through [`Checkpoint`], which the coordinator
//! implements over live store state.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use droppedneedle_kernel::jobs::CancellationToken;

use super::models::{
    Counter, Counters, Disposition, Event, FailureCode, Outcome, Phase, RunState,
    ScanFailureRecord, ScanInventoryItem, ScanKind, ScanPhase, ScanRequest, ScanRequestResult,
    ScanRun, ScanScope, ScanState, ScopeDiscoveryState, Verdict,
};
use super::pool::BlockingPool;
use super::roots::{PolicyResolver, RootRegistry};
use super::seams::{Checkpoint, ScannedTags, TagReadError, TagReader};
use super::sqlite_store::SqliteScanStore;
use super::stop::RunStop;
use super::store::{CommitIndexedItem, EditionFill, IndexWindow, ScanStoreError};
use super::walk::InventoryScanner;
use super::watcher::WorkWakeups;
use crate::clock::LibraryClock as _;

/// Return freed scan heap to the OS after a terminal run. A 100k scan
/// churns hundreds of megabytes of transient buffers (inventory pages,
/// verdict maps, commit rows); the allocator keeps the freed pages mapped,
/// so post-scan idle RSS holds the whole peak without this release.
fn release_scan_memory() {
    // malloc_trim is glibc-only; other targets keep their pages.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // malloc_trim only releases fully-free top pages; retained live
        // blocks stay mapped, so this never moves live data.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

/// Index batch size between checkpoints (v2 commits per tag batch).
pub const INDEX_CHECKPOINT_EVERY: usize = 16;
/// Inventory rows processed per index page. The coordinator streams the
/// run's inventory in pages instead of holding all rows resident; 5,000
/// rows keep a page near 2 MB while paging overhead stays negligible.
pub const INDEX_PAGE_SIZE: usize = 5_000;
/// Index rows between commit/counter flushes. Control still checks every
/// [`INDEX_CHECKPOINT_EVERY`] rows; only the SQLite flush is coarser, so
/// a crash re-offers at most one window next run.
pub const INDEX_FLUSH_EVERY: usize = 256;
/// Bound on stale retries when settling control (v2
/// `SETTLE_STALE_MAX_RETRIES`).
pub const SETTLE_STALE_MAX_RETRIES: u32 = 10;
/// Counter-event throttle (v2 `COUNTER_EVENT_INTERVAL_SECONDS`).
pub const COUNTER_EVENT_INTERVAL_SECS: f64 = 2.0;

/// Live resolver source. v2 re-reads the resolver through a getter every
/// checkpoint so settings-save rebuilds never strand a run on stale
/// policy; this trait is that getter.
pub trait ResolverSource: Send + Sync {
    fn resolver(&self) -> PolicyResolver;
}

/// Fixed resolver for tests and single-config runtimes.
#[cfg(any(test, feature = "test-support"))]
pub struct StaticResolver {
    resolver: PolicyResolver,
}

#[cfg(any(test, feature = "test-support"))]
impl StaticResolver {
    pub fn new(registry: RootRegistry) -> Self {
        Self {
            resolver: PolicyResolver::new(registry),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ResolverSource for StaticResolver {
    fn resolver(&self) -> PolicyResolver {
        self.resolver.clone()
    }
}

/// Shared mutable resolver; settings saves swap the registry in place.
#[derive(Clone)]
pub struct SharedResolver {
    inner: Arc<Mutex<PolicyResolver>>,
}

impl Default for SharedResolver {
    fn default() -> Self {
        Self::new(RootRegistry::new(Vec::new(), false, "rev-0"))
    }
}

impl SharedResolver {
    pub fn new(registry: RootRegistry) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PolicyResolver::new(registry))),
        }
    }

    pub fn update(&self, registry: RootRegistry) {
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = PolicyResolver::new(registry);
    }
}

impl ResolverSource for SharedResolver {
    fn resolver(&self) -> PolicyResolver {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Why `request_run` refused a request. Messages match v2 exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanRequestError {
    Disabled,
    EmptyScopes,
    StalePolicy,
    UnknownRoots,
    BadCursor,
    /// The store could not record the request.
    Store(ScanStoreError),
}

impl std::fmt::Display for ScanRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanRequestError::Disabled => write!(
                f,
                "The local library is disabled. Enable it in Settings -> Library before starting a scan."
            ),
            ScanRequestError::EmptyScopes => write!(f, "Select at least one library scope."),
            ScanRequestError::StalePolicy => {
                write!(f, "The selected library policy has changed.")
            }
            ScanRequestError::UnknownRoots => {
                write!(f, "One or more selected library scopes no longer exist.")
            }
            ScanRequestError::BadCursor => {
                write!(f, "The scan history cursor is invalid.")
            }
            ScanRequestError::Store(error) => write!(f, "The scan store failed: {error}"),
        }
    }
}

impl std::error::Error for ScanRequestError {}

/// Scan event published to the event bus (v2 `LibraryScanEventPublisher`).
#[derive(Debug, Clone)]
pub struct ScanEvent {
    pub id: String,
    pub stream_kind: String,
    pub stream_revision: u64,
    pub run_id: String,
    pub row_revision: u64,
    pub event_revision: u64,
    pub state: ScanState,
    pub event: String,
    /// Whether the run indexed, added, changed or lost any file so far.
    pub changed: bool,
}

/// Durable-revision scan invalidations with counter-rate throttling (v2
/// scan events). Terminal states drop their throttle entry; runs end via
/// store transitions, never publisher callbacks.
#[derive(Clone)]
pub struct ScanEventPublisher {
    sink: Arc<dyn Fn(ScanEvent) + Send + Sync>,
    last_counter_event: Arc<Mutex<HashMap<String, Instant>>>,
}

impl ScanEventPublisher {
    pub fn new(sink: Arc<dyn Fn(ScanEvent) + Send + Sync>) -> Self {
        Self {
            sink,
            last_counter_event: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Publish one event. Counter events throttle to one per 2s per run;
    /// returns false when throttled.
    pub fn publish(&self, run: &ScanRun, stream_revision: u64, event: &str, counter: bool) -> bool {
        let mut last = self
            .last_counter_event
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if counter {
            if let Some(previous) = last.get(&run.id)
                && previous.elapsed().as_secs_f64() < COUNTER_EVENT_INTERVAL_SECS
            {
                return false;
            }
            last.insert(run.id.clone(), Instant::now());
        } else if run.state.is_ended() {
            last.remove(&run.id);
        }
        drop(last);
        let changed = [
            Counter::New,
            Counter::Changed,
            Counter::Indexed,
            Counter::Missing,
        ]
        .into_iter()
        .any(|counter| run.counters.get(counter) > 0);
        (self.sink)(ScanEvent {
            id: format!("scan:{stream_revision}"),
            stream_kind: "scan".to_owned(),
            stream_revision,
            run_id: run.id.clone(),
            row_revision: run.row_revision,
            event_revision: run.event_revision,
            state: run.state.scan_state(),
            event: event.to_owned(),
            changed,
        });
        true
    }
}

fn default_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Scan coordinator (v2 `LibraryScanCoordinator`).
pub struct LibraryScanCoordinator<T: TagReader> {
    store: Arc<SqliteScanStore>,
    inventory: InventoryScanner,
    tags: Arc<T>,
    pool: BlockingPool,
    resolvers: Arc<dyn ResolverSource>,
    events: Option<ScanEventPublisher>,
    wakeups: Option<WorkWakeups>,
    clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    idgen: Arc<dyn Fn() -> String + Send + Sync>,
    pending_control: Mutex<HashSet<String>>,
    last_progress_log: Mutex<HashMap<String, f64>>,
    /// The stop flag of the run being driven, by run id. A pause or stop
    /// request for that run trips it.
    drives: Mutex<HashMap<String, RunStop>>,
    /// Held while dirty scope marks are turned into a scan request.
    dirty_turn: Mutex<()>,
}

/// Why a phase of a drive ended early.
enum Halt {
    /// The run was asked to pause or stop: settle as asked.
    Stop,
    /// The database is closing: leave the run as it is, so the next start
    /// resumes it.
    Closed,
    /// Anything else: the run fails.
    Fail(ScanStoreError),
}

impl From<ScanStoreError> for Halt {
    fn from(error: ScanStoreError) -> Self {
        match error {
            ScanStoreError::Stopped | ScanStoreError::ReadCancelled { .. } => Halt::Stop,
            ScanStoreError::LaneClosed => Halt::Closed,
            other => Halt::Fail(other),
        }
    }
}

impl<T: TagReader + 'static> LibraryScanCoordinator<T> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<SqliteScanStore>,
        pool: BlockingPool,
        tags: Arc<T>,
        resolvers: Arc<dyn ResolverSource>,
        clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    ) -> Self {
        let inventory = InventoryScanner::new(Arc::clone(&store), pool.clone(), Arc::clone(&clock));
        Self {
            store,
            inventory,
            tags,
            pool,
            resolvers,
            events: None,
            wakeups: None,
            clock,
            idgen: Arc::new(default_id),
            pending_control: Mutex::new(HashSet::new()),
            last_progress_log: Mutex::new(HashMap::new()),
            drives: Mutex::new(HashMap::new()),
            dirty_turn: Mutex::new(()),
        }
    }

    pub fn with_events(mut self, events: ScanEventPublisher) -> Self {
        self.events = Some(events);
        self
    }

    pub fn with_wakeups(mut self, wakeups: WorkWakeups) -> Self {
        self.wakeups = Some(wakeups);
        self
    }

    pub fn store(&self) -> &Arc<SqliteScanStore> {
        &self.store
    }

    /// Live policy resolver (re-read, never cached).
    pub fn resolver(&self) -> PolicyResolver {
        self.resolvers.resolver()
    }

    /// Live root registry (re-read, never cached).
    pub fn registry(&self) -> RootRegistry {
        self.resolvers.resolver().registry().clone()
    }

    fn now(&self) -> f64 {
        self.clock.now_secs()
    }

    /// The turn to turn dirty scope marks into a scan request.
    pub(super) fn dirty_turn(&self) -> std::sync::MutexGuard<'_, ()> {
        self.dirty_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn drives(&self) -> std::sync::MutexGuard<'_, HashMap<String, RunStop>> {
        self.drives
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish(&self, run: &ScanRun, event: &str, counter: bool) {
        if let Some(events) = &self.events {
            match self.store.stream_revision("scan") {
                Ok(revision) => {
                    events.publish(run, revision, event, counter);
                }
                Err(error) => tracing::warn!(%error, event, "scan event not published"),
            }
        }
    }

    fn log_progress(&self, run: &ScanRun, event: &str, force: bool) {
        let now = self.now();
        {
            let mut last = self
                .last_progress_log
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !force && now - last.get(&run.id).copied().unwrap_or(0.0) < 30.0 {
                return;
            }
            last.insert(run.id.clone(), now);
        }
        let counter = |counter| run.counters.get(counter);
        let total = counter(Counter::Total);
        let inspected = counter(Counter::Inspected);
        let percentage = if run.state.scan_state() == ScanState::Completed {
            100.0
        } else if total > 0 {
            inspected as f64 * 100.0 / total as f64
        } else {
            0.0
        };
        tracing::info!(
            event,
            state = run.state.scan_state().as_str(),
            phase = run.state.phase().as_str(),
            discovered = counter(Counter::Discovered),
            inspected,
            total,
            percentage,
            new = counter(Counter::New),
            changed = counter(Counter::Changed),
            unchanged = counter(Counter::Unchanged),
            excluded = counter(Counter::Excluded),
            failed = counter(Counter::Errored),
            "library_scan",
        );
    }

    /// Request a scan run (v2 `request_run`, validations included).
    pub fn request_run(
        &self,
        request: &ScanRequest,
    ) -> Result<ScanRequestResult, ScanRequestError> {
        if !self.resolvers.resolver().enabled() {
            return Err(ScanRequestError::Disabled);
        }
        if request.scopes.is_empty() {
            return Err(ScanRequestError::EmptyScopes);
        }
        if request
            .scopes
            .iter()
            .any(|scope| scope.policy_revision != request.policy_revision)
        {
            return Err(ScanRequestError::StalePolicy);
        }
        if request.kind != ScanKind::PolicyReconcile {
            // GH-296: a request may not queue scopes for unconfigured
            // roots. Frozen policy-apply scopes are exempt: a removed
            // root's frozen scope converges through skip-and-report.
            let configured: HashSet<String> = self
                .resolvers
                .resolver()
                .registry()
                .roots()
                .iter()
                .map(|root| root.id.clone())
                .collect();
            if request
                .scopes
                .iter()
                .any(|scope| !configured.contains(&scope.root_id))
            {
                return Err(ScanRequestError::UnknownRoots);
            }
        }
        let run_id = (self.idgen)();
        let result = self
            .store
            .request_run(request, &run_id, self.now())
            .map_err(ScanRequestError::Store)?;
        if result.disposition != Disposition::Conflict {
            // v2 request_scan_run wakes the supervisor for every accepted
            // disposition; the store here has no wakeups, so the
            // coordinator wakes instead.
            if let Some(wakeups) = &self.wakeups {
                wakeups.notify("scan");
            }
            match self.store.get_run(&result.run_id) {
                Ok((run, _, _)) => self.publish(&run, "scan.requested", false),
                Err(error) => tracing::warn!(%error, "scan request event not published"),
            }
        }
        Ok(result)
    }

    pub fn snapshot(
        &self,
        run_id: &str,
    ) -> Result<(ScanRun, Vec<ScanScope>, Counters), ScanStoreError> {
        self.store.get_run(run_id)
    }

    /// The active run and the queued one.
    pub fn current_runs(&self) -> Result<Vec<ScanRun>, ScanStoreError> {
        self.store.list_current()
    }

    /// The active and queued runs, for the callers that must never work
    /// while a scan runs (management, operations, repairs). It waits for
    /// the long pool rather than answering busy, and a run list it cannot
    /// read answers as if a scan were active, with an error logged: those
    /// callers then wait, and none moves a file under the walker.
    pub fn current(&self) -> Vec<ScanRun> {
        self.store
            .list_current_waiting(&RunStop::never())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan runs unreadable; answering as if a scan is active");
                vec![self.missing_run("unreadable", FailureCode::UnexpectedWorkerFailure)]
            })
    }

    pub fn history(&self, limit: usize) -> Result<Vec<ScanRun>, ScanStoreError> {
        self.store.list_history(limit, None)
    }

    pub fn latest_filesystem_terminal(&self) -> Result<Option<ScanRun>, ScanStoreError> {
        self.store.latest_filesystem_terminal()
    }

    /// One history page; cursor is `terminal_at:run_id` (v2 `history_page`).
    pub fn history_page(
        &self,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<ScanRun>, Option<String>), ScanRequestError> {
        let mut before: Option<(f64, String)> = None;
        if let Some(cursor) = cursor {
            let (terminal, id) = cursor.split_once(':').ok_or(ScanRequestError::BadCursor)?;
            let terminal_at: f64 = terminal.parse().map_err(|_| ScanRequestError::BadCursor)?;
            before = Some((terminal_at, id.to_owned()));
        }
        let mut items = self
            .store
            .list_history(
                limit + 1,
                before.as_ref().map(|(at, id)| (*at, id.as_str())),
            )
            .map_err(ScanRequestError::Store)?;
        let mut next_cursor = None;
        if items.len() > limit {
            items.truncate(limit);
            if let Some(last) = items.last() {
                next_cursor = last
                    .terminal_at
                    .map(|terminal| format!("{terminal}:{}", last.id));
            }
        }
        Ok((items, next_cursor))
    }

    /// One page of a run's failures (v2 `scan_run_failures`). An unknown
    /// run is not-found, never an empty page.
    pub fn scan_run_failures(
        &self,
        run_id: &str,
        limit: usize,
        after: Option<i64>,
    ) -> Result<(Vec<ScanFailureRecord>, Option<i64>), ScanStoreError> {
        self.store.get_run(run_id)?;
        self.store.failures_page(run_id, limit, after)
    }

    /// Approximate file count for the scopes plus when it was taken (v2
    /// `estimate`).
    pub fn estimate(&self, scopes: &[ScanScope]) -> Result<(u64, f64), ScanStoreError> {
        Ok((self.store.estimate_scopes(scopes)?, self.now()))
    }

    /// Pause, resume, or stop a run (v2 `control`). A pause or stop of the
    /// run being driven trips its stop flag, so a long read, a chunked
    /// write or a wait on a busy database ends early. The worker is woken
    /// after every control: a resumed run, a run freed for the next one, or
    /// a pause or stop of a run no drive holds has work for it now.
    pub fn control(
        &self,
        run_id: &str,
        event: Event,
        expected_revision: u64,
    ) -> Result<(ScanRun, u64), ScanStoreError> {
        let (run, stream_revision) =
            self.store
                .request_control(run_id, event, expected_revision, self.now())?;
        {
            let mut pending = self
                .pending_control
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if run.state.is_settling() {
                pending.insert(run.id.clone());
            } else {
                pending.remove(&run.id);
            }
        }
        if (run.state.is_settling() || run.state.is_ended())
            && let Some(stop) = self.drives().get(&run.id)
        {
            stop.trip();
        }
        self.publish(&run, "scan.transition", false);
        if run.state.is_ended() {
            self.flush_terminal();
        }
        if let Some(wakeups) = &self.wakeups {
            wakeups.notify("scan");
        }
        let logged = match event {
            Event::Resume => "control_resume",
            Event::Pause => "control_pause",
            _ => "control_stop",
        };
        self.log_progress(&run, logged, true);
        Ok((run, stream_revision))
    }

    /// The terminal flush: drop the classify snapshot. The catalog
    /// revision moved with each window that wrote a row.
    fn flush_terminal(&self) {
        self.store.drop_classify_snapshot();
    }

    /// Boot recovery (v2 `recover` / `recover_stopping`).
    pub fn recover(&self) -> Result<Vec<ScanRun>, ScanStoreError> {
        if !self.resolvers.resolver().enabled() {
            return self.recover_stopping();
        }
        let runs = self.store.recover(&RunStop::never(), self.now())?;
        {
            let mut pending = self
                .pending_control
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending.clear();
            for run in &runs {
                self.log_progress(run, "recovery", true);
            }
        }
        Ok(runs)
    }

    pub fn recover_stopping(&self) -> Result<Vec<ScanRun>, ScanStoreError> {
        let runs = self.store.recover_stopping(&RunStop::never(), self.now())?;
        let mut pending = self
            .pending_control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for run in &runs {
            pending.remove(&run.id);
            self.publish(run, "scan.transition", false);
            self.flush_terminal();
            self.log_progress(run, "recovery", true);
        }
        Ok(runs)
    }

    /// Settle a pausing/stopping run (v2 `_settle_pending_control`). Its
    /// writes wait out a busy database: they are the answer to the stop.
    fn settle_pending_control(&self, run_id: &str) -> ScanRun {
        let mut stale_retries = 0u32;
        loop {
            let (run, _, _) = match self.store.reread_run(&RunStop::never(), run_id) {
                Ok(found) => found,
                Err(error) => {
                    // The run vanished or cannot be read mid-settle.
                    // Answer failed, never panic.
                    tracing::error!(run_id, %error, "scan run unreadable while settling");
                    self.pending_control
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(run_id);
                    return self.missing_run(run_id, FailureCode::UnexpectedWorkerFailure);
                }
            };
            if !run.state.is_settling() {
                self.pending_control
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&run.id);
                return run;
            }
            match self.store.transition(
                &RunStop::never(),
                &run.id,
                run.row_revision,
                Event::Settle,
                self.now(),
                None,
            ) {
                Ok(settled) => {
                    self.publish(&settled, "scan.transition", false);
                    if settled.state.is_ended() {
                        self.flush_terminal();
                    }
                    self.log_progress(
                        &settled,
                        if settled.state.is_ended() {
                            "stop"
                        } else {
                            "pause"
                        },
                        true,
                    );
                    self.pending_control
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&settled.id);
                    return settled;
                }
                Err(ScanStoreError::StaleRevision { .. }) => {
                    // A racing writer keeps moving the revision.
                    // Bounded retries, then the stop-signal path: return
                    // unsettled without discarding the pending entry, so
                    // checkpoint keeps returning false.
                    stale_retries += 1;
                    if stale_retries >= SETTLE_STALE_MAX_RETRIES {
                        tracing::warn!(
                            run_id = %run_id,
                            retries = stale_retries,
                            "scan settle spin exhausted; leaving unsettled for the supervisor"
                        );
                        return run;
                    }
                }
                Err(error) => {
                    // Left unsettled: the pending entry stays, and the next
                    // drive or boot recovery settles it.
                    tracing::error!(run_id, %error, "scan control not settled");
                    return run;
                }
            }
        }
    }

    /// Drive one run like [`run_once`](Self::run_once), but a signalled
    /// shutdown abandons the drive instead of waiting it out, and trips the
    /// drive's stop flag so a long read or a chunked write on a blocking
    /// thread ends too. The run keeps its active state, exactly as after a
    /// crash: every committed window is durable, the rest is still
    /// pending, and the next start resumes it. A pre-signalled shutdown
    /// claims no new work. Returns the run when it reached a stopping
    /// point, `None` otherwise.
    pub async fn run_once_with_shutdown(
        &self,
        root_paths: &HashMap<String, PathBuf>,
        shutdown: &CancellationToken,
    ) -> Option<ScanRun> {
        if shutdown.is_cancelled() {
            return None;
        }
        let stop = RunStop::default();
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                stop.trip();
                tracing::info!("shutdown: the active scan resumes on the next start");
                None
            }
            outcome = self.drive(root_paths, &stop) => outcome,
        }
    }

    /// Drive one run to the next stopping point (v2 `run_once`). Returns
    /// the run, or `None` when no work was available.
    pub async fn run_once(&self, root_paths: &HashMap<String, PathBuf>) -> Option<ScanRun> {
        self.drive(root_paths, &RunStop::default()).await
    }

    async fn drive(
        &self,
        root_paths: &HashMap<String, PathBuf>,
        stop: &RunStop,
    ) -> Option<ScanRun> {
        // Runs that ended without their end passes (a stop request, boot
        // recovery, a crash) settle first; their inventory waits for it.
        // Finished runs' inventory goes even while the library is off.
        self.settle_ended_runs(stop).await;
        self.cleanup_terminal_inventory(stop).await;
        if !self.resolvers.resolver().enabled() {
            return None;
        }
        let claimed = match self.store.resumable(stop) {
            Ok(Some(run)) => Ok(Some((run, false))),
            Ok(None) => self
                .store
                .claim_next(stop, self.now())
                .map(|run| run.map(|run| (run, true))),
            Err(error) => Err(error),
        };
        let (run, newly_claimed) = match claimed {
            Ok(Some(found)) => found,
            Ok(None) => return None,
            Err(error) => {
                tracing::error!(%error, "no scan run could be claimed");
                return None;
            }
        };
        if newly_claimed {
            self.publish(&run, "scan.transition", false);
        }
        self.log_progress(&run, "start", true);
        self.drives().insert(run.id.clone(), stop.clone());
        let mut outcome = self.continue_run(&run, root_paths, stop).await;
        if outcome.terminal_at.is_some() {
            // The stop that ended the run must not end its settling too.
            let settling = RunStop::never();
            self.settle_ended_runs(&settling).await;
            match self.store.reread_run(&settling, &outcome.id) {
                Ok((settled, _, _)) => outcome = settled,
                Err(error) => tracing::warn!(%error, "ended scan run not read again"),
            }
        }
        self.drives().remove(&run.id);
        self.pending_control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&run.id);
        if outcome.state.is_ended() {
            // Release the heap the scan churned. The WAL it grew is
            // reclaimed by the checkpoint service once the database is idle.
            release_scan_memory();
        }
        Some(outcome)
    }

    /// End a drive that a phase cut short, by the scan's failure rule.
    async fn after_halt(&self, run: &ScanRun, halt: Halt) -> ScanRun {
        match halt {
            Halt::Stop => self.settle_pending_control(&run.id),
            Halt::Closed => {
                tracing::info!(run_id = %run.id, "the database is closing; the scan resumes on the next start");
                run.clone()
            }
            Halt::Fail(error) => {
                tracing::error!(run_id = %run.id, %error, "scan worker failed");
                self.fail_active(run, FailureCode::UnexpectedWorkerFailure)
                    .await
            }
        }
    }

    async fn continue_run(
        &self,
        run: &ScanRun,
        root_paths: &HashMap<String, PathBuf>,
        stop: &RunStop,
    ) -> ScanRun {
        macro_rules! or_halt {
            ($call:expr) => {
                match $call {
                    Ok(value) => value,
                    Err(error) => return self.after_halt(run, Halt::from(error)).await,
                }
            };
        }
        let (mut run, scopes, _) = or_halt!(self.store.reread_run(stop, &run.id));
        if run.state.running().is_none() {
            // A pause or a stop asked for while no drive held the run.
            return self.settle_pending_control(&run.id);
        }
        if scopes.is_empty() {
            // v2 indexes scopes[0] and the IndexError becomes
            // UNEXPECTED_WORKER_FAILURE; fail the same way directly.
            return self
                .fail_active(&run, FailureCode::UnexpectedWorkerFailure)
                .await;
        }
        let frozen_policy_revision = scopes[0].policy_revision.clone();
        if run.state == RunState::Running(Phase::Discovering) {
            self.log_progress(&run, "phase_discovery_start", true);
            or_halt!(self.store.prepare_discovery_resume(stop, &run.id));
            let run_id = run.id.clone();
            or_halt!(
                self.on_store(stop, move |store, stop| {
                    store.cleanup_stale_inventory(stop, &run_id)
                })
                .await
            );
            let (fresh, scopes, _) = or_halt!(self.store.reread_run(stop, &run.id));
            run = fresh;
            run = or_halt!(
                self.inventory
                    .discover(
                        &run,
                        &scopes,
                        root_paths,
                        &self.resolvers.resolver(),
                        self,
                        stop
                    )
                    .await
            );
            self.publish(&run, "scan.progress", true);
            run = or_halt!(self.store.reread_run(stop, &run.id)).0;
            if run.state != RunState::Running(Phase::Discovering) {
                return self.settle_pending_control(&run.id);
            }
            run = or_halt!(self.store.finalize_discovery(stop, &run.id, self.now()));
            self.log_progress(&run, "phase_discovery_end", true);
            if let Err(halt) = self.guard_vanished_scopes(&run.id, stop).await {
                return self.after_halt(&run, halt).await;
            }
            run = or_halt!(self.store.transition(
                stop,
                &run.id,
                run.row_revision,
                Event::Advance,
                self.now(),
                None,
            ));
            self.publish(&run, "scan.transition", false);
        }
        if run.state == RunState::Running(Phase::Indexing) {
            self.log_progress(&run, "phase_indexing_start", true);
            if !self.check(&run.id, &frozen_policy_revision) {
                return or_halt!(self.store.reread_run(stop, &run.id)).0;
            }
            let counts = match self.index(&run, &frozen_policy_revision, stop).await {
                Ok(counts) => counts,
                Err(halt) => return self.after_halt(&run, halt).await,
            };
            run = or_halt!(self.store.reread_run(stop, &run.id)).0;
            if counts.identification_enqueued > 0 {
                self.publish(&run, "scan.progress", true);
            }
            if run.state != RunState::Running(Phase::Indexing) {
                return self.settle_pending_control(&run.id);
            }
            if !self.check(&run.id, &frozen_policy_revision) {
                return or_halt!(self.store.reread_run(stop, &run.id)).0;
            }
            self.log_progress(&run, "phase_indexing_end", true);
            run = or_halt!(self.store.transition(
                stop,
                &run.id,
                run.row_revision,
                Event::Advance,
                self.now(),
                None,
            ));
            self.publish(&run, "scan.transition", false);
        }
        self.log_progress(&run, "phase_reconciliation_start", true);
        let scopes = or_halt!(self.store.reread_run(stop, &run.id)).1;
        let missing = match self
            .reconcile(&run.id, &scopes, &frozen_policy_revision, stop)
            .await
        {
            Ok(missing) => missing,
            Err(halt) => return self.after_halt(&run, halt).await,
        };
        run = or_halt!(self.store.reread_run(stop, &run.id)).0;
        if missing > 0 {
            self.publish(&run, "scan.progress", true);
        }
        if run.state != RunState::Running(Phase::Reconciling) {
            return self.settle_pending_control(&run.id);
        }
        if !self.check(&run.id, &frozen_policy_revision) {
            return or_halt!(self.store.reread_run(stop, &run.id)).0;
        }
        self.log_progress(&run, "phase_reconciliation_end", true);
        run = or_halt!(self.store.transition(
            stop,
            &run.id,
            run.row_revision,
            Event::Advance,
            self.now(),
            None,
        ));
        self.publish(&run, "scan.transition", false);
        self.flush_terminal();
        self.log_progress(&run, "completion", true);
        self.last_progress_log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&run.id);
        run
    }

    /// A policy change superseded a run: queue its scopes again under the
    /// new policy, so a root whose scan was cut short (adding another root
    /// changes the policy) still gets scanned. Roots that are gone or now
    /// excluded drop out.
    fn requeue_superseded(&self, run: &ScanRun) {
        let scopes = match self.store.get_run(&run.id) {
            Ok((_, scopes, _)) => scopes,
            Err(error) => {
                tracing::warn!(run_id = %run.id, %error, "superseded scan scopes unreadable");
                return;
            }
        };
        let registry = self.registry();
        let scopes: Vec<ScanScope> = scopes
            .into_iter()
            .filter_map(|mut scope| {
                let root = registry.resolve(&scope.root_id)?;
                if root.policy == super::models::EffectivePolicy::Excluded {
                    return None;
                }
                scope.policy_revision = registry.policy_revision().to_owned();
                scope.effective_policy = root.policy;
                Some(scope)
            })
            .collect();
        if scopes.is_empty() {
            return;
        }
        let request = ScanRequest {
            kind: run.kind,
            trigger: run.trigger,
            scopes,
            requested_by_user_id: run.requested_by_user_id.clone(),
            policy_revision: registry.policy_revision().to_owned(),
        };
        match self.request_run(&request) {
            Ok(result) => tracing::info!(
                superseded = %run.id,
                run_id = %result.run_id,
                disposition = ?result.disposition,
                "superseded scan queued again under the new policy"
            ),
            Err(error) => {
                tracing::warn!(superseded = %run.id, %error, "superseded scan not queued again");
            }
        }
    }

    /// Synthetic failed run for a run that cannot be read while settling.
    fn missing_run(&self, run_id: &str, code: FailureCode) -> ScanRun {
        ScanRun {
            id: run_id.to_owned(),
            kind: ScanKind::Incremental,
            trigger: super::models::ScanTrigger::Manual,
            state: RunState::Ended {
                outcome: Outcome::Failed,
                reached: Some(Phase::Discovering),
            },
            requested_by_user_id: None,
            aggregate_scope: "all".to_owned(),
            queued_at: 0.0,
            started_at: None,
            updated_at: self.now(),
            terminal_at: Some(self.now()),
            terminal_code: Some(code),
            coalesced_request_count: 0,
            row_revision: 0,
            event_revision: 0,
            counters: Counters::default(),
            phase_timings: Default::default(),
        }
    }

    /// Fail an active run (v2 `run_once` crash path). Its writes wait out
    /// a busy database.
    async fn fail_active(&self, run: &ScanRun, code: FailureCode) -> ScanRun {
        let (current, _, _) = match self.store.reread_run(&RunStop::never(), &run.id) {
            Ok(found) => found,
            Err(error) => {
                tracing::error!(run_id = %run.id, %error, "scan run unreadable; not failed");
                return run.clone();
            }
        };
        if current.state.is_settling() {
            return self.settle_pending_control(&current.id);
        }
        if current.state.running().is_none() {
            return current;
        }
        match self.store.transition(
            &RunStop::never(),
            &current.id,
            current.row_revision,
            Event::Fail,
            self.now(),
            Some(code),
        ) {
            Ok(failed) => {
                self.publish(&failed, "scan.transition", false);
                self.flush_terminal();
                failed
            }
            Err(error) => {
                tracing::error!(run_id = %current.id, %error, "scan run not failed");
                current
            }
        }
    }

    /// Run the end passes ended runs still owe, on a blocking thread. A
    /// failure is logged; the next drive resumes where it stopped.
    async fn settle_ended_runs(&self, stop: &RunStop) {
        let settled = self
            .on_store(stop, |store, stop| store.settle_ended_runs(stop))
            .await;
        match settled {
            Ok(()) | Err(ScanStoreError::Stopped | ScanStoreError::ReadCancelled { .. }) => {}
            Err(error) => tracing::warn!(%error, "scan end passes did not finish"),
        }
    }

    /// Clear finished runs' inventory and prune history, on a blocking
    /// thread. A failure is logged; the next drive carries on from the
    /// rows still there.
    async fn cleanup_terminal_inventory(&self, stop: &RunStop) {
        let cleaned = self
            .on_store(stop, |store, stop| store.cleanup_terminal_inventory(stop))
            .await;
        match cleaned {
            Ok(()) | Err(ScanStoreError::Stopped | ScanStoreError::ReadCancelled { .. }) => {}
            Err(error) => tracing::warn!(%error, "scan inventory cleanup did not finish"),
        }
    }

    /// Run one store call on a blocking thread so SQLite never stalls an
    /// async worker.
    async fn on_store<R: Send + 'static>(
        &self,
        stop: &RunStop,
        op: impl FnOnce(&SqliteScanStore, &RunStop) -> Result<R, ScanStoreError> + Send + 'static,
    ) -> Result<R, ScanStoreError> {
        let store = Arc::clone(&self.store);
        let stop = stop.clone();
        tokio::task::spawn_blocking(move || op(&store, &stop))
            .await
            .map_err(|error| ScanStoreError::Internal {
                message: format!("scan store task failed: {error}"),
            })?
    }

    /// Indexing phase (v2 `LibraryIndexer.index`): page through the
    /// unprocessed inventory, read tags for new and changed files with
    /// bounded concurrency, and commit each window of rows atomically.
    /// A pause, stop, or shutdown between windows loses nothing: the rows
    /// not yet committed are still pending and the next drive resumes
    /// there.
    async fn index(
        &self,
        run: &ScanRun,
        frozen_policy_revision: &str,
        stop: &RunStop,
    ) -> Result<IndexCounts, Halt> {
        let mut counts = IndexCounts::default();
        let mut after: Option<(String, String)> = None;
        loop {
            let cursor = after.clone();
            let run_id = run.id.clone();
            // A page that will not read is a store error, not end-of-run:
            // the run fails instead of completing short.
            let (page, _) = self
                .on_store(stop, move |store, stop| {
                    let after = cursor
                        .as_ref()
                        .map(|(root, path)| (root.as_str(), path.as_str()));
                    store.inventory_page(stop, &run_id, after, INDEX_PAGE_SIZE)
                })
                .await?;
            if page.is_empty() {
                break;
            }
            let short_page = page.len() < INDEX_PAGE_SIZE;
            // Unchanged files whose edition hints no scan stored (files
            // carried over from v2) are read for those hints alone.
            let unchanged: Vec<String> = if run.kind == ScanKind::Incremental {
                page.iter()
                    .filter(|item| {
                        item.comparison_result == Verdict::Unchanged
                            && item.effective_policy != super::models::EffectivePolicy::Excluded
                    })
                    .filter_map(|item| item.local_track_id.clone())
                    .collect()
            } else {
                Vec::new()
            };
            let unread = self
                .on_store(stop, move |store, stop| {
                    store.without_edition_tags(stop, &unchanged)
                })
                .await?;
            for rows in page.chunks(INDEX_FLUSH_EVERY) {
                let Some(window) = self
                    .read_window(
                        run,
                        rows,
                        after.clone(),
                        frozen_policy_revision,
                        &unread,
                        &mut counts,
                    )
                    .await
                else {
                    return Ok(counts);
                };
                let through = window.through.clone();
                let failures = window_failures(&window, self.now());
                if !failures.is_empty() {
                    let run_id = run.id.clone();
                    self.on_store(stop, move |store, stop| {
                        store.record_failures(stop, &run_id, &failures)
                    })
                    .await?;
                }
                let outcome = self
                    .on_store(stop, move |store, stop| store.commit_window(stop, &window))
                    .await?;
                // Files the catalog refused are already on the run's
                // failure list, written with the window.
                counts.identification_enqueued += outcome.enqueued;
                counts.indexed = counts.indexed.saturating_sub(outcome.failed.len());
                counts.errored += outcome.failed.len();
                after = Some(through);
                let (fresh, _, _) = self.store.reread_run(stop, &run.id)?;
                self.publish(&fresh, "scan.progress", true);
                self.log_progress(&fresh, "progress", false);
            }
            if short_page {
                break;
            }
        }
        Ok(counts)
    }

    /// Read the tags of a few files at once; the pool caps the reads at
    /// its worker count.
    async fn read_files(
        &self,
        items: &[&ScanInventoryItem],
    ) -> Vec<Result<ScannedTags, TagReadError>> {
        let reads = items.iter().map(|item| {
            let tags = Arc::clone(&self.tags);
            let path = PathBuf::from(&item.absolute_path);
            let pool = self.pool.clone();
            async move {
                pool.run(move || tags.read_tags(&path))
                    .await
                    .unwrap_or(Err(TagReadError::Fatal))
            }
        });
        futures_util::future::join_all(reads).await
    }

    /// Classify one window of inventory rows and read the tags the
    /// catalog needs: in full for new and changed files, and the edition
    /// hints alone for unchanged tracks in `unread`. `None` when the
    /// checkpoint says stop.
    async fn read_window(
        &self,
        run: &ScanRun,
        rows: &[ScanInventoryItem],
        after: Option<(String, String)>,
        frozen_policy_revision: &str,
        unread: &HashSet<String>,
        counts: &mut IndexCounts,
    ) -> Option<IndexWindow> {
        let last = rows.last()?;
        let mut window = IndexWindow {
            run_id: run.id.clone(),
            after,
            through: (last.root_id.clone(), last.relative_path.clone()),
            now: self.now(),
            ..IndexWindow::default()
        };
        let force = run.kind == ScanKind::RescanFiles;
        let mut to_read: Vec<&ScanInventoryItem> = Vec::new();
        let mut to_fill: Vec<&ScanInventoryItem> = Vec::new();
        for item in rows {
            let row = (item.root_id.clone(), item.relative_path.clone());
            window.counters.push((row.clone(), Counter::Inspected, 1));
            if item.effective_policy == super::models::EffectivePolicy::Excluded
                || item.comparison_result == Verdict::Excluded
            {
                window.counters.push((row, Counter::Excluded, 1));
                counts.excluded += 1;
            } else if run.kind == ScanKind::PolicyReconcile
                || (!force && item.comparison_result == Verdict::Unchanged)
            {
                // Reconcile-only runs refresh inventory, never tags.
                window.counters.push((row, Counter::Unchanged, 1));
                counts.unchanged += 1;
                if item
                    .local_track_id
                    .as_ref()
                    .is_some_and(|id| unread.contains(id))
                {
                    to_fill.push(item);
                }
            } else if item.comparison_result != Verdict::CandidateMissing {
                to_read.push(item);
            }
        }
        for chunk in to_read.chunks(INDEX_CHECKPOINT_EVERY) {
            if !self.check(&run.id, frozen_policy_revision) {
                return None;
            }
            let outcomes = self.read_files(chunk).await;
            for (item, outcome) in chunk.iter().zip(outcomes) {
                let row = (item.root_id.clone(), item.relative_path.clone());
                match outcome {
                    Err(TagReadError::Deferred) => {
                        // The deferred marker re-offers the file next run.
                        self.store
                            .mark_deferred(&item.root_id, &item.relative_path, true);
                        window
                            .failed
                            .push((row.0.clone(), row.1.clone(), "deferred"));
                        window.counters.push((row, Counter::Errored, 1));
                        counts.errored += 1;
                    }
                    Err(TagReadError::Fatal) => {
                        window.failed.push((row.0.clone(), row.1.clone(), "failed"));
                        window.counters.push((row, Counter::Errored, 1));
                        counts.errored += 1;
                    }
                    Ok(tags) => {
                        let verdict_counter = match item.comparison_result {
                            Verdict::New => {
                                counts.new += 1;
                                Counter::New
                            }
                            Verdict::Changed => {
                                counts.changed += 1;
                                Counter::Changed
                            }
                            // A rescan_files re-read of an unchanged row
                            // lands indexed but keeps its verdict.
                            _ => {
                                counts.unchanged += 1;
                                Counter::Unchanged
                            }
                        };
                        window.counters.push((row.clone(), Counter::Indexed, 1));
                        window.counters.push((row, verdict_counter, 1));
                        counts.indexed += 1;
                        window.items.push(CommitIndexedItem {
                            root_id: item.root_id.clone(),
                            relative_path: item.relative_path.clone(),
                            absolute_path: item.absolute_path.clone(),
                            size_bytes: item.file_size_bytes,
                            mtime_ns: item.file_mtime_ns,
                            tags_read_at: self.now(),
                            tags,
                            effective_policy: item.effective_policy,
                            policy_revision: item.policy_revision.clone(),
                            verdict_counter: verdict_counter.column(),
                        });
                    }
                }
            }
        }
        for chunk in to_fill.chunks(INDEX_CHECKPOINT_EVERY) {
            if !self.check(&run.id, frozen_policy_revision) {
                return None;
            }
            let outcomes = self.read_files(chunk).await;
            for (item, outcome) in chunk.iter().zip(outcomes) {
                let Some(track_id) = item.local_track_id.clone() else {
                    continue;
                };
                // A file that cannot be read gets an empty row, so it is not
                // tried on every scan; changing it brings a full read. A busy
                // file is tried again next scan.
                let (edition, disc_number) = match outcome {
                    Ok(tags) => (tags.tag.edition, tags.tag.disc_number),
                    Err(TagReadError::Fatal) => {
                        tracing::warn!(
                            path = item.relative_path,
                            "could not read an unchanged file's edition tags"
                        );
                        Default::default()
                    }
                    Err(TagReadError::Deferred) => {
                        tracing::debug!(
                            path = item.relative_path,
                            "edition tags of an unchanged file wait for the next scan"
                        );
                        continue;
                    }
                };
                window.edition_fills.push(EditionFill {
                    root_id: item.root_id.clone(),
                    relative_path: item.relative_path.clone(),
                    track_id,
                    stat_revision: item.stat_revision.clone(),
                    edition,
                    disc_number,
                    effective_policy: item.effective_policy,
                });
            }
        }
        Some(window)
    }

    /// Reconciliation phase (v2 `LibraryReconciler`): catalog rows under a
    /// cleanly walked scope that the walk did not see are marked missing,
    /// never deleted. Only completed scopes qualify. A scope that would
    /// lose most of its tracks at once (an unmounted share looks exactly
    /// like that) is held back with a failure row instead.
    async fn reconcile(
        &self,
        run_id: &str,
        scopes: &[ScanScope],
        frozen_policy_revision: &str,
        stop: &RunStop,
    ) -> Result<usize, Halt> {
        let mut missing = 0usize;
        for scope in scopes {
            if !self.check(run_id, frozen_policy_revision) {
                return Ok(missing);
            }
            if self.store.scope_discovery_state(
                stop,
                run_id,
                &scope.root_id,
                &scope.relative_path,
            )? != ScopeDiscoveryState::Completed
                || self
                    .store
                    .scope_guarded(stop, run_id, &scope.root_id, &scope.relative_path)?
            {
                continue;
            }
            let (id, root, rel) = (
                run_id.to_owned(),
                scope.root_id.clone(),
                scope.relative_path.clone(),
            );
            let paths = self
                .on_store(stop, move |store, stop| {
                    store.missing_catalog_paths(stop, &id, &root, &rel)
                })
                .await?;
            if paths.is_empty() {
                continue;
            }
            let indexed = self
                .store
                .indexed_count(stop, &scope.root_id, &scope.relative_path)?;
            if mass_missing(paths.len(), indexed) {
                self.hold_back_scope(
                    run_id,
                    scope,
                    paths.len(),
                    indexed,
                    ScanPhase::Reconciling,
                    stop,
                )?;
                continue;
            }
            // Nothing under a folder this run held back is marked.
            let paths: Vec<String> = paths
                .into_iter()
                .filter_map(|(path, held)| (!held).then_some(path))
                .collect();
            for chunk in paths.chunks(RECONCILE_CHUNK) {
                if !self.check(run_id, frozen_policy_revision) {
                    return Ok(missing);
                }
                let (id, root, chunk, now) = (
                    run_id.to_owned(),
                    scope.root_id.clone(),
                    chunk.to_vec(),
                    self.now(),
                );
                missing += self
                    .on_store(stop, move |store, stop| {
                        store.mark_missing(stop, &id, &root, &chunk, now)
                    })
                    .await?;
            }
        }
        Ok(missing)
    }
}

impl<T: TagReader + 'static> LibraryScanCoordinator<T> {
    /// Before indexing, hold back every walked scope that lost most of its
    /// tracks at once, and in a scope not held back, every drive or share
    /// mounted inside it that the walk found empty while it has indexed
    /// tracks. A held-back folder counts as not walked for the whole run,
    /// so an unmounted share neither marks its tracks missing nor hands
    /// their ids and albums to copies found elsewhere.
    async fn guard_vanished_scopes(&self, run_id: &str, stop: &RunStop) -> Result<(), Halt> {
        let scopes = self.store.reread_run(stop, run_id)?.1;
        for scope in &scopes {
            if self.store.scope_discovery_state(
                stop,
                run_id,
                &scope.root_id,
                &scope.relative_path,
            )? != ScopeDiscoveryState::Completed
            {
                continue;
            }
            let (id, root, rel) = (
                run_id.to_owned(),
                scope.root_id.clone(),
                scope.relative_path.clone(),
            );
            let (missing, total) = self
                .on_store(stop, move |store, stop| {
                    store.vanish_counts(stop, &id, &root, &rel)
                })
                .await?;
            if mass_missing(missing, total) {
                self.hold_back_scope(run_id, scope, missing, total, ScanPhase::Discovering, stop)?;
                continue;
            }
            let (id, root, rel) = (
                run_id.to_owned(),
                scope.root_id.clone(),
                scope.relative_path.clone(),
            );
            let mounts = self
                .on_store(stop, move |store, stop| {
                    store.empty_mounts(stop, &id, &root, &rel)
                })
                .await?;
            for (folder, indexed) in mounts {
                tracing::warn!(
                    root_id = scope.root_id,
                    folder,
                    indexed,
                    "a drive or share inside a library folder was found empty; \
                     missing detection held back"
                );
                self.hold_back(
                    run_id,
                    &scope.root_id,
                    &folder,
                    ScanPhase::Discovering,
                    format!(
                        "None of the {indexed} indexed files on the drive or share \
                         mounted at this folder were found. Check that it is mounted; \
                         nothing under it was marked missing."
                    ),
                    stop,
                )?;
            }
        }
        Ok(())
    }

    /// Record that missing detection is held back for one scope.
    fn hold_back_scope(
        &self,
        run_id: &str,
        scope: &ScanScope,
        missing: usize,
        indexed: usize,
        phase: ScanPhase,
        stop: &RunStop,
    ) -> Result<(), ScanStoreError> {
        tracing::warn!(
            root_id = scope.root_id,
            scope = scope.relative_path,
            missing,
            indexed,
            "most of a scope vanished at once; missing detection held back"
        );
        self.hold_back(
            run_id,
            &scope.root_id,
            &scope.relative_path,
            phase,
            format!(
                "{missing} of {indexed} indexed files under this scope were not found. \
                 Check that the library is mounted; nothing was marked missing."
            ),
            stop,
        )
    }

    /// Record that missing detection is held back for one folder: a scope,
    /// or a mount inside one.
    fn hold_back(
        &self,
        run_id: &str,
        root_id: &str,
        folder: &str,
        phase: ScanPhase,
        failure_detail: String,
        stop: &RunStop,
    ) -> Result<(), ScanStoreError> {
        self.store.record_failures(
            stop,
            run_id,
            &[ScanFailureRecord {
                root_id: root_id.to_owned(),
                relative_path: folder.to_owned(),
                failure_code: FailureCode::MassMissingGuard,
                recorded_at: self.now(),
                failure_detail,
                phase,
            }],
        )
    }
}

/// Missing rows marked per transaction during reconcile.
const RECONCILE_CHUNK: usize = 500;

/// Scopes at or above this many indexed tracks are guarded against losing
/// most of them in one walk.
const MASS_MISSING_MIN_INDEXED: usize = 20;

/// True when a walk would mark so much of a scope missing that a vanished
/// mount is the likelier story: a walk that found none of the scope's
/// files at all (an unmounted share or an empty mount point, whatever its
/// size, as v2 protects), or more than half of a larger scope.
fn mass_missing(missing: usize, indexed: usize) -> bool {
    let nothing_left = missing > 0 && missing == indexed;
    nothing_left || (indexed >= MASS_MISSING_MIN_INDEXED && missing * 2 > indexed)
}

/// Failure rows for the files a window could not read.
fn window_failures(window: &IndexWindow, now: f64) -> Vec<ScanFailureRecord> {
    window
        .failed
        .iter()
        .map(|(root_id, relative_path, state)| {
            let (code, detail) = if *state == "deferred" {
                (
                    FailureCode::TagReadDeferred,
                    "The tag-read capacity was exhausted; the read is deferred for this run.",
                )
            } else {
                (
                    FailureCode::TagReadFailed,
                    "The tag read failed; the file was skipped for this run.",
                )
            };
            ScanFailureRecord {
                root_id: root_id.clone(),
                relative_path: relative_path.clone(),
                failure_code: code,
                recorded_at: now,
                failure_detail: detail.to_owned(),
                phase: ScanPhase::Indexing,
            }
        })
        .collect()
}

impl<T: TagReader + 'static> Checkpoint for LibraryScanCoordinator<T> {
    /// v2 `checkpoint`: fast true while policy matches and no control is
    /// pending, otherwise settle or supersede through the store. A run
    /// that cannot be read answers false, logged: the drive ends and the
    /// run keeps its state.
    fn check(&self, run_id: &str, frozen_policy_revision: &str) -> bool {
        let current_policy_revision = self.resolvers.resolver().policy_revision().to_owned();
        if current_policy_revision == frozen_policy_revision
            && !self
                .pending_control
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(run_id)
        {
            return true;
        }
        let (run, _, _) = match self.store.reread_run(&RunStop::never(), run_id) {
            Ok(found) => found,
            Err(error) => {
                tracing::error!(run_id, %error, "scan run unreadable at a checkpoint");
                return false;
            }
        };
        if current_policy_revision != frozen_policy_revision {
            if matches!(run.state, RunState::Stopping(_)) {
                self.settle_pending_control(&run.id);
                return false;
            }
            if super::models::transition(run.state, Event::Supersede).is_ok() {
                match self.store.transition(
                    &RunStop::never(),
                    &run.id,
                    run.row_revision,
                    Event::Supersede,
                    self.now(),
                    Some(FailureCode::SupersededPolicyChanged),
                ) {
                    Ok(_) => self.requeue_superseded(&run),
                    Err(error) => {
                        tracing::warn!(run_id = %run.id, %error, "superseded scan did not settle");
                    }
                }
            }
            self.flush_terminal();
            self.pending_control
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&run.id);
            return false;
        }
        if run.state.is_settling() {
            self.settle_pending_control(&run.id);
            return false;
        }
        self.pending_control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&run.id);
        run.state.running().is_some()
    }
}

/// Index-phase counts for one run.
#[derive(Debug, Default, Clone, Copy)]
pub struct IndexCounts {
    pub indexed: usize,
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub excluded: usize,
    pub errored: usize,
    pub identification_enqueued: usize,
}

#[cfg(test)]
mod tests {
    use super::super::models::ScanTrigger;
    use super::super::roots::LibraryRoot;
    use super::super::seams::NullTagReader;
    use super::super::sqlite_store::scratch_store;
    use super::*;
    use std::path::PathBuf;

    fn coordinator() -> (
        droppedneedle_kernel::db::scratch::ScratchDir,
        LibraryScanCoordinator<NullTagReader>,
    ) {
        let registry = RootRegistry::new(
            vec![LibraryRoot::new(
                "r1",
                PathBuf::from("/music"),
                super::super::models::EffectivePolicy::Automatic,
            )],
            true,
            "rev-1",
        );
        let (dir, store) = scratch_store();
        let coordinator = LibraryScanCoordinator::new(
            Arc::new(store),
            BlockingPool::new(2),
            Arc::new(NullTagReader::new()),
            Arc::new(StaticResolver::new(registry)),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        );
        (dir, coordinator)
    }

    fn request() -> ScanRequest {
        ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes: vec![ScanScope::root("r1", "/music", "rev-1")],
            requested_by_user_id: None,
            policy_revision: "rev-1".to_owned(),
        }
    }

    #[test]
    fn request_validations_match_v2() {
        let (_dir, coordinator) = coordinator();
        assert!(coordinator.request_run(&request()).is_ok());

        let mut empty = request();
        empty.scopes.clear();
        assert_eq!(
            coordinator.request_run(&empty),
            Err(ScanRequestError::EmptyScopes)
        );

        let mut stale = request();
        stale.scopes[0].policy_revision = "rev-2".to_owned();
        assert_eq!(
            coordinator.request_run(&stale),
            Err(ScanRequestError::StalePolicy)
        );

        let mut unknown = request();
        unknown.scopes[0].root_id = "nope".to_owned();
        assert_eq!(
            coordinator.request_run(&unknown),
            Err(ScanRequestError::UnknownRoots)
        );
    }

    #[test]
    fn history_cursor_pages() {
        let (_dir, coordinator) = coordinator();
        let result = coordinator.request_run(&request()).expect("requested");
        let (run, _, _) = coordinator.store().get_run(&result.run_id).expect("run");
        coordinator
            .store()
            .request_control(&run.id, Event::Stop, run.row_revision, 9.0)
            .expect("stopped");
        let (items, next) = coordinator.history_page(50, None).expect("page");
        assert_eq!(items.len(), 1);
        assert_eq!(next, None);
        let cursor = format!("{}:{}", items[0].terminal_at.unwrap_or(0.0), items[0].id);
        let (items, _) = coordinator.history_page(50, Some(&cursor)).expect("page 2");
        assert!(items.is_empty());
        assert!(coordinator.history_page(50, Some("bogus")).is_err());
    }
}
