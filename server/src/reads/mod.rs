//! Native reads: library, search, discover, collections, platform.
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

pub mod collections;
pub mod discover;
pub mod library;
pub mod platform;
pub mod search;

use std::sync::Arc;

use axum::{Router, extract::Request, middleware::Next, response::Response};

use crate::{
    auth::{
        session::{middleware::CurrentSession, store::now_unix},
        users::{UsersDeps, stores::StoreError},
    },
    ids::IdGenerator,
};

/// Seconds in a mean Gregorian year, for the wrapped-year stamp.
const SECS_PER_YEAR: i64 = 31_556_952;

/// Everything `create_app` needs to mount the reads routes, built once.
#[derive(Clone)]
pub struct ReadsSetup {
    /// Library catalog deps (SQLite catalog over the reader pool).
    pub library: library::LibraryDeps,
    /// Unified search deps (SQLite search over the reader pool).
    pub search: search::SearchDeps,
    /// Discover/home/queue/radio deps (still the fakes; no providers wired).
    pub discover: discover::ReadsDeps,
    /// Collections state (in-memory stores; SQLite ports land later).
    pub collections: collections::CollectionsState,
    /// Covers/version/wrapped states (still the fakes; no providers wired).
    pub platform: platform::PlatformState,
}

impl ReadsSetup {
    /// Build the production bundle. `pool` serves library and search reads;
    /// `users` resolves library favorites and collections roles;
    /// `wrapped_key` yields the `wrapped_settings` secret per request (empty
    /// denies every wrapped request, the fail-closed rule); `enrichment`
    /// carries the live provider pair (`None` keeps the
    /// unconfigured ports: bare enrichment echoes and empty lyrics).
    pub fn build(
        pool: &sqlx::SqlitePool,
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        wrapped_key: impl platform::wrapped::WrappedKeySource + 'static,
        enrichment: Option<crate::providers::adapters::ProductionEnrichment>,
    ) -> Self {
        let library_db = library::sqlite::LibraryDb::new(pool);
        let catalog: Arc<dyn library::stores::LibraryCatalog> =
            Arc::new(library::sqlite::SqliteCatalog::new(&library_db));
        // No stored-lyrics table exists in the schema, so without the
        // provider pair lyrics reads stay on the empty port (404s); with
        // it, catalog tracks resolve through live LRCLIB when
        // lyrics are enabled, and stay on the empty port otherwise.
        let lyrics: Arc<dyn library::stores::LyricsPort> = match &enrichment {
            Some(pair) => match pair.lyrics.clone() {
                Some(live) => Arc::new(crate::providers::enrich::ProviderLyrics::new(
                    catalog.clone(),
                    live,
                    crate::providers::enrich::SourceBudgets::default(),
                )),
                None => Arc::new(library::memory::MemoryLyrics::new()),
            },
            None => Arc::new(library::memory::MemoryLyrics::new()),
        };
        let library = library::LibraryDeps {
            catalog,
            favorites: Arc::new(library::sqlite::SqliteFavorites::new(&library_db)),
            lyrics,
            auth: users,
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
            discover: discover_deps(ids.clone()),
            collections: collections::CollectionsState::new(),
            platform: platform_state(wrapped_key),
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
                auth: users,
                ids: ids.clone(),
            },
            search: search::SearchDeps::new(
                search::service::SearchService::new(pool),
                Arc::new(search::ports::UnconfiguredEnrichment),
                ids.clone(),
            ),
            discover: discover_deps(ids.clone()),
            collections: collections::CollectionsState::new(),
            platform: platform_state(String::new()),
        })
    }

    /// Relative-path routers for nesting under `/api/v3` inside the session
    /// gate. The collections leg carries the principal-translation layer so
    /// its handlers keep their `Principal` extractor against the real gate.
    pub fn gated_router(&self) -> Router {
        let collections = collections::collections_routes(self.collections.clone()).layer(
            axum::middleware::from_fn_with_state(self.library.auth.clone(), translate_principal),
        );
        Router::new()
            .merge(library::library_router(self.library.clone()))
            .merge(discover::reads_router(self.discover.clone()))
            .merge(collections)
            .merge(platform::session_router(&self.platform))
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

/// Discover deps: every port still runs its fake. Clocks pin at build time
/// (fakes stamp from them) while staleness reads the system clock. Real
/// providers and stores would replace the ports here.
fn discover_deps(ids: Arc<dyn IdGenerator>) -> discover::ReadsDeps {
    use discover::fakes::{
        FakeBatches, FakeCharts, FakeContent, FakeNowPlaying, FakePreviews, FakeQueues, FakeRadio,
        FakeYouTube, ManualClock,
    };

    let clock = ManualClock::new(now_unix());
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

/// Platform states: empty art, tagged version with no known
/// releases, and empty wrapped data behind the configured key.
fn platform_state(
    wrapped_key: impl platform::wrapped::WrappedKeySource + 'static,
) -> platform::PlatformState {
    use platform::{
        covers::{CoversState, FakeCoverArt},
        version::{FakeReleases, VersionState},
        wrapped::{FakeWrappedData, WrappedState},
    };

    // Mean-year math is approximate by days at most; the fake only needs a
    // plausible stats year until real data is aggregated.
    let year = (1970 + now_unix() / SECS_PER_YEAR) as i32;
    platform::PlatformState::new(
        CoversState::new(Arc::new(FakeCoverArt::empty())),
        VersionState::new(Arc::new(FakeReleases::tagged(env!("CARGO_PKG_VERSION")))),
        WrappedState::new(wrapped_key, Arc::new(FakeWrappedData::empty(year))),
    )
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
    use collections::{auth::Principal, error::CollectionsError};

    let missing = || CollectionsError::Unauthorized {
        message: "Authentication required".to_owned(),
    };
    let Some(session) = request.extensions().get::<CurrentSession>().cloned() else {
        return missing().into_response();
    };
    let user = match users.users.get_by_id(&session.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return missing().into_response(),
        Err(StoreError::Conflict) => {
            return CollectionsError::Conflict {
                message: "Conflicting state".to_owned(),
            }
            .into_response();
        }
        Err(StoreError::Internal(cause)) => {
            return CollectionsError::internal(&cause).into_response();
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
