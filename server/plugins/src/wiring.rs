//! Plugins bundle: host, routes, tick loops.
//!
//! [`new_host`] builds the [`PluginHost`] early in boot so the flows that
//! raise events or look up plugin sources (playback, acquisition,
//! streaming) can hold it; plugin state is backed by the jobs tick store
//! from [`tick_store`]. [`PluginsSetup`] is the one `AppState` field
//! plugins adds: it starts the enabled plugins, mounts the routes, and
//! runs tick loops over the same store on the shared loop supervisor as
//! `plugin-tick:{name}`.
//!
//! Production runs each plugin as a subprocess through
//! [`ProcessLauncher`]; the Python helper module ships inside the server
//! binary and is written to `<plugins>/.sdk/python` at boot, so a Python
//! plugin needs nothing installed beyond `python3` itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::FromRef;

use droppedneedle_kernel::jobs::Supervisor;
use droppedneedle_kernel::jobs::plugin_ticks::SqliteTickStore;
use droppedneedle_kernel::mount::Routes;
use droppedneedle_kernel::platform::Platform;

use super::handlers::{ExtRateLimiter, PluginsDeps, plugin_routes};
use super::host::PluginHost;
use super::install::{GitHubHosts, ReqwestFetcher, ZipUnpacker};
use super::process::ProcessLauncher;
use super::runtime::PluginLauncher;
use super::ticks::{PluginTickLoops, TICK_CANCEL_GRACE, TickLoopSync, TickStoreKind};

/// The Python helper module, shipped inside the binary.
const PYTHON_SDK: &str = include_str!("../../../sdk/python/droppedneedle_plugin.py");

/// Where the Python helper lives under the plugins directory.
pub fn python_sdk_dir(plugins_dir: &Path) -> PathBuf {
    plugins_dir.join(".sdk").join("python")
}

/// Write the Python helper next to the plugins, replacing an older copy.
/// A failure is logged: Python plugins then fail to start with a clear
/// import error, and nothing else is affected.
pub fn write_python_sdk(plugins_dir: &Path) {
    let dir = python_sdk_dir(plugins_dir);
    let target = dir.join("droppedneedle_plugin.py");
    if std::fs::read_to_string(&target).is_ok_and(|current| current == PYTHON_SDK) {
        return;
    }
    let written = std::fs::create_dir_all(&dir).and_then(|()| {
        let staged = dir.join(".droppedneedle_plugin.py.new");
        std::fs::write(&staged, PYTHON_SDK)?;
        std::fs::rename(&staged, &target)
    });
    if let Err(error) = written {
        tracing::warn!(%error, dir = %dir.display(), "could not write the Python plugin helper");
    }
}

/// The tick and plugin state store over the platform's database. The host
/// and the tick loops share this one value: a plugin is known to both.
pub fn tick_store(platform: &Platform) -> TickStoreKind {
    TickStoreKind::Sqlite(SqliteTickStore::new(platform.db.clone()))
}

/// The production host: subprocess plugins over the deployment's plugins
/// directory, each process supervised on the platform's supervisor, with
/// plugin state in `ticks` and library writes holding the roots through
/// `library_fs`, the publisher's lease table. Nothing starts until
/// [`PluginsSetup::build`] loads it.
pub fn new_host(
    platform: &Platform,
    ticks: &TickStoreKind,
    library_fs: droppedneedle_library::scan::fs::FsCoordinator,
) -> Arc<PluginHost> {
    let plugins_dir = platform.config.plugins_dir();
    // Best-effort: the host reads a missing directory as empty and the
    // install path creates it, so a failure here must not fail the boot.
    if let Err(error) = std::fs::create_dir_all(&plugins_dir) {
        tracing::warn!(%error, "plugins directory could not be created");
    }
    write_python_sdk(&plugins_dir);
    let launcher: Arc<dyn PluginLauncher> = Arc::new(ProcessLauncher::new(
        python_sdk_dir(&plugins_dir),
        platform.supervisor.clone(),
    ));
    PluginHost::new(
        plugins_dir,
        platform.settings.clone(),
        launcher,
        Arc::new(ticks.clone()),
        library_fs,
    )
}

/// Removes a test bundle's scratch folder once the last clone drops.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
struct ScratchGuard(PathBuf);

#[cfg(any(test, feature = "test-support"))]
impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Everything `create_app` needs to mount the plugin routes, plus the
/// tick loops on the shared loop supervisor.
#[derive(Clone)]
pub struct PluginsSetup {
    host: Arc<PluginHost>,
    plugins: PluginsDeps,
    ticks: Arc<PluginTickLoops<TickStoreKind>>,
    #[cfg(any(test, feature = "test-support"))]
    _scratch: Option<Arc<ScratchGuard>>,
}

impl PluginsSetup {
    /// Bind the production seams over a host from [`new_host`] and the
    /// tick store it was given: the HTTP fetcher, and tick loops over the
    /// shared loop supervisor. Loads the host, which starts the enabled
    /// plugins.
    pub fn build(
        platform: &Platform,
        host: Arc<PluginHost>,
        ticks: TickStoreKind,
        github: GitHubHosts,
    ) -> Self {
        Self::assemble(
            host,
            platform.http.shared().clone(),
            github,
            platform.supervisor.clone(),
            ticks,
        )
    }

    /// Test bundle: scratch config store and plugins directory, fake
    /// plugin runtimes (no subprocesses), a memory tick store, and tick
    /// loops on the caller's supervisor.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(supervisor: Supervisor) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};

        use super::fakes::FakeLauncher;
        use droppedneedle_kernel::http_client::HttpClientFactory;
        use droppedneedle_kernel::jobs::plugin_ticks::MemoryTickStore;
        use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};

        /// Scratch-dir sequence so parallel test states never share a store.
        static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

        let http = HttpClientFactory::new().map_err(|error| error.to_string())?;
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "droppedneedle-plugins-test-{}-{seq}",
            std::process::id()
        ));
        let scratch = Arc::new(ScratchGuard(dir.clone()));
        let config = Arc::new(ConfigStore::scratch(
            &dir,
            Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| error.to_string())?,
        )?);
        let ticks = TickStoreKind::Memory(MemoryTickStore::new());
        let host = PluginHost::new(
            dir.join("plugins"),
            config,
            Arc::new(FakeLauncher::new()),
            Arc::new(ticks.clone()),
            droppedneedle_library::scan::fs::FsCoordinator::new(),
        );
        let mut setup = Self::assemble(
            host,
            http.shared().clone(),
            GitHubHosts {
                api: super::install::API_BASE.to_owned(),
                codeload: super::install::CODELOAD_BASE.to_owned(),
            },
            supervisor,
            ticks,
        );
        setup._scratch = Some(scratch);
        Ok(setup)
    }

    /// Assemble the bundle from its parts, the same for production and
    /// tests.
    fn assemble(
        host: Arc<PluginHost>,
        http: reqwest::Client,
        github: GitHubHosts,
        supervisor: Supervisor,
        ticks: TickStoreKind,
    ) -> Self {
        host.load_all();
        let ticks = Arc::new(PluginTickLoops::new(supervisor, ticks, TICK_CANCEL_GRACE));
        let plugins = PluginsDeps {
            host: Arc::clone(&host),
            config: Arc::clone(host.config()),
            github,
            fetcher: Arc::new(ReqwestFetcher::new(http.clone())),
            unpacker: Arc::new(ZipUnpacker),
            tick_sync: Arc::clone(&ticks) as Arc<dyn TickLoopSync>,
            ext_limiter: Arc::new(ExtRateLimiter::new()),
        };
        Self {
            host,
            plugins,
            ticks,
            #[cfg(any(test, feature = "test-support"))]
            _scratch: None,
        }
    }

    /// The plugin host, for dispatches outside the route layer.
    pub fn host(&self) -> &Arc<PluginHost> {
        &self.host
    }

    /// Rebuild tick loops to match the host's enabled scheduler plugins.
    /// Boot calls this once after building; installs, updates, and
    /// uninstalls re-sync through the handlers' shared loops.
    pub async fn sync_ticks(&self) {
        self.ticks.sync_host(&self.host).await;
    }
}

/// The plugin routes. Admin-only routes are in the admin group and also
/// take the `Admin` extractor.
pub fn routes() -> Routes<PluginsSetup> {
    plugin_routes()
}

impl FromRef<PluginsSetup> for PluginsDeps {
    fn from_ref(setup: &PluginsSetup) -> Self {
        setup.plugins.clone()
    }
}
