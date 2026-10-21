//! Acquisition and reads bundles built the way the app builds them, for
//! rigs that need production-shaped bundles without the whole app. Every
//! outside address is closed, and the plugin host launches nothing.

use std::sync::Arc;

use droppedneedle_acquire::AcquireSetup;
use droppedneedle_acquire::flows::seams::{FlowEvent, TickSink};
use droppedneedle_acquire::landing::library::LibraryLanding;
use droppedneedle_acquire::plugin_events::FlowTicks;
use droppedneedle_acquire::requests::discovery::DiscoveryBatchRequests;
use droppedneedle_acquire::wiring::{AcquirePorts, AcquireStores};
use droppedneedle_auth::{Admin, Principal, users::UsersDeps};
use droppedneedle_catalog::reads::catalog::{
    Catalog,
    library::LocalCatalog,
    upstream::{LiveCatalogSettings, Upstream},
};
use droppedneedle_catalog::reads::collections::{
    CollectionsState, db::CollectionsDb, store::Stores,
};
use droppedneedle_catalog::reads::discover::adapters::batches::{BatchAlbum, BatchRequests};
use droppedneedle_catalog::reads::discover::ports::{BatchFailure, BoxFuture, ProviderFailure};
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
        Arc::new(droppedneedle_plugins::fakes::FakeLauncher::new()),
        Arc::new(droppedneedle_plugins::wiring::tick_store(platform)),
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
        AcquireStores::open(&platform.db, ticks),
        AcquirePorts {
            library: Arc::new(LibraryLanding::new(
                library.clone(),
                library.releases.clone(),
                platform.db.sqlx().clone(),
            )),
            edition_plans: Arc::new(droppedneedle_library::operations::service::Operations::new(
                library,
            )),
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
/// upstreams, files read through `library`, and discovery batches filed
/// through `acquire` when there is one.
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
    let batch_requests: Arc<dyn BatchRequests> = match acquire {
        Some(acquire) => Arc::new(DiscoveryBatchRequests::new(
            acquire.requests.clone(),
            Some(library.clone()),
        )),
        None => Arc::new(NoBatchRequests),
    };
    ReadsSetup::build(
        platform,
        users,
        inputs,
        None,
        ReadsPorts {
            collections,
            catalog,
            artwork,
            library_files: LibraryFiles::new(platform.db.sqlx().clone(), library.root_source()),
            batch_requests,
            cover_warm: None,
        },
    )
}

/// The collections database and stores, opened before acquisition is
/// built on them.
pub struct Collections {
    pub db: CollectionsDb,
    pub stores: Stores,
}

/// The collections over the platform's database.
pub fn collections_at(platform: &Platform) -> Collections {
    let db = CollectionsDb::new(platform.db.clone());
    let stores = Stores::new(&db);
    Collections { db, stores }
}

impl Collections {
    /// The collections state over these stores.
    pub fn wired(&self) -> CollectionsState {
        CollectionsState::over(self.db.clone(), self.stores.clone())
    }
}

/// Discovery batches for a rig with no download system: every request is
/// refused.
struct NoBatchRequests;

impl BatchRequests for NoBatchRequests {
    fn request_albums<'a>(
        &'a self,
        _caller: &'a Principal,
        _albums: Vec<BatchAlbum>,
    ) -> BoxFuture<'a, Result<(), BatchFailure>> {
        Box::pin(async { Err(BatchFailure::Provider(not_running())) })
    }

    fn withdraw_album<'a>(
        &'a self,
        _owner_id: &'a str,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async { Ok(false) })
    }

    fn remove_album<'a>(
        &'a self,
        _admin: &'a Admin,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async { Ok(false) })
    }
}

fn not_running() -> ProviderFailure {
    ProviderFailure::not_built("no download system in this rig")
}
