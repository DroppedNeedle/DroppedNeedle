//! Download worker: startup recovery plus the poll loop.
//!
//! [`run_startup_recovery`] classifies every active task before serving
//! traffic (queued tasks re-dispatch through the first worker pass,
//! landings and live attempts resume, the rest restart clean under the
//! same idempotency keys, so a double resume cannot double-enqueue).
//! [`DownloadWorker`] then runs the steady-state passes on one cadence:
//!
//! - enqueue: queued tasks below the concurrency cap search and enqueue
//!   through the source order, journaling each attempt; nothing starts,
//!   and no finished download lands, while the library folder downloads
//!   go to is missing, read-only or not answering ([`crate::library_ready`],
//!   checked once a pass under a timeout);
//! - poll: live attempts report progress through the [`Watchdog`], whose
//!   verdicts complete, fail over, or requeue their tasks. Each source is
//!   polled side by side with the others, and a client that stops
//!   answering is polled again only after a jittered backoff;
//! - retry: terminal tasks whose backoff elapsed spawn successors;
//! - cleanup: claimed journal rows abort and discard client records and
//!   settle; downloads whose cleanup keeps failing show on service health;
//! - orphans (hourly): files kept for "Retry import" that are 90 days old,
//!   or whose task was cancelled or cleared, go to cleanup; complete-dir
//!   debris with no owner is removed.
//!
//! Sources and tuning are resolved at the start of every pass, so saved
//! settings take effect on the next pass without a restart. The loop runs
//! on the loop supervisor as `download-worker`. Journal failures are logged
//! and skip the affected item; a read error never stands in for "no rows".

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::downloads::journal::Journal;
use crate::downloads::source_tag::SourceTag;
use crate::downloads::sources::{DownloadSource, SourceHandle};
use crate::downloads::state::AttemptState;
use crate::downloads::store::{AttemptRow, StoreError};
use crate::downloads::watchdog::{RetryPolicy, Watchdog, WatchdogConfig};
use crate::landing::LandingService;
use crate::library_ready::LibraryWait;
use crate::readiness::SourceReadiness;
use crate::search_jobs::review::ReviewMode;
use crate::sources::JournalOwnership;
use crate::target::Targets;
use droppedneedle_providers::health::ServiceHealth;

mod cleanup;
mod controls;
mod enqueue;
#[cfg(any(test, feature = "test-support"))]
mod fixed_source;
mod landings;
mod orphans;
mod poll;
mod recovery;
mod retry;
mod review;
mod run;
mod settings;

pub use controls::{NextSourceError, RetryError};
#[cfg(any(test, feature = "test-support"))]
pub use fixed_source::FixedSource;
pub use landings::ReimportError;
pub use recovery::{RecoveryReport, run_startup_recovery};
pub use run::spawn_download_worker;
pub use settings::WorkerSettings;
pub(crate) use settings::sabnzbd_mount;

use poll::{PollBackoff, PollMemory};

/// Job name of the worker loop.
pub const DOWNLOAD_WORKER_JOB: &str = "download-worker";

/// Job name of the wait, at shutdown, for the landings the worker started.
pub const DOWNLOAD_LANDINGS_JOB: &str = "download-landings";

/// Longest gap between passes when nothing wakes the worker.
pub const WORKER_INTERVAL: Duration = Duration::from_secs(30);

/// How long a pass waits for the library folder check before it counts
/// the folder as not answering.
const LIBRARY_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

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
    Live(Arc<WorkerSettings>),
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
    Live(Arc<WorkerSettings>),
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
}

/// Everything one pass works with: settings and sources resolved once at
/// the start of the pass.
struct Pass {
    /// Why downloads wait for the library this pass: none starts or lands.
    library: Option<LibraryWait>,
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
    /// The library folder check: its last answer, and whether one runs.
    library_check: Arc<Mutex<LibraryCheck>>,
    /// The library wait last logged, so each change is logged once.
    library_wait_logged: Mutex<Option<LibraryWait>>,
}

/// The kept answer of the library folder check.
#[derive(Debug, Default)]
struct LibraryCheck {
    /// A check runs (or hangs) now; no second one starts beside it.
    running: bool,
    /// The folder the last finished check looked at.
    folder: Option<PathBuf>,
    /// Why downloads wait, as last found.
    wait: Option<LibraryWait>,
}

fn lock_check(check: &Mutex<LibraryCheck>) -> std::sync::MutexGuard<'_, LibraryCheck> {
    check
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

impl DownloadWorker {
    /// Now, as the float unix seconds the journal stores.
    fn now_unix_f64(&self) -> f64 {
        self.journal.db().now_unix_f64()
    }

    /// Wire the worker over the shared journal and the saved settings,
    /// sending live progress to `events` and the cleanup warning to
    /// `health`. `readiness` reads the same settings.
    pub fn new(
        journal: Arc<Journal>,
        settings: Arc<WorkerSettings>,
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
            library_check: Arc::new(Mutex::new(LibraryCheck::default())),
            library_wait_logged: Mutex::new(None),
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

    /// Whether a download source is on: the answer requests, the health
    /// smoke and Discover read too.
    pub fn readiness(&self) -> &SourceReadiness {
        &self.readiness
    }

    /// Why downloads wait for the library, as the last pass found it.
    /// `None` when they may go ahead, or when imports are not wired.
    pub fn library_wait(&self) -> Option<LibraryWait> {
        lock_check(&self.library_check).wait.clone()
    }

    /// Check the library folder downloads go to, at most one check at a
    /// time and bounded by [`LIBRARY_CHECK_TIMEOUT`]. A check that does
    /// not finish in time counts as not answering, and the next pass
    /// starts none until it ends.
    async fn check_library(&self) -> Option<LibraryWait> {
        let library = self.landing.as_ref()?.library()?.clone();
        {
            let mut state = lock_check(&self.library_check);
            if state.running {
                return state.wait.clone();
            }
            state.running = true;
        }
        let shared = self.library_check.clone();
        let check = tokio::task::spawn_blocking(move || {
            let folder = library.import_folder();
            let wait = folder.as_deref().and_then(crate::library_ready::check);
            let mut state = lock_check(&shared);
            state.running = false;
            state.folder = folder;
            state.wait = wait;
        });
        let finished = tokio::time::timeout(LIBRARY_CHECK_TIMEOUT, check).await;
        let mut state = lock_check(&self.library_check);
        match finished {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, "library folder check did not run");
                state.running = false;
                state.wait = Some(LibraryWait::NotAnswering(state.folder.clone()));
            }
            Err(_) if state.running => {
                state.wait = Some(LibraryWait::NotAnswering(state.folder.clone()));
            }
            Err(_) => {}
        }
        state.wait.clone()
    }

    /// Log a change of the library wait once.
    fn note_library_wait(&self, wait: Option<&LibraryWait>) {
        let Ok(mut logged) = self.library_wait_logged.lock() else {
            return;
        };
        if logged.as_ref() == wait {
            return;
        }
        match wait {
            Some(wait) => tracing::warn!("downloads wait for the library: {}", wait.sentence()),
            None => tracing::info!("the library folder is back; downloads start again"),
        }
        *logged = wait.cloned();
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

    /// Run one journal write step, logging a failure with its name.
    async fn step<R, F>(&self, name: &'static str, op: F) -> Option<R>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(
                &'a crate::downloads::store::DownloadStore<'b>,
            ) -> Result<R, StoreError>
            + Send
            + 'static,
    {
        logged(name, self.journal.run(name, op).await)
    }

    /// Record a poll that changed nothing.
    async fn touch_poll(&self, task_id: &str, now: f64) {
        let task_id = task_id.to_owned();
        self.step("downloads.touch_poll", move |store| {
            store.touch_poll(&task_id, now)
        })
        .await;
    }

    /// Run one journal read on the read pool, logging a failure with its
    /// name.
    async fn read_step<R, F>(&self, name: &'static str, op: F) -> Option<R>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(
                &'a crate::downloads::store::DownloadStore<'b>,
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
}
