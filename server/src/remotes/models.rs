//! Unified wire shapes for remote-source browse.
//!
//! One shape per concept across Jellyfin, Navidrome, and Plex: the adapters
//! translate source payloads into these, and handlers render them as-is.
//! All snake_case, all in the OpenAPI doc. Remote ids stay opaque strings;
//! every view carries the `source` that owns it so clients can round-trip.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Which remote source a view or id belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SourceName {
    /// Jellyfin media server.
    Jellyfin,
    /// Navidrome (Subsonic/OpenSubsonic API).
    Navidrome,
    /// Plex media server.
    Plex,
}

impl SourceName {
    /// Parse a `{source}` path segment. Unknown names are user input errors.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "jellyfin" => Some(Self::Jellyfin),
            "navidrome" => Some(Self::Navidrome),
            "plex" => Some(Self::Plex),
            _ => None,
        }
    }

    /// Lowercase wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jellyfin => "jellyfin",
            Self::Navidrome => "navidrome",
            Self::Plex => "plex",
        }
    }

    /// Display name for user-facing messages.
    pub fn display(self) -> &'static str {
        match self {
            Self::Jellyfin => "Jellyfin",
            Self::Navidrome => "Navidrome",
            Self::Plex => "Plex",
        }
    }
}

/// One remote album in list and detail views.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesAlbumView)]
pub struct AlbumView {
    /// Owning source.
    pub source: SourceName,
    /// Opaque remote id (Jellyfin item id, Subsonic id, Plex rating key).
    pub id: String,
    /// Album title.
    pub title: String,
    /// Album artist display name.
    pub artist_name: String,
    /// Opaque remote artist id, when the source links one.
    pub artist_id: Option<String>,
    /// Release year, when known.
    pub year: Option<i32>,
    /// Genre label, when the source reports one.
    pub genre: Option<String>,
    /// Track count, when the source reports one.
    pub track_count: Option<i32>,
    /// Linked MusicBrainz release id.
    pub release_mbid: Option<String>,
    /// Linked MusicBrainz release-group id.
    pub release_group_mbid: Option<String>,
    /// Linked MusicBrainz artist id.
    pub artist_mbid: Option<String>,
    /// Relative images URL under `/api/v3`, when art exists.
    pub image_url: Option<String>,
}

/// One remote artist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesArtistView)]
pub struct ArtistView {
    /// Owning source.
    pub source: SourceName,
    /// Opaque remote id.
    pub id: String,
    /// Artist name.
    pub name: String,
    /// Album count, when the source reports one.
    pub album_count: Option<i32>,
    /// Linked MusicBrainz artist id.
    pub artist_mbid: Option<String>,
    /// Relative images URL under `/api/v3`, when art exists.
    pub image_url: Option<String>,
}

/// One remote track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesTrackView)]
pub struct TrackView {
    /// Owning source.
    pub source: SourceName,
    /// Opaque remote id.
    pub id: String,
    /// Track title.
    pub title: String,
    /// Album title.
    pub album_name: String,
    /// Opaque remote album id, when the source links one.
    pub album_id: Option<String>,
    /// Track artist display name.
    pub artist_name: String,
    /// Opaque remote artist id, when the source links one.
    pub artist_id: Option<String>,
    /// Track number within the disc.
    pub track_number: Option<i32>,
    /// Disc number.
    pub disc_number: Option<i32>,
    /// Duration in seconds.
    pub duration_secs: Option<i64>,
    /// Release year, when known.
    pub year: Option<i32>,
    /// Linked MusicBrainz recording id.
    pub recording_mbid: Option<String>,
    /// Relative images URL under `/api/v3`, when art exists.
    pub image_url: Option<String>,
    /// Plex part key for the stream gateway. Plex-only: Jellyfin and
    /// Navidrome stream by item id, so they always leave this empty.
    pub part_key: Option<String>,
}

/// One page of albums.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesAlbumPage)]
pub struct AlbumPage {
    /// Page items.
    pub items: Vec<AlbumView>,
    /// Total matching records upstream.
    pub total: i64,
    /// Echoed offset.
    pub offset: i64,
    /// Echoed limit.
    pub limit: i64,
}

/// One page of artists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesArtistPage)]
pub struct ArtistPage {
    /// Page items.
    pub items: Vec<ArtistView>,
    /// Total matching records upstream.
    pub total: i64,
    /// Echoed offset.
    pub offset: i64,
    /// Echoed limit.
    pub limit: i64,
}

/// One page of tracks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesTrackPage)]
pub struct TrackPage {
    /// Page items.
    pub items: Vec<TrackView>,
    /// Total matching records upstream.
    pub total: i64,
    /// Echoed offset.
    pub offset: i64,
    /// Echoed limit.
    pub limit: i64,
}

/// One alphabetic bucket of the artist index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ArtistIndexEntry {
    /// Bucket label, e.g. "A".
    pub name: String,
    /// Artists in this bucket.
    pub artists: Vec<ArtistView>,
}

/// Full artist index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ArtistIndex {
    /// Alphabetic buckets.
    pub index: Vec<ArtistIndexEntry>,
}

/// Unified search results across the three buckets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesSearchResults)]
pub struct SearchResults {
    /// Matching artists.
    pub artists: Vec<ArtistView>,
    /// Matching albums.
    pub albums: Vec<AlbumView>,
    /// Matching tracks.
    pub tracks: Vec<TrackView>,
}

/// Library totals for one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesStatsView)]
pub struct StatsView {
    /// Album count.
    pub total_albums: i64,
    /// Artist count.
    pub total_artists: i64,
    /// Track count.
    pub total_tracks: i64,
}

/// The hub: one screen of highlights per source. Sections a source lacks
/// stay empty rather than erroring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HubView {
    /// Owning source.
    pub source: SourceName,
    /// Library totals, when the source can count cheaply.
    pub stats: Option<StatsView>,
    /// Recently played albums.
    pub recently_played: Vec<AlbumView>,
    /// Recently added albums.
    pub recently_added: Vec<AlbumView>,
    /// Favorite albums.
    pub favorites: Vec<AlbumView>,
    /// Favorite artists (sources with artist favorites).
    pub favorite_artists: Vec<ArtistView>,
    /// Most-played artists (sources reporting play counts).
    pub most_played_artists: Vec<ArtistView>,
    /// Preview of the full album catalog.
    pub all_albums_preview: Vec<AlbumView>,
    /// Genre labels.
    pub genres: Vec<String>,
}

/// One Plex discovery shelf: a hub title over its album rows. Plex-only
/// (from `/hubs/sections/{id}`), kept out of the unified [`HubView`] so
/// other sources never ship an empty shelf shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryHubView {
    /// Hub title, e.g. "Recommended for you".
    pub title: String,
    /// Hub kind; always `"album"` (other hub kinds are dropped, as in v2).
    pub hub_type: String,
    /// Albums on this shelf.
    pub albums: Vec<AlbumView>,
}

/// Plex discovery shelves for the hub page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryView {
    /// Owning source.
    pub source: SourceName,
    /// Album shelves, in upstream order.
    pub hubs: Vec<DiscoveryHubView>,
}

/// Favorites grouped by kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FavoritesView {
    /// Favorite artists.
    pub artists: Vec<ArtistView>,
    /// Favorite albums.
    pub albums: Vec<AlbumView>,
    /// Favorite tracks.
    pub tracks: Vec<TrackView>,
}

/// One remote playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesPlaylistSummary)]
pub struct PlaylistSummary {
    /// Owning source.
    pub source: SourceName,
    /// Opaque remote id.
    pub id: String,
    /// Playlist name.
    pub name: String,
    /// Track count.
    pub track_count: i64,
    /// Total duration in seconds.
    pub duration_secs: i64,
    /// Relative covers URL under `/api/v3`, when art exists.
    pub image_url: Option<String>,
}

/// Playlist list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PlaylistCollection {
    /// Playlists.
    pub items: Vec<PlaylistSummary>,
}

/// Playlist with its tracks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesPlaylistDetail)]
pub struct PlaylistDetail {
    /// Playlist header.
    pub playlist: PlaylistSummary,
    /// Playlist tracks in order.
    pub tracks: Vec<TrackView>,
}

/// Receipt for a playlist import into the local catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportResult {
    /// Local playlist id that now holds the tracks.
    pub local_playlist_id: String,
    /// Tracks imported.
    pub tracks_imported: i64,
    /// Tracks that failed to resolve.
    pub tracks_failed: i64,
    /// True when this exact playlist was already imported.
    pub already_imported: bool,
}

/// Unified artist/album info passthrough (Last.fm-sourced upstream).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InfoView {
    /// Owning source.
    pub source: SourceName,
    /// Opaque remote id the info belongs to.
    pub id: String,
    /// Biography or release notes.
    pub biography: String,
    /// Linked MusicBrainz id.
    pub musicbrainz_id: String,
    /// Best upstream image URL, when offered.
    pub image_url: String,
    /// Similar artists (artist info only).
    pub similar_artists: Vec<ArtistView>,
}

/// One lyric line, with an optional sync offset in milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesLyricLine)]
pub struct LyricLine {
    /// Line text.
    pub text: String,
    /// Start offset in milliseconds, when synced.
    pub start_ms: Option<i64>,
}

/// Unified lyrics passthrough.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesLyricsView)]
pub struct LyricsView {
    /// Owning source.
    pub source: SourceName,
    /// Full text.
    pub text: String,
    /// True when line timings are present.
    pub is_synced: bool,
    /// Timed lines, when the source provides them.
    pub lines: Vec<LyricLine>,
}

/// One active remote listening session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = RemotesSessionView)]
pub struct SessionView {
    /// Owning source.
    pub source: SourceName,
    /// Remote session id.
    pub session_id: String,
    /// Listening user.
    pub user_name: String,
    /// Player or device label.
    pub device_name: String,
    /// Track title.
    pub track_title: String,
    /// Track artist.
    pub artist_name: String,
    /// Track album.
    pub album_name: String,
    /// Playback position in milliseconds.
    pub progress_ms: i64,
    /// Track length in milliseconds.
    pub duration_ms: i64,
    /// True when paused.
    pub is_paused: bool,
}

/// Active sessions for one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionsView {
    /// Owning source.
    pub source: SourceName,
    /// Active audio sessions.
    pub sessions: Vec<SessionView>,
}

/// One listening-history entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HistoryEntry {
    /// Opaque remote track id.
    pub id: String,
    /// Track title.
    pub track_title: String,
    /// Track artist.
    pub artist_name: String,
    /// Track album.
    pub album_name: String,
    /// Unix time the play ended.
    pub viewed_at: i64,
    /// Track length in milliseconds.
    pub duration_ms: i64,
}

/// Listening history page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HistoryPage {
    /// Owning source.
    pub source: SourceName,
    /// Page items, newest first.
    pub items: Vec<HistoryEntry>,
    /// Total matching records upstream.
    pub total: i64,
}

/// MBID match result: the remote album behind a MusicBrainz id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MatchView {
    /// Owning source.
    pub source: SourceName,
    /// True when the MBID resolved to a remote album.
    pub found: bool,
    /// Opaque remote album id, when found.
    pub remote_album_id: Option<String>,
    /// Album tracks, when found.
    pub tracks: Vec<TrackView>,
}

/// Connection status for one source. Never carries credential material.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConnectionStatus {
    /// Owning source.
    pub source: SourceName,
    /// True when a credential is stored and the source is enabled.
    pub connected: bool,
    /// "linked" (own credential) or "shared" (admin credential).
    pub account_mode: String,
    /// Display label, e.g. the login name.
    pub account_label: String,
}

/// Link the caller's own Navidrome or Jellyfin account on the server the
/// admin configured. The password is checked live, then kept sealed
/// (Navidrome) or traded for a user token and dropped (Jellyfin). It is
/// never echoed back.
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConnectionSave {
    /// Login name on the server.
    pub username: String,
    /// Password on the server.
    pub password: String,
}

/// Manual `Debug` that redacts the password.
impl std::fmt::Debug for ConnectionSave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionSave")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// One account the caller linked, on any service. Never carries the
/// stored secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LinkedAccount {
    /// Service tag (`navidrome`, `jellyfin`, `plex`, `listenbrainz`,
    /// `lastfm`, `spotify`).
    pub service: String,
    /// Whether the link is enabled.
    pub enabled: bool,
    /// Linked account name, empty when the service stores none.
    pub username: String,
}

/// The caller's linked accounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LinkedAccounts {
    /// One entry per linked service.
    pub connections: Vec<LinkedAccount>,
}

/// Jellyfin filter facets for the album browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FilterFacetsView {
    /// Release years, newest first.
    pub years: Vec<i32>,
    /// Tags, sorted.
    pub tags: Vec<String>,
    /// Studios (labels), sorted.
    pub studios: Vec<String>,
}

/// One row of a listening top list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsItem {
    /// Artist, album, or track name.
    pub name: String,
    /// Artist name for album and track rows, empty for artists.
    pub subtitle: String,
    /// Plays counted.
    pub play_count: i64,
}

/// Listening analytics over the server's history (Plex).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsView {
    /// Most-played artists.
    pub top_artists: Vec<AnalyticsItem>,
    /// Most-played albums.
    pub top_albums: Vec<AnalyticsItem>,
    /// Most-played tracks.
    pub top_tracks: Vec<AnalyticsItem>,
    /// History entries counted.
    pub total_listens: i64,
    /// Plays in the last 7 days.
    pub listens_last_7_days: i64,
    /// Plays in the last 30 days.
    pub listens_last_30_days: i64,
    /// Listening time in hours, one decimal.
    pub total_hours: f64,
    /// False when the history was longer than the analysis window.
    pub is_complete: bool,
    /// Entries read.
    pub entries_analyzed: i64,
}

/// One Navidrome music folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MusicFolderView {
    /// Folder id.
    pub id: String,
    /// Folder name.
    pub name: String,
}

/// Navidrome folder preference resolution for the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FolderResolutionView {
    /// "all" or "selected".
    pub mode: String,
    /// Effective folder ids ("all" means upstream default scoping).
    pub folder_ids: Vec<String>,
    /// Folders the server currently exposes.
    pub available_folders: Vec<MusicFolderView>,
    /// Selected ids the server no longer exposes.
    pub stale_folder_ids: Vec<String>,
    /// False when the source is down; the scope still echoes the preference.
    pub source_available: bool,
}

/// Save payload for the Navidrome folder preference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FolderSave {
    /// "all" or "selected".
    pub mode: String,
    /// Selected folder ids. Must be empty for "all", non-empty for "selected".
    pub selected_folder_ids: Vec<String>,
}

/// Shared pagination query: `limit`/`offset` with sane clamps.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PageQuery {
    /// Max items (1-500, default 50).
    pub limit: Option<i64>,
    /// Records to skip (default 0).
    pub offset: Option<i64>,
}

impl PageQuery {
    /// Clamp to the adapter range.
    pub fn clamped(&self) -> (i64, i64) {
        let limit = self.limit.unwrap_or(50).clamp(1, 500);
        let offset = self.offset.unwrap_or(0).max(0);
        (limit, offset)
    }
}

/// Album browse filters shared by every source.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AlbumBrowseQuery {
    /// Max items (1-500, default 50).
    pub limit: Option<i64>,
    /// Records to skip (default 0).
    pub offset: Option<i64>,
    /// Sort field (source-native name, default SortName/titleSort).
    pub sort_by: Option<String>,
    /// "asc" or "desc" (default asc).
    pub sort_order: Option<String>,
    /// Genre filter.
    pub genre: Option<String>,
    /// Exact year filter.
    pub year: Option<i32>,
    /// Decade filter in `"2020s"` spelling (Plex honors it, others ignore it).
    pub decade: Option<String>,
}

/// Artist browse filters shared by every source.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ArtistBrowseQuery {
    /// Max items (1-500, default 50).
    pub limit: Option<i64>,
    /// Records to skip (default 0).
    pub offset: Option<i64>,
    /// Sort field (source-native name).
    pub sort_by: Option<String>,
    /// "asc" or "desc" (default asc).
    pub sort_order: Option<String>,
    /// Name filter.
    pub search: Option<String>,
}

/// Track browse filters shared by every source.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TrackBrowseQuery {
    /// Max items (1-500, default 50).
    pub limit: Option<i64>,
    /// Records to skip (default 0).
    pub offset: Option<i64>,
    /// Sort field (source-native name).
    pub sort_by: Option<String>,
    /// "asc" or "desc" (default asc).
    pub sort_order: Option<String>,
    /// Title filter.
    pub search: Option<String>,
    /// Genre filter.
    pub genre: Option<String>,
}

/// Unified search query.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    /// Query text.
    pub q: String,
    /// Per-bucket cap (1-100, default 20).
    pub limit: Option<i64>,
}

/// Genre songs query.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GenreSongsQuery {
    /// Genre label.
    pub genre: String,
    /// Max items (1-500, default 50).
    pub limit: Option<i64>,
    /// Records to skip (default 0).
    pub offset: Option<i64>,
}

/// Random-tracks query. Limits mirror the v2 Navidrome route
/// (`size` 1-50, default 20) so the hub page keeps its batch size.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RandomQuery {
    /// Max tracks (1-50, default 20).
    pub limit: Option<i64>,
    /// Genre label filter, when present.
    pub genre: Option<String>,
}

/// Plex discovery-hubs query. The count mirrors the v2 Plex route
/// (1-20, default 10).
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DiscoveryQuery {
    /// Items per hub (1-20, default 10).
    pub count: Option<i64>,
}

/// Image size query for images/covers bytes routes.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImageQuery {
    /// Square edge in px (1-2000, default 500).
    pub size: Option<i64>,
}

impl ImageQuery {
    /// Clamp to the adapter range.
    pub fn clamped(&self) -> i64 {
        self.size.unwrap_or(500).clamp(1, 2000)
    }
}

/// MBID match query.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MatchQuery {
    /// MusicBrainz release or release-group id.
    pub mbid: String,
}

/// Lyrics query: plain id lookup, with artist/title fallback for Navidrome.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LyricsQuery {
    /// Artist name fallback (Navidrome classic lyrics).
    pub artist: Option<String>,
    /// Title fallback (Navidrome classic lyrics).
    pub title: Option<String>,
}

/// History pagination query.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryQuery {
    /// Max items (1-200, default 50).
    pub limit: Option<i64>,
    /// Records to skip (default 0).
    pub offset: Option<i64>,
}

impl HistoryQuery {
    /// Clamp to the adapter range.
    pub fn clamped(&self) -> (i64, i64) {
        let limit = self.limit.unwrap_or(50).clamp(1, 200);
        let offset = self.offset.unwrap_or(0).max(0);
        (limit, offset)
    }
}

/// Favorites query.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FavoritesQuery {
    /// Max items per kind (1-100, default 50).
    pub limit: Option<i64>,
}

impl FavoritesQuery {
    /// Clamp to the v2 range.
    pub fn clamped(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 100)
    }
}

/// Most-played query.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MostPlayedQuery {
    /// Max items (1-50, default 10).
    pub limit: Option<i64>,
}

impl MostPlayedQuery {
    /// Clamp to the v2 range.
    pub fn clamped(&self) -> i64 {
        self.limit.unwrap_or(10).clamp(1, 50)
    }
}

/// Instant-mix seed query.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MixQuery {
    /// Seed kind: `item` (default), `artist`, or `genre`.
    pub kind: Option<String>,
    /// Max tracks (1-200, default 50).
    pub limit: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::ConnectionSave;

    /// The manual `Debug` must never print the password. A regression
    /// here would leak secrets into logs through one `{:?}`.
    #[test]
    fn connection_save_debug_redacts_the_password() {
        let save = ConnectionSave {
            username: "ada".to_owned(),
            password: "super-secret".to_owned(),
        };
        let rendered = format!("{save:?}");
        assert!(!rendered.contains("super-secret"));
        assert!(rendered.contains("ada"));
    }
}
