//! Jobs bundle: the loop supervisor plus the loops this crate starts on it.
//!
//! [`JobsSetup`] is the one `AppState` field the jobs loops add. It holds the
//! [`Supervisor`] every loop runs on: the always-on boot loops, the events
//! kick one-shot, precache runs and the post-scan wait before one, and the
//! plugin tick loops (via the plugins bundle, which runs them on this same
//! supervisor). One supervisor, one rebuild choke point (`sync_ticks`), no
//! duplicate loop mechanics.
//!
//! Loop backends are real where they exist (checkpoint passes, the
//! now-playing feed and its upstream session pollers, the Navidrome playlist
//! export, events settings reads, the concerts sweep, the personal mix);
//! a backend a state does not wire runs as a no-op.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tokio::sync::watch;

use droppedneedle_catalog::concerts::ConcertsSweep;
use droppedneedle_kernel::db::CheckpointService;
use droppedneedle_kernel::jobs::cache_sync::CacheSyncStatus;
use droppedneedle_kernel::jobs::checkpoint::{self, CheckpointRunner};
use droppedneedle_kernel::jobs::discovery_demand::{self, DemandTick, NoDemand};
use droppedneedle_kernel::jobs::events_kick::{self, EventsKick, FnKick, KickOutcome};
use droppedneedle_kernel::jobs::events_watcher::{self, PollTimeSource};
use droppedneedle_kernel::jobs::personal_mix::{self, PersonalMixer};
use droppedneedle_kernel::jobs::playlist_export::M3uPlaylistExporter;
use droppedneedle_kernel::jobs::playlist_sync::{self, PlaylistSyncConfig, PlaylistSyncSettings};
use droppedneedle_kernel::jobs::precache::{self, PrecacheLimits, PrecacheWork};
use droppedneedle_kernel::jobs::precache_phases::{LibraryPrecache, PrecacheSources};
use droppedneedle_kernel::jobs::presence;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, BoxFuture, Next, Schedule, Supervisor, Tick, TickCtx, TickResult,
};
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::secret_sections::{
    AdvancedSettings, EventsSettings, NavidromeConnection,
};
use droppedneedle_media::remotes::playlist_sync::{self as playlist_route, PlaylistSyncState};
use droppedneedle_media::remotes::presence_feed::{MediaJobs, RegistryFeed, RemoteSessionPollers};

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

/// Playlist sync config from saved settings, re-read every cycle and every
/// route call. `None` (unreadable section, disabled, or an empty target
/// path) means skip quietly.
#[derive(Clone, Debug)]
pub struct NavidromeSyncSettings {
    store: Arc<ConfigStore>,
}

impl PlaylistSyncSettings for NavidromeSyncSettings {
    fn sync_config(&self) -> BoxFuture<'_, Option<PlaylistSyncConfig>> {
        Box::pin(async move {
            let Ok(connection) = self.store.get_raw::<NavidromeConnection>() else {
                return None;
            };
            if !connection.enabled
                || !connection.playlist_sync_enabled
                || connection.playlist_sync_path.trim().is_empty()
            {
                return None;
            }
            Some(PlaylistSyncConfig {
                target_dir: connection.playlist_sync_path,
                scope: connection.playlist_sync_scope,
                remove_deleted: connection.playlist_sync_remove_deleted,
            })
        })
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

/// The production precache trigger: one supervised run per call. Watchdog
/// limits re-read the advanced settings on every run; an unreadable section
/// falls back to the shipped timeouts.
#[derive(Clone)]
pub struct PrecacheTrigger {
    supervisor: Supervisor,
    config: Arc<ConfigStore>,
    status: CacheSyncStatus,
    sources: Option<Arc<dyn PrecacheSources>>,
    scans: Arc<watch::Sender<u64>>,
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

    /// The hook completed scans call, as v2 refreshed after every library
    /// sync: each call restarts the [`SCAN_PRECACHE_DEBOUNCE`] wait in the
    /// [`precache::AFTER_SCAN_JOB`] loop, which then starts a run with no
    /// user (so no discovery phase, like v2's automatic syncs). Safe to call
    /// from any thread.
    pub fn after_scan_hook(&self) -> Arc<dyn Fn() + Send + Sync> {
        let scans = Arc::clone(&self.scans);
        let wired = self.sources.is_some();
        Arc::new(move || {
            if wired {
                scans.send_modify(|count| *count = count.wrapping_add(1));
            }
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

    /// Watchdog limits for the next run. Values clamp to at least one unit;
    /// a garbage section must not arm a zero stall timeout.
    fn limits(&self) -> PrecacheLimits {
        let fallback = AdvancedSettings::default();
        let (stall_minutes, max_hours) = self
            .config
            .get_raw::<AdvancedSettings>()
            .map(|settings| {
                (
                    settings.sync_stall_timeout_minutes,
                    settings.sync_max_timeout_hours,
                )
            })
            .unwrap_or((
                fallback.sync_stall_timeout_minutes,
                fallback.sync_max_timeout_hours,
            ));
        PrecacheLimits::new(
            Duration::from_secs(stall_minutes.max(1) as u64 * 60),
            Duration::from_secs(max_hours.max(1) as u64 * 3600),
        )
    }

    /// Start the loop that turns scan completions into one run each time
    /// scans go quiet for [`SCAN_PRECACHE_DEBOUNCE`]. A run already going
    /// is left alone.
    pub fn spawn_after_scan(&self) -> Result<(), AlreadyRunning> {
        self.supervisor.on_wakeup(
            precache::AFTER_SCAN_JOB,
            self.scans.subscribe(),
            Schedule::new(Duration::MAX),
            AfterScanTick {
                trigger: self.clone(),
                folded: *self.scans.borrow(),
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
        let mut later = self.trigger.scans.subscribe();
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
    store: Arc<ConfigStore>,
}

impl PollTimeSource for EventsPollTime {
    fn poll_time(&self) -> String {
        self.store
            .get_raw::<EventsSettings>()
            .map(|settings| settings.poll_time)
            .unwrap_or_else(|_| "06:00".to_owned())
    }
}

/// Everything `create_app` and `serve` need for jobs: the supervisor, the
/// playlist route state, and the boot-loop inputs.
#[derive(Clone)]
pub struct JobsSetup {
    supervisor: Supervisor,
    checkpoint: CheckpointInput,
    playlist: PlaylistSyncState<NavidromeSyncSettings, M3uPlaylistExporter>,
    presence: (RegistryFeed, RemoteSessionPollers),
    poll_time: EventsPollTime,
    watcher: Option<ConcertsSweep>,
    config: Arc<ConfigStore>,
    mixer: Arc<dyn PersonalMixer>,
    demand: Arc<dyn DemandTick>,
    cache_sync: CacheSyncStatus,
    precache_sources: Option<Arc<dyn PrecacheSources>>,
    scans: Arc<watch::Sender<u64>>,
}

impl JobsSetup {
    /// Bind the backends over a started supervisor: live checkpoint passes,
    /// settings reads over the shared store, the media feeds (presence,
    /// playlist export), and the concerts sweep (`None` only on states
    /// without concerts).
    pub fn build(
        supervisor: Supervisor,
        checkpoint: CheckpointService,
        config: Arc<ConfigStore>,
        media: MediaJobs,
        watcher: Option<ConcertsSweep>,
    ) -> Self {
        Self {
            supervisor,
            checkpoint: CheckpointInput {
                service: checkpoint,
            },
            playlist: PlaylistSyncState {
                settings: NavidromeSyncSettings {
                    store: Arc::clone(&config),
                },
                exporter: media.exporter(),
            },
            presence: (media.feed(), media.pollers()),
            poll_time: EventsPollTime {
                store: Arc::clone(&config),
            },
            watcher,
            config,
            mixer: Arc::new(UnwiredMixer),
            demand: Arc::new(NoDemand),
            cache_sync: CacheSyncStatus::default(),
            precache_sources: None,
            scans: Arc::new(watch::channel(0).0),
        }
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
                    Arc::clone(config),
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

    /// The precache status; its sink attaches to the event hub.
    pub fn cache_sync(&self) -> &CacheSyncStatus {
        &self.cache_sync
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
            config: Arc::clone(&self.config),
            status: self.cache_sync.clone(),
            sources: self.precache_sources.clone(),
            scans: Arc::clone(&self.scans),
        }
    }

    /// Settings-nested routes jobs owns: `POST
    /// /settings/navidrome/playlist-sync` (the v2 path). Mounts inside the
    /// session gate with the other `/api/v3` routes.
    pub fn settings_router(&self) -> Router {
        Router::new().nest("/settings", playlist_route::router(self.playlist.clone()))
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

    /// Start the always-on loops ([`droppedneedle_kernel::jobs::BOOT_JOBS`]),
    /// and the post-scan precache wait when precache is wired. Boot is the
    /// only caller, so a name collision fails the boot loudly instead of
    /// running half the loops.
    pub fn spawn_loops(&self) -> Result<(), String> {
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
        if self.precache_sources.is_some() {
            self.precache_trigger()
                .spawn_after_scan()
                .map_err(already_running(precache::AFTER_SCAN_JOB))?;
        }
        Ok(())
    }
}

/// The boot error for a loop name that is already taken.
fn already_running(name: &'static str) -> impl FnOnce(AlreadyRunning) -> String {
    move |_| format!("{name} is already running")
}
