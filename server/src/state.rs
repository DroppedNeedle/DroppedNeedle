//! Explicit application state, built once at boot.
//!
//! Every long-lived dependency lives here and reaches handlers through the
//! `State` extractor. External behavior sits behind traits (see the `ids`
//! module) so tests construct the same state with fakes.

use std::sync::Arc;

use crate::{
    config::AppConfig, http_client::HttpClientFactory, ids::IdGenerator,
    provider_policy::ProviderPolicy,
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
}

impl AppState {
    /// Wire the state from its parts. Callers pass the production generator
    /// or a fake; nothing here reaches globals.
    pub fn new(ids: Arc<dyn IdGenerator>, http: HttpClientFactory, config: AppConfig) -> Self {
        Self { ids, http, config }
    }

    /// Verified per-provider rate policy table.
    pub fn provider_policies(&self) -> &'static [ProviderPolicy] {
        crate::provider_policy::PROVIDER_POLICIES
    }
}
