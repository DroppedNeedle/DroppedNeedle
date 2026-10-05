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
