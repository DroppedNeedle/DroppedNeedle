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

use crate::auth::compat_auth::prod::ProdCompatPasswords;
use crate::auth::users::UsersDeps;
use crate::compat::adapters::engines::{GatewayAudio, GatewayStream};
use crate::compat::adapters::jellyfin_library::{CatalogIds, JellyfinLibrary};
use crate::compat::adapters::library::CompatLibrary;
use crate::compat::adapters::playback::CompatPlayback;
use crate::compat::adapters::principal::SubsonicVerifier;
use crate::compat::adapters::queues::CompatQueues;
use crate::compat::adapters::subsonic_store::SubsonicStore;
use crate::compat::http::{CompatLimits, SubsonicState, cors_layer, limits_layer, subsonic_router};
use crate::compat::jellyfin::{JellyfinState, router as jellyfin_router};
use crate::compat::settings::LiveSettings;
use crate::library::wiring::LibrarySetup;
use crate::media::MediaEngine;
use crate::playback::services::PlaybackDeps;
use crate::reads::ReadsSetup;
use crate::runtime_config::Crypto;

/// Subsonic verifier over the app-password store.
pub type CompatVerifier = SubsonicVerifier<ProdCompatPasswords, UsersDeps>;

/// Subsonic audio over the stream engine.
pub type CompatAudio = GatewayAudio<MediaEngine>;

/// Wired Jellyfin router state.
pub type CompatJellyfinState = crate::compat::jellyfin::JellyfinState<
    ProdCompatPasswords,
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
}

impl CompatDeps {
    /// Deps over the native reads bundle, so compat shares its catalog and
    /// collections.
    pub fn over_reads(
        users: UsersDeps,
        crypto: Arc<Crypto>,
        playback: PlaybackDeps,
        engine: Arc<MediaEngine>,
        scan: LibrarySetup,
        reads: &ReadsSetup,
        settings: LiveSettings,
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
        }
    }
}

/// Everything `create_app` needs to mount the compat routers.
#[derive(Clone)]
pub struct CompatSetup {
    verifier: CompatVerifier,
    store: SubsonicStore,
    audio: CompatAudio,
    jellyfin: CompatJellyfinState,
    settings: LiveSettings,
    limits: CompatLimits,
    router: Router,
}

impl CompatSetup {
    /// Bind the production seams.
    pub fn build(deps: CompatDeps) -> Self {
        let passwords = ProdCompatPasswords::new(deps.users.clone(), deps.crypto);
        let playback = CompatPlayback::new(deps.playback);
        let store = SubsonicStore::new(
            deps.library.clone(),
            deps.queues,
            playback.clone(),
            deps.users.clone(),
            deps.scan,
        );
        let jellyfin = JellyfinState::new(
            passwords.clone(),
            JellyfinLibrary::new(deps.library.clone()),
            GatewayStream::new(Arc::clone(&deps.engine)),
            playback,
            CatalogIds::new(deps.library),
            deps.settings.clone(),
        );
        let verifier = SubsonicVerifier::new(passwords.clone(), deps.users);
        let limits = CompatLimits::new(
            Arc::new(crate::compat::http::StoreLabels::new(passwords)),
            deps.settings.clone(),
        );
        let audio = CompatAudio::new(deps.engine);
        let router = Self::assemble(
            &verifier,
            &store,
            &audio,
            &deps.settings,
            &jellyfin,
            &limits,
        );
        Self {
            verifier,
            store,
            audio,
            jellyfin,
            settings: deps.settings,
            limits,
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
        ids: Arc<dyn crate::ids::IdGenerator>,
    ) -> Result<Self, String> {
        use std::path::PathBuf;

        use crate::compat::jellyfin::seams::JellyfinSettings;
        use crate::compat::subsonic::Settings;
        use crate::playback::fakes::{FakeCatalog, FakeHistory, FakeNames, FakePrefs, FakeSinks};
        use crate::playback::ports::SystemClock;
        use crate::playback::reports::ReportQueue;
        use crate::playback::services::{
            MixedDedup, PresenceRegistry, ScrobbleDedup, SessionStore,
        };
        use crate::remotes::connections::{CredentialCoder, MemoryConnectionStore};
        use crate::remotes::reader::RemotesRemoteReader;
        use crate::stream::gateway::Gateway;
        use crate::stream::transcode::TranscodeSettings;
        use crate::stream::transcode::{FfmpegTranscoder, LocalTranscodeGate, StdFfmpegSpawner};

        let crypto = Arc::new(
            Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| format!("test key: {error}"))?,
        );
        let http = crate::http_client::HttpClientFactory::new()
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
            ids,
        };
        Ok(Self::build(CompatDeps::over_reads(
            users,
            crypto,
            playback,
            engine,
            scan,
            reads,
            LiveSettings::fixed(Settings::default(), JellyfinSettings::default()),
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
        );
        self
    }

    /// Key rate limits and lockouts by the client behind these proxies
    /// (`TRUSTED_PROXY_IPS`).
    #[must_use]
    pub fn with_trusted_proxies(
        mut self,
        trusted: crate::auth::session::middleware::TrustedProxies,
    ) -> Self {
        self.limits = self.limits.with_trusted_proxies(trusted);
        self.router = Self::assemble(
            &self.verifier,
            &self.store,
            &self.audio,
            &self.settings,
            &self.jellyfin,
            &self.limits,
        );
        self
    }

    /// The bound Subsonic store, for tests that dispatch directly.
    #[cfg(any(test, feature = "test-support"))]
    pub fn subsonic_store(&self) -> SubsonicStore {
        self.store.clone()
    }

    /// The bound Jellyfin state, for tests that read the library seam.
    #[cfg(any(test, feature = "test-support"))]
    pub fn jellyfin_state(&self) -> CompatJellyfinState {
        self.jellyfin.clone()
    }

    /// The mounted compat routers with the shared layers, ready to merge
    /// outside the `/api` session gate. Case-variant paths and preflights
    /// never match a route; the app fallbacks redispatch them here via
    /// [`fallback_redispatch`](crate::compat::http::fallback_redispatch).
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    fn assemble(
        verifier: &CompatVerifier,
        store: &SubsonicStore,
        audio: &CompatAudio,
        settings: &LiveSettings,
        jellyfin: &CompatJellyfinState,
        limits: &CompatLimits,
    ) -> Router {
        let subsonic = subsonic_router(SubsonicState::new(
            verifier.clone(),
            store.clone(),
            audio.clone(),
            settings.clone(),
            limits.limits.clone(),
            limits.started.clone(),
        ));
        let jellyfin = Router::new().nest("/jellyfin", jellyfin_router(jellyfin.clone()));
        Router::new()
            .merge(subsonic)
            .merge(jellyfin)
            .layer(from_fn_with_state(limits.clone(), limits_layer))
            .layer(axum::middleware::from_fn(cors_layer))
    }
}
