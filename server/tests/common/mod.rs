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
    AppState::new(
        ids.clone(),
        test_http(),
        AppConfig::new(DEFAULT_PORT).with_test_hooks(),
        auth.clone(),
        reads,
        Arc::new(droppedneedle::providers::Providers::with_memory_cache()),
        test_stage6(&auth, ids),
        acquire,
        library,
    )
}

/// Production-like state: real UUID generator, hooks off. Only some test
/// targets use it; each target compiles this module separately.
#[allow(dead_code)]
pub fn prod_like_state() -> AppState {
    let auth = test_auth();
    let ids = Arc::new(droppedneedle::ids::UuidGenerator);
    let mut reads = test_reads(&auth, ids.clone());
    let acquire = test_acquire(&auth, ids.clone(), &mut reads);
    let library = test_library(&auth, ids.clone());
    AppState::new(
        ids.clone(),
        test_http(),
        AppConfig::new(DEFAULT_PORT),
        auth.clone(),
        reads,
        Arc::new(droppedneedle::providers::Providers::with_memory_cache()),
        test_stage6(&auth, ids),
        acquire,
        library,
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
