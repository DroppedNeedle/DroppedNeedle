//! Acquisition doubles for what sits outside the process: MusicBrainz
//! answers for download targets, and a plugin download client that
//! finishes every download at once with files a test put in place. The
//! worker reaches the plugin the way it reaches any plugin source: through
//! the saved settings and the plugin host.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_acquire::clients::LiveClients;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::worker::{DownloadWorker, WorkerPorts, WorkerSettings};
use droppedneedle_acquire::landing::LandingService;
use droppedneedle_acquire::landing::library::LibraryLanding;
use droppedneedle_acquire::readiness::SourceReadiness;
use droppedneedle_acquire::target::Targets;
use droppedneedle_acquire::target::lookup::{
    AlbumLookup, AlbumRelease, LookupError, ReleaseCandidate,
};
use droppedneedle_acquire::wiring::ClientLives;
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_plugins::protocol::{Fault, methods};
use droppedneedle_plugins::runtime::{
    BoxFuture, CallError, LaunchSpec, PluginConnection, PluginLauncher, RuntimeState, RuntimeStatus,
};
use serde_json::{Value, json};

/// Fixed answers for download targets: recordings, releases and
/// release-group dates by MBID. A missing entry reads as "MusicBrainz does
/// not know it", so the default answers nothing.
#[derive(Debug, Default)]
pub struct FixedAlbums {
    /// Recording MBID to its releases.
    pub recordings: HashMap<String, Vec<ReleaseCandidate>>,
    /// Release MBID to the release.
    pub releases: HashMap<String, AlbumRelease>,
    /// Release-group MBID to its first release date.
    pub first_release_dates: HashMap<String, String>,
}

impl AlbumLookup for FixedAlbums {
    fn recording_releases<'a>(
        &'a self,
        recording_mbid: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<ReleaseCandidate>>, LookupError>> + Send + 'a>>
    {
        Box::pin(async move { Ok(self.recordings.get(recording_mbid).cloned()) })
    }

    fn release<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AlbumRelease>, LookupError>> + Send + 'a>> {
        Box::pin(async move { Ok(self.releases.get(release_mbid).cloned()) })
    }

    fn first_release_date<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, LookupError>> + Send + 'a>> {
        Box::pin(async move { Ok(self.first_release_dates.get(release_group_mbid).cloned()) })
    }
}

/// Targets MusicBrainz answers nothing for: an album is searched by its
/// names.
pub fn no_targets(journal: &Arc<Journal>) -> Arc<Targets> {
    Arc::new(Targets::new(
        journal.clone(),
        Arc::new(FixedAlbums::default()),
    ))
}

/// The plugin's name; its source key is `plugin:fixed`.
pub const FIXED: &str = "fixed";

const MANIFEST: &str = "[plugin]\n\
    name = \"fixed\"\n\
    version = \"1.0.0\"\n\
    api_version = 2\n\
    entrypoint = \"plugin:Fixed\"\n\
    capabilities = [\"download_client\", \"indexer\"]\n\
    [[capability]]\n\
    id = \"download_client\"\n\
    source = \"fixed\"\n\
    display_name = \"Fixed\"\n\
    [[capability]]\n\
    id = \"indexer\"\n\
    [[settings]]\n\
    key = \"downloads_dir\"\n\
    label = \"Downloads directory\"\n";

/// A plugin download client and its own indexer. Every search offers one
/// release no earlier search offered, so a retry or a failover always has
/// one left; every download is finished at once, with the files
/// [`FixedPlugin::serve`] last named.
pub struct FixedPlugin {
    /// The host the plugin runs on, enabled.
    pub host: Arc<PluginHost>,
    connection: Arc<FixedConnection>,
}

impl FixedPlugin {
    /// Install and enable the plugin in `platform`'s plugin folder, on a
    /// host of its own adding library files through `library`.
    pub fn install(
        platform: &Platform,
        library: droppedneedle_library::wiring::LibraryCore,
    ) -> Self {
        let folder = platform.config.plugins_dir().join(FIXED);
        std::fs::create_dir_all(&folder).expect("plugin folder");
        std::fs::write(folder.join("plugin.toml"), MANIFEST).expect("manifest");
        let connection = Arc::new(FixedConnection::default());
        let host = PluginHost::new(
            platform.config.plugins_dir(),
            platform.settings.clone(),
            Arc::new(FixedLauncher(connection.clone())),
            droppedneedle_plugins::wiring::tick_store(platform),
            library,
        );
        host.load_all();
        std::fs::create_dir_all(host.data_dir(FIXED)).expect("plugin data folder");
        host.update_settings(FIXED, true, HashMap::new())
            .expect("plugin enabled");
        Self { host, connection }
    }

    /// Finish every later download with `files`, which sit inside
    /// `downloads`, the folder the plugin downloads into.
    pub fn serve(&self, downloads: &Path, files: &[PathBuf]) {
        std::fs::create_dir_all(downloads).expect("downloads folder");
        self.host
            .update_settings(
                FIXED,
                true,
                HashMap::from([(
                    "downloads_dir".to_owned(),
                    downloads.to_string_lossy().into_owned(),
                )]),
            )
            .expect("plugin settings save");
        *self.connection.files.lock().expect("files") = files.to_vec();
    }

    /// Make every later discard fail (or succeed again).
    pub fn fail_discards(&self, fail: bool) {
        self.connection.discard_fails.store(fail, Ordering::Relaxed);
    }
}

/// Save `settings`' download policy as `change` leaves it. The worker reads
/// it on its next pass.
pub fn save_policy(settings: &ConfigStore, change: impl FnOnce(&mut DownloadPolicy)) {
    let live = settings.live::<DownloadPolicy>();
    let mut policy = (**live.read().expect("policy reads")).clone();
    change(&mut policy);
    settings.save_blocking(policy).expect("policy saves");
}

/// The download policy with the search review off: each source grabs its
/// own best guess, as a scripted source with no edition to review against
/// needs.
pub fn review_off(platform: &Platform) {
    review_off_in(&platform.settings);
}

/// [`review_off`] in `settings`.
pub fn review_off_in(settings: &ConfigStore) {
    save_policy(settings, |policy| policy.search_review = "off".to_owned());
}

/// The production download worker over `journal`, for rigs that build
/// acquisition by hand: its sources and tuning are read from `settings`
/// (slskd's downloads mount at `slskd_downloads`), plugins on a host that
/// launches nothing unless `plugins` is given, targets read through
/// `lookup`, finished downloads landing in a library of its own on `db`'s
/// database, and live progress sent to `events`. Answers the targets too.
pub async fn worker_over(
    db: &AcquireDb,
    journal: Arc<Journal>,
    settings: &ConfigStore,
    slskd_downloads: PathBuf,
    lookup: Arc<dyn AlbumLookup>,
    plugins: Option<Arc<PluginHost>>,
    events: droppedneedle_kernel::events::EventHub,
) -> (Arc<DownloadWorker>, Arc<Targets>) {
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let platform = super::TestPlatform::on(db.db(), &dir).build().await;
    let (library, _) = super::library::library_at(&platform);
    let targets = Arc::new(Targets::new(journal.clone(), lookup));
    let landing = Arc::new(LandingService::new(
        journal.clone(),
        Arc::new(LibraryLanding::new(
            &library,
            library.imports.releases.clone(),
        )),
        settings.live(),
        dir.join("held"),
        settings.live(),
        settings.live(),
    ));
    let plugins =
        plugins.unwrap_or_else(|| super::bundles::idle_host(&platform, library.core.clone()));
    let worker = worker_with(
        &platform,
        plugins,
        journal,
        settings,
        slskd_downloads,
        targets.clone(),
        landing,
        Default::default(),
        events,
    );
    (worker, targets)
}

/// Turn slskd on in `settings`, at `url` with the mock's key.
pub fn save_slskd(settings: &ConfigStore, url: &str) {
    settings
        .save_secret_blocking(
            droppedneedle_kernel::runtime_config::secret_sections::SlskdConnection {
                enabled: true,
                url: url.to_owned(),
                api_key: droppedneedle_testkit::slskd::MOCK_API_KEY.into(),
                ..Default::default()
            },
        )
        .expect("slskd connection saves");
}

/// As [`worker_over`], on `platform` with plugins on `plugins`, its own
/// targets and landing, the cleanup warning shown on `health`, and live
/// progress sent to `events`.
#[allow(clippy::too_many_arguments)]
pub fn worker_with(
    platform: &Platform,
    plugins: Arc<PluginHost>,
    journal: Arc<Journal>,
    settings: &ConfigStore,
    slskd_downloads: PathBuf,
    targets: Arc<Targets>,
    landing: Arc<LandingService>,
    health: Arc<droppedneedle_providers::health::ServiceHealth>,
    events: droppedneedle_kernel::events::EventHub,
) -> Arc<DownloadWorker> {
    let clients = Arc::new(LiveClients::new(
        platform.http.shared().clone(),
        platform.http.no_redirect().clone(),
        platform.http.same_host_redirects().clone(),
        droppedneedle_providers::Providers::closed().newznab,
        ClientLives::new(settings),
        slskd_downloads,
        journal.clone(),
        targets.clone(),
        plugins.clone(),
    ));
    let worker_settings = Arc::new(WorkerSettings::new(settings, clients));
    Arc::new(DownloadWorker::new(
        journal,
        worker_settings.clone(),
        SourceReadiness::new(worker_settings),
        events,
        health,
        WorkerPorts {
            landing,
            plugins,
            targets,
        },
    ))
}

struct FixedLauncher(Arc<FixedConnection>);

impl PluginLauncher for FixedLauncher {
    fn launch(&self, _spec: LaunchSpec) -> Result<Arc<dyn PluginConnection>, String> {
        Ok(self.0.clone())
    }
}

#[derive(Default)]
struct FixedConnection {
    files: Mutex<Vec<PathBuf>>,
    searches: AtomicU64,
    discard_fails: AtomicBool,
}

impl FixedConnection {
    fn names(&self) -> Vec<String> {
        self.files
            .lock()
            .expect("files")
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    fn answer(&self, method: &str, params: &Value) -> Result<Value, CallError> {
        Ok(match method {
            methods::SEARCH_ALBUM | methods::SEARCH_TRACK => {
                let n = self.searches.fetch_add(1, Ordering::Relaxed);
                json!([{
                    "title": format!("Fixed release {n}"),
                    "size_bytes": 1,
                    "score": 1.0,
                    "payload": format!("fixed-{n}"),
                }])
            }
            methods::ENQUEUE => json!({
                "filenames": self.names(),
                "job_name": params["job_name"],
            }),
            methods::STATUS => {
                let names = self.names();
                json!({
                    "status": "completed",
                    "files_total": names.len(),
                    "files_completed": names.len(),
                    "bytes_downloaded": 1,
                    "succeeded_filenames": names,
                })
            }
            methods::INSPECT => json!({
                "state": "completed",
                "file_paths": self.names(),
            }),
            methods::DISCARD if self.discard_fails.load(Ordering::Relaxed) => {
                return Err(CallError::Failed {
                    message: "discard refused".to_owned(),
                    fault: Fault::Local,
                });
            }
            methods::DISCARD | methods::ABORT => json!(true),
            _ => Value::Null,
        })
    }
}

impl PluginConnection for FixedConnection {
    fn call<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        _timeout: Duration,
    ) -> BoxFuture<'a, Result<Value, CallError>> {
        Box::pin(async move { self.answer(method, &params) })
    }

    fn notify(&self, _method: &str, _params: Value) {}

    fn status(&self) -> RuntimeStatus {
        RuntimeStatus {
            state: RuntimeState::Running,
            restarts: 0,
            last_error: None,
            implemented: Vec::new(),
        }
    }

    fn stop(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}
