//! Native reads: library, search, artist and album pages, discover,
//! collections, platform.
//!
//! Each area owns its handlers, services, and state; this module only
//! composes them. [`ReadsSetup`] is the single bundle `create_app` mounts,
//! each area's routes with the state the bundle holds for it. Every route
//! sits behind the session gate except the wrapped trio, which mounts in
//! its own-credential group with only its `X-Wrapped-API-Key` extractor.
//! Sessions never satisfy the wrapped routes and the wrapped key never
//! satisfies the session gate.

pub mod cache_hold;
pub mod catalog;
pub mod collections;
pub mod discover;
pub mod enrichment;
pub mod library;
pub mod platform;
pub mod search;
pub mod youtube;

use std::sync::Arc;

use droppedneedle_auth::users::UsersDeps;

/// Everything `create_app` needs to mount the reads routes, built once.
#[derive(Clone)]
pub struct ReadsSetup {
    /// Library catalog deps (SQLite catalog over the reader pool).
    pub library: library::LibraryDeps,
    /// Unified search deps (SQLite search over the reader pool).
    pub search: search::SearchDeps,
    /// Discover/home/queue/radio deps.
    pub discover: discover::ReadsDeps,
    /// Collections state. Unwired on the test bundle.
    pub collections: collections::CollectionsState,
    /// Artist and album pages. Unmounted on the test bundle.
    pub catalog: Option<catalog::CatalogDeps>,
    /// Covers/version/wrapped states.
    pub platform: platform::PlatformState,
    /// The cover and artist image service behind `platform`, for the
    /// precache and the cache admin. `None` on test states.
    pub artwork: Option<platform::artwork::ArtworkService>,
    /// Saved YouTube links. Unwired on the test bundle.
    pub youtube: youtube::YouTubeLinks,
}

/// What the reads take from the other areas, built before them.
pub struct ReadsPorts {
    /// Collections over the database, with acquisition's approvals.
    pub collections: collections::CollectionsState,
    /// Artist and album pages, and the MusicBrainz half of unified search.
    pub catalog: catalog::Catalog,
    /// The cover and artist image service (see [`artwork`]).
    pub artwork: platform::artwork::ArtworkService,
    /// File tags and the lyrics files carry, from the library catalog and
    /// roots.
    pub library_files: droppedneedle_media::stream::local_files::LibraryFiles,
    /// Discovery batches file their albums through this.
    pub batch_requests: Arc<dyn discover::adapters::batches::BatchRequests>,
    /// Where discover, home and genre pages queue their image warm.
    pub cover_warm: Option<platform::artwork::warm::CoverWarm>,
}

/// The cover and artist image service: local and Cover Art Archive art
/// through the disk cache, and artist images named by `artists` (the
/// catalog). The library plans covers through it before the reads exist.
pub fn artwork(
    platform: &droppedneedle_kernel::platform::Platform,
    inputs: &ReadsInputs,
    artists: catalog::Catalog,
) -> platform::artwork::ArtworkService {
    use platform::artwork::{ArtworkService, cache::ArtworkCache, local::LocalArtwork};

    ArtworkService::new(
        ArtworkCache::new(inputs.covers_dir.clone(), inputs.cover_cache_max_bytes),
        LocalArtwork::new(platform.db.sqlx().clone()),
        Some(Arc::new(inputs.providers.coverart.clone())),
        platform.settings.live(),
    )
    .with_images(Arc::new(inputs.providers.images.clone()))
    .with_artist_source(Arc::new(catalog::precache::CatalogArtistImages(artists)))
}

impl ReadsSetup {
    /// Build the production bundle over `platform`: its database serves
    /// library and search reads (through its sqlx pool) and discover's
    /// writes, and its settings hand out the live handles the reads hold
    /// on their sections;
    /// `users` gives wrapped its display names and discover its batch
    /// callers and Last.fm links;
    /// `inputs` carries what the version check, wrapped and discover read
    /// from the composition root; `enrichment` carries the live provider
    /// pair (`None` keeps the unconfigured ports: bare enrichment echoes
    /// and empty lyrics); `ports` are the other areas the reads reach.
    pub fn build(
        platform: &droppedneedle_kernel::platform::Platform,
        users: UsersDeps,
        inputs: ReadsInputs,
        enrichment: Option<enrichment::live::ProductionEnrichment>,
        ports: ReadsPorts,
    ) -> Self {
        let (db, settings, ids) = (&platform.db, &*platform.settings, platform.ids.clone());
        let pool = db.sqlx();
        let ReadsPorts {
            collections,
            catalog,
            artwork,
            library_files,
            batch_requests,
            cover_warm,
        } = ports;
        let discover = discover::adapters::production_deps(
            discover::adapters::DiscoverInputs {
                db: db.clone(),
                http: inputs.http.clone(),
                providers: inputs.providers.clone(),
                listenbrainz_links: inputs.listenbrainz_links.clone(),
                youtube_quota_path: inputs.youtube_quota_path.clone(),
                users: users.clone(),
                discover_warmer: inputs.discover_warmer,
                clock: Arc::clone(&platform.clock),
                batch_requests,
                warm: cover_warm,
            },
            settings,
        );
        let clock = Arc::clone(&platform.clock);
        let platform = platform_state(&users, settings, inputs, &artwork);
        // Saved links search through discover's YouTube client, so both
        // spend from one daily budget.
        let youtube = youtube::YouTubeLinks::new(
            youtube::store::LinkStore::new(collections.db.clone()),
            discover.youtube.clone(),
            ids,
            clock,
        );
        let library = library_deps(pool, settings, enrichment.as_ref(), library_files);
        let search_enrichment: Arc<dyn search::ports::EnrichmentPort> = match &enrichment {
            Some(pair) => pair.search.clone(),
            None => Arc::new(search::ports::UnconfiguredEnrichment),
        };
        let catalog = catalog.with_follows(Arc::new(CollectionsFollows(
            collections.stores.follows.clone(),
        )));
        let search = search::SearchDeps::new(
            search::service::SearchService::new(pool.clone()).with_remote(catalog.clone()),
            search_enrichment,
        );
        Self {
            library,
            search,
            discover,
            collections,
            catalog: Some(catalog::CatalogDeps { catalog }),
            artwork: Some(artwork),
            platform,
            youtube,
        }
    }
}

/// The library reads: the SQLite catalog, favorites and lookups over the
/// reader pool, and tags and lyrics read from the files first. Without the
/// provider pair lyrics fall back to the empty port (404s); with it,
/// catalog tracks resolve through live LRCLIB while the lyrics setting
/// (read per call) is on, and read as absent while it is off.
fn library_deps(
    pool: &sqlx::SqlitePool,
    settings: &droppedneedle_kernel::runtime_config::ConfigStore,
    enrichment: Option<&enrichment::live::ProductionEnrichment>,
    files: droppedneedle_media::stream::local_files::LibraryFiles,
) -> library::LibraryDeps {
    let catalog = library::sqlite::LibraryCatalog::new(pool);
    let provider_lyrics: Arc<dyn library::stores::LyricsPort> = match enrichment {
        Some(pair) => Arc::new(
            enrichment::ProviderLyrics::new(
                catalog.clone(),
                pair.lyrics.clone(),
                enrichment::SourceBudgets::default(),
            )
            .with_settings(pair.lyrics_settings.clone()),
        ),
        None => Arc::new(library::file_lyrics::NoOnlineLyrics),
    };
    library::LibraryDeps {
        lyrics: Arc::new(library::file_lyrics::FileLyrics::new(
            catalog.clone(),
            files.clone(),
            provider_lyrics,
        )),
        catalog,
        favorites: library::sqlite::FavoriteReads::new(pool),
        lookups: library::lookups::LibraryLookups::new(pool),
        upgrade_policy: Some(settings.live()),
        tags: library::tags::FileTagReader::new(files),
    }
}

/// What the reads take from the composition root beyond the database.
pub struct ReadsInputs {
    /// Root of the cover cache (`<cache_dir>/covers`).
    pub covers_dir: std::path::PathBuf,
    /// Size bound of the cover cache in bytes.
    pub cover_cache_max_bytes: u64,
    /// The factory's shared client.
    pub http: reqwest::Client,
    /// Shared provider limiters.
    pub providers: Arc<droppedneedle_providers::Providers>,
    /// Users' ListenBrainz links (your-top charts, wrapped).
    pub listenbrainz_links: Arc<dyn droppedneedle_plugins::scrobble::ListenBrainzLinkStore>,
    /// The YouTube quota file (`<cache_dir>/youtube_quota.json`).
    pub youtube_quota_path: std::path::PathBuf,
    /// Whether the discover warm cycle runs (`DISCOVER_WARMER_ENABLED`).
    pub discover_warmer: bool,
}

/// Production platform states: art through `artwork`, GitHub releases for
/// the version check, and wrapped from ListenBrainz behind the configured
/// key.
fn platform_state(
    users: &UsersDeps,
    settings: &droppedneedle_kernel::runtime_config::ConfigStore,
    inputs: ReadsInputs,
    artwork: &platform::artwork::ArtworkService,
) -> platform::PlatformState {
    use platform::{
        covers::CoversState,
        listenbrainz_wrapped::ListenBrainzWrapped,
        version::{GitHubReleases, VersionState},
        wrapped::{ConfigWrappedKey, WrappedData, WrappedState},
    };

    let wrapped: Arc<dyn WrappedData> = Arc::new(ListenBrainzWrapped::new(
        users.users.clone(),
        inputs.listenbrainz_links,
        inputs.providers.listenbrainz.clone(),
    ));
    platform::PlatformState::new(
        CoversState::new(Arc::new(artwork.clone())),
        VersionState::new(Arc::new(GitHubReleases::new(
            inputs.providers.github.clone(),
        ))),
        WrappedState::new(ConfigWrappedKey::new(settings.live()), wrapped),
    )
}

/// The artist header's follow state, read from the collections follow
/// store. A store failure logs and reads as "not followed".
struct CollectionsFollows(collections::store::FollowStore);

impl catalog::ports::FollowLookup for CollectionsFollows {
    fn status<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> catalog::ports::BoxFuture<'a, Option<catalog::ports::FollowState>> {
        Box::pin(async move {
            match collections::service::follow_status(&self.0, user_id, artist_mbid).await {
                Ok(status) => Some(catalog::ports::FollowState {
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
