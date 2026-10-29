//! Acquisition and reads bundles built the way the app builds them, for
//! rigs that need production-shaped bundles without the whole app. Every
//! outside address is closed, and the plugin host launches nothing.

use std::sync::Arc;

use droppedneedle_acquire::AcquireSetup;
use droppedneedle_acquire::flows::seams::{FlowEvent, TickSink};
use droppedneedle_acquire::landing::library::LibraryLanding;
use droppedneedle_acquire::plugin_events::FlowTicks;
use droppedneedle_acquire::wiring::{AcquirePorts, AcquireStores};
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_catalog::reads::catalog::{
    Catalog,
    library::LocalCatalog,
    upstream::{LiveCatalogSettings, Upstream},
};
use droppedneedle_catalog::reads::collections::{
    CollectionsState, db::CollectionsDb, store::Stores,
};
use droppedneedle_catalog::reads::discover::content::DownloadReadiness;
use droppedneedle_catalog::reads::{ReadsInputs, ReadsPorts, ReadsSetup};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_library::wiring::{LibraryCore, LibrarySetup};
use droppedneedle_media::stream::local_files::LibraryFiles;
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_providers::Providers;

/// Flow outcomes sent nowhere but the event hub, for rigs that build the
/// download journal or the acquisition stores by hand.
pub fn quiet_ticks() -> Arc<dyn TickSink> {
    Arc::new(FlowTicks::new(
        None,
        droppedneedle_kernel::events::EventHub::new(),
    ))
}

/// A tick sink that keeps the key of every request announced as having
/// reached the library, in order.
#[derive(Default)]
pub struct FulfilledTicks {
    keys: std::sync::Mutex<Vec<String>>,
}

impl FulfilledTicks {
    /// The keys announced so far.
    pub fn keys(&self) -> Vec<String> {
        self.keys.lock().expect("fulfilled keys").clone()
    }
}

impl TickSink for FulfilledTicks {
    fn emit(&self, _kind: &str, _detail: &str, _at: i64) {}

    fn announce(&self, event: FlowEvent) {
        if let FlowEvent::RequestFulfilled { request_id, .. } = event {
            self.keys.lock().expect("fulfilled keys").push(request_id);
        }
    }
}

/// A plugin host over the platform's plugins folder that launches nothing,
/// adding library files through `library`.
pub fn idle_host(platform: &Platform, library: LibraryCore) -> Arc<PluginHost> {
    PluginHost::new(
        platform.config.plugins_dir(),
        platform.settings.clone(),
        Arc::new(super::plugin_fakes::FakeLauncher::new()),
        droppedneedle_plugins::wiring::tick_store(platform),
        library,
    )
}

/// The production acquisition bundle over `platform`, finished downloads
/// landing in `library`, follows and playlists in `collections`.
pub fn acquire_at(
    platform: &Platform,
    users: UsersDeps,
    library: &LibrarySetup,
    collections: &Stores,
) -> AcquireSetup {
    let outbound = super::closed_outbound();
    let providers = Arc::new(Providers::build(
        platform,
        outbound.providers(),
        droppedneedle_kernel::db::cache::CacheDb::scratch(),
    ));
    let mb_source = providers.musicbrainz.live_source(platform.settings.live());
    let plugins = idle_host(platform, library.core.clone());
    let ticks = Arc::new(FlowTicks::new(
        Some(plugins.clone()),
        platform.events.clone(),
    ));
    AcquireSetup::build(
        platform,
        users,
        AcquireStores::open(platform, ticks),
        AcquirePorts {
            library: Arc::new(LibraryLanding::new(
                library,
                library.imports.releases.clone(),
            )),
            edition_plans: Arc::new(crate::common::library::operations(library)),
            album_lookup: Arc::new(droppedneedle_acquire::target::lookup::live_albums(
                &providers,
                mb_source.clone(),
            )),
            release_poll: Arc::new(droppedneedle_acquire::search::live_release_poll(
                &providers, mb_source,
            )),
            plugins,
            collections: collections.clone(),
            providers,
            albums: droppedneedle_library::mutations::Mutations::new(library),
            listening: super::listening_store(platform),
        },
        &outbound.spotify(),
        droppedneedle_kernel::jobs::queue::JobQueue::new(
            platform.db.clone(),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
    )
    .expect("acquire bundle builds")
}

/// The production reads bundle over `platform`, with no provider pair:
/// collections in `collections`, the catalog and artwork on closed
/// upstreams, files read through `library`, and download readiness from
/// `acquire` when there is one.
pub fn reads_at(
    platform: &Platform,
    users: UsersDeps,
    inputs: ReadsInputs,
    library: &LibrarySetup,
    collections: CollectionsState,
    acquire: Option<&AcquireSetup>,
) -> ReadsSetup {
    let catalog = Catalog::new(
        Upstream::new(
            Arc::new(Providers::unpaced(Arc::new(
                droppedneedle_providers::executor::ScriptedWire::new(),
            ))),
            Arc::new(LiveCatalogSettings::new(&platform.settings)),
            users.clone(),
        ),
        LocalCatalog::new(platform.db.clone()),
    );
    let artwork = droppedneedle_catalog::reads::artwork(platform, &inputs, catalog.clone());
    let download_readiness: Arc<dyn DownloadReadiness> = match acquire {
        Some(acquire) => Arc::new(acquire.readiness.clone()),
        None => Arc::new(NoDownloadSource),
    };
    ReadsSetup::build(
        platform,
        users,
        inputs,
        None,
        ReadsPorts {
            remotes: Arc::new(remotes_resolver(platform)),
            collections,
            catalog,
            artwork,
            library_files: LibraryFiles::new(platform.db.clone(), library.core.root_source()),
            download_readiness,
            cover_warm: None,
        },
    )
}

/// The collections database and stores, opened before acquisition is
/// built on them.
pub struct Collections {
    pub db: CollectionsDb,
    pub stores: Stores,
    pub catalog: LocalCatalog,
}

/// The collections over the platform's database.
pub fn collections_at(platform: &Platform) -> Collections {
    let db = CollectionsDb::new(platform.db.clone(), platform.clock.clone());
    let stores = Stores::new(&db);
    Collections {
        db,
        stores,
        catalog: LocalCatalog::new(platform.db.clone()),
    }
}

impl Collections {
    /// The collections state over these stores.
    pub fn wired(&self) -> CollectionsState {
        CollectionsState::over(self.db.clone(), self.stores.clone(), self.catalog.clone())
    }
}

/// Download readiness for a rig with no download system: no source is on.
pub struct NoDownloadSource;

impl DownloadReadiness for NoDownloadSource {
    fn can_acquire(&self) -> bool {
        false
    }
}

/// The connection resolver the app builds: the per-user rows in the
/// platform's database, sealed with its key, over the admin's server
/// settings.
pub fn remotes_resolver(
    platform: &Platform,
) -> droppedneedle_media::remotes::connections::ConnectionResolver {
    use droppedneedle_media::remotes::connections::{
        ConfigServers, ConnectionResolver, CredentialCoder, SqliteConnectionStore,
    };
    ConnectionResolver::new(
        Arc::new(SqliteConnectionStore::new(
            platform.db.clone(),
            platform.clock.clone(),
        )),
        Arc::new(CredentialCoder::new(platform.crypto.clone())),
        Arc::new(ConfigServers::new(&platform.settings)),
    )
}

/// The jobs bundle's media inputs for a rig with no media bundle: a fresh
/// presence registry over the platform's event hub and the app's resolver.
pub fn media_jobs(platform: &Platform) -> droppedneedle_media::remotes::presence_feed::MediaJobs {
    droppedneedle_media::remotes::presence_feed::MediaJobs::new(
        droppedneedle_media::playback::presence::PresenceRegistry::new(platform.events.clone()),
        Arc::new(remotes_resolver(platform)),
        platform.http.shared().clone(),
        platform.clock.clone(),
        droppedneedle_media::remotes::warmup::MbidIndex::default(),
    )
}

/// The production media bundle over `platform`, with no ffmpeg: files read
/// through `library`, remote playlist imports landing in `reads`'
/// collections and the personal mix granted through `acquire`. Its workers
/// are not started.
pub fn media_at(
    platform: &Platform,
    users: UsersDeps,
    library: &LibrarySetup,
    reads: &ReadsSetup,
    acquire: &AcquireSetup,
) -> droppedneedle_media::MediaSetup {
    let outbound = super::closed_outbound();
    let providers = Providers::build(
        platform,
        outbound.providers(),
        droppedneedle_kernel::db::cache::CacheDb::scratch(),
    );
    let (media, _workers) = droppedneedle_media::MediaSetup::build(
        platform,
        users,
        library.core.root_source(),
        Arc::new(
            droppedneedle_catalog::reads::collections::remote_import::PlaylistImportSink::new(
                reads.collections.clone(),
            ),
        ),
        idle_host(platform, library.core.clone()),
        droppedneedle_media::MediaSetup::remotes(platform, outbound.plex_tv.clone()),
        None,
        &providers,
        Arc::new(droppedneedle_acquire::requests::mix::MixGrantHooks::new(
            acquire.requests.mixer.clone(),
        )),
        super::listening_store(platform),
        droppedneedle_kernel::jobs::queue::JobQueue::new(
            platform.db.clone(),
            platform.clock.clone(),
        ),
        platform.clock.clone(),
    )
    .expect("media bundle builds");
    media
}
