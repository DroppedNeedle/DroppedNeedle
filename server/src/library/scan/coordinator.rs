//! Single durable entry point for every scan trigger.
//!
//! Port of v2's library scan coordinator plus the indexing and
//! reconciliation phases (v2's library indexer and reconciler, reduced to
//! what scanning needs: tag reads ride
//! the [`TagReader`](super::seams::TagReader) seam, identify offers ride
//! the [`IdentifyQueue`](super::seams::IdentifyQueue) seam, and grouping
//! collapses to parent-directory album keys until identify owns real
//! grouping).
//!
//! Pipeline per run: discover (walk) -> index (tag + catalog) ->
//! reconcile (missing detection) -> completed. Control (pause/stop) and
//! policy supersede flow through [`Checkpoint`], which the coordinator
//! implements over live store state.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use super::fs::FsCoordinator;
use super::models::{
    Counters, Disposition, RequestedControl, ScanControl, ScanFailureRecord, ScanKind, ScanPhase,
    ScanRequest, ScanRequestResult, ScanRun, ScanScope, ScanState, ScopeDiscoveryState, Verdict,
    counter_names, failure_codes,
};
use super::pool::BlockingPool;
use super::roots::{PolicyResolver, RootRegistry};
use super::seams::{Checkpoint, IdentifyQueue, TagReadError, TagReader};
use super::store::{CommitIndexedItem, InventoryPage, ScanStore, ScanStoreError};
use super::walk::InventoryScanner;
use super::watcher::WorkWakeups;

/// Counter deltas held between checkpoints. Per-file `add_counter` calls
/// cost one commit each on SQLite; buffering them and flushing one
/// `add_counters` per checkpoint lands identical totals with one commit
/// per sixteen files. Every early return in the index and reconcile loops
/// flushes first, so progress is never lost to a pause, stop, or supersede.
struct CounterBuffer {
    deltas: Vec<(&'static str, i64)>,
}

impl CounterBuffer {
    fn new() -> Self {
        Self { deltas: Vec::new() }
    }

    fn add(&mut self, name: &'static str, delta: i64) {
        self.deltas.push((name, delta));
    }

    fn flush<S: ScanStore>(&mut self, store: &S, run_id: &str) {
        if self.deltas.is_empty() {
            return;
        }
        store.add_counters(run_id, &self.deltas);
        self.deltas.clear();
    }
}

/// Return freed scan heap to the OS after a terminal run. A 100k scan
/// churns hundreds of megabytes of transient buffers (inventory pages,
/// verdict maps, commit rows); the allocator keeps the freed pages mapped,
/// so post-scan idle RSS holds the whole peak without this release.
fn release_scan_memory() {
    #[cfg(target_family = "unix")]
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
/// Inventory page read attempts before the run fails. Each
/// attempt already rides the store's 5 s busy timeout; the retries only
/// cover flakes between attempts, with the store lock released.
pub const INDEX_PAGE_READ_ATTEMPTS: u32 = 3;
/// Pause between page-read attempts; linear, no jitter needed for a
/// single scan worker.
pub const INDEX_PAGE_READ_RETRY: Duration = Duration::from_millis(25);
/// Bound on stale retries when settling control (v2
/// `SETTLE_STALE_MAX_RETRIES`).
pub const SETTLE_STALE_MAX_RETRIES: u32 = 10;
/// Fixed settle retry sleep. v2 jitters 50ms; without a rand dependency
/// this port keeps the bound and the base, not the jitter.
pub const SETTLE_STALE_RETRY_SECS: f64 = 0.05;
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
        } else if run.state.is_terminal() {
            last.remove(&run.id);
        }
        drop(last);
        (self.sink)(ScanEvent {
            id: format!("scan:{stream_revision}"),
            stream_kind: "scan".to_owned(),
            stream_revision,
            run_id: run.id.clone(),
            row_revision: run.row_revision,
            event_revision: run.event_revision,
            state: run.state,
            event: event.to_owned(),
        });
        true
    }
}

type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn system_clock() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn default_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Scan coordinator (v2 `LibraryScanCoordinator`).
pub struct LibraryScanCoordinator<S: ScanStore, T: TagReader, Q: IdentifyQueue> {
    store: Arc<S>,
    inventory: InventoryScanner<S>,
    tags: Arc<T>,
    identify: Arc<Q>,
    pool: BlockingPool,
    fs: Option<FsCoordinator>,
    resolvers: Arc<dyn ResolverSource>,
    events: Option<ScanEventPublisher>,
    wakeups: Option<WorkWakeups>,
    clock: Clock,
    idgen: Arc<dyn Fn() -> String + Send + Sync>,
    pending_control: Mutex<HashSet<String>>,
    last_progress_log: Mutex<HashMap<String, f64>>,
}

impl<S: ScanStore, T: TagReader + 'static, Q: IdentifyQueue> LibraryScanCoordinator<S, T, Q> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<S>,
        pool: BlockingPool,
        tags: Arc<T>,
        identify: Arc<Q>,
        resolvers: Arc<dyn ResolverSource>,
    ) -> Self {
        let inventory = InventoryScanner::new(Arc::clone(&store), pool.clone());
        Self {
            store,
            inventory,
            tags,
            identify,
            pool,
            fs: None,
            resolvers,
            events: None,
            wakeups: None,
            clock: Arc::new(system_clock),
            idgen: Arc::new(default_id),
            pending_control: Mutex::new(HashSet::new()),
            last_progress_log: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_filesystem(mut self, fs: FsCoordinator) -> Self {
        self.inventory = InventoryScanner::new(Arc::clone(&self.store), self.pool.clone())
            .with_filesystem(fs.clone());
        self.fs = Some(fs);
        self
    }

    pub fn with_events(mut self, events: ScanEventPublisher) -> Self {
        self.events = Some(events);
        self
    }

    pub fn with_wakeups(mut self, wakeups: WorkWakeups) -> Self {
        self.wakeups = Some(wakeups);
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_idgen(mut self, idgen: Arc<dyn Fn() -> String + Send + Sync>) -> Self {
        self.idgen = idgen;
        self
    }

    pub fn store(&self) -> &Arc<S> {
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
        (self.clock)()
    }

    fn publish(&self, run: &ScanRun, event: &str, counter: bool) {
        if let Some(events) = &self.events {
            events.publish(run, self.store.stream_revision("scan"), event, counter);
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
        let counter = |name: &str| run.counters.get(name).copied().unwrap_or(0);
        let total = counter(counter_names::TOTAL);
        let inspected = counter(counter_names::INSPECTED);
        let percentage = if run.state == ScanState::Completed {
            100.0
        } else if total > 0 {
            inspected as f64 * 100.0 / total as f64
        } else {
            0.0
        };
        tracing::info!(
            event,
            state = ?run.state,
            phase = ?run.phase,
            discovered = counter(counter_names::DISCOVERED),
            inspected,
            total,
            percentage,
            new = counter(counter_names::NEW),
            changed = counter(counter_names::CHANGED),
            unchanged = counter(counter_names::UNCHANGED),
            excluded = counter(counter_names::EXCLUDED),
            failed = counter(counter_names::ERRORED),
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
        let result = self.store.request_run(request, &run_id, self.now());
        if result.disposition != Disposition::Conflict {
            // v2 request_scan_run wakes the supervisor for every accepted
            // disposition; the store here has no wakeups, so the
            // coordinator wakes instead.
            if let Some(wakeups) = &self.wakeups {
                wakeups.notify("scan");
            }
            if let Ok((run, _, _)) = self.store.get_run(&result.run_id) {
                self.publish(&run, "scan.requested", false);
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

    pub fn current(&self) -> Vec<ScanRun> {
        self.store.list_current()
    }

    pub fn history(&self, limit: usize) -> Vec<ScanRun> {
        self.store.list_history(limit)
    }

    pub fn latest_filesystem_terminal(&self) -> Option<ScanRun> {
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
        let mut runs = self.store.list_history(limit + 1 + 50);
        if let Some((terminal_at, id)) = before {
            runs.retain(|run| {
                run.terminal_at
                    .map(|terminal| {
                        terminal < terminal_at || (terminal == terminal_at && run.id < id)
                    })
                    .unwrap_or(false)
            });
        }
        let mut items: Vec<ScanRun> = runs.into_iter().take(limit + 1).collect();
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

    pub fn scan_run_failures(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<ScanFailureRecord>, ScanStoreError> {
        self.store.get_run(run_id)?;
        Ok(self
            .store
            .failures(run_id)
            .into_iter()
            .take(limit.max(1))
            .collect())
    }

    /// Pause, resume, or stop a run (v2 `control`).
    pub fn control(
        &self,
        run_id: &str,
        control: ScanControl,
        resume: bool,
        expected_revision: u64,
    ) -> Result<(ScanRun, u64), ScanStoreError> {
        let (run, stream_revision) =
            self.store
                .request_control(run_id, control, resume, expected_revision, self.now())?;
        {
            let mut pending = self
                .pending_control
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if matches!(run.state, ScanState::Pausing | ScanState::Stopping) {
                pending.insert(run.id.clone());
            } else {
                pending.remove(&run.id);
            }
        }
        let event = if resume {
            "control_resume"
        } else if control == ScanControl::Pause {
            "control_pause"
        } else {
            "control_stop"
        };
        self.publish(&run, "scan.transition", false);
        if run.terminal_at.is_some() {
            self.store.flush_invalidation(true);
            if let Some(fs) = &self.fs {
                fs.forget_scan(&run.id);
            }
        }
        self.log_progress(&run, event, true);
        Ok((run, stream_revision))
    }

    /// Boot recovery (v2 `recover` / `recover_stopping`).
    pub fn recover(&self) -> Vec<ScanRun> {
        if !self.resolvers.resolver().enabled() {
            return self.recover_stopping();
        }
        let runs = self.store.recover(self.now());
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
        runs
    }

    pub fn recover_stopping(&self) -> Vec<ScanRun> {
        let runs = self.store.recover_stopping(self.now());
        let mut pending = self
            .pending_control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for run in &runs {
            pending.remove(&run.id);
            self.publish(run, "scan.transition", false);
            self.store.flush_invalidation(true);
            self.log_progress(run, "recovery", true);
            if let Some(fs) = &self.fs {
                fs.forget_scan(&run.id);
            }
        }
        runs
    }

    /// Settle a pausing/stopping run (v2 `_settle_pending_control`).
    fn settle_pending_control(&self, run_id: &str) -> ScanRun {
        let mut stale_retries = 0u32;
        loop {
            let (run, _, _) = match self.store.get_run(run_id) {
                Ok(found) => found,
                Err(_) => {
                    // Defensive: the run vanished mid-settle (impossible
                    // with the memory store). Answer failed, never panic.
                    self.pending_control
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(run_id);
                    return self.missing_run(run_id, failure_codes::UNEXPECTED_WORKER_FAILURE);
                }
            };
            if !matches!(run.state, ScanState::Pausing | ScanState::Stopping) {
                self.pending_control
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&run.id);
                // Every terminal state releases its revision entries,
                // including scanner-created failed.
                if matches!(
                    run.state,
                    ScanState::Completed
                        | ScanState::Cancelled
                        | ScanState::SupersededPolicyChanged
                        | ScanState::Failed
                ) && let Some(fs) = &self.fs
                {
                    fs.forget_scan(&run.id);
                }
                return run;
            }
            let new_state = if run.state == ScanState::Pausing {
                ScanState::Paused
            } else {
                ScanState::Cancelled
            };
            match self.store.transition(
                &run.id,
                run.state,
                run.row_revision,
                new_state,
                self.now(),
                None,
            ) {
                Ok(settled) => {
                    self.publish(&settled, "scan.transition", false);
                    self.store
                        .flush_invalidation(settled.state == ScanState::Cancelled);
                    self.log_progress(
                        &settled,
                        if settled.state == ScanState::Paused {
                            "pause"
                        } else {
                            "stop"
                        },
                        true,
                    );
                    self.pending_control
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&settled.id);
                    if settled.state == ScanState::Cancelled
                        && let Some(fs) = &self.fs
                    {
                        fs.forget_scan(&settled.id);
                    }
                    return settled;
                }
                Err(_) => {
                    // A racing writer keeps moving the revision.
                    // Bounded retries, then the stop-signal path: return
                    // unsettled WITHOUT discarding the pending entry, so
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
                    std::thread::sleep(std::time::Duration::from_secs_f64(SETTLE_STALE_RETRY_SECS));
                }
            }
        }
    }

    /// Request stop on every actively-driven run (shutdown path).
    /// Only discovering/indexing/reconciling runs are in flight; queued
    /// and paused runs are left alone so the next start resumes them.
    /// Best effort: a racing revision retries once, anything else logs.
    /// Returns runs stopped.
    pub fn stop_all_active(&self) -> usize {
        let mut stopped = 0;
        for run in self.current() {
            if !matches!(
                run.state,
                ScanState::Discovering | ScanState::Indexing | ScanState::Reconciling
            ) {
                continue;
            }
            let mut revision = run.row_revision;
            for _ in 0..2 {
                match self.control(&run.id, ScanControl::Stop, false, revision) {
                    Ok(_) => {
                        stopped += 1;
                        break;
                    }
                    Err(ScanStoreError::StaleRevision { .. }) => {
                        match self.store.get_run(&run.id) {
                            Ok((fresh, _, _)) => revision = fresh.row_revision,
                            Err(_) => break,
                        }
                    }
                    Err(error) => {
                        tracing::warn!(run_id = %run.id, %error, "scan shutdown stop request failed");
                        break;
                    }
                }
            }
        }
        stopped
    }

    /// Drive one run like [`run_once`](Self::run_once), but a signalled
    /// shutdown stops the in-flight run instead of waiting it out. The
    /// stop goes through the regular control latch, so the walk and
    /// index checkpoints settle the run to cancelled on their next
    /// check and the next start resumes cleanly. A pre-signalled
    /// shutdown claims no new work. Returns the settled run, or `None`
    /// when no work was available.
    pub async fn run_once_with_shutdown(
        &self,
        root_paths: &HashMap<String, PathBuf>,
        shutdown: &watch::Receiver<bool>,
    ) -> Option<ScanRun> {
        if *shutdown.borrow() {
            self.stop_all_active();
            return None;
        }
        let mut shutdown = shutdown.clone();
        tokio::pin! {
            let run = self.run_once(root_paths);
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                self.stop_all_active();
                run.await
            }
            outcome = &mut run => outcome,
        }
    }

    /// Drive one run to the next stopping point (v2 `run_once`). Returns
    /// the run, or `None` when no work was available.
    pub async fn run_once(&self, root_paths: &HashMap<String, PathBuf>) -> Option<ScanRun> {
        if !self.resolvers.resolver().enabled() {
            return None;
        }
        self.store.cleanup_terminal_inventory(5_000);
        let mut run = self.store.resumable();
        let newly_claimed = run.is_none();
        if run.is_none() {
            run = self.store.claim_next(self.now());
        }
        let run = run?;
        if newly_claimed {
            self.publish(&run, "scan.transition", false);
        }
        self.log_progress(&run, "start", true);
        let outcome = self.continue_run(&run, root_paths).await;
        self.pending_control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&run.id);
        if outcome.state.is_terminal() {
            // Post-terminal housekeeping, off the observed scan wall: fold
            // the WAL, then release the heap the scan churned.
            self.store.checkpoint_terminal();
            release_scan_memory();
        }
        Some(outcome)
    }

    async fn continue_run(&self, run: &ScanRun, root_paths: &HashMap<String, PathBuf>) -> ScanRun {
        let (mut run, scopes, _) = match self.store.get_run(&run.id) {
            Ok(found) => found,
            Err(_) => return run.clone(),
        };
        if scopes.is_empty() {
            // v2 indexes scopes[0] and the IndexError becomes
            // UNEXPECTED_WORKER_FAILURE; fail the same way directly.
            return self
                .fail_active(&run, failure_codes::UNEXPECTED_WORKER_FAILURE)
                .await;
        }
        let frozen_policy_revision = scopes[0].policy_revision.clone();
        if run.state == ScanState::Discovering {
            self.log_progress(&run, "phase_discovery_start", true);
            self.store.prepare_discovery_resume(&run.id);
            self.store.cleanup_stale_inventory(&run.id);
            (run, _, _) = match self.store.get_run(&run.id) {
                Ok(found) => (found.0, found.1, found.2),
                Err(_) => return run,
            };
            let scopes = self
                .store
                .get_run(&run.id)
                .map(|(_, scopes, _)| scopes)
                .unwrap_or_default();
            run = self
                .inventory
                .discover(&run, &scopes, root_paths, &self.resolvers.resolver(), self)
                .await;
            self.publish(&run, "scan.progress", true);
            (run, _, _) = match self.store.get_run(&run.id) {
                Ok(found) => (found.0, found.1, found.2),
                Err(_) => return run,
            };
            if run.state != ScanState::Discovering {
                return self.settle_pending_control(&run.id);
            }
            run = match self.store.finalize_discovery(&run.id, self.now()) {
                Ok(run) => run,
                Err(_) => {
                    return self
                        .fail_active(&run, failure_codes::UNEXPECTED_WORKER_FAILURE)
                        .await;
                }
            };
            self.log_progress(&run, "phase_discovery_end", true);
            run = match self.store.transition(
                &run.id,
                ScanState::Discovering,
                run.row_revision,
                ScanState::Indexing,
                self.now(),
                None,
            ) {
                Ok(run) => run,
                Err(_) => {
                    return self
                        .fail_active(&run, failure_codes::UNEXPECTED_WORKER_FAILURE)
                        .await;
                }
            };
            self.publish(&run, "scan.transition", false);
        }
        if run.state == ScanState::Indexing {
            self.log_progress(&run, "phase_indexing_start", true);
            if !self.check(&run.id, &frozen_policy_revision) {
                return self
                    .store
                    .get_run(&run.id)
                    .map(|(run, _, _)| run)
                    .unwrap_or(run);
            }
            let counts = self.index(&run, &frozen_policy_revision).await;
            if counts.identification_enqueued > 0
                && let Ok((fresh, _, _)) = self.store.get_run(&run.id)
            {
                self.publish(&fresh, "scan.progress", true);
            }
            (run, _, _) = match self.store.get_run(&run.id) {
                Ok(found) => (found.0, found.1, found.2),
                Err(_) => return run,
            };
            if run.state != ScanState::Indexing {
                return self.settle_pending_control(&run.id);
            }
            if !self.check(&run.id, &frozen_policy_revision) {
                return self
                    .store
                    .get_run(&run.id)
                    .map(|(run, _, _)| run)
                    .unwrap_or(run);
            }
            self.store.flush_invalidation(false);
            self.log_progress(&run, "phase_indexing_end", true);
            run = match self.store.transition(
                &run.id,
                ScanState::Indexing,
                run.row_revision,
                ScanState::Reconciling,
                self.now(),
                None,
            ) {
                Ok(run) => run,
                Err(_) => {
                    return self
                        .fail_active(&run, failure_codes::UNEXPECTED_WORKER_FAILURE)
                        .await;
                }
            };
            self.publish(&run, "scan.transition", false);
        }
        self.log_progress(&run, "phase_reconciliation_start", true);
        let scopes = self
            .store
            .get_run(&run.id)
            .map(|(_, scopes, _)| scopes)
            .unwrap_or_default();
        let missing = self.reconcile(&run.id, &scopes, &frozen_policy_revision);
        if missing > 0
            && let Ok((fresh, _, _)) = self.store.get_run(&run.id)
        {
            self.publish(&fresh, "scan.progress", true);
        }
        (run, _, _) = match self.store.get_run(&run.id) {
            Ok(found) => (found.0, found.1, found.2),
            Err(_) => return run,
        };
        if run.state != ScanState::Reconciling {
            return self.settle_pending_control(&run.id);
        }
        if !self.check(&run.id, &frozen_policy_revision) {
            return self
                .store
                .get_run(&run.id)
                .map(|(run, _, _)| run)
                .unwrap_or(run);
        }
        self.log_progress(&run, "phase_reconciliation_end", true);
        run = match self.store.transition(
            &run.id,
            ScanState::Reconciling,
            run.row_revision,
            ScanState::Completed,
            self.now(),
            None,
        ) {
            Ok(run) => run,
            Err(_) => {
                return self
                    .fail_active(&run, failure_codes::UNEXPECTED_WORKER_FAILURE)
                    .await;
            }
        };
        self.publish(&run, "scan.transition", false);
        self.store.flush_invalidation(true);
        self.log_progress(&run, "completion", true);
        self.last_progress_log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&run.id);
        if let Some(fs) = &self.fs {
            fs.forget_scan(&run.id);
        }
        run
    }

    /// Synthetic failed run for the impossible missing-run path.
    fn missing_run(&self, run_id: &str, code: &str) -> ScanRun {
        ScanRun {
            id: run_id.to_owned(),
            kind: ScanKind::Incremental,
            trigger: super::models::ScanTrigger::Manual,
            state: ScanState::Failed,
            phase: ScanPhase::Discovering,
            requested_by_user_id: None,
            aggregate_scope: "all".to_owned(),
            queued_at: 0.0,
            started_at: None,
            updated_at: self.now(),
            terminal_at: Some(self.now()),
            resume_phase: None,
            requested_control: RequestedControl::None,
            terminal_code: Some(code.to_owned()),
            coalesced_request_count: 0,
            row_revision: 0,
            event_revision: 0,
            counters: HashMap::new(),
            phase_timings: HashMap::new(),
        }
    }

    /// Fail an active run (v2 `run_once` crash path).
    async fn fail_active(&self, run: &ScanRun, code: &str) -> ScanRun {
        let (current, _, _) = match self.store.get_run(&run.id) {
            Ok(found) => found,
            Err(_) => return run.clone(),
        };
        if matches!(current.state, ScanState::Pausing | ScanState::Stopping) {
            return self.settle_pending_control(&current.id);
        }
        if !matches!(
            current.state,
            ScanState::Discovering | ScanState::Indexing | ScanState::Reconciling
        ) {
            return current;
        }
        match self.store.transition(
            &current.id,
            current.state,
            current.row_revision,
            ScanState::Failed,
            self.now(),
            Some(code),
        ) {
            Ok(failed) => {
                self.publish(&failed, "scan.transition", false);
                self.store.flush_invalidation(true);
                if let Some(fs) = &self.fs {
                    fs.forget_scan(&failed.id);
                }
                failed
            }
            Err(_) => current,
        }
    }

    /// One inventory page with bounded read retries. The store lock is
    /// released between attempts; only a persistent read error returns
    /// `Err`, and the caller fails the run on it.
    async fn read_inventory_page(
        &self,
        run_id: &str,
        after: Option<(&str, &str)>,
    ) -> Result<InventoryPage, ScanStoreError> {
        let mut attempt = 0u32;
        loop {
            match self.store.inventory_page(run_id, after, INDEX_PAGE_SIZE) {
                Ok(page) => return Ok(page),
                Err(_) if attempt + 1 < INDEX_PAGE_READ_ATTEMPTS => {
                    attempt += 1;
                    tokio::time::sleep(INDEX_PAGE_READ_RETRY * attempt).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Indexing phase: tag-read new/changed rows, commit catalog rows,
    /// offer album keys to identify (v2 `LibraryIndexer.index`, reduced).
    /// The inventory streams in bounded pages; catalog commits and counter
    /// deltas buffer between checkpoints and flush together, so a 100k run
    /// pays hundreds of commits instead of hundreds of thousands.
    async fn index(&self, run: &ScanRun, frozen_policy_revision: &str) -> IndexCounts {
        let mut counts = IndexCounts::default();
        let mut counters = CounterBuffer::new();
        let mut commits: Vec<CommitIndexedItem> = Vec::new();
        // Album grouping collapses to parent directories until identify
        // owns real grouping; one offer per directory.
        let mut album_tracks: HashMap<String, Vec<String>> = HashMap::new();
        let mut position = 0usize;
        let mut after: Option<(String, String)> = None;
        loop {
            let after_ref = after
                .as_ref()
                .map(|(root, path)| (root.as_str(), path.as_str()));
            let (page, cursor) = match self.read_inventory_page(&run.id, after_ref).await {
                Ok(page) => page,
                // A page that will not read is a persistent store error,
                // not end-of-run: flush what landed and fail the run
                // instead of completing it short.
                Err(error) => {
                    tracing::error!(%error, "scan inventory page read failed");
                    self.store.commit_indexed_batch(&commits);
                    counters.flush(self.store.as_ref(), &run.id);
                    self.fail_active(run, failure_codes::UNEXPECTED_WORKER_FAILURE)
                        .await;
                    return counts;
                }
            };
            if page.is_empty() {
                break;
            }
            let short_page = page.len() < INDEX_PAGE_SIZE;
            after = cursor;
            for item in &page {
                if position % INDEX_CHECKPOINT_EVERY == 0 && position > 0 {
                    if !self.check(&run.id, frozen_policy_revision) {
                        self.store.commit_indexed_batch(&commits);
                        counters.flush(self.store.as_ref(), &run.id);
                        return counts;
                    }
                    if position % INDEX_FLUSH_EVERY == 0 {
                        self.store.commit_indexed_batch(&commits);
                        commits.clear();
                        counters.flush(self.store.as_ref(), &run.id);
                    }
                }
                position += 1;
                counters.add(counter_names::INSPECTED, 1);
                if item.effective_policy == super::models::EffectivePolicy::Excluded
                    || item.comparison_result == Verdict::Excluded
                {
                    counters.add(counter_names::EXCLUDED, 1);
                    counts.excluded += 1;
                    continue;
                }
                if run.kind == ScanKind::PolicyReconcile {
                    // Reconcile-only: inventory refreshes, tags are untouched.
                    counters.add(counter_names::UNCHANGED, 1);
                    counts.unchanged += 1;
                    continue;
                }
                let force = run.kind == ScanKind::RescanFiles;
                if !force && item.comparison_result == Verdict::Unchanged {
                    counters.add(counter_names::UNCHANGED, 1);
                    counts.unchanged += 1;
                    continue;
                }
                if item.comparison_result == Verdict::CandidateMissing {
                    continue;
                }
                // Tag reads ride the pool so slow disks never stall the worker.
                let tags = Arc::clone(&self.tags);
                let path = PathBuf::from(&item.absolute_path);
                let outcome = self.pool.run(move || tags.read_tags(&path)).await;
                match outcome {
                    Err(TagReadError::Deferred) => {
                        // The persisted marker re-offers the file next
                        // run. v2 still counts the row errored this run.
                        self.store.record_failures(
                            &run.id,
                            vec![ScanFailureRecord {
                                root_id: item.root_id.clone(),
                                relative_path: item.relative_path.clone(),
                                failure_code: failure_codes::TAG_READ_DEFERRED.to_owned(),
                                recorded_at: self.now(),
                                failure_detail: "The tag-read capacity was exhausted; the read is deferred for this run.".to_owned(),
                                phase: ScanPhase::Indexing,
                            }],
                        );
                        self.store
                            .mark_deferred(&item.root_id, &item.relative_path, true);
                        counters.add(counter_names::ERRORED, 1);
                        counts.errored += 1;
                    }
                    Err(TagReadError::Fatal) => {
                        self.store.record_failures(
                            &run.id,
                            vec![ScanFailureRecord {
                                root_id: item.root_id.clone(),
                                relative_path: item.relative_path.clone(),
                                failure_code: failure_codes::TAG_READ_FAILED.to_owned(),
                                recorded_at: self.now(),
                                failure_detail:
                                    "The tag read failed; the file was skipped for this run."
                                        .to_owned(),
                                phase: ScanPhase::Indexing,
                            }],
                        );
                        counters.add(counter_names::ERRORED, 1);
                        counts.errored += 1;
                    }
                    Ok(_) => {
                        let track_id = item
                            .local_track_id
                            .clone()
                            .unwrap_or_else(|| (self.idgen)());
                        commits.push(CommitIndexedItem {
                            root_id: item.root_id.clone(),
                            relative_path: item.relative_path.clone(),
                            size_bytes: item.file_size_bytes,
                            mtime_ns: item.file_mtime_ns,
                            track_id: track_id.clone(),
                            tags_read_at: self.now(),
                        });
                        counters.add(counter_names::INDEXED, 1);
                        counts.indexed += 1;
                        match item.comparison_result {
                            Verdict::New => {
                                counters.add(counter_names::NEW, 1);
                                counts.new += 1;
                            }
                            Verdict::Changed => {
                                counters.add(counter_names::CHANGED, 1);
                                counts.changed += 1;
                            }
                            // A rescan_files re-read of an unchanged row lands
                            // as indexed but keeps its unchanged verdict.
                            Verdict::Unchanged => {
                                counters.add(counter_names::UNCHANGED, 1);
                                counts.unchanged += 1;
                            }
                            Verdict::Excluded | Verdict::CandidateMissing => {}
                        }
                        let album_key = format!(
                            "{}::{}",
                            item.root_id,
                            item.relative_path
                                .rsplit_once('/')
                                .map(|(parent, _)| parent)
                                .unwrap_or(".")
                        );
                        album_tracks.entry(album_key).or_default().push(track_id);
                    }
                }
            }
            if short_page {
                break;
            }
        }
        self.store.commit_indexed_batch(&commits);
        for (album_key, track_ids) in &album_tracks {
            let queued = self.identify.enqueue(album_key, track_ids);
            counters.add(counter_names::IDENTIFICATION_ENQUEUED, queued as i64);
            counts.identification_enqueued += queued;
        }
        counters.flush(self.store.as_ref(), &run.id);
        if let Ok((fresh, _, _)) = self.store.get_run(&run.id) {
            self.publish(&fresh, "scan.progress", true);
            self.log_progress(&fresh, "progress", false);
        }
        counts
    }

    /// Reconciliation phase: catalog rows under a cleanly-walked scope
    /// that the walk did not see are missing (v2 `LibraryReconciler`,
    /// reduced). Only completed scopes allow missing detection: a
    /// partially-read scope keeps the reconciler conservative.
    /// The missing set comes from the store, so reconcile holds no
    /// inventory or catalog listing resident.
    fn reconcile(&self, run_id: &str, scopes: &[ScanScope], frozen_policy_revision: &str) -> usize {
        let mut counters = CounterBuffer::new();
        let mut missing = 0usize;
        for scope in scopes {
            if !self.check(run_id, frozen_policy_revision) {
                counters.flush(self.store.as_ref(), run_id);
                return missing;
            }
            if self
                .store
                .scope_discovery_state(run_id, &scope.root_id, &scope.relative_path)
                != ScopeDiscoveryState::Completed
            {
                continue;
            }
            for relative_path in
                self.store
                    .missing_catalog_paths(run_id, &scope.root_id, &scope.relative_path)
            {
                self.store.remove_catalog(&scope.root_id, &relative_path);
                counters.add(counter_names::MISSING, 1);
                missing += 1;
            }
            counters.flush(self.store.as_ref(), run_id);
        }
        counters.flush(self.store.as_ref(), run_id);
        missing
    }
}

impl<S: ScanStore, T: TagReader + 'static, Q: IdentifyQueue> Checkpoint
    for LibraryScanCoordinator<S, T, Q>
{
    /// v2 `checkpoint`: fast true while policy matches and no control is
    /// pending, otherwise settle or supersede through the store.
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
        let (run, _, _) = match self.store.get_run(run_id) {
            Ok(found) => found,
            Err(_) => return false,
        };
        if current_policy_revision != frozen_policy_revision {
            if run.state == ScanState::Stopping || run.requested_control == RequestedControl::Stop {
                self.settle_pending_control(&run.id);
                return false;
            }
            if matches!(
                run.state,
                ScanState::Paused
                    | ScanState::Discovering
                    | ScanState::Indexing
                    | ScanState::Reconciling
                    | ScanState::Pausing
            ) {
                let _ = self.store.transition(
                    &run.id,
                    run.state,
                    run.row_revision,
                    ScanState::SupersededPolicyChanged,
                    self.now(),
                    Some(failure_codes::SUPERSEDED_POLICY_CHANGED),
                );
            }
            self.store.flush_invalidation(true);
            self.pending_control
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&run.id);
            if let Some(fs) = &self.fs {
                fs.forget_scan(&run.id);
            }
            return false;
        }
        if matches!(run.state, ScanState::Pausing | ScanState::Stopping) {
            self.settle_pending_control(&run.id);
            return false;
        }
        self.pending_control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&run.id);
        matches!(
            run.state,
            ScanState::Discovering | ScanState::Indexing | ScanState::Reconciling
        )
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
    use super::super::seams::{NullIdentifyQueue, NullTagReader};
    use super::super::store::MemoryScanStore;
    use super::*;
    use std::path::PathBuf;

    fn coordinator() -> LibraryScanCoordinator<MemoryScanStore, NullTagReader, NullIdentifyQueue> {
        let registry = RootRegistry::new(
            vec![LibraryRoot::new(
                "r1",
                PathBuf::from("/music"),
                super::super::models::EffectivePolicy::Automatic,
            )],
            true,
            "rev-1",
        );
        LibraryScanCoordinator::new(
            Arc::new(MemoryScanStore::new()),
            BlockingPool::new(2),
            Arc::new(NullTagReader::new()),
            Arc::new(NullIdentifyQueue::new()),
            Arc::new(StaticResolver::new(registry)),
        )
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
        let coordinator = coordinator();
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
        let coordinator = coordinator();
        let result = coordinator.request_run(&request()).expect("requested");
        let (run, _, _) = coordinator.store().get_run(&result.run_id).expect("run");
        coordinator
            .store()
            .transition(
                &run.id,
                run.state,
                run.row_revision,
                ScanState::Failed,
                9.0,
                Some("X"),
            )
            .expect("failed");
        let (items, next) = coordinator.history_page(50, None).expect("page");
        assert_eq!(items.len(), 1);
        assert_eq!(next, None);
        let cursor = format!("{}:{}", items[0].terminal_at.unwrap_or(0.0), items[0].id);
        let (items, _) = coordinator.history_page(50, Some(&cursor)).expect("page 2");
        assert!(items.is_empty());
        assert!(coordinator.history_page(50, Some("bogus")).is_err());
    }
}
