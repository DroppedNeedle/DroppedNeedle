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
//!   verdicts complete, fail over, or requeue their tasks;
//! - retry: terminal tasks whose backoff elapsed spawn successors;
//! - cleanup: claimed journal rows abort and discard client records and
//!   settle;
//! - orphans (hourly): complete-dir debris with no owner is removed.
//!
//! Sources and tuning are resolved at the start of every pass, so saved
//! settings take effect on the next pass without a restart. The loop
//! registers as the durable `download-worker` job, heartbeats every pass
//! and stops on the shared shutdown watch. Journal failures are logged and
//! skip the affected item; a read error never stands in for "no rows".

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::dispatch::Journal;
use super::downloads::manifest::{DownloadManifest, ExpectedFile, ManifestCodec};
use super::downloads::orphans::{
    OrphanDecision, OrphanEvidence, RecycleBin, evaluate_orphan, job_name_parts,
};
use super::downloads::quarantine::{
    QUARANTINE_TTL_SECONDS, QuarantineReason, failover_identities, is_local_fault,
};
use super::downloads::recovery::{
    FAILOVER_CLAIM_LIMIT, FAILOVER_LEASE_SECONDS, StartupAction, StartupCtx, classify_startup,
    plan_retry,
};
use super::downloads::sources::OrphanOwnership;
use super::downloads::sources::{DownloadSource, SourceError, SourceHandle};
use super::downloads::state::{AttemptState, TaskStatus};
use super::downloads::store::{AttemptRow, NewTask, StoreError, TaskRow};
use super::downloads::watchdog::{PollSample, RetryPolicy, Watchdog, WatchdogConfig};
use super::landing::specs::Disposition;
use super::landing::{LandingReport, LandingResult, LandingService};
use super::plugin_source::PluginDownloadSource;
use super::sources::{JournalOwnership, SabnzbdSource, SlskdSource};
use crate::db::{DurableWorkWakeups, JobKind, JobState, WriteLane};

/// Registry name for the worker loop.
pub const DOWNLOAD_WORKER_JOB: &str = "download-worker";

/// Steady-state pass cadence.
pub const WORKER_INTERVAL: Duration = Duration::from_secs(30);

/// Orphan sweep runs every Nth pass (hourly at the 30s cadence).
const ORPHAN_EVERY_NTH_PASS: u64 = 120;

/// Enqueue backoff after a pass where no source could serve a task.
const ENQUEUE_BACKOFF_SECONDS: f64 = 15.0 * 60.0;

/// Current unix time as float seconds.
fn now_unix_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
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
        .run("downloads.recovery.list", |store| {
            store.list_active(&[
                TaskStatus::Queued,
                TaskStatus::Downloading,
                TaskStatus::Processing,
            ])
        })
        .await?;
    let mut report = RecoveryReport::default();
    for task in tasks {
        let manifest = read_manifest(staging_root, &task.id).await;
        let manifest_matches_attempt = match manifest.as_ref().and_then(|m| m.attempt_id.clone()) {
            None => None,
            Some(linked) => {
                let task_id = task.id.clone();
                journal
                    .run("downloads.recovery.attempts", move |store| {
                        store.list_attempts(&task_id)
                    })
                    .await
                    .ok()
                    .map(|attempts| attempts.iter().any(|attempt| attempt.id == linked))
            }
        };
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
                    .await?;
                report.restarted += 1;
            }
            StartupAction::Noop => {}
        }
    }
    Ok(report)
}

/// A task's manifest, when present and readable.
async fn read_manifest(staging_root: &Path, task_id: &str) -> Option<DownloadManifest> {
    let path = ManifestCodec::checked_path(staging_root, task_id)?;
    let bytes = tokio::fs::read(path).await.ok()?;
    ManifestCodec.decode(&bytes).ok()
}

/// One download source behind the fetch seam. Cheap to clone, so each
/// pass can add the plugin sources enabled right now to the built-in ones.
#[derive(Clone)]
pub enum Source {
    /// Soulseek via slskd.
    Slskd(Arc<SlskdSource>),
    /// Usenet via SABnzbd.
    Sab(Arc<SabnzbdSource>),
    /// A plugin's download client (`plugin:<name>`).
    Plugin(Arc<PluginDownloadSource>),
}

impl Source {
    fn name(&self) -> &str {
        match self {
            Self::Slskd(_) => "slskd",
            Self::Sab(_) => "sabnzbd",
            Self::Plugin(source) => source.key(),
        }
    }

    /// Journal source tag (`soulseek`, `usenet`, `plugin:<name>`).
    fn journal_source(&self) -> &str {
        match self {
            Self::Slskd(_) => "soulseek",
            Self::Sab(_) => "usenet",
            Self::Plugin(source) => source.key(),
        }
    }

    async fn enqueue(
        &self,
        task_id: &str,
        candidate_index: i64,
    ) -> Result<SourceHandle, SourceError> {
        match self {
            Self::Slskd(source) => source.enqueue(task_id, candidate_index).await,
            Self::Sab(source) => source.enqueue(task_id, candidate_index).await,
            Self::Plugin(source) => source.enqueue(task_id, candidate_index).await,
        }
    }

    async fn poll(
        &self,
        handle: &SourceHandle,
    ) -> Result<super::downloads::sources::TransferProgress, SourceError> {
        match self {
            Self::Slskd(source) => source.poll(handle).await,
            Self::Sab(source) => source.poll(handle).await,
            Self::Plugin(source) => source.poll(handle).await,
        }
    }

    async fn discard(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        match self {
            Self::Slskd(source) => source.discard(handle).await,
            Self::Sab(source) => source.discard(handle).await,
            Self::Plugin(source) => source.discard(handle).await,
        }
    }

    async fn abort(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        match self {
            Self::Slskd(source) => source.abort(handle).await,
            Self::Sab(source) => source.abort(handle).await,
            Self::Plugin(source) => source.abort(handle).await,
        }
    }

    /// The landed files (or job folder) for a finished handle.
    async fn landed_paths(&self, handle: &SourceHandle) -> Result<Vec<PathBuf>, SourceError> {
        match self {
            Self::Slskd(source) => source.locate_files(handle).await,
            Self::Sab(source) => source.inspect(handle).await.map(|seen| seen.paths),
            Self::Plugin(source) => source.inspect(handle).await.map(|seen| seen.paths),
        }
    }

    /// Whether the client still owns one job (orphan evidence). `None`
    /// means the lookup failed and the folder must stay.
    async fn job_active(&self, handle: &SourceHandle) -> Option<bool> {
        match self {
            Self::Slskd(_) | Self::Plugin(_) => None,
            Self::Sab(source) => source.job_present(handle).await.ok(),
        }
    }

    /// Client-side mount health for orphan evidence. `None` fails closed
    /// (the folder stays). Soulseek has no cheap mount probe and its
    /// folders always keep anyway, so it answers `None`.
    async fn mount_healthy(&self, handle: &SourceHandle) -> Option<bool> {
        match self {
            Self::Slskd(_) => None,
            Self::Sab(source) => source
                .inspect(handle)
                .await
                .map(|seen| seen.mount_healthy)
                .ok(),
            Self::Plugin(source) => source
                .inspect(handle)
                .await
                .map(|seen| seen.mount_healthy)
                .ok(),
        }
    }
}

/// Worker tuning, read at the start of every pass.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Pass cadence.
    pub interval: Duration,
    /// Concurrent live downloads.
    pub max_concurrent_downloads: usize,
    /// Failover attempts per task across all sources.
    pub max_failover_attempts: i64,
    /// Auto-retry timing.
    pub retry: RetryPolicy,
    /// Watchdog timing.
    pub watchdog: WatchdogConfig,
    /// Journal source tags in try order (`soulseek`, `usenet`).
    pub source_order: Vec<String>,
    /// Per-task staging root (manifests live beneath it).
    pub staging_root: PathBuf,
    /// Upgrade recycle bin, when resolvable.
    pub recycle: Option<RecycleBin>,
    /// Complete dirs to walk as `(journal source, dir)`.
    pub orphan_roots: Vec<(String, PathBuf)>,
    /// Lease owner name for cleanup claims.
    pub worker_id: String,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            interval: WORKER_INTERVAL,
            max_concurrent_downloads: 3,
            max_failover_attempts: 3,
            retry: RetryPolicy::default(),
            watchdog: WatchdogConfig::default(),
            source_order: vec!["soulseek".to_owned(), "usenet".to_owned()],
            staging_root: PathBuf::from("staging"),
            recycle: None,
            orphan_roots: Vec::new(),
            worker_id: "download-worker".to_owned(),
        }
    }
}

/// The configured sources for one pass.
pub type SourceSet = Arc<Vec<Source>>;
/// Resolves the configured sources; called once per pass.
pub type SourceProvider = Arc<dyn Fn() -> SourceSet + Send + Sync>;
/// Resolves the worker tuning; called once per pass.
pub type ConfigProvider = Arc<dyn Fn() -> WorkerConfig + Send + Sync>;

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
    fn ordered_sources(&self) -> Vec<&Source> {
        let mut ordered = Vec::with_capacity(self.sources.len());
        for wanted in &self.config.source_order {
            for source in self.sources.iter() {
                if source.journal_source() == wanted {
                    ordered.push(source);
                }
            }
        }
        for source in self.sources.iter() {
            if !ordered.iter().any(|placed| std::ptr::eq(*placed, source)) {
                ordered.push(source);
            }
        }
        ordered
    }

    /// The adapter for one journal source tag.
    fn source_for(&self, tag: &str) -> Option<&Source> {
        self.sources
            .iter()
            .find(|source| source.journal_source() == tag)
    }
}

/// One attempt plus its decoded client handle, when it has one.
#[derive(Debug, Clone)]
struct Attempt {
    row: AttemptRow,
    handle: Option<SourceHandle>,
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
    sources: SourceProvider,
    config: ConfigProvider,
    ownership: JournalOwnership,
    poll_cache: Mutex<HashMap<String, PollMemory>>,
    enqueue_not_before: Mutex<HashMap<String, f64>>,
    unsearchable_warned: Mutex<HashSet<String>>,
    plugins: super::wiring::PluginSlot,
    landing: Option<Arc<LandingService>>,
    landing_waits: Mutex<HashMap<String, u32>>,
    settled: Option<SettledHook>,
}

/// Called after a landing settles a task, so its requests and wanted
/// watches resolve right away instead of on the next status sync.
pub type SettledHook =
    Arc<dyn Fn(TaskRow, TaskStatus) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

/// Passes a landing may wait on files that are not ready before the task
/// fails (about five minutes at the 30-second cadence).
const MAX_LANDING_WAITS: u32 = 10;

impl DownloadWorker {
    /// Wire the worker over the shared journal plus live source and config
    /// resolvers.
    pub fn new(journal: Arc<Journal>, sources: SourceProvider, config: ConfigProvider) -> Self {
        let ownership = JournalOwnership::new(journal.clone());
        Self {
            journal,
            sources,
            config,
            ownership,
            poll_cache: Mutex::new(HashMap::new()),
            enqueue_not_before: Mutex::new(HashMap::new()),
            unsearchable_warned: Mutex::new(HashSet::new()),
            plugins: Default::default(),
            landing: None,
            landing_waits: Mutex::new(HashMap::new()),
            settled: None,
        }
    }

    /// Import finished downloads through the landing instead of settling
    /// them as completed unseen.
    pub fn with_landing(mut self, landing: Arc<LandingService>) -> Self {
        self.landing = Some(landing);
        self
    }

    /// Resolve requests as soon as a landing settles a task.
    pub fn with_settled(mut self, hook: SettledHook) -> Self {
        self.settled = Some(hook);
        self
    }

    /// Announce download starts, completions and failures to `subscriber`
    /// plugins through this slot.
    pub fn with_plugin_events(mut self, plugins: super::wiring::PluginSlot) -> Self {
        self.plugins = plugins;
        self
    }

    /// Worker over a fixed source list and fixed tuning.
    pub fn fixed(journal: Arc<Journal>, sources: Vec<Source>, config: WorkerConfig) -> Self {
        let sources: SourceSet = Arc::new(sources);
        Self::new(
            journal,
            Arc::new(move || sources.clone()),
            Arc::new(move || config.clone()),
        )
    }

    /// Run every steady-state pass once, in order. The orphan sweep only
    /// runs when `pass` hits its hourly slot.
    pub async fn run_once(&self, pass: u64) {
        let config = (self.config)();
        let pass_ctx = Pass {
            watchdog: Watchdog::new(config.watchdog.clone()),
            sources: (self.sources)(),
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

    /// Run one journal step, logging a failure with its name.
    async fn step<R, F>(&self, name: &'static str, op: F) -> Option<R>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(
                &'a super::downloads::store::DownloadStore<'b>,
            ) -> Result<R, StoreError>
            + Send
            + 'static,
    {
        match self.journal.run(name, op).await {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(step = name, %error, "download journal step failed");
                None
            }
        }
    }

    /// A task's attempts with their decoded handles, in one read.
    async fn attempts(&self, task_id: &str) -> Option<Vec<Attempt>> {
        let task_id = task_id.to_owned();
        self.step("downloads.attempts", move |store| {
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

    /// Enqueue queued tasks while under the concurrency cap.
    async fn enqueue_pass(&self, pass: &Pass) {
        let Some(live) = self
            .step("downloads.live", |store| {
                store.list_active(&[TaskStatus::Downloading, TaskStatus::Processing])
            })
            .await
        else {
            // Without the live count the cap cannot hold; skip this pass.
            return;
        };
        let Some(queued) = self
            .step("downloads.queued", |store| {
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
    }

    /// Enqueue one queued task. Answers whether a fetch started.
    async fn enqueue_one(&self, pass: &Pass, task: &TaskRow) -> bool {
        let now = pass.now;
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
            self.fail_unserved(task, &attempts, now).await;
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
        for source in pass.ordered_sources() {
            // Each source walks its own candidate list: the index is the
            // number of attempts this source already handled.
            let source_index = handled_count(&attempts, Some(source.journal_source()));
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
                    if !matches!(error, SourceError::Rejected(_)) {
                        every_source_refused = false;
                    }
                    tracing::warn!(
                        task_id = %task.id,
                        source = source.name(),
                        %error,
                        "enqueue failed; trying the next source"
                    );
                }
            }
        }
        if every_source_refused && !pass.sources.is_empty() {
            // Every configured source answered that it has nothing for
            // this release: the candidate lists are spent.
            self.fail_unserved(task, &attempts, now).await;
            return false;
        }
        self.backoff(&task.id, now);
        false
    }

    /// Fail a task no source can serve, keeping its last attempt.
    async fn fail_unserved(&self, task: &TaskRow, attempts: &[Attempt], now: f64) {
        let task_id = task.id.clone();
        let last_attempt = attempts.last().map(|attempt| attempt.row.id.clone());
        self.step("downloads.fail_unserved", move |store| {
            store.finalize_task_and_attempt(
                &task_id,
                TaskStatus::Failed,
                now,
                Some("no source could serve this release"),
                last_attempt.as_deref(),
                true,
            )
        })
        .await;
        super::plugin_events::download_event(
            &self.plugins,
            crate::plugins::runtime::EventKind::DownloadFailed,
            task,
            &task.source,
            "failed",
        );
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
        source: &Source,
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
        let journal_source = source.journal_source().to_owned();
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
            crate::plugins::runtime::EventKind::DownloadStarted,
            task,
            source.journal_source(),
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
                naming_template: String::new(),
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
            });
        manifest.handle = Some(super::downloads::manifest::TaskHandle {
            source: handle.source.clone(),
            username: handle.username.clone(),
            filenames: handle.filenames.clone(),
            job_name: handle.job_name.clone(),
        });
        manifest.attempt_id = Some(attempt_id.to_owned());
        if manifest.target_files.is_empty() {
            manifest.target_files = handle
                .filenames
                .iter()
                .map(|filename| ExpectedFile {
                    filename: filename.clone(),
                    size: 0,
                    duration: None,
                })
                .collect();
        }
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

    /// Poll every live task through the watchdog.
    async fn poll_pass(&self, pass: &Pass) {
        let Some(tasks) = self
            .step("downloads.live", |store| {
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
        for task in &tasks {
            self.poll_one(pass, task).await;
        }
    }

    /// Poll one live task: judge the sample, then complete, fail over, or
    /// keep waiting. Poll errors never fail the task directly; the stall
    /// math ages a dead client out into failover instead.
    async fn poll_one(&self, pass: &Pass, task: &TaskRow) {
        let now = pass.now;
        let Some(attempts) = self.attempts(&task.id).await else {
            // A read failure is not "no attempt": leave the task alone.
            return;
        };
        let Some(attempt) = live_attempt(&attempts).cloned() else {
            // No pollable attempt: back to queued for a fresh enqueue.
            let task_id = task.id.clone();
            self.step("downloads.requeue", move |store| {
                store.transition_task(&task_id, TaskStatus::Queued, now, None)
            })
            .await;
            return;
        };
        let Some(handle) = attempt.handle.clone() else {
            return;
        };
        let Some(source) = pass.source_for(&attempt.row.source) else {
            // Plugin sources have no adapter yet; leave the row for a
            // worker that knows them rather than failing it here.
            return;
        };
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
            }
            Err(SourceError::Rejected(detail)) => {
                self.fail_over(
                    pass,
                    task,
                    &attempt.row,
                    source,
                    &handle,
                    &detail,
                    Some(QuarantineReason::DownloadFailed),
                )
                .await;
            }
            Err(error) => {
                // Unavailable or local fault: stamp the poll and let the
                // stall math age the task out; a momentary outage must
                // not fail a healthy transfer.
                tracing::warn!(task_id = %task.id, %error, "download poll failed");
                let task_id = task.id.clone();
                self.step("downloads.touch_poll", move |store| {
                    store.touch_poll(&task_id, now)
                })
                .await;
            }
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
        &self,
        pass: &Pass,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Source,
        handle: &SourceHandle,
        progress: super::downloads::sources::TransferProgress,
        sample: PollSample,
    ) {
        use super::downloads::watchdog::WatchdogOutcome;
        let now = pass.now;
        let reason = match pass.watchdog.evaluate(&sample) {
            WatchdogOutcome::Continue => {
                let task_id = task.id.clone();
                self.step("downloads.touch_poll", move |store| {
                    store.touch_poll(&task_id, now)
                })
                .await;
                return;
            }
            WatchdogOutcome::Completed if self.landing.is_some() => {
                self.land_finished(pass, task, attempt, source, handle)
                    .await;
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
                    crate::plugins::runtime::EventKind::DownloadCompleted,
                    task,
                    source.journal_source(),
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
            pass,
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
        pass: &Pass,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Source,
        handle: &SourceHandle,
        reason: &str,
        quarantine: Option<QuarantineReason>,
    ) {
        let now = pass.now;
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
        let identities = failover_identities(
            &attempt.source,
            &handle.username,
            &handle.filenames,
            &handle.job_name,
        );
        if identities.is_empty() {
            return;
        }
        let source = attempt.source.clone();
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

    /// A finished transfer: hand the files to the landing, then settle the
    /// task by what it decided. The task shows `processing` meanwhile.
    async fn land_finished(
        &self,
        pass: &Pass,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Source,
        handle: &SourceHandle,
    ) {
        let Some(landing) = self.landing.clone() else {
            return;
        };
        let now = pass.now;
        if task.status != TaskStatus::Processing {
            let task_id = task.id.clone();
            self.step("downloads.processing", move |store| {
                store.transition_task(&task_id, TaskStatus::Processing, now, None)
            })
            .await;
        }
        let paths = match source.landed_paths(handle).await {
            Ok(paths) => paths,
            Err(error) => {
                let detail = format!("the download client could not list the files: {error}");
                self.wait_or_fail(task, attempt, &detail, now).await;
                return;
            }
        };
        let manifest = read_manifest(&pass.config.staging_root, &task.id).await;
        let report = landing
            .land(task, Some(&attempt.id), manifest.as_ref(), paths)
            .await;
        match report.result {
            LandingResult::Rejected(rejection)
                if rejection.disposition == Disposition::Permanent =>
            {
                self.forget_landing(&task.id);
                self.fail_over(
                    pass,
                    task,
                    attempt,
                    source,
                    handle,
                    &rejection.detail,
                    rejection.quarantine,
                )
                .await;
            }
            LandingResult::Rejected(rejection)
                if rejection.disposition == Disposition::Temporary =>
            {
                self.wait_or_fail(task, attempt, &rejection.detail, now)
                    .await;
            }
            _ => self.settle_landing(task, attempt, report, now).await,
        }
    }

    /// Settle a task from its landing report: imported files complete or
    /// short-land it, held files fail it (the held gate pauses retries),
    /// and a local fault fails it with the files kept for a reimport.
    async fn settle_landing(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        report: LandingReport,
        now: f64,
    ) {
        match report.result {
            LandingResult::Imported { complete } => {
                let status = if complete {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Partial
                };
                self.settle(task, &attempt.id, status, None, false, now)
                    .await;
            }
            LandingResult::Held { detail, .. } => {
                let error = format!("Held for review: {detail}");
                self.settle(
                    task,
                    &attempt.id,
                    TaskStatus::Failed,
                    Some(&error),
                    false,
                    now,
                )
                .await;
            }
            LandingResult::Rejected(rejection) => {
                // A rejection the caller could not fail over (a reimport)
                // or our own fault: keep the files for another try.
                self.settle(
                    task,
                    &attempt.id,
                    TaskStatus::Failed,
                    Some(&rejection.detail),
                    true,
                    now,
                )
                .await;
            }
        }
    }

    /// Wait another pass for files that are not ready, up to
    /// [`MAX_LANDING_WAITS`]; then fail with the files kept.
    async fn wait_or_fail(&self, task: &TaskRow, attempt: &AttemptRow, detail: &str, now: f64) {
        let waited = self
            .landing_waits
            .lock()
            .map(|mut waits| {
                let count = waits.entry(task.id.clone()).or_insert(0);
                *count += 1;
                *count
            })
            .unwrap_or(MAX_LANDING_WAITS);
        if waited < MAX_LANDING_WAITS {
            tracing::info!(task_id = %task.id, waited, detail, "landing waits for the next pass");
            let task_id = task.id.clone();
            self.step("downloads.touch_poll", move |store| {
                store.touch_poll(&task_id, now)
            })
            .await;
            return;
        }
        self.settle(
            task,
            &attempt.id,
            TaskStatus::Failed,
            Some(detail),
            true,
            now,
        )
        .await;
    }

    /// Finalize one landed task and its attempt, announce it, and resolve
    /// its requests.
    async fn settle(
        &self,
        task: &TaskRow,
        attempt_id: &str,
        status: TaskStatus,
        error: Option<&str>,
        preserve_attempt: bool,
        now: f64,
    ) {
        self.forget_landing(&task.id);
        let task_id = task.id.clone();
        let attempt_id = attempt_id.to_owned();
        let error = error.map(str::to_owned);
        let settled = self
            .step("downloads.settle_landing", move |store| {
                store.finalize_task_and_attempt(
                    &task_id,
                    status,
                    now,
                    error.as_deref(),
                    Some(&attempt_id),
                    preserve_attempt,
                )
            })
            .await;
        if settled.is_none() {
            return;
        }
        let (kind, outcome) = match status {
            TaskStatus::Completed => (
                crate::plugins::runtime::EventKind::DownloadCompleted,
                "completed",
            ),
            TaskStatus::Partial => (
                crate::plugins::runtime::EventKind::DownloadCompleted,
                "partial",
            ),
            _ => (crate::plugins::runtime::EventKind::DownloadFailed, "failed"),
        };
        super::plugin_events::download_event(&self.plugins, kind, task, &task.source, outcome);
        if let Some(hook) = &self.settled {
            hook(task.clone(), status).await;
        }
    }

    /// Drop a task's poll memory and landing waits.
    fn forget_landing(&self, task_id: &str) {
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.remove(task_id);
        }
        if let Ok(mut waits) = self.landing_waits.lock() {
            waits.remove(task_id);
        }
    }

    /// Import a failed or short-landed task's files again, right away
    /// (v2 `reimport_task`): the files of its last attempt go through the
    /// landing as if the transfer had just finished, without a new search
    /// or download. `Ok(None)` when the task is missing or not
    /// reimportable; the returned row is the task after the landing.
    pub async fn reimport(&self, task_id: &str) -> Result<Option<TaskRow>, ReimportError> {
        let Some(landing) = self.landing.clone() else {
            return Err(ReimportError::Unavailable(
                "imports are not available yet".to_owned(),
            ));
        };
        let Some(task) = self
            .journal
            .read_task(task_id)
            .await
            .map_err(ReimportError::Journal)?
        else {
            return Ok(None);
        };
        if !matches!(task.status, TaskStatus::Failed | TaskStatus::Partial) {
            return Ok(None);
        }
        let Some(attempts) = self.attempts(task_id).await else {
            return Err(ReimportError::Journal(
                "download attempts unreadable".to_owned(),
            ));
        };
        let Some(attempt) = attempts.iter().rev().find(|attempt| {
            attempt.handle.is_some()
                && matches!(
                    attempt.row.state,
                    AttemptState::Preserved | AttemptState::CleanupPending | AttemptState::Complete
                )
        }) else {
            return Ok(None);
        };
        let Some(handle) = attempt.handle.clone() else {
            return Ok(None);
        };
        let sources = (self.sources)();
        let Some(source) = sources
            .iter()
            .find(|source| source.journal_source() == attempt.row.source)
            .cloned()
        else {
            return Err(ReimportError::Unavailable(format!(
                "the {} download client is not configured",
                attempt.row.source
            )));
        };
        let now = now_unix_f64();
        let begun = {
            let task_id = task_id.to_owned();
            self.journal
                .run_foreground("downloads.reimport", move |store| {
                    store.begin_reimport(&task_id, now)
                })
                .await
                .map_err(ReimportError::Journal)?
        };
        if !begun {
            return Ok(None);
        }
        let task = self
            .journal
            .read_task(task_id)
            .await
            .map_err(ReimportError::Journal)?
            .unwrap_or(task);
        // The attempt row moved since it was read only if another writer
        // touched it; re-read it so the settle's revision check holds.
        let row = self
            .attempts(task_id)
            .await
            .and_then(|fresh| {
                fresh
                    .into_iter()
                    .find(|fresh| fresh.row.id == attempt.row.id)
            })
            .map(|fresh| fresh.row)
            .unwrap_or_else(|| attempt.row.clone());
        match source.landed_paths(&handle).await {
            Ok(paths) => {
                let staging_root = (self.config)().staging_root;
                let manifest = read_manifest(&staging_root, &task.id).await;
                let report = landing
                    .land(&task, Some(&row.id), manifest.as_ref(), paths)
                    .await;
                self.settle_landing(&task, &row, report, now).await;
            }
            Err(error) => {
                let detail = format!("the download client could not list the files: {error}");
                self.settle(&task, &row.id, TaskStatus::Failed, Some(&detail), true, now)
                    .await;
            }
        }
        self.journal
            .read_task(task_id)
            .await
            .map_err(ReimportError::Journal)
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
            let Some(spawn) = plan_retry(&task.id, task.status, &task.origin, task.retry_count)
            else {
                continue;
            };
            // Successor ids keep the 32-hex shape the orphan parser
            // recognises; the retry generation rides in `retry_count`.
            let successor = hex_task_id(&spawn.task_id);
            let row = NewTask {
                id: successor.clone(),
                user_id: task.user_id.clone(),
                artist_name: task.artist_name.clone(),
                album_title: task.album_title.clone(),
                release_group_mbid: task.release_group_mbid.clone(),
                origin: spawn.origin.clone(),
                retry_count: spawn.retry_count,
            };
            let source_task = task.id.clone();
            let is_track = task.download_type == "track";
            let recording = task.recording_mbid.clone().unwrap_or_default();
            let spawned = self
                .step("downloads.retry", move |store| {
                    // Held tracks gate their task: re-downloading a held
                    // track loops.
                    if store.has_unresolved_held_for_task(&source_task)?
                        || store.get_task(&row.id)?.is_some()
                    {
                        return Ok(false);
                    }
                    // Edition pins and track identity ride onto the
                    // successor; without them a retried edition loses its
                    // pinned release.
                    let details = store.task_details(&source_task)?;
                    if is_track {
                        store.insert_track_task(&row, &recording, now)?;
                    } else {
                        store.insert_task(&row, now)?;
                    }
                    store.set_task_details(&row.id, &details, now)?;
                    Ok(true)
                })
                .await;
            if spawned == Some(true) {
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
    }

    /// Settle one claimed cleanup row.
    async fn cleanup_one(&self, pass: &Pass, attempt: &AttemptRow) {
        let now = pass.now;
        let read = async {
            Ok::<_, String>((
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
        let handle: SourceHandle = decoded.unwrap_or(SourceHandle {
            source: attempt.source.clone(),
            username: String::new(),
            filenames: Vec::new(),
            job_name: attempt.job_name.clone(),
            nzo_id: String::new(),
            plugin_token: String::new(),
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
    /// answer keeps the folder; the recycle bin prunes expired entries.
    async fn orphan_pass(&self, pass: &Pass) {
        for (source_tag, root) in &pass.config.orphan_roots {
            self.orphan_root(pass, source_tag, root).await;
        }
        if let Some(bin) = &pass.config.recycle {
            let bin = bin.clone();
            let pruned = tokio::task::spawn_blocking(move || bin.prune(SystemTime::now())).await;
            if let Err(error) = pruned
                .map_err(|join| join.to_string())
                .and_then(|r| r.map_err(|e| e.to_string()))
            {
                tracing::warn!(%error, "recycle bin prune failed");
            }
        }
    }

    /// Reconcile one complete dir.
    async fn orphan_root(&self, pass: &Pass, source_tag: &str, root: &Path) {
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
                .orphan_evidence(pass, source_tag, &task_id, &job_name, root, &entry.path())
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
        source_tag: &str,
        task_id: &str,
        job_name: &str,
        root: &Path,
        path: &Path,
    ) -> Option<OrphanEvidence> {
        let has_cleanup_debt = self
            .ownership
            .has_cleanup_debt(source_tag, task_id, job_name)
            .await
            .ok()?;
        let task_status = self.ownership.task_status(task_id).await.ok()?;
        let task_active = task_status.is_some_and(|status| {
            matches!(status.as_str(), "queued" | "downloading" | "processing")
        });
        let bundles_settled = self.ownership.bundles_settled(task_id).await.ok()?;
        let source = pass.source_for(source_tag)?;
        let handle = SourceHandle {
            source: source_tag.to_owned(),
            username: String::new(),
            filenames: Vec::new(),
            job_name: job_name.to_owned(),
            nzo_id: String::new(),
            plugin_token: String::new(),
        };
        // Soulseek handles need the peer plus filenames, which the folder
        // name does not carry: without them the client check cannot run,
        // so slskd folders always keep (fail closed). Mount health still
        // comes from a real readability probe, failing closed on error.
        let client_job_active = if source_tag == "soulseek" {
            let root_readable = tokio::fs::read_dir(root).await.is_ok();
            if !root_readable {
                return None;
            }
            return Some(OrphanEvidence {
                has_cleanup_debt,
                task_active,
                bundles_settled,
                mount_healthy: true,
                client_job_active: true,
                age_seconds: 0.0,
            });
        } else {
            source.job_active(&handle).await?
        };
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
        source_tag: &str,
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
            source: source_tag.to_owned(),
            username: String::new(),
            filenames: Vec::new(),
            job_name: job_name.to_owned(),
            nzo_id: String::new(),
            plugin_token: String::new(),
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

/// Why a reimport could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReimportError {
    /// Imports or the task's download client are not set up.
    Unavailable(String),
    /// The journal could not be read or written.
    Journal(String),
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
fn handled_count(attempts: &[Attempt], source: Option<&str>) -> i64 {
    attempts
        .iter()
        .filter(|attempt| attempt.handle.is_some())
        .filter(|attempt| source.is_none_or(|tag| attempt.row.source == tag))
        .count() as i64
}

/// Whether the failover walk is spent: `max` attempts occupy indices
/// `0..max`, so a next index at or past `max` fails the task.
fn failover_exhausted(handled: i64, max: i64) -> bool {
    handled >= max
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

/// Spawn the worker loop: register the durable job, run passes on the
/// configured cadence until shutdown, then mark the job stopped. The
/// first pass runs immediately so redispatches do not wait a full
/// interval after boot.
pub async fn spawn_download_worker(
    worker: Arc<DownloadWorker>,
    wakeups: DurableWorkWakeups,
    lane: WriteLane,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<tokio::task::JoinHandle<()>, String> {
    wakeups
        .register_job(&lane, DOWNLOAD_WORKER_JOB, JobKind::Durable, None)
        .await
        .map_err(|error| error.to_string())?;
    let task = tokio::spawn(async move {
        // A pre-signaled shutdown skips the first pass entirely.
        if *shutdown.borrow() {
            mark(&wakeups, &lane, JobState::Stopped).await;
            return;
        }
        mark(&wakeups, &lane, JobState::Running).await;
        let mut pass: u64 = 0;
        loop {
            worker.run_once(pass).await;
            if let Err(error) = wakeups.heartbeat(&lane, DOWNLOAD_WORKER_JOB).await {
                tracing::warn!(%error, "download worker heartbeat failed");
            }
            pass += 1;
            let interval = (worker.config)().interval;
            tokio::select! {
                () = tokio::time::sleep(jittered_interval(interval)) => {}
                _ = shutdown.changed() => break,
            }
            if *shutdown.borrow() {
                break;
            }
        }
        mark(&wakeups, &lane, JobState::Stopped).await;
    });
    Ok(task)
}

/// Record the worker job's state, logging a failed write.
async fn mark(wakeups: &DurableWorkWakeups, lane: &WriteLane, state: JobState) {
    if let Err(error) = wakeups
        .set_job_state(lane, DOWNLOAD_WORKER_JOB, state)
        .await
    {
        tracing::warn!(%error, "download worker state write failed");
    }
}

/// Pure removal gate: the folder must sit strictly under the walked
/// root and must not be a symlink.
fn orphan_remove_allowed(root: &Path, path: &Path, is_symlink: bool) -> bool {
    !is_symlink && path != root && path.starts_with(root)
}

/// Pass cadence with ±10% jitter so a fleet of workers does not beat in
/// lockstep after a shared restart.
fn jittered_interval(base: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.subsec_nanos())
        .unwrap_or(0);
    let factor = 0.9 + f64::from(nanos % 201) / 1000.0;
    Duration::from_secs_f64((base.as_secs_f64() * factor).max(1.0))
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
