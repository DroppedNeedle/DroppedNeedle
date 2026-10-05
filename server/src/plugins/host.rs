//! PluginHost: discover, validate, install, and run plugins.
//!
//! Trust model, stated plainly: a plugin is third-party code with the
//! app's full privileges once loaded. Nothing loads until BOTH the manifest
//! validates AND an admin has explicitly enabled the plugin. Dropping a
//! folder in the directory alone runs no code, and installing from GitHub
//! stores code without running it: the install arrives disabled.
//!
//! Failure isolation: a plugin that fails to load, ticks badly, or answers
//! a route with garbage is recorded and skipped. It never crashes the host
//! or the flow that invoked it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::runtime_config::ConfigStore;
use crate::runtime_config::sections::PluginConfig;

use super::manifest::{PluginManifest, active_capabilities, load_manifest};
use super::models::PluginSource;
use super::runtime::{
    BoxFuture, EventKind, EventPayload, ModuleLoader, PluginEvent, PluginModule,
    PluginPublishResult, PluginPurchaseLink, PluginRouteBody, PluginRouteResponse, PublishPayload,
    PublishedRecord, ScrobbleEvent, TickContext, publish_fields,
};

/// Largest install archive the host buffers (32 MiB).
pub const MAX_PLUGIN_ZIP_BYTES: usize = 32 * 1024 * 1024;
/// Largest archive entry count.
pub const MAX_PLUGIN_ZIP_ENTRIES: usize = 2000;
/// Largest single archived file (32 MiB).
pub const MAX_PLUGIN_ZIP_FILE_BYTES: usize = 32 * 1024 * 1024;
/// Largest total unpacked bytes (256 MiB).
pub const MAX_PLUGIN_ZIP_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;
/// Per-plugin event notification budget.
const EVENT_NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);
/// Per-route handler budget.
const ROUTE_TIMEOUT: Duration = Duration::from_secs(5);
/// Ext route verbs.
const ROUTE_METHODS: &[&str] = &["GET", "POST", "DELETE"];
/// Largest `/ext/` answer body the host passes through (1 MiB).
pub const ROUTE_BODY_MAX_BYTES: usize = 1024 * 1024;
/// Purchase-link fan-in budget per plugin.
const PURCHASE_TIMEOUT: Duration = Duration::from_secs(10);
/// Publishes allowed per plugin per minute.
const PUBLISH_RATE_LIMIT: usize = 30;
/// Publish rate window.
const PUBLISH_RATE_WINDOW: Duration = Duration::from_secs(60);
/// Cap on tracked (plugin, principal) rate buckets.
const PUBLISH_RATE_CAP: usize = 10_000;
/// Cap on remembered causations.
const CAUSATION_CAP: usize = 10_000;
/// How long a causation dedups.
const CAUSATION_TTL: Duration = Duration::from_secs(10 * 60);
/// Lazy causation sweep cadence.
const CAUSATION_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// Queued publish hints; overflow drops the oldest.
const PUBLISH_QUEUE_MAX: usize = 100;
/// Largest publisher note (1 KiB).
const PUBLISH_NOTE_MAX_BYTES: usize = 1024;

tokio::task_local! {
    /// Depth of the current subscriber fan-out. A publish from inside
    /// `on_event` runs at depth 1 and is enqueued once; anything deeper
    /// is dropped and logged.
    static PUBLISH_DEPTH: u32;
    /// Causation of the event currently being delivered, so a depth-1
    /// publish inherits it instead of the caller-supplied id. Rotating
    /// causations cannot dodge the (causation, subscriber) dedup this way.
    static PUBLISH_CAUSATION: Option<String>;
}

fn current_depth() -> u32 {
    PUBLISH_DEPTH.try_with(|depth| *depth).unwrap_or(0)
}

fn current_causation() -> Option<String> {
    PUBLISH_CAUSATION
        .try_with(|causation| causation.clone())
        .unwrap_or(None)
}

/// One discovered plugin.
#[derive(Clone)]
pub struct LoadedPlugin {
    /// Validated manifest (placeholder with the error set when invalid).
    pub manifest: PluginManifest,
    /// Whether the plugin is enabled and loaded.
    pub enabled: bool,
    /// Load failure, when the plugin failed to validate or instantiate.
    pub error: Option<String>,
    /// Manifest capabilities the module actually implements.
    pub active_capabilities: Vec<String>,
    /// On-disk folder. It need not equal the manifest name; uninstall
    /// removes exactly this one.
    pub directory: String,
    /// Live module, present only for enabled plugins.
    pub module: Option<Arc<dyn PluginModule>>,
}

/// Fetch one URL for installs. `None` means the ref does not exist (try
/// the next); anything else is the archive bytes, capped while streaming.
pub trait ArchiveFetcher: Send + Sync {
    /// GET one URL, enforcing the install cap while streaming.
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>>;
}

/// Production archive fetcher over `reqwest`.
pub struct ReqwestFetcher {
    http: reqwest::Client,
}

impl ReqwestFetcher {
    /// Wrap a shared HTTP client.
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
}

impl ArchiveFetcher for ReqwestFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move {
            let mut response = self
                .http
                .get(url)
                .send()
                .await
                .map_err(|error| format!("plugin download failed: {error}"))?;
            if response.status() != reqwest::StatusCode::OK {
                return Ok(None);
            }
            if let Some(declared) = response.content_length()
                && declared > MAX_PLUGIN_ZIP_BYTES as u64
            {
                return Err("That repository is too large to install as a plugin".to_owned());
            }
            let mut body = Vec::new();
            loop {
                let chunk = response
                    .chunk()
                    .await
                    .map_err(|error| format!("plugin download failed: {error}"))?;
                let Some(chunk) = chunk else {
                    break;
                };
                body.extend_from_slice(&chunk);
                if body.len() > MAX_PLUGIN_ZIP_BYTES {
                    return Err("That repository is too large to install as a plugin".to_owned());
                }
            }
            Ok(Some(body))
        })
    }
}

/// One file inside an install archive.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// Slash-separated path inside the archive.
    pub path: String,
    /// Whether the entry is a symlink (always refused).
    pub is_symlink: bool,
    /// File bytes.
    pub data: Vec<u8>,
}

/// Unpack an install archive into entries. GitHub serves deflated zips,
/// which need a zip crate the slice does not vendor; the integrator wires
/// the real reader here and tests use scripted entries.
pub trait ArchiveUnpacker: Send + Sync {
    /// List every file in the archive.
    fn unpack(&self, bytes: &[u8]) -> Result<Vec<ArchiveEntry>, String>;
}

/// Every way an install can fail. All but `Io` render as user-facing 400s
/// with the message below; `Io` is a 500 with a fixed body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// The URL is not a public GitHub repository URL.
    InvalidUrl,
    /// No ref downloaded.
    Download,
    /// Archive over the cap.
    TooLarge,
    /// Too many archived files.
    TooManyFiles,
    /// One archived file over the cap.
    OversizedFile,
    /// Traversal or absolute paths inside the archive.
    UnsafePaths,
    /// Symlinks inside the archive.
    Symlinks,
    /// Archive layout is not one top-level directory.
    Layout,
    /// No `plugin.toml` at the repository root.
    NoManifest,
    /// The manifest failed validation.
    InvalidManifest(String),
    /// The archive did not unpack. v2 lets a corrupt zip bubble to a 500
    /// here; v3 answers 400, since a corrupt download is a caller problem.
    ArchiveUnreadable(String),
    /// Local filesystem failure. The message is log-only.
    Io(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::InvalidUrl => {
                f.write_str("Enter a public GitHub repository URL, e.g. https://github.com/owner/repo")
            }
            InstallError::Download => f.write_str(
                "Could not download that repository - check the URL is public and the branch exists",
            ),
            InstallError::TooLarge => {
                f.write_str("That repository is too large to install as a plugin")
            }
            InstallError::TooManyFiles => f.write_str("That repository has too many files"),
            InstallError::OversizedFile => {
                f.write_str("That repository contains an oversized file")
            }
            InstallError::UnsafePaths => f.write_str("The archive contains unsafe paths"),
            InstallError::Symlinks => f.write_str("The archive contains symlinks"),
            InstallError::Layout => f.write_str("Unexpected archive layout"),
            InstallError::NoManifest => f.write_str(
                "No plugin.toml at the repository root - this is not a DroppedNeedle plugin",
            ),
            InstallError::InvalidManifest(reason) => f.write_str(reason),
            InstallError::ArchiveUnreadable(reason) => f.write_str(reason),
            InstallError::Io(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for InstallError {}

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
/// layer clamps the interval again through its own rule; the host clamp
/// below keeps the value sane for any other reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredTick {
    /// Plugin name.
    pub name: String,
    /// Tick interval in minutes.
    pub interval_minutes: i64,
    /// Fire once right away instead of waiting out the interval.
    pub run_on_load: bool,
}

/// The plugin host. Cloneable through `Arc`; all mutable state sits behind
/// locks so dispatches never see a half-rebuilt registry.
pub struct PluginHost {
    dir: PathBuf,
    config: Arc<ConfigStore>,
    loader: Arc<dyn ModuleLoader>,
    plugins: Mutex<HashMap<String, LoadedPlugin>>,
    generation: AtomicU64,
    inflight: Mutex<HashSet<String>>,
    dropped_events: Mutex<HashMap<String, u64>>,
    seen_causations: Mutex<HashMap<(String, String), Instant>>,
    causation_sweep_at: Mutex<Instant>,
    publish_rate: Mutex<HashMap<(String, String), Vec<Instant>>>,
    publish_queue: Mutex<std::collections::VecDeque<PublishedRecord>>,
    publish_dropped: AtomicU64,
}

impl PluginHost {
    /// Build a host over one plugins directory. Nothing loads until
    /// [`load_all`](PluginHost::load_all) runs.
    pub fn new(
        plugins_dir: PathBuf,
        config: Arc<ConfigStore>,
        loader: Arc<dyn ModuleLoader>,
    ) -> Self {
        Self {
            dir: plugins_dir,
            config,
            loader,
            plugins: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            inflight: Mutex::new(HashSet::new()),
            dropped_events: Mutex::new(HashMap::new()),
            seen_causations: Mutex::new(HashMap::new()),
            causation_sweep_at: Mutex::new(Instant::now()),
            publish_rate: Mutex::new(HashMap::new()),
            publish_queue: Mutex::new(std::collections::VecDeque::new()),
            publish_dropped: AtomicU64::new(0),
        }
    }

    /// Plugins directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Monotonic rebuild counter, bumped after every atomic `load_all` swap.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Per-plugin skip-if-pending drops, surfaced in plugin health.
    pub fn dropped_event_counts(&self) -> HashMap<String, u64> {
        self.dropped_events
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Queue-full drop-oldest evictions from `publish_from_plugin`.
    pub fn publish_dropped_count(&self) -> u64 {
        self.publish_dropped.load(Ordering::Relaxed)
    }

    /// Discover every plugin folder and load the manifest-valid,
    /// admin-enabled ones. The registry builds aside and swaps atomically,
    /// so a dispatch never sees a half-populated map. Runs at startup and
    /// after every install, update, or uninstall.
    pub fn load_all(&self) {
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
                Self::load_one(&self.config, self.loader.as_ref(), &child, &mut plugins);
            }
        }
        if let Ok(mut guard) = self.plugins.lock() {
            *guard = plugins;
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    fn load_one(
        config: &ConfigStore,
        loader: &dyn ModuleLoader,
        plugin_dir: &Path,
        plugins: &mut HashMap<String, LoadedPlugin>,
    ) {
        let dir_name = plugin_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("plugin")
            .to_owned();
        let manifest = match load_manifest(plugin_dir) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::warn!(%error, "plugin manifest invalid");
                plugins.insert(
                    dir_name.clone(),
                    LoadedPlugin {
                        manifest: PluginManifest {
                            name: dir_name.clone(),
                            ..PluginManifest::default()
                        },
                        enabled: false,
                        error: Some(error.to_string()),
                        active_capabilities: Vec::new(),
                        directory: plugin_dir.to_string_lossy().into_owned(),
                        module: None,
                    },
                );
                return;
            }
        };
        // Fail closed: unreadable stored state means disabled, never enabled.
        let enabled = config
            .get_plugin(&manifest.name)
            .map(|stored| stored.enabled)
            .unwrap_or(false);
        if !enabled {
            plugins.insert(
                manifest.name.clone(),
                LoadedPlugin {
                    manifest,
                    enabled: false,
                    error: None,
                    active_capabilities: Vec::new(),
                    directory: plugin_dir.to_string_lossy().into_owned(),
                    module: None,
                },
            );
            return;
        }
        let module = match loader.load(plugin_dir, &manifest) {
            Ok(module) => module,
            Err(reason) => {
                tracing::error!(plugin = %manifest.name, %reason, "plugin failed to load");
                plugins.insert(
                    manifest.name.clone(),
                    LoadedPlugin {
                        manifest,
                        enabled: false,
                        error: Some(format!("Failed to load: {reason}")),
                        active_capabilities: Vec::new(),
                        directory: plugin_dir.to_string_lossy().into_owned(),
                        module: None,
                    },
                );
                return;
            }
        };
        let active_set = active_capabilities(manifest.api_version);
        let mut active = Vec::new();
        for capability in &manifest.capabilities {
            if !active_set.contains(&capability.as_str()) {
                tracing::info!(
                    plugin = %manifest.name,
                    %capability,
                    "capability reserved, not active yet"
                );
                continue;
            }
            if module.provides(capability) {
                active.push(capability.clone());
            } else {
                tracing::warn!(
                    plugin = %manifest.name,
                    %capability,
                    "capability declared but not implemented"
                );
            }
        }
        plugins.insert(
            manifest.name.clone(),
            LoadedPlugin {
                manifest,
                enabled: true,
                error: None,
                active_capabilities: active,
                directory: plugin_dir.to_string_lossy().into_owned(),
                module: Some(module),
            },
        );
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

    fn with_capability(&self, capability: &str) -> Vec<LoadedPlugin> {
        self.plugins
            .lock()
            .map(|guard| {
                guard
                    .values()
                    .filter(|plugin| {
                        plugin.enabled
                            && plugin.module.is_some()
                            && plugin.active_capabilities.iter().any(|c| c == capability)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Enabled plugins with an active `purchase_links` capability.
    pub fn purchase_providers(&self) -> Vec<LoadedPlugin> {
        self.with_capability("purchase_links")
    }

    /// Enabled plugins with an active `download_client` capability.
    pub fn download_clients(&self) -> Vec<LoadedPlugin> {
        self.with_capability("download_client")
    }

    /// Enabled plugins with an active `indexer` capability.
    pub fn indexers(&self) -> Vec<LoadedPlugin> {
        self.with_capability("indexer")
    }

    /// Enabled plugins with an active `subscriber` capability.
    pub fn subscribers(&self) -> Vec<LoadedPlugin> {
        self.with_capability("subscriber")
    }

    /// Enabled plugins with an active `publisher` capability.
    pub fn publishers(&self) -> Vec<LoadedPlugin> {
        self.with_capability("publisher")
    }

    /// Enabled plugins with an active `metadata_provider` capability.
    pub fn metadata_providers(&self) -> Vec<LoadedPlugin> {
        self.with_capability("metadata_provider")
    }

    /// Enabled plugin acquisition sources, merged by source key.
    pub fn plugin_sources(&self) -> Vec<PluginSource> {
        let mut by_key: HashMap<String, PluginSource> = HashMap::new();
        let mut seen = |plugin: &LoadedPlugin, is_client: bool, is_indexer: bool| {
            let manifest = &plugin.manifest;
            let mut source = String::new();
            let mut target_source = String::new();
            let mut display = if manifest.display_name.is_empty() {
                manifest.name.clone()
            } else {
                manifest.display_name.clone()
            };
            for cap in &manifest.capability_configs {
                if cap.id == "download_client" && !cap.source.is_empty() {
                    source = cap.source.clone();
                    if !cap.display_name.is_empty() {
                        display = cap.display_name.clone();
                    }
                }
                if cap.id == "indexer" && !cap.target_source.is_empty() {
                    target_source = cap.target_source.clone();
                }
            }
            let key = if !source.is_empty() {
                format!("plugin:{source}")
            } else {
                target_source.clone()
            };
            if !super::manifest::valid_plugin_key(&key) {
                return;
            }
            let health = if plugin.enabled && plugin.error.is_none() {
                "ok"
            } else if plugin.enabled {
                "degraded"
            } else {
                "unknown"
            };
            let target = sanitize_target_source(&target_source, &key);
            match by_key.get_mut(&key) {
                Some(existing) => {
                    existing.has_client = existing.has_client || is_client;
                    existing.has_indexer = existing.has_indexer || is_indexer;
                    existing.configured =
                        existing.configured || (plugin.enabled && (is_client || is_indexer));
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
                            configured: plugin.enabled && (is_client || is_indexer),
                            health: health.to_owned(),
                        },
                    );
                }
            }
        };
        for plugin in self.download_clients() {
            seen(&plugin, true, false);
        }
        for plugin in self.indexers() {
            seen(&plugin, false, true);
        }
        let mut sources: Vec<PluginSource> = by_key.into_values().collect();
        sources.sort_by(|left, right| left.key.cmp(&right.key));
        sources
    }
}

/// Keep `usenet` and plugin keys, else fall back to a valid key, else
/// `unknown`. Anything else would lie to the source picker.
fn sanitize_target_source(value: &str, fallback: &str) -> String {
    if value == "usenet" || super::manifest::valid_plugin_key(value) {
        return value.to_owned();
    }
    if super::manifest::valid_plugin_key(fallback) {
        return fallback.to_owned();
    }
    "unknown".to_owned()
}

/// Owner/repo path segments: word characters, dots, and dashes.
fn valid_github_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// Branch ref segments: word characters, dots, dashes, and slashes.
fn valid_github_ref(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' || c == '/')
}

/// Parse a public GitHub repository URL into owner, repo, and candidate
/// refs. Mirrors v2's install URL rule, including the trailing-slash and
/// `.git` tolerance.
fn parse_github_url(url: &str) -> Result<(String, String, Vec<String>), InstallError> {
    let rest = url
        .trim()
        .strip_prefix("https://github.com/")
        .ok_or(InstallError::InvalidUrl)?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let mut parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 2 {
        return Err(InstallError::InvalidUrl);
    }
    let owner = parts.remove(0).to_owned();
    let mut repo = parts.remove(0).to_owned();
    if let Some(stripped) = repo.strip_suffix(".git") {
        repo = stripped.to_owned();
    }
    if !valid_github_segment(&owner) || !valid_github_segment(&repo) {
        return Err(InstallError::InvalidUrl);
    }
    let refs = if parts.is_empty() {
        vec!["main".to_owned(), "master".to_owned()]
    } else {
        if parts.remove(0) != "tree" || parts.is_empty() {
            return Err(InstallError::InvalidUrl);
        }
        let reference = parts.join("/");
        if !valid_github_ref(&reference) {
            return Err(InstallError::InvalidUrl);
        }
        vec![reference]
    };
    if owner.contains("..") || repo.contains("..") || refs.iter().any(|r| r.contains("..")) {
        return Err(InstallError::InvalidUrl);
    }
    Ok((owner, repo, refs))
}

fn codeload_url(owner: &str, repo: &str, reference: &str) -> String {
    format!("https://codeload.github.com/{owner}/{repo}/zip/refs/heads/{reference}")
}

impl PluginHost {
    /// Download a public GitHub repo's default (or given) branch as a zip.
    /// Returns the archive bytes; unpacking and validation happen in
    /// [`install_archive`](PluginHost::install_archive), which callers run
    /// off the async runtime.
    pub async fn fetch_plugin_archive(
        url: &str,
        fetcher: &dyn ArchiveFetcher,
    ) -> Result<Vec<u8>, InstallError> {
        let (owner, repo, refs) = parse_github_url(url)?;
        for reference in &refs {
            let target = codeload_url(&owner, &repo, reference);
            match fetcher.fetch(&target).await {
                Ok(Some(archive)) => return Ok(archive),
                Ok(None) => continue,
                Err(reason) => {
                    if reason.contains("too large") {
                        return Err(InstallError::TooLarge);
                    }
                    return Err(InstallError::Download);
                }
            }
        }
        Err(InstallError::Download)
    }

    /// Unpack a downloaded archive into the plugins directory. Refuses
    /// traversal, absolute paths, symlinks, oversized entries, and anything
    /// without a valid `plugin.toml`, then swaps the result in atomically
    /// and reloads. The install arrives disabled: storing code never runs
    /// it. Reinstalling over an existing plugin updates its code in place;
    /// the saved settings survive because they live in config, not here.
    pub fn install_archive(
        &self,
        archive: &[u8],
        unpacker: &dyn ArchiveUnpacker,
    ) -> Result<String, InstallError> {
        if archive.len() > MAX_PLUGIN_ZIP_BYTES {
            return Err(InstallError::TooLarge);
        }
        let entries = unpacker
            .unpack(archive)
            .map_err(InstallError::ArchiveUnreadable)?;
        if entries.is_empty() {
            return Err(InstallError::Layout);
        }
        if entries.len() > MAX_PLUGIN_ZIP_ENTRIES {
            return Err(InstallError::TooManyFiles);
        }
        if entries
            .iter()
            .any(|entry| entry.data.len() > MAX_PLUGIN_ZIP_FILE_BYTES)
        {
            return Err(InstallError::OversizedFile);
        }
        let mut roots: HashSet<String> = HashSet::new();
        for entry in &entries {
            let root = entry.path.split('/').next().unwrap_or("").to_owned();
            roots.insert(root);
        }
        if roots.len() != 1 {
            return Err(InstallError::Layout);
        }
        let root = roots.into_iter().next().unwrap_or_default();
        if root.is_empty() {
            return Err(InstallError::Layout);
        }
        let manifest_path = format!("{root}/plugin.toml");
        if !entries.iter().any(|entry| entry.path == manifest_path) {
            return Err(InstallError::NoManifest);
        }

        let staging = self.dir.join(format!(".installing-{root}"));
        let _ = std::fs::remove_dir_all(&staging);
        let outcome = self.write_staged_entries(&staging, &root, &entries);
        if let Err(error) = outcome {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        let manifest = load_manifest(&staging)
            .map_err(|error| InstallError::InvalidManifest(error.to_string()))?;
        let placed = self.swap_staged_dir(&staging, &manifest.name);
        if let Err(reason) = placed {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(InstallError::Io(reason));
        }
        self.load_all();
        tracing::info!(plugin = %manifest.name, "plugin installed");
        Ok(manifest.name)
    }

    fn write_staged_entries(
        &self,
        staging: &Path,
        root: &str,
        entries: &[ArchiveEntry],
    ) -> Result<(), InstallError> {
        let prefix = format!("{root}/");
        let mut total_written: usize = 0;
        for entry in entries {
            if entry.is_symlink {
                return Err(InstallError::Symlinks);
            }
            if entry.path.starts_with('/') {
                return Err(InstallError::UnsafePaths);
            }
            let Some(remainder) = entry.path.strip_prefix(&prefix) else {
                continue;
            };
            if remainder.is_empty() {
                continue;
            }
            let parts: Vec<&str> = remainder.split('/').collect();
            if parts.iter().any(|part| part.is_empty() || *part == "..") {
                return Err(InstallError::UnsafePaths);
            }
            total_written += entry.data.len();
            if total_written > MAX_PLUGIN_ZIP_DECOMPRESSED_BYTES {
                return Err(InstallError::TooLarge);
            }
            let mut target = staging.to_path_buf();
            for part in &parts {
                target.push(part);
            }
            if let Some(parent) = target.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                return Err(InstallError::Io(error.to_string()));
            }
            if let Err(error) = std::fs::write(&target, &entry.data) {
                return Err(InstallError::Io(error.to_string()));
            }
        }
        Ok(())
    }

    fn swap_staged_dir(&self, staging: &Path, name: &str) -> Result<(), String> {
        if let Err(error) = std::fs::create_dir_all(&self.dir) {
            return Err(error.to_string());
        }
        let placed = self.dir.join(name);
        // Two renames, never a gap: the live dir steps aside first, so a
        // crash mid-swap leaves either the old or the new tree in place,
        // never a missing plugin. A failed second rename restores the old
        // tree best-effort; the staging dir is the caller's to clean.
        if placed.exists() {
            let backup = self.dir.join(format!(".swap-backup-{name}"));
            let _ = std::fs::remove_dir_all(&backup);
            std::fs::rename(&placed, &backup).map_err(|error| error.to_string())?;
            if let Err(error) = std::fs::rename(staging, &placed) {
                let _ = std::fs::rename(&backup, &placed);
                return Err(error.to_string());
            }
            let _ = std::fs::remove_dir_all(&backup);
            return Ok(());
        }
        std::fs::rename(staging, &placed).map_err(|error| error.to_string())
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

    /// Disable-by-deletion: remove the plugin's folder. The saved settings
    /// stay in config, so a reinstall keeps its configuration.
    pub fn uninstall(&self, name: &str) -> Result<(), UninstallError> {
        let plugin = self.get(name).ok_or(UninstallError::NotFound)?;
        let target = if plugin.directory.is_empty() {
            self.dir.join(name)
        } else {
            PathBuf::from(&plugin.directory)
        };
        if target.is_dir() && target.parent() == Some(self.dir.as_path()) {
            let _ = std::fs::remove_dir_all(&target);
        }
        self.load_all();
        tracing::info!(plugin = %name, "plugin uninstalled");
        Ok(())
    }

    /// Enabled scheduler plugins mapped to their loops, sorted by name.
    /// `load_all` only records this set via the swap; loops rebuild solely
    /// through the tick supervisor.
    pub fn desired_ticks(&self) -> Vec<DesiredTick> {
        let mut desired = Vec::new();
        let plugins = self
            .plugins
            .lock()
            .map(|guard| guard.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for plugin in plugins {
            if !plugin.enabled
                || plugin.module.is_none()
                || !plugin.active_capabilities.iter().any(|c| c == "scheduler")
            {
                continue;
            }
            let minutes = plugin
                .manifest
                .schedule
                .as_ref()
                .map(|schedule| schedule.interval_minutes)
                .unwrap_or(60)
                .clamp(5, 1440);
            let run_on_load = plugin
                .manifest
                .schedule
                .as_ref()
                .map(|schedule| schedule.run_on_load)
                .unwrap_or(false);
            desired.push(DesiredTick {
                name: plugin.manifest.name.clone(),
                interval_minutes: minutes,
                run_on_load,
            });
        }
        desired.sort_by(|left, right| left.name.cmp(&right.name));
        desired
    }

    /// Fan one accepted play out to every scrobbler plugin. Best-effort:
    /// a failing plugin is logged, never propagated.
    pub async fn dispatch_scrobble(&self, event: &ScrobbleEvent) {
        for plugin in self.with_capability("scrobbler") {
            let Some(module) = plugin.module.clone() else {
                continue;
            };
            if let Err(reason) = module.on_scrobble(event).await {
                tracing::warn!(
                    plugin = %plugin.manifest.name,
                    %reason,
                    "plugin scrobble failed"
                );
            }
        }
    }

    /// Gather purchase links from every provider, each under its own
    /// timeout. A failing plugin drops its links, never the section.
    pub async fn gather_purchase_links(
        &self,
        artist: &str,
        album: &str,
        release_group_mbid: &str,
    ) -> Vec<PluginPurchaseLink> {
        let mut links = Vec::new();
        for plugin in self.purchase_providers() {
            let Some(module) = plugin.module.clone() else {
                continue;
            };
            match tokio::time::timeout(
                PURCHASE_TIMEOUT,
                module.purchase_links(artist, album, release_group_mbid),
            )
            .await
            {
                Ok(Ok(found)) => links.extend(found),
                Ok(Err(reason)) => {
                    tracing::warn!(
                        plugin = %plugin.manifest.name,
                        %reason,
                        "plugin purchase links failed"
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        plugin = %plugin.manifest.name,
                        "plugin purchase links timed out"
                    );
                }
            }
        }
        links
    }

    /// Fan one engine event out to every subscriber without awaiting any
    /// of them: each notification is its own task, so a slow subscriber
    /// never delays the publishing flow. At most one in-flight notification
    /// per plugin; an overlapped dispatch is skipped and counted.
    /// `scrobble`-kind events additionally reach v0 `scrobbler` plugins.
    pub async fn dispatch_event(self: &Arc<Self>, event: PluginEvent) {
        self.sweep_causations();
        for plugin in self.subscribers() {
            let name = plugin.manifest.name.clone();
            if !event.causation_id.is_empty() && self.seen_causation(&event.causation_id, &name) {
                continue;
            }
            if self.mark_inflight(&name) {
                self.note_dropped_event(&name, event.kind.as_str());
                continue;
            }
            if !event.causation_id.is_empty() {
                self.remember_causation(&event.causation_id, &name);
            }
            let host = Arc::clone(self);
            let task_event = event.clone();
            let depth = current_depth();
            let causation = if task_event.causation_id.is_empty() {
                None
            } else {
                Some(task_event.causation_id.clone())
            };
            tokio::spawn(PUBLISH_DEPTH.scope(
                depth + 1,
                PUBLISH_CAUSATION.scope(causation, async move {
                    host.notify_one(&plugin, &task_event).await
                }),
            ));
        }
        if event.kind == EventKind::Scrobble
            && let EventPayload::Scrobble(scrobble) = &event.payload
        {
            let host = Arc::clone(self);
            let scrobble = scrobble.clone();
            tokio::spawn(async move { host.dispatch_scrobble(&scrobble).await });
        }
    }

    async fn notify_one(&self, plugin: &LoadedPlugin, event: &PluginEvent) {
        let name = plugin.manifest.name.clone();
        let started = Instant::now();
        let outcome = match plugin.module.clone() {
            Some(module) => {
                match tokio::time::timeout(EVENT_NOTIFY_TIMEOUT, module.on_event(event)).await {
                    Ok(Ok(())) => None,
                    Ok(Err(reason)) => Some(reason),
                    Err(_) => Some("timed out".to_owned()),
                }
            }
            None => Some("module gone".to_owned()),
        };
        if let Some(reason) = outcome {
            tracing::warn!(
                plugin = %name,
                kind = event.kind.as_str(),
                %reason,
                "plugin event failed"
            );
        } else {
            tracing::info!(
                plugin = %name,
                kind = event.kind.as_str(),
                duration_ms = started.elapsed().as_secs_f64() * 1000.0,
                "plugin event notified"
            );
        }
        if let Ok(mut guard) = self.inflight.lock() {
            guard.remove(&name);
        }
    }

    /// Try to mark a plugin busy. True means it already was (skip it).
    fn mark_inflight(&self, name: &str) -> bool {
        self.inflight
            .lock()
            .map(|mut guard| !guard.insert(name.to_owned()))
            .unwrap_or(false)
    }

    fn note_dropped_event(&self, name: &str, kind: &str) {
        tracing::warn!(plugin = %name, %kind, "plugin event dropped: slow subscriber");
        if let Ok(mut guard) = self.dropped_events.lock() {
            *guard.entry(name.to_owned()).or_insert(0) += 1;
        }
    }

    fn seen_causation(&self, causation: &str, subscriber: &str) -> bool {
        self.seen_causations
            .lock()
            .map(|guard| guard.contains_key(&(causation.to_owned(), subscriber.to_owned())))
            .unwrap_or(false)
    }

    fn remember_causation(&self, causation: &str, subscriber: &str) {
        if let Ok(mut guard) = self.seen_causations.lock() {
            guard.insert(
                (causation.to_owned(), subscriber.to_owned()),
                Instant::now(),
            );
        }
    }

    fn sweep_causations(&self) {
        let now = Instant::now();
        let sweep_due = self
            .causation_sweep_at
            .lock()
            .map(|guard| now.duration_since(*guard) >= CAUSATION_SWEEP_INTERVAL)
            .unwrap_or(false);
        let over_cap = self
            .seen_causations
            .lock()
            .map(|guard| guard.len() >= CAUSATION_CAP)
            .unwrap_or(false);
        if !sweep_due && !over_cap {
            return;
        }
        if let Ok(mut guard) = self.causation_sweep_at.lock() {
            *guard = now;
        }
        if let Ok(mut guard) = self.seen_causations.lock() {
            guard.retain(|_, seen_at| now.duration_since(*seen_at) < CAUSATION_TTL);
            while guard.len() > CAUSATION_CAP {
                if let Some(first) = guard.keys().next().cloned() {
                    guard.remove(&first);
                } else {
                    break;
                }
            }
        }
    }

    /// Validate and enqueue one plugin hint for the engine consumer.
    /// Unknown kinds are a loud error; field, depth, rate, and queue
    /// overflows return a result instead. `principal` is trusted engine
    /// input for rate keying only: the module-bound publish path never
    /// forwards a caller-supplied value, so rotating principals share one
    /// per-plugin bucket.
    pub fn publish_from_plugin(
        &self,
        plugin_name: &str,
        kind: &str,
        payload: &PublishPayload,
        principal: &str,
        causation_id: Option<&str>,
    ) -> Result<PluginPublishResult, String> {
        let usable = self
            .get(plugin_name)
            .map(|plugin| {
                plugin.enabled
                    && plugin.module.is_some()
                    && plugin.active_capabilities.iter().any(|c| c == "publisher")
            })
            .unwrap_or(false);
        if !usable {
            tracing::warn!(
                plugin = %plugin_name,
                %kind,
                "publish rejected: unknown or disabled publisher"
            );
            return Ok(PluginPublishResult {
                ok: false,
                status: 404,
                retry_after: 0,
                error: Some("unknown or disabled publisher".to_owned()),
            });
        }
        if publish_fields(kind).is_none() {
            return Err(format!("unknown publish kind '{kind}'"));
        }
        let fields = match coerce_publish_payload(kind, payload) {
            Ok(fields) => fields,
            Err(reason) => {
                tracing::warn!(
                    plugin = %plugin_name,
                    %kind,
                    %reason,
                    "publish rejected"
                );
                return Ok(PluginPublishResult {
                    ok: false,
                    status: 422,
                    retry_after: 0,
                    error: Some(reason),
                });
            }
        };
        let depth = current_depth();
        if depth > 1 {
            tracing::warn!(
                plugin = %plugin_name,
                %kind,
                "publish dropped: max depth exceeded"
            );
            return Ok(PluginPublishResult {
                ok: false,
                status: 409,
                retry_after: 0,
                error: Some("max publish depth exceeded".to_owned()),
            });
        }
        if let Some(retry_after) = self.publish_rate_hit(plugin_name, principal) {
            tracing::warn!(
                plugin = %plugin_name,
                %kind,
                %retry_after,
                "publish rate limited"
            );
            return Ok(PluginPublishResult {
                ok: false,
                status: 429,
                retry_after,
                error: Some("rate_limited".to_owned()),
            });
        }
        let causation = match (depth, current_causation()) {
            (1.., Some(parent)) => parent,
            _ => causation_id.unwrap_or("").to_owned(),
        };
        let causation = if causation.is_empty() {
            uuid::Uuid::new_v4().simple().to_string()
        } else {
            causation
        };
        let record = PublishedRecord {
            source_plugin: plugin_name.to_owned(),
            kind: kind.to_owned(),
            payload: fields,
            principal: principal.to_owned(),
            causation_id: causation,
            depth: depth + 1,
        };
        if let Ok(mut guard) = self.publish_queue.lock() {
            if guard.len() >= PUBLISH_QUEUE_MAX {
                guard.pop_front();
                self.publish_dropped.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    plugin = %plugin_name,
                    %kind,
                    "publish dropped: queue full"
                );
            }
            guard.push_back(record);
        }
        Ok(PluginPublishResult {
            ok: true,
            status: 200,
            retry_after: 0,
            error: None,
        })
    }

    /// Record a publish; `None` when allowed, else Retry-After seconds.
    fn publish_rate_hit(&self, plugin_name: &str, principal: &str) -> Option<u64> {
        let now = Instant::now();
        let mut guard = self.publish_rate.lock().ok()?;
        if !guard.contains_key(&(plugin_name.to_owned(), principal.to_owned()))
            && guard.len() >= PUBLISH_RATE_CAP
        {
            guard.retain(|_, hits| {
                hits.iter()
                    .any(|hit| now.duration_since(*hit) < PUBLISH_RATE_WINDOW)
            });
            while guard.len() >= PUBLISH_RATE_CAP {
                if let Some(first) = guard.keys().next().cloned() {
                    guard.remove(&first);
                } else {
                    break;
                }
            }
        }
        let hits = guard
            .entry((plugin_name.to_owned(), principal.to_owned()))
            .or_default();
        hits.retain(|hit| now.duration_since(*hit) < PUBLISH_RATE_WINDOW);
        if hits.len() >= PUBLISH_RATE_LIMIT {
            let retry_after = hits
                .first()
                .and_then(|oldest| oldest.checked_add(PUBLISH_RATE_WINDOW))
                .and_then(|reset| reset.checked_duration_since(now))
                .map(|wait| wait.as_secs() + 1)
                .unwrap_or(1)
                .max(1);
            return Some(retry_after);
        }
        hits.push(now);
        None
    }

    /// Take every queued hint for the engine consumer.
    pub fn drain_published(&self) -> Vec<PublishedRecord> {
        self.publish_queue
            .lock()
            .map(|mut guard| guard.drain(..).collect())
            .unwrap_or_default()
    }

    /// Run one declared plugin route with timeout and isolation. Disabled
    /// plugins, undeclared paths, and off-list methods are 404 (no oracle);
    /// plugin-chosen statuses pass only when 200-299, 400, or 404, else map
    /// to 200/502 by the fixed table; any failure is a generic 502, never
    /// a traceback.
    pub async fn handle_plugin_route(
        &self,
        plugin_name: &str,
        method: &str,
        subpath: &str,
        query: &HashMap<String, String>,
        body: &PluginRouteBody,
    ) -> PluginRouteResponse {
        let failed = || PluginRouteResponse {
            status: 502,
            body: serde_json::json!({"error": {"code": "EXTERNAL_SERVICE_UNAVAILABLE", "message": "Plugin route failed", "details": null}}),
        };
        let not_found = || PluginRouteResponse {
            status: 404,
            body: serde_json::json!({"error": {"code": "NOT_FOUND", "message": "Not found", "details": null}}),
        };
        let plugin = match self.get(plugin_name) {
            Some(plugin)
                if plugin.enabled
                    && plugin.module.is_some()
                    && plugin.active_capabilities.iter().any(|c| c == "publisher") =>
            {
                plugin
            }
            _ => return not_found(),
        };
        let verb = method.to_ascii_uppercase();
        if !ROUTE_METHODS.contains(&verb.as_str()) {
            return not_found();
        }
        let declared = plugin
            .manifest
            .routes
            .iter()
            .any(|route| route.path == subpath && route.method.to_ascii_uppercase() == verb);
        if !declared {
            return not_found();
        }
        let Some(module) = plugin.module.clone() else {
            return not_found();
        };
        if !module.has_route_handler() {
            tracing::warn!(plugin = %plugin_name, "route has no handler");
            return failed();
        }
        let response = match tokio::time::timeout(
            ROUTE_TIMEOUT,
            module.handle_route(&verb, subpath, query, body),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(reason)) => {
                tracing::warn!(
                    plugin = %plugin_name,
                    method = %verb,
                    path = %subpath,
                    %reason,
                    "plugin route failed"
                );
                return failed();
            }
            Err(_) => {
                tracing::warn!(
                    plugin = %plugin_name,
                    method = %verb,
                    path = %subpath,
                    "plugin route timed out"
                );
                return failed();
            }
        };
        let status =
            if (200..=299).contains(&response.status) || [400, 404].contains(&response.status) {
                response.status
            } else if (500..=599).contains(&response.status) {
                502
            } else {
                200
            };
        let raw = serde_json::to_vec(&response.body).unwrap_or_default();
        if raw.len() > ROUTE_BODY_MAX_BYTES {
            tracing::warn!(
                plugin = %plugin_name,
                method = %verb,
                path = %subpath,
                "plugin route body over 1 MiB"
            );
            return failed();
        }
        PluginRouteResponse {
            status,
            body: response.body,
        }
    }

    /// Fire one module tick for the jobs loop. Resolves the module fresh
    /// every call, never captured, so a settings-save rebuild applies on
    /// the next tick. The caller builds the context over the tick store.
    pub async fn fire_module_tick(&self, name: &str, ctx: &TickContext<'_>) -> Result<(), String> {
        let plugin = match self.get(name) {
            Some(plugin)
                if plugin.enabled
                    && plugin.module.is_some()
                    && plugin.active_capabilities.iter().any(|c| c == "scheduler") =>
            {
                plugin
            }
            _ => return Err("unknown or disabled scheduler".to_owned()),
        };
        let Some(module) = plugin.module else {
            return Err("unknown or disabled scheduler".to_owned());
        };
        module.on_tick(ctx).await
    }
}

/// Validate one publish payload into its stored field map.
fn coerce_publish_payload(
    kind: &str,
    payload: &PublishPayload,
) -> Result<HashMap<String, String>, String> {
    let Some(allowed) = publish_fields(kind) else {
        return Err(format!("unknown publish kind '{kind}'"));
    };
    let data: HashMap<String, String> = match payload {
        PublishPayload::Empty => HashMap::new(),
        PublishPayload::Fields(fields) => fields.clone(),
    };
    let mut unknown: Vec<&str> = data
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .map(String::as_str)
        .collect();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(format!(
            "unknown fields for kind '{kind}': [{}]",
            unknown.join(", ")
        ));
    }
    let mut stored = HashMap::new();
    for field in allowed {
        if *field == "note" || *field == "body" {
            stored.insert(
                field.to_string(),
                data.get(*field).cloned().unwrap_or_default(),
            );
            continue;
        }
        match data.get(*field) {
            Some(value) if !value.trim().is_empty() => {
                stored.insert(field.to_string(), value.clone());
            }
            _ => {
                return Err(format!("missing fields for kind '{kind}': ['{field}']"));
            }
        }
    }
    if kind == "download_note"
        && let Some(note) = stored.get("note")
        && note.len() > PUBLISH_NOTE_MAX_BYTES
    {
        return Err("note exceeds 1 KiB".to_owned());
    }
    Ok(stored)
}
