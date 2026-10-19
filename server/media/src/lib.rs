//! Media bundle: remote sources, stream gateway, playback reporting.
//!
//! [`MediaSetup`] is the single bundle `create_app` mounts [`routes`] over,
//! all of them in the user group inside the session gate. Remote
//! connections and Navidrome folder preferences are SQLite rows; the admin
//! servers are read from the config store on every call. Playback catalog,
//! history, prefs, and display names read the SQLite schema through a
//! dedicated rusqlite handle. Remote attribution and scrobble forwarding
//! drain through [`MediaWorkers`], one job on the loop supervisor.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod playback;
pub mod remotes;
pub mod stream;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::FromRef;

use crate::playback::forwarding::{ForwardWorker, ScrobbleForwarder, StoredScrobbleCredentials};
use crate::playback::reports::{ReportQueue, run_report_worker};
use crate::playback::services::{
    MixedDedup, PlaybackDeps, PresenceRegistry, ScrobbleDedup, SessionStore,
};
use crate::playback::sqlite::PlaybackDb;
use crate::remotes::adapter::ImportSink;
use crate::remotes::connections::{
    ConfigServers, ConnectionResolver, CredentialCoder, SqliteConnectionStore,
};
use crate::remotes::folders::SqliteFolderStore;
use crate::remotes::handlers::RemotesDeps;
use crate::remotes::plex::PlexTokenProbe;
use crate::remotes::reader::RemotesRemoteReader;
use crate::remotes::service::RemotesService;
use crate::stream::download::DownloadState;
use crate::stream::gateway::Gateway;
use crate::stream::local_files::LibraryFiles;
use crate::stream::routes::StreamState;
use crate::stream::transcode::{
    FfmpegTranscoder, LocalTranscodeGate, OutFormat, StdFfmpegSpawner, TranscodeSettings,
    ffmpeg_available,
};
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_kernel::config::AppConfig;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::CancellationToken;
use droppedneedle_kernel::mount::Routes;
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use droppedneedle_kernel::runtime_config::sections::{AudioFormat, ConnectApps};
use droppedneedle_kernel::runtime_config::{ConfigStore, Live};
use droppedneedle_plugins::scrobble::SqliteListenBrainzLinkStore;

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

/// The media drains' name on the supervisor.
pub const MEDIA_WORKERS: &str = "media-workers";

/// Every always-on media loop, by name.
pub const LOOPS: &[&str] = &[MEDIA_WORKERS];

/// The media background drains: remote attribution and scrobble
/// forwarding, one [`MEDIA_WORKERS`] job on the supervisor. Both end on the
/// stop after delivering what was queued, or once every queue handle in
/// the app drops.
pub struct MediaWorkers {
    reports: tokio::sync::mpsc::Receiver<crate::playback::reports::QueuedReport>,
    http: reqwest::Client,
    resolver: Arc<ConnectionResolver>,
    forwards: ForwardWorker,
}

impl MediaWorkers {
    /// Drain both queues until `stop` is cancelled or the app drops its
    /// handles.
    pub async fn run(self, stop: CancellationToken) {
        tokio::join!(
            run_report_worker(self.reports, self.http, self.resolver, stop.clone()),
            self.forwards.run(stop),
        );
    }
}

impl MediaSetup {
    /// Build the production bundle. `db` backs the connection and folder
    /// rows, and the playback stores open their own connections on its
    /// file; `config` hands out live handles on the admin's server
    /// settings, the download access and the transcode policy (read per
    /// stream); `crypto` seals linked credentials;
    /// `library_roots` plus the catalog in `db` turn local stream keys
    /// and download ids (track ids) into files under the live library
    /// roots (`None` keeps the constructor fallback for unwired builds and
    /// serves no downloads); `imports` is where remote playlist imports
    /// land; `plex_tv` and `scrobbles` are the plex.tv root and the
    /// scrobble forwarding addresses.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        db: &Db,
        app_config: &AppConfig,
        users: UsersDeps,
        crypto: Arc<Crypto>,
        http_factory: &droppedneedle_kernel::http_client::HttpClientFactory,
        ids: Arc<dyn IdGenerator>,
        library_roots: Option<droppedneedle_library::scan::coordinator::SharedResolver>,
        config: &ConfigStore,
        imports: Arc<dyn ImportSink>,
        plex_tv: String,
        scrobbles: crate::playback::forwarding::Endpoints,
    ) -> Result<(Self, MediaWorkers), String> {
        let http = http_factory.shared().clone();
        crate::stream::plugin_url::sweep_stale_spools();
        let library = library_roots.map(|roots| LibraryFiles::new(db.sqlx().clone(), roots));
        let download = DownloadState::new(library.clone(), config.live());
        let resolver = Arc::new(
            ConnectionResolver::new(
                Arc::new(SqliteConnectionStore::new(db.clone())),
                Arc::new(CredentialCoder::new(crypto.clone())),
                Arc::new(ConfigServers::new(config)),
            )
            .with_plex_tokens(Arc::new(PlexTokenProbe::with_plex_tv(
                http.clone(),
                plex_tv,
            ))),
        );
        let credentials = Arc::new(StoredScrobbleCredentials::new(
            Arc::new(SqliteListenBrainzLinkStore::new(db.clone(), crypto.clone())),
            users.lastfm.clone(),
            users.lastfm_switch.clone(),
            crypto,
        ));
        let service = RemotesService::new(
            http.clone(),
            resolver.clone(),
            Arc::new(SqliteFolderStore::new(db.clone())),
            imports,
        );
        let remotes = RemotesDeps { service };
        let reader = RemotesRemoteReader::with_resolver(
            http_factory.remote_audio().clone(),
            resolver.clone(),
        );
        let spawner = StdFfmpegSpawner::detect()
            .unwrap_or_else(|| StdFfmpegSpawner::with_path(PathBuf::from("ffmpeg")));
        let transcoder = FfmpegTranscoder::new(spawner, Arc::new(LocalTranscodeGate::new()));
        let gateway = Gateway::new(
            local_root(app_config),
            reader,
            transcoder,
            TranscodeSettings::default(),
            ffmpeg_available(),
        )
        .with_settings(config.live());
        let engine = match library {
            Some(library) => gateway.with_library(library),
            None => gateway,
        };
        let stream = StreamState {
            engine: Arc::new(engine),
        };
        let playback_db = Arc::new(PlaybackDb::new(db.clone(), ids.clone()));
        let (queue, rx) = ReportQueue::channel();
        let (forwarder, forwards) =
            ScrobbleForwarder::channel(playback_db.clone(), users.lastfm_switch.clone());
        let forwards = ForwardWorker::new(forwards, http.clone(), credentials, scrobbles);
        let playback = PlaybackDeps {
            catalog: playback_db.clone(),
            sinks: Arc::new(forwarder),
            remotes: Arc::new(queue),
            history: playback_db.clone(),
            prefs: playback_db.clone(),
            names: playback_db,
            sessions: SessionStore::new(),
            presence: PresenceRegistry::new(),
            dedup: ScrobbleDedup::new(),
            mixed: MixedDedup::new(),
            clock: Arc::new(SystemClock),
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

    /// Send accepted plays to listeners (the plugin host).
    pub fn with_play_events(mut self, events: Arc<dyn crate::playback::ports::PlayEvents>) -> Self {
        self.playback.events = events;
        self
    }
}

/// Every remote-source, stream, download and playback route.
pub fn routes() -> Routes<MediaSetup> {
    remotes::handlers::routes()
        .merge(stream::routes::routes())
        .merge(stream::download::routes())
        .merge(playback::routes())
}

impl FromRef<MediaSetup> for RemotesDeps {
    fn from_ref(setup: &MediaSetup) -> Self {
        setup.remotes.clone()
    }
}

impl FromRef<MediaSetup> for StreamState<MediaEngine> {
    fn from_ref(setup: &MediaSetup) -> Self {
        setup.stream.clone()
    }
}

impl FromRef<MediaSetup> for DownloadState {
    fn from_ref(setup: &MediaSetup) -> Self {
        setup.download.clone()
    }
}

impl FromRef<MediaSetup> for PlaybackDeps {
    fn from_ref(setup: &MediaSetup) -> Self {
        setup.playback.clone()
    }
}

/// The transcode policy in the `connect_apps` section now, read on every
/// stream open; `None` while the section does not read, and the stream is
/// refused rather than served under the defaults.
pub(crate) fn transcode_policy(settings: &Live<ConnectApps>) -> Option<TranscodeSettings> {
    settings.read().map(|section| transcode_settings(&section))
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
