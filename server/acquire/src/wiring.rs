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
//! is state-token identified, like v2's ungated route).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use futures_util::future::BoxFuture;

use super::db::AcquireDb;
use super::dispatch::DownloadDispatch;
use super::downloads::journal::Journal;
use super::downloads::source_tag::SourceTag;
use super::downloads::sources::DownloadSource;
use super::downloads::watchdog::{RetryPolicy, WatchdogConfig};
use super::drop_import::DropImports;
use super::drop_import::store::DropStore;
use super::flows::loops::{
    FOLLOW_JOB, FollowDeps, PRUNE_JOB, PruneDeps, SYNC_JOB, SweepDeps, SyncDeps, UPGRADE_JOB,
    WANTED_JOB, WantedDeps, spawn_follow_loop, spawn_prune_loop, spawn_sweep_loop, spawn_sync_loop,
    spawn_wanted_loop,
};
use super::flows::seams::ReleasePoll;
use super::flows::seams::TickSink;
use super::flows::stores::{AdminDirectory, LibraryPresence, UpgradeWorklist};
use super::follows::Follows;
use super::imports::handlers::{ImportsDeps, imports_legacy_callback_router};
use super::imports::jobs::{SpotifyImportJobs, SpotifyImports};
use super::imports::lidarr::LidarrClient;
use super::imports::spotify::SpotifyImportService;
use super::imports::spotify_albums::AlbumResolver;
use super::imports::spotify_client::SpotifyClient;
use super::imports::spotify_store::{
    CollectionsPlaylistBridge, SqliteSpotifyLinks, SqliteSpotifyStates,
};
use super::landing::LandingService;
use super::landing::ports::LandingLibrary;
use super::probes::{LiveProbes, ProbeCache, ProbeInputs, refresh_probes};
use super::readiness::SourceReadiness;
use super::requests::error::RequestsError;
use super::requests::quota::QuotaLedger;
use super::requests::sqlite::{RequestStore, WantedStore};
use super::requests::state::{AcquireReadiness, RequestsState};
use super::search_jobs::SearchJobs;
use super::search_jobs::store::JobStore;
use super::settings::{
    ConfigLidarrSettings, ConfigSpotifySettings, clamp_u32, clamp_u64, clamp_usize,
};
use super::slskd::{DownloadPolicy as SlskdPolicy, ReqwestSlskdHttp, SlskdClient, SlskdRepository};
use super::sources::{SabnzbdSource, SlskdSource};
use super::target::Targets;
use super::target::lookup::AlbumLookup;
use super::usenet::indexer::{Indexer, PluginIndexers};
use super::usenet::newznab::{NewznabClient, NewznabIndexer, NewznabIndexerEntry};
use super::usenet::policy::{QualityTier, UsenetPolicy};
use super::usenet::prowlarr::{ProwlarrClient, ProwlarrIndexer};
use super::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
use super::worker::{DownloadWorker, SourceList, SourceSet, WorkerConfig, run_startup_recovery};
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_catalog::reads::collections::store::Stores as CollectionsStores;
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor, TickCtx};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::secret_sections::{
    DownloadClients, Indexers as ConfigIndexers, LidarrImportConnection,
    NewznabIndexer as ConfigIndexer, ProwlarrConnection, SlskdConnection, TypedLibrary,
};
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, MusicBrainzSettings, SourcePriority, UsenetBackend, UsenetBackendSetting,
};
use droppedneedle_kernel::runtime_config::{ConfigStore, Live, Section};
use droppedneedle_library::operations::port::EditionPlans;
use droppedneedle_library::publish::bin::DownloadTrees;
use droppedneedle_media::playback::listening::{ListenBrainzLinkStore, ListeningStore};
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_providers::Providers;
use tokio::sync::watch;

/// Spotify Web API root (v2 `spotify_client.py`).
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

/// Health-probe refresh cadence.
const PROBE_INTERVAL: Duration = Duration::from_secs(300);

/// Job name of the probe refresh loop.
pub const PROBE_JOB: &str = "acquire-probe-refresh";

/// Job name of the loop that follows the source settings and plugins.
pub const SOURCE_WATCH_JOB: &str = "acquire-source-watch";

/// Every loop [`AcquireSetup::spawn_loops`] starts, by job name.
pub const ACQUIRE_LOOPS: &[&str] = &[
    WANTED_JOB,
    FOLLOW_JOB,
    UPGRADE_JOB,
    SYNC_JOB,
    PRUNE_JOB,
    super::worker::DOWNLOAD_WORKER_JOB,
    PROBE_JOB,
    SOURCE_WATCH_JOB,
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
    fn subscribe(&self) -> Vec<watch::Receiver<i64>> {
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
    fn read(&self) -> ClientSettings {
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

/// The settings one client set is built from.
struct ClientSettings {
    slskd: SlskdConnection,
    sabnzbd: DownloadClients,
    indexers: Vec<ConfigIndexer>,
    prowlarr: ProwlarrConnection,
    lidarr: LidarrImportConnection,
    /// `None` when the download policy does not read.
    policy: Option<DownloadPolicy>,
    backend: UsenetBackendSetting,
    /// Library root folders (the mount probe compares filesystems).
    library_roots: Vec<PathBuf>,
}

/// One built set of clients for the current settings.
pub struct ClientSet {
    /// Download sources for the worker.
    pub sources: SourceSet,
    /// Health probe inputs.
    pub probe_inputs: Arc<ProbeInputs>,
}

/// A built client set and the change notices of the sections it was
/// built from.
struct Built {
    notices: Vec<watch::Receiver<i64>>,
    set: Arc<ClientSet>,
}

/// Acquisition clients over the live settings. The set is rebuilt only
/// after a section it was built from changes, so per-client caches survive
/// between uses while a saved setting takes effect on the next use.
pub struct LiveClients {
    http: reqwest::Client,
    settings: ClientLives,
    slskd_downloads: PathBuf,
    journal: Arc<Journal>,
    built: Mutex<Option<Built>>,
    /// Plugin sources and usenet-targeting plugin indexers join the
    /// pipeline through the host. `None` on the test bundle.
    plugins: Option<Arc<PluginHost>>,
    targets: Arc<Targets>,
}

impl LiveClients {
    /// Clients over the live sections. `slskd_downloads` is the slskd
    /// downloads mount (`SLSKD_DOWNLOADS_PATH`).
    pub fn new(
        http: reqwest::Client,
        settings: ClientLives,
        slskd_downloads: PathBuf,
        journal: Arc<Journal>,
        targets: Arc<Targets>,
        plugins: Option<Arc<PluginHost>>,
    ) -> Self {
        Self {
            http,
            settings,
            slskd_downloads,
            journal,
            built: Mutex::new(None),
            plugins,
            targets,
        }
    }

    /// What each task fetches, shared by every source.
    pub fn targets(&self) -> &Arc<Targets> {
        &self.targets
    }

    /// The built-in sources for the current settings plus one source per
    /// enabled plugin download client.
    pub fn sources_with_plugins(&self, policy: &DownloadPolicy) -> super::worker::SourceSet {
        let built = self.current().sources.clone();
        let Some(host) = &self.plugins else {
            return built;
        };
        let keys = host.download_source_keys();
        if keys.is_empty() {
            return built;
        }
        let release_policy = super::plugin_source::ReleasePolicy {
            usenet: usenet_policy_from(policy),
            ignored_terms: policy.ignored_terms.clone(),
            required_terms: policy.required_terms.clone(),
        };
        let mut sources: Vec<Arc<dyn DownloadSource>> = built.iter().cloned().collect();
        for key in keys {
            sources.push(Arc::new(
                super::plugin_source::PluginDownloadSource::new(
                    Arc::clone(host),
                    key,
                    self.journal.clone(),
                    release_policy.clone(),
                )
                .with_targets(self.targets.clone()),
            ));
        }
        Arc::new(sources)
    }

    /// The client set for the current settings.
    pub fn current(&self) -> Arc<ClientSet> {
        let mut built = match self.built.lock() {
            Ok(built) => built,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(built) = built.as_ref()
            && !built
                .notices
                .iter()
                .any(|notice| notice.has_changed().unwrap_or(false))
        {
            return built.set.clone();
        }
        // Subscribed before the read, so a change that lands during the
        // build is seen on the next use.
        let notices = self.settings.subscribe();
        let set = Arc::new(self.build(&self.settings.read()));
        *built = Some(Built {
            notices,
            set: set.clone(),
        });
        set
    }

    fn build(&self, settings: &ClientSettings) -> ClientSet {
        let http = &self.http;
        // The probes still see the saved clients while the download policy
        // does not read; nothing searches or downloads.
        let readable = settings.policy.is_some();
        let usenet_policy = usenet_policy_from(&settings.policy.clone().unwrap_or_default());
        let slskd_repo = slskd_repository(http, &settings.slskd, &self.slskd_downloads);
        let sab_queue = sabnzbd_queue(http, &settings.sabnzbd, &usenet_policy);
        let newznab = Arc::new(newznab_indexer(http, &settings.indexers));
        let prowlarr = Arc::new(prowlarr_indexer(http, &settings.prowlarr));
        let mut sources: Vec<Arc<dyn DownloadSource>> = Vec::new();
        if let Some(repo) = slskd_repo.clone()
            && readable
            && settings.slskd.enabled
        {
            sources.push(Arc::new(
                SlskdSource::new(repo, self.journal.clone()).with_targets(self.targets.clone()),
            ));
        }
        if let Some(queue) = sab_queue.clone()
            && readable
            && settings.sabnzbd.sabnzbd.enabled
        {
            sources.push(Arc::new(
                SabnzbdSource::new(
                    queue,
                    usenet_indexers(settings.backend, &newznab, &prowlarr, self.plugins.as_ref()),
                    usenet_policy,
                    self.journal.clone(),
                    Some(settings.sabnzbd.sabnzbd.category.clone()),
                )
                .with_targets(self.targets.clone()),
            ));
        }
        let probe_inputs = Arc::new(ProbeInputs {
            slskd: slskd_repo,
            slskd_enabled: settings.slskd.enabled,
            slskd_mount_base: self.slskd_downloads.clone(),
            slskd_subpath: settings.slskd.downloads_subpath.clone(),
            library_roots: settings.library_roots.clone(),
            sabnzbd: sab_queue,
            sabnzbd_section: settings.sabnzbd.clone(),
            newznab,
            newznab_entries: settings.indexers.clone(),
            prowlarr,
            prowlarr_section: settings.prowlarr.clone(),
            lidarr: LidarrClient::new(http.clone()),
            lidarr_section: settings.lidarr.clone(),
        });
        ClientSet {
            sources: Arc::new(sources),
            probe_inputs,
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
    /// Runs the Spotify import jobs; registered on the jobs table.
    pub spotify_import_jobs: Arc<SpotifyImportJobs>,
}

/// Flows stores plus the loop deps built over them.
#[derive(Clone)]
pub struct FlowsBundle {
    /// Wanted watches (shared with the wanted view).
    pub watches: WantedStore,
    /// Request ledger (shared with intake).
    pub ledger: RequestStore,
    /// Follow-poll cursors.
    pub follows: Follows,
    /// Upgrade worklist, read from the catalog.
    pub worklist: UpgradeWorklist,
    /// Library presence, read from the catalog.
    pub library: Arc<LibraryPresence>,
    /// Sweep ownership directory, read from the accounts.
    pub admins: AdminDirectory,
    /// Plugin ticks.
    pub ticks: Arc<dyn TickSink>,
    /// Wanted-watcher deps.
    pub wanted_deps: Arc<WantedDeps>,
    /// Follow-poll deps.
    pub follow_deps: Arc<FollowDeps>,
    /// Upgrade-sweep deps.
    pub sweep_deps: Arc<SweepDeps>,
    /// Status-sweep deps.
    pub sync_deps: Arc<SyncDeps>,
}

/// The drop-import service over the landing's library port, a staging
/// folder of its own, and the request stores it resolves.
fn drop_imports(
    db: &AcquireDb,
    landing: &LandingService,
    staging_root: &Path,
    events: &EventHub,
    flows: &FlowsBundle,
) -> Arc<DropImports> {
    Arc::new(DropImports::new(
        DropStore::new(db.clone()),
        landing.library().cloned(),
        staging_root.join("drop"),
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
}

impl AcquireStores {
    /// The stores over the platform's database and clock, announcing flow
    /// outcomes on `ticks`.
    pub fn open(platform: &Platform, ticks: Arc<dyn TickSink>) -> Self {
        Self::over(
            AcquireDb::new(platform.db.clone(), platform.clock.clone()),
            ticks,
        )
    }

    fn over(db: AcquireDb, ticks: Arc<dyn TickSink>) -> Self {
        Self {
            journal: Arc::new(Journal::new(db.clone(), ticks.clone())),
            wanted: WantedStore::new(db.clone()),
            db,
            ticks,
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
        super::requests::mix_sources::live_builder(
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
    ));
    Core {
        requests,
        dispatch,
        journal,
        flows,
    }
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
            config.cache_dir.join("held"),
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
        let drops = drop_imports(&db, &landing, &staging_root, &events, &core.flows);

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
            spotify_import_jobs,
        })
    }

    /// Test bundle over `db` (a scratch database), default settings and
    /// the import stores over it. Finished downloads land in `library` when
    /// there is one. Call inside a tokio runtime.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(
        db: AcquireDb,
        ids: Arc<dyn IdGenerator>,
        events: EventHub,
        library: Option<Arc<dyn LandingLibrary>>,
    ) -> Result<Self, String> {
        use droppedneedle_catalog::reads::collections::db::CollectionsDb;
        use droppedneedle_catalog::reads::collections::store::PlaylistStore;
        use droppedneedle_kernel::runtime_config::crypto::Crypto;

        let stores = AcquireStores::over(
            db.clone(),
            Arc::new(super::plugin_events::FlowTicks::new(None, events.clone())),
        );
        // Under the scratch database's directory, so it goes with it.
        let staging_root = db
            .path()
            .parent()
            .map(|dir| dir.join("staging"))
            .ok_or_else(|| "scratch database has no directory".to_owned())?;
        let settings = super::settings::scratch_settings(&staging_root.join("settings"))?;
        let factory = droppedneedle_kernel::http_client::HttpClientFactory::new()
            .map_err(|error| error.to_string())?;
        let http = factory.shared().clone();
        let clients = Arc::new(LiveClients::new(
            http.clone(),
            ClientLives::new(&settings),
            staging_root.clone(),
            stores.journal.clone(),
            Arc::new(Targets::new(stores.journal.clone(), None)),
            None,
        ));
        let worker_settings = Arc::new(WorkerSettings::new(&settings, clients.clone()));
        let core = core(
            &stores,
            ids.clone(),
            &settings,
            Arc::new(QuotaLedger::unlimited(db.clone())),
            SourceList::Live(worker_settings.clone()),
            CorePorts {
                plugins: None,
                edition_plans: None,
                readiness: None,
                release_poll: Arc::new(super::search::NoReleasePoll),
                mix: None,
            },
            events.clone(),
        );
        let spotify_client = SpotifyClient::new(
            http.clone(),
            factory.no_redirect().clone(),
            SPOTIFY_API_BASE,
            SPOTIFY_ACCOUNTS_BASE,
            None,
        );
        let spotify_settings = Arc::new(ConfigSpotifySettings::new(Arc::new(settings.clone())));
        let crypto =
            Arc::new(Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| error.to_string())?);
        let spotify_links = Arc::new(SqliteSpotifyLinks::new(db.clone(), crypto));
        let spotify_playlists = Arc::new(CollectionsPlaylistBridge::new(
            PlaylistStore::new(CollectionsDb::new(db.db().clone(), db.clock().clone())),
            db.clone(),
        ));
        // No outside address is open here: every album stays unresolved.
        let providers = droppedneedle_providers::Providers::unpaced(Arc::new(
            droppedneedle_providers::executor::ScriptedWire::new(),
        ));
        let resolver = Arc::new(AlbumResolver::new(&providers, settings.live()));
        let service = Arc::new(SpotifyImportService::new(
            spotify_client.clone(),
            spotify_settings.clone(),
            spotify_links.clone(),
            spotify_playlists.clone(),
            resolver.clone(),
        ));
        let spotify_import_jobs = Arc::new(SpotifyImportJobs::new(service, events.clone()));
        let probe_cache = Arc::new(ProbeCache::new(clients.clone()));
        let probes = Arc::new(LiveProbes::new(probe_cache.clone()));
        // The worker here runs no sources, so neither does its readiness.
        let readiness = SourceReadiness::unprobed(SourceList::Fixed(Arc::new(Vec::new())));
        // No runner takes these here: the tests on this bundle import no
        // playlists.
        let spotify_imports = SpotifyImports::new(
            db.db().clone(),
            JobQueue::new(db.db().clone(), db.clock().clone()),
        );
        let imports = ImportsDeps {
            lidarr: LidarrClient::new(http),
            lidarr_settings: Arc::new(ConfigLidarrSettings::new(Arc::new(settings.clone()))),
            follows: core.requests.follows.clone(),
            spotify: spotify_client,
            spotify_settings,
            spotify_states: Arc::new(SqliteSpotifyStates::new(db.clone())),
            spotify_links,
            spotify_playlists,
            resolver,
            spotify_imports,
            probes: Arc::new(LiveProbes::bundle(&probes)),
            readiness: readiness.clone(),
            ids: ids.clone(),
            base_path: String::new(),
            trusted_proxies: Default::default(),
        };
        let held_dir = db
            .path()
            .parent()
            .map(|dir| dir.join("held"))
            .ok_or_else(|| "scratch database has no directory".to_owned())?;
        let landing = Arc::new(LandingService::new(
            core.journal.clone(),
            library,
            settings.live(),
            held_dir,
        ));
        let worker = Arc::new(
            DownloadWorker::fixed(
                core.journal.clone(),
                Vec::new(),
                WorkerConfig::default(),
                events.clone(),
                Default::default(),
            )
            .with_landing(landing.clone()),
        );
        let search_jobs = Arc::new(SearchJobs::new(
            JobStore::new(db.clone()),
            worker.clone(),
            clients.targets().clone(),
            Arc::new(QuotaLedger::unlimited(db.clone())),
            events.clone(),
        ));
        let drops = drop_imports(&db, &landing, &staging_root, &events, &core.flows);
        let prune = Arc::new(PruneDeps {
            settings: settings.live(),
            ledger: core.requests.store.clone(),
            watches: core.requests.wanted.clone(),
            search_jobs: JobStore::new(db.clone()),
        });
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
            source_notices: SourceNotices {
                settings: worker_settings,
                plugins: None,
            },
            probe_wake: Arc::new(watch::Sender::new(0)),
            prune,
            events,
            landing,
            search_jobs,
            drops,
            spotify_import_jobs,
        })
    }

    /// The state the queue and held-import routes read
    /// ([`super::downloads::routes`]).
    pub fn downloads_routes(&self) -> super::downloads::DownloadsRoutes {
        super::downloads::DownloadsRoutes::new(
            self.journal.clone(),
            self.worker.clone(),
            super::downloads::upgrades::Upgrades::new(
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
    pub async fn run_recovery(&self) -> Result<super::worker::RecoveryReport, RecoveryError> {
        self.drops.sweep_stale().await;
        let report = run_startup_recovery(&self.journal)
            .await
            .map_err(RecoveryError::Failed)?;
        let requests = super::requests::service::RequestsService::new(&self.requests)
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
        super::worker::spawn_download_worker(self.worker.clone(), supervisor)?;
        self.spawn_probe_loop(supervisor)?;
        self.spawn_source_watch(supervisor)
    }

    /// Probe refresh: one pass now, then one on every wake and at least
    /// every five minutes, each over the clients the current settings
    /// build. A pass waiting on a slow client ends at the stop; it only
    /// reads, and the cache keeps the last pass.
    fn spawn_probe_loop(&self, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
        let cache = self.probe_cache.clone();
        let clients = self.clients.clone();
        let mut wake = self.probe_wake.subscribe();
        wake.mark_changed();
        supervisor.on_wakeup(
            PROBE_JOB,
            wake,
            Schedule::new(PROBE_INTERVAL),
            FnTick(move |ctx: TickCtx| {
                let cache = cache.clone();
                let inputs = clients.current().probe_inputs.clone();
                async move {
                    tokio::select! {
                        () = ctx.token().cancelled() => {}
                        () = refresh_probes(&cache, &inputs) => {}
                    }
                    Ok(Next::Continue)
                }
            }),
        )
    }

    /// Follow everything the readiness reads. On a change, wake the
    /// download worker, so a request held for want of a source starts at
    /// once, and refresh the probes, so the status routes show the saved
    /// client.
    fn spawn_source_watch(&self, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
        let mut settings = self.source_notices.settings.notices();
        let mut plugins = self
            .source_notices
            .plugins
            .as_ref()
            .map(|host| host.subscribe());
        let worker = self.worker.clone();
        let probe_wake = self.probe_wake.clone();
        supervisor.one_shot(SOURCE_WATCH_JOB, move |ctx| async move {
            loop {
                tokio::select! {
                    () = ctx.token().cancelled() => return Ok(()),
                    open = any_changed(&mut settings, plugins.as_mut()) => {
                        if !open {
                            tracing::warn!("a source settings notice closed; source changes are no longer followed");
                            ctx.token().cancelled().await;
                            return Ok(());
                        }
                    }
                }
                worker.wake();
                probe_wake.send_modify(|count| *count = count.wrapping_add(1));
            }
        })
    }
}

/// What the source readiness reads, followed for changes.
#[derive(Clone)]
struct SourceNotices {
    settings: Arc<WorkerSettings>,
    plugins: Option<Arc<PluginHost>>,
}

/// Wait for any notice to change. False when one closed.
async fn any_changed(
    settings: &mut [watch::Receiver<i64>],
    plugins: Option<&mut watch::Receiver<u64>>,
) -> bool {
    let mut waits: Vec<BoxFuture<'_, bool>> = settings
        .iter_mut()
        .map(|notice| {
            Box::pin(async move { notice.changed().await.is_ok() }) as BoxFuture<'_, bool>
        })
        .collect();
    if let Some(notice) = plugins {
        waits.push(Box::pin(async move { notice.changed().await.is_ok() }));
    }
    if waits.is_empty() {
        return std::future::pending().await;
    }
    futures_util::future::select_all(waits).await.0
}

/// Flows bundle over the shared durable stores and live settings.
#[allow(clippy::too_many_arguments)]
fn flows_bundle(
    db: &AcquireDb,
    requests: &RequestsState,
    sources: SourceList,
    dispatch: Arc<DownloadDispatch>,
    settings: &ConfigStore,
    ticks: Arc<dyn TickSink>,
    release_poll: Arc<dyn ReleasePoll>,
) -> FlowsBundle {
    let watches = requests.wanted.clone();
    let ledger = requests.store.clone();
    let follows = requests.follows.clone();
    let worklist = UpgradeWorklist::new(db.clone());
    let library = Arc::new(LibraryPresence::over_catalog(db.pool().clone()));
    let admins = AdminDirectory::over_users(db.clone());
    let wanted_deps = Arc::new(WantedDeps {
        settings: settings.live(),
        watches: watches.clone(),
        ledger: ledger.clone(),
        sources,
        downloads: dispatch.clone(),
        library: library.clone(),
        ticks: ticks.clone(),
    });
    let follow_deps = Arc::new(FollowDeps {
        follows: follows.clone(),
        poll: release_poll,
        downloads: dispatch.clone(),
        requests: Arc::new(super::requests::service::RequestsService::new(requests)),
        library: library.clone(),
        ticks: ticks.clone(),
        preferences: settings.live(),
        today: {
            let clock = db.clock().clone();
            Arc::new(move || {
                // Days since epoch to a UTC calendar date.
                utc_ymd(super::db::epoch_secs(clock.as_ref()) / 86_400)
            })
        },
    });
    let sweep_deps = Arc::new(SweepDeps {
        policy: settings.live(),
        worklist: Arc::new(worklist.clone()),
        admins: admins.clone(),
        downloads: dispatch.clone(),
        ticks: ticks.clone(),
    });
    let sync_deps = Arc::new(SyncDeps {
        ledger: ledger.clone(),
        journal: dispatch.journal().clone(),
        library: library.clone(),
        ticks: ticks.clone(),
    });
    FlowsBundle {
        watches,
        ledger,
        follows,
        worklist,
        library,
        admins,
        ticks,
        wanted_deps,
        follow_deps,
        sweep_deps,
        sync_deps,
    }
}

/// Days since epoch to `YYYY-MM-DD` (Howard Hinnant's civil algorithm).
fn utc_ymd(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    if month <= 2 {
        year += 1;
    }
    format!("{year:04}-{month:02}-{day:02}")
}

/// slskd repository, when the section configures one. The mount is
/// `SLSKD_DOWNLOADS_PATH` plus the confined subpath from settings.
fn slskd_repository(
    http: &reqwest::Client,
    section: &SlskdConnection,
    downloads: &Path,
) -> Option<Arc<SlskdRepository<ReqwestSlskdHttp>>> {
    if section.url.is_empty() || section.api_key.expose().is_empty() {
        return None;
    }
    let transport = ReqwestSlskdHttp::new(http.clone(), &section.url, section.api_key.expose());
    let client = SlskdClient::new(transport);
    let mount = crate::landing::DownloadsFolder::with_subpath(
        downloads.to_path_buf(),
        &section.downloads_subpath,
    );
    let mut repo = SlskdRepository::new(
        client,
        &section.url,
        section.api_key.expose(),
        mount,
        SlskdPolicy::default(),
    );
    if !section.slskd_incomplete_mount.trim().is_empty() {
        repo = repo.with_incomplete_mount(PathBuf::from(section.slskd_incomplete_mount.trim()));
    }
    Some(Arc::new(repo))
}

/// SABnzbd queue, when the section configures one.
fn sabnzbd_queue(
    http: &reqwest::Client,
    section: &DownloadClients,
    policy: &UsenetPolicy,
) -> Option<Arc<SabnzbdQueue>> {
    let sab = &section.sabnzbd;
    if sab.url.is_empty() || sab.api_key.expose().is_empty() {
        return None;
    }
    let client = SabnzbdClient::new(
        http.clone(),
        &sab.url,
        sab.api_key.expose(),
        3,
        Duration::from_secs(1),
    );
    Some(Arc::new(SabnzbdQueue::new(
        client,
        &sab.url,
        sab.api_key.expose(),
        PathBuf::from(sab.downloads_mount.clone()),
        policy.clone(),
    )))
}

/// The indexers Usenet searches: the native list or Prowlarr, as the
/// backend setting chooses, then the plugin indexers that feed Usenet.
/// The one place the backend is matched.
fn usenet_indexers(
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

/// Native indexer fan-out over the configured entries.
fn newznab_indexer(http: &reqwest::Client, entries: &[ConfigIndexer]) -> NewznabIndexer {
    let entries = entries
        .iter()
        .map(|entry| NewznabIndexerEntry {
            client: NewznabClient::new(
                http.clone(),
                &entry.url,
                entry.api_key.expose(),
                &entry.id,
                &entry.name,
            ),
            id: entry.id.clone(),
            name: entry.name.clone(),
            categories: entry.categories.iter().map(|cat| *cat as i32).collect(),
            enabled: entry.enabled,
            priority: entry.priority.max(0) as u32,
            limit: 100,
        })
        .collect();
    NewznabIndexer::new(
        entries,
        Duration::from_secs(300),
        Duration::from_secs(60),
        Duration::from_secs(300),
        Duration::from_secs(30),
    )
}

/// Prowlarr fan-out over the single connection.
fn prowlarr_indexer(http: &reqwest::Client, section: &ProwlarrConnection) -> ProwlarrIndexer {
    let client = if section.url.is_empty() || section.api_key.expose().is_empty() {
        None
    } else {
        Some(ProwlarrClient::new(
            http.clone(),
            &section.url,
            section.api_key.expose(),
            "prowlarr",
        ))
    };
    ProwlarrIndexer::new(
        client,
        vec![3000, 3010, 3040],
        section.enabled,
        Duration::from_secs(300),
        Duration::from_secs(300),
        Duration::from_secs(30),
    )
}

/// Usenet policy bound from the download-policy section.
fn usenet_policy_from(policy: &DownloadPolicy) -> UsenetPolicy {
    let mut out = UsenetPolicy::v2_defaults();
    if let Some(tier) = QualityTier::parse(&policy.quality_min) {
        out.quality_min = tier;
    }
    if let Some(tier) = QualityTier::parse(&policy.quality_max) {
        out.quality_max = tier;
    }
    if let Some(tier) = QualityTier::parse(&policy.quality_cutoff) {
        out.quality_cutoff = tier;
    }
    out.flac_mp3_only = policy.flac_mp3_only;
    out.retention_days = clamp_u32(policy.usenet_retention_days);
    out.max_size_mb = clamp_u64(policy.max_size_mb);
    out.stall_timeout =
        Duration::from_secs(clamp_u64(policy.download_stall_timeout_minutes).max(1) * 60);
    out.queued_timeout =
        Duration::from_secs(clamp_u64(policy.download_queued_timeout_minutes).max(1) * 60);
    out
}

/// The sections the download worker reads, once per pass.
pub struct WorkerSettings {
    clients: Arc<LiveClients>,
    policy: Live<DownloadPolicy>,
    source_priority: Live<SourcePriority>,
    sabnzbd: Live<DownloadClients>,
    library: Live<TypedLibrary>,
    musicbrainz: Live<MusicBrainzSettings>,
}

impl WorkerSettings {
    /// Handles on `store`'s sections, with the sources `clients` build.
    pub fn new(store: &ConfigStore, clients: Arc<LiveClients>) -> Self {
        Self {
            clients,
            policy: store.live(),
            source_priority: store.live(),
            sabnzbd: store.live(),
            library: store.live(),
            musicbrainz: store.live(),
        }
    }

    /// The sources turned on right now, plugin download clients included.
    /// None while the download settings do not read.
    pub(crate) fn sources(&self) -> SourceSet {
        if !self.readable() {
            return SourceSet::default();
        }
        self.policy
            .read()
            .map(|policy| self.clients.sources_with_plugins(&policy))
            .unwrap_or_default()
    }

    /// One change notice per section the sources and tuning are read
    /// from.
    fn notices(&self) -> Vec<watch::Receiver<i64>> {
        let mut notices = self.clients.settings.subscribe();
        notices.push(self.source_priority.subscribe());
        notices.push(self.musicbrainz.subscribe());
        notices
    }

    /// Whether every section the worker cannot stand in for reads. While
    /// one does not, no pass runs: a default quality band would land the
    /// wrong files, and an unreadable MusicBrainz source would fail every
    /// landing's match.
    pub(crate) fn readable(&self) -> bool {
        self.policy.read().is_some()
            && self.source_priority.read().is_some()
            && self.library.read().is_some()
            && self.musicbrainz.read().is_some()
    }

    /// The worker tuning as saved right now, or `None` while a section it
    /// needs does not read. An unreadable SABnzbd section is an
    /// unconfigured one, as in the client set, so slskd keeps working.
    pub(crate) fn config(&self) -> Option<WorkerConfig> {
        if !self.readable() {
            return None;
        }
        let policy = self.policy.read()?;
        let source_priority = self.source_priority.read()?;
        let sab = self.sabnzbd.read().unwrap_or_default();
        let sab_mount = sabnzbd_mount(&sab);
        Some(worker_config(&policy, &source_priority, sab_mount))
    }
}

/// SABnzbd's completed-downloads folder, while SABnzbd is set up.
fn sabnzbd_mount(section: &DownloadClients) -> Option<PathBuf> {
    (!section.sabnzbd.url.is_empty()).then(|| PathBuf::from(&section.sabnzbd.downloads_mount))
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

/// Worker tuning bound from the download-policy section. Only the
/// SABnzbd complete dir is walked: slskd folders cannot prove client
/// inactivity from the folder name, so they always keep.
fn worker_config(
    policy: &DownloadPolicy,
    source_priority: &SourcePriority,
    sab_mount: Option<PathBuf>,
) -> WorkerConfig {
    let mut source_order: Vec<SourceTag> = source_priority
        .0
        .iter()
        .filter_map(|source| source.parse().ok())
        .collect();
    if source_order.is_empty() {
        source_order = vec![SourceTag::Soulseek, SourceTag::Usenet];
    }
    let orphan_roots = sab_mount
        .map(|mount| vec![(SourceTag::Usenet, mount)])
        .unwrap_or_default();
    WorkerConfig {
        max_concurrent_downloads: clamp_usize(policy.max_concurrent_downloads).max(1),
        max_failover_attempts: policy.max_failover_attempts.max(0),
        retry: RetryPolicy::from_policy(policy),
        watchdog: WatchdogConfig {
            poll_interval_seconds: 2.0,
            stall_timeout_seconds: (clamp_u64(policy.download_stall_timeout_minutes).max(1) * 60)
                as f64,
            queued_timeout_seconds: (clamp_u64(policy.download_queued_timeout_minutes).max(1) * 60)
                as f64,
            deadline_seconds: 6.0 * 3600.0,
            materialize_seconds: 90.0,
            reap_threshold_seconds: 1800.0,
        },
        source_order,
        orphan_roots,
        worker_id: "download-worker".to_owned(),
        review: crate::search_jobs::review::ReviewMode::from_setting(&policy.search_review),
    }
}
