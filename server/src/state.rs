//! Explicit application state, built once at boot.
//!
//! Every long-lived dependency lives here and reaches handlers through the
//! `State` extractor. External behavior sits behind traits (see the `ids`
//! module) so tests construct the same state with fakes.

use std::sync::Arc;

use crate::{
    acquire::AcquireSetup, admin::AdminSetup, auth::wiring::AuthSetup, compat::CompatSetup,
    concerts::ConcertsSetup, config::AppConfig, events::EventHub, http_client::HttpClientFactory,
    ids::IdGenerator, jobs::wiring::JobsSetup, library::wiring::LibrarySetup, media::MediaSetup,
    plugins::wiring::PluginsSetup, provider_policy::ProviderPolicy, providers::Providers,
    reads::ReadsSetup, settings::wiring::SettingsSetup,
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
    /// Concerts: the feed routes (the jobs bundle runs the sweep).
    pub concerts: ConcertsSetup,
    /// Live event hub behind `/api/v3/events/stream`.
    pub events: EventHub,
}

impl AppState {
    /// Wire the state from its parts. Callers pass the production generator
    /// or a fake; nothing here reaches globals. One argument per bundle.
    /// The state gets its own event hub, attached to every bundle that
    /// publishes; boot swaps in the hub it built with [`Self::with_events`].
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
        concerts: ConcertsSetup,
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
            concerts,
            events: EventHub::new(),
        }
        .attach_events()
    }

    /// Use `hub` for the stream route and every publishing bundle. Boot
    /// builds the hub first so the revision poller (and any bundle built
    /// before the state) shares it.
    pub fn with_events(mut self, hub: EventHub) -> Self {
        self.events = hub;
        self.attach_events()
    }

    /// Point every publishing bundle at this state's hub.
    fn attach_events(self) -> Self {
        self.media.playback.presence.attach_events(&self.events);
        self.acquire.attach_events(&self.events);
        self.library.events.attach(&self.events);
        self.jobs.cache_sync().sink().attach(&self.events);
        self
    }

    /// Verified per-provider rate policy table.
    pub fn provider_policies(&self) -> &'static [ProviderPolicy] {
        crate::provider_policy::PROVIDER_POLICIES
    }
}
