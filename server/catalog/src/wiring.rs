//! Builds the catalog's modules into the one bundle `create_app` mounts.
//!
//! Each module owns its handlers, services and state; this file only
//! composes them. [`CatalogSetup`] holds each module's routes state. Every
//! route sits behind the session gate except the wrapped trio, which mounts
//! in its own-credential group with only its `X-Wrapped-API-Key` extractor.
//! Sessions never satisfy the wrapped routes and the wrapped key never
//! satisfies the session gate. Never move the wrapped routes to the public
//! group: public means anyone. The covers debug route mounts on its own, on
//! dev-only tooling routes (see [`crate::artwork::covers_debug`]).

use std::sync::Arc;

use droppedneedle_auth::users::UsersDeps;

use crate::{
    artwork as art, collections, discover, enrichment, local, lyrics, metadata, search, version,
    wrapped, youtube,
};

/// Everything `create_app` needs to mount the catalog routes, built once.
#[derive(Clone)]
pub struct CatalogSetup {
    /// Library catalog deps (SQLite catalog over the reader pool).
    pub local: local::LibraryDeps,
    /// Unified search deps (SQLite search over the reader pool).
    pub search: search::SearchDeps,
    /// Discover/home/queue/radio deps.
    pub discover: discover::DiscoverDeps,
    /// Collections state. Unwired on the test bundle.
    pub collections: collections::CollectionsState,
    /// Artist and album pages. Unmounted on the test bundle.
    pub metadata: Option<metadata::CatalogDeps>,
    /// Cover art routes.
    pub covers: art::covers::CoversState,
    /// Version check routes.
    pub version: version::VersionState,
    /// Wrapped routes.
    pub wrapped: wrapped::WrappedState,
    /// The cover and artist image service behind `covers`, for the
    /// precache and the cache admin. `None` on test states.
    pub artwork: Option<art::ArtworkService>,
    /// Saved YouTube links. Unwired on the test bundle.
    pub youtube: youtube::YouTubeLinks,
}

/// What the catalog takes from the other areas, built before it.
pub struct CatalogPorts {
    /// The app's connection resolver, for the Jellyfin reads discover
    /// makes.
    pub remotes: Arc<droppedneedle_media::remotes::connections::ConnectionResolver>,
    /// Collections over the database, with acquisition's approvals.
    pub collections: collections::CollectionsState,
    /// Artist and album pages, and the MusicBrainz half of unified search.
    pub catalog: metadata::Catalog,
    /// The cover and artist image service (see [`artwork`]).
    pub artwork: art::ArtworkService,
    /// File tags and the lyrics files carry, from the library catalog and
    /// roots.
    pub library_files: droppedneedle_media::stream::local_files::LibraryFiles,
    /// Acquisition's answer on whether a download source is on.
    pub download_readiness: Arc<dyn discover::content::DownloadReadiness>,
    /// Where discover, home and genre pages queue their image warm.
    pub cover_warm: Option<art::warm::CoverWarm>,
}

/// The cover and artist image service: local and Cover Art Archive art
/// through the disk cache, and artist images named by `artists` (the
/// catalog). The library plans covers through it before the bundle exists.
pub fn artwork(
    platform: &droppedneedle_kernel::platform::Platform,
    inputs: &CatalogInputs,
    artists: metadata::Catalog,
) -> art::ArtworkService {
    use art::{ArtworkService, cache::ArtworkCache, local::LocalArtwork};

    ArtworkService::new(
        ArtworkCache::new(
            inputs.covers_dir.clone(),
            inputs.cover_cache_max_bytes,
            Arc::clone(&platform.clock),
        ),
        LocalArtwork::new(platform.db.clone()),
        Arc::new(inputs.providers.coverart.clone()),
        platform.settings.live(),
        Arc::new(inputs.providers.images.clone()),
        Arc::new(metadata::precache::CatalogArtistImages(artists)),
    )
}

impl CatalogSetup {
    /// Build the production bundle over `platform`: its database serves
    /// library reads, search reads and discover's
    /// writes, and its settings hand out the live handles the modules hold
    /// on their sections;
    /// `users` gives wrapped its display names and discover its Last.fm
    /// links;
    /// `inputs` carries what the version check, wrapped and discover read
    /// from the composition root; `enrichment` carries the live provider
    /// pair (`None` keeps the unconfigured ports: bare enrichment echoes
    /// and empty lyrics); `ports` are the other areas the modules reach.
    pub fn build(
        platform: &droppedneedle_kernel::platform::Platform,
        users: UsersDeps,
        inputs: CatalogInputs,
        enrichment: Option<enrichment::live::ProductionEnrichment>,
        ports: CatalogPorts,
    ) -> Self {
        let (db, settings, ids) = (&platform.db, &*platform.settings, platform.ids.clone());
        let CatalogPorts {
            remotes,
            collections,
            catalog,
            artwork,
            library_files,
            download_readiness,
            cover_warm,
        } = ports;
        let discover = discover::wiring::production_deps(
            discover::wiring::DiscoverInputs {
                db: db.clone(),
                http: inputs.http.clone(),
                providers: inputs.providers.clone(),
                listening: inputs.listening.clone(),
                youtube_quota_path: inputs.youtube_quota_path.clone(),
                users: users.clone(),
                discover_warmer: inputs.discover_warmer,
                clock: Arc::clone(&platform.clock),
                download_readiness,
                warm: cover_warm,
                remotes,
            },
            settings,
        );
        let clock = Arc::clone(&platform.clock);
        let covers = art::covers::CoversState::new(Arc::new(artwork.clone()));
        let version = version::VersionState::new(Arc::new(version::GitHubReleases::new(
            inputs.providers.github.clone(),
            version::VersionInfo {
                version: platform.config.commit_tag.clone(),
                build_date: platform.config.build_date.clone(),
            },
        )));
        let wrapped = wrapped_state(&users, settings, inputs);
        let local = library_deps(
            catalog.local().clone(),
            catalog.clone(),
            settings,
            enrichment.as_ref(),
            library_files,
            artwork.clone(),
        );
        let search_enrichment: Arc<dyn search::ports::EnrichmentPort> = match &enrichment {
            Some(pair) => pair.search.clone(),
            None => Arc::new(search::ports::UnconfiguredEnrichment),
        };
        let catalog = catalog.with_follows(Arc::new(CollectionsFollows(
            collections.stores.follows.clone(),
        )));
        // Saved links search through discover's YouTube client, so both
        // spend from one daily budget.
        let youtube = youtube::YouTubeLinks::new(
            youtube::store::LinkStore::new(collections.db.clone()),
            discover.youtube.clone(),
            catalog.clone(),
            ids,
            clock,
        );
        let search = search::SearchDeps::new(
            search::service::SearchService::new(db.clone()).with_remote(catalog.clone()),
            search_enrichment,
        );
        Self {
            local,
            search,
            discover,
            collections,
            metadata: Some(metadata::CatalogDeps { catalog }),
            artwork: Some(artwork),
            covers,
            version,
            wrapped,
            youtube,
        }
    }
}

/// The library reads: the one local catalog, and tags and lyrics read
/// from the files first. Without the provider pair a track whose file
/// carries no lyrics has none (404); with it, the online lyrics answer
/// while the lyrics setting (read per call) is on. `covers` says which
/// covers are known missing.
fn library_deps(
    catalog: local::store::LocalCatalog,
    releases: metadata::Catalog,
    settings: &droppedneedle_kernel::runtime_config::ConfigStore,
    enrichment: Option<&enrichment::live::ProductionEnrichment>,
    files: droppedneedle_media::stream::local_files::LibraryFiles,
    covers: art::ArtworkService,
) -> local::LibraryDeps {
    let online = enrichment.map(|pair| pair.lyrics.clone());
    local::LibraryDeps {
        lyrics: Arc::new(lyrics::LyricsService::new(catalog.clone(), files, online)),
        catalog,
        upgrade_policy: Some(settings.live()),
        covers: Some(covers),
        releases: Some(releases),
    }
}

/// What the catalog takes from the composition root beyond the database.
pub struct CatalogInputs {
    /// Root of the cover cache (`<cache_dir>/covers`).
    pub covers_dir: std::path::PathBuf,
    /// Size bound of the cover cache in bytes.
    pub cover_cache_max_bytes: u64,
    /// The factory's shared client.
    pub http: reqwest::Client,
    /// Shared provider limiters.
    pub providers: Arc<droppedneedle_providers::Providers>,
    /// The app's listening store: users' ListenBrainz links (your-top
    /// charts, wrapped) and listening preferences.
    pub listening: droppedneedle_media::playback::listening::ListeningStore,
    /// The YouTube quota file (`<cache_dir>/youtube_quota.json`).
    pub youtube_quota_path: std::path::PathBuf,
    /// Whether the discover warm cycle runs (`DISCOVER_WARMER_ENABLED`).
    pub discover_warmer: bool,
}

/// Wrapped from ListenBrainz behind the configured key.
fn wrapped_state(
    users: &UsersDeps,
    settings: &droppedneedle_kernel::runtime_config::ConfigStore,
    inputs: CatalogInputs,
) -> wrapped::WrappedState {
    use wrapped::{ConfigWrappedKey, WrappedData, WrappedState, listenbrainz::ListenBrainzWrapped};

    let data: Arc<dyn WrappedData> = Arc::new(ListenBrainzWrapped::new(
        users.users.clone(),
        inputs.listening,
        inputs.providers.listenbrainz.clone(),
        Arc::clone(&users.clock),
    ));
    WrappedState::new(ConfigWrappedKey::new(settings.live()), data)
}

/// The artist header's follow state, read from the collections follow
/// store. A store failure logs and reads as "not followed".
struct CollectionsFollows(collections::store::FollowStore);

impl metadata::ports::FollowLookup for CollectionsFollows {
    fn status<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> metadata::ports::BoxFuture<'a, Option<metadata::ports::FollowState>> {
        Box::pin(async move {
            match collections::service::follow_status(&self.0, user_id, artist_mbid).await {
                Ok(status) => Some(metadata::ports::FollowState {
                    followed: status.followed,
                    auto_download: status.auto_download,
                    auto_download_state: status.auto_download_state,
                }),
                Err(error) => {
                    tracing::warn!(?error, "follow state unavailable for the artist header");
                    None
                }
            }
        })
    }
}
