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

use droppedneedle_kernel::jobs::plugin_ticks::SqliteTickStore;
use droppedneedle_kernel::mount::Routes;
use droppedneedle_kernel::platform::Platform;

use super::handlers::{ExtRateLimiter, PluginsDeps, plugin_routes};
use super::host::PluginHost;
use super::install::{GitHubHosts, ReqwestFetcher};
use super::process::ProcessLauncher;
use super::runtime::PluginLauncher;
use super::ticks::{PluginTickLoops, TICK_CANCEL_GRACE};

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
pub fn tick_store(platform: &Platform) -> SqliteTickStore {
    SqliteTickStore::new(platform.db.clone(), platform.clock.clone())
}

/// The production host: subprocess plugins over the deployment's plugins
/// directory, each process supervised on the platform's supervisor, with
/// plugin state in `ticks` and library writes going through `library`,
/// the library's own publisher. Nothing starts until
/// [`PluginsSetup::build`] loads it.
pub fn new_host(
    platform: &Platform,
    ticks: &SqliteTickStore,
    library: droppedneedle_library::wiring::LibraryCore,
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
        ticks.clone(),
        library,
    )
}

/// Everything `create_app` needs to mount the plugin routes, plus the
/// tick loops on the shared loop supervisor.
#[derive(Clone)]
pub struct PluginsSetup {
    host: Arc<PluginHost>,
    plugins: PluginsDeps,
    ticks: Arc<PluginTickLoops>,
}

impl PluginsSetup {
    /// Bind the production seams over a host from [`new_host`] and the
    /// tick store it was given: the HTTP fetcher, and tick loops over the
    /// shared loop supervisor. Loads the host, which starts the enabled
    /// plugins.
    pub fn build(
        platform: &Platform,
        host: Arc<PluginHost>,
        ticks: SqliteTickStore,
        github: GitHubHosts,
    ) -> Self {
        host.load_all();
        let supervisor = platform.supervisor.clone();
        let clock = Arc::clone(supervisor.clock());
        let ticks = Arc::new(PluginTickLoops::new(supervisor, ticks, TICK_CANCEL_GRACE));
        let plugins = PluginsDeps {
            host: Arc::clone(&host),
            config: Arc::clone(host.config()),
            github,
            fetcher: Arc::new(ReqwestFetcher::new(
                platform.http.shared().clone(),
                Arc::clone(&clock),
            )),
            tick_sync: Arc::clone(&ticks),
            ext_limiter: Arc::new(ExtRateLimiter::new()),
            clock,
        };
        Self {
            host,
            plugins,
            ticks,
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
