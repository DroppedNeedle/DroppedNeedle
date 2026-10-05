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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;

use crate::runtime_config::ConfigStore;
use crate::runtime_config::sections::PluginConfig;

use super::capabilities::events::EventState;
use super::manifest::{PluginManifest, active_capabilities, load_manifest};
use super::models::PluginSource;
use super::protocol::{RpcError, codes, methods};
use super::runtime::{
    BoxFuture, HostServices, LaunchSpec, PluginConnection, PluginLauncher, PublishPayload,
    RuntimeState, RuntimeStatus,
};

/// Folder under the plugins directory holding each plugin's data folder.
/// Hidden, so discovery skips it.
pub const DATA_FOLDER: &str = ".data";

/// Durable per-plugin key/value state, reached by plugins through
/// `host.state.get` and `host.state.set`. Production backs it with the
/// jobs tick store (SQLite), so state survives restarts and reinstalls.
pub trait PluginStateStore: Send + Sync {
    /// Read one key; `None` when unset.
    fn read<'a>(
        &'a self,
        plugin: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>>;
    /// Write one key.
    fn write<'a>(
        &'a self,
        plugin: &'a str,
        key: &'a str,
        value: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), String>>;
}

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
    /// Declared capabilities this host dispatches to (those active for the
    /// manifest's `api_version`). Empty while disabled.
    pub active_capabilities: Vec<String>,
    /// On-disk folder. It need not equal the manifest name; uninstall
    /// removes exactly this one.
    pub directory: String,
    /// The running plugin, present only while enabled.
    pub runtime: Option<Arc<dyn PluginConnection>>,
}

impl LoadedPlugin {
    /// Whether calls for `capability` should go to this plugin.
    pub fn serves(&self, capability: &str) -> bool {
        self.enabled
            && self.runtime.is_some()
            && self.active_capabilities.iter().any(|c| c == capability)
    }

    /// Runtime health, or `None` while disabled.
    pub fn runtime_status(&self) -> Option<RuntimeStatus> {
        self.runtime.as_ref().map(|runtime| runtime.status())
    }
}

/// Update (enable plus settings save) failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// Unknown plugin name.
    NotFound,
    /// Stored settings failed to save. The message is log-only.
    StoreFailed(String),
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
    config: Arc<ConfigStore>,
    launcher: Arc<dyn PluginLauncher>,
    plugins: Mutex<HashMap<String, LoadedPlugin>>,
    running: Mutex<HashMap<String, Running>>,
    /// Serializes reloads so two saves cannot launch the same plugin twice.
    reload: Mutex<()>,
    generation: AtomicU64,
    state: Mutex<Option<Arc<dyn PluginStateStore>>>,
    /// Subscriber fan-out and publish bookkeeping.
    pub(super) events: EventState,
}

impl PluginHost {
    /// Build a host over one plugins directory. Nothing loads until
    /// [`load_all`](PluginHost::load_all) runs.
    pub fn new(
        plugins_dir: PathBuf,
        config: Arc<ConfigStore>,
        launcher: Arc<dyn PluginLauncher>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            dir: plugins_dir,
            config,
            launcher,
            plugins: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
            reload: Mutex::new(()),
            generation: AtomicU64::new(0),
            state: Mutex::new(None),
            events: EventState::default(),
        })
    }

    /// Back the `host.state.*` requests with a durable store.
    pub fn set_state_store(&self, store: Arc<dyn PluginStateStore>) {
        if let Ok(mut slot) = self.state.lock() {
            *slot = Some(store);
        }
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
        self.generation.load(Ordering::Relaxed)
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
        self.generation.fetch_add(1, Ordering::Relaxed);
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
            Ok(stored) => stored.settings,
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
        let active_set = active_capabilities(manifest.api_version);
        let active: Vec<String> = manifest
            .capabilities
            .iter()
            .filter(|capability| {
                let active = active_set.contains(&capability.as_str());
                if !active {
                    tracing::info!(
                        plugin = %manifest.name,
                        %capability,
                        "capability reserved for api_version 1, not active"
                    );
                }
                active
            })
            .cloned()
            .collect();
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

    /// Every discovered plugin, sorted by name.
    pub fn list_plugins(&self) -> Vec<LoadedPlugin> {
        let mut plugins = self
            .plugins
            .lock()
            .map(|guard| guard.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        plugins.sort_by(|left, right| left.manifest.name.cmp(&right.manifest.name));
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
    pub fn serving(&self, capability: &str) -> Vec<LoadedPlugin> {
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
                if cap.id == "download_client" && !cap.display_name.is_empty() {
                    display = cap.display_name.clone();
                }
                if cap.id == "indexer" && !cap.target_source.is_empty() {
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
            let health = source_health(plugin.runtime_status().as_ref());
            let target = if target_source.is_empty() {
                key.clone()
            } else {
                target_source.clone()
            };
            match by_key.get_mut(&key) {
                Some(existing) => {
                    existing.has_client = existing.has_client || is_client;
                    existing.has_indexer = existing.has_indexer || is_indexer;
                    existing.configured = existing.configured || plugin.enabled;
                    if is_client {
                        existing.plugin = manifest.name.clone();
                        existing.display_name = display;
                        existing.health = health.to_owned();
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
                            configured: plugin.enabled,
                            health: health.to_owned(),
                        },
                    );
                }
            }
        };
        for plugin in self.serving("download_client") {
            seen(&plugin, true, false);
        }
        for plugin in self.serving("indexer") {
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
            .save_plugin(name, PluginConfig { enabled, settings }, &secret_keys)
            .map_err(|error| {
                tracing::warn!(plugin = %name, %error, "plugin settings save failed");
                UpdateError::StoreFailed(error.to_string())
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
        self.serving("scheduler")
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

    fn state_store(&self) -> Option<Arc<dyn PluginStateStore>> {
        self.state.lock().ok().and_then(|slot| slot.clone())
    }
}

/// Source health from runtime health: a closed set the UI understands.
fn source_health(status: Option<&RuntimeStatus>) -> &'static str {
    match status.map(|status| status.state) {
        Some(RuntimeState::Running) => "ok",
        Some(RuntimeState::Starting) => "unknown",
        Some(RuntimeState::Restarting) => "degraded",
        Some(RuntimeState::Failed) => "error",
        Some(RuntimeState::Stopped) | None => "unknown",
    }
}

/// Code identity for one plugin folder: a reinstall rewrites `plugin.toml`,
/// so its modification time changes whenever the code does.
fn fingerprint(plugin_dir: &Path, manifest: &PluginManifest) -> String {
    let modified = std::fs::metadata(plugin_dir.join("plugin.toml"))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!(
        "{}|{}|{}|{:?}|{modified}",
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
    crate::jobs::plugin_ticks::validate_state_key(key).map_err(|_| {
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
                    let store = host.state_store().ok_or_else(|| {
                        RpcError::new(codes::HOST_REFUSED, "durable state is not available")
                    })?;
                    let value = store
                        .read(plugin, &key)
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
                    let store = host.state_store().ok_or_else(|| {
                        RpcError::new(codes::HOST_REFUSED, "durable state is not available")
                    })?;
                    store
                        .write(plugin, &key, value.as_bytes().to_vec())
                        .await
                        .map_err(|reason| RpcError::new(codes::HOST_REFUSED, reason))?;
                    Ok(serde_json::json!({}))
                }
                other => Err(RpcError::new(
                    codes::METHOD_NOT_FOUND,
                    format!("the host has no method {other}"),
                )),
            }
        })
    }
}
