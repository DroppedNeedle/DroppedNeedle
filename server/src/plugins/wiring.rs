//! Stage-10 plugins bundle: host, routes, scrobble backend, tick loops.
//!
//! [`PluginsSetup`] is the one `AppState` field plugins adds. It builds the
//! [`PluginHost`] over the deployment's plugins directory, assembles the
//! plugin and scrobble route dependencies, and shares the jobs registry so
//! tick loops register as `plugin-tick:{name}` through the one jobs choke
//! point — the plugins slice never runs a second registry or its own loop
//! mechanics.
//!
//! Production binds the real archive fetcher, a deflate zip reader, the
//! HTTP ListenBrainz verifier, cache invalidation on link changes, and the
//! SQLite tick and scrobble stores (tick state survives restarts with the
//! database; so do prefs and links). Two seams stay interim by design:
//! module loading (v3 has no execution engine for plugin code yet, so
//! enabling a plugin records a per-plugin error instead of failing the
//! host) and the personal-mix hooks (stage 7 owns the queue; the static
//! reader answers until the service plugs in here).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;

// Shared items resolve through the `droppedneedle` crate name, slice
// siblings through `super`.
use droppedneedle::auth::users::{UsersDeps, roles::Role};
use droppedneedle::db::WriteLane;
use droppedneedle::http_client::HttpClientFactory;
use droppedneedle::ids::IdGenerator;
use droppedneedle::jobs::plugin_ticks::{MemoryTickStore, SqliteTickStore};
use droppedneedle::jobs::registry::JobRegistry;
use droppedneedle::jobs::wiring::StoreKind;
use droppedneedle::providers::InMemoryProviderCache;
use droppedneedle::providers::cache::{ProviderCache, invalidate_source};
use droppedneedle::runtime_config::{ConfigStore, Crypto};

use super::handlers::{
    ExtRateLimiter, PluginsDeps, ScrobbleHttpDeps, UserRoles, plugins_router, scrobble_router,
};
use super::host::{
    ArchiveEntry, ArchiveUnpacker, MAX_PLUGIN_ZIP_DECOMPRESSED_BYTES, MAX_PLUGIN_ZIP_ENTRIES,
    MAX_PLUGIN_ZIP_FILE_BYTES, PluginHost, ReqwestFetcher,
};
use super::manifest::PluginManifest;
use super::runtime::{ModuleLoader, PluginModule};
use super::scrobble::{
    ConnectionChangedHook, HttpListenBrainzVerifier, ListenBrainzLinkStore,
    MemoryListenBrainzLinkStore, MemoryScrobblePrefsStore, NoopMixApprovalHook, ScrobbleDeps,
    ScrobblePrefsStore, SqliteListenBrainzLinkStore, SqliteScrobblePrefsStore, StaticMixState,
};
use super::ticks::{PluginTickLoops, TICK_CANCEL_GRACE, TickLoopSync, TickStoreKind};

/// Scratch-dir sequence so parallel test states never share a store.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

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

/// Module loader without an engine. v2 imports plugin code in-process;
/// v3 has no execution engine for that yet, so every load fails the one
/// plugin with the reason — discovery, validation, settings, and dispatch
/// around it keep working, and the engine plugs in behind this seam.
#[derive(Debug, Default)]
pub struct NoEngineLoader;

impl ModuleLoader for NoEngineLoader {
    fn load(
        &self,
        _dir: &Path,
        manifest: &PluginManifest,
    ) -> Result<Arc<dyn PluginModule>, String> {
        Err(format!(
            "plugin '{}' needs an execution engine; v3 has none wired yet",
            manifest.name,
        ))
    }
}

/// Zip reader over stored and deflated entries (what GitHub serves).
/// Caps apply while streaming, before the slice's own checks: entry
/// count, per-file bytes, and total decompressed bytes, so an archive at
/// the fetch cap cannot balloon past the decompressed budget. Anything
/// outside the two methods, or a name escaping its root, refuses loudly.
#[derive(Debug, Default)]
pub struct ZipUnpacker;

impl ArchiveUnpacker for ZipUnpacker {
    fn unpack(&self, bytes: &[u8]) -> Result<Vec<ArchiveEntry>, String> {
        use std::io::Read as _;

        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|error| format!("plugin archive is not a readable zip: {error}"))?;
        if archive.len() > MAX_PLUGIN_ZIP_ENTRIES {
            return Err(format!(
                "plugin archive holds {} entries; the cap is {MAX_PLUGIN_ZIP_ENTRIES}",
                archive.len(),
            ));
        }
        let mut entries = Vec::new();
        let mut total: u64 = 0;
        for index in 0..archive.len() {
            let file = archive
                .by_index(index)
                .map_err(|error| format!("plugin archive entry {index} is unreadable: {error}"))?;
            if file.is_dir() {
                continue;
            }
            match file.compression() {
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated => {}
                method => {
                    return Err(format!(
                        "plugin archive uses {method:?} compression; only stored and deflated entries install"
                    ));
                }
            }
            let name = file.name().to_owned();
            if file.enclosed_name().is_none() {
                return Err(format!("plugin archive entry {name:?} escapes its root"));
            }
            let is_symlink = file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170_000 == 0o120_000);
            let mut data = Vec::new();
            file.take(MAX_PLUGIN_ZIP_FILE_BYTES as u64 + 1)
                .read_to_end(&mut data)
                .map_err(|error| format!("plugin archive entry is unreadable: {error}"))?;
            if data.len() > MAX_PLUGIN_ZIP_FILE_BYTES {
                return Err(format!(
                    "plugin archive entry {name:?} tops the per-file cap"
                ));
            }
            total += data.len() as u64;
            if total > MAX_PLUGIN_ZIP_DECOMPRESSED_BYTES as u64 {
                return Err("plugin archive decompresses past the total cap".to_owned());
            }
            entries.push(ArchiveEntry {
                path: name,
                is_symlink,
                data,
            });
        }
        Ok(entries)
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

/// Everything `create_app` needs to mount the plugins slice, plus the
/// tick loops over the shared jobs registry.
#[derive(Clone)]
pub struct PluginsSetup {
    host: Arc<PluginHost>,
    plugins: PluginsDeps,
    scrobble: ScrobbleHttpDeps,
    ticks: Arc<PluginTickLoops<StoreKind, TickStoreKind>>,
}

impl PluginsSetup {
    /// Bind the production seams: the deployment's plugins directory and
    /// config store, the HTTP verifier and fetcher, cache invalidation on
    /// link changes, SQLite tick and scrobble stores over the database,
    /// and tick loops over the shared jobs registry. The host loads once
    /// here; installs and updates reload it.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        users: UsersDeps,
        http: reqwest::Client,
        config: Arc<ConfigStore>,
        ids: Arc<dyn IdGenerator>,
        crypto: Arc<Crypto>,
        plugins_dir: PathBuf,
        cache: Arc<dyn ProviderCache>,
        registry: JobRegistry<StoreKind>,
        pool: sqlx::SqlitePool,
        lane: WriteLane,
    ) -> Self {
        Self::assemble(
            users,
            http,
            config,
            ids,
            plugins_dir,
            cache,
            registry,
            TickStoreKind::Sqlite(SqliteTickStore::new(pool.clone(), lane.clone())),
            Arc::new(SqliteScrobblePrefsStore::new(pool.clone(), lane.clone())),
            Arc::new(SqliteListenBrainzLinkStore::new(pool, lane, crypto)),
        )
    }

    /// Test bundle: scratch config store and plugins directory, memory tick
    /// and scrobble stores, the production verifier and zip reader, and
    /// tick loops over the caller's registry (usually the test jobs
    /// setup's).
    pub fn for_tests(
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        registry: JobRegistry<StoreKind>,
    ) -> Result<Self, String> {
        let http = HttpClientFactory::new().map_err(|error| error.to_string())?;
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "droppedneedle-plugins-test-{}-{seq}",
            std::process::id()
        ));
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
        Ok(Self::assemble(
            users,
            http.shared().clone(),
            config,
            ids,
            dir.join("plugins"),
            cache,
            registry,
            TickStoreKind::Memory(MemoryTickStore::new()),
            Arc::new(MemoryScrobblePrefsStore::new()),
            Arc::new(MemoryListenBrainzLinkStore::new(crypto)),
        ))
    }

    /// Assemble the bundle from its parts. Production passes the SQLite
    /// stores, tests the memory ones; everything else is identical.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        users: UsersDeps,
        http: reqwest::Client,
        config: Arc<ConfigStore>,
        ids: Arc<dyn IdGenerator>,
        plugins_dir: PathBuf,
        cache: Arc<dyn ProviderCache>,
        registry: JobRegistry<StoreKind>,
        ticks: TickStoreKind,
        prefs: Arc<dyn ScrobblePrefsStore>,
        links: Arc<dyn ListenBrainzLinkStore>,
    ) -> Self {
        // Best-effort: the host reads a missing directory as empty and the
        // install path creates it, so a failure here must not fail the boot.
        let _ = std::fs::create_dir_all(&plugins_dir);
        let host = Arc::new(PluginHost::new(
            plugins_dir,
            Arc::clone(&config),
            Arc::new(NoEngineLoader),
        ));
        host.load_all();
        let roles: Arc<dyn UserRoles> = Arc::new(StoreRoles {
            users: users.clone(),
        });
        let ticks = Arc::new(PluginTickLoops::new(registry, ticks, TICK_CANCEL_GRACE));
        let plugins = PluginsDeps {
            host: Arc::clone(&host),
            config,
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
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_reader_round_trips_stored_and_deflated_entries() {
        use std::io::Write as _;

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("root/plugin.toml", stored).unwrap();
        writer.write_all(b"[plugin]").unwrap();
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("root/ui/panel.js", deflated).unwrap();
        writer
            .write_all(b"console.log(1);".repeat(64).as_slice())
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        let entries = ZipUnpacker.unpack(&bytes).unwrap();
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, ["root/plugin.toml", "root/ui/panel.js"]);
        assert_eq!(entries[0].data, b"[plugin]");
        assert!(!entries[0].is_symlink);
    }

    #[test]
    fn zip_reader_refuses_garbage_and_escapes() {
        assert!(ZipUnpacker.unpack(b"not a zip").is_err());

        use std::io::Write as _;

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("../escape.toml", options).unwrap();
        writer.write_all(b"[plugin]").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(ZipUnpacker.unpack(&bytes).is_err());
    }
}
