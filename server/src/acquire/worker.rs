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
//! - cleanup: claimed journal rows discard client records and settle;
//! - orphans (hourly): complete-dir debris with no owner is removed.
//!
//! The loop registers as the durable `download-worker` job and heartbeats
//! every pass, following the stage-5/6 shutdown plumbing.

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
use super::downloads::store::{AttemptRow, TaskRow};
use super::downloads::watchdog::{PollSample, RetryPolicy, Watchdog, WatchdogConfig};
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

/// What startup recovery did, for the boot log and briefs.
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
pub fn run_startup_recovery(
    journal: &Journal,
    staging_root: &Path,
) -> Result<RecoveryReport, String> {
    let tasks = journal.with_store(|store| {
        store.list_active(&[
            TaskStatus::Queued,
            TaskStatus::Downloading,
            TaskStatus::Processing,
        ])
    })?;
    let mut report = RecoveryReport::default();
    for task in tasks {
        let manifest_path = ManifestCodec::path(staging_root, &task.id);
        let manifest_present = manifest_path.is_file();
        let manifest_matches_attempt = manifest_attempt_match(journal, staging_root, &task);
        match classify_startup(StartupCtx {
            status: task.status,
            manifest_present,
            manifest_matches_attempt,
        }) {
            StartupAction::Redispatch => report.redispatched += 1,
            StartupAction::ResumePoll => report.resumed += 1,
            StartupAction::RestartClean => {
                journal.with_store(|store| {
                    store.transition_task(&task.id, TaskStatus::Queued, now_unix_f64(), None)
                })?;
                report.restarted += 1;
            }
            StartupAction::Noop => {}
        }
    }
    Ok(report)
}

/// Compare the manifest's attempt link against the journal, when both
/// sides exist. Anything unreadable answers `None` (unknown), which
/// classifies toward resuming rather than restarting.
fn manifest_attempt_match(journal: &Journal, staging_root: &Path, task: &TaskRow) -> Option<bool> {
    let bytes = std::fs::read(ManifestCodec::path(staging_root, &task.id)).ok()?;
    let manifest = ManifestCodec.decode(&bytes).ok()?;
    let linked = manifest.attempt_id?;
    let attempts = journal
        .with_store(|store| store.list_attempts(&task.id))
        .ok()?;
    Some(attempts.iter().any(|attempt| attempt.id == linked))
}

/// One bundled download source behind the fetch seam.
pub enum Source {
    /// Soulseek via slskd.
    Slskd(SlskdSource),
    /// Usenet via SABnzbd.
    Sab(SabnzbdSource),
}

impl Source {
    fn name(&self) -> &str {
        match self {
            Self::Slskd(_) => "slskd",
            Self::Sab(_) => "sabnzbd",
        }
    }

    /// Journal source tag (`soulseek`, `usenet`).
    fn journal_source(&self) -> &'static str {
        match self {
            Self::Slskd(_) => "soulseek",
            Self::Sab(_) => "usenet",
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
        }
    }

    async fn poll(
        &self,
        handle: &SourceHandle,
    ) -> Result<super::downloads::sources::TransferProgress, SourceError> {
        match self {
            Self::Slskd(source) => source.poll(handle).await,
            Self::Sab(source) => source.poll(handle).await,
        }
    }

    async fn discard(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        match self {
            Self::Slskd(source) => source.discard(handle).await,
            Self::Sab(source) => source.discard(handle).await,
        }
    }

    /// Whether the client still owns one job (orphan evidence). `None`
    /// means the lookup failed and the folder must stay.
    async fn job_active(&self, handle: &SourceHandle) -> Option<bool> {
        match self.poll(handle).await {
            Ok(progress) => Some(!progress.all_terminal),
            Err(_) => None,
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
        }
    }
}

/// Worker tuning, bound from config at boot.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Pass cadence.
    pub interval: Duration,
    /// Concurrent live downloads.
    pub max_concurrent_downloads: usize,
    /// Failover attempts per task (candidate-index ceiling).
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
    sources: Vec<Source>,
    ownership: JournalOwnership,
    config: WorkerConfig,
    watchdog: Watchdog,
    poll_cache: Mutex<HashMap<String, PollMemory>>,
    enqueue_not_before: Mutex<HashMap<String, f64>>,
    unsearchable_warned: Mutex<HashSet<String>>,
}

impl DownloadWorker {
    /// Wire the worker over the shared journal and sources.
    pub fn new(journal: Arc<Journal>, sources: Vec<Source>, config: WorkerConfig) -> Self {
        let watchdog = Watchdog::new(config.watchdog.clone());
        let ownership = JournalOwnership::new(journal.clone());
        Self {
            journal,
            sources,
            ownership,
            config,
            watchdog,
            poll_cache: Mutex::new(HashMap::new()),
            enqueue_not_before: Mutex::new(HashMap::new()),
            unsearchable_warned: Mutex::new(HashSet::new()),
        }
    }

    /// Sources in configured try order.
    fn ordered_sources(&self) -> Vec<&Source> {
        let mut ordered = Vec::with_capacity(self.sources.len());
        for wanted in &self.config.source_order {
            for source in &self.sources {
                if source.journal_source() == wanted {
                    ordered.push(source);
                }
            }
        }
        for source in &self.sources {
            if !ordered.iter().any(|placed| std::ptr::eq(*placed, source)) {
                ordered.push(source);
            }
        }
        ordered
    }

    /// Run every steady-state pass once, in order. The orphan sweep only
    /// runs when `pass` hits its hourly slot.
    pub async fn run_once(&self, pass: u64) {
        let now = now_unix_f64();
        self.enqueue_pass(now).await;
        self.poll_pass(now).await;
        self.retry_pass(now).await;
        self.cleanup_pass(now).await;
        if pass.is_multiple_of(ORPHAN_EVERY_NTH_PASS) {
            self.orphan_pass().await;
        }
    }

    /// Enqueue queued tasks while under the concurrency cap.
    async fn enqueue_pass(&self, now: f64) {
        let live = Journal::with_store_async(&self.journal, |store| {
            store.list_active(&[TaskStatus::Downloading, TaskStatus::Processing])
        })
        .await
        .unwrap_or_default()
        .len();
        let mut budget = self.config.max_concurrent_downloads.saturating_sub(live);
        if budget == 0 {
            return;
        }
        let queued = Journal::with_store_async(&self.journal, |store| {
            store.list_active(&[TaskStatus::Queued])
        })
        .await
        .unwrap_or_default();
        for task in queued {
            if budget == 0 {
                break;
            }
            if self.enqueue_one(&task, now).await {
                budget -= 1;
            }
        }
    }

    /// Enqueue one queued task. Answers whether a fetch started.
    async fn enqueue_one(&self, task: &TaskRow, now: f64) -> bool {
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
        let task_id = task.id.clone();
        let attempts =
            Journal::with_store_async(&self.journal, move |store| store.list_attempts(&task_id))
                .await
                .unwrap_or_default();
        if self.live_attempt(&attempts).await.is_some() {
            // A pollable attempt exists; the poll pass owns this task.
            // Queued rows in that state just missed their transition.
            let task_id = task.id.clone();
            let _ = Journal::with_store_async(&self.journal, move |store| {
                store.transition_task(&task_id, TaskStatus::Downloading, now, None)
            })
            .await;
            return false;
        }
        let index = handled_count(&self.journal, &attempts).await;
        if failover_exhausted(index, self.config.max_failover_attempts) {
            let task_id = task.id.clone();
            let last_attempt = attempts.last().map(|attempt| attempt.id.clone());
            let _ = Journal::with_store_async(&self.journal, move |store| {
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
            return false;
        }
        let key = format!("enqueue:{}:{index}", task.id);
        // A repeat claim means a crash between the client add and the
        // journal write: the enqueue below re-attaches instead of
        // double-adding (usenet probes queue/history by the deterministic
        // job name; slskd re-enqueue re-attaches by content), and the
        // attempt insert stays single-winner on its id.
        let task_id = task.id.clone();
        let _ = Journal::with_store_async(&self.journal, move |store| {
            store.claim_key(&key, &task_id, "enqueue", now)
        })
        .await;
        for source in self.ordered_sources() {
            match source.enqueue(&task.id, index).await {
                Ok(handle) => {
                    self.journal_enqueued(task, source, index, &handle, now)
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
                    tracing::warn!(
                        task_id = %task.id,
                        source = source.name(),
                        %error,
                        "enqueue failed; trying the next source"
                    );
                }
            }
        }
        self.backoff(&task.id, now);
        false
    }

    /// Newest attempt with a journaled client handle, if any.
    async fn live_attempt<'a>(&self, attempts: &'a [AttemptRow]) -> Option<&'a AttemptRow> {
        for attempt in attempts.iter().rev() {
            if !matches!(attempt.state, AttemptState::Acquiring | AttemptState::InUse) {
                continue;
            }
            let attempt_id = attempt.id.clone();
            let handled = Journal::with_store_async(&self.journal, move |store| {
                store.attempt_handle_json(&attempt_id)
            })
            .await
            .unwrap_or(None)
            .is_some();
            if handled {
                return Some(attempt);
            }
        }
        None
    }

    /// Journal one successful enqueue: the attempt row, the manifest
    /// handle plus target files, the Downloading transition, and fresh
    /// poll memory.
    async fn journal_enqueued(
        &self,
        task: &TaskRow,
        source: &Source,
        index: i64,
        handle: &SourceHandle,
        now: f64,
    ) {
        let handle_json = serde_json::to_string(handle).unwrap_or_default();
        let attempt_id = format!("{}-a{index}", task.id);
        let task_id = task.id.clone();
        let journal_source = source.journal_source();
        let job_name = handle.job_name.clone();
        let journaled = Journal::with_store_async(&self.journal, move |store| {
            store.insert_attempt(
                &attempt_id,
                &task_id,
                journal_source,
                index,
                &job_name,
                &handle_json,
                AttemptState::Acquiring,
                now,
            )
        })
        .await
        .is_ok();
        if !journaled {
            // A concurrent worker won the same index; the poll pass will
            // pick up whichever attempt journaled first.
            return;
        }
        self.write_manifest_handle(task, handle).await;
        let task_id = task.id.clone();
        let _ = Journal::with_store_async(&self.journal, move |store| {
            store.transition_task(&task_id, TaskStatus::Downloading, now, None)
        })
        .await;
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

    /// Merge the client handle and target files into the task manifest.
    /// Best-effort: a missing manifest only steers recovery toward a
    /// clean restart.
    async fn write_manifest_handle(&self, task: &TaskRow, handle: &SourceHandle) {
        let Some(path) = ManifestCodec::checked_path(&self.config.staging_root, &task.id) else {
            tracing::warn!(task_id = %task.id, "manifest handle write refused: unsafe task id");
            return;
        };
        let mut manifest: DownloadManifest = tokio::fs::read(&path)
            .await
            .ok()
            .and_then(|bytes| ManifestCodec.decode(&bytes).ok())
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
        match ManifestCodec.encode(&manifest) {
            Ok(bytes) => {
                if let Some(parent) = path.parent()
                    && let Err(error) = tokio::fs::create_dir_all(parent).await
                {
                    tracing::warn!(task_id = %task.id, %error, "manifest handle dir failed");
                    return;
                }
                if let Err(error) = tokio::fs::write(&path, bytes).await {
                    tracing::warn!(task_id = %task.id, %error, "manifest handle write failed");
                }
            }
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "manifest handle encode failed");
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
    async fn poll_pass(&self, now: f64) {
        let tasks = Journal::with_store_async(&self.journal, |store| {
            store.list_active(&[TaskStatus::Downloading, TaskStatus::Processing])
        })
        .await
        .unwrap_or_default();
        for task in tasks {
            self.poll_one(&task, now).await;
        }
    }

    /// Poll one live task: judge the sample, then complete, fail over, or
    /// keep waiting. Poll errors never fail the task directly; the stall
    /// math ages a dead client out into failover instead.
    async fn poll_one(&self, task: &TaskRow, now: f64) {
        let task_id = task.id.clone();
        let attempts =
            Journal::with_store_async(&self.journal, move |store| store.list_attempts(&task_id))
                .await
                .unwrap_or_default();
        let Some(attempt) = self.live_attempt(&attempts).await.cloned() else {
            // No pollable attempt: back to queued for a fresh enqueue.
            let task_id = task.id.clone();
            let _ = Journal::with_store_async(&self.journal, move |store| {
                store.transition_task(&task_id, TaskStatus::Queued, now, None)
            })
            .await;
            return;
        };
        let attempt_id = attempt.id.clone();
        let handle_json = Journal::with_store_async(&self.journal, move |store| {
            store.attempt_handle_json(&attempt_id)
        })
        .await
        .unwrap_or(None)
        .unwrap_or_default();
        let handle: SourceHandle = match serde_json::from_str(&handle_json) {
            Ok(handle) => handle,
            Err(_) => {
                let task_id = task.id.clone();
                let _ = Journal::with_store_async(&self.journal, move |store| {
                    store.transition_task(&task_id, TaskStatus::Queued, now, None)
                })
                .await;
                return;
            }
        };
        let source = match self
            .sources
            .iter()
            .find(|source| source.journal_source() == attempt.source)
        {
            Some(source) => source,
            // Plugin sources have no adapter yet; leave the row for a
            // worker that knows them rather than failing it here.
            None => return,
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
                self.apply_verdict(task, &attempt, source, &handle, progress, sample, now)
                    .await;
            }
            Err(SourceError::Rejected(detail)) => {
                self.fail_over(task, &attempt, source, &handle, &detail, now)
                    .await;
            }
            Err(error) => {
                // Unavailable or local fault: stamp the poll and let the
                // stall math age the task out; a momentary outage must
                // not fail a healthy transfer.
                tracing::warn!(task_id = %task.id, %error, "download poll failed");
                let task_id = task.id.clone();
                let _ = Journal::with_store_async(&self.journal, move |store| {
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
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Source,
        handle: &SourceHandle,
        progress: super::downloads::sources::TransferProgress,
        sample: PollSample,
        now: f64,
    ) {
        match self.watchdog.evaluate(&sample) {
            super::downloads::watchdog::WatchdogOutcome::Continue => {
                let task_id = task.id.clone();
                let _ = Journal::with_store_async(&self.journal, move |store| {
                    store.touch_poll(&task_id, now)
                })
                .await;
            }
            super::downloads::watchdog::WatchdogOutcome::Completed => {
                let task_id = task.id.clone();
                let attempt_id = attempt.id.clone();
                let _ = Journal::with_store_async(&self.journal, move |store| {
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
            }
            super::downloads::watchdog::WatchdogOutcome::Terminal => {
                let detail = format!(
                    "source batch terminal ({} files succeeded)",
                    progress.succeeded_filenames.len()
                );
                self.fail_over(task, attempt, source, handle, &detail, now)
                    .await;
            }
            super::downloads::watchdog::WatchdogOutcome::Stalled => {
                self.fail_over(task, attempt, source, handle, "transfer stalled", now)
                    .await;
            }
            super::downloads::watchdog::WatchdogOutcome::QueuedTimeout => {
                self.fail_over(
                    task,
                    attempt,
                    source,
                    handle,
                    "stuck in the remote queue",
                    now,
                )
                .await;
            }
            super::downloads::watchdog::WatchdogOutcome::MaterializeTimeout => {
                self.fail_over(
                    task,
                    attempt,
                    source,
                    handle,
                    "no transfer materialized after enqueue",
                    now,
                )
                .await;
            }
            super::downloads::watchdog::WatchdogOutcome::Deadline => {
                self.fail_over(task, attempt, source, handle, "poll deadline hit", now)
                    .await;
            }
        }
    }

    /// Fail one attempt over to the next candidate: discard client
    /// records best-effort, settle the attempt row, blocklist the failed
    /// release (never on a local fault), and requeue the task so the next
    /// pass enqueues the following index.
    async fn fail_over(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Source,
        handle: &SourceHandle,
        reason: &str,
        now: f64,
    ) {
        self.quarantine_failed(task, attempt, handle, reason, now)
            .await;
        match source.discard(handle).await {
            Ok(_) => {
                let attempt_id = attempt.id.clone();
                let revision = attempt.row_revision;
                let _ = Journal::with_store_async(&self.journal, move |store| {
                    store.transition_attempt(
                        &attempt_id,
                        revision,
                        AttemptState::Complete,
                        now,
                        Some("discard"),
                        None,
                        true,
                    )
                })
                .await;
            }
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "discard failed; cleanup will retry");
                let attempt_id = attempt.id.clone();
                let revision = attempt.row_revision;
                let _ = Journal::with_store_async(&self.journal, move |store| {
                    store.transition_attempt(
                        &attempt_id,
                        revision,
                        AttemptState::CleanupPending,
                        now,
                        Some("discard"),
                        None,
                        true,
                    )
                })
                .await;
            }
        }
        let task_id = task.id.clone();
        let reason = reason.to_owned();
        let _ = Journal::with_store_async(&self.journal, move |store| {
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
    /// recovers. Best-effort: a journal hiccup here must not fail the
    /// failover itself.
    async fn quarantine_failed(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        handle: &SourceHandle,
        reason: &str,
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
        let _ = Journal::with_store_async(&self.journal, move |store| {
            for identity in &identities {
                store.record_quarantine(
                    &source,
                    identity,
                    QuarantineReason::DownloadFailed.as_str(),
                    scope.as_deref(),
                    now,
                    QUARANTINE_TTL_SECONDS,
                )?;
            }
            Ok::<_, super::downloads::store::StoreError>(())
        })
        .await;
    }

    /// Spawn successors for terminal tasks whose retry backoff elapsed.
    /// Held tracks gate their task: re-downloading a held track loops.
    async fn retry_pass(&self, now: f64) {
        let max = self.config.retry.auto_retry_max() as i64;
        if max == 0 {
            return;
        }
        let retryable =
            Journal::with_store_async(&self.journal, move |store| store.list_retryable(max))
                .await
                .unwrap_or_default();
        for task in retryable {
            let anchor = task.completed_at.unwrap_or(task.updated_at);
            let due = self.config.retry.next_retry_at(
                task.retry_count.max(0) as u32,
                anchor,
                task.status.as_str(),
            );
            if due.is_none_or(|at| at > now) {
                continue;
            }
            let held_task = task.id.clone();
            let held = Journal::with_store_async(&self.journal, move |store| {
                store.has_unresolved_held_for_task(&held_task)
            })
            .await
            .unwrap_or(true);
            if held {
                continue;
            }
            let Some(spawn) = plan_retry(&task.id, task.status, &task.origin, task.retry_count)
            else {
                continue;
            };
            // Successor ids keep the 32-hex shape the orphan parser
            // recognises; the retry generation rides in `retry_count`.
            let successor = hex_task_id(&spawn.task_id);
            // One successor per (task, generation): a crash between the
            // insert and the next pass must not spawn twins.
            let retry_key = format!("retry:{}:{}", task.id, spawn.retry_count);
            let successor_key = successor.clone();
            let claimed = Journal::with_store_async(&self.journal, move |store| {
                store.claim_key(&retry_key, &successor_key, "retry", now)
            })
            .await
            .unwrap_or(false);
            if !claimed {
                continue;
            }
            let details_task = task.id.clone();
            let details = Journal::with_store_async(&self.journal, move |store| {
                store.task_details(&details_task)
            })
            .await
            .unwrap_or_default();
            let row = super::downloads::store::NewTask {
                id: successor.clone(),
                user_id: task.user_id.clone(),
                artist_name: task.artist_name.clone(),
                album_title: task.album_title.clone(),
                release_group_mbid: task.release_group_mbid.clone(),
                origin: spawn.origin.clone(),
                retry_count: spawn.retry_count,
            };
            let is_track = task.download_type == "track";
            let recording = task.recording_mbid.clone().unwrap_or_default();
            let successor_id = successor.clone();
            let inserted = Journal::with_store_async(&self.journal, move |store| {
                if is_track {
                    store.insert_track_task(&row, &recording, now)?;
                } else {
                    store.insert_task(&row, now)?;
                }
                // Edition pins and track identity ride onto the successor;
                // without them a retried edition loses its pinned release.
                store.set_task_details(&successor_id, &details, now)
            })
            .await;
            if inserted.is_err() {
                continue;
            }
            tracing::info!(
                task_id = %task.id,
                successor = %successor,
                "download auto-retry spawned"
            );
        }
    }

    /// Claim due cleanup rows and settle them: discard client records,
    /// then mark complete; a discard failure defers with backoff.
    async fn cleanup_pass(&self, now: f64) {
        let worker_id = self.config.worker_id.clone();
        let claimed = Journal::with_store_async(&self.journal, move |store| {
            store.claim_cleanup_attempts(
                &worker_id,
                now,
                FAILOVER_CLAIM_LIMIT,
                FAILOVER_LEASE_SECONDS,
            )
        })
        .await
        .unwrap_or_default();
        for attempt in claimed {
            self.cleanup_one(&attempt, now).await;
        }
    }

    /// Settle one claimed cleanup row.
    async fn cleanup_one(&self, attempt: &AttemptRow, now: f64) {
        let attempt_id = attempt.id.clone();
        let handle_json = Journal::with_store_async(&self.journal, move |store| {
            store.attempt_handle_json(&attempt_id)
        })
        .await
        .unwrap_or(None)
        .unwrap_or_default();
        let handle: SourceHandle = serde_json::from_str(&handle_json).unwrap_or(SourceHandle {
            source: attempt.source.clone(),
            username: String::new(),
            filenames: Vec::new(),
            job_name: attempt.job_name.clone(),
        });
        let Some(source) = self
            .sources
            .iter()
            .find(|source| source.journal_source() == attempt.source)
        else {
            // No adapter for this source: defer with backoff rather than
            // spinning on a row this worker can never settle.
            let attempt_id = attempt.id.clone();
            let revision = attempt.row_revision;
            let _ = Journal::with_store_async(&self.journal, move |store| {
                store.record_cleanup_failure(&attempt_id, revision, "no_adapter", now)
            })
            .await;
            return;
        };
        match source.discard(&handle).await {
            Ok(_) => {
                let attempt_id = attempt.id.clone();
                let revision = attempt.row_revision;
                let disposition = attempt.disposition.clone();
                let _ = Journal::with_store_async(&self.journal, move |store| {
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
                let attempt_id = attempt.id.clone();
                let revision = attempt.row_revision;
                let _ = Journal::with_store_async(&self.journal, move |store| {
                    store.record_cleanup_failure(&attempt_id, revision, "discard_failed", now)
                })
                .await;
            }
        }
    }

    /// Walk the complete dirs and remove proven debris. Every ambiguous
    /// answer keeps the folder; the recycle bin prunes expired entries.
    async fn orphan_pass(&self) {
        for (source_tag, root) in &self.config.orphan_roots {
            self.orphan_root(source_tag, root).await;
        }
        if let Some(bin) = &self.config.recycle {
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
    async fn orphan_root(&self, source_tag: &str, root: &Path) {
        let mut entries = match tokio::fs::read_dir(root).await {
            Ok(entries) => entries,
            Err(_) => return,
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(_) => continue,
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
                .orphan_evidence(source_tag, &task_id, &job_name, root, &entry.path())
                .await;
            match evaluate_orphan(&name, is_symlink, evidence) {
                OrphanDecision::Remove => {
                    self.remove_orphan(source_tag, &task_id, &job_name, root, &entry.path())
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
        let source = self
            .sources
            .iter()
            .find(|source| source.journal_source() == source_tag)?;
        let handle = SourceHandle {
            source: source_tag.to_owned(),
            username: String::new(),
            filenames: Vec::new(),
            job_name: job_name.to_owned(),
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
        let Some(source) = self
            .sources
            .iter()
            .find(|source| source.journal_source() == source_tag)
        else {
            return;
        };
        let handle = SourceHandle {
            source: source_tag.to_owned(),
            username: String::new(),
            filenames: Vec::new(),
            job_name: job_name.to_owned(),
        };
        if source.discard(&handle).await.is_err() {
            return;
        }
        if let Err(error) = tokio::fs::remove_dir_all(path).await {
            tracing::warn!(task_id, %error, "orphan folder remove failed");
        } else {
            tracing::info!(task_id, path = %path.display(), "orphan folder removed");
        }
    }
}

/// Attempts with a journaled handle: the next candidate index. Handle-less
/// rows (a crash between journaling and enqueue) do not consume indices.
async fn handled_count(journal: &Arc<Journal>, attempts: &[AttemptRow]) -> i64 {
    let mut handled = 0;
    for attempt in attempts {
        let attempt_id = attempt.id.clone();
        let has_handle =
            Journal::with_store_async(journal, move |store| store.attempt_handle_json(&attempt_id))
                .await
                .unwrap_or(None)
                .is_some();
        if has_handle {
            handled += 1;
        }
    }
    handled
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
    let interval = worker.config.interval;
    let task = tokio::spawn(async move {
        // A pre-signaled shutdown skips the first pass entirely.
        if *shutdown.borrow() {
            let _ = wakeups
                .set_job_state(&lane, DOWNLOAD_WORKER_JOB, JobState::Stopped)
                .await;
            return;
        }
        let _ = wakeups
            .set_job_state(&lane, DOWNLOAD_WORKER_JOB, JobState::Running)
            .await;
        let mut pass: u64 = 0;
        loop {
            worker.run_once(pass).await;
            let _ = wakeups.heartbeat(&lane, DOWNLOAD_WORKER_JOB).await;
            pass += 1;
            tokio::select! {
                () = tokio::time::sleep(jittered_interval(interval)) => {}
                _ = shutdown.changed() => break,
            }
            if *shutdown.borrow() {
                break;
            }
        }
        let _ = wakeups
            .set_job_state(&lane, DOWNLOAD_WORKER_JOB, JobState::Stopped)
            .await;
    });
    Ok(task)
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
    fn failover_cap_fails_at_the_ceiling() {
        assert!(!failover_exhausted(0, 3));
        assert!(!failover_exhausted(2, 3));
        assert!(failover_exhausted(3, 3));
        assert!(failover_exhausted(4, 3));
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
