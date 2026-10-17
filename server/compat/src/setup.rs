//! Compat bundle: production seam bindings plus the mounted routers.
//!
//! [`CompatSetup`] is the one `AppState` field compat adds. It binds auth
//! to the app-password store ([`ProdCompatPasswords`]), the library to the
//! v3 catalog and the shared collections ([`CompatLibrary`]), queues and
//! bookmarks to SQLite, playback to the reporting services and streaming to
//! the stream engine. Settings (kill switches, names, transcode policy)
//! are read per request ([`LiveSettings`]). Both routers mount outside the
//! `/api` session gate with the shared layers (case, CORS, limits).
//!
//! Stream leases count against the authenticated caller on both protocols.

use std::sync::Arc;

use axum::Router;
use axum::middleware::from_fn_with_state;

use crate::adapters::engines::{GatewayAudio, GatewayStream};
use crate::adapters::jellyfin_library::{CatalogIds, JellyfinLibrary, REBUILD_COOLDOWN};
use crate::adapters::library::CompatLibrary;
use crate::adapters::playback::CompatPlayback;
use crate::adapters::principal::SubsonicVerifier;
use crate::adapters::queues::CompatQueues;
use crate::adapters::subsonic_store::SubsonicStore;
use crate::http::{CompatLimits, SubsonicState, cors_layer, limits_layer, subsonic_router};
use crate::jellyfin::{JellyfinState, router as jellyfin_router};
use crate::settings::LiveSettings;
use crate::shared::address::ListenerConfig;
use droppedneedle_auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_catalog::reads::ReadsSetup;
use droppedneedle_kernel::runtime_config::Crypto;
use droppedneedle_library::wiring::LibrarySetup;
use droppedneedle_media::MediaEngine;
use droppedneedle_media::playback::services::PlaybackDeps;

/// Subsonic audio over the stream engine.
pub type CompatAudio = GatewayAudio<MediaEngine>;

/// Wired Jellyfin router state.
pub type CompatJellyfinState = crate::jellyfin::JellyfinState<
    JellyfinLibrary,
    GatewayStream<MediaEngine>,
    CompatPlayback,
    CatalogIds,
>;

/// What the compat bundle is built from.
pub struct CompatDeps {
    /// Users and avatars.
    pub users: UsersDeps,
    /// Key for the app-password store.
    pub crypto: Arc<Crypto>,
    /// Playback reporting.
    pub playback: PlaybackDeps,
    /// The stream engine the native routes use.
    pub engine: Arc<MediaEngine>,
    /// Scan status and triggers.
    pub scan: LibrarySetup,
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
    /// collections.
    #[allow(clippy::too_many_arguments)]
    pub fn over_reads(
        users: UsersDeps,
        crypto: Arc<Crypto>,
        playback: PlaybackDeps,
        engine: Arc<MediaEngine>,
        scan: LibrarySetup,
        reads: &ReadsSetup,
        settings: LiveSettings,
        listener: ListenerConfig,
    ) -> Self {
        Self {
            users,
            crypto,
            playback,
            engine,
            scan,
            library: CompatLibrary::from_reads(reads),
            queues: CompatQueues::new(reads.collections.db.clone()),
            settings,
            listener,
        }
    }
}

/// Everything `create_app` needs to mount the compat routers.
#[derive(Clone)]
pub struct CompatSetup {
    verifier: SubsonicVerifier,
    store: SubsonicStore,
    audio: CompatAudio,
    jellyfin: CompatJellyfinState,
    settings: LiveSettings,
    limits: CompatLimits,
    listener: ListenerConfig,
    router: Router,
}

impl CompatSetup {
    /// Bind the production seams.
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
        let jellyfin = JellyfinState::new(
            passwords.clone(),
            JellyfinLibrary::new(deps.library.clone()),
            GatewayStream::new(Arc::clone(&deps.engine)),
            playback,
            CatalogIds::new(deps.library, deps.crypto, REBUILD_COOLDOWN),
            deps.settings.clone(),
            deps.listener.clone(),
        );
        let verifier = SubsonicVerifier::new(passwords.clone(), deps.users);
        // Rate limits key the client behind these proxies (`TRUSTED_PROXY_IPS`).
        let limits = CompatLimits::new(
            Arc::new(crate::http::StoreLabels::new(passwords)),
            deps.settings.clone(),
        )
        .with_trusted_proxies(deps.listener.trusted_proxies.clone());
        let audio = CompatAudio::new(deps.engine);
        let router = Self::assemble(
            &verifier,
            &store,
            &audio,
            &deps.settings,
            &jellyfin,
            &limits,
            &deps.listener,
        );
        Self {
            verifier,
            store,
            audio,
            jellyfin,
            settings: deps.settings,
            limits,
            listener: deps.listener,
            router,
        }
    }

    /// Minimal bundle for unit-style app tests: real seam types, an unused
    /// engine over an empty root, both protocols off.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(
        users: UsersDeps,
        scan: LibrarySetup,
        reads: &ReadsSetup,
    ) -> Result<Self, String> {
        use std::path::PathBuf;

        use crate::jellyfin::seams::JellyfinSettings;
        use crate::subsonic::Settings;
        use droppedneedle_media::playback::fakes::{
            FakeCatalog, FakeHistory, FakeNames, FakePrefs, FakeSinks,
        };
        use droppedneedle_media::playback::ports::SystemClock;
        use droppedneedle_media::playback::reports::ReportQueue;
        use droppedneedle_media::playback::services::{
            MixedDedup, PresenceRegistry, ScrobbleDedup, SessionStore,
        };
        use droppedneedle_media::remotes::connections::{CredentialCoder, MemoryConnectionStore};
        use droppedneedle_media::remotes::reader::RemotesRemoteReader;
        use droppedneedle_media::stream::gateway::Gateway;
        use droppedneedle_media::stream::transcode::TranscodeSettings;
        use droppedneedle_media::stream::transcode::{
            FfmpegTranscoder, LocalTranscodeGate, StdFfmpegSpawner,
        };

        let crypto = Arc::new(
            Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| format!("test key: {error}"))?,
        );
        let http = droppedneedle_kernel::http_client::HttpClientFactory::new()
            .map_err(|error| format!("test compat http: {error}"))?
            .shared()
            .clone();
        let connections: Arc<MemoryConnectionStore> = Arc::new(MemoryConnectionStore::new());
        let reader = RemotesRemoteReader::new(
            http,
            connections,
            Arc::new(CredentialCoder::new(crypto.clone())),
        );
        let engine = Arc::new(Gateway::new(
            PathBuf::from("unmounted-compat-test-music"),
            reader,
            FfmpegTranscoder::new(
                StdFfmpegSpawner::with_path(PathBuf::from("ffmpeg")),
                Arc::new(LocalTranscodeGate::new()),
            ),
            TranscodeSettings::default(),
            false,
        ));
        let playback = PlaybackDeps {
            catalog: Arc::new(FakeCatalog::with_tracks(Vec::new())),
            sinks: Arc::new(FakeSinks::unlinked()),
            remotes: Arc::new(ReportQueue::detached()),
            history: Arc::new(FakeHistory::default()),
            prefs: Arc::new(FakePrefs::default()),
            names: Arc::new(FakeNames::default()),
            sessions: SessionStore::new(),
            presence: PresenceRegistry::new(),
            dedup: ScrobbleDedup::new(),
            mixed: MixedDedup::new(),
            clock: Arc::new(SystemClock),
            events: std::sync::Arc::new(droppedneedle_media::playback::ports::NoPlayEvents),
        };
        Ok(Self::build(CompatDeps::over_reads(
            users,
            crypto,
            playback,
            engine,
            scan,
            reads,
            LiveSettings::fixed(Settings::default(), JellyfinSettings::default()),
            ListenerConfig {
                port: droppedneedle_kernel::config::DEFAULT_PORT,
                trusted_proxies: droppedneedle_kernel::config::TrustedProxies::default(),
                base_path: String::new(),
            },
        )))
    }

    /// Enable one or both protocols with fixed settings (wiring tests;
    /// production reads the config per request).
    pub fn with_enabled(mut self, subsonic: bool, jellyfin: bool) -> Self {
        let mut subsonic_settings = self.settings.subsonic();
        subsonic_settings.enabled = subsonic;
        let mut jellyfin_settings = self.settings.jellyfin();
        jellyfin_settings.enabled = jellyfin;
        self.settings = LiveSettings::fixed(subsonic_settings, jellyfin_settings);
        self.jellyfin.settings = self.settings.clone();
        self.limits.settings = self.settings.clone();
        self.router = Self::assemble(
            &self.verifier,
            &self.store,
            &self.audio,
            &self.settings,
            &self.jellyfin,
            &self.limits,
            &self.listener,
        );
        self
    }

    /// The mounted compat routers with the shared layers, ready to merge
    /// outside the `/api` session gate. Case-variant paths and preflights
    /// never match a route; the app fallbacks redispatch them here via
    /// [`fallback_redispatch`](crate::http::fallback_redispatch).
    pub fn router(&self) -> Router {
        self.router.clone()
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
        jellyfin: &CompatJellyfinState,
        limits: &CompatLimits,
        listener: &ListenerConfig,
    ) -> Router {
        let subsonic = subsonic_router(SubsonicState::new(
            verifier.clone(),
            store.clone(),
            audio.clone(),
            settings.clone(),
            limits.limits.clone(),
            limits.started.clone(),
            listener.clone(),
        ));
        let jellyfin = Router::new().nest("/jellyfin", jellyfin_router(jellyfin.clone()));
        Router::new()
            .merge(subsonic)
            .merge(jellyfin)
            .layer(from_fn_with_state(limits.clone(), limits_layer))
            .layer(axum::middleware::from_fn(cors_layer))
    }
}
