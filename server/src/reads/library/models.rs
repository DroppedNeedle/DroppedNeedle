//! Wire shapes for the library reads. All snake_case, all generated into
//! the OpenAPI doc; the frontend consumes the generated client, never copies.
//!
//! Counts are streamable-only throughout: tracks whose file is missing or
//! excluded never inflate album, artist, genre, or library totals. Favorite
//! flags and counts reflect the caller only.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// One catalog album.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlbumView {
    /// Local album id.
    pub id: String,
    /// Album title.
    pub title: String,
    /// Album artist display name.
    pub artist_name: String,
    /// Local album-artist id.
    pub artist_id: String,
    /// Linked MusicBrainz release group, when identified.
    pub release_group_mbid: Option<String>,
    /// Linked MusicBrainz release, when an exact release is known.
    pub release_mbid: Option<String>,
    /// Linked MusicBrainz artist id, when the artist is identified.
    pub artist_mbid: Option<String>,
    /// `local_only` or `linked`.
    pub identity_state: String,
    /// Streamable tracks on this album.
    pub track_count: u64,
    /// Summed streamable-track duration, seconds.
    pub total_duration_seconds: f64,
    /// Summed streamable-track size, bytes.
    pub total_size_bytes: i64,
    /// Most common track format, when any track exists.
    pub format: Option<String>,
    /// Release year, when known.
    pub year: Option<i64>,
    /// True for compilations and various-artists sets.
    pub is_compilation: bool,
    /// True when an artwork row exists for this album.
    pub cover_available: bool,
    /// Import time, unix seconds.
    pub date_added: Option<f64>,
    /// True when the caller favorited this album.
    pub favorite: bool,
    /// The album's open MusicBrainz contribution, if any.
    pub contribution_id: Option<String>,
    /// That contribution's state: `draft`, `ready`, `seeded`, `verifying`
    /// or `needs_review`.
    pub contribution_state: Option<String>,
}

/// One page of catalog albums.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlbumPage {
    /// This page of albums.
    pub items: Vec<AlbumView>,
    /// Albums matching the filter, all pages.
    pub total: u64,
    /// Echo of the requested offset.
    pub offset: u64,
    /// Echo of the requested limit.
    pub limit: u64,
}

/// Album list query. Unknown `sort` values fail with INVALID_INPUT.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AlbumQuery {
    /// Page size, 1-200. Default 50.
    pub limit: Option<i64>,
    /// Page start. Default 0.
    pub offset: Option<i64>,
    /// `name`, `date_added`, `year`, or `artist`. Default `name`.
    pub sort: Option<String>,
    /// `asc` or `desc`. Default `asc`.
    pub order: Option<String>,
    /// Title/artist substring filter, case folded.
    pub q: Option<String>,
    /// Restrict to one album artist.
    pub artist_id: Option<String>,
    /// Restrict to one decade start year, e.g. 1990.
    pub decade: Option<i64>,
    /// Restrict to one primary track format, e.g. `flac`. Blank reads as
    /// absent; matching is case-insensitive.
    pub format: Option<String>,
}

/// Track list query for album and genre scopes.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PageQuery {
    /// Page size, 1-1000. Default 200.
    pub limit: Option<i64>,
    /// Page start. Default 0.
    pub offset: Option<i64>,
}

/// One catalog artist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ArtistView {
    /// Local artist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Linked MusicBrainz artist id, when identified.
    pub artist_mbid: Option<String>,
    /// `local_only` or `linked`.
    pub identity_state: String,
    /// Albums where this artist is the album artist.
    pub album_count: u64,
    /// Streamable tracks credited to this artist.
    pub track_count: u64,
    /// Albums where this artist appears without leading.
    pub appearance_album_count: u64,
    /// First import time, unix seconds.
    pub date_added: Option<f64>,
    /// True when the caller favorited this artist.
    pub favorite: bool,
}

/// One page of catalog artists, with scope totals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ArtistPage {
    /// This page of artists.
    pub items: Vec<ArtistView>,
    /// Artists matching the scope and filter, all pages.
    pub total: u64,
    /// Album artists in the catalog (ignores paging, honors `q`).
    pub album_artist_total: u64,
    /// Contributors in the catalog (ignores paging, honors `q`).
    pub contributor_total: u64,
    /// Echo of the requested offset.
    pub offset: u64,
    /// Echo of the requested limit.
    pub limit: u64,
}

/// Artist list query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ArtistQuery {
    /// Page size, 1-200. Default 50.
    pub limit: Option<i64>,
    /// Page start. Default 0.
    pub offset: Option<i64>,
    /// `name`, `album_count`, or `date_added`. Default `name`.
    pub sort: Option<String>,
    /// `asc` or `desc`. Default `asc`.
    pub order: Option<String>,
    /// Name substring filter, case folded.
    pub q: Option<String>,
    /// `all`, `album_artists`, or `contributors`. Default `all`.
    pub scope: Option<String>,
}

/// One catalog track. Only streamable tracks are listed; missing or
/// excluded files read as absent (404 on the detail route).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TrackView {
    /// Local track id.
    pub id: String,
    /// Track title.
    pub title: String,
    /// Owning local album id.
    pub album_id: String,
    /// Album title.
    pub album_title: String,
    /// Track artist display name.
    pub artist_name: String,
    /// Local track-artist id, when credited.
    pub artist_id: Option<String>,
    /// Album artist display name.
    pub album_artist_name: String,
    /// Disc number.
    pub disc_number: i64,
    /// Track number within the disc.
    pub track_number: i64,
    /// Release year, when known.
    pub year: Option<i64>,
    /// First genre tag, when tagged.
    pub genre: Option<String>,
    /// Duration, seconds.
    pub duration_seconds: Option<f64>,
    /// Container or codec label, e.g. `flac`.
    pub format: String,
    /// Bit rate, when probed.
    pub bit_rate: Option<i64>,
    /// Sample rate, when probed.
    pub sample_rate: Option<i64>,
    /// File size, bytes.
    pub file_size_bytes: i64,
    /// Import time, unix seconds.
    pub date_added: Option<f64>,
    /// True when the owning album has artwork.
    pub cover_available: bool,
    /// True when the caller favorited this track.
    pub favorite: bool,
}

/// One page of catalog tracks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TrackPage {
    /// This page of tracks.
    pub items: Vec<TrackView>,
    /// Tracks matching the filter, all pages.
    pub total: u64,
    /// Echo of the requested offset.
    pub offset: u64,
    /// Echo of the requested limit.
    pub limit: u64,
}

/// Track list query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TrackQuery {
    /// Page size, 1-500. Default 50.
    pub limit: Option<i64>,
    /// Page start. Default 0.
    pub offset: Option<i64>,
    /// `title` or `date_added`. Default `title`.
    pub sort: Option<String>,
    /// `asc` or `desc`. Default `asc`.
    pub order: Option<String>,
    /// Title/artist/album substring filter, case folded.
    pub q: Option<String>,
    /// Restrict to one album.
    pub album_id: Option<String>,
    /// Restrict to one credited artist.
    pub artist_id: Option<String>,
    /// Restrict to one genre tag, case folded.
    pub genre: Option<String>,
}

/// Library totals. Track and size totals count streamable tracks only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StatsView {
    /// Albums in the catalog.
    pub total_albums: u64,
    /// Artists in the catalog.
    pub total_artists: u64,
    /// Streamable tracks in the catalog.
    pub total_tracks: u64,
    /// Summed streamable-track size, bytes.
    pub total_size_bytes: i64,
    /// Streamable-track counts by format label.
    pub format_breakdown: HashMap<String, u64>,
    /// Albums the caller favorited.
    pub favorite_albums: u64,
    /// Artists the caller favorited.
    pub favorite_artists: u64,
    /// Tracks the caller favorited.
    pub favorite_tracks: u64,
}

/// One genre with streamable-only counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct GenreView {
    /// Genre display name, representative casing.
    pub name: String,
    /// Streamable tracks carrying this tag.
    pub track_count: u64,
    /// Albums holding at least one such track.
    pub album_count: u64,
}

/// Full genre listing. Catalogs hold dozens of genres, so no paging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct GenreList {
    /// Genres by descending track count, name tiebreak.
    pub items: Vec<GenreView>,
}

/// Compact album card for browse surfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlbumCard {
    /// Local album id.
    pub id: String,
    /// Album title.
    pub title: String,
    /// Album artist display name.
    pub artist_name: String,
    /// Linked MusicBrainz artist id, when the artist is identified.
    pub artist_mbid: Option<String>,
    /// Linked MusicBrainz release group, when identified.
    pub release_group_mbid: Option<String>,
    /// Release year, when known.
    pub year: Option<i64>,
    /// Streamable tracks on this album.
    pub track_count: u64,
    /// Summed streamable-track size, bytes.
    pub total_size_bytes: i64,
    /// Most common track format, when any track exists.
    pub primary_format: Option<String>,
    /// True when an artwork row exists for this album.
    pub cover_available: bool,
    /// Import time, unix seconds.
    pub date_added: Option<f64>,
}

/// One page of album cards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlbumCardPage {
    /// This page of cards.
    pub items: Vec<AlbumCard>,
    /// Cards matching the filter, all pages.
    pub total: u64,
    /// Echo of the requested offset.
    pub offset: u64,
    /// Echo of the requested limit.
    pub limit: u64,
}

/// Local-library browse query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BrowseQuery {
    /// Page size, 1-200. Default 50.
    pub limit: Option<i64>,
    /// Page start. Default 0.
    pub offset: Option<i64>,
    /// `name`, `date_added`, `year`, `random`, or `rediscover`
    /// (oldest first). Default `name`.
    pub sort: Option<String>,
    /// `asc` or `desc`. Default `asc`.
    pub order: Option<String>,
    /// Title/artist substring filter, case folded.
    pub q: Option<String>,
    /// Restrict to one decade start year, e.g. 1990.
    pub decade: Option<i64>,
}

/// Local-library search query. `q` is required.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    /// Substring query, case folded, at least one character.
    pub q: Option<String>,
    /// Cap applied per group, 1-50. Default 20.
    pub limit: Option<i64>,
}

/// Matching albums plus playable tracks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SearchResults {
    /// Albums matching the query.
    pub albums: Vec<AlbumCard>,
    /// Tracks matching the query.
    pub tracks: Vec<SuggestionTrack>,
}

/// One suggested track, tagged with why it was picked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SuggestionTrack {
    /// Local track id.
    pub track_id: String,
    /// Track title.
    pub title: String,
    /// Album title.
    pub album_title: String,
    /// Track artist display name.
    pub artist_name: String,
    /// Owning local album id.
    pub album_id: String,
    /// True when the owning album has artwork.
    pub cover_available: bool,
    /// Container or codec label.
    pub format: String,
    /// Release year, when known.
    pub year: Option<i64>,
    /// Duration, seconds.
    pub duration_seconds: Option<f64>,
    /// `recent`, `rediscover`, `surprise`, or `same_era`.
    pub reason: String,
}

/// Suggestion listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SuggestionsResponse {
    /// Reason-tagged tracks, pools interleaved.
    pub items: Vec<SuggestionTrack>,
}

/// Suggestion query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuggestionsQuery {
    /// Track cap, 1-40. Default 12.
    pub limit: Option<i64>,
    /// Era anchor for the `same_era` pool, e.g. 1990.
    pub decade: Option<i64>,
}

/// One decade shelf. Shelves carry counts; fetch the shelf's albums
/// through browse with the same `decade` value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DecadeShelf {
    /// Decade start year, e.g. 1990.
    pub decade: i64,
    /// Human label, e.g. `1990s`.
    pub label: String,
    /// Albums dated to this decade.
    pub album_count: u64,
}

/// Decade listing, ascending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DecadesResponse {
    /// Shelves, oldest decade first.
    pub items: Vec<DecadeShelf>,
}

/// Recent-albums query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RecentQuery {
    /// Card cap, 1-50. Default 20.
    pub limit: Option<i64>,
}

/// One lyric line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LyricLine {
    /// Line text.
    pub text: String,
    /// Line start, seconds, for synced lyrics.
    pub start_seconds: Option<f64>,
}

/// Stored lyrics for one track. Missing lyrics read as 404, never empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LyricsView {
    /// Full text, lines joined with newlines.
    pub text: String,
    /// True when lines carry timestamps.
    pub is_synced: bool,
    /// Lyric lines in order.
    pub lines: Vec<LyricLine>,
}
