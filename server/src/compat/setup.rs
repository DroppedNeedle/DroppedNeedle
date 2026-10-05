//! Compat bundle: production seam bindings plus the mounted routers.
//!
//! [`CompatSetup`] is the one `AppState` field compat adds. It binds auth
//! to the app-password store ([`ProdCompatPasswords`](crate::auth::compat_auth::prod::ProdCompatPasswords)),
//! playback to the reporting services, and streaming to the stream engine;
//! the library store is the in-memory [`MemoryStore`] until it is joined to
//! the v3 catalog (see its docs). Both routers mount outside the `/api`
//! session gate with the shared layers (case, CORS, limits) and kill
//! switches default off.
//!
//! Lease principals: Subsonic media is always authed but the audio seam
//! carries no caller, so leases run under the fixed `compat:subsonic`
//! principal; Jellyfin audio (anonymous by design) runs under
//! `compat:jellyfin`. Per-user lease fairness needs a seam parameter and
//! is a recorded follow-up.

use std::sync::Arc;

use axum::Router;
use axum::middleware::from_fn_with_state;

use crate::auth::compat_auth::prod::ProdCompatPasswords;
use crate::auth::users::UsersDeps;
use crate::compat::adapters::empty::MemoryStore;
use crate::compat::adapters::engines::{GatewayAudio, GatewayStream};
use crate::compat::adapters::playback::CompatPlayback;
use crate::compat::adapters::principal::SubsonicVerifier;
use crate::compat::adapters::queues::CompatQueues;
use crate::compat::http::{CompatLimits, SubsonicState, cors_layer, limits_layer, subsonic_router};
use crate::compat::jellyfin::seams::{JellyfinSettings, MemoryIds, MemoryLibrary};
use crate::compat::jellyfin::{JellyfinState, router as jellyfin_router};
use crate::compat::subsonic::Settings as SubsonicSettings;
use crate::library::wiring::LibrarySetup;
use crate::media::MediaEngine;
use crate::playback::services::PlaybackDeps;
use crate::runtime_config::Crypto;
use crate::runtime_config::sections::{AudioFormat, ConnectApps};

/// Subsonic verifier over the app-password store.
pub type CompatVerifier = SubsonicVerifier<ProdCompatPasswords, UsersDeps>;

/// Subsonic audio over the stream engine.
pub type CompatAudio = GatewayAudio<MediaEngine>;

/// Wired Jellyfin router state.
pub type CompatJellyfinState = crate::compat::jellyfin::JellyfinState<
    ProdCompatPasswords,
    MemoryLibrary,
    GatewayStream<MediaEngine>,
    CompatPlayback,
    MemoryIds,
>;

/// Everything `create_app` needs to mount the compat routers.
#[derive(Clone)]
pub struct CompatSetup {
    verifier: CompatVerifier,
    store: MemoryStore,
    audio: CompatAudio,
    jellyfin: CompatJellyfinState,
    subsonic_settings: SubsonicSettings,
    limits: CompatLimits,
    router: Router,
}

impl CompatSetup {
    /// Bind the production seams.
    pub fn build(
        users: UsersDeps,
        crypto: Arc<Crypto>,
        playback_deps: PlaybackDeps,
        engine: Arc<MediaEngine>,
        scan: LibrarySetup,
        connect_apps: &ConnectApps,
    ) -> Self {
        let passwords = ProdCompatPasswords::new(users.clone(), crypto);
        let playback = CompatPlayback::new(playback_deps);
        let store = MemoryStore::new(CompatQueues::new(), playback.clone(), users.clone(), scan);
        let subsonic_settings = SubsonicSettings {
            enabled: connect_apps.subsonic_enabled,
            server_name: connect_apps.advertise_server_name.clone(),
            server_version: connect_apps.advertise_server_version.clone(),
            transcoding_enabled: connect_apps.transcoding_enabled,
            transcode_default_format: match connect_apps.transcode_default_format {
                AudioFormat::Opus => "opus".to_owned(),
                AudioFormat::Mp3 | AudioFormat::Flac => "mp3".to_owned(),
            },
            transcode_max_bitrate_kbps: connect_apps.transcode_max_bitrate_kbps,
            ffmpeg_available: crate::stream::transcode::ffmpeg_available(),
            base_url: String::new(),
        };
        let jellyfin = JellyfinState::new(
            passwords.clone(),
            MemoryLibrary::new(),
            GatewayStream::new(Arc::clone(&engine)),
            playback,
            MemoryIds::new(),
            JellyfinSettings {
                enabled: connect_apps.jellyfin_enabled,
                server_name: connect_apps.advertise_server_name.clone(),
                server_version: connect_apps.advertise_server_version.clone(),
                transcoding_enabled: connect_apps.transcoding_enabled,
                transcode_max_bitrate_kbps: connect_apps.transcode_max_bitrate_kbps.clamp(32, 1411)
                    as u32,
                transcode_default_format: match connect_apps.transcode_default_format {
                    AudioFormat::Opus => "opus".to_owned(),
                    AudioFormat::Mp3 | AudioFormat::Flac => "mp3".to_owned(),
                },
                ffmpeg_available: crate::stream::transcode::ffmpeg_available(),
            },
        );
        let verifier = SubsonicVerifier::new(passwords.clone(), users);
        let limits = CompatLimits::new(
            Arc::new(crate::compat::http::StoreLabels::new(passwords.clone())),
            subsonic_settings.clone(),
        );
        let audio = CompatAudio::new(engine, "compat:subsonic".to_owned());
        let router = Self::assemble(
            &verifier,
            &store,
            &audio,
            &subsonic_settings,
            &jellyfin,
            &limits,
        );
        Self {
            verifier,
            store,
            audio,
            jellyfin,
            subsonic_settings,
            limits,
            router,
        }
    }

    /// Minimal bundle for unit-style app tests: real seam types, an unused
    /// engine over an empty root, kill switches OFF. Tests that serve
    /// compat use [`CompatSetup::with_enabled`] plus fixture routers.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(
        users: UsersDeps,
        scan: LibrarySetup,
        ids: Arc<dyn crate::ids::IdGenerator>,
    ) -> Result<Self, String> {
        use std::path::PathBuf;

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
        let playback_deps = PlaybackDeps {
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
        Ok(Self::build(
            users,
            crypto,
            playback_deps,
            engine,
            scan,
            &ConnectApps::default(),
        ))
    }

    /// Enable one or both protocols (wiring tests; production reads config).
    pub fn with_enabled(mut self, subsonic: bool, jellyfin: bool) -> Self {
        self.subsonic_settings.enabled = subsonic;
        self.jellyfin.settings.enabled = jellyfin;
        self.limits.settings = self.subsonic_settings.clone();
        self.router = Self::assemble(
            &self.verifier,
            &self.store,
            &self.audio,
            &self.subsonic_settings,
            &self.jellyfin,
            &self.limits,
        );
        self
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
        store: &MemoryStore,
        audio: &CompatAudio,
        subsonic_settings: &SubsonicSettings,
        jellyfin: &CompatJellyfinState,
        limits: &CompatLimits,
    ) -> Router {
        let subsonic = subsonic_router(SubsonicState::new(
            verifier.clone(),
            store.clone(),
            audio.clone(),
            subsonic_settings.clone(),
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
