//! Ports: the seams the library reads depend on.
//!
//! Every external behavior sits behind one of these traits so tests inject
//! fakes (see `memory.rs`). The SQLite implementations in `sqlite.rs` read
//! the 0001 baseline tables. Futures are boxed by hand because `async fn`
//! is not object-safe.
//!
//! Absence is `None` or an empty page, never failure. Counts are
//! streamable-only: tracks with `availability` other than `indexed` are
//! invisible to every method here.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

/// Boxed sendable future for object-safe async ports.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Every way a store call can fail. The string goes to the log only,
/// never the wire: handlers render the fixed 5xx body.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    /// Anything unexpected. The string goes to the log only, never the wire.
    #[error("store failure: {0}")]
    Internal(String),
}

/// Album sort orders the list methods accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumSort {
    /// Title, case folded.
    Name,
    /// Import time.
    DateAdded,
    /// Release year, unknowns last.
    Year,
    /// Album artist, case folded.
    Artist,
    /// Database random order.
    Random,
    /// Oldest imports first.
    Rediscover,
}

/// Artist sort orders the list method accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistSort {
    /// Display name, case folded.
    Name,
    /// Album-artist album count.
    AlbumCount,
    /// Albums the artist appears on without leading.
    AppearanceCount,
    /// First import time.
    DateAdded,
}

/// Track sort orders the list method accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSort {
    /// Title, case folded.
    Title,
    /// Import time.
    DateAdded,
}

/// Which artists a listing covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtistScope {
    /// Every credited artist.
    #[default]
    All,
    /// Artists leading at least one album.
    AlbumArtists,
    /// Artists appearing without leading.
    Contributors,
}

/// Album list filter, shared by the catalog and browse surfaces.
#[derive(Debug, Clone, Default)]
pub struct AlbumFilter {
    /// Title/artist substring, case folded.
    pub q: Option<String>,
    /// Restrict to one album artist.
    pub artist_id: Option<String>,
    /// Restrict to one decade start year.
    pub decade: Option<i64>,
    /// Restrict to one primary track format, already lowercased.
    pub format: Option<String>,
}

/// Track list filter.
#[derive(Debug, Clone, Default)]
pub struct TrackFilter {
    /// Title/artist/album substring, case folded.
    pub q: Option<String>,
    /// Restrict to one album.
    pub album_id: Option<String>,
    /// Restrict to one credited artist.
    pub artist_id: Option<String>,
    /// Restrict to one genre tag, case folded.
    pub genre: Option<String>,
}

/// One album row with its streamable-only aggregates.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRecord {
    /// Local album id.
    pub id: String,
    /// Album title.
    pub title: String,
    /// Album artist display name.
    pub artist_name: String,
    /// Local album-artist id.
    pub artist_id: String,
    /// Linked release group, when identified.
    pub release_group_mbid: Option<String>,
    /// Linked exact release, when known.
    pub release_mbid: Option<String>,
    /// Linked artist id, when the artist is identified.
    pub artist_mbid: Option<String>,
    /// True when an identity row exists.
    pub linked: bool,
    /// Streamable track count.
    pub track_count: u64,
    /// Summed streamable-track duration, seconds.
    pub total_duration_seconds: f64,
    /// Summed streamable-track size, bytes.
    pub total_size_bytes: i64,
    /// Most common track format, when any track exists.
    pub format: Option<String>,
    /// Release year, when known.
    pub year: Option<i64>,
    /// True for compilations.
    pub is_compilation: bool,
    /// True when an artwork row exists.
    pub cover_available: bool,
    /// Import time, unix seconds.
    pub date_added: Option<f64>,
    /// The album's open MusicBrainz contribution, if any.
    pub contribution_id: Option<String>,
    /// That contribution's state (`draft`, `ready`, `seeded`, ...).
    pub contribution_state: Option<String>,
}

/// One artist row with its streamable-only aggregates.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistRecord {
    /// Local artist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Linked MusicBrainz id, when identified.
    pub artist_mbid: Option<String>,
    /// True when an identity row exists.
    pub linked: bool,
    /// Albums led as album artist.
    pub album_count: u64,
    /// Streamable tracks credited.
    pub track_count: u64,
    /// Albums with an appearance but no lead.
    pub appearance_album_count: u64,
    /// First import time, unix seconds.
    pub date_added: Option<f64>,
}

/// One streamable track row.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackRecord {
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
    /// the track has no credit), when identified.
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
    /// Container or codec label.
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
}

/// Library totals. Track and size totals count streamable tracks only.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatsRecord {
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
}

/// Library totals that live outside the catalog tables: the review
/// queue, unidentified albums and the last finished scan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatsExtras {
    /// Identification reviews waiting on a person.
    pub review_count: u64,
    /// Albums with streamable tracks and no MusicBrainz identity.
    pub local_only_count: u64,
    /// When the last scan finished successfully, unix seconds.
    pub last_scan_at: Option<f64>,
}

/// The upgrade settings album status judges tracks against, read from the
/// download policy on every call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpgradePolicy {
    /// Tier a track must reach before it stops counting as an upgrade
    /// candidate, e.g. `lossless`.
    pub quality_cutoff: Option<String>,
    /// Whether upgrades are switched on at all.
    pub upgrade_allowed: bool,
}

/// Reads the current upgrade settings. A closure so production can read the
/// config store per call and tests can pin a value.
pub type UpgradePolicySource = std::sync::Arc<dyn Fn() -> UpgradePolicy + Send + Sync>;

/// One genre with streamable-only counts.
#[derive(Debug, Clone, PartialEq)]
pub struct GenreRecord {
    /// Display name, representative casing.
    pub name: String,
    /// Folded name, the stable key.
    pub folded_name: String,
    /// Streamable tracks carrying this tag.
    pub track_count: u64,
    /// Albums holding at least one such track.
    pub album_count: u64,
}

/// One decade with its album count.
#[derive(Debug, Clone, PartialEq)]
pub struct DecadeRecord {
    /// Decade start year, e.g. 1990.
    pub decade: i64,
    /// Albums dated to this decade.
    pub album_count: u64,
}

/// Artist listing: page items, match total, album-artist total,
/// contributor total.
pub type ArtistListing = (Vec<ArtistRecord>, u64, u64, u64);

/// Stored lyrics for one track.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricDoc {
    /// Lines in order: text plus optional start, milliseconds.
    pub lines: Vec<(String, Option<i64>)>,
    /// True when lines carry timestamps.
    pub synced: bool,
}

/// Catalog reads over the `local_*` tables plus artwork and identity rows.
///
/// Table mapping: `local_albums`, `local_artists`, `local_tracks`,
/// `local_album_artists`, `local_track_artists`,
/// `local_album_external_identities`, `local_artist_external_identities`,
/// `local_track_genres`, `local_album_artwork`. Every track read filters
/// `local_tracks.availability = 'indexed'`; missing or excluded files are
/// invisible here.
pub trait LibraryCatalog: Send + Sync {
    /// One page of albums plus the match total.
    fn list_albums<'a>(
        &'a self,
        filter: &'a AlbumFilter,
        sort: AlbumSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<AlbumRecord>, u64), StoreError>>;

    /// One album by id. None is absence, never failure.
    fn get_album<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumRecord>, StoreError>>;

    /// Oldest album carrying one release-group mbid. None is absence,
    /// never failure; siblings order oldest first so the match route is
    /// deterministic.
    fn get_album_by_release_group<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumRecord>, StoreError>>;

    /// One page of an album's streamable tracks, disc/track order, plus total.
    fn album_tracks<'a>(
        &'a self,
        album_id: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<TrackRecord>, u64), StoreError>>;

    /// Other local albums sharing the album's release group, oldest first.
    /// Empty when the album is unidentified or has no siblings.
    fn album_copies<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<AlbumRecord>, StoreError>>;

    /// One page of artists in scope plus the match total, the album-artist
    /// total, and the contributor total (totals honor `q`, ignore paging).
    fn list_artists<'a>(
        &'a self,
        scope: ArtistScope,
        q: Option<&'a str>,
        sort: ArtistSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<ArtistListing, StoreError>>;

    /// One artist by id. None is absence, never failure.
    fn get_artist<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ArtistRecord>, StoreError>>;

    /// One page of albums led by the artist, name order, plus total.
    fn artist_albums<'a>(
        &'a self,
        artist_id: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<AlbumRecord>, u64), StoreError>>;

    /// One page of albums where the artist appears without leading, name
    /// order, plus total.
    fn artist_appearances<'a>(
        &'a self,
        artist_id: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<AlbumRecord>, u64), StoreError>>;

    /// One page of streamable tracks plus the match total.
    fn list_tracks<'a>(
        &'a self,
        filter: &'a TrackFilter,
        sort: TrackSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<TrackRecord>, u64), StoreError>>;

    /// One streamable track by id. None covers unknown ids and tracks
    /// whose file is missing or excluded.
    fn get_track<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<TrackRecord>, StoreError>>;

    /// Library totals.
    fn stats<'a>(&'a self) -> BoxFuture<'a, Result<StatsRecord, StoreError>>;

    /// Newest albums first, capped.
    fn recently_added<'a>(
        &'a self,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<AlbumRecord>, StoreError>>;

    /// Genres by descending track count, name tiebreak.
    fn genres<'a>(&'a self) -> BoxFuture<'a, Result<Vec<GenreRecord>, StoreError>>;

    /// One page of a genre's streamable tracks, artist/album/disc/track
    /// order, plus total. Empty when the genre is unknown.
    fn genre_tracks<'a>(
        &'a self,
        genre_folded: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<TrackRecord>, u64), StoreError>>;

    /// Decade shelves, ascending. Albums without a year sit outside.
    fn decades<'a>(&'a self) -> BoxFuture<'a, Result<Vec<DecadeRecord>, StoreError>>;

    /// Suggestion candidates: newest imports.
    fn newest_tracks<'a>(
        &'a self,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>>;

    /// Suggestion candidates: oldest imports.
    fn oldest_tracks<'a>(
        &'a self,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>>;

    /// Suggestion candidates: random streamable tracks, optionally from
    /// one decade.
    fn random_tracks<'a>(
        &'a self,
        limit: u64,
        decade: Option<i64>,
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>>;
}

/// Favorite flags for the caller.
///
/// Table mapping: `library_user_favorites` (`item_kind` is `album`,
/// `artist`, or `track`; `item_id` is the local id).
pub trait FavoriteReads: Send + Sync {
    /// Which of the ids the user favorited, for one kind.
    fn filter_favorites<'a>(
        &'a self,
        user_id: &'a str,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>>;

    /// Favorite counts for the caller: albums, artists, tracks.
    fn favorite_counts<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(u64, u64, u64), StoreError>>;
}

/// Stored-lyrics reads for one track.
///
/// No baseline table holds lyrics: the library engine owns embedded-tag
/// extraction and the provider fetch is a separate client. This port is
/// the seam both implement behind. None is absence (the handler
/// answers 404), never failure.
pub trait LyricsPort: Send + Sync {
    /// Stored lyrics for one streamable track id.
    fn get<'a>(&'a self, track_id: &'a str) -> BoxFuture<'a, Result<Option<LyricDoc>, StoreError>>;
}

/// Every held release group and every open album request, lowercase and
/// sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumMbids {
    /// Release groups held with at least one streamable track.
    pub owned: Vec<String>,
    /// Album ids with an open acquisition request.
    pub requested: Vec<String>,
}

/// What reading one catalog track's tags from disk found.
#[derive(Debug, Clone, PartialEq)]
pub enum TagRead {
    /// The file's tags.
    Found(Box<crate::library::tags::AudioTag>),
    /// No streamable catalog track has this id.
    Unknown,
    /// The catalog knows the track, but its file is gone from disk.
    Gone,
    /// The file resolves outside every library root.
    Outside,
    /// The file is there but its tags cannot be read.
    Unreadable,
}

/// Tag reads from the audio files themselves (not the catalog copy).
pub trait TrackTagReader: Send + Sync {
    /// Read the tags of one catalog track's file.
    fn read<'a>(&'a self, track_id: &'a str) -> BoxFuture<'a, Result<TagRead, StoreError>>;
}

/// Identity lookups behind membership, album status, track resolution and
/// the stats extras.
///
/// Identifiers here are what a catalog page holds: a local album id, a
/// legacy alias, or a MusicBrainz release-group or release id (compared
/// case-insensitively). Retired (merged) albums resolve to the album they
/// merged into. Absence is an empty answer, never failure.
pub trait LibraryLookups: Send + Sync {
    /// Review, local-only and last-scan totals.
    fn stats_extras<'a>(&'a self) -> BoxFuture<'a, Result<StatsExtras, StoreError>>;

    /// The MusicBrainz album ids (lowercase) the library holds with at
    /// least one streamable track.
    fn owned_albums<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>>;

    /// The album ids (lowercase) with an open acquisition request.
    fn requested_albums<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>>;

    /// Every held release group and every open album request (the
    /// unfiltered membership answer).
    fn all_album_mbids<'a>(&'a self) -> BoxFuture<'a, Result<AlbumMbids, StoreError>>;

    /// The one live local album each identifier names. Identifiers that
    /// name nothing, or a MusicBrainz id held by more than one album, are
    /// left out rather than guessed.
    fn resolve_albums<'a>(
        &'a self,
        identifiers: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, String>, StoreError>>;

    /// Every live local album an identifier covers for the album status
    /// view: the album a local id or alias names, else every album holding
    /// the MusicBrainz release group (or release). Ordered by id.
    fn status_albums<'a>(
        &'a self,
        identifier: &'a str,
    ) -> BoxFuture<'a, Result<Vec<String>, StoreError>>;

    /// Streamable tracks of the albums, ordered by album id, disc, track.
    fn album_tracks_batch<'a>(
        &'a self,
        album_ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>>;
}
