//! PascalCase Jellyfin DTOs, ported from v2 `api/compat/jellyfin/models.py`.
//!
//! Field declaration order matches the v2 msgspec structs, because serde
//! emits fields in declaration order and the goldens pin the wire bytes.
//! `None` serializes to nothing (`skip_serializing_none`), never `null`
//! (v2 `_strip_none` parity).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use droppedneedle_auth::compat_auth::jellyfin::PolicyFacts;

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

/// `GET /System/Info` shape (v2 `SystemInfo`), plus the installation
/// fields Jellyfin 10.10.6 sends (`system.info.json` in the references).
/// The path fields are empty: this server does not report its host.
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
    pub web_socket_port_number: u16,
    pub completed_installations: Vec<String>,
    pub can_self_restart: bool,
    pub can_launch_web_browser: bool,
    pub program_data_path: String,
    pub web_path: String,
    pub items_by_name_path: String,
    pub cache_path: String,
    pub log_path: String,
    pub internal_metadata_path: String,
    /// Always empty: this server has no cast receivers to list.
    pub cast_receiver_applications: Vec<String>,
    pub has_update_available: bool,
    pub encoder_location: String,
    pub system_architecture: String,
    pub operating_system_display_name: String,
    pub transcoding_temp_path: String,
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
    /// Empty: no subtitle language is stored.
    pub subtitle_language_preference: String,
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
    /// Empty: the app has no cast receiver.
    pub cast_receiver_id: String,
}

impl Default for UserConfiguration {
    fn default() -> Self {
        Self {
            audio_language_preference: None,
            play_default_audio_track: true,
            subtitle_language_preference: String::new(),
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
            cast_receiver_id: String::new(),
        }
    }
}

/// User policy. `EnableAllFolders` must be true or strict clients (Manet)
/// conclude "no libraries" and never call `/UserViews`; every bool must be
/// present or Finamp's non-nullable casts crash login (v2 issues #144, #376).
///
/// A permission is true only when the Jellyfin API here lets the user do
/// it. Nothing can be deleted through it, it has no collections, channels,
/// video, remuxing, sync jobs or SyncPlay groups, it takes no lyrics or
/// subtitles and it saves no user preferences, so those are false (or
/// `"None"`) for everyone.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserPolicy {
    pub is_administrator: bool,
    /// True: Jellyfin hides a hidden user from the list of accounts it
    /// shows before sign-in (`/Users/Public`, read in the 10.10.6 source),
    /// and this API shows nobody there.
    pub is_hidden: bool,
    pub is_disabled: bool,
    pub enable_all_folders: bool,
    pub enabled_folders: Vec<String>,
    pub enable_all_channels: bool,
    pub enabled_channels: Vec<String>,
    pub enable_all_devices: bool,
    pub enabled_devices: Vec<String>,
    pub enable_media_playback: bool,
    /// True while transcoding is switched on and ffmpeg is present.
    pub enable_audio_playback_transcoding: bool,
    pub enable_video_playback_transcoding: bool,
    pub enable_playback_remuxing: bool,
    /// True for a user the admin's download setting admits.
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
    pub enable_collection_management: bool,
    pub enable_subtitle_management: bool,
    pub enable_lyric_management: bool,
    pub force_remote_source_transcoding: bool,
    pub enable_content_deletion_from_folders: Vec<String>,
    /// -1, Jellyfin's "never": no number of failures disables an account
    /// here. The back-off after failed sign-ins is per address and timed.
    pub login_attempts_before_lockout: i32,
    /// 0, Jellyfin's "no limit".
    pub max_active_sessions: u32,
    pub blocked_media_folders: Vec<String>,
    pub blocked_channels: Vec<String>,
    /// Empty, like the next: Jellyfin names a class of its own here and
    /// the app has no such part.
    pub authentication_provider_id: String,
    pub password_reset_provider_id: String,
}

impl UserPolicy {
    /// The policy of one user. Key for key what `login_echo_json` writes.
    pub fn for_user(is_admin: bool, facts: PolicyFacts) -> Self {
        Self {
            is_administrator: is_admin,
            is_hidden: true,
            is_disabled: false,
            enable_all_folders: true,
            enabled_folders: Vec::new(),
            enable_all_channels: false,
            enabled_channels: Vec::new(),
            enable_all_devices: true,
            enabled_devices: Vec::new(),
            enable_media_playback: true,
            enable_audio_playback_transcoding: facts.audio_transcoding,
            enable_video_playback_transcoding: false,
            enable_playback_remuxing: false,
            enable_content_downloading: facts.content_downloading,
            enable_remote_access: true,
            enable_sync_transcoding: false,
            enable_user_preference_access: false,
            enable_live_tv_access: false,
            enable_live_tv_management: false,
            enable_content_deletion: false,
            enable_media_conversion: false,
            enable_public_sharing: false,
            enable_remote_control_of_other_users: false,
            enable_shared_device_control: false,
            invalid_login_attempt_count: 0,
            remote_client_bitrate_limit: 0,
            sync_play_access: "None".to_owned(),
            blocked_tags: Vec::new(),
            allowed_tags: Vec::new(),
            access_schedules: Vec::new(),
            block_unrated_items: Vec::new(),
            enable_collection_management: false,
            enable_subtitle_management: false,
            enable_lyric_management: false,
            force_remote_source_transcoding: false,
            enable_content_deletion_from_folders: Vec::new(),
            login_attempts_before_lockout: -1,
            max_active_sessions: 0,
            blocked_media_folders: Vec::new(),
            blocked_channels: Vec::new(),
            authentication_provider_id: String::new(),
            password_reset_provider_id: String::new(),
        }
    }
}

/// User object for `/Users/Me` and `/Users/{id}` (the `{id}` is ignored,
/// v2 parity). The login echo itself is rendered by `compat_auth`'s
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
    /// False: the app has no sign-in without a password.
    pub enable_auto_login: bool,
    /// When the app password behind this request was last used; at
    /// sign-in, now.
    pub last_login_date: String,
    /// The time of this request.
    pub last_activity_date: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub premiere_date: Option<String>,
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
    /// Tracks only, always false: these routes serve no lyrics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_lyrics: Option<bool>,
    pub location_type: String,
    pub backdrop_image_tags: Vec<String>,
    pub image_blur_hashes: BTreeMap<String, BTreeMap<String, String>>,
    /// Only on one track fetched by id: its file and audio stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_sources: Option<Vec<MediaSourceInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_streams: Option<Vec<MediaStream>>,
    /// The long form Jellyfin sends for one item fetched by id and for the
    /// library view; lists stay short.
    #[serde(flatten)]
    pub detail: Option<ItemDetail>,
}

/// An array this API always sends empty (people, studios, trailers, tags,
/// locked fields): nothing here records them.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmptyList;

impl Serialize for EmptyList {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        serializer.serialize_seq(Some(0))?.end()
    }
}

/// One `ExternalUrls` entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExternalUrl {
    pub name: String,
    pub url: String,
}

/// The keys only the long form of an item carries.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ItemDetail {
    /// Changes whenever what is sent about the item changes.
    pub etag: String,
    pub can_delete: bool,
    pub can_download: bool,
    /// A track's path under its library folder; `""` for the rest, which
    /// have no one folder here. Never a host path.
    pub path: String,
    pub external_urls: Vec<ExternalUrl>,
    pub enable_media_source_display: bool,
    pub taglines: EmptyList,
    pub play_access: String,
    pub remote_trailers: EmptyList,
    pub people: EmptyList,
    pub studios: EmptyList,
    pub genre_items: Vec<NameGuidPair>,
    pub local_trailer_count: u32,
    pub special_feature_count: u32,
    pub display_preferences_id: String,
    pub tags: EmptyList,
    pub locked_fields: EmptyList,
    pub lock_data: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cumulative_run_time_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursive_item_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub song_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub music_video_count: Option<usize>,
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

/// One stream of a file (v2 `MediaStream`): the audio, and a `Lyric` entry
/// when the file carries lyrics. Finamp hard-casts the five trailing bools
/// too (v2 issue #438), so they are always present. The audio-only fields
/// are left out of a `Lyric` entry, as Jellyfin leaves them out.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_layout: Option<String>,
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
    /// `1/<sample rate>`, when the sample rate is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_base: Option<String>,
    /// Codec and channels, as Jellyfin titles the stream (`FLAC - Stereo`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_title: Option<String>,
    /// The fields below are fixed: this server has no video, spatial
    /// audio, accessibility tracks or translated labels.
    pub video_range: String,
    pub video_range_type: String,
    pub audio_spatial_format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub localized_default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub localized_external: Option<String>,
    #[serde(rename = "IsAVC", skip_serializing_if = "Option::is_none")]
    pub is_avc: Option<bool>,
    pub is_hearing_impaired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
}

/// `GET /Audio/{id}/Lyrics` (Jellyfin `LyricDto`), as Jellyfin 10.10.6
/// answers it in `jellyfin-10.10.6-lyrics/lyrics.airbag.json`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct LyricDto {
    pub metadata: LyricMetadata,
    pub lyrics: Vec<LyricLine>,
}

/// What this server knows about the lyrics: only whether they are synced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct LyricMetadata {
    pub is_synced: bool,
}

/// One line. `Start` is in ticks and left out of an untimed line.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct LyricLine {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
}

/// Playback source (v2 `MediaSourceInfo`). `DirectStreamUrl` embeds
/// `?api_key=<token>` because headerless players (Finamp, Jellify, Manet)
/// fetch it with no auth headers; it is sent only in the playback
/// information answer. The ten bools after `Type` are the rest of
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
    /// The file's name without its extension.
    pub name: String,
    /// The file's path under its library folder, never a host path.
    pub path: String,
    /// Changes whenever the file's row does.
    #[serde(rename = "ETag")]
    pub etag: String,
    pub is_remote: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_stream_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcoding_url: Option<String>,
    /// Always `http`, as Jellyfin sends it.
    pub transcoding_sub_protocol: String,
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
    /// The fields below are fixed: a library file has no attachments,
    /// segments or required headers.
    pub formats: EmptyList,
    pub media_attachments: EmptyList,
    pub required_http_headers: BTreeMap<String, String>,
    pub use_most_compatible_transcoding_profile: bool,
    pub has_segments: bool,
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
