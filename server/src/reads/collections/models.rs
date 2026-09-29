//! Transport DTOs for the collections slice. Wire format is snake_case.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Playlists.

/// One track inside a playlist.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaylistTrack {
    /// Track row id.
    pub id: String,
    /// Zero-based position in the playlist.
    pub position: usize,
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album name.
    pub album_name: String,
    /// Album id, when the track resolved to the library.
    pub album_id: Option<String>,
    /// Artist id, when resolved.
    pub artist_id: Option<String>,
    /// Library track source id, when resolved.
    pub track_source_id: Option<String>,
    /// Cover art URL, when known.
    pub cover_url: Option<String>,
    /// Preferred source type, empty when unresolved.
    pub source_type: String,
    /// Sources carrying this track, when known.
    pub available_sources: Option<Vec<String>>,
    /// Audio format, when known.
    pub format: Option<String>,
    /// Track number, when known.
    pub track_number: Option<i32>,
    /// Disc number, when known.
    pub disc_number: Option<i32>,
    /// Duration in seconds, when known.
    pub duration: Option<f64>,
    /// Row creation time, epoch seconds.
    pub created_at: u64,
    /// Plex rating key, when the track came from Plex.
    pub plex_rating_key: Option<String>,
    /// Local library file id, when the track resolved locally.
    pub library_file_id: Option<String>,
}

/// Playlist summary for lists and visibility answers.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaylistSummary {
    /// Playlist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Track count.
    pub track_count: usize,
    /// Total duration in seconds, when any track reports one.
    pub total_duration: Option<f64>,
    /// Up to four track covers for the list tile.
    pub cover_urls: Vec<String>,
    /// Custom cover URL, when one was uploaded.
    pub custom_cover_url: Option<String>,
    /// Creation time, epoch seconds.
    pub created_at: u64,
    /// Last mutation time, epoch seconds.
    pub updated_at: u64,
    /// Public playlists are visible to other users.
    pub is_public: bool,
    /// True when the caller owns the playlist.
    pub is_owner: bool,
    /// Owner display name.
    pub owner_name: Option<String>,
    /// Always false on a full summary; true on redacted rows.
    pub is_redacted: bool,
}

/// Full playlist detail: summary fields plus tracks.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaylistDetail {
    /// Playlist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Up to four track covers for the header.
    pub cover_urls: Vec<String>,
    /// Custom cover URL, when one was uploaded.
    pub custom_cover_url: Option<String>,
    /// Tracks in position order.
    pub tracks: Vec<PlaylistTrack>,
    /// Track count.
    pub track_count: usize,
    /// Total duration in seconds, when any track reports one.
    pub total_duration: Option<f64>,
    /// Creation time, epoch seconds.
    pub created_at: u64,
    /// Last mutation time, epoch seconds.
    pub updated_at: u64,
    /// Public playlists are visible to other users.
    pub is_public: bool,
    /// True when the caller owns the playlist.
    pub is_owner: bool,
    /// Owner display name.
    pub owner_name: Option<String>,
    /// Always false on a full detail; true on redacted rows.
    pub is_redacted: bool,
}

/// Another user's private playlist: existence, count, and owner only. Never
/// the name, tracks, or covers.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RedactedPlaylist {
    /// Playlist id.
    pub id: String,
    /// Track count.
    pub track_count: usize,
    /// Owner display name.
    pub owner_name: Option<String>,
    /// Always true.
    pub is_redacted: bool,
}

/// One row of the playlist list: a full summary or a redacted stub.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum PlaylistListItem {
    /// Full summary for owned and public playlists.
    Full(PlaylistSummary),
    /// Redacted stub for other users' private playlists.
    Redacted(RedactedPlaylist),
}

/// Playlist list answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaylistListResponse {
    /// Playlists visible to the caller.
    pub playlists: Vec<PlaylistListItem>,
}

/// Create-playlist body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreatePlaylistBody {
    /// Display name. Must not be blank.
    pub name: String,
}

/// Rename-playlist body. A missing name leaves the playlist unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdatePlaylistBody {
    /// New display name. Must not be blank when present.
    pub name: Option<String>,
}

/// Visibility-toggle body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VisibilityBody {
    /// True makes the playlist public; false makes it private.
    pub is_public: bool,
}

/// One track to add.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackInput {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album name.
    pub album_name: String,
    /// Album id, when known.
    pub album_id: Option<String>,
    /// Artist id, when known.
    pub artist_id: Option<String>,
    /// Library track source id, when known.
    pub track_source_id: Option<String>,
    /// Cover art URL, when known.
    pub cover_url: Option<String>,
    /// Preferred source type.
    #[serde(default)]
    pub source_type: String,
    /// Sources carrying this track, when known.
    pub available_sources: Option<Vec<String>>,
    /// Audio format, when known.
    pub format: Option<String>,
    /// Track number, when known.
    pub track_number: Option<i32>,
    /// Disc number, when known.
    pub disc_number: Option<i32>,
    /// Duration in seconds, when known.
    pub duration: Option<f64>,
    /// Plex rating key, when the track came from Plex.
    pub plex_rating_key: Option<String>,
}

/// Add-tracks body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddTracksBody {
    /// Tracks to append or insert.
    pub tracks: Vec<TrackInput>,
    /// Insert position. Missing appends; past-the-end clamps.
    pub position: Option<usize>,
}

/// Added-tracks answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddTracksResponse {
    /// The added tracks with their ids and positions.
    pub tracks: Vec<PlaylistTrack>,
}

/// Bulk-remove body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RemoveTracksBody {
    /// Track row ids to remove.
    pub track_ids: Vec<String>,
}

/// Bulk-remove answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RemoveTracksResponse {
    /// Always "ok".
    pub status: String,
    /// Human message.
    pub message: String,
    /// How many rows were removed.
    pub removed: usize,
}

/// Reorder body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReorderBody {
    /// Track row id to move.
    pub track_id: String,
    /// Desired position. Past-the-end clamps to the last slot.
    pub new_position: usize,
}

/// Reorder answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReorderResponse {
    /// Always "ok".
    pub status: String,
    /// Human message.
    pub message: String,
    /// Where the track actually landed.
    pub actual_position: usize,
}

/// Track source-update body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateTrackBody {
    /// Preferred source type.
    pub source_type: Option<String>,
    /// Sources carrying this track.
    pub available_sources: Option<Vec<String>>,
}

/// Track identity for membership checks.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackIdentifier {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album name.
    pub album_name: String,
}

/// Membership-check body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckTracksBody {
    /// Tracks to look up.
    pub tracks: Vec<TrackIdentifier>,
}

/// Membership-check answer: each request index maps to the ids of the
/// caller's visible playlists holding that track.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckTracksResponse {
    /// Request index (as a string) to playlist ids.
    pub membership: HashMap<String, Vec<String>>,
}

/// Source-resolution answer: each track id maps to its known sources.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveSourcesResponse {
    /// Track id to sources.
    pub sources: HashMap<String, Vec<String>>,
}

/// Cover-upload body. Base64 JSON instead of multipart: the app has no
/// multipart feature and the frontend regenerates against this contract.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CoverUploadBody {
    /// Base64-encoded image bytes (png, jpeg, or webp, max 5 MiB decoded).
    pub image_base64: String,
    /// Mime type of the image.
    pub content_type: String,
}

/// Cover-upload answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CoverUploadResponse {
    /// URL serving the uploaded cover.
    pub cover_url: String,
}

/// Generic status answer for deletes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusResponse {
    /// Always "ok".
    pub status: String,
    /// Human message.
    pub message: String,
}

// Favorites.

/// One favorite row.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FavoriteItem {
    /// Favorited kind: album, artist, or track.
    pub kind: String,
    /// Favorited item id.
    pub item_id: String,
    /// Display name, when the caller sent one.
    pub name: Option<String>,
    /// When it was favorited, epoch seconds.
    pub favorited_at: u64,
}

/// Per-kind counts.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FavoriteCounts {
    /// Favorited albums.
    pub album: usize,
    /// Favorited artists.
    pub artist: usize,
    /// Favorited tracks.
    pub track: usize,
}

/// Favorite-list answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FavoriteListResponse {
    /// Favorites, optionally filtered by kind.
    pub items: Vec<FavoriteItem>,
    /// Counts across all kinds, ignoring the filter.
    pub counts: FavoriteCounts,
}

/// Favorite query: optional kind filter.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FavoriteQuery {
    /// Kind filter: album, artist, or track.
    pub kind: Option<String>,
}

/// Set-favorite body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetFavoriteBody {
    /// True favorites the item; false unfavorites it.
    pub favorited: bool,
    /// Display name to keep, when favoriting.
    pub name: Option<String>,
}

/// Favorite-status answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FavoriteStatusResponse {
    /// Favorited kind.
    pub kind: String,
    /// Favorited item id.
    pub item_id: String,
    /// Current state.
    pub favorited: bool,
}

// Follows.

/// Follow-status answer for one artist.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FollowStatusResponse {
    /// Artist MBID.
    pub artist_mbid: String,
    /// Whether the caller follows the artist.
    pub followed: bool,
    /// Whether auto-download is wanted.
    pub auto_download: bool,
    /// Approval state: off, pending, or active.
    pub auto_download_state: String,
}

/// Follow-toggle body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FollowBody {
    /// True follows the artist; false unfollows (and drops auto-download).
    pub followed: bool,
    /// Artist name to keep, when following.
    pub artist_name: Option<String>,
}

/// Auto-download-toggle body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AutoDownloadBody {
    /// True requests auto-download; false turns it off.
    pub enabled: bool,
}

/// One followed artist.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FollowedArtist {
    /// Artist MBID.
    pub artist_mbid: String,
    /// Artist name as known when followed.
    pub name: String,
    /// Whether auto-download is wanted.
    pub auto_download: bool,
    /// Approval state: off, pending, or active.
    pub auto_download_state: String,
    /// When the follow started, epoch seconds.
    pub followed_at: u64,
}

/// Followed-artist list answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FollowedArtistListResponse {
    /// Followed artists.
    pub artists: Vec<FollowedArtist>,
}

/// One new-release sighting.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NewReleaseItem {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Release title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// Artist MBID.
    pub artist_mbid: String,
    /// Release-group primary type, when known.
    pub primary_type: Option<String>,
    /// First release date, when known.
    pub first_release_date: Option<String>,
}

/// New-release list answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NewReleaseListResponse {
    /// Sightings for followed artists, newest first.
    pub items: Vec<NewReleaseItem>,
    /// Item count.
    pub total: usize,
}

/// Unseen-count answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UnseenCountResponse {
    /// Sightings newer than the caller's seen watermark.
    pub count: usize,
}

// Approvals (read paths only; approve/reject/revoke land with acquisition).

/// One pending auto-download request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AutoDownloadApprovalItem {
    /// Requesting user id.
    pub user_id: String,
    /// Requesting user display name.
    pub user_name: Option<String>,
    /// Artist MBID.
    pub artist_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// When it was requested, epoch seconds.
    pub requested_at: u64,
}

/// Pending-approval list answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AutoDownloadApprovalListResponse {
    /// Pending requests.
    pub items: Vec<AutoDownloadApprovalItem>,
    /// Item count.
    pub count: usize,
}

/// One grouped approval card: a user's pending requests as a batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApprovalBatchItem {
    /// Batch id, stable per user while requests stay pending.
    pub batch_id: String,
    /// Requesting user id.
    pub user_id: String,
    /// Requesting user display name.
    pub user_name: Option<String>,
    /// Pending artist count.
    pub artist_count: usize,
    /// First few artist names for the card preview.
    pub sample_names: Vec<String>,
    /// Oldest request time, epoch seconds.
    pub requested_at: u64,
    /// What produced the requests.
    pub source: String,
}

/// Approval-batch list answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApprovalBatchListResponse {
    /// Batches.
    pub batches: Vec<ApprovalBatchItem>,
    /// Batch count.
    pub count: usize,
}

// Edition pins (display lane only).

/// Edition-pin display answer. `selected_release_mbid` is the soft display
/// hint: the pin when set, else the catalog default. It never becomes
/// catalog identity.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EditionPinResponse {
    /// Album id.
    pub album_id: String,
    /// Pinned release MBID, when pinned.
    pub pinned_release_mbid: Option<String>,
    /// Display pick: the pin when set, else the catalog default.
    pub selected_release_mbid: Option<String>,
    /// Where the pick came from: pin, default, or none.
    pub hint_source: String,
}

/// Pin-set body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EditionPinBody {
    /// Release MBID to pin. Must be a known edition of the album.
    pub release_mbid: String,
}
