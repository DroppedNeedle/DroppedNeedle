//! The seam between the host and a running plugin, plus the payload types
//! that cross it.
//!
//! The host never links plugin code. It talks to a [`PluginConnection`]:
//! send a method name and JSON params, get JSON back or a [`CallError`].
//! [`PluginLauncher`] starts one connection per enabled plugin. Production
//! launches a subprocess per plugin (`process.rs`); tests launch fakes.
//! Because the seam is just "method plus JSON", a WebAssembly sandbox can
//! implement it later and plugins do not change.
//!
//! Plugins are trusted code once an admin enables them, as in v2. What the
//! subprocess model adds is isolation from crashes and hangs: a plugin
//! that dies or stops answering fails its own calls and is restarted, and
//! the flow that called it carries on.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::manifest::PluginManifest;

/// Boxed future for trait methods. The host holds modules behind
/// `Arc<dyn ...>`, so every async trait method returns this instead of
/// `impl Future` (which is not dyn-compatible).
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// A play accepted by the scrobble pipeline, already deduped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrobbleEvent {
    /// Artist name.
    pub artist: String,
    /// Track title.
    pub track: String,
    /// Album title, when known.
    pub album: Option<String>,
    /// Play time, unix seconds.
    pub timestamp: i64,
    /// Track length in milliseconds, when known.
    pub duration_ms: Option<i64>,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
}

/// A purchase link a plugin contributes to the where-to-buy section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPurchaseLink {
    /// Link label.
    pub label: String,
    /// Link target.
    pub url: String,
    /// `digital`, `physical`, or `free`.
    #[serde(default = "default_link_kind")]
    pub kind: String,
}

fn default_link_kind() -> String {
    "digital".to_owned()
}

/// Fan-out event kinds for subscriber plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A scrobble was accepted (also reaches v0 scrobbler plugins).
    Scrobble,
    /// A download started.
    DownloadStarted,
    /// A download completed.
    DownloadCompleted,
    /// A download failed.
    DownloadFailed,
    /// A request was created.
    RequestCreated,
    /// A request was fulfilled.
    RequestFulfilled,
    /// An import finished.
    ImportFinished,
    /// Playback started.
    PlaybackStarted,
    /// Another plugin published a `plugin_notice`.
    PluginNotice,
}

impl EventKind {
    /// Wire name for the kind.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Scrobble => "scrobble",
            EventKind::DownloadStarted => "download_started",
            EventKind::DownloadCompleted => "download_completed",
            EventKind::DownloadFailed => "download_failed",
            EventKind::RequestCreated => "request_created",
            EventKind::RequestFulfilled => "request_fulfilled",
            EventKind::ImportFinished => "import_finished",
            EventKind::PlaybackStarted => "playback_started",
            EventKind::PluginNotice => "plugin_notice",
        }
    }

    /// Parse a wire name back into a kind.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "scrobble" => Some(EventKind::Scrobble),
            "download_started" => Some(EventKind::DownloadStarted),
            "download_completed" => Some(EventKind::DownloadCompleted),
            "download_failed" => Some(EventKind::DownloadFailed),
            "request_created" => Some(EventKind::RequestCreated),
            "request_fulfilled" => Some(EventKind::RequestFulfilled),
            "import_finished" => Some(EventKind::ImportFinished),
            "playback_started" => Some(EventKind::PlaybackStarted),
            "plugin_notice" => Some(EventKind::PluginNotice),
            _ => None,
        }
    }
}

/// Payload for the download lifecycle kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadTaskEvent {
    /// Download task id.
    pub task_id: String,
    /// Owning user id.
    #[serde(default)]
    pub user_id: String,
    /// Release-group MBID.
    #[serde(default)]
    pub release_group_mbid: String,
    /// Source key.
    #[serde(default)]
    pub source: String,
    /// Terminal state name (`started` while running).
    #[serde(default)]
    pub outcome: String,
}

/// Payload for the request kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEvent {
    /// Request id.
    pub request_id: String,
    /// Owning user id.
    #[serde(default)]
    pub user_id: String,
    /// Release-group MBID.
    #[serde(default)]
    pub release_group_mbid: String,
    /// Request status.
    #[serde(default)]
    pub status: String,
}

/// Payload for the `import_finished` kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportEvent {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Imported track count.
    #[serde(default)]
    pub track_count: i64,
    /// Source key.
    #[serde(default)]
    pub source: String,
}

/// Payload for the `playback_started` kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackEvent {
    /// Artist name.
    pub artist: String,
    /// Track title.
    pub track: String,
    /// Album title, when known.
    pub album: Option<String>,
    /// Owning user id.
    #[serde(default)]
    pub user_id: String,
}

/// Payload for the `plugin_notice` kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeEvent {
    /// Plugin that published the notice, stamped by the host.
    pub source_plugin: String,
    /// Notice title.
    pub title: String,
    /// Notice body.
    #[serde(default)]
    pub body: String,
}

/// One struct per event kind. The plugin gets the struct only: it cannot
/// reach the task row or mutate engine state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    /// `scrobble` payload.
    Scrobble(ScrobbleEvent),
    /// `download_*` payload.
    Download(DownloadTaskEvent),
    /// `request_*` payload.
    Request(RequestEvent),
    /// `import_finished` payload.
    Import(ImportEvent),
    /// `playback_started` payload.
    Playback(PlaybackEvent),
    /// `plugin_notice` payload.
    Notice(NoticeEvent),
}

/// One fan-out event for subscriber plugins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEvent {
    /// Event kind.
    pub kind: EventKind,
    /// Kind-specific payload.
    pub payload: EventPayload,
    /// Host-stamped causation id (uuid hex) so fan-out dedups on
    /// (causation, subscriber). Empty means unstamped.
    #[serde(default)]
    pub causation_id: String,
}

/// Kinds a `publisher` plugin may publish: `indexer_invalidate` (a hint
/// to search a source again), `download_note` (a note on a download) and
/// `plugin_notice` (a message for the other subscribers).
pub const PUBLISH_KINDS: &[&str] = &["indexer_invalidate", "download_note", "plugin_notice"];

/// Required fields per publish kind: references must arrive non-empty,
/// while `note` and `body` default to empty.
pub fn publish_fields(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "indexer_invalidate" => Some(&["target_source"]),
        "download_note" => Some(&["task_id", "note"]),
        "plugin_notice" => Some(&["title", "body"]),
        _ => None,
    }
}

/// Payload shapes the host accepts for one publish call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PublishPayload {
    /// No payload (valid only when every field has a default, which no
    /// current kind does, so this always fails field validation).
    #[default]
    Empty,
    /// Field map.
    Fields(HashMap<String, String>),
}

/// Outcome of one `publish_from_plugin` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPublishResult {
    /// True when the publish was accepted.
    pub ok: bool,
    /// 200 accepted, 404 unknown/disabled, 409 max-depth, 422 invalid,
    /// 429 rate-limited.
    pub status: u16,
    /// Retry-After seconds on 429, else 0.
    #[serde(default)]
    pub retry_after: u64,
    /// Short reason on failure.
    #[serde(default)]
    pub error: Option<String>,
}

/// Request body shapes for one `/ext/` call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PluginRouteBody {
    /// No body.
    #[default]
    Empty,
    /// Decoded JSON body.
    Json(serde_json::Value),
    /// Body that was not JSON but was UTF-8 text.
    Text(String),
}

/// Plugin answer to one `/ext/` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRouteResponse {
    /// Chosen status. The host clamps it to 200-299 plus 400/404.
    pub status: i32,
    /// Answer body, capped at 1 MiB serialized.
    pub body: serde_json::Value,
}

impl PluginRouteResponse {
    /// A 200 answer with a JSON body.
    pub fn ok(body: serde_json::Value) -> Self {
        Self { status: 200, body }
    }
}

/// Why one call into a plugin failed. The host logs these against the
/// plugin and turns them into the caller's safe fallback (no links, no
/// results, a 502 on `/ext/`); they never reach a user verbatim.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// The plugin is starting, restarting or stopped.
    #[error("plugin is not running: {0}")]
    NotRunning(String),
    /// No answer inside the call's time budget.
    #[error("plugin did not answer in time")]
    Timeout,
    /// The plugin does not implement the method.
    #[error("plugin does not implement {0}")]
    Unsupported(String),
    /// The plugin's own code raised or refused.
    #[error("plugin failed: {0}")]
    Failed(String),
    /// The answer did not have the expected shape.
    #[error("plugin answer was malformed: {0}")]
    Malformed(String),
}

/// Where a plugin's runtime is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = PluginRuntimeState)]
pub enum RuntimeState {
    /// Launched, handshake not finished yet.
    Starting,
    /// Answering calls.
    Running,
    /// Crashed or hung; waiting out the backoff before the next start.
    Restarting,
    /// Stopped on purpose (disabled, uninstalled, shutting down).
    Stopped,
    /// Cannot run until something changes (bad command, protocol mismatch).
    Failed,
}

/// Health snapshot of one plugin runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStatus {
    /// Current state.
    pub state: RuntimeState,
    /// Restarts since the plugin was enabled.
    pub restarts: u64,
    /// Last start failure, crash or protocol problem.
    pub last_error: Option<String>,
    /// Capabilities the plugin said it implements in the handshake.
    pub implemented: Vec<String>,
}

/// One running plugin, whatever carries the messages.
pub trait PluginConnection: Send + Sync {
    /// Call one method and wait up to `timeout` for the answer. A call made
    /// while the plugin is starting waits for the handshake inside the same
    /// budget.
    fn call<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Value, CallError>>;

    /// Send one notification. Dropped silently when the plugin is down.
    fn notify(&self, method: &str, params: Value);

    /// Current health.
    fn status(&self) -> RuntimeStatus;

    /// Stop for good: ask the plugin to exit, then kill it.
    fn stop(&self) -> BoxFuture<'_, ()>;
}

/// What a plugin may ask of the host. The host answers on behalf of the
/// named plugin only; a plugin cannot act as another.
pub trait HostServices: Send + Sync {
    /// Answer one plugin-to-host request.
    fn handle<'a>(
        &'a self,
        plugin: &'a str,
        method: &'a str,
        params: Value,
    ) -> BoxFuture<'a, Result<Value, super::protocol::RpcError>>;
}

/// Everything a launcher needs to start one plugin.
pub struct LaunchSpec {
    /// Validated manifest.
    pub manifest: PluginManifest,
    /// Plugin code directory.
    pub plugin_dir: PathBuf,
    /// Writable data directory for this plugin (also its working directory).
    pub data_dir: PathBuf,
    /// Current settings, secrets decrypted.
    pub settings: HashMap<String, String>,
    /// Plugin-to-host requests land here.
    pub services: Arc<dyn HostServices>,
}

/// Starts plugin runtimes. Launching returns at once; the handshake runs
/// in the background and calls wait for it.
pub trait PluginLauncher: Send + Sync {
    /// Start one plugin.
    fn launch(&self, spec: LaunchSpec) -> Result<Arc<dyn PluginConnection>, String>;
}
