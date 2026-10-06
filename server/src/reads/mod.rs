//! Native reads: library, search, artist and album pages, discover,
//! collections, platform.
//!
//! Each area owns its handlers, services, and state; this module only
//! composes them. [`ReadsSetup`] is the single bundle `create_app` mounts:
//! [`ReadsSetup::gated_router`] plus [`ReadsSetup::search_router`] nest
//! inside the deny-by-default session gate (search carries full `/api/v3`
//! paths, so it merges at the router root instead of under the nest), while
//! [`ReadsSetup::wrapped_router`] mounts under `/api/v3` outside the gate
//! with only its `X-Wrapped-API-Key` extractor. Sessions never satisfy the
//! wrapped routes and the wrapped key never satisfies the session gate.
//!
//! Collections and requests take a local `Principal`; [`translate_principal`]
//! builds it from the real session by resolving the role fresh from the
//! user store, mirroring the users role extractors.

pub mod catalog;
pub mod collections;
pub mod discover;
pub mod enrichment;
pub mod library;
pub mod platform;
pub mod search;

use std::sync::Arc;

use axum::{Router, extract::Request, middleware::Next, response::Response};

use crate::{
    auth::{
        session::middleware::CurrentSession,
        users::{UsersDeps, stores::StoreError},
    },
    ids::IdGenerator,
};

/// Everything `create_app` needs to mount the reads routes, built once.
#[derive(Clone)]
pub struct ReadsSetup {
    /// Library catalog deps (SQLite catalog over the reader pool).
    pub library: library::LibraryDeps,
    /// Unified search deps (SQLite search over the reader pool).
    pub search: search::SearchDeps,
    /// Discover/home/queue/radio deps.
    pub discover: discover::ReadsDeps,
    /// Collections state. Unwired until [`ReadsSetup::with_collections`]
    /// hands it the database.
    pub collections: collections::CollectionsState,
    /// Artist and album pages. Unmounted until [`ReadsSetup::with_catalog`]
    /// hands them their upstreams.
    pub catalog: Option<catalog::CatalogDeps>,
    /// Covers/version/wrapped states.
    pub platform: platform::PlatformState,
}

impl ReadsSetup {
    /// Build the production bundle. `pool` serves library and search reads;
    /// `users` resolves library favorites and collections roles;
    /// `inputs` carries what cover art, the version check, wrapped and
    /// discover read from the composition root; `enrichment`
    /// carries the live provider pair (`None` keeps the
    /// unconfigured ports: bare enrichment echoes and empty lyrics).
    pub fn build(
        pool: &sqlx::SqlitePool,
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        inputs: ReadsInputs,
        enrichment: Option<enrichment::live::ProductionEnrichment>,
    ) -> Self {
        let inputs_config = inputs.config.clone();
        let discover = discover::adapters::production_deps(
            discover::adapters::DiscoverInputs {
                pool: pool.clone(),
                config: inputs.config.clone(),
                http: inputs.http.clone(),
                providers: inputs.providers.clone(),
                listenbrainz_links: inputs.listenbrainz_links.clone(),
                youtube_quota_path: inputs.youtube_quota_path.clone(),
                users: users.clone(),
                lastfm_key: inputs.lastfm_key.clone(),
            },
            ids.clone(),
        );
        let platform = platform_state(pool, &users, inputs);
        let library_db = library::sqlite::LibraryDb::new(pool);
        let catalog: Arc<dyn library::stores::LibraryCatalog> =
            Arc::new(library::sqlite::SqliteCatalog::new(&library_db));
        // No stored-lyrics table exists in the schema, so without the
        // provider pair lyrics reads stay on the empty port (404s); with
        // it, catalog tracks resolve through live LRCLIB while the lyrics
        // setting (read per call) is on, and read as absent while it is off.
        let lyrics: Arc<dyn library::stores::LyricsPort> = match &enrichment {
            Some(pair) => Arc::new(
                enrichment::ProviderLyrics::new(
                    catalog.clone(),
                    pair.lyrics.clone(),
                    enrichment::SourceBudgets::default(),
                )
                .with_switch(pair.lyrics_enabled.clone()),
            ),
            None => Arc::new(library::memory::MemoryLyrics::new()),
        };
        let library = library::LibraryDeps {
            catalog,
            favorites: Arc::new(library::sqlite::SqliteFavorites::new(&library_db)),
            lyrics,
            lookups: Arc::new(library::lookups::SqliteLookups::new(&library_db)),
            upgrade_policy: upgrade_policy_source(inputs_config.clone()),
            auth: users.clone(),
            ids: ids.clone(),
        };
        let search_enrichment: Arc<dyn search::ports::EnrichmentPort> = match &enrichment {
            Some(pair) => pair.search.clone(),
            None => Arc::new(search::ports::UnconfiguredEnrichment),
        };
        let search = search::SearchDeps::new(
            search::service::SearchService::new(pool.clone()),
            search_enrichment,
            ids.clone(),
        );
        Self {
            library,
            search,
            discover,
            collections: collections::CollectionsState::unwired(),
            catalog: None,
            platform,
        }
    }

    /// Test bundle over unwired adapters. Library answers from empty memory
    /// stores, search from a lazy pool, and wrapped denies everything; the
    /// hooked-state suites that use this never touch reads routes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(users: UsersDeps, ids: Arc<dyn IdGenerator>) -> Result<Self, String> {
        let pool = sqlx::SqlitePool::connect_lazy("sqlite::memory:")
            .map_err(|error| format!("test reads pool: {error}"))?;
        Ok(Self {
            library: library::LibraryDeps {
                catalog: Arc::new(library::memory::MemoryCatalog::new()),
                favorites: Arc::new(library::memory::MemoryFavorites::new()),
                lyrics: Arc::new(library::memory::MemoryLyrics::new()),
                lookups: Arc::new(library::memory::EmptyLookups),
                upgrade_policy: Arc::new(library::stores::UpgradePolicy::default),
                auth: users,
                ids: ids.clone(),
            },
            search: search::SearchDeps::new(
                search::service::SearchService::new(pool),
                Arc::new(search::ports::UnconfiguredEnrichment),
                ids.clone(),
            ),
            discover: test_discover_deps(ids.clone()),
            collections: collections::CollectionsState::unwired(),
            catalog: None,
            platform: test_platform_state(),
        })
    }

    /// Serve collections from this database (pool plus writer lane).
    #[must_use]
    pub fn with_collections(mut self, db: collections::db::CollectionsDb) -> Self {
        self.collections = collections::CollectionsState::new(db);
        self
    }

    /// Serve the artist and album pages, and join MusicBrainz into unified
    /// search, through this catalog.
    #[must_use]
    pub fn with_catalog(mut self, catalog: catalog::Catalog) -> Self {
        let catalog = catalog
            .with_follows(Arc::new(CollectionsFollows(self.collections.clone())))
            .with_edition_pins(Arc::new(CollectionsPins(self.collections.clone())));
        self.search.service = self.search.service.clone().with_remote(catalog.clone());
        self.catalog = Some(catalog::CatalogDeps {
            catalog,
            ids: self.search.ids.clone(),
        });
        self
    }

    /// Relative-path routers for nesting under `/api/v3` inside the session
    /// gate. The collections leg carries the principal-translation layer so
    /// its handlers keep their `Principal` extractor against the real gate.
    pub fn gated_router(&self) -> Router {
        let collections = collections::collections_routes(self.collections.clone()).layer(
            axum::middleware::from_fn_with_state(self.library.auth.clone(), translate_principal),
        );
        let router = Router::new()
            .merge(library::library_router(self.library.clone()))
            .merge(discover::reads_router(self.discover.clone()))
            .merge(collections)
            .merge(platform::session_router(&self.platform));
        match &self.catalog {
            Some(deps) => router.merge(catalog::router(deps.clone())),
            None => router,
        }
    }

    /// Full-path search router. It already carries the `/api/v3` prefix, so
    /// the app merges it at the gated router root instead of under the nest.
    pub fn search_router(&self) -> Router {
        search::router(self.search.clone())
    }

    /// Key-gated wrapped router. The app nests this under `/api/v3` outside
    /// the session middleware: the shared secret is the only credential and
    /// must never ride the session allowlist (allowlisted means public).
    pub fn wrapped_router(&self) -> Router {
        platform::wrapped_router(&self.platform)
    }
}

/// The upgrade cutoff from the download policy, read on every call so a
/// settings change applies at once. A section that fails to read is
/// logged and treated as upgrades off.
fn upgrade_policy_source(
    config: Arc<crate::runtime_config::ConfigStore>,
) -> library::stores::UpgradePolicySource {
    Arc::new(
        move || match config.get::<crate::runtime_config::sections::DownloadPolicy>() {
            Ok(policy) => library::stores::UpgradePolicy {
                quality_cutoff: Some(policy.quality_cutoff),
                upgrade_allowed: policy.upgrade_allowed,
            },
            Err(error) => {
                tracing::warn!(%error, "download policy unreadable; album status shows no upgrades");
                library::stores::UpgradePolicy::default()
            }
        },
    )
}

/// Test discover deps: every port runs its fake.
#[cfg(any(test, feature = "test-support"))]
fn test_discover_deps(ids: Arc<dyn IdGenerator>) -> discover::ReadsDeps {
    use discover::fakes::{
        FakeBatches, FakeCharts, FakeContent, FakeNowPlaying, FakePreviews, FakeQueues, FakeRadio,
        FakeYouTube, ManualClock,
    };

    let clock = ManualClock::new(0);
    discover::ReadsDeps {
        content: Arc::new(FakeContent::new(clock.clone())),
        queues: Arc::new(FakeQueues::new(clock.clone())),
        batches: Arc::new(FakeBatches::new(clock)),
        charts: Arc::new(FakeCharts::new()),
        previews: Arc::new(FakePreviews),
        youtube: Arc::new(FakeYouTube::unconfigured()),
        radio: Arc::new(FakeRadio),
        now_playing: Arc::new(FakeNowPlaying),
        ids,
        clock: Arc::new(discover::ports::SystemClock),
    }
}

/// What the reads take from the composition root beyond the database.
pub struct ReadsInputs {
    /// Runtime settings, read per request (wrapped key, local-art
    /// preference, YouTube and integration settings).
    pub config: Arc<crate::runtime_config::ConfigStore>,
    /// Root of the cover cache (`<cache_dir>/covers`).
    pub covers_dir: std::path::PathBuf,
    /// Size bound of the cover cache in bytes.
    pub cover_cache_max_bytes: u64,
    /// The factory's shared client.
    pub http: reqwest::Client,
    /// The factory's no-redirect client (cover fetches check each hop).
    pub no_redirect: reqwest::Client,
    /// Shared provider limiters.
    pub providers: Arc<crate::providers::Providers>,
    /// Users' ListenBrainz links (your-top charts, wrapped).
    pub listenbrainz_links: Arc<dyn crate::plugins::scrobble::ListenBrainzLinkStore>,
    /// The YouTube quota file (`<cache_dir>/youtube_quota.json`).
    pub youtube_quota_path: std::path::PathBuf,
    /// The instance Last.fm API key, read per call (Last.fm charts).
    pub lastfm_key: catalog::upstream::InstanceLastFmKey,
}

/// Production platform states: local and Cover Art Archive art through
/// the disk cache, GitHub releases for the version check, and wrapped from
/// ListenBrainz behind the configured key.
fn platform_state(
    pool: &sqlx::SqlitePool,
    users: &UsersDeps,
    inputs: ReadsInputs,
) -> platform::PlatformState {
    use crate::providers::adapters::{CorePacer, CoreSink};
    use crate::providers::coverart::ReqwestCaaTransport;
    use crate::providers::github::GitHubClient;
    use crate::providers::listenbrainz::{DEFAULT_BASE_URL, ListenBrainzClient};
    use platform::{
        artwork::{ArtworkService, cache::ArtworkCache, local::LocalArtwork, remote::cover_client},
        covers::CoversState,
        listenbrainz_wrapped::ListenBrainzWrapped,
        version::{GitHubReleases, VersionState},
        wrapped::{ConfigWrappedKey, WrappedData, WrappedState},
    };

    let config = inputs.config.clone();
    let prefer_local: platform::artwork::PreferLocal = Arc::new(move || {
        match config.get::<crate::runtime_config::secret_sections::AdvancedSettings>() {
            Ok(settings) => settings.prefer_local_cover_art,
            Err(error) => {
                tracing::warn!(%error, "cannot read advanced settings; preferring local art");
                true
            }
        }
    });
    let artwork = ArtworkService::new(
        ArtworkCache::new(inputs.covers_dir, inputs.cover_cache_max_bytes),
        LocalArtwork::new(pool.clone()),
        Some(Arc::new(cover_client(ReqwestCaaTransport::new(
            inputs.no_redirect,
        )))),
        prefer_local,
    );
    let wrapped: Arc<dyn WrappedData> =
        match CorePacer::for_source(inputs.providers.clone(), "listenbrainz") {
            Some(pacer) => Arc::new(ListenBrainzWrapped::new(
                users.users.clone(),
                inputs.listenbrainz_links,
                ListenBrainzClient::new(inputs.http.clone(), DEFAULT_BASE_URL, pacer, CoreSink),
            )),
            None => {
                tracing::error!("no listenbrainz rate limit row; wrapped has no data source");
                Arc::new(platform::wrapped::NoWrappedData)
            }
        };
    platform::PlatformState::new(
        CoversState::new(Arc::new(artwork)),
        VersionState::new(Arc::new(GitHubReleases::new(GitHubClient::new(
            inputs.http,
        )))),
        WrappedState::new(ConfigWrappedKey::new(inputs.config), wrapped),
    )
}

/// Test platform states: no art, a tagged build, and wrapped denying
/// every request.
#[cfg(any(test, feature = "test-support"))]
fn test_platform_state() -> platform::PlatformState {
    use platform::{
        covers::{CoversState, FakeCoverArt},
        version::{FakeReleases, VersionState},
        wrapped::{FakeWrappedData, WrappedState},
    };

    platform::PlatformState::new(
        CoversState::new(Arc::new(FakeCoverArt::empty())),
        VersionState::new(Arc::new(FakeReleases::tagged("dev"))),
        WrappedState::new(String::new(), Arc::new(FakeWrappedData::empty(2026))),
    )
}

/// The artist header's follow state, read from the collections follow
/// store. A store failure logs and reads as "not followed".
struct CollectionsFollows(collections::CollectionsState);

impl catalog::ports::FollowLookup for CollectionsFollows {
    fn status<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> catalog::ports::BoxFuture<'a, Option<catalog::ports::FollowState>> {
        Box::pin(async move {
            match collections::service::CollectionsService::new(&self.0)
                .follow_status(user_id, artist_mbid)
                .await
            {
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

/// Edition pins written through the collections pin store.
struct CollectionsPins(collections::CollectionsState);

impl catalog::ports::EditionPins for CollectionsPins {
    fn set<'a>(
        &'a self,
        album_id: &'a str,
        release_group_mbid: &'a str,
        release_mbid: &'a str,
        user_id: &'a str,
    ) -> catalog::ports::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0
                .stores
                .pins
                .set(album_id, release_group_mbid, release_mbid, user_id)
                .await
                .map_err(|error| format!("{error:?}"))
        })
    }

    fn clear<'a>(&'a self, album_id: &'a str) -> catalog::ports::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0
                .stores
                .pins
                .clear(album_id)
                .await
                .map_err(|error| format!("{error:?}"))
        })
    }
}

/// Resolve the collections principal from the stashed session, mirroring
/// the users role extractors: the role rereads the user row every request
/// so role changes take effect immediately. A session whose account is gone
/// reads as stale (401), never as its last-known role.
async fn translate_principal(
    axum::extract::State(users): axum::extract::State<UsersDeps>,
    mut request: Request,
    next: Next,
) -> Response {
    use axum::response::IntoResponse;
    use collections::{auth::Principal, error::CollectionsError, http::CollectionsHttpError};

    let missing = || {
        CollectionsHttpError(CollectionsError::Unauthorized {
            message: "Authentication required".to_owned(),
        })
    };
    let Some(session) = request.extensions().get::<CurrentSession>().cloned() else {
        return missing().into_response();
    };
    let user = match users.users.get_by_id(&session.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return missing().into_response(),
        Err(StoreError::Conflict) => {
            return CollectionsHttpError(CollectionsError::Conflict {
                message: "Conflicting state".to_owned(),
            })
            .into_response();
        }
        Err(StoreError::Internal(cause)) => {
            return CollectionsHttpError(CollectionsError::internal(&cause)).into_response();
        }
    };
    request.extensions_mut().insert(Principal {
        user_id: user.id,
        username: user.username,
        role: match user.role {
            crate::auth::users::roles::Role::User => collections::auth::Role::User,
            crate::auth::users::roles::Role::Trusted => collections::auth::Role::Trusted,
            crate::auth::users::roles::Role::Admin => collections::auth::Role::Admin,
        },
    });
    next.run(request).await
}
