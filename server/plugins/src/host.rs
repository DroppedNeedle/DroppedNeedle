//! PluginHost: discover plugins, keep one runtime per enabled plugin, and
//! answer the plugins' own requests.
//!
//! Trust model, plainly: a plugin is third-party code. Nothing runs until
//! its manifest validates and an admin enables it. Dropping a folder in
//! the plugins directory runs nothing, and installing from GitHub stores
//! code without running it: the install arrives disabled. Once enabled, a
//! plugin runs as its own process with the server user's file and network
//! access (see `process.rs` for the limits it does get).
//!
//! Failure isolation: a plugin that fails to start, crashes, hangs, or
//! answers with garbage is recorded and skipped. It never crashes the
//! server or the flow that called it.
//!
//! The capability fan-outs live beside their capability in
//! `capabilities/`; install and update from GitHub live in `install.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;

use droppedneedle_kernel::jobs::plugin_ticks::{SqliteTickStore, TickStoreError};
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::sections::PluginConfig;

use super::capabilities::events::EventState;
use super::manifest::{Capability, PluginManifest, load_manifest};
use super::models::{PluginHealth, PluginHealthStatus, PluginSource, PluginSourceHealth};
use super::protocol::{RpcError, codes, methods};
use super::runtime::{
    BoxFuture, HostServices, LaunchSpec, PluginConnection, PluginLauncher, PublishPayload,
    RuntimeState, RuntimeStatus,
};

/// Folder under the plugins directory holding each plugin's data folder.
/// Hidden, so discovery skips it.
pub const DATA_FOLDER: &str = ".data";

/// A list read asks a plugin for its health again when the last answer is
/// older than this.
const HEALTH_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// Most entries of a plugin folder the code fingerprint reads.
const FINGERPRINT_ENTRIES_MAX: usize = 4096;

/// One discovered plugin.
#[derive(Clone)]
pub struct LoadedPlugin {
    /// Validated manifest (placeholder with the error set when invalid).
    pub manifest: PluginManifest,
    /// Whether an admin enabled the plugin and it was launched.
    pub enabled: bool,
    /// Load failure: an invalid manifest, unreadable settings, or a
    /// launch that could not start.
    pub error: Option<String>,
    /// Declared capabilities this host dispatches to. Empty while disabled.
    pub active_capabilities: Vec<Capability>,
    /// On-disk folder. It need not equal the manifest name; uninstall
    /// removes exactly this one.
    pub directory: String,
    /// The running plugin, present only while enabled.
    pub runtime: Option<Arc<dyn PluginConnection>>,
}

impl LoadedPlugin {
    /// Whether calls for `capability` should go to this plugin.
    pub fn serves(&self, capability: Capability) -> bool {
        self.enabled && self.runtime.is_some() && self.active_capabilities.contains(&capability)
    }

    /// Runtime health, or `None` while disabled.
    pub fn runtime_status(&self) -> Option<RuntimeStatus> {
        self.runtime.as_ref().map(|runtime| runtime.status())
    }

    /// The plugin's own health answer in its current run, once it gave one.
    pub fn health(&self) -> Option<PluginHealth> {
        self.runtime.as_ref().and_then(|runtime| runtime.health())
    }
}

/// Update (enable plus settings save) failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// Unknown plugin name.
    NotFound,
    /// Stored settings failed to save. The message is log-only.
    StoreFailed(String),
    /// The database was too busy, or stopping, to take the save. The
    /// message is log-only.
    Busy(String),
}

/// Uninstall failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallError {
    /// Unknown plugin name.
    NotFound,
}

/// One desired scheduler loop, resolved from the manifest. The jobs
/// layer clamps the interval again through its own rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredTick {
    /// Plugin name.
    pub name: String,
    /// Tick interval in minutes.
    pub interval_minutes: i64,
    /// Fire once right away instead of waiting out the interval.
    pub run_on_load: bool,
}

/// A launched plugin plus what it was launched with, so a reload can tell
/// "same code, new settings" from "new code".
struct Running {
    runtime: Arc<dyn PluginConnection>,
    fingerprint: String,
    settings: HashMap<String, String>,
}

/// The plugin host. Shared through `Arc`; all mutable state sits behind
/// locks so dispatches never see a half-rebuilt registry.
pub struct PluginHost {
    me: Weak<PluginHost>,
    dir: PathBuf,
    pub(super) config: Arc<ConfigStore>,
    launcher: Arc<dyn PluginLauncher>,
    plugins: Mutex<HashMap<String, LoadedPlugin>>,
    running: Mutex<HashMap<String, Running>>,
    /// Serializes reloads so two saves cannot launch the same plugin twice.
    reload: Mutex<()>,
    /// Bumped after every reload; acquisition wakes its download worker
    /// on it, so enabling a download client takes effect at once.
    generation: tokio::sync::watch::Sender<u64>,
    /// Durable per-plugin key/value state behind the `host.state.*`
    /// requests, shared with the tick loops, so state survives restarts
    /// and reinstalls.
    state: SqliteTickStore,
    /// Subscriber fan-out and publish bookkeeping.
    pub(super) events: EventState,
    /// Library writes per plugin (`host.library.write`).
    pub(super) library_writes: super::capabilities::library_files::LibraryWrites,
    /// The library's write side: plugin files go through its publisher.
    pub(super) library: droppedneedle_library::wiring::LibraryCore,
}

impl PluginHost {
    /// Build a host over one plugins directory. Nothing loads until
    /// [`load_all`](PluginHost::load_all) runs. `state` backs the
    /// `host.state.*` requests. Plugin library writes go through the
    /// library's publisher in `library`, the one the library holds.
    pub fn new(
        plugins_dir: PathBuf,
        config: Arc<ConfigStore>,
        launcher: Arc<dyn PluginLauncher>,
        state: SqliteTickStore,
        library: droppedneedle_library::wiring::LibraryCore,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            dir: plugins_dir,
            config,
            launcher,
            plugins: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
            reload: Mutex::new(()),
            generation: tokio::sync::watch::Sender::new(0),
            state,
            events: EventState::default(),
            library_writes: Default::default(),
            library,
        })
    }

    /// Plugins directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// One plugin's data folder (its working directory when running).
    pub fn data_dir(&self, name: &str) -> PathBuf {
        self.dir.join(DATA_FOLDER).join(name)
    }

    /// Config store holding plugin settings.
    pub fn config(&self) -> &Arc<ConfigStore> {
        &self.config
    }

    /// Monotonic rebuild counter, bumped after every `load_all` swap.
    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }

    /// A notice that changes after every `load_all` swap.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.generation.subscribe()
    }

    /// Discover every plugin folder, then make the running set match:
    /// launch newly enabled plugins, relaunch ones whose code changed,
    /// send new settings to the rest, and stop anything disabled or gone.
    /// The registry builds aside and swaps atomically. Runs at startup and
    /// after every install, update, or uninstall.
    pub fn load_all(&self) {
        let Ok(_reload) = self.reload.lock() else {
            tracing::error!("plugin reload lock poisoned; reload skipped");
            return;
        };
        let mut plugins: HashMap<String, LoadedPlugin> = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            let mut children: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_dir())
                .collect();
            children.sort();
            for child in children {
                let hidden = child
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with('_') || name.starts_with('.'))
                    .unwrap_or(true);
                if hidden {
                    continue;
                }
                self.load_one(&child, &mut plugins);
            }
        }
        // Stop every runtime the new registry no longer holds.
        let stale: Vec<Arc<dyn PluginConnection>> = match self.running.lock() {
            Ok(mut running) => {
                let gone: Vec<String> = running
                    .keys()
                    .filter(|name| {
                        plugins
                            .get(*name)
                            .is_none_or(|plugin| plugin.runtime.is_none())
                    })
                    .cloned()
                    .collect();
                gone.into_iter()
                    .filter_map(|name| running.remove(&name).map(|entry| entry.runtime))
                    .collect()
            }
            Err(_) => Vec::new(),
        };
        for runtime in stale {
            stop_in_background(runtime);
        }
        if let Ok(mut guard) = self.plugins.lock() {
            *guard = plugins;
        }
        self.generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    fn load_one(&self, plugin_dir: &Path, plugins: &mut HashMap<String, LoadedPlugin>) {
        let dir_name = plugin_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("plugin")
            .to_owned();
        let directory = plugin_dir.to_string_lossy().into_owned();
        let manifest = match load_manifest(plugin_dir) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::warn!(%error, "plugin manifest invalid");
                plugins.insert(
                    dir_name.clone(),
                    LoadedPlugin {
                        manifest: PluginManifest {
                            name: dir_name,
                            ..PluginManifest::default()
                        },
                        enabled: false,
                        error: Some(error.to_string()),
                        active_capabilities: Vec::new(),
                        directory,
                        runtime: None,
                    },
                );
                return;
            }
        };
        let disabled = |manifest: PluginManifest, error: Option<String>| LoadedPlugin {
            manifest,
            enabled: false,
            error,
            active_capabilities: Vec::new(),
            directory: directory.clone(),
            runtime: None,
        };
        if plugins.contains_key(&manifest.name) {
            let error = format!(
                "another folder already holds a plugin named '{}'",
                manifest.name
            );
            tracing::warn!(folder = %dir_name, %error, "plugin skipped");
            return;
        }
        // Fail closed: unreadable stored state means disabled, never enabled.
        let enabled = self
            .config
            .get_plugin(&manifest.name)
            .map(|stored| stored.enabled)
            .unwrap_or(false);
        if !enabled {
            plugins.insert(manifest.name.clone(), disabled(manifest, None));
            return;
        }
        let settings = match self
            .config
            .get_plugin_raw(&manifest.name, &manifest.secret_keys())
        {
            Ok(stored) => stored.into_settings(),
            Err(error) => {
                tracing::error!(plugin = %manifest.name, %error, "plugin settings unreadable");
                let reason = "Saved settings could not be read; save them again".to_owned();
                plugins.insert(manifest.name.clone(), disabled(manifest, Some(reason)));
                return;
            }
        };
        let runtime = match self.ensure_running(plugin_dir, &manifest, settings) {
            Ok(runtime) => runtime,
            Err(reason) => {
                tracing::error!(plugin = %manifest.name, %reason, "plugin failed to start");
                let reason = format!("Failed to start: {reason}");
                plugins.insert(manifest.name.clone(), disabled(manifest, Some(reason)));
                return;
            }
        };
        let active = manifest.capabilities.clone();
        plugins.insert(
            manifest.name.clone(),
            LoadedPlugin {
                manifest,
                enabled: true,
                error: None,
                active_capabilities: active,
                directory,
                runtime: Some(runtime),
            },
        );
    }

    /// Keep or (re)launch the runtime for one enabled plugin.
    fn ensure_running(
        &self,
        plugin_dir: &Path,
        manifest: &PluginManifest,
        settings: HashMap<String, String>,
    ) -> Result<Arc<dyn PluginConnection>, String> {
        let fingerprint = fingerprint(plugin_dir, manifest);
        let mut running = self
            .running
            .lock()
            .map_err(|_| "plugin registry lock poisoned".to_owned())?;
        if let Some(entry) = running.get_mut(&manifest.name) {
            let usable = entry.fingerprint == fingerprint
                && !matches!(
                    entry.runtime.status().state,
                    RuntimeState::Stopped | RuntimeState::Failed
                );
            if usable {
                if entry.settings != settings {
                    entry.runtime.notify(
                        methods::SETTINGS_UPDATE,
                        serde_json::json!({ "settings": settings }),
                    );
                    entry.settings = settings;
                }
                return Ok(Arc::clone(&entry.runtime));
            }
        }
        if let Some(old) = running.remove(&manifest.name) {
            stop_in_background(old.runtime);
        }
        let services: Arc<dyn HostServices> = Arc::new(HostRequests {
            host: self.me.clone(),
        });
        let runtime = self.launcher.launch(LaunchSpec {
            manifest: manifest.clone(),
            plugin_dir: plugin_dir.to_path_buf(),
            data_dir: self.data_dir(&manifest.name),
            settings: settings.clone(),
            services,
        })?;
        running.insert(
            manifest.name.clone(),
            Running {
                runtime: Arc::clone(&runtime),
                fingerprint,
                settings,
            },
        );
        Ok(runtime)
    }

    /// Stop every plugin process. Server shutdown calls this.
    pub async fn stop_all(&self) {
        let runtimes: Vec<Arc<dyn PluginConnection>> = match self.running.lock() {
            Ok(mut running) => running.drain().map(|(_, entry)| entry.runtime).collect(),
            Err(_) => Vec::new(),
        };
        if let Ok(mut plugins) = self.plugins.lock() {
            for plugin in plugins.values_mut() {
                plugin.runtime = None;
                plugin.enabled = false;
            }
        }
        let stops = runtimes.iter().map(|runtime| runtime.stop());
        futures_util::future::join_all(stops).await;
    }

    /// Every discovered plugin, sorted by name. A stale health answer is
    /// asked again in the background; the list never waits for it.
    pub fn list_plugins(&self) -> Vec<LoadedPlugin> {
        let mut plugins = self
            .plugins
            .lock()
            .map(|guard| guard.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        plugins.sort_by(|left, right| left.manifest.name.cmp(&right.manifest.name));
        for plugin in &plugins {
            if let Some(runtime) = &plugin.runtime {
                runtime.refresh_health(HEALTH_MAX_AGE);
            }
        }
        plugins
    }

    /// One plugin by manifest name.
    pub fn get(&self, name: &str) -> Option<LoadedPlugin> {
        self.plugins
            .lock()
            .ok()
            .and_then(|guard| guard.get(name).cloned())
    }

    /// Enabled plugins serving one capability, sorted by name so fan-outs
    /// and first-hit lookups are deterministic.
    pub fn serving(&self, capability: Capability) -> Vec<LoadedPlugin> {
        let mut plugins: Vec<LoadedPlugin> = self
            .plugins
            .lock()
            .map(|guard| {
                guard
                    .values()
                    .filter(|plugin| plugin.serves(capability))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        plugins.sort_by(|left, right| left.manifest.name.cmp(&right.manifest.name));
        plugins
    }

    /// Enabled plugin acquisition sources, merged by source key.
    pub fn plugin_sources(&self) -> Vec<PluginSource> {
        let mut by_key: HashMap<String, PluginSource> = HashMap::new();
        let mut seen = |plugin: &LoadedPlugin, is_client: bool, is_indexer: bool| {
            let manifest = &plugin.manifest;
            let mut display = manifest.display_name.clone();
            let mut target_source = String::new();
            for cap in &manifest.capability_configs {
                if cap.id == Capability::DownloadClient && !cap.display_name.is_empty() {
                    display = cap.display_name.clone();
                }
                if cap.id == Capability::Indexer && !cap.target_source.is_empty() {
                    target_source = cap.target_source.clone();
                }
            }
            // One plugin, one source key: `plugin:<manifest-name>`. An
            // indexer-only plugin lists under the source it feeds.
            let key = if is_client || target_source.is_empty() {
                format!("plugin:{}", manifest.name)
            } else {
                target_source.clone()
            };
            if key != "usenet" && !super::manifest::valid_plugin_key(&key) {
                return;
            }
            if let Some(runtime) = &plugin.runtime {
                runtime.refresh_health(HEALTH_MAX_AGE);
            }
            let answer = plugin.health();
            let health = source_health(plugin.runtime_status().as_ref(), answer.as_ref());
            let configured = answer.as_ref().is_some_and(|answer| answer.configured);
            let message = answer.map(|answer| answer.message).unwrap_or_default();
            let target = if target_source.is_empty() {
                key.clone()
            } else {
                target_source.clone()
            };
            match by_key.get_mut(&key) {
                Some(existing) => {
                    existing.has_client = existing.has_client || is_client;
                    existing.has_indexer = existing.has_indexer || is_indexer;
                    existing.configured = existing.configured || configured;
                    if is_client {
                        existing.plugin = manifest.name.clone();
                        existing.display_name = display;
                        existing.health = health;
                        existing.health_message = message;
                    }
                }
                None => {
                    by_key.insert(
                        key.clone(),
                        PluginSource {
                            key,
                            plugin: manifest.name.clone(),
                            display_name: display,
                            has_client: is_client,
                            has_indexer: is_indexer,
                            target_source: target,
                            configured,
                            health,
                            health_message: message,
                        },
                    );
                }
            }
        };
        for plugin in self.serving(Capability::DownloadClient) {
            seen(&plugin, true, false);
        }
        for plugin in self.serving(Capability::Indexer) {
            seen(&plugin, false, true);
        }
        let mut sources: Vec<PluginSource> = by_key.into_values().collect();
        sources.sort_by(|left, right| left.key.cmp(&right.key));
        sources
    }

    /// Save one plugin's enable switch plus its settings. Secret-flagged
    /// values resolve the mask against the stored ciphertext and are
    /// encrypted at rest; other values store verbatim. Reloads afterwards
    /// so the change applies without a restart.
    pub fn update_settings(
        &self,
        name: &str,
        enabled: bool,
        settings: HashMap<String, String>,
    ) -> Result<LoadedPlugin, UpdateError> {
        let plugin = self.get(name).ok_or(UpdateError::NotFound)?;
        let secret_keys = plugin.manifest.secret_keys();
        self.config
            .save_plugin_blocking(name, PluginConfig { enabled, settings }, &secret_keys)
            .map_err(|error| {
                tracing::warn!(plugin = %name, %error, "plugin settings save failed");
                if error.is_busy() {
                    UpdateError::Busy(error.to_string())
                } else {
                    UpdateError::StoreFailed(error.to_string())
                }
            })?;
        self.load_all();
        self.get(name).ok_or(UpdateError::NotFound)
    }

    /// Remove the plugin's code folder. Its settings stay in config and its
    /// data folder stays on disk, so a reinstall picks up where it left off.
    pub fn uninstall(&self, name: &str) -> Result<(), UninstallError> {
        let plugin = self.get(name).ok_or(UninstallError::NotFound)?;
        let target = if plugin.directory.is_empty() {
            self.dir.join(name)
        } else {
            PathBuf::from(&plugin.directory)
        };
        if target.is_dir()
            && target.parent() == Some(self.dir.as_path())
            && let Err(error) = std::fs::remove_dir_all(&target)
        {
            tracing::warn!(plugin = %name, %error, "plugin folder not fully removed");
        }
        self.load_all();
        tracing::info!(plugin = %name, "plugin uninstalled");
        Ok(())
    }

    /// Enabled scheduler plugins mapped to their loops, sorted by name.
    pub fn desired_ticks(&self) -> Vec<DesiredTick> {
        self.serving(Capability::Scheduler)
            .into_iter()
            .map(|plugin| {
                let schedule = plugin.manifest.schedule.as_ref();
                DesiredTick {
                    name: plugin.manifest.name.clone(),
                    interval_minutes: schedule
                        .map(|schedule| schedule.interval_minutes)
                        .unwrap_or(60)
                        .clamp(5, 1440),
                    run_on_load: schedule
                        .map(|schedule| schedule.run_on_load)
                        .unwrap_or(false),
                }
            })
            .collect()
    }

    /// One setting as the running plugin got it (secrets opened), or
    /// `None` while it is not running.
    pub(super) fn running_setting(&self, name: &str, key: &str) -> Option<String> {
        self.running
            .lock()
            .ok()?
            .get(name)
            .and_then(|entry| entry.settings.get(key).cloned())
    }

    /// One plugin's state key; an unset key or a plugin the store has not
    /// met yet reads as empty.
    async fn read_state(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
        match self.state.read(plugin, key).await {
            Ok(found) => Ok(found),
            Err(TickStoreError::NotFound(_)) | Err(TickStoreError::UnknownPlugin(_)) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Store one plugin's state key, registering the plugin first.
    async fn write_state(&self, plugin: &str, key: &str, value: Vec<u8>) -> Result<(), String> {
        self.state.add_plugin(plugin);
        self.state
            .write(plugin, key, value)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Source health from the process state and, while it runs, the plugin's
/// own answer.
fn source_health(
    status: Option<&RuntimeStatus>,
    answer: Option<&PluginHealth>,
) -> PluginSourceHealth {
    match status.map(|status| status.state) {
        Some(RuntimeState::Running) => match answer.map(|answer| answer.status) {
            Some(PluginHealthStatus::Ok) => PluginSourceHealth::Ok,
            Some(PluginHealthStatus::Error) => PluginSourceHealth::Error,
            None => PluginSourceHealth::Unknown,
        },
        Some(RuntimeState::Restarting) => PluginSourceHealth::Degraded,
        Some(RuntimeState::Failed) => PluginSourceHealth::Error,
        Some(RuntimeState::Starting | RuntimeState::Stopped) | None => PluginSourceHealth::Unknown,
    }
}

/// Code identity for one plugin folder: the manifest's launch fields plus
/// every entry below the folder (relative path, size and modification
/// time), so editing any file relaunches the plugin on the next reload.
/// Links are recorded, never followed, and the walk stops after
/// [`FINGERPRINT_ENTRIES_MAX`] entries.
fn fingerprint(plugin_dir: &Path, manifest: &PluginManifest) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    let mut seen = 0usize;
    let mut folders = vec![PathBuf::new()];
    while let Some(relative) = folders.pop() {
        let mut entries: Vec<(PathBuf, std::fs::Metadata)> = match std::fs::read_dir(
            plugin_dir.join(&relative),
        ) {
            Ok(entries) => entries
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let meta = entry.metadata().ok()?;
                    Some((relative.join(entry.file_name()), meta))
                })
                .collect(),
            Err(error) => {
                tracing::warn!(folder = %plugin_dir.join(&relative).display(), %error, "plugin folder unreadable");
                continue;
            }
        };
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for (path, meta) in entries {
            if seen == FINGERPRINT_ENTRIES_MAX {
                tracing::warn!(folder = %plugin_dir.display(), "plugin folder too large to fingerprint in full");
                return finish_fingerprint(plugin_dir, manifest, hasher.finish());
            }
            seen += 1;
            path.hash(&mut hasher);
            meta.len().hash(&mut hasher);
            meta.file_type().is_symlink().hash(&mut hasher);
            meta.modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_nanos())
                .hash(&mut hasher);
            if meta.is_dir() {
                folders.push(path);
            }
        }
    }
    finish_fingerprint(plugin_dir, manifest, hasher.finish())
}

fn finish_fingerprint(plugin_dir: &Path, manifest: &PluginManifest, files: u64) -> String {
    format!(
        "{}|{}|{}|{:?}|{files:016x}",
        plugin_dir.display(),
        manifest.version,
        manifest.entrypoint,
        manifest.command
    )
}

/// Stop one runtime without blocking the caller. Outside a runtime the
/// handle's drop stops it instead.
fn stop_in_background(runtime: Arc<dyn PluginConnection>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move { runtime.stop().await });
    }
}

/// Answers plugin-to-host requests for every plugin of one host.
struct HostRequests {
    host: Weak<PluginHost>,
}

/// Largest state value (1 MiB).
const STATE_VALUE_MAX: usize = 1024 * 1024;

fn state_key(params: &Value) -> Result<String, RpcError> {
    let key = params
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default();
    droppedneedle_kernel::jobs::plugin_ticks::validate_state_key(key).map_err(|_| {
        RpcError::new(
            codes::INVALID_PARAMS,
            "state keys are 1-64 lowercase letters, digits, _ - or /, starting with a letter or digit",
        )
    })?;
    Ok(key.to_owned())
}

impl HostServices for HostRequests {
    fn handle<'a>(
        &'a self,
        plugin: &'a str,
        method: &'a str,
        params: Value,
    ) -> BoxFuture<'a, Result<Value, RpcError>> {
        Box::pin(async move {
            let Some(host) = self.host.upgrade() else {
                return Err(RpcError::new(
                    codes::HOST_REFUSED,
                    "server is shutting down",
                ));
            };
            match method {
                methods::HOST_PUBLISH => {
                    let kind = params
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let fields: HashMap<String, String> = match params.get("payload") {
                        None | Some(Value::Null) => HashMap::new(),
                        Some(Value::Object(map)) => map
                            .iter()
                            .map(|(key, value)| {
                                let text = match value {
                                    Value::String(text) => text.clone(),
                                    other => other.to_string(),
                                };
                                (key.clone(), text)
                            })
                            .collect(),
                        Some(_) => {
                            return Err(RpcError::new(
                                codes::INVALID_PARAMS,
                                "publish payload must be an object",
                            ));
                        }
                    };
                    let payload = PublishPayload::Fields(fields);
                    let principal = format!("plugin:{plugin}");
                    let causation = params.get("causation_id").and_then(Value::as_str);
                    let result = host
                        .publish_from_plugin(plugin, kind, &payload, &principal, causation)
                        .map_err(|reason| RpcError::new(codes::INVALID_PARAMS, reason))?;
                    serde_json::to_value(result)
                        .map_err(|error| RpcError::new(codes::INTERNAL_ERROR, error.to_string()))
                }
                methods::HOST_STATE_GET => {
                    let key = state_key(&params)?;
                    let value = host
                        .read_state(plugin, &key)
                        .await
                        .map_err(|reason| RpcError::new(codes::INTERNAL_ERROR, reason))?;
                    let text = value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
                    Ok(serde_json::json!({ "value": text }))
                }
                methods::HOST_STATE_SET => {
                    let key = state_key(&params)?;
                    let Some(value) = params.get("value").and_then(Value::as_str) else {
                        return Err(RpcError::new(
                            codes::INVALID_PARAMS,
                            "value must be a string",
                        ));
                    };
                    if value.len() > STATE_VALUE_MAX {
                        return Err(RpcError::new(codes::INVALID_PARAMS, "value is over 1 MiB"));
                    }
                    host.write_state(plugin, &key, value.as_bytes().to_vec())
                        .await
                        .map_err(|reason| RpcError::new(codes::HOST_REFUSED, reason))?;
                    Ok(serde_json::json!({}))
                }
                methods::HOST_LIBRARY_WRITE => {
                    let rel_path = params
                        .get("rel_path")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let data = params
                        .get("data_base64")
                        .and_then(Value::as_str)
                        .and_then(|text| {
                            use base64::Engine as _;
                            base64::engine::general_purpose::STANDARD.decode(text).ok()
                        })
                        .ok_or_else(|| {
                            RpcError::new(codes::INVALID_PARAMS, "data_base64 must be base64 text")
                        })?;
                    let (path, bundle_id) =
                        host.write_library_file(plugin, rel_path, data)
                            .await
                            .map_err(|reason| RpcError::new(codes::HOST_REFUSED, reason))?;
                    Ok(serde_json::json!({ "path": path, "bundle_id": bundle_id }))
                }
                other => Err(RpcError::new(
                    codes::METHOD_NOT_FOUND,
                    format!("the host has no method {other}"),
                )),
            }
        })
    }
}
