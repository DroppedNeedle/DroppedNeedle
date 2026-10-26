//! Explicit application state, built once at boot.
//!
//! Every long-lived dependency lives here and reaches handlers through the
//! `State` extractor. External behavior sits behind traits (see the `ids`
//! module) so tests construct the same state with fakes.

use std::sync::Arc;

use crate::{admin::AdminSetup, bootstrap::jobs::JobsSetup, settings::wiring::SettingsSetup};
use droppedneedle_acquire::AcquireSetup;
use droppedneedle_auth::wiring::AuthSetup;
use droppedneedle_catalog::{concerts::ConcertsSetup, reads::ReadsSetup};
use droppedneedle_compat::CompatSetup;
use droppedneedle_kernel::{config::AppConfig, events::EventHub, ids::IdGenerator};
use droppedneedle_library::wiring::{LibraryServices, LibrarySetup};
use droppedneedle_media::MediaSetup;
use droppedneedle_plugins::wiring::PluginsSetup;

/// What the router and the outer layers serve from: one bundle per area,
/// the request ids, the deployment config and the event hub.
#[derive(Clone)]
pub struct AppState {
    /// Fresh request and error ids.
    pub ids: Arc<dyn IdGenerator>,
    /// Deployment configuration.
    pub config: AppConfig,
    /// Auth bundle: stores, gates, and route states for `/api/v3`.
    pub auth: AuthSetup,
    /// Reads bundle: library, search, discover, collections, platform.
    pub reads: ReadsSetup,
    /// Remote sources, stream gateway, playback reporting.
    pub media: MediaSetup,
    /// Acquisition: requests, downloads, flows, imports.
    pub acquire: AcquireSetup,
    /// Library: scan, identify, publish, contrib.
    pub library: LibrarySetup,
    /// The library's services, built once over `library`: the library
    /// router's state.
    pub library_services: LibraryServices,
    /// Subsonic/Jellyfin compat (outside the `/api` gate).
    pub compat: CompatSetup,
    /// Admin routes, the latest checkpoint pass among them.
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
    /// or a fake; nothing here reaches globals. One argument per bundle,
    /// and the platform's event hub, which every publishing bundle already
    /// holds.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ids: Arc<dyn IdGenerator>,
        config: AppConfig,
        auth: AuthSetup,
        reads: ReadsSetup,
        media: MediaSetup,
        acquire: AcquireSetup,
        library: LibraryServices,
        compat: CompatSetup,
        admin: AdminSetup,
        settings: SettingsSetup,
        jobs: JobsSetup,
        plugins: PluginsSetup,
        concerts: ConcertsSetup,
        events: EventHub,
    ) -> Self {
        Self {
            ids,
            config,
            auth,
            reads,
            media,
            acquire,
            library: library.setup.clone(),
            library_services: library,
            compat,
            admin,
            settings,
            jobs,
            plugins,
            concerts,
            events,
        }
    }
}
