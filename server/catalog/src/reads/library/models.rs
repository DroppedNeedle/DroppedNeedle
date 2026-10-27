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

/// A library album with whether the caller favorited it.
impl From<(AlbumRecord, bool)> for AlbumView {
    fn from((album, favorite): (AlbumRecord, bool)) -> Self {
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
            total_duration_seconds: album.total_duration_seconds,
            total_size_bytes: album.total_size_bytes,
            format: album.format,
            year: album.year,
            is_compilation: album.is_compilation,
            cover_available: album.cover_available,
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

/// Album list query. Unknown `sort` values fail with INVALID_INPUT.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AlbumQuery {
    /// Page size, 1-200. Default 50.
    pub limit: Option<i64>,
    /// The `next` of the page before, with the same sort and order.
    /// Absent for the first page.
    pub after: Option<String>,
    /// `name`, `date_added`, `year`, `artist`, `random` (one shuffle per
    /// walk, kept across its pages), or `rediscover` (oldest first).
    /// Default `name`.
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
    /// Albums where this artist appears without leading.
    pub appearance_album_count: u64,
    /// Streamable tracks credited to this artist on albums led by someone
    /// else.
    pub appearance_track_count: u64,
    /// First import time, unix seconds.
    pub date_added: Option<f64>,
    /// True when the caller favorited this artist.
    pub favorite: bool,
}

/// A library artist with whether the caller favorited it.
impl From<(ArtistRecord, bool)> for ArtistView {
    fn from((artist, favorite): (ArtistRecord, bool)) -> Self {
        Self {
            id: artist.id,
            name: artist.name,
            artist_mbid: artist.artist_mbid,
            identity_state: identity_state(artist.linked),
            album_count: artist.album_count,
            track_count: artist.track_count,
            appearance_album_count: artist.appearance_album_count,
            appearance_track_count: artist.appearance_track_count,
            date_added: artist.date_added,
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
    /// `name`, `album_count`, `appearance_count`, or `date_added`.
    /// Default `name`.
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
    /// True when the owning album has artwork.
    pub cover_available: bool,
    /// True when the caller favorited this track.
    pub favorite: bool,
}

/// A library track with whether the caller favorited it.
impl From<(TrackRecord, bool)> for TrackView {
    fn from((track, favorite): (TrackRecord, bool)) -> Self {
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
            favorite,
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
    /// `title`, `date_added`, `artist` (then album, disc and track),
    /// `album` (then disc and track), or `random` (one shuffle per walk,
    /// kept across its pages). Default `title`.
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
    /// Identification reviews waiting on a person.
    pub review_count: u64,
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

/// Full genre listing. Catalogs hold dozens of genres, so no paging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct GenreList {
    /// Genres by descending track count, name tiebreak.
    pub items: Vec<GenreView>,
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

/// A library track with why it was suggested.
impl From<(TrackRecord, &str)> for SuggestionTrack {
    fn from((track, reason): (TrackRecord, &str)) -> Self {
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
            reason: reason.to_owned(),
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

/// Membership answer. Both lists are lowercase and sorted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryMembershipResponse {
    /// Asked ids the library holds with at least one streamable track.
    pub owned_ids: Vec<String>,
    /// Asked ids with an open acquisition request.
    pub requested_ids: Vec<String>,
}

/// One artist value as the file carries it (v2 `AudioArtistCredit`).
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

/// The tags in one audio file, field for field v2's `AudioTag`. The
/// `musicbrainz_*` fields use Picard's tag names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TrackTags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_number: u32,
    pub album_artist: Option<String>,
    pub disc_number: u32,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
    pub musicbrainz_album_artist_id: Option<String>,
    pub acoustid_id: Option<String>,
    pub compilation: bool,
    /// Raw RELEASETYPE / MUSICBRAINZ_ALBUMTYPE value.
    pub release_type: Option<String>,
    pub title_sort: Option<String>,
    pub artist_sort: Option<String>,
    pub album_sort: Option<String>,
    pub album_artist_sort: Option<String>,
    pub disc_subtitle: Option<String>,
    pub original_release_date: Option<String>,
    pub replaygain_track_gain: Option<f64>,
    pub replaygain_album_gain: Option<f64>,
    pub replaygain_track_peak: Option<f64>,
    pub replaygain_album_peak: Option<f64>,
    pub genres: Vec<String>,
    pub artists: Vec<TagArtistCredit>,
    pub album_artists: Vec<TagArtistCredit>,
    pub musicbrainz_artist_ids: Vec<String>,
    pub musicbrainz_album_artist_ids: Vec<String>,
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

impl From<droppedneedle_library::tags::AudioTag> for TrackTags {
    fn from(tag: droppedneedle_library::tags::AudioTag) -> Self {
        let credits = |list: Vec<droppedneedle_library::tags::AudioArtistCredit>| {
            list.into_iter().map(TagArtistCredit::from).collect()
        };
        Self {
            title: tag.title,
            artist: tag.artist,
            album: tag.album,
            track_number: tag.track_number,
            album_artist: tag.album_artist,
            disc_number: tag.disc_number,
            year: tag.year,
            genre: tag.genre,
            musicbrainz_release_group_id: tag.musicbrainz_release_group_id,
            musicbrainz_release_id: tag.musicbrainz_release_id,
            musicbrainz_recording_id: tag.musicbrainz_recording_id,
            musicbrainz_release_track_id: tag.musicbrainz_release_track_id,
            musicbrainz_artist_id: tag.musicbrainz_artist_id,
            musicbrainz_album_artist_id: tag.musicbrainz_album_artist_id,
            acoustid_id: tag.acoustid_id,
            compilation: tag.compilation,
            release_type: tag.release_type,
            title_sort: tag.title_sort,
            artist_sort: tag.artist_sort,
            album_sort: tag.album_sort,
            album_artist_sort: tag.album_artist_sort,
            disc_subtitle: tag.disc_subtitle,
            original_release_date: tag.original_release_date,
            replaygain_track_gain: tag.replaygain_track_gain,
            replaygain_album_gain: tag.replaygain_album_gain,
            replaygain_track_peak: tag.replaygain_track_peak,
            replaygain_album_peak: tag.replaygain_album_peak,
            genres: tag.genres,
            artists: credits(tag.artists),
            album_artists: credits(tag.album_artists),
            musicbrainz_artist_ids: tag.musicbrainz_artist_ids,
            musicbrainz_album_artist_ids: tag.musicbrainz_album_artist_ids,
        }
    }
}

/// Every MusicBrainz release group the library holds and every album with
/// an open request (v2 `/library/mbids`). Both lists are lowercase and
/// sorted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryMbids {
    /// Release groups held with at least one streamable track.
    pub mbids: Vec<String>,
    /// Album ids with an open acquisition request.
    pub requested_mbids: Vec<String>,
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
