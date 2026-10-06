//! Media bundle: remote sources, stream gateway, playback reporting.
//!
//! [`MediaSetup`] is the single bundle `create_app` mounts: its
//! [`MediaSetup::gated_router`] nests under `/api/v3` inside the
//! deny-by-default session gate, next to the reads nest. Remote
//! connections and Navidrome folder preferences are SQLite rows; the admin
//! servers are read from the config store on every call. Playback catalog,
//! history, prefs, and display names read the SQLite schema through a
//! dedicated rusqlite handle. Remote attribution and scrobble forwarding
//! drain through the [`MediaWorkers`] boot spawns beside the other loops.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;

use crate::auth::users::UsersDeps;
use crate::config::AppConfig;
use crate::db::WriteLane;
use crate::ids::IdGenerator;
use crate::playback::forwarding::{
    Endpoints, ForwardWorker, ScrobbleForwarder, StoredScrobbleCredentials,
};
use crate::playback::ports::SystemClock;
use crate::playback::reports::{ReportQueue, run_report_worker};
use crate::playback::services::{
    MixedDedup, PlaybackDeps, PresenceRegistry, ScrobbleDedup, SessionStore,
};
use crate::playback::sqlite::PlaybackDb;
use crate::plugins::scrobble::SqliteListenBrainzLinkStore;
use crate::remotes::adapter::ImportSink;
use crate::remotes::connections::{
    ConfigServers, ConnectionResolver, CredentialCoder, SqliteConnectionStore,
};
use crate::remotes::folders::SqliteFolderStore;
use crate::remotes::handlers::{RemotesDeps, remotes_router};
use crate::remotes::plex::PlexTokenProbe;
use crate::remotes::reader::RemotesRemoteReader;
use crate::remotes::service::RemotesService;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::crypto::Crypto;
use crate::runtime_config::sections::{AudioFormat, ConnectApps};
use crate::stream::download::{DownloadState, access_from_config, download_routes};
use crate::stream::gateway::Gateway;
use crate::stream::local_files::LibraryFiles;
use crate::stream::routes::{StreamState, stream_routes};
use crate::stream::transcode::{
    FfmpegTranscoder, LocalTranscodeGate, OutFormat, StdFfmpegSpawner, TranscodeSettings,
    ffmpeg_available,
};

/// Production stream engine: gateway over stored-connection remote reads
/// and ffmpeg transcodes behind the shared transcode gate.
pub type MediaEngine =
    Gateway<RemotesRemoteReader, FfmpegTranscoder<StdFfmpegSpawner, Arc<LocalTranscodeGate>>>;

/// Everything `create_app` needs to mount the remote-source, stream and
/// playback routes, built once.
#[derive(Clone)]
pub struct MediaSetup {
    /// Remote-source route deps.
    pub remotes: RemotesDeps,
    /// Stream-gateway state over the production engine.
    pub stream: StreamState<MediaEngine>,
    /// Local file downloads (track files and album archives).
    pub download: DownloadState,
    /// Playback-reporting deps (SQLite catalog/history/prefs/names).
    pub playback: PlaybackDeps,
}

/// The media background drains: remote attribution and scrobble
/// forwarding. Boot spawns [`MediaWorkers::run`] and awaits it after
/// serve; both drains exit once every queue handle in the app drops.
pub struct MediaWorkers {
    reports: tokio::sync::mpsc::Receiver<crate::playback::reports::QueuedReport>,
    http: reqwest::Client,
    resolver: Arc<ConnectionResolver>,
    forwards: ForwardWorker,
}

impl MediaWorkers {
    /// Drain both queues until the app drops its handles.
    pub async fn run(self) {
        tokio::join!(
            run_report_worker(self.reports, self.http, self.resolver),
            self.forwards.run(),
        );
    }
}

impl MediaSetup {
    /// Build the production bundle. `db_path` backs the playback stores;
    /// `pool` and `lane` back the connection and folder rows; `config`
    /// holds the admin's server settings; `connect_apps` carries the
    /// transcode policy; `crypto` seals linked credentials;
    /// `library_roots` plus the catalog in `pool` turn local stream keys
    /// and download ids (track ids) into files under the live library
    /// roots (`None` keeps the constructor fallback for unwired builds and
    /// serves no downloads); `imports` is where remote playlist imports
    /// land.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        db_path: &Path,
        app_config: &AppConfig,
        users: UsersDeps,
        crypto: Arc<Crypto>,
        http: reqwest::Client,
        ids: Arc<dyn IdGenerator>,
        connect_apps: ConnectApps,
        library_roots: Option<crate::library::wiring::RootSource>,
        pool: sqlx::SqlitePool,
        lane: WriteLane,
        config: Arc<ConfigStore>,
        imports: Arc<dyn ImportSink>,
    ) -> Result<(Self, MediaWorkers), String> {
        let library = library_roots.map(|roots| LibraryFiles::new(pool.clone(), roots));
        let download = DownloadState::new(
            library.clone(),
            access_from_config(config.clone()),
            users.clone(),
            ids.clone(),
        );
        let resolver = Arc::new(
            ConnectionResolver::new(
                Arc::new(SqliteConnectionStore::new(pool.clone(), lane.clone())),
                Arc::new(CredentialCoder::new(crypto.clone())),
                Arc::new(ConfigServers::new(config)),
            )
            .with_plex_tokens(Arc::new(PlexTokenProbe::new(http.clone()))),
        );
        let credentials = Arc::new(StoredScrobbleCredentials::new(
            Arc::new(SqliteListenBrainzLinkStore::new(
                pool.clone(),
                lane.clone(),
                crypto.clone(),
            )),
            users.lastfm.clone(),
            users.lastfm_switch.clone(),
            crypto,
        ));
        let service = RemotesService::new(
            http.clone(),
            resolver.clone(),
            Arc::new(SqliteFolderStore::new(pool, lane)),
            imports,
        );
        let remotes = RemotesDeps {
            service,
            auth: users,
            ids: ids.clone(),
        };
        let reader = RemotesRemoteReader::with_resolver(http.clone(), resolver.clone());
        let spawner = StdFfmpegSpawner::detect()
            .unwrap_or_else(|| StdFfmpegSpawner::with_path(PathBuf::from("ffmpeg")));
        let transcoder = FfmpegTranscoder::new(spawner, Arc::new(LocalTranscodeGate::new()));
        let gateway = Gateway::new(
            local_root(app_config),
            reader,
            transcoder,
            transcode_settings(&connect_apps),
            ffmpeg_available(),
        );
        let engine = match library {
            Some(library) => gateway.with_library(library),
            None => gateway,
        };
        let stream = StreamState {
            engine: Arc::new(engine),
            ids: ids.clone(),
        };
        let db = Arc::new(PlaybackDb::open(db_path, ids.clone())?);
        let (queue, rx) = ReportQueue::channel();
        let (forwarder, forwards) =
            ScrobbleForwarder::channel(db.clone(), remotes.auth.lastfm_switch.clone());
        let forwards =
            ForwardWorker::new(forwards, http.clone(), credentials, Endpoints::default());
        let playback = PlaybackDeps {
            catalog: db.clone(),
            sinks: Arc::new(forwarder),
            remotes: Arc::new(queue),
            history: db.clone(),
            prefs: db.clone(),
            names: db,
            sessions: SessionStore::new(),
            presence: PresenceRegistry::new(),
            dedup: ScrobbleDedup::new(),
            mixed: MixedDedup::new(),
            clock: Arc::new(SystemClock),
            ids: ids.clone(),
            events: std::sync::Arc::new(crate::playback::ports::NoPlayEvents),
        };
        let worker = MediaWorkers {
            reports: rx,
            http,
            resolver,
            forwards,
        };
        Ok((
            Self {
                remotes,
                stream,
                download,
                playback,
            },
            worker,
        ))
    }

    /// Test bundle over memory stores and fakes. No admin server is
    /// configured, the catalog is empty (so reads 404), attribution drops,
    /// and ffmpeg is absent, so every transcode decision lands direct.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(users: UsersDeps, ids: Arc<dyn IdGenerator>) -> Result<Self, String> {
        use crate::playback::fakes::{FakeCatalog, FakeHistory, FakeNames, FakePrefs, FakeSinks};
        use crate::remotes::adapter::MemoryImportSink;
        use crate::remotes::connections::{MemoryConnectionStore, NoServers};
        use crate::remotes::folders::MemoryFolderStore;

        let http = crate::http_client::HttpClientFactory::new()
            .map_err(|error| format!("test media http: {error}"))?
            .shared()
            .clone();
        let crypto = Arc::new(
            Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| format!("test key: {error}"))?,
        );
        let resolver = Arc::new(ConnectionResolver::new(
            Arc::new(MemoryConnectionStore::new()),
            Arc::new(CredentialCoder::new(crypto)),
            Arc::new(NoServers),
        ));
        // No library wired: downloads answer 404.
        let download = DownloadState::new(
            None,
            Arc::new(|| Ok(crate::runtime_config::sections::SecuritySettings::default())),
            users.clone(),
            ids.clone(),
        );
        let remotes = RemotesDeps {
            service: RemotesService::new(
                http.clone(),
                resolver.clone(),
                Arc::new(MemoryFolderStore::new()),
                Arc::new(MemoryImportSink::new()),
            ),
            auth: users,
            ids: ids.clone(),
        };
        let reader = RemotesRemoteReader::with_resolver(http, resolver);
        let transcoder = FfmpegTranscoder::new(
            StdFfmpegSpawner::with_path(PathBuf::from("ffmpeg")),
            Arc::new(LocalTranscodeGate::new()),
        );
        let engine = Gateway::new(
            PathBuf::from("unmounted-test-music"),
            reader,
            transcoder,
            TranscodeSettings::default(),
            false,
        );
        Ok(Self {
            remotes,
            stream: StreamState {
                engine: Arc::new(engine),
                ids: ids.clone(),
            },
            download,
            playback: PlaybackDeps {
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
                events: std::sync::Arc::new(crate::playback::ports::NoPlayEvents),
            },
        })
    }

    /// Send accepted plays to listeners (the plugin host).
    pub fn with_play_events(mut self, events: Arc<dyn crate::playback::ports::PlayEvents>) -> Self {
        self.playback.events = events;
        self
    }

    /// Relative-path routers for nesting under `/api/v3` inside the
    /// session gate.
    pub fn gated_router(&self) -> Router {
        Router::new()
            .merge(remotes_router(self.remotes.clone()))
            .merge(stream_routes(self.stream.clone()))
            .merge(download_routes(self.download.clone()))
            .merge(crate::playback::playback_router(self.playback.clone()))
    }
}

/// Map the `connect_apps` section onto the transcode policy. `Flac` cannot
/// arrive here (section validation rejects it), so it falls back to Mp3.
fn transcode_settings(connect_apps: &ConnectApps) -> TranscodeSettings {
    TranscodeSettings {
        transcoding_enabled: connect_apps.transcoding_enabled,
        default_format: match connect_apps.transcode_default_format {
            AudioFormat::Opus => OutFormat::Opus,
            AudioFormat::Mp3 | AudioFormat::Flac => OutFormat::Mp3,
        },
        max_bitrate_kbps: connect_apps.transcode_max_bitrate_kbps,
    }
}

/// Local-stream sandbox root for unwired builds (`None` roots, test
/// bundles): local keys resolve under `<root>/music`, which is
/// normally absent, so local reads 404. Wired builds
/// resolve against the live library registry instead.
fn local_root(config: &AppConfig) -> PathBuf {
    config.root_app_dir.join("music")
}
