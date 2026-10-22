//! Download worker: startup recovery plus the poll loop.
//!
//! [`run_startup_recovery`] classifies every active task before serving
//! traffic (queued tasks re-dispatch through the first worker pass,
//! manifest-backed tasks resume polling, the rest restart clean under the
//! same idempotency keys, so a double resume cannot double-enqueue).
//! [`DownloadWorker`] then runs the steady-state passes on one cadence:
//!
//! - enqueue: queued tasks below the concurrency cap search and enqueue
//!   through the source order, journaling each attempt;
//! - poll: live attempts report progress through the [`Watchdog`], whose
//!   verdicts complete, fail over, or requeue their tasks. Each source is
//!   polled side by side with the others, and a client that stops
//!   answering is polled again only after a jittered backoff;
//! - retry: terminal tasks whose backoff elapsed spawn successors;
//! - cleanup: claimed journal rows abort and discard client records and
//!   settle; downloads whose cleanup keeps failing show on service health;
//! - orphans (hourly): complete-dir debris with no owner is removed.
//!
//! Sources and tuning are resolved at the start of every pass, so saved
//! settings take effect on the next pass without a restart. The loop runs
//! on the loop supervisor as `download-worker`. Journal failures are logged
//! and skip the affected item; a read error never stands in for "no rows".

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::downloads::journal::Journal;
use super::downloads::manifest::{DownloadManifest, ExpectedFile, ManifestCodec};
use super::downloads::orphans::{OrphanDecision, OrphanEvidence, evaluate_orphan, job_name_parts};
use super::downloads::quarantine::{
    QUARANTINE_TTL_SECONDS, QuarantineReason, failover_identities, is_local_fault,
};
use super::downloads::recovery::{
    FAILOVER_CLAIM_LIMIT, FAILOVER_LEASE_SECONDS, StartupAction, StartupCtx, classify_startup,
    plan_retry,
};
use super::downloads::source_tag::SourceTag;
use super::downloads::sources::OrphanOwnership;
use super::downloads::sources::{DownloadSource, SourceError, SourceHandle};
use super::downloads::state::{AttemptState, TaskStatus};
use super::downloads::store::{AttemptRow, NewTask, StoreError, TaskRow};
use super::downloads::watchdog::{PollSample, RetryPolicy, Watchdog, WatchdogConfig};
use super::landing::LandingService;
use super::readiness::SourceReadiness;
use super::search_jobs::review::ReviewMode;
use super::sources::JournalOwnership;
use super::target::Targets;
use super::target::reasons::TrackReason;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickResult,
};
use droppedneedle_providers::health::ServiceHealth;
#[cfg(any(test, feature = "test-support"))]
use futures_util::future::BoxFuture;

mod controls;
mod landings;
mod review;

pub use controls::{NextSourceError, RetryError};
pub use landings::ReimportError;

/// Job name of the worker loop.
pub const DOWNLOAD_WORKER_JOB: &str = "download-worker";

/// Job name of the wait, at shutdown, for the landings the worker started.
pub const DOWNLOAD_LANDINGS_JOB: &str = "download-landings";

/// Longest gap between passes when nothing wakes the worker.
pub const WORKER_INTERVAL: Duration = Duration::from_secs(30);

/// Orphan sweep runs every Nth pass (hourly at the 30s cadence).
const ORPHAN_EVERY_NTH_PASS: u64 = 120;

/// Service health key of the cleanup warning.
const CLEANUP_HEALTH_SERVICE: &str = "acquisition_cleanup";

/// How long the cleanup warning outlives the pass that reported it, so it
/// lapses once the worker stops.
const CLEANUP_HEALTH_TTL: Duration = Duration::from_secs(90);

/// Enqueue backoff after a pass where no source could serve a task.
const ENQUEUE_BACKOFF_SECONDS: f64 = 15.0 * 60.0;

/// First wait before polling a source again after its client stopped
/// answering; it doubles while the client keeps failing.
const POLL_BACKOFF_BASE_SECONDS: f64 = 30.0;

/// Longest wait between polls of a client that keeps failing, so one that
/// comes back is polled again within a few minutes.
const POLL_BACKOFF_CAP_SECONDS: f64 = 5.0 * 60.0;

/// Current unix time as float seconds.
fn now_unix_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// A journal step's value, or `None` after logging why it failed.
fn logged<R>(name: &'static str, result: Result<R, StoreError>) -> Option<R> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(step = name, %error, "download journal step failed");
            None
        }
    }
}

/// What startup recovery did, for the boot log and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Queued rows the first worker pass will dispatch.
    pub redispatched: usize,
    /// Manifest-backed rows resuming polling.
    pub resumed: usize,
    /// Rows restarted clean (back to queued, same keys).
    pub restarted: usize,
}

/// Classify every active task and restart the clean ones. Never fails a
/// task: queued rows stay queued for the first pass, manifest-backed rows
/// keep polling, and anything else returns to queued under its existing
/// idempotency keys. Re-running after a clean shutdown is a no-op.
pub async fn run_startup_recovery(
    journal: &Journal,
    staging_root: &Path,
) -> Result<RecoveryReport, String> {
    let tasks = journal
        .read_tasks_in(&[
            TaskStatus::Queued,
            TaskStatus::Downloading,
            TaskStatus::Processing,
        ])
        .await
        .map_err(|error| error.to_string())?;
    let mut report = RecoveryReport::default();
    for task in tasks {
        let manifest = read_manifest(staging_root, &task.id).await;
        let attempts = match journal.read_attempts(&task.id).await {
            Ok(attempts) => attempts,
            Err(error) => {
                // Without its attempts a task is never restarted clean: it
                // keeps its state and the first pass looks at it again.
                tracing::warn!(task_id = %task.id, %error, "startup recovery could not read the task's attempts");
                report.resumed += 1;
                continue;
            }
        };
        let manifest_matches_attempt = manifest
            .as_ref()
            .and_then(|m| m.attempt_id.as_deref())
            .map(|linked| attempts.iter().any(|attempt| attempt.id == linked));
        if task.status == TaskStatus::Processing
            && attempts.iter().any(|attempt| {
                matches!(
                    attempt.state,
                    AttemptState::Acquiring
                        | AttemptState::InUse
                        | AttemptState::Preserved
                        | AttemptState::CleanupPending
                )
            })
        {
            // Processing means a landing was running: it resumes on the
            // first poll pass from the files its attempt names, whether
            // or not the manifest survived. Never a fresh download.
            report.resumed += 1;
            continue;
        }
        match classify_startup(StartupCtx {
            status: task.status,
            manifest_present: manifest.is_some(),
            manifest_matches_attempt,
        }) {
            StartupAction::Redispatch => report.redispatched += 1,
            StartupAction::ResumePoll => report.resumed += 1,
            StartupAction::RestartClean => {
                let task_id = task.id.clone();
                journal
                    .run("downloads.recovery.restart", move |store| {
                        store.transition_task(&task_id, TaskStatus::Queued, now_unix_f64(), None)
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                report.restarted += 1;
            }
            StartupAction::Noop => {}
        }
    }
    Ok(report)
}

/// A retry's manifest skeleton, from its source's manifest: the release
/// it fetches, the tracks it asks for and the library copy it is for, so
/// the retry fetches and files the same as the first attempt. Nothing
/// when the source has no manifest. Best effort, like the dispatch
/// skeleton.
async fn write_successor_manifest(staging_root: &Path, source_id: &str, successor: &NewTask) {
    let Some(from) = read_manifest(staging_root, source_id).await else {
        return;
    };
    let task_id = successor.id.clone();
    let manifest = DownloadManifest {
        task_id: successor.id.clone(),
        release_group_mbid: from.release_group_mbid,
        artist_name: from.artist_name,
        album_title: from.album_title,
        target_files: Vec::new(),
        source_username: None,
        handle: None,
        expected_tracks: Vec::new(),
        release_mbid: from.release_mbid,
        artist_mbid: None,
        year: None,
        is_track: from.is_track,
        hold_on_wrong_track: false,
        origin: successor.origin.clone(),
        requested_by_user_id: None,
        attempt_id: None,
        track_album: None,
        album_wanted: from.album_wanted,
        planned_album: from.planned_album,
    };
    let root = staging_root.to_path_buf();
    match tokio::task::spawn_blocking(move || ManifestCodec.write(&root, &manifest)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%task_id, %error, "retry manifest write failed");
        }
        Err(error) => {
            tracing::warn!(%task_id, %error, "retry manifest write did not run");
        }
    }
}

/// A task's manifest, when present and readable.
async fn read_manifest(staging_root: &Path, task_id: &str) -> Option<DownloadManifest> {
    let path = ManifestCodec::checked_path(staging_root, task_id)?;
    let bytes = tokio::fs::read(path).await.ok()?;
    ManifestCodec.decode(&bytes).ok()
}

/// A test source whose every download is finished at once and landed at
/// fixed paths, so journeys can drive the worker end to end. It answers
/// as the plugin source `fixed`.
#[cfg(any(test, feature = "test-support"))]
pub struct FixedSource {
    mount: PathBuf,
    paths: Vec<PathBuf>,
    discard_fails: std::sync::atomic::AtomicBool,
}

#[cfg(any(test, feature = "test-support"))]
impl FixedSource {
    /// Downloads land at `paths`, inside the downloads folder `mount`.
    pub fn new(mount: PathBuf, paths: Vec<PathBuf>) -> Self {
        Self {
            mount,
            paths,
            discard_fails: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Make every later discard fail (or succeed again).
    pub fn fail_discards(&self, fail: bool) {
        self.discard_fails
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    fn names(&self) -> Vec<String> {
        self.paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl DownloadSource for FixedSource {
    fn tag(&self) -> SourceTag {
        SourceTag::Plugin("fixed".to_owned())
    }

    fn search<'a>(
        &'a self,
        _target: &'a super::target::SearchTarget,
    ) -> BoxFuture<'a, Result<Vec<super::search_jobs::candidates::Candidate>, SourceError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn enqueue<'a>(
        &'a self,
        task_id: &'a str,
        candidate_index: i64,
    ) -> BoxFuture<'a, Result<SourceHandle, SourceError>> {
        Box::pin(async move {
            Ok(SourceHandle {
                filenames: self.names(),
                job_name: format!("fixed-{task_id}-{candidate_index}"),
                ..SourceHandle::new(self.tag())
            })
        })
    }

    fn poll<'a>(
        &'a self,
        _handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<super::downloads::sources::TransferProgress, SourceError>> {
        Box::pin(async move {
            Ok(super::downloads::sources::TransferProgress {
                all_terminal: true,
                all_succeeded: true,
                has_active_transfer: false,
                downloaded_bytes: 1,
                succeeded_filenames: self.names(),
                ..Default::default()
            })
        })
    }

    fn landed<'a>(
        &'a self,
        _handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<crate::landing::Landed, SourceError>> {
        Box::pin(async move {
            Ok(crate::landing::Landed {
                mounts: vec![self.mount.clone().into()],
                files: self
                    .paths
                    .iter()
                    .cloned()
                    .map(crate::landing::Reported::path)
                    .collect(),
            })
        })
    }

    fn discard<'a>(
        &'a self,
        _handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(async move {
            if self
                .discard_fails
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                Err(SourceError::LocalFault("discard refused".to_owned()))
            } else {
                Ok(true)
            }
        })
    }

    fn abort<'a>(&'a self, _handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(async { Ok(true) })
    }
}

/// Worker tuning, read at the start of every pass.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Concurrent live downloads.
    pub max_concurrent_downloads: usize,
    /// Failover attempts per task across all sources.
    pub max_failover_attempts: i64,
    /// Auto-retry timing.
    pub retry: RetryPolicy,
    /// Watchdog timing.
    pub watchdog: WatchdogConfig,
    /// Sources in try order.
    pub source_order: Vec<SourceTag>,
    /// Per-task staging root (manifests live beneath it).
    pub staging_root: PathBuf,
    /// Complete dirs to walk as `(journal source, dir)`.
    pub orphan_roots: Vec<(SourceTag, PathBuf)>,
    /// Lease owner name for cleanup claims.
    pub worker_id: String,
    /// When an album waits for a hand pick instead of the best guess.
    pub review: ReviewMode,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_downloads: 3,
            max_failover_attempts: 3,
            retry: RetryPolicy::default(),
            watchdog: WatchdogConfig::default(),
            source_order: vec![SourceTag::Soulseek, SourceTag::Usenet],
            staging_root: PathBuf::from("staging"),
            orphan_roots: Vec::new(),
            worker_id: "download-worker".to_owned(),
            review: ReviewMode::default(),
        }
    }
}

/// The configured sources for one pass.
pub type SourceSet = Arc<Vec<Arc<dyn DownloadSource>>>;

/// The sources turned on right now, for work outside the worker's passes
/// (the wanted watcher's search).
#[derive(Clone)]
pub enum SourceList {
    /// The sources the saved settings turn on, plugin sources included.
    Live(Arc<super::wiring::WorkerSettings>),
    /// A fixed set, for tests that script the sources.
    Fixed(SourceSet),
}

impl SourceList {
    /// The sources on now, or `None` while the download settings do not
    /// read.
    pub fn current(&self) -> Option<SourceSet> {
        match self {
            Self::Live(settings) => settings.readable().then(|| settings.sources()),
            Self::Fixed(sources) => Some(sources.clone()),
        }
    }
}

/// Where the worker's sources and tuning come from, read once per pass.
enum Inputs {
    /// The saved settings.
    Live(Arc<super::wiring::WorkerSettings>),
    /// A fixed set, for tests that script the sources.
    Fixed {
        sources: SourceSet,
        config: Box<WorkerConfig>,
    },
}

impl Inputs {
    fn sources(&self) -> SourceSet {
        match self {
            Self::Live(settings) => settings.sources(),
            Self::Fixed { sources, .. } => sources.clone(),
        }
    }

    /// `None` while a section the worker needs does not read.
    fn config(&self) -> Option<WorkerConfig> {
        match self {
            Self::Live(settings) => settings.config(),
            Self::Fixed { config, .. } => Some(WorkerConfig::clone(config)),
        }
    }

    fn readable(&self) -> bool {
        match self {
            Self::Live(settings) => settings.readable(),
            Self::Fixed { .. } => true,
        }
    }

    fn staging_root(&self) -> PathBuf {
        match self {
            Self::Live(settings) => settings.staging_root().to_owned(),
            Self::Fixed { config, .. } => config.staging_root.clone(),
        }
    }
}

/// Everything one pass works with: settings and sources resolved once at
/// the start of the pass.
struct Pass {
    config: WorkerConfig,
    sources: SourceSet,
    watchdog: Watchdog,
    now: f64,
}

impl Pass {
    /// Sources in configured try order.
    fn ordered_sources(&self) -> Vec<&Arc<dyn DownloadSource>> {
        in_try_order(&self.sources, &self.config.source_order)
    }

    /// The adapter for one source.
    fn source_for(&self, tag: &SourceTag) -> Option<&Arc<dyn DownloadSource>> {
        self.sources.iter().find(|source| source.tag() == *tag)
    }
}

/// Sources in the configured try order; any the order does not name
/// come last.
fn in_try_order<'a>(
    sources: &'a [Arc<dyn DownloadSource>],
    order: &[SourceTag],
) -> Vec<&'a Arc<dyn DownloadSource>> {
    let mut ordered = Vec::with_capacity(sources.len());
    for wanted in order {
        for source in sources {
            if source.tag() == *wanted {
                ordered.push(source);
            }
        }
    }
    for source in sources {
        if !ordered.iter().any(|placed| std::ptr::eq(*placed, source)) {
            ordered.push(source);
        }
    }
    ordered
}

/// One attempt plus its decoded client handle, when it has one.
#[derive(Debug, Clone)]
struct Attempt {
    row: AttemptRow,
    handle: Option<SourceHandle>,
}

/// A source whose client stopped answering polls: how many poll passes
/// failed in a row and when it is polled again.
#[derive(Debug, Clone, Copy)]
struct PollBackoff {
    failures: u32,
    not_before: f64,
}

impl PollBackoff {
    /// The backoff after one more failed poll at `now`: the base doubled
    /// per failure, capped, then spread by up to 20% so sources that went
    /// down together are not retried together.
    fn after_failure(previous: Option<Self>, now: f64) -> Self {
        let failures = previous
            .map_or(0, |backoff| backoff.failures)
            .saturating_add(1);
        let grown = POLL_BACKOFF_BASE_SECONDS * 2_f64.powi(failures.min(16) as i32 - 1);
        let mut byte = [0u8; 1];
        let spread = if getrandom::fill(&mut byte).is_ok() {
            f64::from(byte[0]) / 255.0 * 0.2
        } else {
            0.0
        };
        Self {
            failures,
            not_before: now + grown.min(POLL_BACKOFF_CAP_SECONDS) * (1.0 + spread),
        }
    }
}

/// What one task's poll learned about its source.
enum Polled {
    /// The client answered (or the task needed no poll).
    Answered,
    /// The client did not answer.
    Unavailable(SourceTag),
}

/// Per-task poll memory: byte progress across passes.
#[derive(Debug, Clone)]
struct PollMemory {
    enqueued_at: f64,
    last_bytes: u64,
    last_move_at: f64,
}

/// The steady-state worker. All passes are idempotent and per-item
/// isolated: one bad task never stops its siblings.
pub struct DownloadWorker {
    journal: Arc<Journal>,
    inputs: Inputs,
    ownership: JournalOwnership,
    poll_cache: Mutex<HashMap<String, PollMemory>>,
    /// Sources whose client stopped answering, polled again only once
    /// their backoff lapses.
    poll_backoff: Mutex<HashMap<SourceTag, PollBackoff>>,
    enqueue_not_before: Mutex<HashMap<String, f64>>,
    unsearchable_warned: Mutex<HashSet<String>>,
    plugins: Option<Arc<droppedneedle_plugins::host::PluginHost>>,
    landing: Option<Arc<LandingService>>,
    landing_waits: Mutex<HashMap<String, u32>>,
    /// Tasks whose landing runs right now, off the pass. The poll and
    /// enqueue passes leave them alone.
    in_flight: Arc<Mutex<HashSet<String>>>,
    /// Spawned landings, awaited on shutdown.
    landing_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Live progress for the web UI (`download_progress`).
    events: droppedneedle_kernel::events::EventHub,
    /// Wakes the loop early after a queue action (retry, next source).
    wake: tokio::sync::watch::Sender<u64>,
    /// Album targets for the look before an automatic grab; without them
    /// every source grabs its own best guess.
    targets: Option<Arc<Targets>>,
    /// Looks running off the pass, and tasks already looked at.
    reviews: review::Reviews,
    /// Where the cleanup warning is shown.
    health: Arc<ServiceHealth>,
    /// Whether a download source is on, over the same sources.
    readiness: SourceReadiness,
}

impl DownloadWorker {
    /// Wire the worker over the shared journal and the saved settings,
    /// sending live progress to `events` and the cleanup warning to
    /// `health`. `readiness` reads the same settings.
    pub fn new(
        journal: Arc<Journal>,
        settings: Arc<super::wiring::WorkerSettings>,
        readiness: SourceReadiness,
        events: droppedneedle_kernel::events::EventHub,
        health: Arc<ServiceHealth>,
    ) -> Self {
        Self::over(journal, Inputs::Live(settings), readiness, events, health)
    }

    fn over(
        journal: Arc<Journal>,
        inputs: Inputs,
        readiness: SourceReadiness,
        events: droppedneedle_kernel::events::EventHub,
        health: Arc<ServiceHealth>,
    ) -> Self {
        let ownership = JournalOwnership::new(journal.clone(), None);
        Self {
            readiness,
            journal,
            inputs,
            ownership,
            poll_cache: Mutex::new(HashMap::new()),
            poll_backoff: Mutex::new(HashMap::new()),
            enqueue_not_before: Mutex::new(HashMap::new()),
            unsearchable_warned: Mutex::new(HashSet::new()),
            plugins: None,
            landing: None,
            landing_waits: Mutex::new(HashMap::new()),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            landing_tasks: Mutex::new(Vec::new()),
            events,
            wake: tokio::sync::watch::Sender::new(0),
            targets: None,
            reviews: review::Reviews::default(),
            health,
        }
    }

    /// Search every source and look over the candidates before an album's
    /// first automatic attempt (search review).
    pub fn with_targets(mut self, targets: Arc<Targets>) -> Self {
        self.targets = Some(targets);
        self
    }

    /// The download journal the worker writes.
    pub fn journal(&self) -> &Arc<Journal> {
        &self.journal
    }

    /// The worker tuning as saved right now, or `None` while the download
    /// settings do not read.
    pub fn current_config(&self) -> Option<WorkerConfig> {
        self.inputs.config()
    }

    /// Whether the download settings read. While they do not, no pass runs
    /// and [`Self::sources_now`] is empty.
    pub fn settings_readable(&self) -> bool {
        self.inputs.readable()
    }

    /// Where landings stage files.
    pub fn staging_root(&self) -> PathBuf {
        self.inputs.staging_root()
    }

    /// Whether a download source is on: the answer requests, the health
    /// smoke and Discover read too.
    pub fn readiness(&self) -> &SourceReadiness {
        &self.readiness
    }

    /// The sources turned on right now, in the configured try order.
    pub fn sources_now(&self) -> Vec<Arc<dyn DownloadSource>> {
        let Some(config) = self.inputs.config() else {
            return Vec::new();
        };
        let sources = self.inputs.sources();
        let order = config.source_order;
        in_try_order(&sources, &order)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Run the next pass now instead of at the next interval.
    pub fn wake(&self) {
        self.wake
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Import finished downloads through the landing instead of settling
    /// them as completed unseen.
    pub fn with_landing(mut self, landing: Arc<LandingService>) -> Self {
        self.ownership = JournalOwnership::new(self.journal.clone(), landing.library().cloned());
        self.landing = Some(landing);
        self
    }

    /// The landing, when imports are wired.
    pub fn landing(&self) -> Option<&Arc<LandingService>> {
        self.landing.as_ref()
    }

    /// Announce download starts, completions and failures to `subscriber`
    /// plugins through this host.
    pub fn with_plugin_events(
        mut self,
        host: Arc<droppedneedle_plugins::host::PluginHost>,
    ) -> Self {
        self.plugins = Some(host);
        self
    }

    /// Worker over a fixed source list and fixed tuning.
    pub fn fixed(
        journal: Arc<Journal>,
        sources: Vec<Arc<dyn DownloadSource>>,
        config: WorkerConfig,
        events: droppedneedle_kernel::events::EventHub,
        health: Arc<ServiceHealth>,
    ) -> Self {
        let sources = Arc::new(sources);
        Self::over(
            journal,
            Inputs::Fixed {
                sources: sources.clone(),
                config: Box::new(config),
            },
            SourceReadiness::unprobed(SourceList::Fixed(sources)),
            events,
            health,
        )
    }

    /// Run every steady-state pass once, in order. The orphan sweep only
    /// runs when `pass` hits its hourly slot. Landings the poll pass
    /// starts run on their own tasks; [`Self::wait_for_landings`] awaits
    /// them.
    pub async fn run_once(self: &Arc<Self>, pass: u64) {
        // Settings that do not read stop the whole pass: nothing starts,
        // lands or fails over until they are saved again.
        let Some(config) = self.inputs.config() else {
            return;
        };
        let pass_ctx = Pass {
            watchdog: Watchdog::new(config.watchdog.clone()),
            sources: self.inputs.sources(),
            config,
            now: now_unix_f64(),
        };
        self.enqueue_pass(&pass_ctx).await;
        self.poll_pass(&pass_ctx).await;
        self.retry_pass(&pass_ctx).await;
        self.cleanup_pass(&pass_ctx).await;
        if pass.is_multiple_of(ORPHAN_EVERY_NTH_PASS) {
            self.orphan_pass(&pass_ctx).await;
        }
    }

    /// Run one journal write step, logging a failure with its name.
    async fn step<R, F>(&self, name: &'static str, op: F) -> Option<R>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(
                &'a super::downloads::store::DownloadStore<'b>,
            ) -> Result<R, StoreError>
            + Send
            + 'static,
    {
        logged(name, self.journal.run(name, op).await)
    }

    /// Run one journal read on the read pool, logging a failure with its
    /// name.
    async fn read_step<R, F>(&self, name: &'static str, op: F) -> Option<R>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(
                &'a super::downloads::store::DownloadStore<'b>,
            ) -> Result<R, StoreError>
            + Send
            + 'static,
    {
        logged(name, self.journal.read(name, op).await)
    }

    /// A task's attempts with their decoded handles, in one read.
    async fn attempts(&self, task_id: &str) -> Option<Vec<Attempt>> {
        let task_id = task_id.to_owned();
        self.read_step("downloads.attempts", move |store| {
            let rows = store.list_attempts(&task_id)?;
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let handle = store
                    .attempt_handle_json(&row.id)?
                    .and_then(|json| serde_json::from_str::<SourceHandle>(&json).ok());
                out.push(Attempt { row, handle });
            }
            Ok(out)
        })
        .await
    }

    /// Enqueue queued tasks while under the concurrency cap. With no
    /// source on, the queue is held as it is.
    async fn enqueue_pass(self: &Arc<Self>, pass: &Pass) {
        if pass.sources.is_empty() {
            // No source is on: queued tasks wait untouched, with no look,
            // no enqueue key and no backoff spent, so the first pass after
            // one is turned on starts them.
            return;
        }
        let Some(live) = self
            .read_step("downloads.live", |store| {
                store.list_active(&[TaskStatus::Downloading, TaskStatus::Processing])
            })
            .await
        else {
            // Without the live count the cap cannot hold; skip this pass.
            return;
        };
        let Some(queued) = self
            .read_step("downloads.queued", |store| {
                store.list_active(&[TaskStatus::Queued])
            })
            .await
        else {
            return;
        };
        self.prune_queue_memory(&queued);
        let mut budget = pass
            .config
            .max_concurrent_downloads
            .saturating_sub(live.len());
        for task in queued {
            if budget == 0 {
                break;
            }
            if self.enqueue_one(pass, &task).await {
                budget -= 1;
            }
        }
    }

    /// Forget backoff and warning marks for tasks that left the queue.
    fn prune_queue_memory(&self, queued: &[TaskRow]) {
        let ids: HashSet<&str> = queued.iter().map(|task| task.id.as_str()).collect();
        if let Ok(mut backoff) = self.enqueue_not_before.lock() {
            backoff.retain(|task_id, _| ids.contains(task_id.as_str()));
        }
        if let Ok(mut warned) = self.unsearchable_warned.lock() {
            warned.retain(|task_id| ids.contains(task_id.as_str()));
        }
        if let Ok(mut looked) = self.reviews.looked.lock() {
            looked.retain(|task_id| ids.contains(task_id.as_str()));
        }
    }

    /// Enqueue one queued task. Answers whether a fetch started.
    async fn enqueue_one(self: &Arc<Self>, pass: &Pass, task: &TaskRow) -> bool {
        let now = pass.now;
        if self.landing_in_flight(&task.id) || Self::parked(task) {
            // A parked task waits for its owner's pick from its search.
            return false;
        }
        if task.artist_name.trim().is_empty() && task.album_title.trim().is_empty() {
            // Edition asks carry no searchable names until the catalog
            // port lands; warn once per task and leave the row queued.
            let fresh = self
                .unsearchable_warned
                .lock()
                .map(|mut warned| warned.insert(task.id.clone()))
                .unwrap_or(false);
            if fresh {
                tracing::warn!(
                    task_id = %task.id,
                    "download task has no searchable names; leaving queued"
                );
            }
            return false;
        }
        if let Some(not_before) = self
            .enqueue_not_before
            .lock()
            .ok()
            .and_then(|backoff| backoff.get(&task.id).copied())
            && now < not_before
        {
            return false;
        }
        let Some(attempts) = self.attempts(&task.id).await else {
            // Without the attempt list the next index is unknown; never
            // guess index 0 and re-post a transfer.
            return false;
        };
        if live_attempt(&attempts).is_some() {
            // A pollable attempt exists; the poll pass owns this task.
            // Queued rows in that state just missed their transition.
            let task_id = task.id.clone();
            self.step("downloads.resume_live", move |store| {
                store.transition_task(&task_id, TaskStatus::Downloading, now, None)
            })
            .await;
            return false;
        }
        let handled = handled_count(&attempts, None);
        if failover_exhausted(handled, pass.config.max_failover_attempts) {
            self.fail_unserved(task, &attempts, now, None).await;
            return false;
        }
        if handled == 0 && task.search_job_id.is_none() && self.start_review(pass, task) {
            // The look runs off the pass; the next pass picks up its link,
            // or leaves the task parked.
            return false;
        }
        // The key marks the window between the client add and the attempt
        // journal write. A repeat claim means a crash landed in it: the
        // enqueue below re-attaches (usenet probes queue and history by the
        // deterministic job name; slskd re-enqueues by content) instead of
        // starting a second transfer.
        let key = format!("enqueue:{}:{handled}", task.id);
        let task_id = task.id.clone();
        let Some(fresh) = self
            .step("downloads.enqueue_key", move |store| {
                store.claim_key(&key, &task_id, "enqueue", now)
            })
            .await
        else {
            return false;
        };
        if !fresh {
            tracing::info!(task_id = %task.id, attempt = handled, "resuming an interrupted enqueue");
        }
        let mut every_source_refused = true;
        // A source's reason (code, sentence, action) becomes the task's
        // message when nothing can serve it.
        let mut reason: Option<String> = None;
        let mut sources = pass.ordered_sources();
        // A task started from a manual pick tries the picked source first.
        if handled == 0
            && task.search_job_id.is_some()
            && task.candidate_index.is_some()
            && let Some(at) = sources
                .iter()
                .position(|source| source.tag() == task.source)
        {
            let picked = sources.remove(at);
            sources.insert(0, picked);
        }
        for source in sources {
            // Each source walks its own candidate list: the index is the
            // number of attempts this source already handled.
            let source_index = handled_count(&attempts, Some(&source.tag()));
            match source.enqueue(&task.id, source_index).await {
                Ok(handle) => {
                    self.journal_enqueued(pass, task, source, handled, source_index, &handle)
                        .await;
                    return true;
                }
                Err(SourceError::LocalFault(detail)) => {
                    // Our side is broken (disk, mount): back off without
                    // blocklisting a healthy release or trying the other
                    // side onto the same broken mount.
                    tracing::warn!(task_id = %task.id, %detail, "enqueue local fault");
                    self.backoff(&task.id, now);
                    return false;
                }
                Err(error) => {
                    match &error {
                        SourceError::Rejected(text) => {
                            if let Some(found) = TrackReason::in_text(text) {
                                reason = Some(found.to_string());
                            }
                        }
                        _ => every_source_refused = false,
                    }
                    tracing::warn!(
                        task_id = %task.id,
                        source = %source.tag(),
                        %error,
                        "enqueue failed; trying the next source"
                    );
                }
            }
        }
        if every_source_refused && !pass.sources.is_empty() {
            // Every configured source answered that it has nothing for
            // this release: the candidate lists are spent.
            self.fail_unserved(task, &attempts, now, reason.as_deref())
                .await;
            return false;
        }
        self.backoff(&task.id, now);
        false
    }

    /// Settle a task no source can serve any more, keeping its last
    /// attempt. Earlier landings decide how: tracks imported along the way
    /// make it partial, held files make it a failure held for review
    /// (v2 `_settle_incomplete`), else the last source's reason (code,
    /// sentence, action) or no source could serve it.
    async fn fail_unserved(
        &self,
        task: &TaskRow,
        attempts: &[Attempt],
        now: f64,
        reason: Option<&str>,
    ) {
        let task_id = task.id.clone();
        let last_attempt = attempts.last().map(|attempt| attempt.row.id.clone());
        let reason = reason.map(str::to_owned);
        // The history and the settle share one transaction: a history that
        // cannot be read settles nothing and the next pass tries again, so
        // imported tracks never end as a plain failure.
        let settled = self
            .step("downloads.fail_unserved", move |store| {
                let history = store.landing_history(&task_id)?;
                let (status, error) = if history.files_imported > 0 {
                    (
                        TaskStatus::Partial,
                        "imported what was found; no source had the rest".to_owned(),
                    )
                } else if let Some(detail) = history.held_detail {
                    (TaskStatus::Failed, format!("Held for review: {detail}"))
                } else {
                    (
                        TaskStatus::Failed,
                        reason.unwrap_or_else(|| "no source could serve this release".to_owned()),
                    )
                };
                store.finalize_task_and_attempt(
                    &task_id,
                    status,
                    now,
                    Some(&error),
                    last_attempt.as_deref(),
                    true,
                )?;
                Ok(status)
            })
            .await;
        let Some(status) = settled else {
            return;
        };
        let (kind, outcome) = if status == TaskStatus::Partial {
            (
                droppedneedle_plugins::runtime::EventKind::DownloadCompleted,
                "partial",
            )
        } else {
            (
                droppedneedle_plugins::runtime::EventKind::DownloadFailed,
                "failed",
            )
        };
        super::plugin_events::download_event(&self.plugins, kind, task, &task.source, outcome);
    }

    /// Journal one successful enqueue: the attempt row plus the
    /// Downloading transition in one write, then the manifest handle and
    /// fresh poll memory. If the attempt id is already taken (another
    /// writer journaled this slot), the new client job is aborted so it
    /// never runs untracked.
    async fn journal_enqueued(
        &self,
        pass: &Pass,
        task: &TaskRow,
        source: &Arc<dyn DownloadSource>,
        attempt_number: i64,
        source_index: i64,
        handle: &SourceHandle,
    ) {
        let now = pass.now;
        let handle_json = match serde_json::to_string(handle) {
            Ok(json) => json,
            Err(error) => {
                tracing::error!(task_id = %task.id, %error, "client handle encode failed");
                return;
            }
        };
        let attempt_id = format!("{}-a{attempt_number}", task.id);
        let task_id = task.id.clone();
        let journal_source = source.tag().to_string();
        let job_name = handle.job_name.clone();
        let row_id = attempt_id.clone();
        let journaled = self
            .step("downloads.enqueued", move |store| {
                store.insert_attempt(
                    &row_id,
                    &task_id,
                    &journal_source,
                    source_index,
                    &job_name,
                    &handle_json,
                    AttemptState::Acquiring,
                    now,
                )?;
                store.transition_task(&task_id, TaskStatus::Downloading, now, None)
            })
            .await;
        if journaled.is_none() {
            if let Err(error) = source.abort(handle).await {
                tracing::warn!(task_id = %task.id, %error, "untracked client job abort failed");
            }
            return;
        }
        self.write_manifest_handle(pass, task, handle, &attempt_id)
            .await;
        super::plugin_events::download_event(
            &self.plugins,
            droppedneedle_plugins::runtime::EventKind::DownloadStarted,
            task,
            &source.tag().to_string(),
            "started",
        );
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.insert(
                task.id.clone(),
                PollMemory {
                    enqueued_at: now,
                    last_bytes: 0,
                    last_move_at: now,
                },
            );
        }
    }

    /// Merge the client handle, target files and attempt link into the
    /// task manifest. Best-effort: a missing manifest only steers recovery
    /// toward a clean restart.
    async fn write_manifest_handle(
        &self,
        pass: &Pass,
        task: &TaskRow,
        handle: &SourceHandle,
        attempt_id: &str,
    ) {
        let staging_root = pass.config.staging_root.clone();
        let mut manifest = read_manifest(&staging_root, &task.id)
            .await
            .unwrap_or_else(|| DownloadManifest {
                task_id: task.id.clone(),
                release_group_mbid: task.release_group_mbid.clone(),
                artist_name: task.artist_name.clone(),
                album_title: task.album_title.clone(),
                target_files: Vec::new(),
                source_username: None,
                handle: None,
                expected_tracks: Vec::new(),
                release_mbid: None,
                artist_mbid: None,
                year: None,
                is_track: task.download_type == "track",
                hold_on_wrong_track: false,
                origin: task.origin.clone(),
                requested_by_user_id: None,
                attempt_id: None,
                track_album: None,
                album_wanted: Vec::new(),
                planned_album: None,
            });
        manifest.handle = Some(super::downloads::manifest::TaskHandle {
            source: handle.source.to_string(),
            username: handle.username.clone(),
            filenames: handle.filenames.clone(),
            job_name: handle.job_name.clone(),
        });
        manifest.attempt_id = Some(attempt_id.to_owned());
        // Each attempt's files, with the sizes the chosen candidate
        // advertised (the import's size check reads them from the handle).
        manifest.target_files = handle
            .filenames
            .iter()
            .enumerate()
            .map(|(index, filename)| ExpectedFile {
                filename: filename.clone(),
                size: handle.sizes.get(index).copied().unwrap_or(0),
                duration: None,
            })
            .collect();
        let task_id = task.id.clone();
        let written =
            tokio::task::spawn_blocking(move || ManifestCodec.write(&staging_root, &manifest))
                .await;
        match written {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(task_id, %error, "manifest handle write failed");
            }
            Err(error) => {
                tracing::warn!(task_id, %error, "manifest handle write join failed");
            }
        }
    }

    /// Push one task's next enqueue attempt out by the backoff window.
    fn backoff(&self, task_id: &str, now: f64) {
        if let Ok(mut backoff) = self.enqueue_not_before.lock() {
            backoff.insert(task_id.to_owned(), now + ENQUEUE_BACKOFF_SECONDS);
        }
    }

    /// Poll every live task through the watchdog. Each source's tasks are
    /// polled in their own round, the rounds side by side, so a client that
    /// stops answering only holds up its own tasks.
    async fn poll_pass(self: &Arc<Self>, pass: &Pass) {
        let Some(tasks) = self
            .read_step("downloads.live", |store| {
                store.list_active(&[TaskStatus::Downloading, TaskStatus::Processing])
            })
            .await
        else {
            return;
        };
        let live: HashSet<&str> = tasks.iter().map(|task| task.id.as_str()).collect();
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.retain(|task_id, _| live.contains(task_id.as_str()));
        }
        if let Ok(mut backoff) = self.poll_backoff.lock() {
            backoff.retain(|tag, _| pass.sources.iter().any(|source| source.tag() == *tag));
        }
        // Grouped by the source of each task's live attempt; the task row's
        // own source names only a candidate a person picked.
        let mut rounds: HashMap<Option<SourceTag>, Vec<&TaskRow>> = HashMap::new();
        for task in &tasks {
            let Some(attempts) = self.attempts(&task.id).await else {
                continue;
            };
            let tag = live_attempt(&attempts).map(|attempt| attempt.row.source.clone());
            rounds.entry(tag).or_default().push(task);
        }
        futures_util::future::join_all(
            rounds
                .into_values()
                .map(|round| self.poll_round(pass, round)),
        )
        .await;
    }

    /// Poll one source's tasks in turn. The first poll its client does not
    /// answer backs the source off; its other tasks wait for a later pass,
    /// and their stall clocks keep running.
    async fn poll_round(self: &Arc<Self>, pass: &Pass, tasks: Vec<&TaskRow>) {
        for task in tasks {
            match self.poll_one(pass, task).await {
                Polled::Answered => {}
                Polled::Unavailable(tag) => {
                    if let Ok(mut backoff) = self.poll_backoff.lock() {
                        let next = PollBackoff::after_failure(backoff.get(&tag).copied(), pass.now);
                        tracing::warn!(
                            source = %tag,
                            failures = next.failures,
                            retry_in_seconds = (next.not_before - pass.now).round(),
                            "download client not answering; polls back off"
                        );
                        backoff.insert(tag, next);
                    }
                }
            }
        }
    }

    /// Whether `tag`'s client is backing off at `now`.
    fn backing_off(&self, tag: &SourceTag, now: f64) -> bool {
        self.poll_backoff
            .lock()
            .is_ok_and(|backoff| backoff.get(tag).is_some_and(|wait| now < wait.not_before))
    }

    /// Poll one live task: judge the sample, then complete, fail over, or
    /// keep waiting. Poll errors never fail the task directly; the stall
    /// math ages a dead client out into failover instead.
    async fn poll_one(self: &Arc<Self>, pass: &Pass, task: &TaskRow) -> Polled {
        let now = pass.now;
        if self.landing_in_flight(&task.id) {
            return Polled::Answered;
        }
        // The pass listed its tasks before any of them were polled; a
        // landing that settled since then must not be seen as live again.
        let task = match self.journal.read_task(&task.id).await {
            Ok(Some(fresh)) if fresh.status == task.status => fresh,
            Ok(_) => return Polled::Answered,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "task re-read failed; poll skipped");
                return Polled::Answered;
            }
        };
        let task = &task;
        let Some(attempts) = self.attempts(&task.id).await else {
            // A read failure is not "no attempt": leave the task alone.
            return Polled::Answered;
        };
        if task.status == TaskStatus::Processing
            && live_attempt(&attempts).is_none()
            && self.resume_landing(pass, task, &attempts).await
        {
            // A landing the last process left running (a reimport whose
            // attempt is no longer live) picks up again instead of the
            // task being requeued for a fresh download.
            return Polled::Answered;
        }
        let Some(attempt) = live_attempt(&attempts).cloned() else {
            // No pollable attempt: back to queued for a fresh enqueue.
            let task_id = task.id.clone();
            self.step("downloads.requeue", move |store| {
                store.transition_task(&task_id, TaskStatus::Queued, now, None)
            })
            .await;
            return Polled::Answered;
        };
        let Some(handle) = attempt.handle.clone() else {
            return Polled::Answered;
        };
        let Some(source) = pass.source_for(&attempt.row.source) else {
            // Plugin sources have no adapter yet; leave the row for a
            // worker that knows them rather than failing it here.
            return Polled::Answered;
        };
        if self.backing_off(&attempt.row.source, now) {
            return Polled::Answered;
        }
        let mut memory = self.poll_memory(&task.id, task, now);
        match source.poll(&handle).await {
            Ok(progress) => {
                if progress.downloaded_bytes > memory.last_bytes {
                    memory.last_move_at = now;
                    memory.last_bytes = progress.downloaded_bytes;
                }
                let sample = PollSample {
                    elapsed_seconds: now - memory.enqueued_at,
                    idle_seconds: now - memory.last_move_at,
                    has_active_transfer: progress.has_active_transfer,
                    downloaded_bytes: progress.downloaded_bytes,
                    all_terminal: progress.all_terminal,
                    all_succeeded: progress.all_succeeded,
                    materialize_wait_seconds: if progress.downloaded_bytes == 0
                        && !progress.has_active_transfer
                    {
                        now - memory.enqueued_at
                    } else {
                        0.0
                    },
                };
                self.store_poll_memory(&task.id, &memory);
                self.apply_verdict(pass, task, &attempt.row, source, &handle, progress, sample)
                    .await;
                self.clear_backoff(&attempt.row.source);
                Polled::Answered
            }
            Err(SourceError::Rejected(detail)) => {
                self.fail_over(
                    now,
                    task,
                    &attempt.row,
                    source,
                    &handle,
                    &detail,
                    Some(QuarantineReason::DownloadFailed),
                )
                .await;
                self.clear_backoff(&attempt.row.source);
                Polled::Answered
            }
            Err(error) => {
                // Unavailable or local fault: stamp the poll and let the
                // stall math age the task out; a momentary outage must
                // not fail a healthy transfer. Only a client that does not
                // answer backs its source off.
                tracing::warn!(task_id = %task.id, %error, "download poll failed");
                let task_id = task.id.clone();
                self.step("downloads.touch_poll", move |store| {
                    store.touch_poll(&task_id, now)
                })
                .await;
                match error {
                    SourceError::Unavailable(_) => Polled::Unavailable(attempt.row.source.clone()),
                    _ => Polled::Answered,
                }
            }
        }
    }

    /// Forget a source's backoff once its client answers again.
    fn clear_backoff(&self, tag: &SourceTag) {
        if let Ok(mut backoff) = self.poll_backoff.lock() {
            backoff.remove(tag);
        }
    }

    /// Cached poll memory, seeded from the task row on first sight.
    fn poll_memory(&self, task_id: &str, task: &TaskRow, now: f64) -> PollMemory {
        if let Ok(cache) = self.poll_cache.lock()
            && let Some(memory) = cache.get(task_id)
        {
            return memory.clone();
        }
        PollMemory {
            enqueued_at: task.started_at.unwrap_or(task.created_at),
            last_bytes: 0,
            last_move_at: task.last_polled_at.unwrap_or(now),
        }
    }

    /// Store refreshed poll memory.
    fn store_poll_memory(&self, task_id: &str, memory: &PollMemory) {
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.insert(task_id.to_owned(), memory.clone());
        }
    }

    /// Apply one watchdog verdict to its task and attempt.
    #[allow(clippy::too_many_arguments)]
    async fn apply_verdict(
        self: &Arc<Self>,
        pass: &Pass,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Arc<dyn DownloadSource>,
        handle: &SourceHandle,
        progress: super::downloads::sources::TransferProgress,
        sample: PollSample,
    ) {
        use super::downloads::watchdog::WatchdogOutcome;
        let now = pass.now;
        let reason = match pass.watchdog.evaluate(&sample) {
            WatchdogOutcome::Continue => {
                self.record_progress(pass, task, &progress).await;
                return;
            }
            WatchdogOutcome::Completed if self.landing.is_some() => {
                self.start_landing(task, attempt, source, handle, now).await;
                return;
            }
            WatchdogOutcome::Completed => {
                let task_id = task.id.clone();
                let attempt_id = attempt.id.clone();
                self.step("downloads.complete", move |store| {
                    store.finalize_task_and_attempt(
                        &task_id,
                        TaskStatus::Completed,
                        now,
                        None,
                        Some(&attempt_id),
                        false,
                    )
                })
                .await;
                if let Ok(mut cache) = self.poll_cache.lock() {
                    cache.remove(&task.id);
                }
                super::plugin_events::download_event(
                    &self.plugins,
                    droppedneedle_plugins::runtime::EventKind::DownloadCompleted,
                    task,
                    &source.tag().to_string(),
                    "completed",
                );
                return;
            }
            WatchdogOutcome::Terminal => format!(
                "source batch terminal ({} files succeeded)",
                progress.succeeded_filenames.len()
            ),
            WatchdogOutcome::Stalled => "transfer stalled".to_owned(),
            WatchdogOutcome::QueuedTimeout => "stuck in the remote queue".to_owned(),
            WatchdogOutcome::MaterializeTimeout => {
                "no transfer materialized after enqueue".to_owned()
            }
            WatchdogOutcome::Deadline => "poll deadline hit".to_owned(),
        };
        self.fail_over(
            now,
            task,
            attempt,
            source,
            handle,
            &reason,
            Some(QuarantineReason::DownloadFailed),
        )
        .await;
    }

    /// Fail one attempt over to the next candidate: blocklist the failed
    /// release under `quarantine` (never on a local fault), discard client
    /// records, then settle the attempt and requeue the task in one write
    /// so the next pass enqueues the following candidate.
    #[allow(clippy::too_many_arguments)]
    async fn fail_over(
        &self,
        now: f64,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Arc<dyn DownloadSource>,
        handle: &SourceHandle,
        reason: &str,
        quarantine: Option<QuarantineReason>,
    ) {
        if let Some(quarantine) = quarantine {
            self.quarantine_failed(task, attempt, handle, reason, quarantine, now)
                .await;
        }
        let settled_state = match source.discard(handle).await {
            Ok(_) => AttemptState::Complete,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "discard failed; cleanup will retry");
                AttemptState::CleanupPending
            }
        };
        let attempt_id = attempt.id.clone();
        let revision = attempt.row_revision;
        let task_id = task.id.clone();
        let reason = reason.to_owned();
        self.step("downloads.fail_over", move |store| {
            store.transition_attempt(
                &attempt_id,
                revision,
                settled_state,
                now,
                Some("discard"),
                None,
                true,
            )?;
            store.transition_task(&task_id, TaskStatus::Queued, now, Some(&reason))
        })
        .await;
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.remove(&task.id);
        }
    }

    /// Blocklist a failed release by source identity, scoped to the album
    /// for retry-clearing. Local faults (disk, mount) are never
    /// quarantined: the backoff'd retry re-grabs once the environment
    /// recovers.
    async fn quarantine_failed(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        handle: &SourceHandle,
        reason: &str,
        quarantine: QuarantineReason,
        now: f64,
    ) {
        if is_local_fault(Some(reason)) {
            return;
        }
        let identities = failover_identities(handle);
        if identities.is_empty() {
            return;
        }
        let source = attempt.source.to_string();
        let scope = if task.release_group_mbid.is_empty() {
            None
        } else {
            Some(task.release_group_mbid.clone())
        };
        self.step("downloads.quarantine", move |store| {
            for identity in &identities {
                store.record_quarantine(
                    &source,
                    identity,
                    quarantine.as_str(),
                    scope.as_deref(),
                    now,
                    QUARANTINE_TTL_SECONDS,
                )?;
            }
            Ok(())
        })
        .await;
    }

    /// Spawn successors for terminal tasks whose retry backoff elapsed.
    /// The successor id is deterministic per (task, generation), and the
    /// existence check, detail copy and insert share one write, so a crash
    /// or a failed insert simply retries on the next pass.
    async fn retry_pass(&self, pass: &Pass) {
        let now = pass.now;
        let max = i64::from(pass.config.retry.auto_retry_max());
        if max == 0 {
            return;
        }
        let retryable = match self.journal.read_retryable(max).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "retryable download read failed");
                return;
            }
        };
        for task in retryable {
            let anchor = task.completed_at.unwrap_or(task.updated_at);
            let due = pass.config.retry.next_retry_at(
                u32::try_from(task.retry_count.max(0)).unwrap_or(u32::MAX),
                anchor,
                task.status.as_str(),
            );
            if due.is_none_or(|at| at > now) {
                continue;
            }
            let Some(row) = successor_row(&task) else {
                continue;
            };
            let successor = row.id.clone();
            let next = row.clone();
            let source = task.clone();
            let spawned = self
                .step("downloads.retry", move |store| {
                    // Held tracks gate their task: re-downloading a held
                    // track loops.
                    if store.has_unresolved_held_for_task(&source.id)? {
                        return Ok(false);
                    }
                    // The request follows its download onto the retry.
                    store.spawn_successor(&source, &row, now)
                })
                .await;
            if spawned == Some(true) {
                write_successor_manifest(&pass.config.staging_root, &task.id, &next).await;
                tracing::info!(
                    task_id = %task.id,
                    successor = %successor,
                    "download auto-retry spawned"
                );
            }
        }
    }

    /// Claim due cleanup rows and settle them: abort a cancelled task's
    /// live transfer, discard client records, then mark complete; a
    /// failure defers with backoff.
    async fn cleanup_pass(&self, pass: &Pass) {
        let now = pass.now;
        let worker_id = pass.config.worker_id.clone();
        let Some(claimed) = self
            .step("downloads.cleanup_claim", move |store| {
                store.claim_cleanup_attempts(
                    &worker_id,
                    now,
                    FAILOVER_CLAIM_LIMIT,
                    FAILOVER_LEASE_SECONDS,
                )
            })
            .await
        else {
            return;
        };
        for attempt in claimed {
            self.cleanup_one(pass, &attempt).await;
        }
        self.report_cleanup_health().await;
    }

    /// Show downloads whose source files keep failing to clear on service
    /// health, or clear the warning once none are left. A read failure
    /// leaves the entry as it is.
    async fn report_cleanup_health(&self) {
        let Some(count) = self
            .read_step("downloads.cleanup_count", |store| {
                store.cleanup_warning_count()
            })
            .await
        else {
            return;
        };
        if count == 0 {
            self.health.heal(CLEANUP_HEALTH_SERVICE);
            return;
        }
        let plural = if count == 1 { "" } else { "s" };
        self.health.report(
            CLEANUP_HEALTH_SERVICE,
            "source files",
            format!(
                "Temporary files couldn't be removed for {count} download{plural}. \
                 Your library is safe. Retrying automatically."
            ),
            CLEANUP_HEALTH_TTL,
            Instant::now(),
        );
    }

    /// Settle one claimed cleanup row.
    async fn cleanup_one(&self, pass: &Pass, attempt: &AttemptRow) {
        let now = pass.now;
        if self.landing_in_flight(&attempt.task_id) {
            // A landing (a reimport) is reading these files: try later.
            self.defer_cleanup(attempt, "landing_in_flight", now).await;
            return;
        }
        let read = async {
            Ok::<_, StoreError>((
                self.journal.read_attempt_handle(&attempt.id).await?,
                self.journal.read_task(&attempt.task_id).await?,
            ))
        };
        let (handle_json, task) = match read.await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(attempt_id = %attempt.id, %error, "cleanup read failed");
                return;
            }
        };
        let decoded = handle_json.and_then(|json| match serde_json::from_str(&json) {
            Ok(handle) => Some(handle),
            Err(error) => {
                tracing::warn!(
                    attempt_id = %attempt.id,
                    %error,
                    "stored client handle does not decode; cleaning by job name"
                );
                None
            }
        });
        let handle: SourceHandle = decoded.unwrap_or_else(|| SourceHandle {
            job_name: attempt.job_name.clone(),
            ..SourceHandle::new(attempt.source.clone())
        });
        let Some(source) = pass.source_for(&attempt.source) else {
            // No adapter for this source: defer with backoff rather than
            // spinning on a row this worker can never settle.
            self.defer_cleanup(attempt, "no_adapter", now).await;
            return;
        };
        let cancelled = task.is_some_and(|task| task.status == TaskStatus::Cancelled);
        if cancelled
            && attempt.disposition == "discard"
            && let Err(error) = source.abort(&handle).await
        {
            tracing::warn!(attempt_id = %attempt.id, %error, "cancelled transfer abort failed");
            self.defer_cleanup(attempt, "abort_failed", now).await;
            return;
        }
        match source.discard(&handle).await {
            Ok(_) => {
                let attempt_id = attempt.id.clone();
                let revision = attempt.row_revision;
                let disposition = attempt.disposition.clone();
                self.step("downloads.cleanup_done", move |store| {
                    store.transition_attempt(
                        &attempt_id,
                        revision,
                        AttemptState::Complete,
                        now,
                        Some(&disposition),
                        None,
                        true,
                    )
                })
                .await;
            }
            Err(error) => {
                tracing::warn!(attempt_id = %attempt.id, %error, "cleanup discard failed");
                self.defer_cleanup(attempt, "discard_failed", now).await;
            }
        }
    }

    /// Push one cleanup row out with backoff.
    async fn defer_cleanup(&self, attempt: &AttemptRow, code: &'static str, now: f64) {
        let attempt_id = attempt.id.clone();
        let revision = attempt.row_revision;
        self.step("downloads.cleanup_defer", move |store| {
            store.record_cleanup_failure(&attempt_id, revision, code, now)
        })
        .await;
    }

    /// Walk the complete dirs and remove proven debris. Every ambiguous
    /// answer keeps the folder.
    async fn orphan_pass(&self, pass: &Pass) {
        for (source_tag, root) in &pass.config.orphan_roots {
            self.orphan_root(pass, source_tag, root).await;
        }
    }

    /// Reconcile one complete dir.
    async fn orphan_root(&self, pass: &Pass, source_tag: &SourceTag, root: &Path) {
        let mut entries = match tokio::fs::read_dir(root).await {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(root = %root.display(), %error, "orphan sweep cannot read root");
                return;
            }
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(root = %root.display(), %error, "orphan sweep entry failed");
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_symlink = entry
                .file_type()
                .await
                .map(|kind| kind.is_symlink())
                .unwrap_or(true);
            let Some((task_id, job_name)) = job_name_parts(&name) else {
                continue;
            };
            let evidence = self
                .orphan_evidence(pass, source_tag, &task_id, &job_name, &entry.path())
                .await;
            match evaluate_orphan(&name, is_symlink, evidence) {
                OrphanDecision::Remove => {
                    self.remove_orphan(pass, source_tag, &task_id, &job_name, root, &entry.path())
                        .await;
                }
                OrphanDecision::Keep | OrphanDecision::Ignore => {}
            }
        }
    }

    /// Gather ownership evidence for one candidate folder. `None` fails
    /// closed (the folder stays).
    async fn orphan_evidence(
        &self,
        pass: &Pass,
        source_tag: &SourceTag,
        task_id: &str,
        job_name: &str,
        path: &Path,
    ) -> Option<OrphanEvidence> {
        let has_cleanup_debt = self
            .ownership
            .has_cleanup_debt(&source_tag.to_string(), task_id, job_name)
            .await
            .ok()?;
        let task_status = self.ownership.task_status(task_id).await.ok()?;
        let task_active = task_status.is_some_and(|status| {
            matches!(status.as_str(), "queued" | "downloading" | "processing")
        });
        let bundles_settled = self.ownership.bundles_settled(task_id).await.ok()?;
        let source = pass.source_for(source_tag)?;
        let handle = SourceHandle {
            job_name: job_name.to_owned(),
            ..SourceHandle::new(source_tag.clone())
        };
        // A source that cannot tell whether it still owns the job (slskd:
        // the folder name carries no peer or filenames) answers `None`,
        // and the folder stays.
        let client_job_active = source.job_active(&handle).await?;
        // Real client-side mount truth; a lookup error fails closed.
        let mount_healthy = source.mount_healthy(&handle).await?;
        let age_seconds = tokio::fs::metadata(path)
            .await
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .map(|age| age.as_secs_f64())
            .unwrap_or(0.0);
        Some(OrphanEvidence {
            has_cleanup_debt,
            task_active,
            bundles_settled,
            mount_healthy,
            client_job_active,
            age_seconds,
        })
    }

    /// Discard client records, then remove the folder. A discard failure
    /// keeps the folder for the next sweep. Confinement is re-asserted
    /// immediately before removal: the path must still sit under the
    /// walked root and must not be a symlink.
    async fn remove_orphan(
        &self,
        pass: &Pass,
        source_tag: &SourceTag,
        task_id: &str,
        job_name: &str,
        root: &Path,
        path: &Path,
    ) {
        let is_symlink = tokio::fs::symlink_metadata(path)
            .await
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(true);
        if !orphan_remove_allowed(root, path, is_symlink) {
            tracing::warn!(task_id, path = %path.display(), "orphan remove refused");
            return;
        }
        let Some(source) = pass.source_for(source_tag) else {
            return;
        };
        let handle = SourceHandle {
            job_name: job_name.to_owned(),
            ..SourceHandle::new(source_tag.clone())
        };
        if let Err(error) = source.discard(&handle).await {
            tracing::warn!(task_id, %error, "orphan client discard failed; folder kept");
            return;
        }
        if let Err(error) = tokio::fs::remove_dir_all(path).await {
            tracing::warn!(task_id, %error, "orphan folder remove failed");
        } else {
            tracing::info!(task_id, path = %path.display(), "orphan folder removed");
        }
    }
}

/// Newest attempt with a client handle that is still acquiring or in use.
fn live_attempt(attempts: &[Attempt]) -> Option<&Attempt> {
    attempts.iter().rev().find(|attempt| {
        matches!(
            attempt.row.state,
            AttemptState::Acquiring | AttemptState::InUse
        ) && attempt.handle.is_some()
    })
}

/// Attempts with a client handle, optionally for one source only.
/// Handle-less rows never reached the client and do not consume a slot.
fn handled_count(attempts: &[Attempt], source: Option<&SourceTag>) -> i64 {
    attempts
        .iter()
        .filter(|attempt| attempt.handle.is_some())
        .filter(|attempt| source.is_none_or(|tag| attempt.row.source == *tag))
        .count() as i64
}

/// Whether the failover walk is spent: `max` attempts occupy indices
/// `0..max`, so a next index at or past `max` fails the task.
fn failover_exhausted(handled: i64, max: i64) -> bool {
    handled >= max
}

/// The retry successor row for a settled task: same ask, one generation
/// on. `None` for a task that is not settled yet.
fn successor_row(task: &TaskRow) -> Option<NewTask> {
    let spawn = plan_retry(&task.id, task.status, &task.origin, task.retry_count)?;
    // Successor ids keep the 32-hex shape the orphan parser recognises;
    // the retry generation rides in `retry_count`.
    Some(NewTask {
        id: hex_task_id(&spawn.task_id),
        user_id: task.user_id.clone(),
        artist_name: task.artist_name.clone(),
        album_title: task.album_title.clone(),
        release_group_mbid: task.release_group_mbid.clone(),
        origin: spawn.origin,
        retry_count: spawn.retry_count,
    })
}

/// Fold a retry successor id into the 32-hex shape: the planned
/// `{task}-r{n}` suffix breaks the orphan parser, so hash it down while
/// the generation rides in `retry_count`. FNV-1a, stable across processes
/// (the std `DefaultHasher` is process-randomised, so a successor id must
/// never depend on it).
fn hex_task_id(planned: &str) -> String {
    fn fnv1a(text: &str, seed: u64) -> u64 {
        let mut hash = seed;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
    let first = fnv1a(planned, 0xcbf2_9ce4_8422_2325);
    let second = fnv1a(planned, 0x8422_2325_cbf2_9ce4 ^ first);
    format!("{first:016x}{second:016x}")
}

/// Run the worker on the supervisor: a pass at once, then one on every
/// wake and at least every [`WORKER_INTERVAL`]. At shutdown the landings it
/// started finish within the grace, under [`DOWNLOAD_LANDINGS_JOB`].
pub fn spawn_download_worker(
    worker: Arc<DownloadWorker>,
    supervisor: &Supervisor,
) -> Result<(), AlreadyRunning> {
    let mut wake = worker.wake.subscribe();
    wake.mark_changed();
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    supervisor.on_wakeup(
        DOWNLOAD_WORKER_JOB,
        wake,
        Schedule::new(WORKER_INTERVAL),
        WorkerTick {
            worker: Arc::clone(&worker),
            gate: Arc::clone(&gate),
            pass: 0,
        },
    )?;
    supervisor.one_shot(DOWNLOAD_LANDINGS_JOB, move |ctx| async move {
        ctx.token().cancelled().await;
        // A pass still running may start a landing: wait for it first.
        let _pass = gate.lock().await;
        worker.wait_for_landings().await;
        Ok(())
    })
}

/// One worker pass. The gate keeps a pass and the shutdown wait for
/// landings apart, so no landing starts after that wait began.
struct WorkerTick {
    worker: Arc<DownloadWorker>,
    gate: Arc<tokio::sync::Mutex<()>>,
    pass: u64,
}

impl Tick for WorkerTick {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        let _pass = self.gate.lock().await;
        if ctx.token().is_cancelled() {
            return Ok(Next::Finish);
        }
        self.worker.run_once(self.pass).await;
        self.pass = self.pass.wrapping_add(1);
        Ok(Next::Continue)
    }
}

/// Pure removal gate: the folder must sit strictly under the walked
/// root and must not be a symlink.
fn orphan_remove_allowed(root: &Path, path: &Path, is_symlink: bool) -> bool {
    !is_symlink && path != root && path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successor_ids_are_stable_32_hex() {
        let first = hex_task_id("task-r1");
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(first, hex_task_id("task-r1"));
        assert_ne!(first, hex_task_id("task-r2"));
    }

    #[test]
    fn orphan_removal_stays_confined() {
        let root = Path::new("/data/complete");
        assert!(orphan_remove_allowed(
            root,
            &root.join("droppedneedle-job"),
            false
        ));
        assert!(!orphan_remove_allowed(root, root, false));
        assert!(!orphan_remove_allowed(
            root,
            Path::new("/data/other/job"),
            false
        ));
        assert!(!orphan_remove_allowed(
            root,
            &root.join("droppedneedle-job"),
            true
        ));
    }
}
