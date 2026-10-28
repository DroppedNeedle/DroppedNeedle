//! The acquisition bundle over a scratch database, for tests.

use std::path::Path;
use std::sync::Arc;

use super::{
    AcquireSetup, AcquireStores, ClientLives, CorePorts, SPOTIFY_ACCOUNTS_BASE, SPOTIFY_API_BASE,
    SourceNotices, core, drop_imports,
};
use crate::clients::LiveClients;
use crate::db::AcquireDb;
use crate::downloads::worker::{DownloadWorker, SourceList, WorkerConfig, WorkerSettings};
use crate::flows::loops::PruneDeps;
use crate::held::{HeldFolders, HeldService};
use crate::imports::handlers::ImportsDeps;
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
use crate::readiness::SourceReadiness;
use crate::requests::quota::QuotaLedger;
use crate::search_jobs::SearchJobs;
use crate::search_jobs::store::JobStore;
use crate::settings::{ConfigLidarrSettings, ConfigSpotifySettings};
use crate::target::Targets;
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::queue::JobQueue;
use tokio::sync::watch;

impl AcquireSetup {
    /// Test bundle over `db` (a scratch database), default settings and
    /// the import stores over it. Finished downloads land in `library` when
    /// there is one. Call inside a tokio runtime.
    pub fn for_tests(
        db: AcquireDb,
        ids: Arc<dyn IdGenerator>,
        events: EventHub,
        library: Option<Arc<dyn LandingLibrary>>,
    ) -> Result<Self, String> {
        use droppedneedle_catalog::reads::collections::db::CollectionsDb;
        use droppedneedle_catalog::reads::collections::store::PlaylistStore;
        use droppedneedle_kernel::runtime_config::crypto::Crypto;

        // Under the scratch database's directory, so it goes with it.
        let scratch = db
            .path()
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "scratch database has no directory".to_owned())?;
        let staging_root = scratch.join("staging");
        let stores = AcquireStores::over(
            db.clone(),
            Arc::new(crate::plugin_events::FlowTicks::new(None, events.clone())),
            HeldFolders {
                held: scratch.join("held"),
                drop: staging_root.join("drop"),
            },
        );
        let settings = crate::settings::scratch_settings(&staging_root.join("settings"))?;
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
                release_poll: Arc::new(crate::search::NoReleasePoll),
                mix: None,
                targets: clients.targets().clone(),
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
        let landing = Arc::new(LandingService::new(
            core.journal.clone(),
            library,
            settings.live(),
            stores.folders.held.clone(),
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
        let drops = drop_imports(&db, &landing, &stores.folders, &events, &core.flows);
        let held = Arc::new(HeldService::new(
            core.journal.clone(),
            worker.clone(),
            drops.clone(),
            stores.folders.clone(),
        ));
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
            held,
            spotify_import_jobs,
        })
    }
}
