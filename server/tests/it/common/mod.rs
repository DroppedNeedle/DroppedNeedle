//! Shared test doubles: a fixed-id generator proving the trait seam, and
//! state builders for hooked and production-like apps.

use std::sync::Arc;

use droppedneedle::{
    AppConfig, AppState, config::DEFAULT_PORT, http_client::HttpClientFactory, ids::IdGenerator,
};

/// Fixed id used across briefs. A valid UUID so id-format assertions hold.
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
    let compat = droppedneedle::compat::CompatSetup::for_tests(
        auth.users.clone(),
        library.clone(),
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
    let jobs = droppedneedle::jobs::wiring::JobsSetup::for_tests(auth.users.clone());
    let plugins = droppedneedle::plugins::wiring::PluginsSetup::for_tests(
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
        test_stage6(&auth, ids.clone()),
        acquire,
        library,
        compat,
        admin,
        settings,
        jobs,
        plugins,
    )
}

/// State with the compat kill switches set explicitly (wiring tests).
pub fn hooked_state_with_compat(subsonic: bool, jellyfin: bool) -> AppState {
    let auth = test_auth();
    let ids = Arc::new(FixedIdGenerator::new(FIXED_ID));
    let mut reads = test_reads(&auth, ids.clone());
    let acquire = test_acquire(&auth, ids.clone(), &mut reads);
    let library = test_library(&auth, ids.clone());
    let compat = droppedneedle::compat::CompatSetup::for_tests(
        auth.users.clone(),
        library.clone(),
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
    let jobs = droppedneedle::jobs::wiring::JobsSetup::for_tests(auth.users.clone());
    let plugins = droppedneedle::plugins::wiring::PluginsSetup::for_tests(
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
        test_stage6(&auth, ids.clone()),
        acquire,
        library,
        compat,
        admin,
        settings,
        jobs,
        plugins,
    )
}

/// Production-like state: real UUID generator, hooks off.
pub fn prod_like_state() -> AppState {
    let auth = test_auth();
    let ids = Arc::new(droppedneedle::ids::UuidGenerator);
    let mut reads = test_reads(&auth, ids.clone());
    let acquire = test_acquire(&auth, ids.clone(), &mut reads);
    let library = test_library(&auth, ids.clone());
    let compat = droppedneedle::compat::CompatSetup::for_tests(
        auth.users.clone(),
        library.clone(),
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
    let jobs = droppedneedle::jobs::wiring::JobsSetup::for_tests(auth.users.clone());
    let plugins = droppedneedle::plugins::wiring::PluginsSetup::for_tests(
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
        test_stage6(&auth, ids.clone()),
        acquire,
        library,
        compat,
        admin,
        settings,
        jobs,
        plugins,
    )
}

/// Unwired acquire bundle over the test auth deps.
fn test_acquire(
    auth: &droppedneedle::auth::wiring::AuthSetup,
    ids: Arc<dyn IdGenerator>,
    reads: &mut droppedneedle::reads::ReadsSetup,
) -> droppedneedle::acquire::AcquireSetup {
    droppedneedle::acquire::AcquireSetup::for_tests(auth.users.clone(), ids, &mut reads.collections)
        .expect("test acquire bundle builds")
}

/// Unwired stage-6 bundle over the test auth deps.
fn test_stage6(
    auth: &droppedneedle::auth::wiring::AuthSetup,
    ids: Arc<dyn IdGenerator>,
) -> droppedneedle::stage6::Stage6Setup {
    droppedneedle::stage6::Stage6Setup::for_tests(auth.users.clone(), ids)
        .expect("test stage6 bundle builds")
}

/// Unwired reads bundle over the test auth deps.
fn test_reads(
    auth: &droppedneedle::auth::wiring::AuthSetup,
    ids: Arc<dyn IdGenerator>,
) -> droppedneedle::reads::ReadsSetup {
    droppedneedle::reads::ReadsSetup::for_tests(auth.users.clone(), ids)
        .expect("test reads bundle builds")
}

/// Unwired library bundle over the test auth deps.
fn test_library(
    auth: &droppedneedle::auth::wiring::AuthSetup,
    ids: Arc<dyn IdGenerator>,
) -> droppedneedle::library::wiring::LibrarySetup {
    droppedneedle::library::wiring::LibrarySetup::for_tests(auth.users.clone(), ids)
        .expect("test library bundle builds")
}

/// Unwired auth bundle: every adapter fails closed, which is what non-auth
/// tests need (the gate passes non-v3 paths through untouched).
fn test_auth() -> droppedneedle::auth::wiring::AuthSetup {
    droppedneedle::auth::wiring::AuthSetup::for_tests().expect("test auth bundle builds")
}

/// Provider deps with the byte cache shared out, so the admin bundle
/// observes the same entries the clients read.
fn test_providers() -> (
    Arc<droppedneedle::providers::Providers>,
    Arc<droppedneedle::providers::InMemoryProviderCache>,
) {
    let cache = Arc::new(droppedneedle::providers::InMemoryProviderCache::new());
    let providers = Arc::new(droppedneedle::providers::Providers::new(cache.clone()));
    (providers, cache)
}
