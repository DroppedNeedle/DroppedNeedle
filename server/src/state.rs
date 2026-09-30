//! Explicit application state, built once at boot.
//!
//! Every long-lived dependency lives here and reaches handlers through the
//! `State` extractor. External behavior sits behind traits (see the `ids`
//! module) so tests construct the same state with fakes.

use std::sync::Arc;

use crate::{
    acquire::AcquireSetup, auth::wiring::AuthSetup, config::AppConfig,
    http_client::HttpClientFactory, ids::IdGenerator, library::wiring::LibrarySetup,
    provider_policy::ProviderPolicy, providers::Providers, reads::ReadsSetup, stage6::Stage6Setup,
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
    /// Shared provider deps: verified limiters, slot lanes, byte cache.
    /// Built once at boot (in-memory cache) and shared with the reads
    /// enrichment pair so production pacing runs through one limiter set.
    pub providers: Arc<Providers>,
    /// Stage-6 bundle: remote sources, stream gateway, playback reporting.
    pub stage6: Stage6Setup,
    /// Stage-7 bundle: requests, downloads, flows, imports.
    pub acquire: AcquireSetup,
    /// Stage-8 bundle: library scan, identify, publish, contrib.
    pub library: LibrarySetup,
}

impl AppState {
    /// Wire the state from its parts. Callers pass the production generator
    /// or a fake; nothing here reaches globals. One argument per bundle.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ids: Arc<dyn IdGenerator>,
        http: HttpClientFactory,
        config: AppConfig,
        auth: AuthSetup,
        reads: ReadsSetup,
        providers: Arc<Providers>,
        stage6: Stage6Setup,
        acquire: AcquireSetup,
        library: LibrarySetup,
    ) -> Self {
        Self {
            ids,
            http,
            config,
            auth,
            reads,
            providers,
            stage6,
            acquire,
            library,
        }
    }

    /// Verified per-provider rate policy table.
    pub fn provider_policies(&self) -> &'static [ProviderPolicy] {
        crate::provider_policy::PROVIDER_POLICIES
    }
}
