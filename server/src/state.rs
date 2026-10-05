//! Explicit application state, built once at boot.
//!
//! Every long-lived dependency lives here and reaches handlers through the
//! `State` extractor. External behavior sits behind traits (see the `ids`
//! module) so tests construct the same state with fakes.

use std::sync::Arc;

use crate::{
    acquire::AcquireSetup, admin::AdminSetup, auth::wiring::AuthSetup, compat::CompatSetup,
    config::AppConfig, http_client::HttpClientFactory, ids::IdGenerator, jobs::wiring::JobsSetup,
    library::wiring::LibrarySetup, media::MediaSetup, plugins::wiring::PluginsSetup,
    provider_policy::ProviderPolicy, providers::Providers, reads::ReadsSetup,
    settings::wiring::SettingsSetup,
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
    /// Remote sources, stream gateway, playback reporting.
    pub media: MediaSetup,
    /// Acquisition: requests, downloads, flows, imports.
    pub acquire: AcquireSetup,
    /// Library: scan, identify, publish, contrib.
    pub library: LibrarySetup,
    /// Subsonic/Jellyfin compat (outside the `/api` gate).
    pub compat: CompatSetup,
    /// Admin routes plus checkpoint health.
    pub admin: AdminSetup,
    /// Settings: section service with save effects.
    pub settings: SettingsSetup,
    /// Jobs: registry, playlist route, boot loops.
    pub jobs: JobsSetup,
    /// Plugins: host, routes, scrobble backend, ticks.
    pub plugins: PluginsSetup,
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
        media: MediaSetup,
        acquire: AcquireSetup,
        library: LibrarySetup,
        compat: CompatSetup,
        admin: AdminSetup,
        settings: SettingsSetup,
        jobs: JobsSetup,
        plugins: PluginsSetup,
    ) -> Self {
        Self {
            ids,
            http,
            config,
            auth,
            reads,
            providers,
            media,
            acquire,
            library,
            compat,
            admin,
            settings,
            jobs,
            plugins,
        }
    }

    /// Verified per-provider rate policy table.
    pub fn provider_policies(&self) -> &'static [ProviderPolicy] {
        crate::provider_policy::PROVIDER_POLICIES
    }
}
