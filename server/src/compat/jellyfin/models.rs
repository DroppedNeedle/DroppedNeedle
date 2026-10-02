//! PascalCase Jellyfin DTOs, ported from v2 `api/compat/jellyfin/models.py`.
//!
//! Field declaration order matches the v2 msgspec structs, because serde
//! emits fields in declaration order and the goldens pin the wire bytes.
//! `None` serializes to nothing (`skip_serializing_none`), never `null`
//! (v2 `_strip_none` parity).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ===== System / identity =====

/// `GET /System/Info/Public` shape (v2 `PublicSystemInfo`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PublicSystemInfo {
    pub local_address: String,
    pub server_name: String,
    pub version: String,
    pub product_name: String,
    pub operating_system: String,
    pub id: String,
    pub startup_wizard_completed: bool,
}

/// `GET /System/Info` shape (v2 `SystemInfo`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SystemInfo {
    pub local_address: String,
    pub server_name: String,
    pub version: String,
    pub product_name: String,
    pub operating_system: String,
    pub id: String,
    pub startup_wizard_completed: bool,
    pub has_pending_restart: bool,
    pub is_shutting_down: bool,
    pub supports_library_monitor: bool,
}

// ===== User objects (Finamp/Manet login contract) =====

/// Fully populated: strict clients hard-cast the bools, so an empty `{}`
/// crashes them (Finamp "Null is not a subtype of bool", v2 issue #144).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserConfiguration {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_language_preference: Option<String>,
    pub play_default_audio_track: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle_language_preference: Option<String>,
    pub display_missing_episodes: bool,
    pub grouped_folders: Vec<String>,
    pub subtitle_mode: String,
    pub display_collections_view: bool,
    pub enable_local_password: bool,
    pub ordered_views: Vec<String>,
    pub latest_items_excludes: Vec<String>,
    pub my_media_excludes: Vec<String>,
    pub hide_played_in_latest: bool,
    pub remember_audio_selections: bool,
    pub remember_subtitle_selections: bool,
    pub enable_next_episode_auto_play: bool,
}

impl Default for UserConfiguration {
    fn default() -> Self {
        Self {
            audio_language_preference: None,
            play_default_audio_track: true,
            subtitle_language_preference: None,
            display_missing_episodes: false,
            grouped_folders: Vec::new(),
            subtitle_mode: "Default".to_owned(),
            display_collections_view: false,
            enable_local_password: false,
            ordered_views: Vec::new(),
            latest_items_excludes: Vec::new(),
            my_media_excludes: Vec::new(),
            hide_played_in_latest: true,
            remember_audio_selections: true,
            remember_subtitle_selections: true,
            enable_next_episode_auto_play: true,
        }
    }
}

/// User policy. `EnableAllFolders` must be true or strict clients (Manet)
/// conclude "no libraries" and never call `/UserViews`; every bool must be
/// present or Finamp's non-nullable casts crash login (v2 issues #144, #376).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserPolicy {
    pub is_administrator: bool,
    pub is_hidden: bool,
    pub is_disabled: bool,
    pub enable_all_folders: bool,
    pub enabled_folders: Vec<String>,
    pub enable_all_channels: bool,
    pub enabled_channels: Vec<String>,
    pub enable_all_devices: bool,
    pub enabled_devices: Vec<String>,
    pub enable_media_playback: bool,
    pub enable_audio_playback_transcoding: bool,
    pub enable_video_playback_transcoding: bool,
    pub enable_playback_remuxing: bool,
    pub enable_content_downloading: bool,
    pub enable_remote_access: bool,
    pub enable_sync_transcoding: bool,
    pub enable_user_preference_access: bool,
    pub enable_live_tv_access: bool,
    pub enable_live_tv_management: bool,
    pub enable_content_deletion: bool,
    pub enable_media_conversion: bool,
    pub enable_public_sharing: bool,
    pub enable_remote_control_of_other_users: bool,
    pub enable_shared_device_control: bool,
    pub invalid_login_attempt_count: u32,
    pub remote_client_bitrate_limit: u32,
    pub sync_play_access: String,
    pub blocked_tags: Vec<String>,
    pub allowed_tags: Vec<String>,
    pub access_schedules: Vec<String>,
    pub block_unrated_items: Vec<String>,
}

impl UserPolicy {
    /// Permissive defaults with `EnableAllFolders`, v2 `_user_dto` verbatim.
    pub fn permissive(is_admin: bool) -> Self {
        Self {
            is_administrator: is_admin,
            is_hidden: false,
            is_disabled: false,
            enable_all_folders: true,
            enabled_folders: Vec::new(),
            enable_all_channels: true,
            enabled_channels: Vec::new(),
            enable_all_devices: true,
            enabled_devices: Vec::new(),
            enable_media_playback: true,
            enable_audio_playback_transcoding: true,
            enable_video_playback_transcoding: true,
            enable_playback_remuxing: true,
            enable_content_downloading: true,
            enable_remote_access: true,
            enable_sync_transcoding: true,
            enable_user_preference_access: true,
            enable_live_tv_access: false,
            enable_live_tv_management: false,
            enable_content_deletion: false,
            enable_media_conversion: false,
            enable_public_sharing: false,
            enable_remote_control_of_other_users: false,
            enable_shared_device_control: false,
            invalid_login_attempt_count: 0,
            remote_client_bitrate_limit: 0,
            sync_play_access: "CreateAndJoinGroups".to_owned(),
            blocked_tags: Vec::new(),
            allowed_tags: Vec::new(),
            access_schedules: Vec::new(),
            block_unrated_items: Vec::new(),
        }
    }
}

/// User object for `/Users/Me` and `/Users/{id}` (the `{id}` is ignored,
/// v2 parity). The login echo itself is rendered by the auth slice's
/// `login_echo_json`; this struct's field order matches it exactly so the
/// goldens can assert the two agree byte for byte.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserDto {
    pub id: String,
    pub name: String,
    pub server_id: String,
    pub has_password: bool,
    pub has_configured_password: bool,
    /// Deprecated upstream but still sent; Finamp requires it non-null.
    pub has_configured_easy_password: bool,
    pub configuration: UserConfiguration,
    pub policy: UserPolicy,
}

// ===== Library items =====

/// Artist reference pair (v2 `NameGuidPair`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NameGuidPair {
    pub name: String,
    pub id: String,
}

/// Per-user item state (v2 `UserItemDataDto`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserItemDataDto {
    pub item_id: String,
    pub key: String,
    pub playback_position_ticks: i64,
    pub play_count: u64,
    pub is_favorite: bool,
    pub played: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_played_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played_percentage: Option<f64>,
}

impl UserItemDataDto {
    /// Blank state for the music view and markers (v2 `_music_view`,
    /// `_user_item_data`).
    pub fn blank(item_id: &str) -> Self {
        Self {
            item_id: item_id.to_owned(),
            key: item_id.to_owned(),
            playback_position_ticks: 0,
            play_count: 0,
            is_favorite: false,
            played: false,
            last_played_date: None,
            rating: None,
            played_percentage: None,
        }
    }
}

/// The library item (v2 `BaseItemDto`). Real Jellyfin sets `LocationType`,
/// `BackdropImageTags`, and `ImageBlurHashes` non-null on every item and
/// strict clients (Manet, Swift Codable) require them.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BaseItemDto {
    pub id: String,
    pub name: String,
    #[serde(rename = "Type")]
    pub item_type: String,
    pub server_id: String,
    pub is_folder: bool,
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_time_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_year: Option<i32>,
    /// Track number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_number: Option<i32>,
    /// Disc number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_index_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_artists: Option<Vec<NameGuidPair>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_items: Option<Vec<NameGuidPair>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artists: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_primary_image_tag: Option<String>,
    pub image_tags: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genres: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_ids: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data: Option<UserItemDataDto>,
    /// Only on playlist members: the per-entry remove/reorder handle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_item_id: Option<String>,
    pub location_type: String,
    pub backdrop_image_tags: Vec<String>,
    pub image_blur_hashes: BTreeMap<String, BTreeMap<String, String>>,
}

/// Paged browse result (v2 `BaseItemDtoQueryResult`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BaseItemDtoQueryResult {
    pub items: Vec<BaseItemDto>,
    pub total_record_count: usize,
    pub start_index: usize,
}

// ===== Playback =====

/// Audio stream description (v2 `MediaStream`). Finamp hard-casts the five
/// trailing bools too (v2 issue #438), so they are always present.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaStream {
    #[serde(rename = "Type")]
    pub stream_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<u64>,
    pub channels: u32,
    pub channel_layout: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    pub is_default: bool,
    pub is_interlaced: bool,
    pub is_forced: bool,
    pub is_external: bool,
    pub is_text_subtitle_stream: bool,
    pub supports_external_stream: bool,
}

/// Playback source (v2 `MediaSourceInfo`). `DirectStreamUrl` embeds
/// `?api_key=<token>` because headerless players (Finamp, Jellify, Manet)
/// fetch it with no auth headers. The ten trailing fields are the rest of
/// Finamp's 15 non-null fields (v2 issue #438).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSourceInfo {
    pub id: String,
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_time_ticks: Option<i64>,
    pub supports_direct_play: bool,
    pub supports_direct_stream: bool,
    pub supports_transcoding: bool,
    pub default_audio_stream_index: u32,
    pub media_streams: Vec<MediaStream>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub is_remote: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_stream_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcoding_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcoding_sub_protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcoding_container: Option<String>,
    #[serde(rename = "Type")]
    pub source_type: String,
    pub is_infinite_stream: bool,
    pub requires_opening: bool,
    pub requires_closing: bool,
    pub requires_looping: bool,
    pub supports_probing: bool,
    pub read_at_native_framerate: bool,
    pub ignore_dts: bool,
    pub ignore_index: bool,
    pub gen_pts_input: bool,
}

/// PlaybackInfo result (v2 `PlaybackInfoResponse`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackInfoResponse {
    pub media_sources: Vec<MediaSourceInfo>,
    pub play_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

// ===== Lenient request bodies =====

/// Login body. Unknown fields are ignored by serde default, matching v2's
/// lenient decode (Finamp sends `UserId`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[serde(default)]
pub struct AuthenticateRequest {
    pub username: String,
    pub pw: String,
}

/// POST PlaybackInfo body (v2 `PlaybackInfoBody`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[serde(default)]
pub struct PlaybackInfoBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_streaming_bitrate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// Playlist creation body (v2 `CreatePlaylistDto`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[serde(default)]
pub struct CreatePlaylistDto {
    pub name: String,
    pub ids: Vec<String>,
    pub is_public: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// Session start body (v2 `PlaybackStartInfo`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[serde(default)]
pub struct PlaybackStartInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
}

/// Session stop body (v2 `PlaybackStopInfo`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[serde(default)]
pub struct PlaybackStopInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_time_ticks: Option<i64>,
    pub failed: bool,
}

/// Progress body (v2 `PlaybackProgressInfo`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[serde(default)]
pub struct PlaybackProgressInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_time_ticks: Option<i64>,
    pub is_paused: bool,
}
