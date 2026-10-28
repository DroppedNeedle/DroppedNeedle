//! Acquire bundle: one setup the app mounts.
//!
//! [`AcquireSetup`] owns the requests state, the imports deps, the flows
//! stores and loop deps, the unified dispatch, the live client set, the
//! health probes, and the download worker. Every durable piece sits on the
//! shared SQLite runtime ([`AcquireDb`]). Clients, quotas and the download
//! policy read their sections through live handles, so saving settings
//! takes effect without a restart. `for_tests` binds the same shape over a
//! scratch database and scratch settings. Routes are declared in posture
//! groups through `routes()`; only the Spotify OAuth callback is public (it
//! is state-token identified).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::clients::{ClientSettings, LiveClients};
use crate::db::AcquireDb;
use crate::dispatch::DownloadDispatch;
use crate::downloads::journal::Journal;
use crate::downloads::store::StoreError;
use crate::downloads::worker::{
    DownloadWorker, SourceList, WorkerSettings, run_startup_recovery, sabnzbd_mount,
};
use crate::drop_import::DropImports;
use crate::drop_import::store::{DropStore, StoreError as DropStoreError};
use crate::flows::bundle::{FlowsBundle, flows_bundle};
use crate::flows::loops::{
    FOLLOW_JOB, PRUNE_JOB, PruneDeps, SYNC_JOB, UPGRADE_JOB, WANTED_JOB, spawn_follow_loop,
    spawn_prune_loop, spawn_sweep_loop, spawn_sync_loop, spawn_wanted_loop,
};
use crate::flows::seams::{ReleasePoll, TickSink};
use crate::held::{HeldFolders, HeldService};
use crate::imports::handlers::{ImportsDeps, imports_legacy_callback_router};
use crate::imports::jobs::{SpotifyImportJobs, SpotifyImports};
use crate::imports::lidarr::LidarrClient;
use crate::imports::spotify::SpotifyImportService;
use crate::imports::spotify_albums::AlbumResolver;
use crate::imports::spotify_client::SpotifyClient;
use crate::imports::spotify_store::{
    CollectionsPlaylistBridge, SqliteSpotifyLinks, SqliteSpotifyStates,
};
use crate::landing::LandingService;
use crate::landing::ports::LandingLibrary;
use crate::probes::{LiveProbes, ProbeCache};
use crate::readiness::{SourceNotices, SourceReadiness};
use crate::requests::error::RequestsError;
use crate::requests::quota::QuotaLedger;
use crate::requests::sqlite::WantedStore;
use crate::requests::state::{AcquireReadiness, RequestsState};
use crate::search_jobs::SearchJobs;
use crate::search_jobs::store::JobStore;
use crate::settings::{ConfigLidarrSettings, ConfigSpotifySettings};
use crate::target::Targets;
use crate::target::lookup::AlbumLookup;
use crate::usenet::indexer::{Indexer, PluginIndexers};
use crate::usenet::newznab::NewznabIndexer;
use crate::usenet::prowlarr::ProwlarrIndexer;
use axum::Router;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_catalog::reads::collections::store::Stores as CollectionsStores;
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_kernel::jobs::{AlreadyRunning, Supervisor};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::secret_sections::{
    DownloadClients, Indexers as ConfigIndexers, LidarrImportConnection, ProwlarrConnection,
    SlskdConnection, TypedLibrary,
};
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, UsenetBackend, UsenetBackendSetting,
};
use droppedneedle_kernel::runtime_config::{ConfigStore, Live, Section};
use droppedneedle_library::operations::port::EditionPlans;
use droppedneedle_library::publish::bin::DownloadTrees;
use droppedneedle_media::playback::listening::{ListenBrainzLinkStore, ListeningStore};
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_providers::Providers;
use tokio::sync::watch;

#[cfg(any(test, feature = "test-support"))]
mod test_bundle;

/// Spotify Web API root.
pub const SPOTIFY_API_BASE: &str = "https://api.spotify.com/v1";
/// Spotify accounts origin (token exchange).
pub const SPOTIFY_ACCOUNTS_BASE: &str = "https://accounts.spotify.com";

/// Where the Spotify import reaches Spotify.
#[derive(Debug, Clone)]
pub struct SpotifyEndpoints {
    /// Spotify Web API root.
    pub api: String,
    /// Spotify accounts origin (token exchange).
    pub accounts: String,
    /// A cover origin allowed beside the Spotify CDN; tests only.
    pub cover_cdn: Option<String>,
}

/// Every loop [`AcquireSetup::spawn_loops`] starts, by job name.
pub const ACQUIRE_LOOPS: &[&str] = &[
    WANTED_JOB,
    FOLLOW_JOB,
    UPGRADE_JOB,
    SYNC_JOB,
    PRUNE_JOB,
    crate::held::PRUNE_JOB,
    crate::downloads::worker::DOWNLOAD_WORKER_JOB,
    crate::probes::PROBE_JOB,
    crate::readiness::SOURCE_WATCH_JOB,
];

/// The sections the acquisition clients are built from.
pub struct ClientLives {
    slskd: Live<SlskdConnection>,
    sabnzbd: Live<DownloadClients>,
    prowlarr: Live<ProwlarrConnection>,
    lidarr: Live<LidarrImportConnection>,
    policy: Live<DownloadPolicy>,
    backend: Live<UsenetBackendSetting>,
    library: Live<TypedLibrary>,
    /// The indexers are not a listed section, so they have no live
    /// handle: they are read through the store's indexer read, and
    /// followed through their change notice.
    indexers: ConfigStore,
}

impl ClientLives {
    /// Handles on `store`'s sections.
    pub fn new(store: &ConfigStore) -> Self {
        Self {
            slskd: store.live(),
            sabnzbd: store.live(),
            prowlarr: store.live(),
            lidarr: store.live(),
            policy: store.live(),
            backend: store.live(),
            library: store.live(),
            indexers: store.clone(),
        }
    }

    /// One change notice per section read here.
    pub(crate) fn subscribe(&self) -> Vec<watch::Receiver<i64>> {
        vec![
            self.slskd.subscribe(),
            self.sabnzbd.subscribe(),
            self.prowlarr.subscribe(),
            self.lidarr.subscribe(),
            self.policy.subscribe(),
            self.backend.subscribe(),
            self.library.subscribe(),
            self.indexers.subscribe(ConfigIndexers::KEY),
        ]
    }

    /// The sections as saved right now.
    pub(crate) fn read(&self) -> ClientSettings {
        let indexers = self
            .indexers
            .get_indexers_raw()
            .map(|indexers| indexers.iter().map(|indexer| (**indexer).clone()).collect())
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "indexer settings read failed; treating as none");
                Vec::new()
            });
        // A client section that does not read is an unconfigured client.
        // The download policy is never its defaults: without it nothing
        // searches or downloads.
        ClientSettings {
            slskd: (**self.slskd.read().unwrap_or_default()).clone(),
            sabnzbd: (**self.sabnzbd.read().unwrap_or_default()).clone(),
            indexers,
            prowlarr: (**self.prowlarr.read().unwrap_or_default()).clone(),
            lidarr: (**self.lidarr.read().unwrap_or_default()).clone(),
            policy: self.policy.read().map(|policy| (**policy).clone()),
            backend: **self.backend.read().unwrap_or_default(),
            library_roots: self
                .library
                .read()
                .unwrap_or_default()
                .library_roots
                .iter()
                .map(|root| PathBuf::from(&root.path))
                .collect(),
        }
    }
}

/// Why startup recovery stopped.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    /// The database answered busy. The recovery can run again: see
    /// [`AcquireSetup::run_recovery`].
    #[error("request recovery found the database busy during {operation}")]
    Busy {
        /// Operation name. Never SQL text or a path.
        operation: String,
    },
    /// Any other failure.
    #[error("{0}")]
    Failed(String),
}

impl RecoveryError {
    /// True when waiting and running the recovery again can succeed.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }
}

/// Everything `create_app` needs to mount the acquisition routes, built once.
#[derive(Clone)]
pub struct AcquireSetup {
    /// Shared database handle.
    pub db: AcquireDb,
    /// Request intake state over the durable stores.
    pub requests: RequestsState,
    /// Imports deps (config-backed settings, bridged follows).
    pub imports: ImportsDeps,
    /// The one way into the download queue.
    pub dispatch: Arc<DownloadDispatch>,
    /// Shared download journal.
    pub journal: Arc<Journal>,
    /// Flows stores and loop deps.
    pub flows: Arc<FlowsBundle>,
    /// Steady-state download worker.
    pub worker: Arc<DownloadWorker>,
    /// Acquisition clients over the live settings.
    pub clients: Arc<LiveClients>,
    /// Live health probes.
    pub probes: Arc<LiveProbes>,
    /// Probe cache the refresh loop fills.
    pub probe_cache: Arc<ProbeCache>,
    /// Whether a download source is on: requests, search, the queue, the
    /// health smoke and Discover all read this one answer.
    pub readiness: SourceReadiness,
    /// What the readiness reads changes on: the source settings and, in
    /// production, the plugin host.
    source_notices: SourceNotices,
    /// Asks the probe loop for a pass now.
    probe_wake: Arc<watch::Sender<u64>>,
    /// Store-prune deps (request history and wanted rows).
    pub prune: Arc<PruneDeps>,
    /// Live event hub handle shared by the flows and the imports.
    pub events: EventHub,
    /// Finished-download import: verify, match, publish or hold.
    pub landing: Arc<LandingService>,
    /// Manual album searches over the worker's live sources.
    pub search_jobs: Arc<SearchJobs>,
    /// Drop import: uploaded music into the library.
    pub drops: Arc<DropImports>,
    /// The held list: every file waiting for a person's decision.
    pub held: Arc<HeldService>,
    /// Runs the Spotify import jobs; registered on the jobs table.
    pub spotify_import_jobs: Arc<SpotifyImportJobs>,
}

/// The drop-import service over the landing's library port, a staging
/// folder of its own, and the request stores it resolves.
fn drop_imports(
    db: &AcquireDb,
    landing: &LandingService,
    folders: &HeldFolders,
    events: &EventHub,
    flows: &FlowsBundle,
) -> Arc<DropImports> {
    Arc::new(DropImports::new(
        DropStore::new(db.clone()),
        landing.library().cloned(),
        folders.drop.clone(),
        events.clone(),
        flows.ticks.clone(),
        flows.ledger.clone(),
    ))
}

/// The acquisition stores, opened once over the platform's database. The
/// library's album cleanup is built on them before this bundle is, and the
/// bundle then takes them.
#[derive(Clone)]
pub struct AcquireStores {
    /// The database every acquisition store wraps.
    pub db: AcquireDb,
    /// The download journal.
    pub journal: Arc<Journal>,
    /// Wanted watches.
    pub wanted: WantedStore,
    /// Where flow outcomes go: plugin events, user notices and the flow
    /// log. The journal announces fulfilled requests on it.
    pub ticks: Arc<dyn TickSink>,
    /// The two folders held copies live in.
    pub folders: HeldFolders,
}

impl AcquireStores {
    /// The stores over the platform's database and clock, announcing flow
    /// outcomes on `ticks`.
    pub fn open(platform: &Platform, ticks: Arc<dyn TickSink>) -> Self {
        Self::over(
            AcquireDb::new(platform.db.clone(), platform.clock.clone()),
            ticks,
            HeldFolders {
                held: platform.config.cache_dir.join("held"),
                drop: platform.config.imports_dir().join("drop"),
            },
        )
    }

    fn over(db: AcquireDb, ticks: Arc<dyn TickSink>, folders: HeldFolders) -> Self {
        Self {
            journal: Arc::new(Journal::new(db.clone(), ticks.clone())),
            wanted: WantedStore::new(db.clone()),
            db,
            ticks,
            folders,
        }
    }
}

/// What acquisition reaches in other areas, handed in when it is built.
pub struct AcquirePorts {
    /// Finished downloads land in the library through this.
    pub library: Arc<dyn LandingLibrary>,
    /// "Get missing tracks" asks the library which tracks the chosen
    /// edition lacks.
    pub edition_plans: Arc<dyn EditionPlans>,
    /// MusicBrainz reads: edition tracklists, and the album a single track
    /// is on.
    pub album_lookup: Arc<dyn AlbumLookup>,
    /// The follow poll's MusicBrainz reader.
    pub release_poll: Arc<dyn ReleasePoll>,
    /// Plugin download sources, plugin indexers that feed usenet, and
    /// plugin events.
    pub plugins: Arc<PluginHost>,
    /// The collections stores: imported playlists and the Weekly Mix
    /// playlist are written to them.
    pub collections: CollectionsStores,
    /// The shared providers; the Weekly Mix reads ListenBrainz through
    /// them.
    pub providers: Arc<Providers>,
}

/// What the Weekly Mix builder is made from.
struct MixInputs {
    providers: Arc<Providers>,
    links: Arc<dyn ListenBrainzLinkStore>,
    prefs: ListeningStore,
    playlists: droppedneedle_catalog::reads::collections::store::playlists::PlaylistStore,
}

/// What the durable core reaches outside acquisition. The test bundle
/// leaves most of it out.
struct CorePorts {
    plugins: Option<Arc<PluginHost>>,
    edition_plans: Option<Arc<dyn EditionPlans>>,
    readiness: Option<AcquireReadiness>,
    release_poll: Arc<dyn ReleasePoll>,
    mix: Option<MixInputs>,
    /// MusicBrainz reads the wanted watcher dates its watches with.
    targets: Arc<Targets>,
}

/// The pieces shared by the production and test bundles.
struct Core {
    requests: RequestsState,
    dispatch: Arc<DownloadDispatch>,
    journal: Arc<Journal>,
    flows: Arc<FlowsBundle>,
}

/// Build the durable core: dispatch, requests state and the flows bundle,
/// all over the shared stores.
#[allow(clippy::too_many_arguments)]
fn core(
    stores: &AcquireStores,
    ids: Arc<dyn IdGenerator>,
    settings: &ConfigStore,
    quota: Arc<QuotaLedger>,
    sources: SourceList,
    ports: CorePorts,
    events: EventHub,
) -> Core {
    let db = &stores.db;
    let journal = stores.journal.clone();
    let dispatch = Arc::new(DownloadDispatch::new(journal.clone(), ids, settings.live()));
    // Album intake as the Weekly Mix builder files it: everything but the
    // builder itself, which intake never reaches.
    let intake = RequestsState {
        wanted: stores.wanted.clone(),
        plugins: ports.plugins.clone(),
        edition_plans: ports.edition_plans,
        // "Get missing tracks" also asks for the edition's tracks below
        // the upgrade cutoff, under the policy the upgrade sweep reads.
        upgrade_policy: Some(settings.live()),
        readiness: ports.readiness,
        ..RequestsState::new(db, quota, dispatch.clone())
    };
    let mixer = ports.mix.map(|mix| {
        crate::requests::mix_sources::live_builder(
            &intake,
            &mix.providers,
            mix.links,
            mix.prefs,
            mix.playlists,
            events.clone(),
        )
    });
    let requests = RequestsState { mixer, ..intake };
    let ticks = stores.ticks.clone();
    let flows = Arc::new(flows_bundle(
        db,
        &requests,
        sources,
        dispatch.clone(),
        settings,
        ticks,
        ports.release_poll,
        ports.targets,
    ));
    Core {
        requests,
        dispatch,
        journal,
        flows,
    }
}

/// The indexers Usenet searches: the native list or Prowlarr, as the
/// backend setting chooses, then the plugin indexers that feed Usenet.
/// The one place the backend is matched.
pub(crate) fn usenet_indexers(
    backend: UsenetBackendSetting,
    newznab: &Arc<NewznabIndexer>,
    prowlarr: &Arc<ProwlarrIndexer>,
    plugins: Option<&Arc<PluginHost>>,
) -> Vec<Arc<dyn Indexer>> {
    let primary: Arc<dyn Indexer> = match backend.0 {
        UsenetBackend::Indexers => newznab.clone(),
        UsenetBackend::Prowlarr => prowlarr.clone(),
    };
    let mut indexers = vec![primary];
    if let Some(host) = plugins {
        indexers.push(Arc::new(PluginIndexers::new(host.clone())));
    }
    indexers
}

/// The download folders no recycle bin may overlap: the staging folder
/// and SABnzbd's completed-downloads folder. An empty folder setting names
/// no folder, so it guards nothing.
fn download_trees(staging_root: &Path, sab_mount: Option<PathBuf>) -> Vec<PathBuf> {
    std::iter::once(staging_root.to_owned())
        .chain(sab_mount)
        .filter(|tree| !tree.as_os_str().is_empty())
        .collect()
}

/// The download folders a configured recycle bin must stay clear of, read
/// live from the SABnzbd settings, for the library's builder. `None` while
/// those settings cannot be read.
pub fn recycle_guard(store: &ConfigStore, staging_root: PathBuf) -> DownloadTrees {
    let sabnzbd: Live<DownloadClients> = store.live();
    Arc::new(move || {
        let section = sabnzbd.read()?;
        Some(download_trees(&staging_root, sabnzbd_mount(&section)))
    })
}

impl AcquireSetup {
    /// Build the production bundle over the platform's database and the
    /// stores already opened on it. Credentials and tuning are read from
    /// its settings on use (decrypted in memory only); notices and import
    /// completions go to its event hub. `ports` are the other areas it
    /// reaches. Spotify imports are queued on `queue`.
    pub fn build(
        platform: &Platform,
        users: UsersDeps,
        stores: AcquireStores,
        ports: AcquirePorts,
        spotify: &SpotifyEndpoints,
        queue: JobQueue,
    ) -> Result<Self, String> {
        let db = stores.db.clone();
        let (config, ids, config_store) = (
            &platform.config,
            platform.ids.clone(),
            platform.settings.clone(),
        );
        let http = &platform.http;
        let (http, no_redirect) = (http.shared().clone(), http.no_redirect().clone());
        let staging_root = config.imports_dir();
        let quota = Arc::new(QuotaLedger::new(config_store.live(), db.clone()));
        let clients = Arc::new(LiveClients::new(
            http.clone(),
            ClientLives::new(&config_store),
            config.slskd_downloads_path.clone(),
            stores.journal.clone(),
            Arc::new(Targets::new(
                stores.journal.clone(),
                Some(ports.album_lookup),
            )),
            Some(ports.plugins.clone()),
        ));
        let events = platform.events.clone();
        let search_quota = quota.clone();
        let worker_settings = Arc::new(WorkerSettings::new(&config_store, clients.clone()));
        let probe_cache = Arc::new(ProbeCache::new(clients.clone()));
        let readiness = SourceReadiness::new(
            SourceList::Live(worker_settings.clone()),
            probe_cache.clone(),
        );
        let source_notices = SourceNotices {
            settings: worker_settings.clone(),
            plugins: Some(ports.plugins.clone()),
        };
        let collections = ports.collections;
        let health = ports.providers.health.clone();
        let resolver = Arc::new(AlbumResolver::new(&ports.providers, config_store.live()));
        let core = core(
            &stores,
            ids.clone(),
            &config_store,
            quota,
            SourceList::Live(worker_settings.clone()),
            CorePorts {
                plugins: Some(ports.plugins.clone()),
                edition_plans: Some(ports.edition_plans),
                readiness: Some(AcquireReadiness {
                    sources: readiness.clone(),
                    staging_root: staging_root.clone(),
                }),
                release_poll: ports.release_poll,
                mix: Some({
                    let listening = ListeningStore::new(
                        platform.db.clone(),
                        platform.crypto.clone(),
                        platform.clock.clone(),
                    );
                    MixInputs {
                        providers: ports.providers,
                        links: Arc::new(listening.clone()),
                        prefs: listening,
                        playlists: collections.playlists.clone(),
                    }
                }),
                targets: clients.targets().clone(),
            },
            events.clone(),
        );
        let prune = Arc::new(PruneDeps {
            settings: config_store.live(),
            ledger: core.requests.store.clone(),
            watches: core.requests.wanted.clone(),
            search_jobs: JobStore::new(db.clone()),
        });

        // Imports deps.
        let lidarr_settings = Arc::new(ConfigLidarrSettings::new(config_store.clone()));
        let spotify_settings = Arc::new(ConfigSpotifySettings::new(config_store.clone()));
        let spotify_states = Arc::new(SqliteSpotifyStates::new(db.clone()));
        let spotify_links = Arc::new(SqliteSpotifyLinks::new(db.clone(), users.crypto.clone()));
        let spotify_playlists = Arc::new(CollectionsPlaylistBridge::new(
            collections.playlists.clone(),
            db.clone(),
        ));
        let spotify_client = SpotifyClient::new(
            http.clone(),
            no_redirect,
            &spotify.api,
            &spotify.accounts,
            spotify.cover_cdn.as_deref(),
        );
        let spotify_service = Arc::new(SpotifyImportService::new(
            spotify_client.clone(),
            spotify_settings.clone(),
            spotify_links.clone(),
            spotify_playlists.clone(),
            resolver.clone(),
        ));
        let spotify_import_jobs = Arc::new(SpotifyImportJobs::new(spotify_service, events.clone()));
        let spotify_imports = SpotifyImports::new(platform.db.clone(), queue);
        let probes = Arc::new(LiveProbes::new(probe_cache.clone()));
        let imports = ImportsDeps {
            lidarr: LidarrClient::new(http.clone()),
            lidarr_settings,
            follows: core.requests.follows.clone(),
            spotify: spotify_client,
            spotify_settings,
            spotify_states,
            spotify_links,
            spotify_playlists,
            resolver,
            spotify_imports,
            probes: Arc::new(LiveProbes::bundle(&probes)),
            readiness: readiness.clone(),
            ids: ids.clone(),
            base_path: config.base_path.clone(),
            trusted_proxies: config.trusted_proxies.clone(),
        };

        // Finished downloads reach the library through its import seam.
        let landing = Arc::new(LandingService::new(
            core.journal.clone(),
            Some(ports.library),
            config_store.live(),
            stores.folders.held.clone(),
        ));

        // Worker over the live sources and tuning.
        let worker = Arc::new(
            DownloadWorker::new(
                core.journal.clone(),
                worker_settings,
                readiness.clone(),
                events.clone(),
                health,
            )
            .with_plugin_events(ports.plugins)
            .with_targets(clients.targets().clone())
            .with_landing(landing.clone()),
        );
        let search_jobs = Arc::new(SearchJobs::new(
            JobStore::new(db.clone()),
            worker.clone(),
            clients.targets().clone(),
            search_quota,
            events.clone(),
        ));
        let drops = drop_imports(&db, &landing, &stores.folders, &events, &core.flows);
        let held = Arc::new(HeldService::new(
            core.journal.clone(),
            worker.clone(),
            drops.clone(),
            stores.folders.clone(),
        ));

        Ok(Self {
            db,
            requests: core.requests,
            imports,
            dispatch: core.dispatch,
            journal: core.journal,
            flows: core.flows,
            worker,
            clients,
            probes,
            probe_cache,
            readiness,
            source_notices,
            probe_wake: Arc::new(watch::Sender::new(0)),
            prune,
            events,
            landing,
            search_jobs,
            drops,
            held,
            spotify_import_jobs,
        })
    }

    /// The state the queue and upgrade routes read
    /// ([`crate::downloads::routes`]).
    pub fn downloads_routes(&self) -> crate::downloads::DownloadsRoutes {
        crate::downloads::DownloadsRoutes::new(
            self.journal.clone(),
            self.worker.clone(),
            crate::downloads::upgrades::Upgrades::new(
                self.flows.worklist.clone(),
                self.flows.sweep_deps.policy.clone(),
                self.dispatch.clone(),
            ),
        )
    }

    /// The Spotify OAuth callback at its v2 path, at full path: mount it at
    /// the root, outside the session gate, like the OIDC legacy callback.
    pub fn legacy_callback_router(&self) -> Router {
        imports_legacy_callback_router(self.imports.clone())
    }

    /// Run startup recovery before serving traffic: drop imports a restart
    /// cut off, the download journal classification, and the request
    /// ledger (interrupted cancels and dispatches). It may run again after
    /// a run that stopped on a busy database: the drop sweep leaves an
    /// upload it could not read in place, a restarted download keeps its
    /// keys, and a request's cancel or dispatch reuses its key or relinks.
    pub async fn run_recovery(
        &self,
    ) -> Result<crate::downloads::worker::RecoveryReport, RecoveryError> {
        self.drops
            .sweep_stale()
            .await
            .map_err(|error| match error {
                DropStoreError::Busy { operation } => RecoveryError::Busy { operation },
                other => RecoveryError::Failed(other.to_string()),
            })?;
        self.held.recover().await;
        let report = run_startup_recovery(&self.journal)
            .await
            .map_err(|error| match error {
                StoreError::Db(DbError::Busy { operation }) => RecoveryError::Busy { operation },
                other => RecoveryError::Failed(other.to_string()),
            })?;
        let requests = crate::requests::service::RequestsService::new(&self.requests)
            .recover()
            .await
            .map_err(|error| match error {
                RequestsError::Busy { operation } => RecoveryError::Busy { operation },
                other => RecoveryError::Failed(format!("request recovery failed: {other:?}")),
            })?;
        if requests.cancelled > 0 || requests.relinked > 0 || requests.redispatched > 0 {
            tracing::info!(
                cancelled = requests.cancelled,
                relinked = requests.relinked,
                redispatched = requests.redispatched,
                "requests recovered"
            );
        }
        Ok(report)
    }

    /// Start the flows loops, the download worker and the probe refresh on
    /// the loop supervisor, which stops them at shutdown.
    pub fn spawn_loops(&self, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
        spawn_wanted_loop(supervisor, self.flows.wanted_deps.clone())?;
        spawn_follow_loop(supervisor, self.flows.follow_deps.clone())?;
        spawn_sweep_loop(supervisor, self.flows.sweep_deps.clone())?;
        spawn_sync_loop(supervisor, self.flows.sync_deps.clone())?;
        spawn_prune_loop(supervisor, self.prune.clone())?;
        crate::downloads::worker::spawn_download_worker(self.worker.clone(), supervisor)?;
        crate::held::spawn_held_prune(supervisor, self.held.clone())?;
        crate::probes::spawn_probe_loop(
            supervisor,
            self.probe_cache.clone(),
            self.clients.clone(),
            &self.probe_wake,
        )?;
        crate::readiness::spawn_source_watch(
            supervisor,
            &self.source_notices,
            self.worker.clone(),
            self.probe_wake.clone(),
        )
    }
}
