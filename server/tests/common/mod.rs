//! Shared test doubles: a fixed-id generator proving the trait seam, state
//! builders for hooked and production-like apps, the whole app for
//! journeys, the auth rig with its test sign-in layer, and a scratch
//! directory that removes itself.

// Each test binary uses part of this module.
#![allow(dead_code, unused_imports)]

mod app;
pub mod app_passwords;
pub mod auth;
pub mod auth_fakes;
pub mod compat;
pub mod library;

pub use app::TestApp;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use droppedneedle::AppState;
use droppedneedle_kernel::{
    config::AppConfig, config::DEFAULT_PORT, http_client::HttpClientFactory, ids::IdGenerator,
};

/// Fixed id used across tests. A valid UUID so id-format assertions hold.
pub const FIXED_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

/// Fake id generator returning one fixed value.
#[derive(Debug, Clone)]
pub struct FixedIdGenerator {
    id: String,
}

impl FixedIdGenerator {
    pub fn new(id: &str) -> Self {
        Self { id: id.to_owned() }
    }
}

impl IdGenerator for FixedIdGenerator {
    fn new_id(&self) -> String {
        self.id.clone()
    }
}

fn test_http() -> HttpClientFactory {
    HttpClientFactory::new().expect("test client factory builds")
}

/// State with the failure hooks mounted and fixed ids.
pub fn hooked_state() -> AppState {
    let auth = test_auth();
    let ids = Arc::new(FixedIdGenerator::new(FIXED_ID));
    let mut reads = test_reads(&auth, ids.clone());
    let acquire = test_acquire(&auth, ids.clone(), &mut reads);
    let library = test_library(&auth, ids.clone());
    let compat = droppedneedle_compat::CompatSetup::for_tests(
        auth.users.clone(),
        library.clone(),
        &reads,
        ids.clone(),
    )
    .expect("compat bundle builds");
    let (providers, cache) = test_providers();
    let admin = droppedneedle::admin::AdminSetup::for_tests(
        auth.users.clone(),
        acquire.requests.quota.clone(),
        cache,
        providers.clone(),
    );
    let jobs = droppedneedle::bootstrap::jobs::JobsSetup::for_tests(auth.users.clone());
    let plugins = droppedneedle_plugins::wiring::PluginsSetup::for_tests(
        auth.users.clone(),
        ids.clone(),
        jobs.registry().clone(),
    )
    .expect("test plugins bundle builds");
    let settings =
        droppedneedle::settings::wiring::SettingsSetup::for_tests(ids.clone(), auth.users.clone())
            .expect("test settings bundle builds");
    AppState::new(
        ids.clone(),
        test_http(),
        AppConfig::new(DEFAULT_PORT).with_test_hooks(),
        auth.clone(),
        reads,
        providers,
        test_media(&auth, ids.clone()),
        acquire,
        library,
        compat,
        admin,
        settings,
        jobs,
        plugins,
        droppedneedle_catalog::concerts::ConcertsSetup::unwired(),
    )
}

/// State with the compat kill switches set explicitly (wiring tests).
pub fn hooked_state_with_compat(subsonic: bool, jellyfin: bool) -> AppState {
    let auth = test_auth();
    let ids = Arc::new(FixedIdGenerator::new(FIXED_ID));
    let mut reads = test_reads(&auth, ids.clone());
    let acquire = test_acquire(&auth, ids.clone(), &mut reads);
    let library = test_library(&auth, ids.clone());
    let compat = droppedneedle_compat::CompatSetup::for_tests(
        auth.users.clone(),
        library.clone(),
        &reads,
        ids.clone(),
    )
    .expect("compat bundle builds")
    .with_enabled(subsonic, jellyfin);
    let (providers, cache) = test_providers();
    let admin = droppedneedle::admin::AdminSetup::for_tests(
        auth.users.clone(),
        acquire.requests.quota.clone(),
        cache,
        providers.clone(),
    );
    let jobs = droppedneedle::bootstrap::jobs::JobsSetup::for_tests(auth.users.clone());
    let plugins = droppedneedle_plugins::wiring::PluginsSetup::for_tests(
        auth.users.clone(),
        ids.clone(),
        jobs.registry().clone(),
    )
    .expect("test plugins bundle builds");
    let settings =
        droppedneedle::settings::wiring::SettingsSetup::for_tests(ids.clone(), auth.users.clone())
            .expect("test settings bundle builds");
    AppState::new(
        ids.clone(),
        test_http(),
        AppConfig::new(DEFAULT_PORT).with_test_hooks(),
        auth.clone(),
        reads,
        providers,
        test_media(&auth, ids.clone()),
        acquire,
        library,
        compat,
        admin,
        settings,
        jobs,
        plugins,
        droppedneedle_catalog::concerts::ConcertsSetup::unwired(),
    )
}

/// Production-like state: real UUID generator, hooks off.
pub fn prod_like_state() -> AppState {
    let auth = test_auth();
    let ids = Arc::new(droppedneedle_kernel::ids::UuidGenerator);
    let mut reads = test_reads(&auth, ids.clone());
    let acquire = test_acquire(&auth, ids.clone(), &mut reads);
    let library = test_library(&auth, ids.clone());
    let compat = droppedneedle_compat::CompatSetup::for_tests(
        auth.users.clone(),
        library.clone(),
        &reads,
        ids.clone(),
    )
    .expect("compat bundle builds");
    let (providers, cache) = test_providers();
    let admin = droppedneedle::admin::AdminSetup::for_tests(
        auth.users.clone(),
        acquire.requests.quota.clone(),
        cache,
        providers.clone(),
    );
    let jobs = droppedneedle::bootstrap::jobs::JobsSetup::for_tests(auth.users.clone());
    let plugins = droppedneedle_plugins::wiring::PluginsSetup::for_tests(
        auth.users.clone(),
        ids.clone(),
        jobs.registry().clone(),
    )
    .expect("test plugins bundle builds");
    let settings =
        droppedneedle::settings::wiring::SettingsSetup::for_tests(ids.clone(), auth.users.clone())
            .expect("test settings bundle builds");
    AppState::new(
        ids.clone(),
        test_http(),
        AppConfig::new(DEFAULT_PORT),
        auth.clone(),
        reads,
        providers,
        test_media(&auth, ids.clone()),
        acquire,
        library,
        compat,
        admin,
        settings,
        jobs,
        plugins,
        droppedneedle_catalog::concerts::ConcertsSetup::unwired(),
    )
}

/// Unwired acquire bundle over the test auth deps.
fn test_acquire(
    auth: &droppedneedle_auth::wiring::AuthSetup,
    ids: Arc<dyn IdGenerator>,
    reads: &mut droppedneedle_catalog::reads::ReadsSetup,
) -> droppedneedle_acquire::AcquireSetup {
    droppedneedle_acquire::AcquireSetup::for_tests(auth.users.clone(), ids, &mut reads.collections)
        .expect("test acquire bundle builds")
}

/// Unwired media bundle over the test auth deps.
fn test_media(
    auth: &droppedneedle_auth::wiring::AuthSetup,
    ids: Arc<dyn IdGenerator>,
) -> droppedneedle_media::MediaSetup {
    droppedneedle_media::MediaSetup::for_tests(auth.users.clone(), ids)
        .expect("test media bundle builds")
}

/// Unwired reads bundle over the test auth deps.
fn test_reads(
    auth: &droppedneedle_auth::wiring::AuthSetup,
    ids: Arc<dyn IdGenerator>,
) -> droppedneedle_catalog::reads::ReadsSetup {
    droppedneedle_catalog::reads::ReadsSetup::for_tests(auth.users.clone(), ids)
        .expect("test reads bundle builds")
}

/// Unwired library bundle over the test auth deps.
fn test_library(
    auth: &droppedneedle_auth::wiring::AuthSetup,
    ids: Arc<dyn IdGenerator>,
) -> droppedneedle_library::wiring::LibrarySetup {
    droppedneedle_library::wiring::LibrarySetup::for_tests(auth.users.clone(), ids)
        .expect("test library bundle builds")
}

/// Unwired auth bundle: every adapter fails closed, which is what non-auth
/// tests need (the gate passes non-v3 paths through untouched).
fn test_auth() -> droppedneedle_auth::wiring::AuthSetup {
    droppedneedle_auth::wiring::AuthSetup::for_tests().expect("test auth bundle builds")
}

/// Provider deps with the byte cache shared out, so the admin bundle
/// observes the same entries the clients read.
fn test_providers() -> (
    Arc<droppedneedle_providers::Providers>,
    Arc<droppedneedle_providers::InMemoryProviderCache>,
) {
    let cache = Arc::new(droppedneedle_providers::InMemoryProviderCache::new());
    let providers = Arc::new(droppedneedle_providers::Providers::new(cache.clone()));
    (providers, cache)
}

/// A fresh directory under the system temp dir, removed with everything in
/// it when dropped. Use this for every scratch path so test runs leave
/// nothing behind.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// Create `<tmp>/dn-it-<tag>-<pid>-<seq>`.
    pub fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("dn-it-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&path).expect("scratch dir creates");
        Self { path }
    }
}

impl std::ops::Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Write a private copy of the migrated database to `path`, which must not
/// exist yet. The migrations run once per test binary, not once per test.
pub fn migrated_db(path: &Path) {
    droppedneedle_kernel::schema::template::copy_to(path).expect("migrated database copies");
}

/// A one-connection pool on a private migrated database. Hold the guard:
/// dropping it removes the file.
pub async fn migrated_pool(tag: &str) -> (sqlx::SqlitePool, ScratchDir) {
    let dir = ScratchDir::new(tag);
    let path = dir.join("app.db");
    migrated_db(&path);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
        .await
        .expect("migrated pool opens");
    (pool, dir)
}

/// The production runtime on a fresh migrated database at `path`.
pub async fn scratch_runtime(path: &Path) -> droppedneedle_kernel::db::DbRuntime {
    migrated_db(path);
    droppedneedle_kernel::db::open_runtime(&droppedneedle_kernel::db::DbConfig::new(path))
        .await
        .expect("scratch runtime opens")
}

/// Reads inputs for production-shaped rigs: covers and the YouTube quota
/// under `dir`, settings from `config`, no linked ListenBrainz accounts and
/// no instance Last.fm key.
/// Nothing dials out unless a test asks for a cover or a chart.
pub fn reads_inputs(
    config: Arc<droppedneedle_kernel::runtime_config::ConfigStore>,
    dir: &Path,
    lane: droppedneedle_kernel::db::WriteLane,
) -> droppedneedle_catalog::reads::ReadsInputs {
    let http = test_http();
    droppedneedle_catalog::reads::ReadsInputs {
        config,
        lane,
        covers_dir: dir.join("covers"),
        cover_cache_max_bytes: 16 * 1024 * 1024,
        http: http.shared().clone(),
        no_redirect: http.no_redirect().clone(),
        providers: Arc::new(droppedneedle_providers::Providers::with_memory_cache()),
        listenbrainz_links: Arc::new(
            droppedneedle_plugins::scrobble::MemoryListenBrainzLinkStore::new(Arc::new(
                droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32])
                    .expect("test key"),
            )),
        ),
        youtube_quota_path: dir.join("youtube_quota.json"),
        lastfm_key: Arc::new(|| None),
        discover_warmer: false,
        // Nothing listens on the discard port, so no test reaches a real
        // chart or discovery provider.
        endpoints: closed_endpoints(),
    }
}

/// Every catalog upstream on a closed port.
pub fn closed_endpoints() -> droppedneedle_catalog::reads::catalog::upstream::Endpoints {
    closed_outbound().catalog()
}

/// Every outside service with a fixed address on the discard port, where
/// nothing listens.
pub fn closed_outbound() -> droppedneedle::bootstrap::Outbound {
    outbound_at("http://127.0.0.1:9")
}

/// Every outside service with a fixed address at its own path under one
/// origin.
pub fn outbound_at(origin: &str) -> droppedneedle::bootstrap::Outbound {
    let at = |path: &str| format!("{origin}/{path}");
    droppedneedle::bootstrap::Outbound {
        listenbrainz: at("listenbrainz"),
        lastfm: at("lastfm/"),
        audiodb: at("audiodb"),
        wikidata: at("wikidata"),
        wikipedia: at("wikipedia-{lang}"),
        commons: at("commons"),
        itunes: at("itunes/search"),
        deezer: at("deezer"),
        cover_art_archive: at("coverart"),
        discogs: at("discogs"),
        lrclib: at("lrclib"),
        github_releases: at("releases"),
        github_api: at("github"),
        github_codeload: at("codeload"),
        youtube: at("youtube"),
        acoustid: at("acoustid"),
        pwned_passwords: at("pwned/range/"),
        plex_tv: at("plex"),
        spotify_api: at("spotify"),
        spotify_accounts: at("spotify-accounts"),
        ticketmaster: at("ticketmaster"),
        skiddle: at("skiddle"),
        geocoding: at("geocoding"),
        musicbrainz: at("musicbrainz"),
        brainzmash: at("brainzmash"),
        identify_providers: droppedneedle::bootstrap::Port::live(),
        contribution_providers: droppedneedle::bootstrap::Port::live(),
    }
}

/// A settings store in `dir` with a fixed test key.
pub fn config_store(dir: &Path) -> Arc<droppedneedle_kernel::runtime_config::ConfigStore> {
    Arc::new(
        droppedneedle_kernel::runtime_config::ConfigStore::open(
            &dir.join("config.json"),
            droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32])
                .expect("test key"),
        )
        .expect("config store opens"),
    )
}
