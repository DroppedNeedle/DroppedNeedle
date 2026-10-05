//! Playback-reporting request and response shapes.
//!
//! Clean-slate native shapes for the playback reporting routes. Field names
//! stay close to the v2 scrobble and now-playing schemas so the web player
//! keeps its vocabulary; validation
//! (timestamp bounds, non-negative durations) lives in the services, not
//! here, because it needs the clock.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Default playback source: the local library.
fn default_source() -> String {
    "local".to_owned()
}

/// Default reporting device slug.
fn default_device() -> String {
    "web".to_owned()
}

// ---------------------------------------------------------------------------
// Session lifecycle: start / progress / stop
// ---------------------------------------------------------------------------

/// Open a playback session for one catalog track.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackStartRequest {
    /// Catalog track id.
    pub track_id: String,
    /// Playback source (`local`, `youtube`, `jellyfin`, `navidrome`, `plex`).
    #[serde(default = "default_source")]
    pub source: String,
    /// Reporting device slug.
    #[serde(default = "default_device")]
    pub device: String,
}

/// Answer to a session start.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackStartResponse {
    /// Always true: reporting never fails the player.
    pub accepted: bool,
    /// Session key (`user_id:device:track_id`), for log correlation.
    pub session: String,
}

/// Heartbeat for a live session: keeps presence alive and the scrubber live.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackProgressRequest {
    /// Catalog track id.
    pub track_id: String,
    /// Playback source.
    #[serde(default = "default_source")]
    pub source: String,
    /// Reporting device slug.
    #[serde(default = "default_device")]
    pub device: String,
    /// Position in milliseconds, when the player knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<i64>,
    /// True while paused.
    #[serde(default)]
    pub is_paused: bool,
}

/// Answer to a progress heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackProgressResponse {
    /// Always true: reporting never fails the player.
    pub accepted: bool,
}

/// Close a session. A stop past the scrobble threshold counts the play.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackStopRequest {
    /// Catalog track id.
    pub track_id: String,
    /// Playback source.
    #[serde(default = "default_source")]
    pub source: String,
    /// Reporting device slug.
    #[serde(default = "default_device")]
    pub device: String,
    /// Final position in milliseconds, when the player knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<i64>,
    /// Skip the scrobble even past threshold (private session).
    #[serde(default)]
    pub ignore_scrobble: bool,
}

/// Answer to a stop report.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackStopResponse {
    /// Always true: reporting never fails the player.
    pub accepted: bool,
    /// True when the play counted (history recorded, forwarding attempted).
    pub scrobbled: bool,
}

// ---------------------------------------------------------------------------
// Native scrobble submit / now-playing forward
// ---------------------------------------------------------------------------

/// One native scrobble, by name (v2 `ScrobbleRequest` fields).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScrobbleSubmitRequest {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Play time, unix seconds.
    pub timestamp: i64,
    /// Album title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// Track length in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Recording MBID, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    /// Release-group MBID, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_group_mbid: Option<String>,
    /// Playback source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// One native now-playing forward, by name (v2 `NowPlayingRequest` fields).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScrobbleNowPlayingRequest {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// Track length in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Recording MBID, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    /// Release-group MBID, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_group_mbid: Option<String>,
    /// Playback source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Per-service forwarding outcome (v2 `ServiceResult`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServiceResult {
    /// True when that service accepted the report.
    pub success: bool,
    /// Failure text, when the service refused or errored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Answer to a scrobble or now-playing forward (v2 `ScrobbleResponse`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScrobbleResponse {
    /// Whether the play counted. Submit is true whenever history recorded,
    /// even with no linked account; now-playing is true only when at least
    /// one service accepted (the v2 accepted-quirk, kept).
    pub accepted: bool,
    /// Per-service outcomes, keyed by service name.
    #[serde(default)]
    pub services: HashMap<String, ServiceResult>,
}

// ---------------------------------------------------------------------------
// Now-playing presence
// ---------------------------------------------------------------------------

/// Native heartbeat (v2 `NowPlayingReport` fields).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NowPlayingHeartbeat {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// Cover art URL.
    #[serde(default)]
    pub cover_url: String,
    /// Playback source.
    #[serde(default = "default_source")]
    pub source: String,
    /// Reporting device slug.
    #[serde(default = "default_device")]
    pub device: String,
    /// True while paused.
    #[serde(default)]
    pub is_paused: bool,
    /// Position in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_ms: Option<i64>,
    /// Track length in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

/// Query for clearing presence: which device stopped.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NowPlayingDeleteQuery {
    /// Device slug (`web` default, the v2 spelling).
    #[serde(default)]
    pub device: Option<String>,
}

/// One live session, already privacy-projected (same JSON as the
/// `reads::discover` now-playing shape, so clients see no difference).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NowPlayingEntry {
    /// Session key (`user_id:device`).
    pub id: String,
    /// Display name of the listener.
    pub user_name: String,
    /// Track title (empty when redacted).
    pub track_name: String,
    /// Artist name (empty when redacted).
    pub artist_name: String,
    /// Album title (none when redacted or unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// Cover art URL (empty when redacted).
    pub cover_url: String,
    /// Device label.
    pub device_name: String,
    /// True when paused.
    pub is_paused: bool,
    /// Playback source.
    pub source: String,
    /// Position in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_ms: Option<i64>,
    /// Track length in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// True when the owner hides the track.
    #[serde(default)]
    pub redacted: bool,
}

/// The live presence snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NowPlayingSnapshot {
    /// Projected live sessions.
    pub sessions: Vec<NowPlayingEntry>,
}
