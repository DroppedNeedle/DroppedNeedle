//! Stage-6 bundle: remote sources, stream gateway, playback reporting.
//!
//! [`Stage6Setup`] is the single bundle `create_app` mounts: its
//! [`Stage6Setup::gated_router`] nests under `/api/v3` inside the
//! deny-by-default session gate, next to the reads nest. Connections,
//! folder preferences, and playlist imports run on the slice memory stores
//! (durable rows are a later-stage persistence tier); playback catalog,
//! history, prefs, and display names read the stage-2 schema through a
//! dedicated rusqlite handle. Outbound remote attribution drains through
//! the [`ReportWorker`] `main` spawns beside the warmup loops.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;

use crate::auth::users::UsersDeps;
use crate::config::AppConfig;
use crate::ids::IdGenerator;
use crate::playback::ports::{ReportTrack, ScrobbleSinks, ServiceOutcome, SystemClock};
use crate::playback::reports::{ReportQueue, run_report_worker};
use crate::playback::services::{
    MixedDedup, PlaybackDeps, PresenceRegistry, ScrobbleDedup, SessionStore,
};
use crate::playback::sqlite::PlaybackDb;
use crate::remotes::adapter::MemoryImportSink;
use crate::remotes::connections::{CredentialCoder, MemoryConnectionStore};
use crate::remotes::folders::MemoryFolderStore;
use crate::remotes::handlers::{RemotesDeps, remotes_router};
use crate::remotes::reader::RemotesRemoteReader;
use crate::runtime_config::crypto::Crypto;
use crate::runtime_config::sections::{AudioFormat, ConnectApps};
use crate::stream::gateway::Gateway;
use crate::stream::routes::{StreamState, stream_routes};
use crate::stream::transcode::{
    FfmpegTranscoder, LocalTranscodeGate, OutFormat, StdFfmpegSpawner, TranscodeSettings,
    ffmpeg_available,
};

/// Production stream engine: gateway over stored-connection remote reads
/// and ffmpeg transcodes behind the shared transcode gate.
pub type Stage6Engine =
    Gateway<RemotesRemoteReader, FfmpegTranscoder<StdFfmpegSpawner, Arc<LocalTranscodeGate>>>;

/// Everything `create_app` needs to mount the stage-6 slices, built once.
#[derive(Clone)]
pub struct Stage6Setup {
    /// Remote-browse deps (memory connections/folders/imports).
    pub remotes: RemotesDeps,
    /// Stream-gateway state over the production engine.
    pub stream: StreamState<Stage6Engine>,
    /// Playback-reporting deps (SQLite catalog/history/prefs/names).
    pub playback: PlaybackDeps,
}

/// Outbound attribution drain. `main` spawns [`ReportWorker::run`] and
/// awaits it after serve; the worker exits once every queue handle in the
/// app drops.
pub struct ReportWorker {
    rx: tokio::sync::mpsc::Receiver<crate::playback::reports::QueuedReport>,
    http: reqwest::Client,
    connections: Arc<dyn crate::remotes::connections::ConnectionStore>,
    coder: Arc<CredentialCoder>,
}

impl ReportWorker {
    /// Drain attribution reports until the app drops its queue handles.
    pub async fn run(self) {
        run_report_worker(self.rx, self.http, self.connections, self.coder).await;
    }
}

impl Stage6Setup {
    /// Build the production bundle. `db_path` backs the playback SQLite
    /// stores; `connect_apps` carries the transcode policy; `crypto` seals
    /// remote credentials; `library_roots` resolves local stream reads
    /// against the live library registry (`None` keeps the constructor
    /// fallback for unwired builds).
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        db_path: &Path,
        config: &AppConfig,
        users: UsersDeps,
        crypto: Arc<Crypto>,
        http: reqwest::Client,
        ids: Arc<dyn IdGenerator>,
        connect_apps: ConnectApps,
        library_roots: Option<crate::library::wiring::RootSource>,
    ) -> Result<(Self, ReportWorker), String> {
        let connections: Arc<MemoryConnectionStore> = Arc::new(MemoryConnectionStore::new());
        let folders: Arc<MemoryFolderStore> = Arc::new(MemoryFolderStore::new());
        let imports: Arc<MemoryImportSink> = Arc::new(MemoryImportSink::new());
        let coder = Arc::new(CredentialCoder::new(crypto));
        let remotes = RemotesDeps {
            http: http.clone(),
            connections: connections.clone(),
            coder: coder.clone(),
            folders,
            imports,
            auth: users,
            ids: ids.clone(),
        };
        let reader = RemotesRemoteReader::new(http.clone(), connections.clone(), coder.clone());
        let spawner = StdFfmpegSpawner::detect()
            .unwrap_or_else(|| StdFfmpegSpawner::with_path(PathBuf::from("ffmpeg")));
        let transcoder = FfmpegTranscoder::new(spawner, Arc::new(LocalTranscodeGate::new()));
        let gateway = Gateway::new(
            local_root(config),
            reader,
            transcoder,
            transcode_settings(&connect_apps),
            ffmpeg_available(),
        );
        let engine = match library_roots {
            Some(roots) => gateway.with_library_roots(roots),
            None => gateway,
        };
        let stream = StreamState {
            engine: Arc::new(engine),
            ids: ids.clone(),
        };
        let db = Arc::new(PlaybackDb::open(db_path, ids.clone())?);
        let (queue, rx) = ReportQueue::channel();
        let playback = PlaybackDeps {
            catalog: db.clone(),
            sinks: Arc::new(UnlinkedSinks),
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
        };
        let worker = ReportWorker {
            rx,
            http,
            connections,
            coder,
        };
        Ok((
            Self {
                remotes,
                stream,
                playback,
            },
            worker,
        ))
    }

    /// Test bundle over memory stores and slice fakes. The catalog is
    /// empty (honest 404s), attribution drops, and ffmpeg is absent, so
    /// every transcode decision lands direct.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(users: UsersDeps, ids: Arc<dyn IdGenerator>) -> Result<Self, String> {
        use crate::playback::fakes::{FakeCatalog, FakeHistory, FakeNames, FakePrefs, FakeSinks};

        let http = crate::http_client::HttpClientFactory::new()
            .map_err(|error| format!("test stage6 http: {error}"))?
            .shared()
            .clone();
        let crypto = Arc::new(
            Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| format!("test key: {error}"))?,
        );
        let connections: Arc<MemoryConnectionStore> = Arc::new(MemoryConnectionStore::new());
        let coder = Arc::new(CredentialCoder::new(crypto));
        let remotes = RemotesDeps {
            http: http.clone(),
            connections: connections.clone(),
            coder: coder.clone(),
            folders: Arc::new(MemoryFolderStore::new()),
            imports: Arc::new(MemoryImportSink::new()),
            auth: users,
            ids: ids.clone(),
        };
        let reader = RemotesRemoteReader::new(http, connections, coder);
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
            },
        })
    }

    /// Relative-path routers for nesting under `/api/v3` inside the
    /// session gate.
    pub fn gated_router(&self) -> Router {
        Router::new()
            .merge(remotes_router(self.remotes.clone()))
            .merge(stream_routes(self.stream.clone()))
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
/// normally absent, so local reads honestly 404. Wired builds
/// resolve against the live library registry instead.
fn local_root(config: &AppConfig) -> PathBuf {
    config.root_app_dir.join("music")
}

/// Scrobble sinks with no linked accounts: every forward answers empty,
/// which the services already read as "unlinked, history still records".
/// v3 native Last.fm/ListenBrainz linkage lands in a later stage.
struct UnlinkedSinks;

impl ScrobbleSinks for UnlinkedSinks {
    fn report_now_playing(
        &self,
        _user_id: &str,
        _track: &ReportTrack,
    ) -> HashMap<String, ServiceOutcome> {
        HashMap::new()
    }

    fn submit_scrobble(
        &self,
        _user_id: &str,
        _track: &ReportTrack,
    ) -> HashMap<String, ServiceOutcome> {
        HashMap::new()
    }
}
