//! Explicit application state, built once at boot.
//!
//! Every long-lived dependency lives here and reaches handlers through the
//! `State` extractor. External behavior sits behind traits (see the `ids`
//! module) so tests construct the same state with fakes.

use std::sync::Arc;

use crate::{
    auth::wiring::AuthSetup, config::AppConfig, http_client::HttpClientFactory, ids::IdGenerator,
    provider_policy::ProviderPolicy, reads::ReadsSetup,
};

/// All long-lived server dependencies.
#[derive(Clone)]
pub struct AppState {
    /// Fresh request and error ids.
    pub ids: Arc<dyn IdGenerator>,
    /// Factory for outbound HTTP clients.
    pub http: HttpClientFactory,
    /// Deployment configuration.
    pub config: AppConfig,
    /// Auth bundle: stores, gates, and route states for `/api/v3`.
    pub auth: AuthSetup,
    /// Reads bundle: library, search, discover, collections, platform.
    pub reads: ReadsSetup,
}

impl AppState {
    /// Wire the state from its parts. Callers pass the production generator
    /// or a fake; nothing here reaches globals.
    pub fn new(
        ids: Arc<dyn IdGenerator>,
        http: HttpClientFactory,
        config: AppConfig,
        auth: AuthSetup,
        reads: ReadsSetup,
    ) -> Self {
        Self {
            ids,
            http,
            config,
            auth,
            reads,
        }
    }

    /// Verified per-provider rate policy table.
    pub fn provider_policies(&self) -> &'static [ProviderPolicy] {
        crate::provider_policy::PROVIDER_POLICIES
    }
}
