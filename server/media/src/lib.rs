//! Media bundle: remote sources, stream gateway, playback reporting.
//!
//! [`MediaSetup`] is the single bundle `create_app` mounts [`routes`] over,
//! all of them in the user group inside the session gate. Remote
//! connections and Navidrome folder preferences are SQLite rows; the admin
//! servers are read from the config store on every call. Playback catalog,
//! history, listening preferences, links and display names read the
//! database through the one playback store. Remote attribution and
//! now-playing reports drain through [`MediaWorkers`], one job on the loop
//! supervisor. Scrobbles wait as jobs on the jobs table; their handlers are
//! [`MediaSetup::scrobble_jobs`], registered by the builder.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod playback;
pub mod remotes;
pub mod stream;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::FromRef;

use crate::playback::PlaybackDeps;
use crate::playback::forwarding::{Courier, NowPlayingSender, NowPlayingWorker, ScrobbleJobs};
use crate::playback::listening_routes::{ListeningDeps, MixGrants};
use crate::playback::presence::PresenceRegistry;
use crate::playback::report::{MixedDedup, ScrobbleDedup};
use crate::playback::reports::{ReportQueue, run_report_worker};
use crate::playback::sessions::SessionStore;
use crate::playback::sqlite::PlaybackStore;
use crate::remotes::adapter::ImportSink;
use crate::remotes::connections::ConnectionResolver;
use crate::remotes::folders::SqliteFolderStore;
use crate::remotes::handlers::RemotesDeps;
use crate::remotes::plex::PlexTokenProbe;
use crate::remotes::presence_feed::MediaJobs;
use crate::remotes::reader::RemotesRemoteReader;
use crate::remotes::service::RemotesService;
use crate::remotes::warmup::{self, MbidIndex};
use crate::stream::download::DownloadState;
use crate::stream::gateway::Gateway;
use crate::stream::leases::TranscodeGate;
use crate::stream::local_files::LibraryFiles;
use crate::stream::routes::StreamState;
use crate::stream::transcode::{FfmpegTranscoder, OutFormat, ProcessSpawner, TranscodeSettings};
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::config::AppConfig;
use droppedneedle_kernel::jobs::CancellationToken;
use droppedneedle_kernel::jobs::queue::{JobHandler, JobQueue, KindSpec};
use droppedneedle_kernel::mount::Routes;
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::{AudioFormat, ConnectApps};

/// Production stream engine: gateway over stored-connection remote reads
/// and ffmpeg transcodes behind the shared transcode gate.
pub type MediaEngine = Gateway<RemotesRemoteReader, FfmpegTranscoder<ProcessSpawner>>;

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
    /// Playback-reporting deps over the one playback store.
    pub playback: PlaybackDeps,
    /// Listening-settings route deps.
    pub listening: ListeningDeps,
    /// The scrobble job handlers, one per service.
    scrobble_jobs: Vec<(KindSpec, Arc<dyn JobHandler>)>,
}

/// The media drains' name on the supervisor.
pub const MEDIA_WORKERS: &str = "media-workers";

/// Every always-on media loop, by name.
pub const LOOPS: &[&str] = &[
    MEDIA_WORKERS,
    warmup::JELLYFIN_JOB,
    warmup::NAVIDROME_JOB,
    warmup::PLEX_JOB,
];

/// The media background drains: remote attribution and now-playing
/// reports, one [`MEDIA_WORKERS`] job on the supervisor. Attribution ends
/// on the stop after delivering what was queued, or once every queue handle
/// in the app drops; now-playing reports still waiting at the stop are
/// dropped, as a late one would be wrong.
pub struct MediaWorkers {
    reports: tokio::sync::mpsc::Receiver<crate::playback::reports::QueuedReport>,
    http: reqwest::Client,
    resolver: Arc<ConnectionResolver>,
    clock: Arc<dyn Clock>,
    now_playing: NowPlayingWorker,
}

impl MediaWorkers {
    /// Drain both until `stop` is cancelled.
    pub async fn run(self, stop: CancellationToken) {
        tokio::join!(
            run_report_worker(
                self.reports,
                self.http,
                self.resolver,
                self.clock,
                stop.clone(),
            ),
            self.now_playing.run(stop),
        );
    }
}

impl MediaSetup {
    /// The app's one connection resolver, over `platform`, looking up
    /// missing Plex server tokens on the plex.tv root `plex_tv`. Every
    /// remote read shares it, so they share its per-source read bound.
    pub fn remotes(platform: &Platform, plex_tv: String) -> Arc<ConnectionResolver> {
        Arc::new(
            ConnectionResolver::over(platform).with_plex_tokens(Arc::new(
                PlexTokenProbe::with_plex_tv(platform.http.shared().clone(), plex_tv),
            )),
        )
    }

    /// Build the production bundle over `platform`. Its database backs the
    /// connection and folder rows, and the playback stores open their own
    /// connections on its file; its settings hand out live handles on the
    /// admin's server settings, the download access and the transcode
    /// policy (read per stream); its data key seals linked credentials;
    /// its event hub carries the now-playing snapshots.
    /// `library_roots` plus the catalog turn local stream keys and
    /// download ids (track ids) into files under the live library roots;
    /// `imports` is where remote playlist imports land; `plugins` hears
    /// accepted plays and serves `streaming_source` plugin streams;
    /// `remotes` is the app's one connection resolver (see
    /// [`MediaSetup::remotes`]); `ffmpeg` is the program transcodes
    /// run, and with none the server streams every file as it is;
    /// scrobbles and now-playing reports go through the one ListenBrainz
    /// client and the one Last.fm client of `providers`, and that
    /// ListenBrainz client checks a link before it is stored; `mix` is the
    /// personal-mix standing grant the listening settings report and feed.
    /// Scrobbles wait on `jobs`, whose clock `jobs_clock` measures their
    /// window.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        platform: &Platform,
        users: UsersDeps,
        library_roots: droppedneedle_library::scan::coordinator::SharedResolver,
        imports: Arc<dyn ImportSink>,
        plugins: Arc<droppedneedle_plugins::host::PluginHost>,
        remotes: Arc<ConnectionResolver>,
        ffmpeg: Option<PathBuf>,
        providers: &droppedneedle_providers::Providers,
        mix: Arc<dyn MixGrants>,
        jobs: JobQueue,
        jobs_clock: Arc<dyn Clock>,
    ) -> Result<(Self, MediaWorkers), String> {
        let (db, app_config, config) = (&platform.db, &platform.config, &*platform.settings);
        let (http_factory, ids, crypto) = (
            &platform.http,
            platform.ids.clone(),
            platform.crypto.clone(),
        );
        let http = http_factory.shared().clone();
        crate::stream::plugin_url::sweep_stale_spools();
        let library = LibraryFiles::new(db.sqlx().clone(), library_roots);
        let download = DownloadState::new(Some(library.clone()), config.live());
        let resolver = remotes;
        let service = RemotesService::new(
            http.clone(),
            resolver.clone(),
            Arc::new(SqliteFolderStore::new(db.clone(), platform.clock.clone())),
            imports,
            MbidIndex::default(),
            platform.clock.clone(),
        );
        let remotes = RemotesDeps { service };
        let reader = RemotesRemoteReader::with_resolver(
            http_factory.remote_audio().clone(),
            resolver.clone(),
        );
        // Without an ffmpeg every plan lands direct, so the spawner's path
        // is never run.
        let spawner =
            ProcessSpawner::with_path(ffmpeg.clone().unwrap_or_else(|| PathBuf::from("ffmpeg")));
        let transcoder = FfmpegTranscoder::new(spawner, Arc::new(TranscodeGate::new()));
        let gateway = Gateway::new(
            local_root(app_config),
            reader,
            transcoder,
            TranscodeSettings::default(),
            ffmpeg.is_some(),
        )
        .with_settings(config.live())
        .with_library(library)
        .with_plugins(plugins.clone(), http_factory.direct_no_redirect().clone());
        let stream = StreamState {
            engine: Arc::new(gateway),
        };
        let store = Arc::new(PlaybackStore::new(
            db.clone(),
            ids,
            crypto,
            platform.clock.clone(),
            jobs,
        ));
        let (queue, rx) = ReportQueue::channel();
        let courier = Courier::new(
            providers.listenbrainz.clone(),
            providers.lastfm.clone(),
            store.clone(),
            users.lastfm_switch.clone(),
            jobs_clock,
        );
        let (now_playing, slots) =
            NowPlayingSender::channel(store.clone(), users.lastfm_switch.clone());
        let presence = PresenceRegistry::new(platform.events.clone());
        let listening = ListeningDeps {
            store: store.listening().clone(),
            verifier: Arc::new(providers.listenbrainz.clone()),
            mix,
            presence: presence.clone(),
        };
        let playback = PlaybackDeps {
            store,
            sinks: Arc::new(now_playing),
            lastfm_switch: users.lastfm_switch.clone(),
            remotes: Arc::new(queue),
            sessions: SessionStore::new(),
            presence,
            dedup: ScrobbleDedup::new(),
            mixed: MixedDedup::new(),
            clock: platform.clock.clone(),
            events: Arc::new(crate::playback::plugin_events::PluginPlayEvents(plugins)),
        };
        let worker = MediaWorkers {
            reports: rx,
            http,
            resolver,
            clock: platform.clock.clone(),
            now_playing: NowPlayingWorker::new(slots, courier.clone()),
        };
        Ok((
            Self {
                remotes,
                stream,
                download,
                playback,
                listening,
                scrobble_jobs: ScrobbleJobs::handlers(&courier),
            },
            worker,
        ))
    }

    /// The scrobble job kinds and their handlers, for the builder to
    /// register with the jobs table.
    pub fn scrobble_jobs(&self) -> Vec<(KindSpec, Arc<dyn JobHandler>)> {
        self.scrobble_jobs.clone()
    }

    /// What the jobs bundle reads from media: the presence registry the
    /// playback routes write, the bundle's connection resolver and MBID
    /// index, and the platform's outbound client, database and clock.
    pub fn jobs(&self, platform: &Platform) -> MediaJobs {
        MediaJobs::new(
            self.playback.presence.clone(),
            self.remotes.service.resolver().clone(),
            platform.http.shared().clone(),
            platform.db.sqlx().clone(),
            platform.clock.clone(),
            self.remotes.service.index().clone(),
        )
    }
}

/// Every remote-source, stream, download and playback route.
pub fn routes() -> Routes<MediaSetup> {
    remotes::handlers::routes()
        .merge(stream::routes::routes())
        .merge(stream::download::routes())
        .merge(playback::routes())
        .merge(playback::listening_routes::routes())
}

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub fn statement_files() -> Vec<droppedneedle_kernel::db::statement::StatementFile> {
    vec![playback::sql::registered()]
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

impl FromRef<MediaSetup> for ListeningDeps {
    fn from_ref(setup: &MediaSetup) -> Self {
        setup.listening.clone()
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

/// The gateway's constructor root, `<root>/music`. The built gateway
/// resolves local keys against the live library registry instead.
fn local_root(config: &AppConfig) -> PathBuf {
    config.root_app_dir.join("music")
}
