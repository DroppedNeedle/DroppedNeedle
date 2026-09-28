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
    AppState::new(
        Arc::new(FixedIdGenerator::new(FIXED_ID)),
        test_http(),
        AppConfig::new(DEFAULT_PORT).with_test_hooks(),
    )
}

/// Production-like state: real UUID generator, hooks off. Only some test
/// targets use it; each target compiles this module separately.
#[allow(dead_code)]
pub fn prod_like_state() -> AppState {
    AppState::new(
        Arc::new(droppedneedle::ids::UuidGenerator),
        test_http(),
        AppConfig::new(DEFAULT_PORT),
    )
}
