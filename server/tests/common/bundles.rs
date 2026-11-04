//! Acquisition and catalog bundles built the way the app builds them, for
//! rigs that need production-shaped bundles without the whole app. Every
//! outside address is closed, and the plugin host launches nothing.

use std::sync::Arc;

use droppedneedle_acquire::AcquireSetup;
use droppedneedle_acquire::flows::seams::{FlowEvent, TickSink};
use droppedneedle_acquire::landing::library::LibraryLanding;
use droppedneedle_acquire::landing::ports::LandingLibrary;
use droppedneedle_acquire::plugin_events::FlowTicks;
use droppedneedle_acquire::wiring::{AcquirePorts, AcquireStores};
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_catalog::artwork::warm::CoverWarm;
use droppedneedle_catalog::collections::{CollectionsState, db::CollectionsDb, store::Stores};
use droppedneedle_catalog::discover::content::DownloadReadiness;
use droppedneedle_catalog::enrichment::live::CatalogEnrichment;
use droppedneedle_catalog::lyrics::{LyricsError, LyricsQuery, OnlineLyricsSource};
use droppedneedle_catalog::metadata::{
    Catalog, PageSources,
    library::LocalCatalog,
    ports::{NoDetails, NoFollows, NoPurchaseLinks},
    upstream::{LiveCatalogSettings, Upstream},
};
use droppedneedle_catalog::test_support::UnconfiguredEnrichment;
use droppedneedle_catalog::wiring::{CatalogInputs, CatalogPorts, CatalogSetup};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_kernel::platform::Platform;
use droppedneedle_library::wiring::{LibraryCore, LibrarySetup};
use droppedneedle_media::stream::local_files::LibraryFiles;
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_providers::Providers;

use super::acquire::FixedPlugin;

/// Flow outcomes sent nowhere, for rigs that build the download journal
/// or the acquisition stores by hand.
pub fn quiet_ticks() -> Arc<dyn TickSink> {
    Arc::new(QuietTicks)
}

struct QuietTicks;

impl TickSink for QuietTicks {
    fn emit(&self, _kind: &str, _detail: &str, _at: i64) {}

    fn announce(&self, _event: FlowEvent) {}
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
    let landing = Arc::new(LibraryLanding::new(
        library,
        library.imports.releases.clone(),
    ));
    acquire_landing_at(platform, users, library, collections, landing)
}

/// As [`acquire_at`], with users from an auth rig on `platform` and the
/// platform's collections, finished downloads landing through `landing`.
pub fn acquire_on(
    platform: &Platform,
    library: &LibrarySetup,
    landing: Arc<dyn LandingLibrary>,
) -> AcquireSetup {
    acquire_landing_at(
        platform,
        super::auth::AuthRig::on(platform).deps,
        library,
        &collections_at(platform).stores,
        landing,
    )
}

/// As [`acquire_on`], with the [`FixedPlugin`] installed and enabled as a
/// download source, and the search review off: the bundle's worker
/// downloads through the plugin.
pub fn acquire_serving(
    platform: &Platform,
    library: &LibrarySetup,
    landing: Arc<dyn LandingLibrary>,
) -> (AcquireSetup, FixedPlugin) {
    let fixed = FixedPlugin::install(platform, library.core.clone());
    super::acquire::review_off(platform);
    let acquire = acquire_hosted(
        platform,
        super::auth::AuthRig::on(platform).deps,
        library,
        &collections_at(platform).stores,
        landing,
        fixed.host.clone(),
    );
    (acquire, fixed)
}

/// As [`acquire_at`], with finished downloads landing through `landing`.
pub fn acquire_landing_at(
    platform: &Platform,
    users: UsersDeps,
    library: &LibrarySetup,
    collections: &Stores,
    landing: Arc<dyn LandingLibrary>,
) -> AcquireSetup {
    let plugins = idle_host(platform, library.core.clone());
    acquire_hosted(platform, users, library, collections, landing, plugins)
}

/// As [`acquire_landing_at`], with plugins on `plugins`.
pub fn acquire_hosted(
    platform: &Platform,
    users: UsersDeps,
    library: &LibrarySetup,
    collections: &Stores,
    landing: Arc<dyn LandingLibrary>,
    plugins: Arc<PluginHost>,
) -> AcquireSetup {
    let outbound = super::closed_outbound();
    let providers = Arc::new(Providers::build(
        platform,
        outbound.providers(),
        droppedneedle_kernel::db::cache::CacheDb::scratch(),
    ));
    let mb_source = providers.musicbrainz.live_source(platform.settings.live());
    let ticks = Arc::new(FlowTicks::new(plugins.clone(), platform.events.clone()));
    AcquireSetup::build(
        platform,
        users,
        AcquireStores::open(platform, ticks),
        AcquirePorts {
            library: landing,
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
            albums: droppedneedle_library::removal::Mutations::new(library),
            listening: super::listening_store(platform),
        },
        droppedneedle_kernel::jobs::queue::JobQueue::new(
            platform.db.clone(),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
    )
    .expect("acquire bundle builds")
}

/// The production catalog bundle over `platform`, with no provider pair:
/// collections in `collections`, the catalog and artwork on closed
/// upstreams and no plugins, files read through `library`, and download
/// readiness from `acquire` when there is one. Image warms queue on the
/// platform's jobs table, where nothing runs them.
pub fn catalog_setup_at(
    platform: &Platform,
    users: UsersDeps,
    inputs: CatalogInputs,
    library: &LibrarySetup,
    collections: CollectionsState,
    acquire: Option<&AcquireSetup>,
) -> CatalogSetup {
    let cover_warm = cover_warm_at(&platform.db, platform.clock.clone());
    let catalog = Catalog::new(
        Upstream::new(
            Arc::new(Providers::unpaced(Arc::new(
                droppedneedle_providers::test_support::ScriptedWire::new(),
            ))),
            Arc::new(LiveCatalogSettings::new(&platform.settings)),
            users.clone(),
        ),
        LocalCatalog::new(platform.db.clone()),
        PageSources {
            follows: droppedneedle_catalog::wiring::collections_follows(&collections),
            ..quiet_page_sources(cover_warm.clone())
        },
    );
    let artwork = droppedneedle_catalog::wiring::artwork(platform, &inputs, catalog.clone());
    let download_readiness: Arc<dyn DownloadReadiness> = match acquire {
        Some(acquire) => Arc::new(acquire.readiness.clone()),
        None => Arc::new(NoDownloadSource),
    };
    CatalogSetup::build(
        platform,
        users,
        inputs,
        no_enrichment(),
        CatalogPorts {
            remotes: Arc::new(remotes_resolver(platform)),
            collections,
            catalog,
            artwork,
            library_files: LibraryFiles::new(platform.db.clone(), library.core.root_source()),
            download_readiness,
            cover_warm,
        },
    )
}

/// What a catalog's pages read beyond MusicBrainz and the library, with no
/// follow store and no plugins: an artist page queues its image warm
/// through `cover_warm`.
pub fn quiet_page_sources(cover_warm: CoverWarm) -> PageSources {
    PageSources {
        follows: Arc::new(NoFollows),
        purchase_links: Arc::new(NoPurchaseLinks),
        details: Arc::new(NoDetails),
        cover_warm,
    }
}

/// Image warms queued on the jobs table in `db`, stamped by `clock`, where
/// nothing runs them.
pub fn cover_warm_at(db: &Db, clock: Arc<dyn Clock>) -> CoverWarm {
    CoverWarm::new(db.clone(), JobQueue::new(db.clone(), clock))
}

/// The catalog's enrichment with no provider behind it: search results
/// carry no listener counts and there are no online lyrics.
pub fn no_enrichment() -> CatalogEnrichment {
    CatalogEnrichment {
        search: Arc::new(UnconfiguredEnrichment),
        lyrics: Arc::new(NoOnlineLyrics),
    }
}

/// Online lyrics switched off: a track whose file carries none has none.
pub struct NoOnlineLyrics;

impl OnlineLyricsSource for NoOnlineLyrics {
    fn switched_on(&self) -> bool {
        false
    }

    fn lookup<'a>(
        &'a self,
        _query: LyricsQuery<'a>,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> droppedneedle_catalog::local::stores::BoxFuture<
        'a,
        Result<Option<droppedneedle_providers::lrclib::LyricsCandidate>, LyricsError>,
    > {
        Box::pin(async { Ok(None) })
    }
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
/// settings. plex.tv is not reached: a Plex link stored without a server
/// token uses its account token.
pub fn remotes_resolver(
    platform: &Platform,
) -> droppedneedle_media::remotes::connections::ConnectionResolver {
    droppedneedle_media::remotes::connections::ConnectionResolver::over(
        platform,
        Arc::new(NoPlexTokens),
    )
}

/// A Plex server-token lookup that never answers, as if plex.tv were
/// down: the resolver keeps the account token and stores nothing.
pub struct NoPlexTokens;

impl droppedneedle_media::remotes::connections::PlexServerTokens for NoPlexTokens {
    fn server_token<'a>(
        &'a self,
        _server: &'a droppedneedle_media::remotes::connections::ServerSettings,
        _auth_token: &'a str,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> droppedneedle_media::remotes::source::BoxFuture<
        'a,
        Result<Option<String>, droppedneedle_media::remotes::source::AdapterError>,
    > {
        Box::pin(async {
            Err(
                droppedneedle_media::remotes::source::AdapterError::Transport(
                    "plex.tv is not reached in this rig".to_owned(),
                ),
            )
        })
    }
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
/// through `library`, remote playlist imports landing in `catalog_setup`'
/// collections and the personal mix granted through `acquire`. Its workers
/// are not started.
pub fn media_at(
    platform: &Platform,
    users: UsersDeps,
    library: &LibrarySetup,
    catalog_setup: &CatalogSetup,
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
            droppedneedle_catalog::collections::remote_import::PlaylistImportSink::new(
                catalog_setup.collections.clone(),
            ),
        ),
        idle_host(platform, library.core.clone()),
        droppedneedle_media::MediaSetup::remotes(platform, providers.plextv.clone()),
        None,
        &providers,
        Arc::new(droppedneedle_acquire::requests::mix::MixGrantHooks::new(
            acquire.mixer.clone(),
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

/// A library with nothing identified: a precache run over it walks no
/// artist and no album and finishes at once.
pub struct NothingToWarm;

impl droppedneedle_kernel::jobs::precache_phases::PrecacheSources for NothingToWarm {
    fn tuning(&self) -> droppedneedle_kernel::jobs::precache_phases::PrecacheTuning {
        droppedneedle_kernel::jobs::precache_phases::PrecacheTuning::default()
    }

    fn library(
        &self,
    ) -> droppedneedle_kernel::jobs::BoxFuture<
        '_,
        Result<droppedneedle_kernel::jobs::precache_phases::PrecacheLibrary, String>,
    > {
        Box::pin(async { Ok(Default::default()) })
    }

    fn artist_cached<'a>(
        &'a self,
        _artist: &'a droppedneedle_kernel::jobs::precache_phases::LibraryArtist,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn warm_artist<'a>(
        &'a self,
        _artist: &'a droppedneedle_kernel::jobs::precache_phases::LibraryArtist,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn discovery_cached<'a>(
        &'a self,
        _user_id: &'a str,
        _artist_mbid: &'a str,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn warm_discovery<'a>(
        &'a self,
        _user_id: &'a str,
        _artist_mbid: &'a str,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn album_cached<'a>(
        &'a self,
        _album: &'a droppedneedle_kernel::jobs::precache_phases::LibraryAlbum,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn warm_album<'a>(
        &'a self,
        _album: &'a droppedneedle_kernel::jobs::precache_phases::LibraryAlbum,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn audiodb_artist_cached<'a>(
        &'a self,
        _artist: &'a droppedneedle_kernel::jobs::precache_phases::LibraryArtist,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn warm_audiodb_artist<'a>(
        &'a self,
        _artist: &'a droppedneedle_kernel::jobs::precache_phases::LibraryArtist,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn audiodb_album_cached<'a>(
        &'a self,
        _album: &'a droppedneedle_kernel::jobs::precache_phases::LibraryAlbum,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn warm_audiodb_album<'a>(
        &'a self,
        _album: &'a droppedneedle_kernel::jobs::precache_phases::LibraryAlbum,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// The jobs bundle over `platform` as the app builds it, with `sources`
/// behind library precache: the concerts sweep and the media feeds over
/// closed addresses, a fresh jobs table, and no loop started.
pub fn jobs_at(
    platform: &Platform,
    checkpoint: droppedneedle_kernel::db::CheckpointService,
    sources: Arc<dyn droppedneedle_kernel::jobs::precache_phases::PrecacheSources>,
    scans: droppedneedle::bootstrap::jobs::ScansEnded,
) -> droppedneedle::bootstrap::jobs::JobsSetup {
    let providers = Providers::build(
        platform,
        super::closed_outbound().providers(),
        droppedneedle_kernel::db::cache::CacheDb::scratch(),
    );
    droppedneedle::bootstrap::jobs::JobsSetup::build(
        platform,
        quiet_loops(
            checkpoint,
            media_jobs(platform),
            droppedneedle_catalog::concerts::ConcertsSetup::from_platform(platform, &providers)
                .sweep(),
            sources,
        ),
        droppedneedle::bootstrap::jobs::QueueSetup::new(platform),
        scans,
        droppedneedle_kernel::jobs::queue::Handlers::default(),
    )
}

/// The loops' backends with no personal mix and no discover warm pass.
pub fn quiet_loops(
    checkpoint: droppedneedle_kernel::db::CheckpointService,
    media: droppedneedle_media::remotes::presence_feed::MediaJobs,
    watcher: droppedneedle_catalog::concerts::ConcertsSweep,
    precache_sources: Arc<dyn droppedneedle_kernel::jobs::precache_phases::PrecacheSources>,
) -> droppedneedle::bootstrap::jobs::LoopBackends {
    droppedneedle::bootstrap::jobs::LoopBackends {
        checkpoint,
        media,
        watcher,
        precache_sources,
        mixer: Arc::new(NoMixer),
        demand: Arc::new(droppedneedle_kernel::test_support::NoDemand),
    }
}

/// A personal-mix refresh that rebuilds nothing.
pub struct NoMixer;

impl droppedneedle_kernel::jobs::personal_mix::PersonalMixer for NoMixer {
    fn run_for_all_users(&self) -> droppedneedle_kernel::jobs::BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// The cover and artist image cache under the platform's cache folder.
pub fn covers_at(platform: &Platform) -> droppedneedle_catalog::artwork::cache::ArtworkCache {
    droppedneedle_catalog::artwork::cache::ArtworkCache::new(
        platform.config.cache_dir.join("covers"),
        platform.config.cover_cache_max_bytes,
        platform.clock.clone(),
    )
}

/// The settings bundle over `platform` as the app builds it, with `probes`
/// behind the Verify routes. Its save effects never run, the outside
/// addresses are closed, and the recycle bin check sees no download
/// folders.
pub fn settings_at(
    platform: &Platform,
    probes: Arc<dyn droppedneedle::settings::verify::VerifyProbes>,
) -> droppedneedle::settings::wiring::SettingsSetup {
    use droppedneedle::settings::effects::{Effects, LiveSaveEffects};
    let store = platform.settings.clone();
    let (effects, _) = Effects::new(
        store.clone(),
        Arc::new(LiveSaveEffects::new(
            Arc::new(droppedneedle_kernel::test_support::NoopKick),
            Arc::new(|| {}),
            Arc::new(|| {}),
        )),
    );
    droppedneedle::settings::wiring::SettingsSetup::build(
        platform,
        effects,
        &super::closed_outbound(),
        Arc::new(
            droppedneedle_library::management::settings::service::LibraryManagementService::new(
                store.clone(),
            ),
        ),
        probes,
        droppedneedle_auth::sqlite::LastFmLinks::new(&droppedneedle_auth::sqlite::AuthDb::new(
            &platform.db,
            platform.clock.clone(),
        )),
        droppedneedle_library::publish::bin::BinSource::new(
            store.live(),
            store.live(),
            Arc::new(|| Some(Vec::new())),
        ),
    )
}

/// [`settings_at`] with the live probes over closed providers, for rigs
/// that verify nothing.
pub fn settings_closed(platform: &Platform) -> droppedneedle::settings::wiring::SettingsSetup {
    settings_at(
        platform,
        Arc::new(droppedneedle::settings::verify::LiveProbes::new(
            platform.http.shared().clone(),
            &Providers::closed(),
        )),
    )
}

/// A plugin host that launches nothing, on a platform of its own over
/// `db`'s database and folder, for rigs that build acquisition by hand.
pub async fn idle_host_over(db: &droppedneedle_acquire::db::AcquireDb) -> Arc<PluginHost> {
    let platform = platform_over(db).await;
    let queue = droppedneedle_kernel::jobs::queue::JobQueue::new(
        platform.db.clone(),
        platform.clock.clone(),
    );
    idle_host(&platform, super::library::library_core(&platform, queue))
}

/// The requests state of a production acquisition bundle over `db`'s
/// database and folder, on a platform and library of their own, for rigs
/// that swap in their own dispatch or quota:
/// `RequestsState { dispatch, ..requests_over(&db).await }`.
pub async fn requests_over(
    db: &droppedneedle_acquire::db::AcquireDb,
) -> droppedneedle_acquire::requests::RequestsState {
    let platform = platform_over(db).await;
    let (library, _) = super::library::library_at(&platform);
    acquire_at(
        &platform,
        super::auth::AuthRig::on(&platform).deps,
        &library,
        &collections_at(&platform).stores,
    )
    .requests
}

async fn platform_over(db: &droppedneedle_acquire::db::AcquireDb) -> Platform {
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    super::TestPlatform::on(db.db(), &dir).build().await
}
