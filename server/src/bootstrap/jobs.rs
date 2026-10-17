//! Jobs bundle: one registry plus the loops it owns.
//!
//! [`JobsSetup`] is the one `AppState` field the jobs loops add. It owns the single
//! [`JobRegistry`] every jobs loop registers on: the five always-on boot
//! loops, the events kick one-shot, precache runs, and the plugin tick
//! loops (via the plugins bundle, which shares this registry rather than
//! running its own). One registry, one rebuild choke point
//! (`sync_ticks`), no duplicate loop mechanics.
//!
//! Production binds [`DurableRegistryStore`] (liveness rows survive
//! restarts); test states bind
//! [`MemoryRegistryStore`](droppedneedle_kernel::jobs::registry::MemoryRegistryStore)
//! behind the same [`StoreKind`] seam so the setup type stays concrete.
//! Loop backends are real where they exist (checkpoint passes, the
//! now-playing feed and its upstream session pollers, the Navidrome playlist
//! export, events settings reads, the concerts sweep, the personal mix) and
//! no-ops where their services do not exist yet; each interim adapter says
//! what is missing.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;

use droppedneedle_catalog::concerts::ConcertsSweep;
use droppedneedle_kernel::db::{CheckpointService, DurableWorkWakeups, WriteLane};
use droppedneedle_kernel::jobs::cache_sync::CacheSyncStatus;
use droppedneedle_kernel::jobs::checkpoint::{self, CheckpointRunner};
use droppedneedle_kernel::jobs::discovery_demand::{self, DemandTick, NoDemand};
use droppedneedle_kernel::jobs::events_kick::{self, EventsKick, FnKick, KickOutcome};
use droppedneedle_kernel::jobs::events_watcher::{self, PollTimeSource, SystemWatchClock};
use droppedneedle_kernel::jobs::personal_mix::{self, PersonalMixer};
use droppedneedle_kernel::jobs::playlist_export::M3uPlaylistExporter;
use droppedneedle_kernel::jobs::playlist_sync::{self, PlaylistSyncConfig, PlaylistSyncSettings};
use droppedneedle_kernel::jobs::precache::{self, PrecacheLimits, PrecacheWork};
use droppedneedle_kernel::jobs::precache_phases::{LibraryPrecache, PrecacheSources};
use droppedneedle_kernel::jobs::presence;
use droppedneedle_kernel::jobs::registry::{
    AlreadyRunning, BoxFuture, DurableRegistryStore, JobRegistry, StoreKind,
};
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::secret_sections::{
    AdvancedSettings, EventsSettings, NavidromeConnection,
};
use droppedneedle_media::remotes::playlist_sync::{self as playlist_route, PlaylistSyncState};
use droppedneedle_media::remotes::presence_feed::{MediaJobs, RegistryFeed, RemoteSessionPollers};

/// Grace per job at shutdown. Loops select on their stop signal next to
/// every sleep, so this only binds a cycle that is mid-flight.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Checkpoint passes: the real service in production, skipped cycles on
/// states without one (test states never spawn boot loops anyway).
#[derive(Clone, Debug, Default)]
pub struct CheckpointInput {
    service: Option<CheckpointService>,
}

impl CheckpointRunner for CheckpointInput {
    fn cycle(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(service) = &self.service {
                service.cycle().await;
            }
        })
    }
}

/// Personal-mix refresh without a mixer (test states, or a boot that
/// wired none): cycles succeed without rebuilding anything.
#[derive(Clone, Debug, Default)]
pub struct UnwiredMixer;

impl PersonalMixer for UnwiredMixer {
    fn run_for_all_users(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// Playlist sync config from saved settings, re-read every cycle and every
/// route call. `None` (unwired store, unreadable section, disabled, or an
/// empty target path) means skip quietly.
#[derive(Clone, Debug, Default)]
pub struct NavidromeSyncSettings {
    store: Option<Arc<ConfigStore>>,
}

impl PlaylistSyncSettings for NavidromeSyncSettings {
    fn sync_config(&self) -> BoxFuture<'_, Option<PlaylistSyncConfig>> {
        Box::pin(async move {
            let Some(store) = &self.store else {
                return None;
            };
            let Ok(connection) = store.get_raw::<NavidromeConnection>() else {
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

/// Precache on states without the catalog and artwork behind it (test
/// states): every run lands failed with the reason.
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

/// The production precache trigger: one supervised run per call through the
/// shared registry. Watchdog limits re-read the advanced settings on every
/// run; an unwired store or unreadable section falls back to the shipped
/// timeouts.
#[derive(Clone)]
pub struct PrecacheTrigger {
    registry: JobRegistry<StoreKind>,
    config: Option<Arc<ConfigStore>>,
    status: CacheSyncStatus,
    sources: Option<Arc<dyn PrecacheSources>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
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
            return precache::spawn_run(&self.registry, UnwiredPrecacheWork, self.limits()).await;
        };
        let work = LibraryPrecache::new(sources, self.status.clone(), user_id);
        let settler = work.settler();
        let handle = precache::spawn_run(&self.registry, work, self.limits()).await?;
        let waiter = handle.clone();
        tokio::spawn(async move {
            let outcome = waiter.wait().await;
            settler.settle(&outcome);
        });
        Ok(handle)
    }

    /// The hook completed scans call, as v2 refreshed after every library
    /// sync: each call restarts a [`SCAN_PRECACHE_DEBOUNCE`] wait, and the
    /// last one starts a run with no user (so no discovery phase, like v2's
    /// automatic syncs). A run already going is left alone.
    pub fn after_scan_hook(&self) -> Arc<dyn Fn() + Send + Sync> {
        use std::sync::atomic::{AtomicU64, Ordering};
        let trigger = self.clone();
        let latest = Arc::new(AtomicU64::new(0));
        Arc::new(move || {
            if trigger.sources.is_none() {
                return;
            }
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                tracing::warn!("scan finished outside the runtime; library image refresh skipped");
                return;
            };
            let ticket = latest.fetch_add(1, Ordering::SeqCst) + 1;
            let trigger = trigger.clone();
            let latest = Arc::clone(&latest);
            runtime.spawn(async move {
                tokio::time::sleep(SCAN_PRECACHE_DEBOUNCE).await;
                // A newer scan restarted the wait, or the server is
                // shutting down: no run from this timer.
                if latest.load(Ordering::SeqCst) != ticket || trigger.closed.load(Ordering::SeqCst)
                {
                    return;
                }
                match trigger.run(None).await {
                    Ok(_) => tracing::info!("library image refresh started after a scan"),
                    Err(_) => {
                        tracing::debug!("library image refresh already running; scan kick skipped")
                    }
                }
            });
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
        self.registry
            .cancel(precache::JOB_NAME, PRECACHE_CANCEL_GRACE)
            .await;
    }

    /// Watchdog limits for the next run. Values clamp to at least one unit;
    /// a garbage section must not arm a zero stall timeout.
    fn limits(&self) -> PrecacheLimits {
        let fallback = AdvancedSettings::default();
        let (stall_minutes, max_hours) = self
            .config
            .as_ref()
            .and_then(|store| store.get_raw::<AdvancedSettings>().ok())
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
}

/// The admin's events `poll_time`, re-read every scheduler tick. Missing
/// stores and unreadable sections fall back to 06:00, the watcher's own
/// fallback for garbage values.
#[derive(Clone, Debug, Default)]
pub struct EventsPollTime {
    store: Option<Arc<ConfigStore>>,
}

impl PollTimeSource for EventsPollTime {
    fn poll_time(&self) -> String {
        self.store
            .as_ref()
            .and_then(|store| store.get_raw::<EventsSettings>().ok())
            .map(|settings| settings.poll_time)
            .unwrap_or_else(|| "06:00".to_owned())
    }
}

/// Everything `create_app` and `serve` need for jobs: the shared
/// registry, the playlist route state, and the boot-loop inputs.
#[derive(Clone)]
pub struct JobsSetup {
    registry: JobRegistry<StoreKind>,
    checkpoint: CheckpointInput,
    playlist: PlaylistSyncState<NavidromeSyncSettings, M3uPlaylistExporter>,
    presence: (RegistryFeed, RemoteSessionPollers),
    poll_time: EventsPollTime,
    watcher: Option<ConcertsSweep>,
    config: Option<Arc<ConfigStore>>,
    mixer: Arc<dyn PersonalMixer>,
    demand: Arc<dyn DemandTick>,
    cache_sync: CacheSyncStatus,
    precache_sources: Option<Arc<dyn PrecacheSources>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl JobsSetup {
    /// Bind the production backends: durable rows, live checkpoint passes,
    /// settings reads over the shared store, the media feeds (presence,
    /// playlist export), and the concerts sweep (`None` only on states without concerts).
    pub fn build(
        wakeups: DurableWorkWakeups,
        lane: WriteLane,
        checkpoint: CheckpointService,
        config: Arc<ConfigStore>,
        media: MediaJobs,
        watcher: Option<ConcertsSweep>,
    ) -> Self {
        Self {
            registry: JobRegistry::new(StoreKind::Durable(DurableRegistryStore::new(
                wakeups, lane,
            ))),
            checkpoint: CheckpointInput {
                service: Some(checkpoint),
            },
            playlist: PlaylistSyncState {
                settings: NavidromeSyncSettings {
                    store: Some(Arc::clone(&config)),
                },
                exporter: media.exporter(),
            },
            presence: (media.feed(), media.pollers()),
            poll_time: EventsPollTime {
                store: Some(Arc::clone(&config)),
            },
            watcher,
            config: Some(config),
            mixer: Arc::new(UnwiredMixer),
            demand: Arc::new(NoDemand),
            cache_sync: CacheSyncStatus::default(),
            precache_sources: None,
            closed: Default::default(),
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

    /// Test bundle: memory rows, skipped checkpoint passes and sync off.
    /// Route tests exercise the playlist route through this; loop tests
    /// bind the memory stores directly.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(_users: droppedneedle_auth::users::UsersDeps) -> Self {
        let media = MediaJobs::for_tests();
        Self {
            registry: JobRegistry::new(StoreKind::Memory(
                droppedneedle_kernel::jobs::registry::MemoryRegistryStore::new(),
            )),
            checkpoint: CheckpointInput { service: None },
            playlist: PlaylistSyncState {
                settings: NavidromeSyncSettings { store: None },
                exporter: media.exporter(),
            },
            presence: (media.feed(), media.pollers()),
            poll_time: EventsPollTime { store: None },
            watcher: None,
            config: None,
            mixer: Arc::new(UnwiredMixer),
            demand: Arc::new(NoDemand),
            cache_sync: CacheSyncStatus::default(),
            precache_sources: None,
            closed: Default::default(),
        }
    }

    /// The shared registry. The plugins bundle takes a clone so tick loops
    /// register on this same mechanism (`plugin-tick:{name}`), never on a
    /// second one.
    pub fn registry(&self) -> &JobRegistry<StoreKind> {
        &self.registry
    }

    /// The precache trigger: one supervised run per call through the shared
    /// registry. The admin route owns the only production caller.
    pub fn precache_trigger(&self) -> PrecacheTrigger {
        PrecacheTrigger {
            registry: self.registry.clone(),
            config: self.config.clone(),
            status: self.cache_sync.clone(),
            sources: self.precache_sources.clone(),
            closed: Arc::clone(&self.closed),
        }
    }

    /// Settings-nested routes jobs owns: `POST
    /// /settings/navidrome/playlist-sync` (the v2 path). Mounts inside the
    /// session gate with the other `/api/v3` routes.
    pub fn settings_router(&self) -> Router {
        Router::new().nest("/settings", playlist_route::router(self.playlist.clone()))
    }

    /// The settings-save kick: one immediate events sweep through the
    /// registry, spawned off the save path so the save never waits on it.
    /// Overlapping kicks collapse (the sweep is idempotent).
    pub fn events_kick(&self) -> Arc<dyn EventsKick> {
        let registry = self.registry.clone();
        let watcher = self.watcher.clone();
        Arc::new(FnKick {
            kick_fn: move || {
                let registry = registry.clone();
                let watcher = watcher.clone();
                tokio::spawn(async move {
                    if events_kick::kick(&registry, watcher).await == KickOutcome::Started {
                        tracing::debug!("kicked events sweep started");
                    }
                });
            },
        })
    }

    /// Spawn the always-on loops ([`droppedneedle_kernel::jobs::BOOT_JOBS`]) on the shared
    /// registry. Boot is the only spawner, so a name collision fails the
    /// boot loudly instead of running half the loops.
    pub async fn spawn_loops(&self) -> Result<(), String> {
        checkpoint::spawn_on(
            &self.registry,
            self.checkpoint.clone(),
            checkpoint::default_schedule(),
        )
        .await
        .map_err(|_| format!("{} is already running", checkpoint::JOB_NAME))?;
        presence::spawn_on(
            &self.registry,
            self.presence.0.clone(),
            self.presence.1.clone(),
            presence::default_schedule(),
        )
        .await
        .map_err(|_| format!("{} is already running", presence::JOB_NAME))?;
        personal_mix::spawn_on(
            &self.registry,
            Arc::clone(&self.mixer),
            personal_mix::default_schedule(),
        )
        .await
        .map_err(|_| format!("{} is already running", personal_mix::JOB_NAME))?;
        playlist_sync::spawn_on(
            &self.registry,
            self.playlist.settings.clone(),
            self.playlist.exporter.clone(),
            playlist_sync::default_schedule(),
        )
        .await
        .map_err(|_| format!("{} is already running", playlist_sync::JOB_NAME))?;
        events_watcher::spawn_on(
            &self.registry,
            self.watcher.clone(),
            self.poll_time.clone(),
            SystemWatchClock,
        )
        .await
        .map_err(|_| format!("{} is already running", events_watcher::JOB_NAME))?;
        discovery_demand::spawn_on(
            &self.registry,
            Arc::clone(&self.demand),
            discovery_demand::default_schedule(),
        )
        .await
        .map_err(|_| format!("{} is already running", discovery_demand::JOB_NAME))?;
        Ok(())
    }

    /// Cancel every live job (boot loops, the kick, precache runs, and
    /// plugin ticks; `plugin-tick:*` shares this registry), each with the
    /// same grace. Shutdown calls this before awaiting the loops.
    pub async fn cancel_all(&self, grace: Duration) {
        // Pending post-scan timers must not start a run once shutdown began.
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.registry.cancel_all(grace).await;
    }
}
