//! Plugins bundle: host, routes, scrobble backend, tick loops.
//!
//! [`new_host`] builds the [`PluginHost`] early in boot so the flows that
//! raise events or look up plugin sources (playback, acquisition,
//! streaming) can hold it. [`PluginsSetup`] is the one `AppState` field
//! plugins adds: it backs plugin state with the jobs tick store, starts
//! the enabled plugins, mounts the routes, and runs tick loops on the
//! shared jobs registry as `plugin-tick:{name}`.
//!
//! Production runs each plugin as a subprocess through
//! [`ProcessLauncher`]; the Python helper module ships inside the server
//! binary and is written to `<plugins>/.sdk/python` at boot, so a Python
//! plugin needs nothing installed beyond `python3` itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;

use crate::auth::users::{UsersDeps, roles::Role};
use crate::db::WriteLane;
use crate::ids::IdGenerator;
use crate::jobs::plugin_ticks::SqliteTickStore;
use crate::jobs::registry::JobRegistry;
use crate::jobs::wiring::StoreKind;
use crate::providers::cache::{ProviderCache, invalidate_source};
use crate::runtime_config::{ConfigStore, Crypto};

use super::handlers::{
    ExtRateLimiter, PluginsDeps, ScrobbleHttpDeps, UserRoles, plugins_router, scrobble_router,
};
use super::host::PluginHost;
use super::install::{ReqwestFetcher, ZipUnpacker};
use super::process::ProcessLauncher;
use super::runtime::PluginLauncher;
use super::scrobble::{
    ConnectionChangedHook, ListenBrainzLinkStore, NoopMixApprovalHook, ScrobbleDeps,
    ScrobblePrefsStore, SqliteListenBrainzLinkStore, SqliteScrobblePrefsStore, StaticMixState,
};
use super::ticks::{PluginTickLoops, TICK_CANCEL_GRACE, TickLoopSync, TickStoreKind};
use crate::providers::listenbrainz::HttpListenBrainzVerifier;

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

/// The production host: subprocess plugins over the deployment's plugins
/// directory. Nothing starts until [`PluginsSetup::build`] loads it.
pub fn new_host(plugins_dir: PathBuf, config: Arc<ConfigStore>) -> Arc<PluginHost> {
    // Best-effort: the host reads a missing directory as empty and the
    // install path creates it, so a failure here must not fail the boot.
    if let Err(error) = std::fs::create_dir_all(&plugins_dir) {
        tracing::warn!(%error, "plugins directory could not be created");
    }
    write_python_sdk(&plugins_dir);
    let launcher: Arc<dyn PluginLauncher> =
        Arc::new(ProcessLauncher::new(python_sdk_dir(&plugins_dir)));
    PluginHost::new(plugins_dir, config, launcher)
}

/// Role lookups over the user store. The plugins routes call this
/// synchronously while the store is async, so the lookup bridges with
/// `block_in_place`: production runs the multi-thread runtime, and route
/// tests that reach these extractors must too (`flavor = "multi_thread"`).
/// Missing sessions, gone accounts, and store faults all fail closed.
#[derive(Clone)]
pub struct StoreRoles {
    users: UsersDeps,
}

impl UserRoles for StoreRoles {
    fn role_of(&self, user_id: &str) -> Option<Role> {
        let users = self.users.clone();
        let id = user_id.to_owned();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                users
                    .users
                    .get_by_id(&id)
                    .await
                    .ok()
                    .flatten()
                    .map(|user| user.role)
            })
        })
    }
}

/// Link-change hook over the shared provider cache: a ListenBrainz link
/// mutation invalidates the dependent cached reads, the way v2 resets its
/// breaker and clears caches on every per-user change. The invalidation
/// spawns off the request path; the request never waits on it.
#[derive(Clone)]
pub struct ListenBrainzCacheHook {
    cache: Arc<dyn ProviderCache>,
}

impl ConnectionChangedHook for ListenBrainzCacheHook {
    fn on_listenbrainz_connection_changed(&self) {
        let cache = Arc::clone(&self.cache);
        tokio::spawn(async move {
            invalidate_source(cache.as_ref(), "listenbrainz").await;
        });
    }
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
/// tick loops over the shared jobs registry.
#[derive(Clone)]
pub struct PluginsSetup {
    host: Arc<PluginHost>,
    plugins: PluginsDeps,
    scrobble: ScrobbleHttpDeps,
    ticks: Arc<PluginTickLoops<StoreKind, TickStoreKind>>,
    #[cfg(any(test, feature = "test-support"))]
    _scratch: Option<Arc<ScratchGuard>>,
}

impl PluginsSetup {
    /// Bind the production seams over a host from [`new_host`]: SQLite
    /// tick, state and scrobble stores, the HTTP verifier and fetcher,
    /// cache invalidation on link changes, and tick loops over the shared
    /// jobs registry. Loads the host, which starts the enabled plugins.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        host: Arc<PluginHost>,
        users: UsersDeps,
        http: reqwest::Client,
        ids: Arc<dyn IdGenerator>,
        crypto: Arc<Crypto>,
        cache: Arc<dyn ProviderCache>,
        registry: JobRegistry<StoreKind>,
        pool: sqlx::SqlitePool,
        lane: WriteLane,
    ) -> Self {
        Self::assemble(
            host,
            users,
            http,
            ids,
            cache,
            registry,
            TickStoreKind::Sqlite(SqliteTickStore::new(pool.clone(), lane.clone())),
            Arc::new(SqliteScrobblePrefsStore::new(pool.clone(), lane.clone())),
            Arc::new(SqliteListenBrainzLinkStore::new(pool, lane, crypto)),
        )
    }

    /// Test bundle: scratch config store and plugins directory, fake
    /// plugin runtimes (no subprocesses), memory tick and scrobble stores,
    /// the production verifier, and tick loops over the caller's registry.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        registry: JobRegistry<StoreKind>,
    ) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};

        use super::fakes::FakeLauncher;
        use super::scrobble::{MemoryListenBrainzLinkStore, MemoryScrobblePrefsStore};
        use crate::http_client::HttpClientFactory;
        use crate::jobs::plugin_ticks::MemoryTickStore;
        use crate::providers::InMemoryProviderCache;

        /// Scratch-dir sequence so parallel test states never share a store.
        static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

        let http = HttpClientFactory::new().map_err(|error| error.to_string())?;
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "droppedneedle-plugins-test-{}-{seq}",
            std::process::id()
        ));
        let scratch = Arc::new(ScratchGuard(dir.clone()));
        // `Crypto` is not cloneable, so the store and the link sealer each
        // open the same test key; both instances seal identically.
        let config = Arc::new(
            ConfigStore::open(
                &dir.join("config.json"),
                Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
        );
        let crypto =
            Arc::new(Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| error.to_string())?);
        let cache: Arc<dyn ProviderCache> = Arc::new(InMemoryProviderCache::new());
        let host = PluginHost::new(dir.join("plugins"), config, Arc::new(FakeLauncher::new()));
        let mut setup = Self::assemble(
            host,
            users,
            http.shared().clone(),
            ids,
            cache,
            registry,
            TickStoreKind::Memory(MemoryTickStore::new()),
            Arc::new(MemoryScrobblePrefsStore::new()),
            Arc::new(MemoryListenBrainzLinkStore::new(crypto)),
        );
        setup._scratch = Some(scratch);
        Ok(setup)
    }

    /// Assemble the bundle from its parts. Production passes the SQLite
    /// stores, tests the memory ones; everything else is identical.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        host: Arc<PluginHost>,
        users: UsersDeps,
        http: reqwest::Client,
        ids: Arc<dyn IdGenerator>,
        cache: Arc<dyn ProviderCache>,
        registry: JobRegistry<StoreKind>,
        ticks: TickStoreKind,
        prefs: Arc<dyn ScrobblePrefsStore>,
        links: Arc<dyn ListenBrainzLinkStore>,
    ) -> Self {
        host.set_state_store(Arc::new(ticks.clone()));
        host.load_all();
        let roles: Arc<dyn UserRoles> = Arc::new(StoreRoles {
            users: users.clone(),
        });
        let ticks = Arc::new(PluginTickLoops::new(registry, ticks, TICK_CANCEL_GRACE));
        let plugins = PluginsDeps {
            host: Arc::clone(&host),
            config: Arc::clone(host.config()),
            roles: Arc::clone(&roles),
            ids: Arc::clone(&ids),
            fetcher: Arc::new(ReqwestFetcher::new(http.clone())),
            unpacker: Arc::new(ZipUnpacker),
            tick_sync: Arc::clone(&ticks) as Arc<dyn TickLoopSync>,
            ext_limiter: Arc::new(ExtRateLimiter::new()),
        };
        let scrobble = ScrobbleHttpDeps {
            deps: ScrobbleDeps::new(
                prefs,
                links,
                Arc::new(HttpListenBrainzVerifier::prod(http)),
                Arc::new(NoopMixApprovalHook),
                Arc::new(ListenBrainzCacheHook { cache }),
            ),
            roles,
            mix_state: Arc::new(StaticMixState),
            ids,
        };
        Self {
            host,
            plugins,
            scrobble,
            ticks,
            #[cfg(any(test, feature = "test-support"))]
            _scratch: None,
        }
    }

    /// Send saved `now_playing_visibility` changes to the live presence
    /// feed so a user going offline or hiding the track disappears at once.
    pub fn with_presence(mut self, hook: Arc<dyn super::scrobble::VisibilityHook>) -> Self {
        self.scrobble.deps.visibility_hook = hook;
        self
    }

    /// The plugin host, for dispatches outside the route layer.
    pub fn host(&self) -> &Arc<PluginHost> {
        &self.host
    }

    /// Mount the plugin and scrobble routes. Layers come from `create_app`
    /// (the session gate plus rate limiting); admin-only routes self-gate
    /// through the `AdminUser` extractor inside the handlers.
    pub fn gated_router(&self) -> Router {
        Router::new()
            .merge(plugins_router(self.plugins.clone()))
            .merge(scrobble_router(self.scrobble.clone()))
    }

    /// Rebuild tick loops to match the host's enabled scheduler plugins.
    /// Boot calls this once after building; installs, updates, and
    /// uninstalls re-sync through the handlers' shared loops.
    pub async fn sync_ticks(&self) {
        self.ticks.sync_host(&self.host).await;
    }
}
