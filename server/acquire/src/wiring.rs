//! Acquire bundle: one setup the app mounts.
//!
//! [`AcquireSetup`] owns the requests state, the imports deps, the flows
//! stores and loop deps, the unified dispatch, the live client set, the
//! health probes, and the download worker. Every durable piece sits on the
//! shared SQLite runtime ([`AcquireDb`]). Clients, quotas and the download
//! policy read their sections through live handles, so saving settings
//! takes effect without a restart. Routes are declared in posture groups
//! through `routes()`; only the Spotify OAuth callback is public (it
//! is state-token identified).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::clients::{ClientSettings, LiveClients};
use crate::db::AcquireDb;
use crate::deemix::session::DeemixSession;
use crate::deemix::source::{MountNeighbours, SourceMemory};
use crate::dispatch::DownloadDispatch;
use crate::downloads::journal::Journal;
use crate::downloads::store::StoreError;
use crate::downloads::worker::{
    DownloadWorker, WorkerPorts, WorkerSettings, run_startup_recovery, usenet_mounts,
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
use crate::import_folder::ImportFolders;
use crate::import_folder::site::ImportSite;
use crate::imports::handlers::ImportsDeps;
use crate::imports::jobs::{SpotifyImportJobs, SpotifyImports};
use crate::imports::lidarr::LidarrClient;
use crate::imports::playlist_identify::PlaylistIdentifyJobs;
use crate::imports::spotify::SpotifyImportService;
use crate::imports::spotify_albums::AlbumResolver;
use crate::imports::spotify_client::SpotifyClient;
use crate::imports::spotify_store::{
    CollectionsPlaylistBridge, SqliteSpotifyLinks, SqliteSpotifyStates,
};
use crate::landing::LandingService;
use crate::landing::ports::LandingLibrary;
use crate::playlist_requests::PlaylistRequests;
use crate::probes::{DeemixChecks, DeemixFolders, LiveProbes, ProbeCache};
use crate::readiness::{SourceNotices, SourceReadiness};
use crate::requests::error::RequestsError;
use crate::requests::mix::PersonalMixBuilder;
use crate::requests::quota::QuotaLedger;
use crate::requests::sqlite::WantedStore;
use crate::requests::state::{AcquireReadiness, MixRoutes, RequestsPorts, RequestsState};
use crate::search_jobs::SearchJobs;
use crate::search_jobs::store::JobStore;
use crate::settings::{ConfigLidarrSettings, ConfigSpotifySettings};
use crate::spotify_sync::SpotifySync;
use crate::spotify_sync::job::SpotifySyncJobs;
use crate::spotify_sync::store::SyncStore;
use crate::target::Targets;
use crate::target::lookup::AlbumLookup;
use crate::usenet::indexer::{Indexer, PluginIndexers};
use crate::usenet::newznab::NewznabIndexer;
use crate::usenet::prowlarr::ProwlarrIndexer;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_catalog::collections::store::Stores as CollectionsStores;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_kernel::jobs::{AlreadyRunning, Supervisor};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::secret_sections::{
    DownloadClients, Indexers as ConfigIndexers, ProwlarrConnection, SlskdConnection, TypedLibrary,
};
use droppedneedle_kernel::runtime_config::sections::{
    DeemixConnection, DownloadPolicy, ImportFolder, LibraryManagement, UsenetBackend,
    UsenetBackendSetting,
};
use droppedneedle_kernel::runtime_config::{ConfigStore, Live, Section};
use droppedneedle_library::edition::port::EditionPlans;
use droppedneedle_library::publish::bin::DownloadTrees;
use droppedneedle_library::removal::Mutations;
use droppedneedle_media::playback::listening::ListeningStore;
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_providers::Providers;
use droppedneedle_providers::preview::PreviewClient;
use tokio::sync::watch;

/// Every loop [`AcquireSetup::spawn_loops`] starts, by job name.
pub const ACQUIRE_LOOPS: &[&str] = &[
    WANTED_JOB,
    FOLLOW_JOB,
    UPGRADE_JOB,
    SYNC_JOB,
    PRUNE_JOB,
    crate::held::PRUNE_JOB,
    crate::import_folder::IMPORT_FOLDER_JOB,
    crate::downloads::worker::DOWNLOAD_WORKER_JOB,
    crate::probes::PROBE_JOB,
    crate::readiness::SOURCE_WATCH_JOB,
    crate::spotify_sync::due::DUE_JOB,
];

/// The sections the acquisition clients are built from.
pub struct ClientLives {
    slskd: Live<SlskdConnection>,
    sabnzbd: Live<DownloadClients>,
    prowlarr: Live<ProwlarrConnection>,
    policy: Live<DownloadPolicy>,
    backend: Live<UsenetBackendSetting>,
    library: Live<TypedLibrary>,
    deemix: Live<DeemixConnection>,
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
            policy: store.live(),
            backend: store.live(),
            library: store.live(),
            deemix: store.live(),
            indexers: store.clone(),
        }
    }

    /// One change notice per section read here.
    pub(crate) fn subscribe(&self) -> Vec<watch::Receiver<i64>> {
        vec![
            self.slskd.subscribe(),
            self.sabnzbd.subscribe(),
            self.prowlarr.subscribe(),
            self.policy.subscribe(),
            self.backend.subscribe(),
            self.library.subscribe(),
            self.deemix.subscribe(),
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
            deemix: (**self.deemix.read().unwrap_or_default()).clone(),
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
    /// The personal-mix builder: the Weekly Mix routes, the listening
    /// settings' hooks and the daily sweep drive it.
    pub mixer: Arc<PersonalMixBuilder>,
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
    /// Whether a download source is on: requests, search, the queue and
    /// Discover all read this one answer.
    pub readiness: SourceReadiness,
    /// What the readiness reads changes on: the source settings and the
    /// plugin host.
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
    /// The import folder: music copied there goes in by itself.
    pub import_folder: Arc<ImportFolders>,
    /// Runs the Spotify import jobs; registered on the jobs table.
    pub spotify_import_jobs: Arc<SpotifyImportJobs>,
    /// Finds the albums of the songs a playlist file import kept;
    /// registered on the jobs table.
    pub playlist_identify_jobs: Arc<PlaylistIdentifyJobs>,
    /// Deemix's Test and status checks, on the session the Deezer source
    /// keeps.
    pub deemix: Arc<DeemixChecks>,
    /// A playlist's Request missing, its song requests and what its
    /// missing songs wait for.
    pub playlist_requests: Arc<PlaylistRequests>,
    /// Spotify sync: its switch, its state and its passes.
    pub spotify_sync: Arc<SpotifySync>,
    /// Runs the sync passes; registered on the jobs table.
    pub spotify_sync_jobs: Arc<SpotifySyncJobs>,
}

/// The drop-import service over the landing's library port, a staging
/// folder of its own, and the request stores it resolves.
fn drop_imports(
    db: &AcquireDb,
    landing: &LandingService,
    folders: &HeldFolders,
    events: &EventHub,
    flows: &FlowsBundle,
    site: Arc<ImportSite>,
) -> Arc<DropImports> {
    Arc::new(DropImports::new(
        DropStore::new(db.clone()),
        landing.library().clone(),
        folders.drop.clone(),
        events.clone(),
        flows.ticks.clone(),
        flows.ledger.clone(),
        site,
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
    /// The album pages: a download no person picked an edition for
    /// prefers the edition its album page shows.
    pub catalog: droppedneedle_catalog::metadata::Catalog,
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
    /// The library's album removal, for an admin removing a discovery
    /// batch with its albums.
    pub albums: Mutations,
    /// The app's listening store: the Weekly Mix reads ListenBrainz links
    /// and the auto-request preference through it.
    pub listening: ListeningStore,
}

/// What the durable core reaches outside acquisition.
struct CorePorts {
    requests: RequestsPorts,
    release_poll: Arc<dyn ReleasePoll>,
    /// MusicBrainz reads the wanted watcher dates its watches with.
    targets: Arc<Targets>,
    /// What the Weekly Mix builder reads: ListenBrainz through the shared
    /// providers, the listening store and the playlists it writes.
    providers: Arc<Providers>,
    listening: ListeningStore,
    playlists: droppedneedle_catalog::collections::store::playlists::PlaylistStore,
}

/// The durable core.
struct Core {
    requests: RequestsState,
    mixer: Arc<PersonalMixBuilder>,
    dispatch: Arc<DownloadDispatch>,
    journal: Arc<Journal>,
    flows: Arc<FlowsBundle>,
}

/// Build the durable core: dispatch, requests state, the mix builder and
/// the flows bundle, all over the shared stores.
fn core(
    stores: &AcquireStores,
    ids: Arc<dyn IdGenerator>,
    settings: &ConfigStore,
    quota: Arc<QuotaLedger>,
    sources: Arc<WorkerSettings>,
    ports: CorePorts,
) -> Core {
    let db = &stores.db;
    let journal = stores.journal.clone();
    let dispatch = Arc::new(DownloadDispatch::new(journal.clone(), ids, settings.live()));
    let events = ports.requests.events.clone();
    let requests = RequestsState::new(db, quota, dispatch.clone(), ports.requests);
    let mixer = crate::requests::mix_sources::live_builder(
        &requests,
        &ports.providers,
        ports.listening,
        ports.playlists,
        events,
        settings.live(),
    );
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
        mixer,
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
    plugins: &Arc<PluginHost>,
) -> Vec<Arc<dyn Indexer>> {
    let primary: Arc<dyn Indexer> = match backend.0 {
        UsenetBackend::Indexers => newznab.clone(),
        UsenetBackend::Prowlarr => prowlarr.clone(),
    };
    vec![primary, Arc::new(PluginIndexers::new(plugins.clone()))]
}

/// The download folders no recycle bin may overlap: the staging folder,
/// slskd's downloads and incomplete folders while slskd is set up,
/// each Usenet client's completed-downloads folder, each enabled download
/// plugin's folders, and Deemix's downloads folder while Deemix is set up.
/// An empty folder setting names no folder, so it guards nothing.
fn download_trees(
    staging_root: &Path,
    slskd: Option<(PathBuf, PathBuf)>,
    usenet_mounts: Vec<PathBuf>,
    plugins: Vec<PathBuf>,
    deemix: Option<PathBuf>,
) -> Vec<PathBuf> {
    std::iter::once(staging_root.to_owned())
        .chain(
            slskd
                .into_iter()
                .flat_map(|(downloads, incomplete)| [downloads, incomplete]),
        )
        .chain(usenet_mounts)
        .chain(plugins)
        .chain(deemix)
        .filter(|tree| !tree.as_os_str().is_empty())
        .collect()
}

/// The folders each enabled download plugin's landings read and remove
/// from, as its download source names them: its data folder, and the
/// folder its `downloads_dir` setting names when its manifest declares
/// one. Read from the plugins folder and the stored plugin settings, not
/// the plugin host, so the library's guard has them before plugins load.
/// `None` while an enabled download plugin's settings cannot be read.
/// Blocking.
fn plugin_trees(store: &ConfigStore, plugins_dir: &Path) -> Option<Vec<PathBuf>> {
    use std::collections::HashSet;

    use droppedneedle_kernel::runtime_config::crypto::CIPHER_PREFIX;
    use droppedneedle_plugins::manifest::{Capability, load_manifest};

    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return Some(Vec::new());
    };
    let mut trees = Vec::new();
    for folder in entries.flatten().map(|entry| entry.path()) {
        // The host skips hidden folders, its data folder among them.
        let shown = folder
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.starts_with(['.', '_']));
        if !shown || !folder.is_dir() {
            continue;
        }
        let Ok(manifest) = load_manifest(&folder) else {
            continue;
        };
        if !manifest.declares(Capability::DownloadClient) {
            continue;
        }
        let name = &manifest.name;
        // Read as stored: only the switch and the folder are used, and no
        // value is opened unless the folder itself was sealed.
        let stored = match store.get_plugin(name) {
            Ok(stored) => stored,
            Err(error) => {
                tracing::warn!(plugin = %name, %error, "plugin settings unreadable; its download folders are not known");
                return None;
            }
        };
        if !stored.enabled {
            continue;
        }
        trees.push(
            plugins_dir
                .join(droppedneedle_plugins::host::DATA_FOLDER)
                .join(name),
        );
        if !manifest
            .settings
            .iter()
            .any(|field| field.key == "downloads_dir")
        {
            continue;
        }
        let Some(stored_dir) = stored.settings.get("downloads_dir") else {
            continue;
        };
        let dir = if stored_dir.starts_with(CIPHER_PREFIX) {
            // A v2 import seals every value of a plugin it had no manifest
            // for. Only then is the folder opened, through the plain read,
            // which never fails on a value that will not open.
            match store.get_plugin_raw(name, &HashSet::new()) {
                Ok(opened) => opened
                    .into_settings()
                    .remove("downloads_dir")
                    .unwrap_or_default(),
                Err(error) => {
                    tracing::warn!(plugin = %name, %error, "plugin settings unreadable; its download folders are not known");
                    return None;
                }
            }
        } else {
            stored_dir.clone()
        };
        if !dir.trim().is_empty() {
            trees.push(PathBuf::from(dir.trim()));
        }
    }
    Some(trees)
}

/// slskd's downloads mount (`slskd_downloads`, the whole of it, whatever
/// subfolder slskd saves to) and incomplete folder, while slskd is set up.
fn slskd_trees(section: &SlskdConnection, slskd_downloads: &Path) -> Option<(PathBuf, PathBuf)> {
    (!section.url.is_empty()).then(|| {
        (
            slskd_downloads.to_owned(),
            PathBuf::from(section.slskd_incomplete_mount.trim()),
        )
    })
}

/// Deemix's downloads folder, while Deemix is set up.
fn deemix_tree(section: &DeemixConnection) -> Option<PathBuf> {
    (!section.url.is_empty()).then(|| PathBuf::from(section.downloads_mount.trim()))
}

/// The settings the download folders come from, read on each use.
#[derive(Clone)]
struct DownloadFolders {
    slskd: Live<SlskdConnection>,
    sabnzbd: Live<DownloadClients>,
    deemix: Live<DeemixConnection>,
    /// Plugin settings are not a listed section, so they have no live
    /// handle: they are read through the store's plugin read.
    plugins: ConfigStore,
    staging_root: PathBuf,
    slskd_downloads: PathBuf,
    plugins_dir: PathBuf,
}

impl DownloadFolders {
    fn new(
        store: &ConfigStore,
        staging_root: PathBuf,
        slskd_downloads: PathBuf,
        plugins_dir: PathBuf,
    ) -> Self {
        Self {
            slskd: store.live(),
            sabnzbd: store.live(),
            deemix: store.live(),
            plugins: store.clone(),
            staging_root,
            slskd_downloads,
            plugins_dir,
        }
    }

    /// The download folders, Deemix's own left out unless `with_deemix`
    /// (a folder of another client at the same path still counts). `None`
    /// while a setting they come from cannot be read. Blocking.
    fn read(&self, with_deemix: bool) -> Option<Vec<PathBuf>> {
        let slskd = self.slskd.read()?;
        let usenet = self.sabnzbd.read()?;
        let deemix = if with_deemix {
            deemix_tree(&**self.deemix.read()?)
        } else {
            None
        };
        Some(download_trees(
            &self.staging_root,
            slskd_trees(&slskd, &self.slskd_downloads),
            usenet_mounts(&usenet),
            plugin_trees(&self.plugins, &self.plugins_dir)?,
            deemix,
        ))
    }
}

/// The download folders a configured recycle bin (and the import folder)
/// must stay clear of, read live from the slskd, SABnzbd, Deemix and plugin
/// settings and the plugins in `plugins_dir`, for the library's builder.
/// `None` while those settings cannot be read.
pub fn recycle_guard(
    store: &ConfigStore,
    staging_root: PathBuf,
    slskd_downloads: PathBuf,
    plugins_dir: PathBuf,
) -> DownloadTrees {
    let folders = DownloadFolders::new(store, staging_root, slskd_downloads, plugins_dir);
    Arc::new(move || folders.read(true))
}

/// Where the app keeps its own files, and what the download folders are
/// read beside.
pub(crate) struct AppFolders {
    /// The staging folder downloads are unpacked in.
    pub(crate) staging_root: PathBuf,
    /// slskd's downloads mount (`SLSKD_DOWNLOADS_PATH`).
    pub(crate) slskd_downloads: PathBuf,
    /// The plugins folder.
    pub(crate) plugins_dir: PathBuf,
    /// The cache, config and database folders.
    pub(crate) app: Vec<PathBuf>,
}

impl AppFolders {
    /// The platform's folders.
    fn of(platform: &Platform) -> Self {
        let config = &platform.config;
        Self {
            staging_root: config.imports_dir(),
            slskd_downloads: config.slskd_downloads_path.clone(),
            plugins_dir: config.plugins_dir(),
            app: app_dirs(config),
        }
    }
}

/// The cache, config and database folders.
fn app_dirs(config: &droppedneedle_kernel::config::AppConfig) -> Vec<PathBuf> {
    [
        Some(config.cache_dir.clone()),
        Some(config.config_dir.clone()),
        config.library_db_path.parent().map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The folders Deemix's downloads folder stays clear of, by kind: the
/// library folders, the recycle bin, the import folder, the other download
/// folders and the app's own.
#[derive(Clone)]
struct DeemixNeighbours {
    library: Live<TypedLibrary>,
    management: Live<LibraryManagement>,
    import_folder: Live<ImportFolder>,
    downloads: DownloadFolders,
    app: Vec<PathBuf>,
}

impl DeemixNeighbours {
    /// The sets, read now; the kind reads after "overlaps a". `None` while
    /// a setting they come from cannot be read. Blocking.
    fn sets(&self) -> Option<Vec<(&'static str, Vec<PathBuf>)>> {
        let one = |path: &str| -> Vec<PathBuf> {
            Some(path.trim())
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .into_iter()
                .collect()
        };
        let library = self
            .library
            .read()?
            .library_roots
            .iter()
            .map(|root| PathBuf::from(&root.path))
            .collect();
        Some(vec![
            ("library folder", library),
            (
                "recycle bin",
                one(&self.management.read()?.recycle_bin_path),
            ),
            (
                "folder set for imports",
                one(&self.import_folder.read()?.path),
            ),
            ("download folder", self.downloads.read(false)?),
            ("folder DroppedNeedle keeps its data in", self.app.clone()),
        ])
    }
}

/// The one session DroppedNeedle keeps with Deemix, the settings card's
/// checks on it, and what each Deezer source the clients build is given.
/// Built once, beside the clients, so a rebuild after any settings save
/// keeps the session, its sign-in limits and what the source remembers. A
/// save of the Deemix settings drops the session only when it changes
/// Deemix's address; a session is never sent to another address.
pub struct DeemixWiring {
    pub(crate) session: Arc<DeemixSession>,
    pub(crate) checks: Arc<DeemixChecks>,
    pub(crate) previews: PreviewClient,
    pub(crate) policy: Live<DownloadPolicy>,
    pub(crate) neighbours: MountNeighbours,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) memory: Arc<SourceMemory>,
}

impl DeemixWiring {
    /// Over the platform's settings, folders, clock and the factory's
    /// client that follows no redirect, with Deezer's barcode lookups
    /// through `previews`.
    pub fn build(platform: &Platform, previews: PreviewClient) -> Self {
        Self::over(
            &platform.settings,
            platform.http.no_redirect().clone(),
            platform.clock.clone(),
            previews,
            AppFolders::of(platform),
        )
    }

    /// Over `store`, `no_redirect` (the factory's client that follows no
    /// redirect), `clock` and the app's `folders`.
    pub(crate) fn over(
        store: &ConfigStore,
        no_redirect: reqwest::Client,
        clock: Arc<dyn Clock>,
        previews: PreviewClient,
        folders: AppFolders,
    ) -> Self {
        let neighbours = DeemixNeighbours {
            library: store.live(),
            management: store.live(),
            import_folder: store.live(),
            downloads: DownloadFolders::new(
                store,
                folders.staging_root,
                folders.slskd_downloads,
                folders.plugins_dir,
            ),
            app: folders.app,
        };
        let flat = neighbours.clone();
        let checked: DeemixFolders = Arc::new(move || {
            Some(
                flat.sets()?
                    .into_iter()
                    .flat_map(|(_, dirs)| dirs)
                    .collect(),
            )
        });
        let session = Arc::new(DeemixSession::new());
        Self {
            checks: Arc::new(DeemixChecks::new(
                no_redirect,
                session.clone(),
                clock.clone(),
                store.live(),
                checked,
                store.live(),
            )),
            session,
            previews,
            policy: store.live(),
            neighbours: Arc::new(move || neighbours.sets()),
            clock,
            memory: Arc::default(),
        }
    }

    /// The settings card's Test and the status probe, on the kept session.
    pub fn checks(&self) -> &Arc<DeemixChecks> {
        &self.checks
    }
}

impl AcquireSetup {
    /// Build the production bundle over the platform's database and the
    /// stores already opened on it. Credentials and tuning are read from
    /// its settings on use (decrypted in memory only); notices and import
    /// completions go to its event hub. `ports` are the other areas it
    /// reaches; Spotify through the shared providers. Spotify imports are
    /// queued on `queue`.
    pub fn build(
        platform: &Platform,
        users: UsersDeps,
        stores: AcquireStores,
        ports: AcquirePorts,
        queue: JobQueue,
    ) -> Result<Self, String> {
        let db = stores.db.clone();
        let (config, ids, config_store) = (
            &platform.config,
            platform.ids.clone(),
            platform.settings.clone(),
        );
        let http = platform.http.shared().clone();
        let staging_root = config.imports_dir();
        let quota = Arc::new(QuotaLedger::new(config_store.live(), db.clone()));
        let deemix = DeemixWiring::build(platform, ports.providers.previews.clone());
        let deemix_checks = deemix.checks().clone();
        let clients = Arc::new(LiveClients::new(
            http.clone(),
            platform.http.no_redirect().clone(),
            platform.http.same_host_redirects().clone(),
            ports.providers.newznab.clone(),
            ClientLives::new(&config_store),
            config.slskd_downloads_path.clone(),
            stores.journal.clone(),
            Arc::new(Targets::new(
                stores.journal.clone(),
                ports.album_lookup,
                ports.catalog.clone(),
            )),
            ports.plugins.clone(),
            deemix,
        ));
        let events = platform.events.clone();
        let search_quota = quota.clone();
        let worker_settings = Arc::new(WorkerSettings::new(&config_store, clients.clone()));
        let probe_cache = Arc::new(ProbeCache::new(clients.clone()));
        let readiness = SourceReadiness::new(worker_settings.clone());
        let source_notices = SourceNotices {
            settings: worker_settings.clone(),
            plugins: ports.plugins.clone(),
        };
        let collections = ports.collections;
        let health = ports.providers.health.clone();
        let resolver = Arc::new(AlbumResolver::new(&ports.providers, config_store.live()));
        let spotify_client = SpotifyClient::new(&ports.providers);
        let core = core(
            &stores,
            ids.clone(),
            &config_store,
            quota,
            worker_settings.clone(),
            CorePorts {
                requests: RequestsPorts {
                    plugins: ports.plugins.clone(),
                    edition_plans: ports.edition_plans,
                    upgrade_policy: config_store.live(),
                    readiness: AcquireReadiness {
                        sources: readiness.clone(),
                        staging_root: staging_root.clone(),
                    },
                    albums: ports.albums,
                    events: events.clone(),
                },
                release_poll: ports.release_poll,
                targets: clients.targets().clone(),
                providers: ports.providers,
                listening: ports.listening,
                playlists: collections.playlists.clone(),
            },
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
        let spotify_service = Arc::new(SpotifyImportService::new(
            spotify_client.clone(),
            spotify_settings.clone(),
            spotify_links.clone(),
            spotify_playlists.clone(),
            resolver.clone(),
        ));
        let spotify_import_jobs = Arc::new(SpotifyImportJobs::new(spotify_service, events.clone()));
        let playlist_identify_jobs = Arc::new(PlaylistIdentifyJobs::new(
            collections.playlists.clone(),
            resolver.clone(),
            config_store.live(),
        ));
        let spotify_imports = SpotifyImports::new(platform.db.clone(), queue.clone());
        let sync_store = SyncStore::new(platform.db.clone(), platform.clock.clone(), queue.clone());
        let spotify_sync = Arc::new(SpotifySync::new(
            spotify_client.clone(),
            spotify_settings.clone(),
            spotify_links.clone(),
            collections.playlists.clone(),
            sync_store.clone(),
            events.clone(),
        ));
        let spotify_sync_jobs = Arc::new(SpotifySyncJobs::new(spotify_sync.clone()));
        let playlist_requests = Arc::new(PlaylistRequests::new(
            core.requests.clone(),
            collections.playlists.clone(),
            ports.catalog,
            spotify_imports.clone(),
            queue,
        ));
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
            syncs: sync_store,
            probes: Arc::new(LiveProbes::bundle(&probes)),
            ids: ids.clone(),
            base_path: config.base_path.clone(),
            trusted_proxies: config.trusted_proxies.clone(),
        };

        // Finished downloads reach the library through its import seam.
        let landing = Arc::new(LandingService::new(
            core.journal.clone(),
            ports.library,
            config_store.live(),
            stores.folders.held.clone(),
            config_store.live(),
            config_store.live(),
        ));

        // Worker over the live sources and tuning.
        let worker = Arc::new(DownloadWorker::new(
            core.journal.clone(),
            worker_settings,
            readiness.clone(),
            events.clone(),
            health,
            WorkerPorts {
                landing: landing.clone(),
                plugins: ports.plugins,
                targets: clients.targets().clone(),
            },
        ));
        let search_jobs = Arc::new(SearchJobs::new(
            JobStore::new(db.clone()),
            worker.clone(),
            clients.targets().clone(),
            search_quota,
            events.clone(),
        ));
        // The import folder stays clear of the library, the recycle bin, the
        // download folders and the app's own folders.
        let site = Arc::new(ImportSite::new(
            config_store.live(),
            landing.library().clone(),
            recycle_guard(
                &config_store,
                staging_root.clone(),
                config.slskd_downloads_path.clone(),
                config.plugins_dir(),
            ),
            config_store.live(),
            app_dirs(config),
        ));
        let drops = drop_imports(
            &db,
            &landing,
            &stores.folders,
            &events,
            &core.flows,
            site.clone(),
        );
        let held = Arc::new(HeldService::new(
            core.journal.clone(),
            worker.clone(),
            drops.clone(),
            stores.folders.clone(),
            site.clone(),
        ));
        let import_folder = Arc::new(ImportFolders::new(
            site,
            drops.clone(),
            landing.library().clone(),
        ));

        Ok(Self {
            db,
            requests: core.requests,
            playlist_requests,
            mixer: core.mixer,
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
            import_folder,
            spotify_import_jobs,
            playlist_identify_jobs,
            deemix: deemix_checks,
            spotify_sync,
            spotify_sync_jobs,
        })
    }

    /// The state the Weekly Mix routes read ([`crate::requests::mix_routes`]).
    pub fn mix_routes(&self) -> MixRoutes {
        MixRoutes {
            requests: self.requests.clone(),
            mixer: self.mixer.clone(),
        }
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
        if requests != crate::requests::service::RequestRecovery::default() {
            tracing::info!(
                cancelled = requests.cancelled,
                relinked = requests.relinked,
                redispatched = requests.redispatched,
                mix_asks_cleared = requests.mix_asks_cleared,
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
        crate::import_folder::spawn_import_folder(supervisor, self.import_folder.clone())?;
        crate::spotify_sync::due::spawn_due(supervisor, self.spotify_sync.clone())?;
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
