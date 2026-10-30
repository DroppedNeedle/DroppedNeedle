//! Playback-reporting and listening-settings request and response shapes.
//!
//! ListenBrainz links never carry the token: the status shape holds the
//! username only.
//!
//! Clean-slate native shapes for the playback reporting routes. Field names
//! stay close to the v2 scrobble and now-playing schemas so the web player
//! keeps its vocabulary; validation
//! (timestamp bounds, non-negative durations) lives in the services, not
//! here, because it needs the clock.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::remotes::models::SourceName;

/// Default playback source: the local library.
fn default_source() -> String {
    "local".to_owned()
}

/// Where a session's track plays from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackSource {
    /// A library track.
    #[default]
    Local,
    /// A YouTube pick standing in for a library track.
    Youtube,
    /// The admin's Jellyfin server.
    Jellyfin,
    /// The admin's Navidrome server.
    Navidrome,
    /// The admin's Plex server.
    Plex,
}

impl PlaybackSource {
    /// Lowercase wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Youtube => "youtube",
            Self::Jellyfin => "jellyfin",
            Self::Navidrome => "navidrome",
            Self::Plex => "plex",
        }
    }

    /// The media server a play from this source is reported to, if any.
    pub fn remote(self) -> Option<SourceName> {
        match self {
            Self::Local | Self::Youtube => None,
            Self::Jellyfin => Some(SourceName::Jellyfin),
            Self::Navidrome => Some(SourceName::Navidrome),
            Self::Plex => Some(SourceName::Plex),
        }
    }
}

/// Default reporting device slug.
fn default_device() -> String {
    "web".to_owned()
}

// ---------------------------------------------------------------------------
// Session lifecycle: start / progress / stop
// ---------------------------------------------------------------------------

/// Open a playback session for one track.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackStartRequest {
    /// The library track id for `local` and `youtube`; for a media server,
    /// that server's item id (the Plex rating key).
    pub track_id: String,
    /// Where the track plays from.
    #[serde(default)]
    pub source: PlaybackSource,
    /// Reporting device slug.
    #[serde(default = "default_device")]
    pub device: String,
    /// Where playback starts in milliseconds, when not at the beginning (a
    /// resumed track). Read for media-server plays only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<i64>,
    /// Track length in milliseconds, when the player knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
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
    /// The library track id for `local` and `youtube`; for a media server,
    /// that server's item id (the Plex rating key).
    pub track_id: String,
    /// Where the track plays from.
    #[serde(default)]
    pub source: PlaybackSource,
    /// Reporting device slug.
    #[serde(default = "default_device")]
    pub device: String,
    /// Position in milliseconds, when the player knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<i64>,
    /// Track length in milliseconds, when the player knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
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

/// Close a session. A library play past the scrobble threshold counts.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackStopRequest {
    /// The library track id for `local` and `youtube`; for a media server,
    /// that server's item id (the Plex rating key).
    pub track_id: String,
    /// Where the track plays from.
    #[serde(default)]
    pub source: PlaybackSource,
    /// Reporting device slug.
    #[serde(default = "default_device")]
    pub device: String,
    /// Final position in milliseconds, when the player knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<i64>,
    /// Track length in milliseconds, when the player knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Skip the scrobble even past threshold (private session). A media
    /// server counts a play by its own rule.
    #[serde(default)]
    pub ignore_scrobble: bool,
}

/// Answer to a stop report.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackStopResponse {
    /// Always true: reporting never fails the player.
    pub accepted: bool,
    /// True when the play counted (history recorded, forwarding attempted).
    /// Always false for a media-server play: the web player's scrobble
    /// submit records that one.
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
    /// The library track played. Files the play under that track, whose own
    /// recording and release group replace any sent. Only for a play from
    /// the library (source `local` or none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
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

/// Per-service forwarding outcome. Delivery runs in the background, so this
/// says the report is waiting to be sent, not that the service took it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServiceResult {
    /// True when the report is waiting for that service. A scrobble waits
    /// in a kept queue and is sent, retried while the service is down,
    /// within two weeks of the play; a now-playing report is sent once.
    pub queued: bool,
    /// Why the report is not waiting (the play was not saved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Answer to a scrobble or now-playing forward (v2 `ScrobbleResponse`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScrobbleResponse {
    /// Whether the play counted. Submit is true when history recorded it,
    /// even with no linked account, and false when the play could not be
    /// saved; now-playing is true only when at least one service has it
    /// (the v2 accepted-quirk, kept).
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NowPlayingDeleteQuery {
    /// Device slug (`web` default, the v2 spelling).
    #[serde(default)]
    pub device: Option<String>,
}

// ---------------------------------------------------------------------------
// Listening settings: scrobble preferences and the ListenBrainz link
// ---------------------------------------------------------------------------

/// One linked listening account. Never carries the secret, only the name.
/// Named apart from the remotes `ConnectionStatus` in the one OpenAPI
/// document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = ListeningLinkStatus)]
pub struct LinkStatus {
    /// Service name (`listenbrainz` here).
    pub service: String,
    /// Whether the link is active.
    #[serde(default)]
    pub enabled: bool,
    /// Linked username.
    #[serde(default)]
    pub username: String,
}

/// Per-user scrobble preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScrobblePreferences {
    /// Forward plays to Last.fm.
    #[serde(default)]
    pub scrobble_to_lastfm: bool,
    /// Forward plays to ListenBrainz.
    #[serde(default)]
    pub scrobble_to_listenbrainz: bool,
    /// Navidrome owns external forwarding for Navidrome plays.
    #[serde(default = "default_navidrome_delegation")]
    pub navidrome_handles_external_scrobbles: bool,
    /// `listenbrainz` or `lastfm`.
    #[serde(default = "default_primary_source")]
    pub primary_music_source: String,
    /// `full`, `track_hidden`, or `offline`.
    #[serde(default = "default_visibility")]
    pub now_playing_visibility: String,
    /// Standing intent to auto-request the personal mix.
    #[serde(default)]
    pub auto_request_personal_mix: bool,
    /// Standing-grant state for auto-request.
    #[serde(default = "default_mix_state")]
    pub auto_request_state: String,
}

fn default_navidrome_delegation() -> bool {
    true
}

fn default_primary_source() -> String {
    "listenbrainz".to_owned()
}

fn default_visibility() -> String {
    "full".to_owned()
}

fn default_mix_state() -> String {
    "none".to_owned()
}

/// Partial prefs update: absent fields keep their stored values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(default)]
pub struct ScrobblePreferencesUpdate {
    /// New Last.fm forwarding switch.
    pub scrobble_to_lastfm: Option<bool>,
    /// New ListenBrainz forwarding switch.
    pub scrobble_to_listenbrainz: Option<bool>,
    /// New Navidrome delegation switch.
    pub navidrome_handles_external_scrobbles: Option<bool>,
    /// New primary source.
    pub primary_music_source: Option<String>,
    /// New presence visibility.
    pub now_playing_visibility: Option<String>,
    /// New personal-mix auto-request intent.
    pub auto_request_personal_mix: Option<bool>,
}

/// ListenBrainz connect request. The username is required: it drives every
/// per-user read, and without it a "connected" account yields silently
/// empty discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListenBrainzConnectRequest {
    /// User token.
    pub user_token: String,
    /// ListenBrainz username.
    #[serde(default)]
    pub username: String,
}
