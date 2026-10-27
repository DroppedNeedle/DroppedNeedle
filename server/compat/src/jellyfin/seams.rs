//! The settings, views and records the Jellyfin routes exchange with the
//! adapters they call.
//!
//! - Auth: the routes name the app-password store,
//!   [`droppedneedle_auth::compat_auth::prod::ProdCompatPasswords`],
//!   directly; this file only adds the thin [`Principal`] view over its
//!   `JellyfinUser`. Login bodies reuse `compat_auth`'s
//!   `login_echo_json`, so the Finamp/Manet login contract has one owner.
//! - Library reads:
//!   [`JellyfinLibrary`](crate::adapters::jellyfin_library::JellyfinLibrary).
//!   The routes describe one page ([`TrackFilter`], [`AlbumFilter`],
//!   [`ItemSort`], start, limit) and the library answers that page plus the
//!   match total, so it filters, orders and pages in SQL and the routes
//!   shape only the page.
//! - Streaming: [`GatewayAudio`](crate::adapters::audio::GatewayAudio)
//!   over the stream gateway, which makes the one transcode decision (byte
//!   contract, ffmpeg pipe, concurrency leases). The routes only translate
//!   Jellyfin's parameters into [`StreamParams`].
//! - Ids: [`CatalogIds`](crate::adapters::jellyfin_library::CatalogIds),
//!   the stored ids, album and artist ids made with the data key.
//! - Playback sessions:
//!   [`CompatPlayback`](crate::adapters::playback::CompatPlayback)
//!   (presence and scrobble forwarding).

use droppedneedle_auth::compat_auth::jellyfin::{JellyfinUser, PolicyFacts, effective_name};

use droppedneedle_kernel::runtime_config::sections::DownloadAccess;
pub use droppedneedle_media::stream::routes::StreamParams;
use droppedneedle_media::stream::transcode::{OutFormat, TranscodeSettings};

use super::models::UserDto;

// ===== Settings =====

/// The connect-apps settings subset the shim reads (v2
/// `get_connect_apps_settings`).
#[derive(Debug, Clone)]
pub struct JellyfinSettings {
    /// Kill switch, default off (v2 `jellyfin_enabled`). Disabled → HTTP 404
    /// on every route, before handler lookup (no method enumeration).
    pub enabled: bool,
    /// User-editable advertised name, default "DroppedNeedle".
    pub server_name: String,
    /// User-editable advertised version, default "10.10.6".
    pub server_version: String,
    /// Transcoding master switch.
    pub transcoding_enabled: bool,
    /// Quality ceiling in kbps, default 320: a ceiling, never a trigger.
    pub transcode_max_bitrate_kbps: u32,
    /// Default transcode output (`mp3` or `opus`).
    pub transcode_default_format: String,
    /// Whether ffmpeg is present (silent direct fallback when absent).
    pub ffmpeg_available: bool,
    /// The admin's setting for who may download library files.
    pub download_access: DownloadAccess,
}

impl JellyfinSettings {
    /// The policy values that depend on these settings, for a user with
    /// `role`.
    pub fn policy_facts(&self, role: &str) -> PolicyFacts {
        PolicyFacts {
            audio_transcoding: self.transcoding_enabled && self.ffmpeg_available,
            content_downloading: self.download_access.admits(role),
        }
    }
}

impl JellyfinSettings {
    /// The transcode policy these settings set, for the one decision.
    pub fn transcode_policy(&self) -> TranscodeSettings {
        TranscodeSettings {
            transcoding_enabled: self.transcoding_enabled,
            default_format: if self.transcode_default_format.eq_ignore_ascii_case("opus") {
                OutFormat::Opus
            } else {
                OutFormat::Mp3
            },
            max_bitrate_kbps: i64::from(self.transcode_max_bitrate_kbps),
        }
    }
}

impl Default for JellyfinSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            server_name: "DroppedNeedle".to_owned(),
            server_version: "10.10.6".to_owned(),
            transcoding_enabled: true,
            transcode_max_bitrate_kbps: 320,
            transcode_default_format: "mp3".to_owned(),
            ffmpeg_available: true,
            download_access: DownloadAccess::Everyone,
        }
    }
}

// ===== Auth =====

/// Authenticated caller: id, display name, role, and the presented token
/// (needed to embed `api_key` in `DirectStreamUrl`).
#[derive(Debug, Clone)]
pub struct Principal {
    pub id: String,
    pub name: String,
    pub role: String,
    pub token: String,
    /// When the presented app password was last used before this request,
    /// unix seconds.
    pub last_used_at: Option<i64>,
}

impl Principal {
    /// Build from `compat_auth`'s user (display-name `or`-chain parity via
    /// `effective_name`).
    pub fn from_user(user: &JellyfinUser, token: &str) -> Self {
        Self {
            id: user.id.clone(),
            name: effective_name(user).to_owned(),
            role: user.role.clone(),
            token: token.to_owned(),
            last_used_at: user.last_used_at,
        }
    }

    /// The full non-null user object strict clients hard-cast on (Finamp
    /// #144/#376, Manet `EnableAllFolders`). `now` is the time of this
    /// request in unix seconds. An app password with no recorded use is
    /// being used for the first time by this request, so its last use is
    /// `now` as well.
    pub fn user_dto(&self, server_id: &str, now: f64, policy: PolicyFacts) -> UserDto {
        let last_used = self.last_used_at.map_or(now, |at| at as f64);
        UserDto {
            id: self.id.clone(),
            name: self.name.clone(),
            server_id: server_id.to_owned(),
            has_password: true,
            has_configured_password: true,
            has_configured_easy_password: false,
            enable_auto_login: false,
            last_login_date: super::builders::iso_o(Some(last_used)),
            last_activity_date: super::builders::iso_o(Some(now)),
            configuration: super::models::UserConfiguration::default(),
            policy: super::models::UserPolicy::for_user(self.role == "admin", policy),
        }
    }
}

// ===== Library views =====

/// Ticks per second (v2 `JELLYFIN_TICKS_PER_SECOND`).
pub const TICKS_PER_SECOND: i64 = 10_000_000;

/// Track row the builders shape into an `Audio` DTO (v2 `ViewTrack` subset).
#[derive(Debug, Clone, Default)]
pub struct TrackView {
    pub file_id: String,
    pub title: String,
    pub duration_seconds: Option<f64>,
    pub year: Option<i32>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub album_title: Option<String>,
    /// Local album id.
    pub album_id: Option<String>,
    pub artist_name: Option<String>,
    /// Local id of the track artist.
    pub artist_id: Option<String>,
    pub album_artist_name: Option<String>,
    /// Local id of the album artist.
    pub album_artist_id: Option<String>,
    pub genre: Option<String>,
    pub file_format: Option<String>,
    /// Source bitrate in kbps (v2 `ViewTrack.bitrate`).
    pub bitrate: Option<u32>,
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub file_size_bytes: Option<u64>,
    /// Unix seconds added.
    pub created_at: Option<f64>,
    pub recording_mbid: Option<String>,
    /// MusicBrainz ids of the track artist, the album artist, and the
    /// album's release group and release: the accepted match's, else the
    /// ones the files' tags agree on (the album artist's accepted only).
    pub artist_musicbrainz_id: Option<String>,
    pub album_artist_musicbrainz_id: Option<String>,
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
    /// Path under the track's own library folder (the Subsonic `path`),
    /// never a host path.
    pub path: Option<String>,
    /// Caller-scoped favorite flag, filled by the read methods.
    pub starred: bool,
    /// Caller-scoped play state, with the caller's played and unplayed
    /// marks weighed in: played, play count, and the last play in unix
    /// seconds.
    pub played: bool,
    pub play_count: u64,
    pub last_played: Option<f64>,
    /// `AlbumPrimaryImageTag` of the owning album, when it has art.
    pub album_image_tag: Option<String>,
    /// The track's file carries lyrics (a `.lrc` beside it or a tag).
    pub has_lyrics: bool,
}

/// One track's lyrics, as the lyrics address answers them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LyricsView {
    /// Lines in order: text plus the start in milliseconds, when timed.
    pub lines: Vec<(String, Option<i64>)>,
    /// True when the lines carry timestamps.
    pub synced: bool,
}

/// One track, album or artist, as the browse reads give it.
#[derive(Debug, Clone)]
pub enum ItemView {
    Track(Box<TrackView>),
    Album(Box<AlbumView>),
    Artist(Box<ArtistView>),
}

/// A library or id read failed, which is never absence. It answers through
/// the compat boundary, status only: a busy database 503 with
/// `Retry-After`, anything else 500. The text is the cause, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadFailed {
    /// The database was too busy to answer in time.
    Busy(String),
    /// Any other failure.
    Failed(String),
}

impl From<ReadFailed> for droppedneedle_kernel::error::ApiError {
    fn from(failed: ReadFailed) -> Self {
        use droppedneedle_kernel::error::Unavailable;
        match failed {
            ReadFailed::Busy(cause) => Self::unavailable(Unavailable::ServerBusy, cause),
            ReadFailed::Failed(cause) => Self::internal(cause),
        }
    }
}

/// Album row (v2 `ViewAlbum` subset).
#[derive(Debug, Clone, Default)]
pub struct AlbumView {
    /// Local album id: the Jellyfin item id and image tag come from it.
    pub id: String,
    /// MusicBrainz release group: the accepted match's, else the one the
    /// album's files' tags agree on.
    pub release_group_mbid: Option<String>,
    /// MusicBrainz release, the same way.
    pub release_mbid: Option<String>,
    /// MusicBrainz id of the album artist, when identified.
    pub artist_musicbrainz_id: Option<String>,
    pub title: String,
    pub artist_name: Option<String>,
    /// Local id of the album artist.
    pub artist_id: Option<String>,
    pub year: Option<i32>,
    /// Original release date, `YYYY[-MM[-DD]]`, when the catalog has one.
    pub release_date: Option<String>,
    pub genre: Option<String>,
    pub track_count: usize,
    pub total_duration_seconds: Option<f64>,
    /// Unix seconds added.
    pub date_added: Option<f64>,
    pub starred: bool,
    /// Caller-scoped play state, as for a track: played once every
    /// streamable track on it is.
    pub played: bool,
    pub play_count: u64,
    pub last_played: Option<f64>,
    /// `ImageTags.Primary`, when the album has art.
    pub image_tag: Option<String>,
}

/// Artist row (v2 `ViewArtist` subset).
#[derive(Debug, Clone, Default)]
pub struct ArtistView {
    /// Local artist id: the Jellyfin item id and image tag come from it.
    pub id: String,
    /// Accepted MusicBrainz artist id, when the artist has one.
    pub artist_mbid: Option<String>,
    pub name: String,
    /// Albums the artist leads.
    pub album_count: usize,
    /// Albums the artist appears on without leading them.
    pub appearance_album_count: usize,
    /// Playable tracks crediting the artist as track or album artist.
    pub track_count: usize,
    /// Summed length of those tracks. This is what Jellyfin 10.10.6 means
    /// by an artist's `RunTimeTicks`: `MetadataService`
    /// `UpdateCumulativeRunTimeTicks` sums the non-folder children that
    /// `ArtistMetadataService.GetChildrenForMetadataUpdates` returns, the
    /// items tagged with the artist as artist or album artist
    /// (`SqliteItemRepository`, `ArtistIds`, `Type<=1`).
    pub total_duration_seconds: f64,
    /// Unix seconds added.
    pub date_added: Option<f64>,
    pub starred: bool,
    /// `ImageTags.Primary`, when the artist has an image.
    pub image_tag: Option<String>,
}

/// Genre row (v2 `ViewGenre` subset).
#[derive(Debug, Clone, Default)]
pub struct GenreView {
    pub name: String,
    pub song_count: usize,
}

/// Playlist browse row (v2 `ViewPlaylist` subset). Counts are streamable
/// entries only, matching the served `/Items` listing (v2 issue #181).
#[derive(Debug, Clone, Default)]
pub struct PlaylistView {
    pub id: String,
    pub name: String,
    pub track_count: usize,
    pub total_duration_seconds: Option<f64>,
}

/// One playlist entry: the per-entry remove/reorder handle plus the linked
/// library file (entries without one are never served).
#[derive(Debug, Clone)]
pub struct PlaylistEntry {
    pub id: String,
    pub file_id: Option<String>,
}

/// Playlist detail for the Items/add/remove/move routes.
#[derive(Debug, Clone, Default)]
pub struct PlaylistDetail {
    pub id: String,
    pub name: String,
    pub entries: Vec<PlaylistEntry>,
    /// Whether every user may read it (the playlist's public flag).
    pub open_access: bool,
}

/// Served image bytes plus content type.
#[derive(Debug, Clone)]
pub struct CoverBytes {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

/// Artist listing scope: every artist with music vs album artists (v2
/// `/Artists` vs `/Artists/AlbumArtists`). Empty artist rows are never
/// listed: player apps would show them as artists with nothing in them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistScope {
    All,
    Album,
}

/// Why a library write was refused. The routes answer 404, 403, 400, 409,
/// 503 or 500.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteRefusal {
    /// The item does not exist for this caller.
    NotFound,
    /// The item is not the caller's to change.
    Forbidden,
    /// Bad input.
    Invalid,
    /// Valid input against the wrong state.
    Conflict,
    /// The database was too busy and nothing was written; the text is for
    /// the log only.
    Busy(String),
    /// Server fault; the text is for the log only.
    Failed(String),
}

/// Track filters for one page; every set field narrows.
#[derive(Debug, Clone, Default)]
pub struct TrackFilter {
    /// Tracks of one album.
    pub album: Option<String>,
    /// Tracks crediting any of these artists.
    pub artists: Vec<String>,
    /// Tracks on albums by any of these album artists.
    pub album_artists: Vec<String>,
    /// Title, artist or album substring, case-insensitive.
    pub search: Option<String>,
    /// Only tracks that are played for the caller (`true`) or not
    /// (`false`).
    pub played: Option<bool>,
}

/// Album filters for one page; every set field narrows.
#[derive(Debug, Clone, Default)]
pub struct AlbumFilter {
    /// Albums by any of these album artists.
    pub artists: Vec<String>,
    /// Albums where one of these artists is credited on a track but is not
    /// the album artist ("appears on").
    pub appears_on: Vec<String>,
    /// Title or artist substring, case-insensitive.
    pub search: Option<String>,
    /// Only albums that are played for the caller (`true`) or not
    /// (`false`).
    pub played: Option<bool>,
}

/// Page order. The play-history keys (`DatePlayed`, `PlayCount`) keep
/// every item, as Jellyfin does: what the caller never played sorts last
/// when descending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemSort {
    /// The library's own order: title for tracks and albums.
    Catalog,
    /// Disc then track number (one album's running order).
    Disc,
    /// One `SortBy` key, ascending or (`true`) descending.
    By(super::params::SortKey, bool),
}

/// Everything from `start`; the page reads stop at the match total.
pub const ALL: usize = usize::MAX;

// ===== Streaming =====

/// Served audio: status, headers, body (in memory or streamed).
#[derive(Debug, Clone)]
pub struct ByteOutcome {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: crate::body::AudioBody,
}

/// Content-Type per extension, from the stream routes' table (unknown ->
/// octet-stream).
pub fn content_type_for_format(format: Option<&str>) -> &'static str {
    droppedneedle_media::stream::routes::content_type_for_extension(format.unwrap_or(""))
        .unwrap_or("application/octet-stream")
}

// ===== Playback sessions =====

/// Whether a stop report counts as a play (v2 `_should_scrobble`): an
/// omitted position counts; otherwise past 90% or within a second of the
/// end. `Failed` stops never reach this check.
pub fn should_scrobble(position_ticks: Option<i64>, runtime_ticks: Option<i64>) -> bool {
    let Some(position) = position_ticks else {
        return true;
    };
    if let Some(runtime) = runtime_ticks
        && runtime > 0
    {
        if position as f64 / runtime as f64 * 100.0 > 90.0 {
            return true;
        }
        if position >= runtime - TICKS_PER_SECOND {
            return true;
        }
    }
    false
}
