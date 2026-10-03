//! Capability surface: what a plugin implements, and what the host hands it.
//!
//! A module is instantiated once per plugin and may implement any subset of
//! the capability methods matching its manifest. Every method is fallible so
//! the host can isolate a failing plugin: an error is logged against the
//! plugin and never propagates into the host flow that triggered it.
//!
//! Modules are trusted once an admin enables them, exactly like v2. The
//! context a tick or handler receives carries no ambient authority beyond
//! the tick store; anything else a module wants goes over the public API.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::manifest::PluginManifest;

/// Boxed future for trait methods. The slice holds modules behind
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

/// Hint asking the engine to rescout a source. The engine decides; the
/// plugin cannot force a rescan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexerInvalidate {
    /// Source to rescout.
    pub target_source: String,
}

/// Annotation on one of the publishing plugin's own tasks. Annotation
/// only, never a status mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadNote {
    /// Task id.
    pub task_id: String,
    /// Note text, at most 1 KiB.
    #[serde(default)]
    pub note: String,
}

/// Opaque broadcast, fanned out to subscribers. Never mutates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginNotice {
    /// Notice title.
    pub title: String,
    /// Notice body.
    #[serde(default)]
    pub body: String,
}

/// Allowlisted publish kinds a `publisher` plugin may emit.
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
    /// True when the hint was queued.
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

/// One queued hint, taken by the engine consumer via `drain_published`.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedRecord {
    /// Publishing plugin name, stamped by the host (never spoofable).
    pub source_plugin: String,
    /// Publish kind.
    pub kind: String,
    /// Validated fields.
    pub payload: HashMap<String, String>,
    /// Trusted engine principal, for rate keying only.
    pub principal: String,
    /// Causation id for fan-out dedup.
    pub causation_id: String,
    /// Fan-out depth (1 for a direct publish).
    pub depth: u32,
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

/// Durable tick state, scoped to one plugin. Reads of missing keys come
/// back `None`; writes fail on unsafe keys and over-cap values. Backed by
/// the jobs tick store (see `ticks.rs`).
pub trait TickStateAccess: Send + Sync {
    /// Read one state key.
    fn read<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Vec<u8>>>;
    /// Store one state key, creating or overwriting.
    fn write<'a>(&'a self, key: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<(), String>>;
}

/// What a tick may use: its own plugin name plus durable state. Library
/// file mutations and HTTP stay out of v1 ticks; the module persists
/// whatever it needs through the state handle.
pub struct TickContext<'a> {
    /// Ticking plugin name.
    pub plugin_name: &'a str,
    /// Durable per-plugin state.
    pub state: &'a dyn TickStateAccess,
}

/// One loaded plugin module. Every method defaults to inert so a module
/// implements only what its manifest declares; the host intersects the
/// manifest with [`provides`](PluginModule::provides) before dispatching.
pub trait PluginModule: Send + Sync {
    /// Whether this module implements one capability id.
    fn provides(&self, _capability: &str) -> bool {
        false
    }

    /// Handle one accepted scrobble (`scrobbler`).
    fn on_scrobble<'a>(&'a self, _event: &'a ScrobbleEvent) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    /// Contribute purchase links (`purchase_links`).
    fn purchase_links<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PluginPurchaseLink>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Consume one fan-out event (`subscriber`).
    fn on_event<'a>(&'a self, _event: &'a PluginEvent) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    /// Run one scheduled tick (`scheduler`). Persist through the context:
    /// the store is the tick's only durable memory.
    fn on_tick<'a>(&'a self, _ctx: &'a TickContext<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    /// Whether this module serves `/ext/` routes (`publisher` plus a
    /// handler). A publisher without one answers 502 like v2.
    fn has_route_handler(&self) -> bool {
        false
    }

    /// Serve one declared `/ext/` route (`publisher`).
    fn handle_route<'a>(
        &'a self,
        _method: &'a str,
        _subpath: &'a str,
        _query: &'a HashMap<String, String>,
        _body: &'a PluginRouteBody,
    ) -> BoxFuture<'a, Result<PluginRouteResponse, String>> {
        Box::pin(async {
            Ok(PluginRouteResponse {
                status: 404,
                body: serde_json::Value::Null,
            })
        })
    }
}

/// Resolves a validated manifest to a live module. v2 imports Python
/// in-process; v3 has no execution engine for that yet, so this trait is
/// the seam: tests load fakes, and the production engine plugs in here
/// when it exists. Unknown entrypoints fail the plugin, never the host.
pub trait ModuleLoader: Send + Sync {
    /// Build the module for one enabled plugin.
    fn load(&self, dir: &Path, manifest: &PluginManifest) -> Result<Arc<dyn PluginModule>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kinds_round_trip() {
        for kind in [
            EventKind::Scrobble,
            EventKind::DownloadStarted,
            EventKind::DownloadCompleted,
            EventKind::DownloadFailed,
            EventKind::RequestCreated,
            EventKind::RequestFulfilled,
            EventKind::ImportFinished,
            EventKind::PlaybackStarted,
        ] {
            assert_eq!(EventKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EventKind::parse("nope"), None);
    }
}
