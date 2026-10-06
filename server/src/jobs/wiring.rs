//! Jobs bundle: one registry plus the loops it owns.
//!
//! [`JobsSetup`] is the one `AppState` field jobs adds. It owns the single
//! [`JobRegistry`] every jobs loop registers on: the five always-on boot
//! loops, the events kick one-shot, precache runs, and the plugin tick
//! loops (via the plugins bundle, which shares this registry rather than
//! running its own). One registry, one rebuild choke point
//! (`sync_ticks`), no duplicate loop mechanics.
//!
//! Production binds [`DurableRegistryStore`] (liveness rows survive
//! restarts); test states bind [`MemoryRegistryStore`] behind the same
//! [`StoreKind`] seam so the setup type stays concrete. Loop backends are
//! real where they exist (checkpoint passes, the now-playing feed and its
//! upstream session pollers, the Navidrome playlist export, events
//! settings reads, the concerts sweep) and no-ops where their services do
//! not exist yet (the personal mixer); each interim adapter says what is
//! missing.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;

use crate::auth::users::{UsersDeps, roles::Role};
use crate::concerts::ConcertsSweep;
use crate::db::{CheckpointService, DurableWorkWakeups, WriteLane};
use crate::jobs::checkpoint::{self, CheckpointRunner};
use crate::jobs::events_kick::{self, EventsKick, FnKick, KickOutcome};
use crate::jobs::events_watcher::{self, PollTimeSource, SystemWatchClock};
use crate::jobs::media::{MediaJobs, RegistryFeed, RemoteSessionPollers};
use crate::jobs::personal_mix::{self, PersonalMixer};
use crate::jobs::playlist_export::M3uPlaylistExporter;
use crate::jobs::playlist_sync::{
    self, PlaylistSyncConfig, PlaylistSyncSettings, PlaylistSyncState, SyncRoles,
};
use crate::jobs::precache::{self, PrecacheLimits, PrecacheWork};
use crate::jobs::presence;
use crate::jobs::registry::{
    AlreadyRunning, BoxFuture, DurableRegistryStore, JobKind, JobRegistry, MemoryRegistryStore,
    RegistryStore, WakeupChannel,
};
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::{
    AdvancedSettings, EventsSettings, NavidromeConnection,
};

/// Grace per job at shutdown. Loops select on their stop signal next to
/// every sleep, so this only binds a cycle that is mid-flight.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Registry rows behind one concrete type: durable in production, memory
/// in test states. The enum keeps [`JobsSetup`] non-generic so `AppState`
/// holds one field type.
#[derive(Clone, Debug)]
pub enum StoreKind {
    /// In-memory rows for states without a database.
    Memory(MemoryRegistryStore),
    /// Durable rows over the writer lane.
    Durable(DurableRegistryStore),
}

impl RegistryStore for StoreKind {
    fn register_job(
        &self,
        name: &str,
        kind: JobKind,
        channel: Option<WakeupChannel>,
    ) -> BoxFuture<'_, ()> {
        match self {
            Self::Memory(store) => store.register_job(name, kind, channel),
            Self::Durable(store) => store.register_job(name, kind, channel),
        }
    }

    fn set_job_state(
        &self,
        name: &str,
        state: crate::jobs::registry::JobState,
    ) -> BoxFuture<'_, ()> {
        match self {
            Self::Memory(store) => store.set_job_state(name, state),
            Self::Durable(store) => store.set_job_state(name, state),
        }
    }

    fn heartbeat(&self, name: &str) -> BoxFuture<'_, ()> {
        match self {
            Self::Memory(store) => store.heartbeat(name),
            Self::Durable(store) => store.heartbeat(name),
        }
    }

    fn get_job(&self, name: &str) -> BoxFuture<'_, Option<crate::db::durable::JobRecord>> {
        match self {
            Self::Memory(store) => store.get_job(name),
            Self::Durable(store) => store.get_job(name),
        }
    }

    fn list_jobs(&self) -> BoxFuture<'_, Vec<crate::db::durable::JobRecord>> {
        match self {
            Self::Memory(store) => store.list_jobs(),
            Self::Durable(store) => store.list_jobs(),
        }
    }
}

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

/// Personal-mix refresh without a mixer: cycles succeed without rebuilding
/// until the all-users mixer lands behind this seam.
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

/// Route role lookups over the user store. Async-native: the gate awaits
/// the row read, so role changes land on the next call with no thread
/// bridging. Missing accounts and store faults fail closed.
#[derive(Clone)]
pub struct StoreSyncRoles {
    users: UsersDeps,
}

impl SyncRoles for StoreSyncRoles {
    fn role_of(&self, user_id: &str) -> BoxFuture<'_, Option<Role>> {
        let users = self.users.clone();
        let user_id = user_id.to_owned();
        Box::pin(async move {
            users
                .users
                .get_by_id(&user_id)
                .await
                .ok()
                .flatten()
                .map(|user| user.role)
        })
    }
}

/// Precache phases without an implementation: every run reports the reason
/// and lands failed, so the trigger reports the gap until the artist,
/// album, discovery, and AudioDB passes are wired here.
#[derive(Clone, Debug, Default)]
pub struct UnwiredPrecacheWork;

impl PrecacheWork for UnwiredPrecacheWork {
    fn run(&self, _progress: precache::Progress) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async {
            Err("Library precache phases are not wired yet; the run stays pending.".to_owned())
        })
    }
}

/// The production precache trigger: one supervised run per call through the
/// shared registry. Watchdog limits re-read the advanced settings on every
/// run; an unwired store or unreadable section falls back to the shipped
/// timeouts.
#[derive(Clone)]
pub struct PrecacheTrigger {
    registry: JobRegistry<StoreKind>,
    config: Option<Arc<ConfigStore>>,
}

impl PrecacheTrigger {
    /// Start one supervised run. Rejected while a run is live, like any
    /// duplicate job name.
    pub async fn run(&self) -> Result<precache::PrecacheHandle, AlreadyRunning> {
        precache::spawn_run(&self.registry, UnwiredPrecacheWork, self.limits()).await
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
}

impl JobsSetup {
    /// Bind the production backends: durable rows, live checkpoint passes,
    /// settings reads over the shared store, the route's admin gate over
    /// the user store, the media feeds (presence, playlist export), and the
    /// concerts sweep (`None` only on states without concerts).
    pub fn build(
        users: UsersDeps,
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
                roles: Arc::new(StoreSyncRoles { users }),
            },
            presence: (media.feed(), media.pollers()),
            poll_time: EventsPollTime {
                store: Some(Arc::clone(&config)),
            },
            watcher,
            config: Some(config),
        }
    }

    /// Test bundle: memory rows, skipped checkpoint passes, sync off, and the
    /// live admin gate over the caller's user store. Route tests exercise the
    /// playlist route through this; loop tests bind the memory stores
    /// directly.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(users: UsersDeps) -> Self {
        let media = MediaJobs::for_tests();
        Self {
            registry: JobRegistry::new(StoreKind::Memory(MemoryRegistryStore::new())),
            checkpoint: CheckpointInput { service: None },
            playlist: PlaylistSyncState {
                settings: NavidromeSyncSettings { store: None },
                exporter: media.exporter(),
                roles: Arc::new(StoreSyncRoles { users }),
            },
            presence: (media.feed(), media.pollers()),
            poll_time: EventsPollTime { store: None },
            watcher: None,
            config: None,
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
        }
    }

    /// Settings-nested routes jobs owns: `POST
    /// /settings/navidrome/playlist-sync` (the v2 path). Mounts inside the
    /// session gate with the other `/api/v3` routes.
    pub fn settings_router(&self) -> Router {
        Router::new().nest("/settings", playlist_sync::router(self.playlist.clone()))
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

    /// Spawn the always-on loops ([`crate::jobs::BOOT_JOBS`]) on the shared
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
            UnwiredMixer,
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
        Ok(())
    }

    /// Cancel every live job (boot loops, the kick, precache runs, and
    /// plugin ticks; `plugin-tick:*` shares this registry), each with the
    /// same grace. Shutdown calls this before awaiting the loops.
    pub async fn cancel_all(&self, grace: Duration) {
        self.registry.cancel_all(grace).await;
    }
}
