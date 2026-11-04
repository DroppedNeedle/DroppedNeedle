//! Wire shapes for the library reads. All snake_case, all generated into
//! the OpenAPI doc; the frontend consumes the generated client, never copies.
//!
//! Counts are streamable-only throughout: tracks whose file is missing or
//! excluded never inflate album, artist, genre, or library totals. Favorite
//! flags and counts reflect the caller only.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::stores::{AlbumRecord, ArtistRecord, TrackRecord};

/// `linked` when an identity row exists, else `local_only`.
fn identity_state(linked: bool) -> String {
    if linked { "linked" } else { "local_only" }.to_owned()
}

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
    /// Tracks the album's edition has: the matched MusicBrainz release's
    /// tracklist while the catalog holds it, else the totals its files'
    /// tags give; a guess the files' tags contradict counts by its tags.
    /// `None` when neither tells.
    pub expected_track_count: Option<u64>,
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
    /// True when the album has art of its own: a folder image or an
    /// embedded picture.
    pub cover_available: bool,
    /// True when the server knows the cover routes the web UI would ask
    /// answer 404: no art of its own, and no release group, or one whose
    /// cover is known missing with no other copy's art standing in. False
    /// means ask.
    pub art_missing: bool,
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

/// A library album with whether the caller favorited it and whether its
/// cover is known missing.
impl From<(AlbumRecord, bool, bool)> for AlbumView {
    fn from((album, favorite, art_missing): (AlbumRecord, bool, bool)) -> Self {
        Self {
            id: album.id,
            title: album.title,
            artist_name: album.artist_name,
            artist_id: album.artist_id,
            release_group_mbid: album.release_group_mbid,
            release_mbid: album.release_mbid,
            artist_mbid: album.artist_mbid,
            identity_state: identity_state(album.linked),
            track_count: album.track_count,
            expected_track_count: None,
            total_duration_seconds: album.total_duration_seconds,
            total_size_bytes: album.total_size_bytes,
            format: album.format,
            year: album.year,
            is_compilation: album.is_compilation,
            cover_available: album.cover_available,
            art_missing,
            date_added: album.date_added,
            favorite,
            contribution_id: album.contribution_id,
            contribution_state: album.contribution_state,
        }
    }
}

/// Why library management leaves an album's files alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ManagementHoldView {
    /// `your_choice` when a person chose to leave the album unmanaged.
    /// Otherwise why DroppedNeedle stepped back by itself: `files_changed`
    /// (its files were changed outside DroppedNeedle), `retag_followed`
    /// (it was retagged outside DroppedNeedle), `run_undone` (a person
    /// undid DroppedNeedle's changes to it) or `replaced_hands_off` (its
    /// files appeared beside a hands-off album in the scan that saw that
    /// album's files go).
    pub kind: String,
    /// Since when, unix milliseconds.
    pub held_at: i64,
    /// For `your_choice`: the exclusion's revision, which re-enabling
    /// management echoes.
    pub exclusion_revision: Option<i64>,
    /// True while the album waits for its turn after every album was
    /// handed back at once.
    pub handing_back: bool,
}

/// One catalog album with what its own page shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlbumDetailView {
    #[serde(flatten)]
    pub album: AlbumView,
    /// Set when library management leaves this album's files alone.
    pub management_hold: Option<ManagementHoldView>,
    /// The album row's revision, which a change to its grouping and its
    /// removal echo.
    pub row_revision: i64,
    /// Moves when the album is renamed or a track joins or leaves it. A
    /// removal sends it back beside `row_revision`.
    pub listing_revision: i64,
    /// The album's pending identification review, if any.
    pub pending_review_id: Option<String>,
}

/// One page of catalog albums.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlbumPage {
    /// This page of albums.
    pub items: Vec<AlbumView>,
    /// Albums matching the filter, all pages.
    pub total: u64,
    /// Pass as `after` for the next page; absent on the last page.
    pub next: Option<String>,
}

/// Which way a list runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ListOrder {
    /// Smallest or earliest first.
    Asc,
    /// Largest or latest first.
    Desc,
}

/// How the album list is sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlbumListSort {
    /// Title.
    Name,
    /// Import time.
    DateAdded,
    /// Release year, unknowns last.
    Year,
    /// Album artist.
    Artist,
    /// One shuffle per walk, kept across its pages.
    Random,
    /// Oldest imports first.
    Rediscover,
}

/// How the artist list is sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtistListSort {
    /// Display name.
    Name,
    /// Albums the artist leads.
    AlbumCount,
    /// Albums the artist appears on without leading.
    AppearanceCount,
    /// First import time.
    DateAdded,
}

/// Which artists the artist list covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtistListScope {
    /// Every credited artist.
    All,
    /// Artists leading at least one album.
    AlbumArtists,
    /// Artists appearing without leading.
    Contributors,
}

/// How the track list is sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackListSort {
    /// Title.
    Title,
    /// Import time.
    DateAdded,
    /// Track artist, then album, disc and track number.
    Artist,
    /// Album, then disc and track number.
    Album,
    /// One shuffle per walk, kept across its pages.
    Random,
}

/// Album list query. Unknown `sort` values fail with INVALID_INPUT.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AlbumQuery {
    /// Page size, 1-200. Default 50.
    pub limit: Option<i64>,
    /// The `next` of the page before, with the same sort and order.
    /// Absent for the first page.
    pub after: Option<String>,
    /// Default `name`.
    pub sort: Option<AlbumListSort>,
    /// Default `asc`.
    pub order: Option<ListOrder>,
    /// Title/artist substring filter, case folded.
    pub q: Option<String>,
    /// Restrict to one album artist.
    pub artist_id: Option<String>,
    /// Restrict to one decade start year, e.g. 1990.
    pub decade: Option<i64>,
    /// Restrict to one primary track format, e.g. `flac`. Blank reads as
    /// absent; matching is case-insensitive. `formats` wins when both are
    /// given.
    pub format: Option<String>,
    /// Primary track formats, comma-separated, e.g. `flac,mp3`; matching is
    /// case-insensitive. See `format_mode`.
    pub formats: Option<String>,
    /// `with` (default): only albums in one of `formats`; `without`: only
    /// albums in none of them.
    pub format_mode: Option<String>,
    /// Only albums with at least this many indexed tracks, 1-10000.
    pub min_tracks: Option<u32>,
    /// Only the caller's favourites. Without `sort`, the newest favourite
    /// comes first, and `order=asc` puts the oldest first.
    pub favorites: Option<bool>,
}

/// Page query for an artist's albums and appearances.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CursorQuery {
    /// Page size, 1-1000. Default 200.
    pub limit: Option<i64>,
    /// The `next` of the page before. Absent for the first page.
    pub after: Option<String>,
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
    /// Streamable tracks on albums this artist leads, whoever is credited
    /// on them.
    pub led_track_count: u64,
    /// Albums where this artist appears without leading.
    pub appearance_album_count: u64,
    /// Streamable tracks credited to this artist on albums led by someone
    /// else.
    pub appearance_track_count: u64,
    /// First import time, unix seconds.
    pub date_added: Option<f64>,
    /// True when the server knows the picture route the web UI would ask
    /// answers 404: a linked artist's picture is known missing, or an
    /// unlinked artist has no album art to draw from. False means ask.
    pub art_missing: bool,
    /// True when the caller favorited this artist.
    pub favorite: bool,
}

/// A library artist with whether the caller favorited it and whether its
/// picture is known missing.
impl From<(ArtistRecord, bool, bool)> for ArtistView {
    fn from((artist, favorite, art_missing): (ArtistRecord, bool, bool)) -> Self {
        Self {
            id: artist.id,
            name: artist.name,
            artist_mbid: artist.artist_mbid,
            identity_state: identity_state(artist.linked),
            album_count: artist.album_count,
            track_count: artist.track_count,
            led_track_count: artist.led_track_count,
            appearance_album_count: artist.appearance_album_count,
            appearance_track_count: artist.appearance_track_count,
            date_added: artist.date_added,
            art_missing,
            favorite,
        }
    }
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
    /// Pass as `after` for the next page; absent on the last page.
    pub next: Option<String>,
}

/// Artist list query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ArtistQuery {
    /// Page size, 1-200. Default 50.
    pub limit: Option<i64>,
    /// The `next` of the page before, with the same scope, sort and
    /// order. Absent for the first page.
    pub after: Option<String>,
    /// Default `name`.
    pub sort: Option<ArtistListSort>,
    /// Default `asc`.
    pub order: Option<ListOrder>,
    /// Name substring filter, case folded.
    pub q: Option<String>,
    /// Default `all`.
    pub scope: Option<ArtistListScope>,
    /// Only artists with at least this many indexed tracks in the library,
    /// 1-10000: the tracks credited to them and the tracks on the albums
    /// they lead, each once. The scope totals count the same artists.
    pub min_tracks: Option<u32>,
    /// Only the caller's favourites. Without `sort`, the newest favourite
    /// comes first, and `order=asc` puts the oldest first.
    pub favorites: Option<bool>,
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
    /// Local album-artist id of the owning album.
    pub album_artist_id: Option<String>,
    /// Linked MusicBrainz recording, when identified.
    pub recording_mbid: Option<String>,
    /// Linked release group of the owning album, when identified.
    pub release_group_mbid: Option<String>,
    /// Linked MusicBrainz id of the track artist (the album artist when
    /// the track carries no credit), when identified.
    pub artist_mbid: Option<String>,
    /// Linked MusicBrainz id of the album artist, when identified.
    pub album_artist_mbid: Option<String>,
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
    /// Bit rate in kbit/s, when probed.
    pub bit_rate: Option<i64>,
    /// Sample rate, when probed.
    pub sample_rate: Option<i64>,
    /// Bit depth, when probed (lossless and PCM files).
    pub bit_depth: Option<i64>,
    /// Channel count, when probed.
    pub channels: Option<i64>,
    /// File size, bytes.
    pub file_size_bytes: i64,
    /// Import time, unix seconds.
    pub date_added: Option<f64>,
    /// True when the owning album has art of its own: a folder image or an
    /// embedded picture.
    pub cover_available: bool,
    /// The owning album's `art_missing`: true when the server knows its
    /// cover routes answer 404. False means ask.
    pub art_missing: bool,
    /// True when the caller favorited this track.
    pub favorite: bool,
    /// The track row's revision, which its removal echoes.
    pub row_revision: i64,
}

/// A library track with whether the caller favorited it and whether its
/// album's cover is known missing.
impl From<(TrackRecord, bool, bool)> for TrackView {
    fn from((track, favorite, art_missing): (TrackRecord, bool, bool)) -> Self {
        Self {
            id: track.id,
            title: track.title,
            album_id: track.album_id,
            album_title: track.album_title,
            artist_name: track.artist_name,
            artist_id: track.artist_id,
            album_artist_name: track.album_artist_name,
            album_artist_id: track.album_artist_id,
            recording_mbid: track.recording_mbid,
            release_group_mbid: track.release_group_mbid,
            artist_mbid: track.artist_mbid,
            album_artist_mbid: track.album_artist_mbid,
            disc_number: track.disc_number,
            track_number: track.track_number,
            year: track.year,
            genre: track.genre,
            duration_seconds: track.duration_seconds,
            format: track.format,
            bit_rate: track.bit_rate,
            sample_rate: track.sample_rate,
            bit_depth: track.bit_depth,
            channels: track.channels,
            file_size_bytes: track.file_size_bytes,
            date_added: track.date_added,
            cover_available: track.cover_available,
            art_missing,
            favorite,
            row_revision: track.row_revision,
        }
    }
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

/// One page of the track list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TrackListPage {
    /// This page of tracks.
    pub items: Vec<TrackView>,
    /// Tracks matching the filter, all pages.
    pub total: u64,
    /// Pass as `after` for the next page; absent on the last page.
    pub next: Option<String>,
}

/// Track list query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TrackQuery {
    /// Page size, 1-500. Default 50.
    pub limit: Option<i64>,
    /// The `next` of the page before, with the same sort and order.
    /// Absent for the first page.
    pub after: Option<String>,
    /// Default `title`.
    pub sort: Option<TrackListSort>,
    /// Default `asc`.
    pub order: Option<ListOrder>,
    /// Title/artist/album substring filter, case folded.
    pub q: Option<String>,
    /// Restrict to one album.
    pub album_id: Option<String>,
    /// Restrict to one credited artist.
    pub artist_id: Option<String>,
    /// Restrict to one genre tag, case folded.
    pub genre: Option<String>,
    /// Only the caller's favourites. Without `sort`, the newest favourite
    /// comes first, and `order=asc` puts the oldest first.
    pub favorites: Option<bool>,
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
    /// Identification reviews waiting on a person ("Needs you").
    pub review_count: u64,
    /// Albums whose edition is a best guess.
    pub guess_count: u64,
    /// Albums nothing on MusicBrainz fits.
    pub not_found_count: u64,
    /// Albums someone marked "Looks wrong".
    pub looks_wrong_count: u64,
    /// Albums to check, each once: a best guess, marked "Looks wrong" or
    /// with an open review.
    pub check_count: u64,
    /// Albums with streamable tracks and no MusicBrainz identity.
    pub local_only_count: u64,
    /// When the last library scan finished successfully, unix seconds.
    pub last_scan_at: Option<f64>,
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

/// How many albums one primary format has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AlbumFormatCount {
    /// Lowercased format, e.g. `flac`.
    pub format: String,
    /// Listed albums whose most common track format it is.
    pub album_count: u64,
}

/// Every primary format the album list has, for its format filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AlbumFormatCounts {
    /// Formats by descending album count, name tiebreak.
    pub items: Vec<AlbumFormatCount>,
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
    /// True when the owning album has a folder image or an embedded picture.
    pub cover_available: bool,
    /// Container or codec label.
    pub format: String,
    /// Release year, when known.
    pub year: Option<i64>,
    /// Duration, seconds.
    pub duration_seconds: Option<f64>,
    /// Which pool the track came from.
    pub reason: SuggestionReason,
}

/// Why a track was suggested: the pool it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionReason {
    /// Among the newest imports.
    Recent,
    /// Among the oldest imports.
    Rediscover,
    /// A random pick.
    Surprise,
    /// A random pick from the asked-for decade.
    SameEra,
}

/// A library track with why it was suggested.
impl From<(TrackRecord, SuggestionReason)> for SuggestionTrack {
    fn from((track, reason): (TrackRecord, SuggestionReason)) -> Self {
        Self {
            track_id: track.id,
            title: track.title,
            album_title: track.album_title,
            artist_name: track.artist_name,
            album_id: track.album_id,
            cover_available: track.cover_available,
            format: track.format,
            year: track.year,
            duration_seconds: track.duration_seconds,
            reason,
        }
    }
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
/// through the album list with the same `decade` value.
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

/// Which of these MusicBrainz album ids the library holds or has an open
/// request for. Ids compare case-insensitively.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct LibraryMembershipRequest {
    /// Release-group (or release) ids, at most 500 after de-duplication.
    #[serde(default)]
    pub album_ids: Vec<String>,
}

/// Membership answer. Every list is lowercase and sorted by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryMembershipResponse {
    /// Asked ids the library holds with at least one streamable track.
    pub owned_ids: Vec<String>,
    /// Asked ids with an open acquisition request.
    pub requested_ids: Vec<String>,
    /// For each asked release group the library owns, how many of its
    /// tracks the library holds and how many its edition has.
    pub held_tracks: Vec<HeldTracks>,
}

/// How much of one release group the library holds, over every album that
/// owns it: their tracks add up, to the edition's count at most.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HeldTracks {
    /// Lowercase release-group id.
    pub id: String,
    /// Tracks the library holds.
    pub held: u64,
    /// Tracks the edition has; `None` when neither the matched release nor
    /// the files' tags tell. A guess the files' tags contradict counts by
    /// its tags.
    pub expected: Option<u64>,
}

/// One artist value as the file carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TagArtistCredit {
    /// Artist name.
    pub name: String,
    /// Name as credited, when it differs.
    pub credited_name: Option<String>,
    /// Sort name.
    pub sort_name: Option<String>,
    /// MusicBrainz artist id.
    pub musicbrainz_artist_id: Option<String>,
    /// Text joining this credit to the next.
    pub join_phrase: String,
}

impl From<droppedneedle_library::tags::AudioArtistCredit> for TagArtistCredit {
    fn from(credit: droppedneedle_library::tags::AudioArtistCredit) -> Self {
        Self {
            name: credit.name,
            credited_name: credit.credited_name,
            sort_name: credit.sort_name,
            musicbrainz_artist_id: credit.musicbrainz_artist_id,
            join_phrase: credit.join_phrase,
        }
    }
}

/// One held track on the album status view, judged against the upgrade
/// settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryStatusTrack {
    /// The track.
    #[serde(flatten)]
    pub track: TrackView,
    /// Quality tier of the file: `lossless`, `mp3_320`, `mp3_256`,
    /// `mp3_192`, or `low`.
    pub current_tier: String,
    /// True when upgrades are on and the file sits below the cutoff tier.
    pub below_cutoff: bool,
}

/// What the library holds for one album, by local id or MusicBrainz id.
/// Unknown albums answer with `in_library` false, never 404.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryAlbumStatus {
    /// True when at least one streamable track is held.
    pub in_library: bool,
    /// The local album id holding the tracks, or the asked id when none.
    pub album_id: String,
    /// Held streamable tracks.
    pub track_count: u64,
    /// Held tracks by album, disc and track.
    pub tracks: Vec<LibraryStatusTrack>,
}

/// One track position to look up in the library.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ResolveTrackItem {
    /// MusicBrainz release group (or release, or local album id) the track
    /// belongs to.
    #[serde(default)]
    pub release_group_mbid: Option<String>,
    /// Disc number; absent reads as disc 1.
    #[serde(default)]
    pub disc_number: Option<i64>,
    /// Track number within the disc.
    #[serde(default)]
    pub track_number: Option<i64>,
}

/// Track positions to resolve to playable local files. At most the first
/// 200 items are answered.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ResolveTracksRequest {
    /// Positions in the caller's order.
    #[serde(default)]
    pub items: Vec<ResolveTrackItem>,
}

/// One answered position. The `source` fields are set only when a local
/// file holds the position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ResolvedTrack {
    /// Echo of the asked album id.
    pub release_group_mbid: Option<String>,
    /// Echo of the asked disc.
    pub disc_number: Option<i64>,
    /// Echo of the asked track number.
    pub track_number: Option<i64>,
    /// `local` when a library file holds the position.
    pub source: Option<String>,
    /// Local track id to stream.
    pub track_source_id: Option<String>,
    /// Stream path for the local track.
    pub stream_url: Option<String>,
    /// Container or codec label of the file.
    pub format: Option<String>,
    /// Duration of the file, seconds.
    pub duration: Option<f64>,
}

/// Resolved positions, in the asked order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ResolveTracksResponse {
    /// One answer per asked position (first 200).
    pub items: Vec<ResolvedTrack>,
}
