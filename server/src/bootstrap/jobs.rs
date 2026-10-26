//! Jobs bundle: the loop supervisor plus the loops this crate starts on it.
//!
//! [`JobsSetup`] is the one `AppState` field the jobs loops add. It holds the
//! [`Supervisor`] every loop runs on: the always-on boot loops, the events
//! kick one-shot, precache runs and the post-scan wait before one, and the
//! plugin tick loops (via the plugins bundle, which runs them on this same
//! supervisor). One supervisor, one rebuild choke point (`sync_ticks`), no
//! duplicate loop mechanics.
//!
//! The jobs table's runners start here too: [`QueueSetup`] builds the one
//! [`JobQueue`] beside the supervisor, areas take it to enqueue, and each
//! registered kind's handler gets a supervised runner at boot.
//!
//! Loop backends are real where they exist (checkpoint passes, the
//! now-playing feed and its upstream session pollers, the media servers'
//! MBID warmups, the Navidrome playlist export, events settings reads, the
//! concerts sweep, the personal mix); a backend a state does not wire runs
//! as a no-op.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use droppedneedle_catalog::concerts::ConcertsSweep;
use droppedneedle_kernel::db::CheckpointService;
use droppedneedle_kernel::db::cache::CacheError;
use droppedneedle_kernel::jobs::cache_sync::CacheSyncStatus;
use droppedneedle_kernel::jobs::checkpoint::{self, CheckpointRunner};
use droppedneedle_kernel::jobs::discovery_demand::{self, DemandTick, NoDemand};
use droppedneedle_kernel::jobs::events_kick::{self, EventsKick, FnKick, KickOutcome};
use droppedneedle_kernel::jobs::events_watcher::{self, PollTimeSource};
use droppedneedle_kernel::jobs::personal_mix::{self, PersonalMixer};
use droppedneedle_kernel::jobs::playlist_sync;
use droppedneedle_kernel::jobs::precache::{self, PrecacheLimits, PrecacheWork};
use droppedneedle_kernel::jobs::precache_phases::{LibraryPrecache, PrecacheSources};
use droppedneedle_kernel::jobs::presence;
use droppedneedle_kernel::jobs::queue::{self, Handlers, JobHandler, JobQueue, KindSpec};
use droppedneedle_kernel::jobs::{
    AlreadyRunning, BoxFuture, FnTick, Next, Schedule, Supervisor, Tick, TickCtx, TickError,
    TickResult,
};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::secret_sections::EventsSettings;
use droppedneedle_kernel::runtime_config::{ConfigStore, Live};
use droppedneedle_media::remotes::playlist_sync::{
    NavidromeSyncSettings, PlaylistSyncState, ProdPlaylistSync,
};
use droppedneedle_media::remotes::presence_feed::{MediaJobs, RegistryFeed, RemoteSessionPollers};
use droppedneedle_media::remotes::warmup::{self, Warmups};
use droppedneedle_providers::ProviderCacheStore;

/// Checkpoint passes over the runtime's service.
#[derive(Clone, Debug)]
pub struct CheckpointInput {
    service: CheckpointService,
}

impl CheckpointRunner for CheckpointInput {
    fn cycle(&self) -> BoxFuture<'_, Result<(), String>> {
        self.service.cycle()
    }
}

/// Personal-mix refresh without a mixer (a boot that wired none): cycles
/// succeed without rebuilding anything.
#[derive(Clone, Debug, Default)]
pub struct UnwiredMixer;

impl PersonalMixer for UnwiredMixer {
    fn run_for_all_users(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// Precache on states without the catalog and artwork behind it: every run
/// lands failed with the reason.
#[derive(Clone, Debug, Default)]
pub struct UnwiredPrecacheWork;

impl PrecacheWork for UnwiredPrecacheWork {
    fn run(&self, _progress: precache::Progress) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async {
            Err("Library precache has no catalog to warm on this server.".to_owned())
        })
    }
}

/// How long a cancel waits for the run to stop before aborting it (v2
/// waited 30 seconds for the sync task).
pub const PRECACHE_CANCEL_GRACE: Duration = Duration::from_secs(30);

/// Quiet time after the last completed scan before the automatic library
/// image refresh starts. It lets identification of new files settle and
/// folds a burst of scans (one per root, or a watcher storm) into one run.
pub const SCAN_PRECACHE_DEBOUNCE: Duration = Duration::from_secs(120);

/// Library scans that ended having changed something, counted. The
/// library bumps the count, as v2 refreshed after every library sync; the
/// [`precache::AFTER_SCAN_JOB`] loop waits on it, and each bump restarts
/// its [`SCAN_PRECACHE_DEBOUNCE`] wait before a run with no user (so no
/// discovery phase, like v2's automatic syncs). Safe to bump from any
/// thread.
#[derive(Clone)]
pub struct ScansEnded(Arc<watch::Sender<u64>>);

impl Default for ScansEnded {
    fn default() -> Self {
        Self(Arc::new(watch::channel(0).0))
    }
}

impl ScansEnded {
    /// Count one more ended scan.
    pub fn bump(&self) {
        self.0.send_modify(|count| *count = count.wrapping_add(1));
    }
}

/// A precache run with no progress for this long is stopped. Fixed, not a
/// setting: a healthy run beats far more often than this.
const PRECACHE_STALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A precache run is stopped after this long whatever it is doing. Fixed,
/// not a setting: long enough for a large library's first warm.
const PRECACHE_MAX_TIMEOUT: Duration = Duration::from_secs(8 * 3600);

/// The production precache trigger: one supervised run per call, under the
/// fixed watchdog limits.
#[derive(Clone)]
pub struct PrecacheTrigger {
    supervisor: Supervisor,
    status: CacheSyncStatus,
    sources: Option<Arc<dyn PrecacheSources>>,
    scans: ScansEnded,
}

impl PrecacheTrigger {
    /// Start one supervised run, started by `user_id` when a person asked
    /// for it. Rejected while a run is live, like any duplicate job name.
    /// The status settles when the run lands, however it ends.
    pub async fn run(
        &self,
        user_id: Option<String>,
    ) -> Result<precache::PrecacheHandle, AlreadyRunning> {
        let Some(sources) = self.sources.clone() else {
            return precache::spawn_run(
                &self.supervisor,
                UnwiredPrecacheWork,
                self.limits(),
                |_| {},
            );
        };
        let work = LibraryPrecache::new(sources, self.status.clone(), user_id);
        let settler = work.settler();
        precache::spawn_run(&self.supervisor, work, self.limits(), move |outcome| {
            settler.settle(outcome);
        })
    }

    /// Where the current run stands.
    pub fn status(&self) -> &CacheSyncStatus {
        &self.status
    }

    /// Stop the live run, if any: the status goes idle at once, then the
    /// run gets [`PRECACHE_CANCEL_GRACE`] to stop before it is aborted.
    /// Work already cached stays cached, so the next run carries on.
    pub async fn cancel(&self) {
        self.status.cancel();
        self.supervisor
            .cancel(precache::JOB_NAME, PRECACHE_CANCEL_GRACE)
            .await;
    }

    /// Watchdog limits for the next run.
    fn limits(&self) -> PrecacheLimits {
        PrecacheLimits::new(PRECACHE_STALL_TIMEOUT, PRECACHE_MAX_TIMEOUT)
    }

    /// Start the loop that turns scan completions into one run each time
    /// scans go quiet for [`SCAN_PRECACHE_DEBOUNCE`]. A run already going
    /// is left alone.
    pub fn spawn_after_scan(&self) -> Result<(), AlreadyRunning> {
        self.supervisor.on_wakeup(
            precache::AFTER_SCAN_JOB,
            self.scans.0.subscribe(),
            Schedule::new(Duration::MAX),
            AfterScanTick {
                trigger: self.clone(),
                folded: *self.scans.0.borrow(),
            },
        )
    }
}

/// One turn of the post-scan wait. `folded` is the scan count the last run
/// already covered: scans that ended during a quiet wait restart that wait
/// and leave the loop woken, and the turn that follows finds nothing new.
struct AfterScanTick {
    trigger: PrecacheTrigger,
    folded: u64,
}

impl Tick for AfterScanTick {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        let mut later = self.trigger.scans.0.subscribe();
        if *later.borrow_and_update() == self.folded {
            return Ok(Next::Continue);
        }
        // Each scan in the wait starts it again.
        loop {
            tokio::select! {
                biased;
                () = ctx.token().cancelled() => return Ok(Next::Finish),
                changed = later.changed() => {
                    if changed.is_err() {
                        return Ok(Next::Finish);
                    }
                }
                () = tokio::time::sleep(SCAN_PRECACHE_DEBOUNCE) => break,
            }
        }
        self.folded = *later.borrow_and_update();
        match self.trigger.run(None).await {
            Ok(_) => tracing::info!("library image refresh started after a scan"),
            Err(_) => tracing::debug!("library image refresh already running; scan kick skipped"),
        }
        Ok(Next::Continue)
    }
}

/// The admin's events `poll_time`, re-read every scheduler tick. An
/// unreadable section falls back to 06:00, the watcher's own fallback for
/// garbage values.
#[derive(Clone, Debug)]
pub struct EventsPollTime {
    events: Live<EventsSettings>,
}

impl PollTimeSource for EventsPollTime {
    fn poll_time(&self) -> String {
        self.events
            .get()
            .map(|settings| settings.poll_time.clone())
            .unwrap_or_else(|_| "06:00".to_owned())
    }
}

/// Everything `create_app` and `serve` need for jobs: the supervisor, the
/// playlist route state, and the boot-loop inputs.
#[derive(Clone)]
pub struct JobsSetup {
    supervisor: Supervisor,
    checkpoint: CheckpointInput,
    playlist: ProdPlaylistSync,
    presence: (RegistryFeed, RemoteSessionPollers),
    warmups: Warmups,
    poll_time: EventsPollTime,
    watcher: Option<ConcertsSweep>,
    mixer: Arc<dyn PersonalMixer>,
    demand: Arc<dyn DemandTick>,
    cache_sync: CacheSyncStatus,
    precache_sources: Option<Arc<dyn PrecacheSources>>,
    scans: ScansEnded,
    queues: Option<QueueSetup>,
    handlers: Handlers,
    /// Kinds registered through [`JobsSetup::with_job`].
    kinds: usize,
}

/// The jobs table handle the areas enqueue through, built once beside the
/// supervisor. Under test hooks its clock is a manual one and it carries
/// the scripted test kind.
#[derive(Clone)]
pub struct QueueSetup {
    queue: JobQueue,
    clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    #[cfg(any(test, feature = "test-support"))]
    scripted: Option<scripted::ScriptedJobs>,
}

impl std::fmt::Debug for QueueSetup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("QueueSetup").finish_non_exhaustive()
    }
}

impl QueueSetup {
    /// The jobs table on the platform's database and clock. Under test
    /// hooks it runs on a manual clock instead.
    pub fn new(platform: &Platform) -> Self {
        let db = platform.db.clone();
        #[cfg(any(test, feature = "test-support"))]
        if platform.config.test_hooks {
            use droppedneedle_kernel::clock::{Clock, ManualClock, SystemClock};
            let clock = ManualClock::at(SystemClock.now_ms());
            return Self {
                queue: JobQueue::new(db, Arc::new(clock.clone())),
                clock: Arc::new(clock.clone()),
                scripted: Some(scripted::ScriptedJobs::new(clock)),
            };
        }
        Self {
            queue: JobQueue::new(db, platform.clock.clone()),
            clock: platform.clock.clone(),
            #[cfg(any(test, feature = "test-support"))]
            scripted: None,
        }
    }

    /// The jobs table.
    pub fn queue(&self) -> &JobQueue {
        &self.queue
    }

    /// The clock the jobs table runs on, for a handler that measures time
    /// against its jobs' due times.
    pub fn clock(&self) -> Arc<dyn droppedneedle_kernel::clock::Clock> {
        self.clock.clone()
    }
}

impl JobsSetup {
    /// Bind the backends over the platform's supervisor, settings and
    /// event hub: live checkpoint passes, the media feeds (presence, MBID
    /// warmups, playlist export), the concerts sweep (`None` only on states
    /// without concerts) and the count of ended scans the after-scan refresh
    /// waits on.
    pub fn build(
        platform: &Platform,
        checkpoint: CheckpointService,
        media: MediaJobs,
        watcher: Option<ConcertsSweep>,
        scans: ScansEnded,
    ) -> Self {
        let config = platform.settings.clone();
        Self {
            supervisor: platform.supervisor.clone(),
            checkpoint: CheckpointInput {
                service: checkpoint,
            },
            playlist: PlaylistSyncState {
                settings: NavidromeSyncSettings::new(config.live()),
                exporter: media.exporter(),
            },
            presence: (media.feed(), media.pollers()),
            warmups: media.warmups(),
            poll_time: EventsPollTime {
                events: config.live(),
            },
            watcher,
            mixer: Arc::new(UnwiredMixer),
            demand: Arc::new(NoDemand),
            cache_sync: CacheSyncStatus::new(platform.events.clone(), platform.clock.clone()),
            precache_sources: None,
            scans,
            queues: None,
            handlers: Handlers::default(),
            kinds: 0,
        }
    }

    /// The jobs table whose registered kinds get runners at boot.
    pub fn with_queue(mut self, queues: QueueSetup) -> Self {
        self.queues = Some(queues);
        self
    }

    /// Register one job kind's handler. Every kind that can be enqueued is
    /// registered, so none waits with nothing to run it.
    pub fn with_job(mut self, spec: KindSpec, handler: Arc<dyn JobHandler>) -> Self {
        self.handlers = self.handlers.with(spec, handler);
        self.kinds += 1;
        self
    }

    /// Register several kinds' handlers, as [`JobsSetup::with_job`] does.
    pub fn with_jobs(self, jobs: Vec<(KindSpec, Arc<dyn JobHandler>)>) -> Self {
        jobs.into_iter()
            .fold(self, |setup, (spec, handler)| setup.with_job(spec, handler))
    }

    /// The jobs table, once [`JobsSetup::with_queue`] gave it one.
    pub fn queue(&self) -> Option<&JobQueue> {
        self.queues.as_ref().map(QueueSetup::queue)
    }

    /// The scripted test kind, when the app was built with test hooks.
    #[cfg(any(test, feature = "test-support"))]
    pub fn scripted(&self) -> Option<&scripted::ScriptedJobs> {
        self.queues
            .as_ref()
            .and_then(|queues| queues.scripted.as_ref())
    }
    /// The catalog and artwork lookups library precache runs warm.
    pub fn with_precache_sources(mut self, sources: Arc<dyn PrecacheSources>) -> Self {
        self.precache_sources = Some(sources);
        self
    }

    /// Library precache over the reads' catalog and artwork service, when
    /// both are wired; otherwise runs stay unwired and fail with a reason.
    pub fn with_precache_sources_from(
        self,
        reads: &droppedneedle_catalog::reads::ReadsSetup,
        config: &Arc<ConfigStore>,
    ) -> Self {
        match (&reads.catalog, &reads.artwork) {
            (Some(catalog), Some(artwork)) => self.with_precache_sources(Arc::new(
                droppedneedle_catalog::reads::catalog::precache::CatalogPrecache::new(
                    catalog.catalog.clone(),
                    artwork.clone(),
                    config.live(),
                ),
            )),
            _ => {
                tracing::warn!(
                    "library precache has no catalog or artwork service; runs will fail"
                );
                self
            }
        }
    }

    /// Run the page image warm jobs through the reads' artwork service.
    /// Without one the kind is not registered, and the start-up check
    /// warns about any rows left queued.
    pub fn with_cover_warm_jobs(self, reads: &droppedneedle_catalog::reads::ReadsSetup) -> Self {
        use droppedneedle_catalog::reads::platform::artwork::warm::{self, CoverWarmJobs};
        match &reads.artwork {
            Some(artwork) => {
                self.with_job(warm::SPEC, Arc::new(CoverWarmJobs::new(artwork.clone())))
            }
            None => {
                tracing::warn!("no artwork service; page images are not warmed");
                self
            }
        }
    }

    /// The personal-mix builder the daily refresh loop drives.
    pub fn with_mixer(mut self, mixer: Arc<dyn PersonalMixer>) -> Self {
        self.mixer = mixer;
        self
    }

    /// The discover warm pass the warm-cycle loop drives.
    pub fn with_discovery_demand(mut self, demand: Arc<dyn DemandTick>) -> Self {
        self.demand = demand;
        self
    }

    /// The supervisor every loop runs on. The plugins bundle takes a clone so
    /// tick loops (`plugin-tick:{name}`) run on this same mechanism, and
    /// shutdown stops everything through it.
    pub fn supervisor(&self) -> &Supervisor {
        &self.supervisor
    }

    /// The precache trigger: one supervised run per call. The admin route
    /// owns the only production caller.
    pub fn precache_trigger(&self) -> PrecacheTrigger {
        PrecacheTrigger {
            supervisor: self.supervisor.clone(),
            status: self.cache_sync.clone(),
            sources: self.precache_sources.clone(),
            scans: self.scans.clone(),
        }
    }

    /// The playlist sync route's state, shared with the sync loop.
    pub fn playlist_sync(&self) -> ProdPlaylistSync {
        self.playlist.clone()
    }

    /// The settings-save kick: one immediate events sweep on the supervisor,
    /// started off the save path so the save never waits on it. Overlapping
    /// kicks collapse (the sweep is idempotent).
    pub fn events_kick(&self) -> Arc<dyn EventsKick> {
        let supervisor = self.supervisor.clone();
        let watcher = self.watcher.clone();
        Arc::new(FnKick {
            kick_fn: move || {
                if events_kick::kick(&supervisor, watcher.clone()) == KickOutcome::Started {
                    tracing::debug!("kicked events sweep started");
                }
            },
        })
    }

    /// Start the always-on loops ([`droppedneedle_kernel::jobs::BOOT_JOBS`]
    /// and the media servers' MBID warmups), and the post-scan precache
    /// wait when precache is wired. Boot is the only caller, so a name
    /// collision fails the boot loudly instead of
    /// running half the loops, and so do job kinds registered without a
    /// jobs table, which would otherwise never run.
    pub fn spawn_loops(&self) -> Result<(), String> {
        if self.queues.is_none() && self.kinds > 0 {
            return Err("job kinds are registered but no jobs table was given".to_owned());
        }
        checkpoint::spawn_on(
            &self.supervisor,
            self.checkpoint.clone(),
            checkpoint::default_schedule(),
        )
        .map_err(already_running(checkpoint::JOB_NAME))?;
        presence::spawn_on(
            &self.supervisor,
            self.presence.0.clone(),
            self.presence.1.clone(),
            presence::default_schedule(),
        )
        .map_err(already_running(presence::JOB_NAME))?;
        personal_mix::spawn_on(
            &self.supervisor,
            Arc::clone(&self.mixer),
            personal_mix::default_schedule(),
        )
        .map_err(already_running(personal_mix::JOB_NAME))?;
        playlist_sync::spawn_on(
            &self.supervisor,
            self.playlist.settings.clone(),
            self.playlist.exporter.clone(),
            playlist_sync::default_schedule(),
        )
        .map_err(already_running(playlist_sync::JOB_NAME))?;
        events_watcher::spawn_on(
            &self.supervisor,
            self.watcher.clone(),
            self.poll_time.clone(),
            events_watcher::default_schedule(),
        )
        .map_err(already_running(events_watcher::JOB_NAME))?;
        discovery_demand::spawn_on(
            &self.supervisor,
            Arc::clone(&self.demand),
            discovery_demand::default_schedule(),
        )
        .map_err(already_running(discovery_demand::JOB_NAME))?;
        warmup::start_all(&self.supervisor, &self.warmups)
            .map_err(|_| "a remotes warmup is already running".to_owned())?;
        if self.precache_sources.is_some() {
            self.precache_trigger()
                .spawn_after_scan()
                .map_err(already_running(precache::AFTER_SCAN_JOB))?;
        }
        if let Some(queues) = &self.queues {
            #[cfg_attr(not(any(test, feature = "test-support")), allow(unused_mut))]
            let mut handlers = self.handlers.clone();
            #[cfg(any(test, feature = "test-support"))]
            if let Some(kit) = &queues.scripted {
                handlers = handlers.with(scripted::ScriptedJobs::spec(), Arc::new(kit.clone()));
            }
            queue::start_runners(&queues.queue, &self.supervisor, handlers)
                .map_err(|_| "a job runner is already running".to_owned())?;
        }
        Ok(())
    }
}

/// The test kind the builder registers under test hooks: its payload says
/// how each run answers, and the kit counts runs per key.
#[cfg(any(test, feature = "test-support"))]
pub mod scripted {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use droppedneedle_kernel::clock::ManualClock;
    use droppedneedle_kernel::jobs::BoxFuture;
    use droppedneedle_kernel::jobs::queue::{
        JobCtx, JobHandler, JobKind, KindSpec, Outcome, PAYLOAD_INVALID,
    };
    use serde::Deserialize;
    use tokio::sync::Semaphore;

    /// The scripted kind.
    pub const SCRIPTED: JobKind = JobKind::new("test.scripted");
    /// The code a scripted retry answers with.
    pub const RETRY_CODE: &str = "TEST_RETRY";
    /// The code a scripted failure answers with.
    pub const FAIL_CODE: &str = "TEST_FAILED";

    /// How one run answers.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Answer {
        Done,
        Retry,
        Fail,
    }

    #[derive(Debug, Deserialize)]
    struct Script {
        answer: Answer,
        #[serde(default)]
        gate: bool,
        #[serde(default)]
        at: Option<i64>,
    }

    /// The jobs table's manual clock, the run counts and the gate.
    #[derive(Clone, Debug)]
    pub struct ScriptedJobs {
        clock: ManualClock,
        runs: Arc<Mutex<HashMap<String, u32>>>,
        gate: Arc<Semaphore>,
    }

    impl ScriptedJobs {
        pub(super) fn new(clock: ManualClock) -> Self {
            Self {
                clock,
                runs: Arc::default(),
                gate: Arc::new(Semaphore::new(0)),
            }
        }

        /// How the scripted kind runs: three attempts, the other defaults.
        pub const fn spec() -> KindSpec {
            KindSpec::new(SCRIPTED).max_attempts(3)
        }

        /// The jobs table's clock.
        pub fn clock(&self) -> &ManualClock {
            &self.clock
        }

        /// Runs started for `key` since this boot.
        pub fn runs(&self, key: &str) -> u32 {
            self.lock().get(key).copied().unwrap_or(0)
        }

        /// Let one gated run through.
        pub fn open_gate(&self) {
            self.gate.add_permits(1);
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, u32>> {
            self.runs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    impl JobHandler for ScriptedJobs {
        fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
            let kit = self.clone();
            Box::pin(async move {
                let Ok(script) = job.payload::<Script>() else {
                    return Outcome::Fail {
                        code: PAYLOAD_INVALID,
                    };
                };
                *kit.lock().entry(job.key().to_owned()).or_default() += 1;
                if script.gate {
                    tokio::select! {
                        permit = kit.gate.acquire() => {
                            if let Ok(permit) = permit {
                                permit.forget();
                            }
                        }
                        () = job.token().cancelled() => return Outcome::Busy,
                    }
                }
                match script.answer {
                    Answer::Done => Outcome::Done,
                    Answer::Retry => Outcome::Retry {
                        code: RETRY_CODE,
                        at: script.at,
                    },
                    Answer::Fail => Outcome::Fail { code: FAIL_CODE },
                }
            })
        }
    }
}

/// The boot error for a loop name that is already taken.
fn already_running(name: &'static str) -> impl FnOnce(AlreadyRunning) -> String {
    move |_| format!("{name} is already running")
}

/// The provider cache sweep's loop name.
pub const PROVIDER_CACHE_SWEEP: &str = "provider-cache-sweep";
/// How often rows past their keep time are deleted.
pub const PROVIDER_CACHE_SWEEP_EVERY: Duration = Duration::from_secs(3_600);

/// Start the hourly sweep of the provider cache file: rows past their keep
/// time go, then the file's journal is cut back. The first pass waits a
/// whole interval, so boot never pays for it.
pub fn spawn_provider_cache_sweep(
    supervisor: &Supervisor,
    cache: ProviderCacheStore,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        PROVIDER_CACHE_SWEEP,
        Schedule::new(PROVIDER_CACHE_SWEEP_EVERY)
            .with_initial_delay(PROVIDER_CACHE_SWEEP_EVERY)
            .with_jitter(Duration::from_secs(60))
            .with_jittered_start(),
        FnTick(move |_| {
            let cache = cache.clone();
            async move {
                match cache.sweep().await {
                    Ok(swept) => {
                        tracing::debug!(swept, "provider cache swept");
                        Ok(Next::Continue)
                    }
                    // The cache is off for this run: nothing to sweep.
                    Err(CacheError::Off) => Ok(Next::Finish),
                    Err(error) => Err(TickError::new(
                        "PROVIDER_CACHE_SWEEP_FAILED",
                        error.to_string(),
                    )),
                }
            }
        }),
    )
}
