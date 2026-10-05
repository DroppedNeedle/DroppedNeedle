//! Data seam: the [`Store`] trait plus the records handlers need.
//!
//! The seam to the real v3 library, playlist, favorites, scrobble, queue,
//! bookmark, lyrics, avatar, and scan stores. The trait is async and
//! generic (`RPITIT` style, matching `auth::compat_auth`); handlers take
//! `&impl Store`.
//!
//! v2: `CompatServices` behind the Subsonic router.

use std::collections::HashMap;

use super::ids::IdKind;
use super::views::{ViewAlbum, ViewArtist, ViewGenre, ViewTrack};

/// Album-list sort keys (v2 `_ALBUMLIST_SORTS` + byYear directions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumSort {
    /// Recently added.
    Recent,
    /// By title.
    Title,
    /// By artist name.
    Artist,
    /// Random order.
    Random,
    /// Year ascending (byYear with fromYear <= toYear).
    YearAsc,
    /// Year descending (byYear with fromYear > toYear).
    YearDesc,
}

/// One playlist entry: `library_file_id` is None for legacy/outbound
/// entries, which are skipped as not streamable (#181).
#[derive(Debug, Clone, Default)]
pub struct PlaylistEntry {
    /// Entry id.
    pub id: String,
    /// Linked library file id, when streamable.
    pub library_file_id: Option<String>,
}

/// Playlist record facts.
#[derive(Debug, Clone, Default)]
pub struct PlaylistRecord {
    /// Playlist id.
    pub id: String,
    /// Playlist name.
    pub name: String,
    /// Public flag.
    pub is_public: bool,
    /// True when the playlist has cover art.
    pub has_cover: bool,
    /// Created ISO timestamp.
    pub created_at: Option<String>,
    /// Updated ISO timestamp.
    pub changed_at: Option<String>,
}

/// Playlist list row (None record = redacted private list, skipped).
#[derive(Debug, Clone, Default)]
pub struct PlaylistSummary {
    /// Record, or None when redacted.
    pub record: Option<PlaylistRecord>,
    /// True when the caller owns it.
    pub is_owner: bool,
    /// Owner display name.
    pub owner_name: String,
}

/// Playlist detail: record plus entries in order.
#[derive(Debug, Clone, Default)]
pub struct PlaylistDetail {
    /// Record.
    pub record: PlaylistRecord,
    /// Entries in order.
    pub tracks: Vec<PlaylistEntry>,
    /// True when the caller owns it.
    pub is_owner: bool,
    /// Owner display name.
    pub owner_name: String,
}

/// Saved play queue.
#[derive(Debug, Clone, Default)]
pub struct QueueState {
    /// File ids in order.
    pub file_ids: Vec<String>,
    /// Current index into `file_ids`.
    pub current_index: Option<usize>,
    /// Position ms.
    pub position_ms: i64,
    /// Updated unix seconds.
    pub updated_at: f64,
    /// Client that last changed it.
    pub changed_by_client: String,
}

/// Bookmark record.
#[derive(Debug, Clone, Default)]
pub struct BookmarkRecord {
    /// File id.
    pub file_id: String,
    /// Position ms.
    pub position_ms: i64,
    /// Comment.
    pub comment: String,
    /// Created unix seconds.
    pub created_at: i64,
    /// Changed unix seconds.
    pub changed_at: i64,
}

/// Now-playing presence row.
#[derive(Debug, Clone, Default)]
pub struct NowPlayingRow {
    /// Listener display name.
    pub user_name: String,
    /// File id.
    pub file_id: String,
    /// Last update unix seconds.
    pub updated_at: f64,
    /// Source label (preferred for playerName).
    pub source: Option<String>,
    /// Device name (fallback for playerName).
    pub device_name: Option<String>,
}

/// One lyric line.
#[derive(Debug, Clone, Default)]
pub struct LyricLine {
    /// Line text.
    pub value: String,
    /// Start ms (synced only).
    pub start_ms: Option<i64>,
}

/// Stored lyrics for a track.
#[derive(Debug, Clone, Default)]
pub struct LyricsData {
    /// Language code.
    pub language: String,
    /// Whether lines carry timestamps.
    pub synced: bool,
    /// Lines.
    pub lines: Vec<LyricLine>,
}

/// Stream facts inside an advanced transcode decision.
#[derive(Debug, Clone, Default)]
pub struct StreamDetailsData {
    /// Protocol.
    pub protocol: String,
    /// Container.
    pub container: String,
    /// Codec.
    pub codec: String,
    /// Audio channels.
    pub audio_channels: Option<i64>,
    /// Audio bitrate.
    pub audio_bitrate: Option<i64>,
    /// Sample rate.
    pub audio_samplerate: Option<i64>,
    /// Bit depth.
    pub audio_bitdepth: Option<i64>,
}

/// Advanced transcode decision (signed params minted inside).
#[derive(Debug, Clone, Default)]
pub struct TranscodeDecisionData {
    /// Direct play possible.
    pub can_direct_play: bool,
    /// Transcode possible.
    pub can_transcode: bool,
    /// Why a transcode is needed.
    pub transcode_reason: Vec<String>,
    /// Why nothing is possible.
    pub error_reason: Option<String>,
    /// Signed params for getTranscodeStream.
    pub transcode_params: Option<String>,
    /// Source stream facts.
    pub source_stream: Option<StreamDetailsData>,
    /// Target stream facts.
    pub transcode_stream: Option<StreamDetailsData>,
}

/// Client playback capabilities (getTranscodeDecision JSON body).
/// Parsed and passed to [`Store::advanced_decide`]; no store reads it yet.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    /// Client name.
    pub name: String,
    /// Client platform.
    pub platform: String,
    /// Max audio bitrate (0 = unset, Feishin #464/#468).
    pub max_audio_bitrate: Option<i64>,
    /// Max transcoding audio bitrate (0 = unset).
    pub max_transcoding_audio_bitrate: Option<i64>,
}

/// The library/services backing store. Every method reads or mutates
/// one v2 `CompatServices` field; names follow the v2 call sites.
pub trait Store: Clone + Send + Sync {
    /// Storage error; surfaced as code 0 (generic).
    type Error: std::fmt::Display + Send;

    /// Artists page, optional match query (None = match-all).
    fn get_artists(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
    ) -> impl Future<Output = Result<(Vec<ViewArtist>, usize), Self::Error>> + Send;

    /// Library revision for getIndexes freshness.
    fn get_library_revision(&self) -> impl Future<Output = Result<i64, Self::Error>> + Send;

    /// Artist plus its albums, or None.
    fn get_artist_with_albums(
        &self,
        artist_mbid: &str,
    ) -> impl Future<Output = Result<Option<(ViewArtist, Vec<ViewAlbum>)>, Self::Error>> + Send;

    /// Album by release-group mbid, or None.
    fn get_album(
        &self,
        rg_mbid: &str,
    ) -> impl Future<Output = Result<Option<ViewAlbum>, Self::Error>> + Send;

    /// Tracks of an album.
    fn get_album_tracks(
        &self,
        rg_mbid: &str,
    ) -> impl Future<Output = Result<Vec<ViewTrack>, Self::Error>> + Send;

    /// Track by file id, or None.
    fn get_track(
        &self,
        file_id: &str,
    ) -> impl Future<Output = Result<Option<ViewTrack>, Self::Error>> + Send;

    /// Tracks by file ids (missing ids absent from the map).
    fn get_tracks_by_file_ids(
        &self,
        file_ids: &[String],
    ) -> impl Future<Output = Result<HashMap<String, ViewTrack>, Self::Error>> + Send;

    /// Tracks page, optional match query (None = match-all).
    fn get_tracks_page(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
    ) -> impl Future<Output = Result<(Vec<ViewTrack>, usize), Self::Error>> + Send;

    /// Albums page with sort, year bounds, genre, and match query.
    #[allow(clippy::too_many_arguments)]
    fn get_albums_offset(
        &self,
        limit: usize,
        offset: usize,
        sort: AlbumSort,
        from_year: Option<i64>,
        to_year: Option<i64>,
        genre: Option<&str>,
        query: Option<&str>,
    ) -> impl Future<Output = Result<(Vec<ViewAlbum>, usize), Self::Error>> + Send;

    /// Random songs with optional filters.
    fn get_random_songs(
        &self,
        count: usize,
        genre: Option<&str>,
        from_year: Option<i64>,
        to_year: Option<i64>,
    ) -> impl Future<Output = Result<Vec<ViewTrack>, Self::Error>> + Send;

    /// Recent (`frequent=false`) or frequent history albums.
    fn get_history_albums(
        &self,
        user_id: &str,
        frequent: bool,
        limit: usize,
        offset: usize,
    ) -> impl Future<Output = Result<Vec<ViewAlbum>, Self::Error>> + Send;

    /// Starred albums page.
    fn get_starred_albums(
        &self,
        user_id: &str,
        limit: usize,
        offset: usize,
    ) -> impl Future<Output = Result<Vec<ViewAlbum>, Self::Error>> + Send;

    /// All genres.
    fn get_genres(&self) -> impl Future<Output = Result<Vec<ViewGenre>, Self::Error>> + Send;

    /// Songs of a genre page.
    fn get_songs_by_genre(
        &self,
        genre: &str,
        limit: usize,
        offset: usize,
    ) -> impl Future<Output = Result<Vec<ViewTrack>, Self::Error>> + Send;

    /// Subset of (kind, internal id) targets missing from the library.
    fn missing_targets(
        &self,
        targets: &[(IdKind, String)],
    ) -> impl Future<Output = Result<Vec<(IdKind, String)>, Self::Error>> + Send;

    /// Top songs for an exact artist name.
    fn get_top_songs(
        &self,
        artist: &str,
        user_id: &str,
        count: usize,
    ) -> impl Future<Output = Result<Vec<ViewTrack>, Self::Error>> + Send;

    /// Similar songs for an artist mbid.
    fn get_similar_songs(
        &self,
        artist_mbid: &str,
        user_id: &str,
        count: usize,
    ) -> impl Future<Output = Result<Vec<ViewTrack>, Self::Error>> + Send;

    /// Playlists visible to the user (redacted rows carry None).
    fn get_all_playlists(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Vec<PlaylistSummary>, Self::Error>> + Send;

    /// Streamable (count, duration) per playlist id (#181).
    fn get_streamable_counts(
        &self,
    ) -> impl Future<Output = Result<HashMap<String, (i64, i64)>, Self::Error>> + Send;

    /// Playlist detail, or None.
    fn get_playlist_with_tracks(
        &self,
        playlist_id: &str,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<PlaylistDetail>, Self::Error>> + Send;

    /// Playlist entries in order.
    fn get_playlist_tracks(
        &self,
        playlist_id: &str,
    ) -> impl Future<Output = Result<Vec<PlaylistEntry>, Self::Error>> + Send;

    /// Create a playlist.
    fn create_playlist(
        &self,
        name: &str,
        user_id: &str,
    ) -> impl Future<Output = Result<PlaylistRecord, Self::Error>> + Send;

    /// Rename a playlist.
    fn update_playlist(
        &self,
        playlist_id: &str,
        user_id: &str,
        name: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Flip playlist visibility.
    fn set_playlist_public(
        &self,
        playlist_id: &str,
        user_id: &str,
        public: bool,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Remove entries by entry id.
    fn remove_playlist_tracks(
        &self,
        playlist_id: &str,
        user_id: &str,
        entry_ids: &[String],
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Append a library file to a playlist.
    fn add_playlist_file(
        &self,
        playlist_id: &str,
        file_id: &str,
        user_id: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Delete a playlist.
    fn delete_playlist(
        &self,
        playlist_id: &str,
        user_id: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Playlist cover bytes + content type, or None.
    fn playlist_cover(
        &self,
        playlist_id: &str,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<(Vec<u8>, String)>, Self::Error>> + Send;

    /// Star or unstar targets.
    fn apply_favorites(
        &self,
        user_id: &str,
        targets: &[(IdKind, String)],
        add: bool,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Starred (internal id, starred unix seconds) for a kind.
    fn list_favorites(
        &self,
        user_id: &str,
        kind: IdKind,
    ) -> impl Future<Output = Result<Vec<(String, i64)>, Self::Error>> + Send;

    /// Record a scrobble.
    fn scrobble(
        &self,
        file_id: &str,
        user_id: &str,
        client: Option<&str>,
        played_at: Option<f64>,
        user_name: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Record now-playing presence.
    fn now_playing(
        &self,
        file_id: &str,
        user_id: &str,
        client: Option<&str>,
        user_name: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Current presence rows.
    fn compat_now_playing(
        &self,
    ) -> impl Future<Output = Result<Vec<NowPlayingRow>, Self::Error>> + Send;

    /// Record a playback report (extension playbackReport:1).
    #[allow(clippy::too_many_arguments)]
    fn report_playback(
        &self,
        file_id: &str,
        user_id: &str,
        user_name: &str,
        client: &str,
        position_ms: i64,
        state: &str,
        ignore_scrobble: bool,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Saved play queue.
    fn get_play_queue(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<QueueState, Self::Error>> + Send;

    /// Replace the saved play queue.
    fn replace_play_queue(
        &self,
        user_id: &str,
        file_ids: &[String],
        current_index: Option<usize>,
        position_ms: i64,
        changed_by_client: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Bookmarks for the user.
    fn list_bookmarks(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Vec<BookmarkRecord>, Self::Error>> + Send;

    /// Create or update a bookmark.
    fn upsert_bookmark(
        &self,
        user_id: &str,
        file_id: &str,
        position_ms: i64,
        comment: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Delete a bookmark.
    fn delete_bookmark(
        &self,
        user_id: &str,
        file_id: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Lyrics for a track, or None.
    fn get_lyrics(
        &self,
        file_id: &str,
    ) -> impl Future<Output = Result<Option<LyricsData>, Self::Error>> + Send;

    /// Avatar bytes + content type, or None.
    fn resolve_avatar(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<(Vec<u8>, String)>, Self::Error>> + Send;

    /// (scanning, count).
    fn scan_status(&self) -> impl Future<Output = Result<(bool, Option<i64>), Self::Error>> + Send;

    /// Start a scan (admin gate lives in the handler).
    fn start_scan(&self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Release-group cover bytes + content type for a size bucket, or None.
    fn release_group_cover(
        &self,
        rg_mbid: &str,
        bucket: &str,
    ) -> impl Future<Output = Result<Option<(Vec<u8>, String)>, Self::Error>> + Send;

    /// Artist image bytes + content type for a pixel size, or None.
    fn artist_image(
        &self,
        artist_mbid: &str,
        px: Option<i64>,
    ) -> impl Future<Output = Result<Option<(Vec<u8>, String)>, Self::Error>> + Send;

    /// Advanced transcode decision for a client description.
    fn advanced_decide(
        &self,
        track: &ViewTrack,
        client: &ClientInfo,
        user_id: &str,
    ) -> impl Future<Output = Result<TranscodeDecisionData, Self::Error>> + Send;

    /// Decode signed transcode params: (direct, out format, bitrate kbps).
    fn decode_transcode_params(
        &self,
        params: &str,
        user_id: &str,
        file_id: &str,
    ) -> impl Future<Output = Result<(bool, Option<String>, Option<i64>), Self::Error>> + Send;
}
