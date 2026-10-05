//! Boundary traits to code the Jellyfin routes do not own. The router is
//! generic over every trait here; `compat::setup` binds the production
//! adapters and `fake` holds the test doubles.
//!
//! - Auth: [`crate::auth::compat_auth::jellyfin::JellyfinPasswordStore`]
//!   (from `compat_auth`). This file only adds the thin [`Principal`]
//!   view over its `JellyfinUser`. Login bodies reuse `compat_auth`'s
//!   `login_echo_json`, so the Finamp/Manet login contract has one owner.
//! - Library reads: [`LibraryRead`]. Real view services (to be bound)
//!   (paged, user-scoped, real search); the router's in-memory filtering,
//!   sorting, and paging over the snapshot only pins the quirk contract.
//! - Streaming: [`StreamEngine`]. Production binds the stream engine
//!   (real `stream_track` byte contract, ffmpeg pipe, concurrency leases).
//!   [`MemoryEngine`] replays the same range/status contract over seeded
//!   bytes so the goldens pin it; [`decide`] ports v2's transcode policy rules
//!   verbatim for the PlaybackInfo direct/transcode fork.
//! - Ids: [`IdMap`]. A persisted compat id map is the intended binding;
//!   [`MemoryIds`] ports the deterministic `sha256("kind:internal")[:32]`
//!   derivation with an in-memory reverse table.
//! - Playback sessions: [`PlaybackSessions`]. Production binds the
//!   scrobble adapter (presence + scrobble forwarding).

use crate::auth::compat_auth::jellyfin::{JellyfinUser, effective_name};

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
        }
    }
}

// ===== Auth seam =====

/// Authenticated caller: id, display name, role, and the presented token
/// (needed to embed `api_key` in `DirectStreamUrl`).
#[derive(Debug, Clone)]
pub struct Principal {
    pub id: String,
    pub name: String,
    pub role: String,
    pub token: String,
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
        }
    }

    /// The full non-null user object strict clients hard-cast on (Finamp
    /// #144/#376, Manet `EnableAllFolders`).
    pub fn user_dto(&self, server_id: &str) -> UserDto {
        UserDto {
            id: self.id.clone(),
            name: self.name.clone(),
            server_id: server_id.to_owned(),
            has_password: true,
            has_configured_password: true,
            has_configured_easy_password: false,
            configuration: super::models::UserConfiguration::default(),
            policy: super::models::UserPolicy::permissive(self.role == "admin"),
        }
    }
}

// ===== Id map seam =====

/// Opaque Jellyfin id <-> (kind, internal id). Kinds: `artist`, `album`,
/// `track`, `playlist`, `genre`, `library` (v2 `VALID_KINDS`).
pub trait IdMap: Clone + Send + Sync + 'static {
    /// Stable 32-hex id for a (kind, internal) pair.
    fn to_jf(&self, kind: &str, internal: &str) -> impl Future<Output = String> + Send;
    /// Reverse lookup; accepts dashed or undashed input, `None` when unknown.
    /// (`from_*` takes `self` by v2-naming parity, not by convention.)
    #[allow(clippy::wrong_self_convention)]
    fn from_jf(&self, jf_id: &str) -> impl Future<Output = Option<(String, String)>> + Send;
}

// ===== Library seam =====

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
    pub rg_mbid: Option<String>,
    pub artist_name: Option<String>,
    pub artist_mbid: Option<String>,
    pub album_artist_name: Option<String>,
    pub album_artist_mbid: Option<String>,
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
    /// Caller-scoped favorite flag, filled by the read methods.
    pub starred: bool,
    pub play_count: u64,
    /// Unix seconds of the last play, for the history sorts.
    pub last_played: Option<f64>,
}

/// Album row (v2 `ViewAlbum` subset).
#[derive(Debug, Clone, Default)]
pub struct AlbumView {
    pub rg_mbid: String,
    pub title: String,
    pub artist_name: Option<String>,
    pub artist_mbid: Option<String>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub track_count: usize,
    pub total_duration_seconds: Option<f64>,
    /// Unix seconds added.
    pub date_added: Option<f64>,
    pub starred: bool,
    pub play_count: u64,
    pub last_played: Option<f64>,
}

/// Artist row (v2 `ViewArtist` subset).
#[derive(Debug, Clone, Default)]
pub struct ArtistView {
    pub artist_mbid: String,
    pub name: String,
    pub album_count: usize,
    /// Unix seconds added.
    pub date_added: Option<f64>,
    pub starred: bool,
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
}

/// Served image bytes plus content type.
#[derive(Debug, Clone)]
pub struct CoverBytes {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

/// Artist listing scope: every artist vs album artists (v2
/// `/Artists` vs `/Artists/AlbumArtists`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistScope {
    All,
    Album,
}

/// Library reads behind the browse routes. List methods take the caller so
/// the adapter can fill caller-scoped `starred` flags (v2 `user=user`
/// parity); the router does filtering, sorting, and paging over the
/// returned snapshot.
pub trait LibraryRead: Clone + Send + Sync + 'static {
    fn tracks(&self, user_id: &str) -> impl Future<Output = Vec<TrackView>> + Send;
    fn track(&self, user_id: &str, file_id: &str)
    -> impl Future<Output = Option<TrackView>> + Send;
    fn albums(&self, user_id: &str) -> impl Future<Output = Vec<AlbumView>> + Send;
    fn album(&self, user_id: &str, rg_mbid: &str)
    -> impl Future<Output = Option<AlbumView>> + Send;
    fn artists(
        &self,
        user_id: &str,
        scope: ArtistScope,
    ) -> impl Future<Output = Vec<ArtistView>> + Send;
    fn artist(&self, user_id: &str, mbid: &str) -> impl Future<Output = Option<ArtistView>> + Send;
    fn genres(&self) -> impl Future<Output = Vec<GenreView>> + Send;
    fn playlists(&self, user_id: &str) -> impl Future<Output = Vec<PlaylistView>> + Send;
    fn playlist(
        &self,
        user_id: &str,
        id: &str,
    ) -> impl Future<Output = Option<PlaylistDetail>> + Send;
    fn create_playlist(&self, user_id: &str, name: &str) -> impl Future<Output = String> + Send;
    fn add_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        file_id: &str,
    ) -> impl Future<Output = ()> + Send;
    fn remove_playlist_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> impl Future<Output = ()> + Send;
    fn move_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
        index: usize,
    ) -> impl Future<Output = ()> + Send;
    /// Favorited internal ids of one kind for one caller.
    fn favorites(&self, user_id: &str, kind: &str) -> impl Future<Output = Vec<String>> + Send;
    fn set_favorite(
        &self,
        user_id: &str,
        kind: &str,
        internal: &str,
        add: bool,
    ) -> impl Future<Output = ()> + Send;
    /// Release art bytes (`size` is the 250/500/1200 bucket); `None` renders
    /// 404 with no placeholder, unlike Subsonic (v2 `_image`).
    fn cover(&self, rg_mbid: &str, size: &str) -> impl Future<Output = Option<CoverBytes>> + Send;
    fn artist_image(&self, mbid: &str) -> impl Future<Output = Option<CoverBytes>> + Send;
    /// Etags for `ImageTags.Primary` / `AlbumPrimaryImageTag`.
    fn cover_tag(&self, rg_mbid: &str) -> impl Future<Output = Option<String>> + Send;
    fn artist_tag(&self, mbid: &str) -> impl Future<Output = Option<String>> + Send;
}

// ===== Streaming seam =====

/// Inputs to the direct-vs-transcode policy (v2 `decide()`).
#[derive(Debug, Clone)]
pub struct DecideInput<'a> {
    /// Lowercase source container (`track.file_format`).
    pub src_format: Option<&'a str>,
    /// Source bitrate in kbps (`track.bitrate or 0`).
    pub src_bitrate_kbps: u32,
    /// Client-requested output (`requested_format`), already codec-mapped.
    pub requested: Option<&'a str>,
    /// Client ceiling in kbps (`max_bitrate_kbps`).
    pub ceiling_kbps: Option<u32>,
    pub force_original: bool,
    pub start_seconds: f64,
    pub transcoding_enabled: bool,
    /// Server quality ceiling, default 320: a ceiling, never a trigger.
    pub server_max_kbps: u32,
    /// Default output when the request names none usable.
    pub default_format: &'a str,
    pub ffmpeg: bool,
}

/// The policy verdict (v2 `StreamPlan` subset).
#[derive(Debug, Clone, PartialEq)]
pub enum StreamPlan {
    Direct,
    Transcode {
        format: String,
        bitrate_kbps: u32,
        start_seconds: f64,
    },
}

/// Direct-vs-transcode policy, v2 `transcode_service.decide()` rules in
/// order: silent direct fallback first; a transcode needs an explicit client
/// request (codec mismatch or client ceiling below source); the server max
/// only caps quality once transcoding.
pub fn decide(input: &DecideInput) -> StreamPlan {
    const HUGE: u32 = 1_000_000_000;
    const MIN_BITRATE_KBPS: u32 = 64;
    if input.force_original || !input.transcoding_enabled || !input.ffmpeg {
        return StreamPlan::Direct;
    }
    let ceiling = match input.ceiling_kbps {
        Some(c) if c > 0 => c,
        _ => HUGE,
    };
    let src = input.src_format.unwrap_or("").to_lowercase();
    let req = input.requested.unwrap_or("").to_lowercase();
    let codec_mismatch = !req.is_empty() && req != "raw" && req != src;
    let over_ceiling = ceiling < input.src_bitrate_kbps;
    if !codec_mismatch && !over_ceiling {
        return StreamPlan::Direct;
    }
    let out = if req == "mp3" || req == "opus" {
        req
    } else {
        input.default_format.to_lowercase()
    };
    StreamPlan::Transcode {
        format: out,
        bitrate_kbps: ceiling.min(input.server_max_kbps).max(MIN_BITRATE_KBPS),
        start_seconds: input.start_seconds.max(0.0),
    }
}

/// Served audio bytes: status, headers, body.
#[derive(Debug, Clone)]
pub struct ByteOutcome {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Audio bytes behind the `/Audio` routes. Production binds the stream
/// engine (range-capable file streams, ffmpeg pipe, concurrency leases with
/// 429 + `Retry-After: 1`); the router only maps outcomes to responses.
pub trait StreamEngine: Clone + Send + Sync + 'static {
    /// The engine scoped to one authenticated caller, so stream leases
    /// count against that user. Engines without leases return themselves.
    fn for_caller(&self, _user_id: &str) -> Self {
        self.clone()
    }

    /// Direct bytes with range support (200/206/416 contract).
    fn direct(
        &self,
        file_id: &str,
        range: Option<&str>,
    ) -> impl Future<Output = ByteOutcome> + Send;
    /// HEAD outcome for a file: the same status and headers a `direct`
    /// GET would answer (200/206/416, unknown ids 404), always with an
    /// empty body. The router maps it straight onto the response.
    fn head(&self, file_id: &str, range: Option<&str>) -> impl Future<Output = ByteOutcome> + Send;
    /// Transcoded bytes (estimate off on Jellyfin: never a Content-Length).
    /// The real adapter must serve an unsized streaming
    /// body (ffmpeg pipe). Axum auto-adds `Content-Length` to sized bodies,
    /// so a sized adapter response would violate the v2 contract on the wire;
    /// only an unknown size hint keeps the header off.
    fn transcode(
        &self,
        file_id: &str,
        format: &str,
        bitrate_kbps: u32,
        start_seconds: f64,
    ) -> impl Future<Output = ByteOutcome> + Send;
}

/// Content-Type per extension, v2's list verbatim.
pub fn content_type_for_format(format: Option<&str>) -> &'static str {
    match format.unwrap_or("").to_lowercase().as_str() {
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "wma" => "audio/x-ms-wma",
        "opus" => "audio/opus",
        _ => "application/octet-stream",
    }
}

// ===== Playback sessions seam =====

/// Presence + scrobble calls behind the `/Sessions/Playing*` routes.
/// Production binds the compat scrobble adapter.
pub trait PlaybackSessions: Clone + Send + Sync + 'static {
    fn mark_started(&self, user_id: &str, key: &str) -> impl Future<Output = ()> + Send;
    fn pop_started(&self, user_id: &str, key: &str) -> impl Future<Output = Option<String>> + Send;
    fn now_playing(
        &self,
        user_id: &str,
        file_id: &str,
        client: Option<&str>,
    ) -> impl Future<Output = ()> + Send;
    fn progress(
        &self,
        user_id: &str,
        file_id: &str,
        position_ms: Option<i64>,
        paused: bool,
    ) -> impl Future<Output = ()> + Send;
    fn clear_presence(
        &self,
        user_id: &str,
        client: Option<&str>,
    ) -> impl Future<Output = ()> + Send;
    fn scrobble(
        &self,
        user_id: &str,
        file_id: &str,
        client: Option<&str>,
    ) -> impl Future<Output = ()> + Send;
}

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
