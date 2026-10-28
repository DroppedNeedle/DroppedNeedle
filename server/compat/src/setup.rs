//! Compat bundle: the adapters both protocols call plus the mounted
//! routers.
//!
//! [`CompatSetup`] is the one `AppState` field compat adds. It binds auth
//! to the app-password store ([`ProdCompatPasswords`]), the library to the
//! v3 catalog and the shared collections ([`CompatLibrary`]), queues and
//! bookmarks to SQLite, playback to the reporting services and streaming to
//! the stream engine. Settings (kill switches, names, transcode policy)
//! are read per request ([`LiveSettings`]). Both protocols register through
//! the kernel's compat mount, which records their path templates for case
//! fixing, and serve outside the `/api` session gate with the shared layers
//! (CORS, limits).
//!
//! Stream leases count against the authenticated caller on both protocols.

use std::sync::Arc;

use axum::Router;
use axum::middleware::from_fn_with_state;

use crate::adapters::audio::GatewayAudio;
use crate::adapters::jellyfin_ids::{JellyfinIds, RESOLVE_WAIT};
use crate::adapters::jellyfin_library::{CatalogIds, JellyfinLibrary};
use crate::adapters::library::CompatLibrary;
use crate::adapters::playback::CompatPlayback;
use crate::adapters::principal::SubsonicVerifier;
use crate::adapters::queues::CompatQueues;
use crate::adapters::subsonic_store::SubsonicStore;
use crate::http::{CompatLimits, SubsonicState, cors_layer, limits_layer, subsonic_routes};
use crate::jellyfin::{JellyfinState, routes as jellyfin_routes};
use crate::settings::LiveSettings;
use crate::shared::address::ListenerConfig;
use droppedneedle_auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_catalog::reads::ReadsSetup;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::{AlreadyRunning, Supervisor};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::Crypto;
use droppedneedle_library::scans::Scans;
use droppedneedle_media::MediaEngine;
use droppedneedle_media::playback::PlaybackDeps;

/// Subsonic audio over the stream engine.
pub type CompatAudio = GatewayAudio<MediaEngine>;

/// What the compat bundle is built from.
pub struct CompatDeps {
    /// The database, for the stored Jellyfin ids.
    pub db: Db,
    /// Users and avatars.
    pub users: UsersDeps,
    /// Key for the app-password store.
    pub crypto: Arc<Crypto>,
    /// Playback reporting.
    pub playback: PlaybackDeps,
    /// The stream engine the native routes use.
    pub engine: Arc<MediaEngine>,
    /// Scan status and triggers.
    pub scan: Scans,
    /// Catalog, collections, lyrics and covers.
    pub library: CompatLibrary,
    /// Saved queues and bookmarks.
    pub queues: CompatQueues,
    /// Settings source.
    pub settings: LiveSettings,
    /// Port and trusted proxies of this server.
    pub listener: ListenerConfig,
}

impl CompatDeps {
    /// Deps over the native reads bundle, so compat shares its catalog and
    /// collections; the data key, the live settings and this server's port
    /// and proxies come from `platform`.
    pub fn over_reads(
        platform: &Platform,
        users: UsersDeps,
        playback: PlaybackDeps,
        engine: Arc<MediaEngine>,
        scan: Scans,
        reads: &ReadsSetup,
    ) -> Self {
        let config = &platform.config;
        let settings = LiveSettings::from_config(&platform.settings, engine.can_transcode());
        Self {
            db: platform.db.clone(),
            users,
            crypto: platform.crypto.clone(),
            playback,
            engine,
            scan,
            library: CompatLibrary::from_reads(reads),
            queues: CompatQueues::new(reads.collections.db.clone()),
            settings,
            listener: ListenerConfig {
                port: config.port,
                trusted_proxies: config.trusted_proxies.clone(),
                base_path: config.base_path.clone(),
            },
        }
    }
}

/// Everything `create_app` needs to mount the compat routers.
#[derive(Clone)]
pub struct CompatSetup {
    ids: JellyfinIds,
    settings: LiveSettings,
    router: Router,
    templates: Arc<[&'static str]>,
}

impl CompatSetup {
    /// Build the adapters and mount both routers.
    pub fn build(deps: CompatDeps) -> Self {
        let passwords = ProdCompatPasswords::new(deps.users.clone(), Arc::clone(&deps.crypto));
        let playback = CompatPlayback::new(deps.playback);
        let store = SubsonicStore::new(
            deps.library.clone(),
            deps.queues,
            playback.clone(),
            deps.users.clone(),
            deps.scan,
            Arc::clone(&deps.crypto),
            deps.settings.clone(),
        );
        let ids = JellyfinIds::new(deps.db, Arc::clone(&deps.crypto), RESOLVE_WAIT);
        let jellyfin = JellyfinState::new(
            passwords.clone(),
            JellyfinLibrary::new(deps.library.clone()),
            GatewayAudio::new(Arc::clone(&deps.engine)),
            playback,
            CatalogIds::new(ids.clone(), deps.library.catalog.clone()),
            deps.settings.clone(),
            deps.listener.clone(),
        );
        let verifier = SubsonicVerifier::new(passwords.clone(), deps.users);
        // Rate limits key the client behind these proxies (`TRUSTED_PROXY_IPS`).
        let limits = CompatLimits::new(passwords, deps.settings.clone())
            .with_trusted_proxies(deps.listener.trusted_proxies.clone());
        let audio = CompatAudio::new(deps.engine);
        let (router, templates) = Self::assemble(
            &verifier,
            &store,
            &audio,
            &deps.settings,
            &jellyfin,
            &limits,
            &deps.listener,
        );
        Self {
            ids,
            settings: deps.settings,
            router,
            templates,
        }
    }

    /// Start compat's loops ([`crate::LOOPS`]) on `supervisor`: the fill of
    /// the stored Jellyfin ids.
    pub fn start_loops(&self, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
        self.ids.start(supervisor)
    }

    /// The stored Jellyfin ids, for a caller that waits for the fill.
    pub fn jellyfin_ids(&self) -> &JellyfinIds {
        &self.ids
    }

    /// The mounted compat routers with the shared layers, ready to merge
    /// outside the `/api` session gate. The app answers preflights with
    /// [`preflight`](crate::http::preflight) and fixes path case with
    /// [`canonicalize_case`](crate::http::canonicalize_case) first.
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// The path template of every compat route, in registration order, for
    /// [`canonicalize_case`](crate::http::canonicalize_case).
    pub fn templates(&self) -> Arc<[&'static str]> {
        Arc::clone(&self.templates)
    }

    /// The compat answer to a 5xx on `path`, with the Subsonic settings
    /// read now. See [`server_error_response`](crate::http::server_error_response).
    pub fn server_error(
        &self,
        path: &str,
        head_only: bool,
        raw_query: Option<&str>,
        status: axum::http::StatusCode,
        retry_after: Option<axum::http::HeaderValue>,
    ) -> axum::response::Response {
        crate::http::server_error_response(
            path,
            head_only,
            raw_query,
            status,
            retry_after,
            &self.settings.subsonic(),
        )
    }

    fn assemble(
        verifier: &SubsonicVerifier,
        store: &SubsonicStore,
        audio: &CompatAudio,
        settings: &LiveSettings,
        jellyfin: &JellyfinState,
        limits: &CompatLimits,
        listener: &ListenerConfig,
    ) -> (Router, Arc<[&'static str]>) {
        let (subsonic, mut templates) = subsonic_routes(SubsonicState::new(
            verifier.clone(),
            store.clone(),
            audio.clone(),
            settings.clone(),
            limits.limits.clone(),
            limits.started.clone(),
            listener.clone(),
            store.clock().clone(),
        ))
        .into_parts();
        let (jellyfin, jellyfin_templates) = jellyfin_routes(jellyfin.clone()).into_parts();
        templates.extend(jellyfin_templates);
        let router = Router::new()
            .merge(subsonic)
            .merge(jellyfin)
            .layer(from_fn_with_state(limits.clone(), limits_layer))
            .layer(axum::middleware::from_fn(cors_layer));
        (router, templates.into())
    }
}
