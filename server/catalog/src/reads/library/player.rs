//! Player reads: the catalog as Subsonic and Jellyfin clients see it.
//!
//! The compat protocols need a few things the native read routes do not:
//! every tag a player shows (bit depth, channels, replay gain, sort names,
//! all genres, provider MBIDs), filters by year range and genre, exact
//! artist-name matches, batch lookups by id, and the caller's play history.
//! [`PlayerCatalog`] is that port; [`SqlitePlayerCatalog`] reads the same
//! `local_*` tables as [`super::sqlite::SqliteCatalog`] and shares its album
//! columns. Every track read filters `availability = 'indexed'`.

use std::collections::{HashMap, HashSet};

use sqlx::{Row as _, SqlitePool};

use std::sync::{Arc, Mutex};

use super::sqlite::{
    ALBUM_COLUMNS, ALBUM_JOINS, LED_PREDICATE, LibraryDb, TRACK_MISS_ORACLE, TextSearch,
    hydrate_artists,
};
use super::stores::{AlbumRecord, ArtistRecord, BoxFuture, StoreError};
use droppedneedle_providers::musicbrainz::is_valid_mbid;

/// One streamable track with everything a player shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerTrack {
    /// Local track id.
    pub id: String,
    /// Title.
    pub title: String,
    /// Owning local album id.
    pub album_id: String,
    /// Album title.
    pub album_title: String,
    /// Track artist display name.
    pub artist_name: String,
    /// Lead track artist id, when credited.
    pub artist_id: Option<String>,
    /// Album artist id.
    pub album_artist_id: String,
    /// Album artist display name.
    pub album_artist_name: Option<String>,
    /// Disc number.
    pub disc_number: i64,
    /// Track number.
    pub track_number: i64,
    /// Year.
    pub year: Option<i64>,
    /// Genres in tag order.
    pub genres: Vec<String>,
    /// Duration, seconds.
    pub duration_seconds: Option<f64>,
    /// Container label, lowercased.
    pub format: String,
    /// Bit rate, kbps.
    pub bit_rate: Option<i64>,
    /// Sample rate, Hz.
    pub sample_rate: Option<i64>,
    /// Bit depth.
    pub bit_depth: Option<i64>,
    /// Channels.
    pub channels: Option<i64>,
    /// File size, bytes.
    pub file_size_bytes: i64,
    /// Path under the track's own library folder.
    pub relative_path: String,
    /// Import time, unix seconds.
    pub date_added: Option<f64>,
    /// Title sort tag.
    pub sort_name: Option<String>,
    /// Disc subtitle tag.
    pub disc_subtitle: Option<String>,
    /// Recording MBID: accepted identity, else the embedded tag.
    pub recording_mbid: Option<String>,
    /// Track artist MBID, when the artist is identified.
    pub artist_mbid: Option<String>,
    /// Album artist MBID, when identified.
    pub album_artist_mbid: Option<String>,
    /// Album release group, when identified.
    pub release_group_mbid: Option<String>,
    /// Release type tag.
    pub release_type: Option<String>,
    /// Replay gain: track gain dB.
    pub replaygain_track_gain: Option<f64>,
    /// Replay gain: album gain dB.
    pub replaygain_album_gain: Option<f64>,
    /// Replay gain: track peak.
    pub replaygain_track_peak: Option<f64>,
    /// Replay gain: album peak.
    pub replaygain_album_peak: Option<f64>,
    /// Whether the album has stored artwork.
    pub cover_available: bool,
    /// Whether the track's file carries lyrics, as the lyrics sweep found.
    pub has_lyrics: bool,
}

/// Per artist id: the summed length in seconds and the number of the
/// streamable tracks crediting it (`artist_durations`).
pub type ArtistTracks = HashMap<String, (f64, u64)>;

/// One album with the extra facts players show.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerAlbum {
    /// The catalog row.
    pub record: AlbumRecord,
    /// Primary genre.
    pub genre: Option<String>,
    /// Album sort name.
    pub sort_name: Option<String>,
    /// Original release date, `YYYY[-MM[-DD]]`.
    pub original_release_date: Option<String>,
    /// Release date, `YYYY[-MM[-DD]]`: the one every indexed track's tags
    /// give, else none.
    pub release_date: Option<String>,
}

/// Track filters; every set field narrows.
#[derive(Debug, Clone, Default)]
pub struct TrackQuery {
    /// Title/artist/album search: every word, any column.
    pub q: Option<String>,
    /// Exact title, matched folded.
    pub title: Option<String>,
    /// One album.
    pub album_id: Option<String>,
    /// Credited (any position) artist ids.
    pub artist_ids: Vec<String>,
    /// Album artist ids.
    pub album_artist_ids: Vec<String>,
    /// Genre, matched folded.
    pub genre: Option<String>,
    /// Lowest year, inclusive.
    pub year_from: Option<i64>,
    /// Highest year, inclusive.
    pub year_to: Option<i64>,
    /// Exact track or album artist name, matched folded.
    pub artist_name: Option<String>,
    /// The user whose plays and marks `played` and the play-history orders
    /// read.
    pub listener: Option<String>,
    /// Only tracks that are played for the listener (`true`) or not
    /// (`false`), as [`PlayedValues`] says.
    pub played: Option<bool>,
}

/// A sort key the player protocols can ask for in either direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKey {
    /// Import time.
    Added,
    /// Title.
    Title,
    /// Year; unknown years sort first ascending.
    Year,
    /// The listener's last play; never played sorts first ascending.
    LastPlayed,
    /// The listener's play count.
    PlayCount,
}

/// Track orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOrder {
    /// Album artist, album, disc, track.
    Album,
    /// Disc then track (one album's running order).
    Disc,
    /// Title.
    Title,
    /// Newest import first.
    Newest,
    /// A fresh random sample on every call; for one-shot lists
    /// (getRandomSongs, mixes), not for paging.
    Random,
    /// One fixed shuffle per seed: pages of the same seed never repeat or
    /// skip a track. Callers draw the seed with [`ShuffleSeeds`].
    Shuffle(u32),
    /// Insertion order (rowid): the full-library sync order. Pages walk the
    /// table in rowid order, and pages already read stay put while a scan
    /// appends tracks.
    Natural,
    /// One key, ascending or (`true`) descending; ids break ties in the
    /// same direction.
    By(OrderKey, bool),
}

/// Album filters; every set field narrows.
#[derive(Debug, Clone, Default)]
pub struct AlbumQuery {
    /// Title/artist substring.
    pub q: Option<String>,
    /// One album artist.
    pub artist_id: Option<String>,
    /// Any of these album artists.
    pub album_artist_ids: Vec<String>,
    /// Albums where one of these artists is credited on a track but is
    /// not the album artist ("appears on").
    pub appears_on: Vec<String>,
    /// Genre, matched folded on any track.
    pub genre: Option<String>,
    /// Lowest year, inclusive.
    pub year_from: Option<i64>,
    /// Highest year, inclusive.
    pub year_to: Option<i64>,
    /// The user whose plays and marks `played` and the play-history orders
    /// read.
    pub listener: Option<String>,
    /// Only albums that are played for the listener (`true`) or not
    /// (`false`), as [`PlayedValues`] says.
    pub played: Option<bool>,
}

/// Album orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumOrder {
    /// Newest import first.
    Newest,
    /// Title.
    Title,
    /// Album artist, then title.
    Artist,
    /// Year ascending, unknown last.
    YearAsc,
    /// Year descending, unknown last.
    YearDesc,
    /// One fixed shuffle per seed, for paging (see [`TrackOrder::Shuffle`]).
    Shuffle(u32),
    /// Insertion order (rowid).
    Natural,
    /// One key, ascending or (`true`) descending; ids break ties in the
    /// same direction.
    By(OrderKey, bool),
}

/// Play statistics for one item: count and last play (unix seconds).
pub type PlayStat = (u64, Option<f64>);

/// One listener's values for a track or an album in Jellyfin answers,
/// where a played mark and an unplayed mark weigh against the plays.
///
/// A track's counted plays are the listener's plays of it later than its
/// unplayed mark, in whole seconds, or all of them when it has none. Its
/// play count is that number, or 1 when it is marked played and has none;
/// its last play is the latest counted play, else the played mark; it is
/// played when its count is above 0.
///
/// An album's play count is the sum of its tracks' (every track on it,
/// streamable or not) plus the plays filed under it with no track, which a
/// mark never cuts off; its last play is the latest of theirs; it is
/// played when every streamable track on it is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlayedValues {
    /// Play count.
    pub plays: u64,
    /// Last play, unix seconds.
    pub last_played: Option<f64>,
    /// Whether it counts as played.
    pub played: bool,
}

/// The MusicBrainz ids player apps are sent for one album.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumIds {
    /// True when these are the accepted match's ids, false when they are
    /// the ones the album's files' tags agree on.
    pub accepted: bool,
    /// Release.
    pub release_mbid: Option<String>,
    /// Release group.
    pub release_group_mbid: Option<String>,
}

/// MusicBrainz ids for player answers, by local id. An album or artist
/// with no id is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerIds {
    /// Per album.
    pub albums: HashMap<String, AlbumIds>,
    /// Per artist.
    pub artists: HashMap<String, String>,
}

/// Catalog and history reads for the player protocols.
pub trait PlayerCatalog: Send + Sync {
    /// Tracks by id, in no particular order; unknown or unstreamable ids
    /// are absent.
    fn tracks_by_ids<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<PlayerTrack>, StoreError>>;

    /// One page of tracks plus the match total.
    fn tracks<'a>(
        &'a self,
        query: &'a TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<PlayerTrack>, u64), StoreError>>;

    /// One page of tracks without the total, for callers that never show
    /// it (counting a text search costs more than the page).
    fn track_page<'a>(
        &'a self,
        query: &'a TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<Vec<PlayerTrack>, StoreError>>;

    /// Albums by id, in no particular order.
    fn albums_by_ids<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<PlayerAlbum>, StoreError>>;

    /// One page of albums holding at least one streamable track, plus the
    /// match total.
    fn albums<'a>(
        &'a self,
        query: &'a AlbumQuery,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<PlayerAlbum>, u64), StoreError>>;

    /// One page of albums without the total.
    fn album_page<'a>(
        &'a self,
        query: &'a AlbumQuery,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<Vec<PlayerAlbum>, StoreError>>;

    /// One page of album artists without the total: name order, or
    /// insertion order when `natural` (the search3 empty-query sync, stable
    /// while a scan adds artists). `q` narrows by folded name substring.
    fn album_artist_page<'a>(
        &'a self,
        q: Option<&'a str>,
        natural: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<Vec<ArtistRecord>, StoreError>>;

    /// Which of `ids` exist, for one kind (`artist`, `album`, `track`).
    fn existing<'a>(
        &'a self,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>>;

    /// Catalog revision, bumped by every catalog write.
    fn revision<'a>(&'a self) -> BoxFuture<'a, Result<i64, StoreError>>;

    /// Album ids from the user's play history: most recent first, or most
    /// played first when `frequent`. An album with no streamable track left
    /// is not listed.
    fn history_albums<'a>(
        &'a self,
        user_id: &'a str,
        frequent: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<Vec<String>, StoreError>>;

    /// The listener's [`PlayedValues`] for some ids of one kind (`album`,
    /// `track`). An item with a play count of 0 is absent.
    fn played_values<'a>(
        &'a self,
        user_id: &'a str,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, PlayedValues>, StoreError>>;

    /// The user's play counts and last plays for some ids of one kind
    /// (`artist`, `album`, `track`).
    fn play_stats<'a>(
        &'a self,
        user_id: &'a str,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, PlayStat>, StoreError>>;

    /// The MusicBrainz ids player apps are sent for some albums and
    /// artists: the accepted match's, else the one their files' tags agree
    /// on. An album's tags are its indexed tracks'; an artist's are the ids
    /// its track and album credits took from their files, which the scan
    /// keeps only where a file pairs artist names and ids one for one, so a
    /// tag holding several ids for one artist row gives none. A value that
    /// is not one well-formed MBID is skipped. Tag ids are for player
    /// answers only, never identity or matching.
    fn player_ids<'a>(
        &'a self,
        album_ids: &'a [String],
        artist_ids: &'a [String],
    ) -> BoxFuture<'a, Result<PlayerIds, StoreError>>;

    /// Every `(kind, id)` a player can address: artists, albums and
    /// streamable tracks by local id, genres by display name.
    fn every_id<'a>(&'a self) -> BoxFuture<'a, Result<Vec<(&'static str, String)>, StoreError>>;

    /// The id an album, artist or track (`kind`) has now: `id` itself while
    /// it is live, else the live one a merge or an alias of any kind points
    /// it at. `None` for an album or artist nothing live holds; a track
    /// answers its own id then, so reads test its availability as usual.
    /// One indexed read.
    fn current_id<'a>(
        &'a self,
        kind: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, StoreError>>;

    /// Every `(kind, id)` an album or artist merge retired, and every track
    /// alias: the old local ids an app may still hold.
    fn merged_ids<'a>(&'a self) -> BoxFuture<'a, Result<Vec<(&'static str, String)>, StoreError>>;

    /// Every year at least one album with a streamable track has,
    /// ascending.
    fn album_years<'a>(&'a self) -> BoxFuture<'a, Result<Vec<i64>, StoreError>>;

    /// For some artist ids, the summed length in seconds and the number of
    /// the streamable tracks crediting each one as track artist or album
    /// artist, each track once. An artist with no such track is absent.
    fn artist_durations<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<ArtistTracks, StoreError>>;
}

const TRACK_SELECT: &str = "SELECT t.id AS id, t.title AS title, t.local_album_id AS album_id, \
    t.album_title AS album_title, COALESCE(t.artist_name, '') AS artist_name, \
    ta.local_artist_id AS artist_id, a.album_artist_id AS album_artist_id, \
    COALESCE(an.display_name, t.album_artist_name, a.album_artist_name) AS album_artist_name, \
    t.disc_number AS disc_number, t.track_number AS track_number, t.year AS year, \
    (SELECT group_concat(name, char(31)) FROM (SELECT g.name AS name FROM local_track_genres g \
     WHERE g.local_track_id = t.id ORDER BY g.position)) AS genres, \
    t.genre AS tag_genre, t.duration_seconds AS duration_seconds, \
    LOWER(t.file_format) AS format, t.bit_rate AS bit_rate, t.sample_rate AS sample_rate, \
    t.bit_depth AS bit_depth, t.channels AS channels, t.file_size_bytes AS file_size_bytes, \
    t.relative_path AS relative_path, \
    t.imported_at AS date_added, t.title_sort AS sort_name, t.disc_subtitle AS disc_subtitle, \
    COALESCE(te.recording_mbid, t.embedded_recording_mbid) AS recording_mbid, \
    tae.provider_artist_id AS artist_mbid, aae.provider_artist_id AS album_artist_mbid, \
    ae.release_group_mbid AS release_group_mbid, t.release_type AS release_type, \
    t.replaygain_track_gain AS rg_track_gain, t.replaygain_album_gain AS rg_album_gain, \
    t.replaygain_track_peak AS rg_track_peak, t.replaygain_album_peak AS rg_album_peak, \
    EXISTS (SELECT 1 FROM local_album_artwork w WHERE w.local_album_id = a.id) \
    AS cover_available, \
    EXISTS (SELECT 1 FROM local_track_lyrics l WHERE l.local_track_id = t.id) AS has_lyrics \
    FROM local_tracks t \
    JOIN local_albums a ON a.id = t.local_album_id \
    LEFT JOIN local_artists an ON an.id = a.album_artist_id \
    LEFT JOIN local_track_artists ta ON ta.local_track_id = t.id AND ta.position = 0 \
    LEFT JOIN local_track_external_identities te ON te.local_track_id = t.id \
    LEFT JOIN local_artist_external_identities tae ON tae.local_artist_id = ta.local_artist_id \
    LEFT JOIN local_artist_external_identities aae ON aae.local_artist_id = a.album_artist_id \
    LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id";

/// Genre match for one track, folded.
const TRACK_GENRE_MATCH: &str = "(EXISTS (SELECT 1 FROM local_track_genres g \
    WHERE g.local_track_id = t.id AND g.folded_name = ?) OR t.genre_folded = ?)";

fn map_track(row: &sqlx::sqlite::SqliteRow) -> Result<PlayerTrack, sqlx::Error> {
    let genres: Option<String> = row.try_get("genres")?;
    let tag_genre: Option<String> = row.try_get("tag_genre")?;
    let genres = match genres.filter(|value| !value.is_empty()) {
        Some(list) => list.split('\u{1f}').map(str::to_owned).collect(),
        None => tag_genre
            .into_iter()
            .filter(|genre| !genre.trim().is_empty())
            .collect(),
    };
    Ok(PlayerTrack {
        id: row.try_get("id")?,
        title: row.try_get("title")?,
        album_id: row.try_get("album_id")?,
        album_title: row.try_get("album_title")?,
        artist_name: row.try_get("artist_name")?,
        artist_id: row.try_get("artist_id")?,
        album_artist_id: row.try_get("album_artist_id")?,
        album_artist_name: row.try_get("album_artist_name")?,
        disc_number: row.try_get("disc_number")?,
        track_number: row.try_get("track_number")?,
        year: row.try_get("year")?,
        genres,
        duration_seconds: row.try_get("duration_seconds")?,
        format: row.try_get("format")?,
        bit_rate: row.try_get("bit_rate")?,
        sample_rate: row.try_get("sample_rate")?,
        bit_depth: row.try_get("bit_depth")?,
        channels: row.try_get("channels")?,
        file_size_bytes: row.try_get("file_size_bytes")?,
        relative_path: row.try_get("relative_path")?,
        date_added: row.try_get("date_added")?,
        sort_name: row.try_get("sort_name")?,
        disc_subtitle: row.try_get("disc_subtitle")?,
        recording_mbid: row.try_get("recording_mbid")?,
        artist_mbid: row.try_get("artist_mbid")?,
        album_artist_mbid: row.try_get("album_artist_mbid")?,
        release_group_mbid: row.try_get("release_group_mbid")?,
        release_type: row.try_get("release_type")?,
        replaygain_track_gain: row.try_get("rg_track_gain")?,
        replaygain_album_gain: row.try_get("rg_album_gain")?,
        replaygain_track_peak: row.try_get("rg_track_peak")?,
        replaygain_album_peak: row.try_get("rg_album_peak")?,
        cover_available: row.try_get("cover_available")?,
        has_lyrics: row.try_get("has_lyrics")?,
    })
}

fn map_album(row: &sqlx::sqlite::SqliteRow) -> Result<PlayerAlbum, sqlx::Error> {
    let track_count: i64 = row.try_get("track_count")?;
    Ok(PlayerAlbum {
        record: AlbumRecord {
            id: row.try_get("id")?,
            title: row.try_get("title")?,
            artist_name: row.try_get("artist_name")?,
            artist_id: row.try_get("artist_id")?,
            release_group_mbid: row.try_get("release_group_mbid")?,
            release_mbid: row.try_get("release_mbid")?,
            artist_mbid: row.try_get("artist_mbid")?,
            linked: row.try_get("linked")?,
            track_count: track_count.max(0) as u64,
            total_duration_seconds: row.try_get("total_duration_seconds")?,
            total_size_bytes: row.try_get("total_size_bytes")?,
            format: row.try_get("format")?,
            year: row.try_get("year")?,
            is_compilation: row.try_get("is_compilation")?,
            cover_available: row.try_get("cover_available")?,
            date_added: row.try_get("date_added")?,
            contribution_id: None,
            contribution_state: None,
        },
        genre: row.try_get("primary_genre")?,
        sort_name: row.try_get("album_sort_name")?,
        original_release_date: row.try_get("original_release_date")?,
        release_date: row.try_get("release_date")?,
    })
}

fn read_error(operation: &str) -> impl Fn(sqlx::Error) -> StoreError + '_ {
    move |error| StoreError::read(operation, error)
}

/// Tag ids of some artists (`{marks}`, bound twice): the id each credit
/// of an indexed track or a live album took from its file, set by the scan
/// only where the file pairs its artist names and ids one for one. The
/// credits lead, so a chunk never walks the tracks.
pub(crate) const ARTIST_TAG_IDS: &str = "SELECT ta.local_artist_id, ta.artist_mbid \
    FROM local_track_artists ta CROSS JOIN local_tracks t \
    WHERE ta.local_artist_id IN ({marks}) AND ta.artist_mbid IS NOT NULL \
    AND t.id = ta.local_track_id AND t.availability = 'indexed' \
    UNION ALL \
    SELECT aa.local_artist_id, aa.artist_mbid \
    FROM local_album_artists aa CROSS JOIN local_albums a \
    WHERE aa.local_artist_id IN ({marks}) AND aa.artist_mbid IS NOT NULL \
    AND a.id = aa.local_album_id AND a.retired_into_album_id IS NULL";

/// Tag ids per local id, kept only while every value agrees.
#[derive(Default)]
struct Agreement(HashMap<String, Option<String>>);

impl Agreement {
    /// Count one tag value for `id`. A value that is not one well-formed
    /// MBID is skipped.
    fn note(&mut self, id: &str, value: Option<&str>) {
        let Some(value) = value.map(str::trim).filter(|value| is_valid_mbid(value)) else {
            return;
        };
        let value = value.to_ascii_lowercase();
        match self.0.get_mut(id) {
            Some(seen) => {
                if seen.as_deref() != Some(value.as_str()) {
                    *seen = None;
                }
            }
            None => {
                self.0.insert(id.to_owned(), Some(value));
            }
        }
    }

    /// The agreed id for `id`, if any.
    fn agreed(&self, id: &str) -> Option<String> {
        self.0.get(id).cloned().flatten()
    }
}

fn placeholders(len: usize) -> String {
    vec!["?"; len].join(", ")
}

/// Folded LIKE pattern for a substring filter.
fn like_pattern(raw: &str) -> String {
    let folded = droppedneedle_kernel::db::fold_text(raw)
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{folded}%")
}

/// A WHERE clause plus its binds, built from optional filters.
#[derive(Default)]
struct Clause {
    sql: Vec<String>,
    binds: Vec<Bind>,
}

enum Bind {
    Text(String),
    Int(i64),
}

impl Clause {
    fn push(&mut self, sql: &str, binds: impl IntoIterator<Item = Bind>) {
        self.sql.push(sql.to_owned());
        self.binds.extend(binds);
    }

    fn render(&self) -> String {
        if self.sql.is_empty() {
            "1 = 1".to_owned()
        } else {
            self.sql.join(" AND ")
        }
    }

    fn bind_scalar<'q, O>(
        &'q self,
        mut query: sqlx::query::QueryScalar<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments<'q>>,
    ) -> sqlx::query::QueryScalar<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments<'q>> {
        for bind in &self.binds {
            query = match bind {
                Bind::Text(value) => query.bind(value.as_str()),
                Bind::Int(value) => query.bind(*value),
            };
        }
        query
    }
}

fn texts(values: &[String]) -> Vec<Bind> {
    values.iter().cloned().map(Bind::Text).collect()
}

/// The WHERE clause for a track query. `row_check` keeps the planner off
/// the availability indexes (unary plus): orders that walk the table or
/// another index (insertion order, year) would otherwise be driven from an
/// availability index, which looks free because nearly every track is
/// streamable, and then sort the whole catalog.
fn track_clause(query: &TrackQuery, row_check: bool, played: Option<&str>) -> Clause {
    let mut clause = Clause::default();
    if row_check {
        clause.push("+t.availability = 'indexed'", []);
    } else {
        clause.push("t.availability = 'indexed'", []);
    }
    if let Some(q) = &query.q {
        // Words of 3+ characters search the FTS index, shorter words LIKE
        // over its candidates; a query with no long word keeps the LIKE
        // substring match.
        if let Some(search) = TextSearch::parse(q) {
            clause.push(&search.sql(), search.binds().into_iter().map(Bind::Text));
            return finish_track_clause(clause, query, played);
        }
        let pattern = like_pattern(q);
        clause.push(
            "(t.title_folded LIKE ? ESCAPE '\\' OR t.artist_name_folded LIKE ? ESCAPE '\\' \
             OR t.album_title_folded LIKE ? ESCAPE '\\')",
            [
                Bind::Text(pattern.clone()),
                Bind::Text(pattern.clone()),
                Bind::Text(pattern),
            ],
        );
    }
    finish_track_clause(clause, query, played)
}

/// The non-text track filters. `played` is the listener's played tracks as
/// a JSON list, read once per request with [`played_tracks_sql`], for the
/// played filter.
fn finish_track_clause(mut clause: Clause, query: &TrackQuery, played: Option<&str>) -> Clause {
    if let Some(title) = &query.title {
        clause.push(
            "t.title_folded = ?",
            [Bind::Text(droppedneedle_kernel::db::fold_text(
                title.trim(),
            ))],
        );
    }
    if let Some(album_id) = &query.album_id {
        clause.push("t.local_album_id = ?", [Bind::Text(album_id.clone())]);
    }
    if !query.artist_ids.is_empty() {
        clause.push(
            &format!(
                "EXISTS (SELECT 1 FROM local_track_artists x WHERE x.local_track_id = t.id \
                 AND x.local_artist_id IN ({}))",
                placeholders(query.artist_ids.len())
            ),
            texts(&query.artist_ids),
        );
    }
    if !query.album_artist_ids.is_empty() {
        clause.push(
            &format!(
                "a.album_artist_id IN ({})",
                placeholders(query.album_artist_ids.len())
            ),
            texts(&query.album_artist_ids),
        );
    }
    if let Some(genre) = &query.genre {
        let folded = droppedneedle_kernel::db::fold_text(genre.trim());
        clause.push(
            TRACK_GENRE_MATCH,
            [Bind::Text(folded.clone()), Bind::Text(folded)],
        );
    }
    if let Some(from) = query.year_from {
        clause.push("t.year >= ?", [Bind::Int(from)]);
    }
    if let Some(to) = query.year_to {
        clause.push("t.year <= ?", [Bind::Int(to)]);
    }
    if let Some(name) = &query.artist_name {
        let folded = droppedneedle_kernel::db::fold_text(name.trim());
        clause.push(
            "(t.artist_name_folded = ? OR t.album_artist_name_folded = ?)",
            [Bind::Text(folded.clone()), Bind::Text(folded)],
        );
    }
    if let (Some(wanted), Some(played)) = (query.played, played) {
        let test = if wanted { "IN" } else { "NOT IN" };
        clause.push(
            &format!("t.id {test} (SELECT value FROM json_each(?))"),
            [Bind::Text(played.to_owned())],
        );
    }
    clause
}

/// Whether the track filters read the album row.
fn track_query_reads_album(query: &TrackQuery) -> bool {
    !query.album_artist_ids.is_empty()
}

/// No filter beyond streamability and the played filter: the total comes
/// from the maintained count and the listener's history.
fn track_query_is_open(query: &TrackQuery) -> bool {
    query.q.is_none()
        && query.title.is_none()
        && query.album_id.is_none()
        && query.artist_ids.is_empty()
        && query.album_artist_ids.is_empty()
        && query.genre.is_none()
        && query.year_from.is_none()
        && query.year_to.is_none()
        && query.artist_name.is_none()
}

/// No filter beyond "has a streamable track".
fn album_query_is_open(query: &AlbumQuery) -> bool {
    query.played.is_none()
        && query.q.is_none()
        && query.artist_id.is_none()
        && query.album_artist_ids.is_empty()
        && query.appears_on.is_empty()
        && query.genre.is_none()
        && query.year_from.is_none()
        && query.year_to.is_none()
}

/// ORDER BY for one catalog key over one row kind (`t` or `a`). The
/// play-history keys are paged by [`history_page`] and never reach this;
/// they read as the id order.
fn keyed_order(
    key: OrderKey,
    descending: bool,
    columns: (&str, &str, &str),
    alias: &str,
) -> String {
    let (added, title, year) = columns;
    let direction = if descending { "DESC" } else { "ASC" };
    let id = format!("{alias}.id");
    let expression = match key {
        OrderKey::Added => added,
        OrderKey::Title => title,
        OrderKey::Year => year,
        OrderKey::LastPlayed | OrderKey::PlayCount => id.as_str(),
    };
    format!("{expression} {direction}, {alias}.id {direction}")
}

/// One listener's play count and last play for `count` items of `kind`.
/// Binds: the listener, then the ids; for albums, both a second time.
///
/// An album's plays are the plays of the tracks on it now, found through
/// the index over listener and track, and the plays filed under the album
/// with no track. Those have a small index of their own (listener, album,
/// time, only rows with an album and no track), and the statement names
/// it: with statistics the planner otherwise walks every play of the
/// listener that names no track, and an upgraded history can hold very
/// many.
pub(super) fn play_stats_sql(kind: &str, count: usize) -> Option<String> {
    let ids = placeholders(count);
    let plays = match kind {
        "artist" => format!(
            "SELECT local_artist_id AS id, played_at FROM library_play_history \
             WHERE user_id = ? AND local_artist_id IN ({ids})"
        ),
        "track" => format!(
            "SELECT local_track_id AS id, played_at FROM library_play_history \
             WHERE user_id = ? AND local_track_id IN ({ids})"
        ),
        "album" => format!(
            "SELECT pt.local_album_id AS id, h.played_at AS played_at \
             FROM local_tracks pt CROSS JOIN library_play_history h \
             WHERE h.user_id = ? AND h.local_track_id = pt.id AND pt.local_album_id IN ({ids}) \
             UNION ALL \
             SELECT h.local_album_id, h.played_at \
             FROM library_play_history h INDEXED BY idx_play_history_user_album_no_track \
             WHERE h.user_id = ? AND h.local_track_id IS NULL AND h.local_album_id IN ({ids})"
        ),
        _ => return None,
    };
    Some(format!(
        "SELECT id, COUNT(*) AS plays, \
         CAST(strftime('%s', MAX(played_at)) AS REAL) AS last_played FROM ({plays}) GROUP BY id"
    ))
}

/// The column of a values row a play-history key orders by.
fn history_column(key: OrderKey) -> Option<&'static str> {
    match key {
        OrderKey::LastPlayed => Some("v.played_at"),
        OrderKey::PlayCount => Some("v.plays"),
        OrderKey::Added | OrderKey::Title | OrderKey::Year => None,
    }
}

/// The listener's marks as `m`, read through their primary key: with
/// statistics over one listener's marks the planner would otherwise walk
/// the table, or reach a mark from its track's index.
const MARKS: &str =
    "library_user_play_marks m INDEXED BY sqlite_autoindex_library_user_play_marks_1";

/// The time of the play `h` in whole seconds. `played_at` is ISO text whose
/// form varies (v2 rows), so it is read as seconds, never compared as
/// text; one SQLite cannot read gives NULL. `unixepoch` gives what
/// `CAST(strftime('%s', ...) AS INTEGER)` gives at a fraction of the cost.
const PLAY_SECONDS: &str = "unixepoch(h.played_at)";

/// Whether the play `h` is later than the unplayed mark `m`, in whole
/// seconds. A time SQLite cannot read is never later.
const AFTER_MARK: &str = "unixepoch(h.played_at) > m.marked_unplayed_at / 1000";

/// The plays, as `h`, through the index over listener, track and time.
const PLAYS: &str = "library_play_history h INDEXED BY idx_play_history_user_track";

/// Which tracks a [`track_values_sql`] statement reads.
#[derive(Clone, Copy)]
pub(super) enum ValueScope {
    /// Every track the listener played or marked. Binds: the listener
    /// twice.
    All,
    /// Some track ids. Binds: the listener, the ids, the listener, the ids.
    Tracks(usize),
    /// The tracks on some album ids. Binds: the ids, the listener, the ids,
    /// the listener.
    Albums(usize),
}

/// The listener's [`PlayedValues`] per track, one row each with a play
/// count above 0: `id`, `plays` and `played_at` (whole seconds).
///
/// The plays are grouped per track from the index over listener, track and
/// time, and each track's mark is then looked up by key, so the cost is one
/// probe per played track. Only a track marked unplayed has its plays read
/// again, from the same index, for those after the mark. A track marked
/// played with no play at all comes from the marks by key.
pub(super) fn track_values_sql(scope: ValueScope) -> String {
    let (plays, marks) = match scope {
        ValueScope::All => (
            format!("FROM {PLAYS} WHERE h.user_id = ? AND h.local_track_id IS NOT NULL"),
            format!("FROM {MARKS} WHERE m.user_id = ?"),
        ),
        ValueScope::Tracks(count) => {
            let ids = placeholders(count);
            (
                format!("FROM {PLAYS} WHERE h.user_id = ? AND h.local_track_id IN ({ids})"),
                format!("FROM {MARKS} WHERE m.user_id = ? AND m.local_track_id IN ({ids})"),
            )
        }
        ValueScope::Albums(count) => {
            let ids = placeholders(count);
            (
                format!(
                    "FROM local_tracks pt CROSS JOIN {PLAYS} \
                     WHERE pt.local_album_id IN ({ids}) AND h.user_id = ? \
                     AND h.local_track_id = pt.id"
                ),
                format!(
                    "FROM local_tracks pt CROSS JOIN {MARKS} \
                     WHERE pt.local_album_id IN ({ids}) AND m.user_id = ? \
                     AND m.local_track_id = pt.id"
                ),
            )
        }
    };
    let after_mark = |aggregate: &str| {
        format!(
            "(SELECT {aggregate} FROM {PLAYS} \
             WHERE h.user_id = p.user AND h.local_track_id = p.id AND {AFTER_MARK})"
        )
    };
    format!(
        "SELECT id, CASE WHEN plays > 0 THEN plays ELSE 1 END AS plays, \
         COALESCE(played_at, marked) AS played_at FROM (\
         SELECT p.id AS id, \
         CASE WHEN m.marked_unplayed_at IS NULL THEN p.plays ELSE {count} END AS plays, \
         CASE WHEN m.marked_unplayed_at IS NULL THEN p.played_at ELSE {latest} END \
         AS played_at, m.marked_played_at / 1000 AS marked \
         FROM (SELECT h.user_id AS user, h.local_track_id AS id, COUNT(*) AS plays, \
         MAX({PLAY_SECONDS}) AS played_at {plays} GROUP BY h.local_track_id) p \
         LEFT JOIN {MARKS} ON m.user_id = p.user AND m.local_track_id = p.id \
         UNION ALL \
         SELECT m.local_track_id, 0, NULL, m.marked_played_at / 1000 {marks} \
         AND m.marked_played_at IS NOT NULL AND NOT EXISTS (SELECT 1 FROM {PLAYS} \
         WHERE h.user_id = m.user_id AND h.local_track_id = m.local_track_id)) \
         WHERE plays > 0 OR marked IS NOT NULL",
        count = after_mark("COUNT(*)"),
        latest = after_mark(&format!("MAX({PLAY_SECONDS})")),
    )
}

/// The ids of the tracks that are played for the listener, the rows of
/// [`track_values_sql`] over every track: those with a play, less those
/// marked unplayed with no play since, and those marked played. Only the
/// marks unplayed read their plays again. Binds: the listener three
/// times.
pub(super) fn played_tracks_sql() -> String {
    format!(
        "SELECT h.local_track_id AS id FROM {PLAYS} \
         WHERE h.user_id = ? AND h.local_track_id IS NOT NULL \
         EXCEPT SELECT m.local_track_id FROM {MARKS} \
         WHERE m.user_id = ? AND m.marked_unplayed_at IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM {PLAYS} WHERE h.user_id = m.user_id \
         AND h.local_track_id = m.local_track_id AND {AFTER_MARK}) \
         UNION SELECT m.local_track_id FROM {MARKS} \
         WHERE m.user_id = ? AND m.marked_played_at IS NOT NULL"
    )
}

/// The ids of the albums that are played for the listener: those with a
/// streamable track, all of whose streamable tracks are played. Binds: the
/// listener three times.
pub(super) fn played_albums_sql() -> String {
    format!(
        "SELECT pt.local_album_id AS id FROM ({}) p CROSS JOIN local_tracks pt \
         WHERE pt.id = p.id AND pt.availability = 'indexed' GROUP BY pt.local_album_id \
         HAVING COUNT(*) = (SELECT COUNT(*) FROM local_tracks c \
         WHERE c.local_album_id = pt.local_album_id AND c.availability = 'indexed')",
        played_tracks_sql()
    )
}

/// The plays filed under an album with no track, per album: `id`, `plays`
/// and `played_at`, from the small index that holds only those. A mark
/// never cuts them off.
fn trackless_plays_sql(albums: Option<usize>) -> String {
    let only = albums.map_or_else(
        || "h.local_album_id IS NOT NULL".to_owned(),
        |count| format!("h.local_album_id IN ({})", placeholders(count)),
    );
    format!(
        "SELECT h.local_album_id AS id, COUNT(*) AS plays, MAX({PLAY_SECONDS}) AS played_at \
         FROM library_play_history h INDEXED BY idx_play_history_user_album_no_track \
         WHERE h.user_id = ? AND h.local_track_id IS NULL AND {only} \
         GROUP BY h.local_album_id"
    )
}

/// The listener's [`PlayedValues`] per album, one row each with a play
/// count above 0: `id`, `plays` and `played_at`. A track counts for the
/// album it is on now, streamable or not. Binds: the listener three times.
pub(super) fn album_values_sql() -> String {
    format!(
        "SELECT id, SUM(plays) AS plays, MAX(played_at) AS played_at FROM (\
         SELECT pt.local_album_id AS id, tv.plays AS plays, tv.played_at AS played_at \
         FROM ({}) tv CROSS JOIN local_tracks pt WHERE pt.id = tv.id \
         UNION ALL {}) GROUP BY id",
        track_values_sql(ValueScope::All),
        trackless_plays_sql(None)
    )
}

/// The listener's [`PlayedValues`] for `count` album ids: `id`, `plays`,
/// `played_at` and `played`. Binds: the ids, the listener, the ids, the
/// listener, then the listener and the ids.
pub(super) fn album_page_values_sql(count: usize) -> String {
    format!(
        "SELECT x.id AS id, SUM(x.plays) AS plays, MAX(x.played_at) AS played_at, \
         SUM(x.indexed) > 0 AND SUM(x.indexed) = (SELECT COUNT(*) FROM local_tracks c \
         WHERE c.local_album_id = x.id AND c.availability = 'indexed') AS played FROM (\
         SELECT pt.local_album_id AS id, tv.plays AS plays, tv.played_at AS played_at, \
         pt.availability = 'indexed' AS indexed \
         FROM ({}) tv CROSS JOIN local_tracks pt WHERE pt.id = tv.id \
         UNION ALL SELECT id, plays, played_at, 0 FROM ({})) x GROUP BY x.id",
        track_values_sql(ValueScope::Albums(count)),
        trackless_plays_sql(Some(count))
    )
}

/// One listener's plays per album, one row each: `id`, `plays` and
/// `played_at`, the latest. Subsonic's recently and most played album
/// lists read them here; marks do not weigh in. Binds: the listener twice.
///
/// A play counts for the album its track is on now, streamable or not, so
/// a track that moved takes its plays with it; a play that names an album
/// and no track counts for that album; a play that names neither counts
/// for none. The album id stored on a play with a track is what it was
/// filed under at the time and is not read.
///
/// Two halves, each from its own index, so the cost is the plays that
/// count for an album, never the listener's whole history. The plays with
/// a track come from the index over listener, track and time alone,
/// grouped per track, then each track's album. The plays with an album and
/// no track come from the small index that holds only those.
pub(super) const ALBUM_PLAYS: &str = "SELECT id, SUM(plays) AS plays, \
     MAX(played_at) AS played_at FROM (\
     SELECT pt.local_album_id AS id, p.plays AS plays, p.played_at AS played_at \
     FROM (SELECT local_track_id, COUNT(*) AS plays, MAX(played_at) AS played_at \
     FROM library_play_history INDEXED BY idx_play_history_user_track \
     WHERE user_id = ? AND local_track_id IS NOT NULL GROUP BY local_track_id) p \
     CROSS JOIN local_tracks pt WHERE pt.id = p.local_track_id \
     UNION ALL \
     SELECT local_album_id, COUNT(*), MAX(played_at) \
     FROM library_play_history INDEXED BY idx_play_history_user_album_no_track \
     WHERE user_id = ? AND local_track_id IS NULL AND local_album_id IS NOT NULL \
     GROUP BY local_album_id) GROUP BY id";

/// The statements of one play-history order, and how many times the
/// played page and the played ids bind the listener.
struct HistoryStatements {
    played: String,
    played_ids: String,
    played_count: String,
    unplayed: String,
    unplayed_listed: String,
    unplayed_count: String,
    listeners: (usize, usize),
}

/// What a play-history order pages over: the listener's values per row as
/// `v` (`id`, `plays`, `played_at`), the ids of the rows that have any,
/// and the catalog rows the filters read.
pub(super) struct HistorySource<'a> {
    values: String,
    played_ids: String,
    /// How many times the values and the played ids bind the listener.
    listeners: (usize, usize),
    /// The tables the filters read, starting with the aliased one.
    rows: &'a str,
    /// The alias of the catalog row.
    alias: &'a str,
}

impl<'a> HistorySource<'a> {
    /// Tracks, read from `rows` as `t`.
    pub(super) fn tracks(rows: &'a str) -> Self {
        Self {
            values: track_values_sql(ValueScope::All),
            played_ids: played_tracks_sql(),
            listeners: (2, 3),
            rows,
            alias: "t",
        }
    }

    /// Albums, as `a`.
    pub(super) fn albums() -> Self {
        Self {
            values: album_values_sql(),
            played_ids: format!(
                "SELECT pt.local_album_id AS id FROM ({}) p CROSS JOIN local_tracks pt \
                 WHERE pt.id = p.id \
                 UNION SELECT h.local_album_id \
                 FROM library_play_history h INDEXED BY idx_play_history_user_album_no_track \
                 WHERE h.user_id = ? AND h.local_track_id IS NULL AND h.local_album_id IS NOT NULL",
                played_tracks_sql()
            ),
            listeners: (3, 4),
            rows: "local_albums a",
            alias: "a",
        }
    }

    /// The ids of the rows with a play count above 0, whatever the
    /// filters: the page reads them once and hands them to the count and
    /// the unplayed half as a JSON list. Binds: the listener.
    pub(super) fn played_ids_sql(&self) -> &str {
        &self.played_ids
    }

    /// One page of the played rows, ordered by `column` of the values; ids
    /// break ties in the same direction. The CROSS JOIN keeps the
    /// listener's values as the outer loop, so this costs by their history
    /// and never by the size of the library. Binds: the listener, the
    /// filters, limit, offset.
    pub(super) fn played_sql(&self, filters: &str, column: &str, descending: bool) -> String {
        let Self {
            values,
            rows,
            alias,
            ..
        } = self;
        let direction = if descending { "DESC" } else { "ASC" };
        format!(
            "SELECT v.id FROM ({values}) v CROSS JOIN {rows} WHERE {alias}.id = v.id \
             AND {filters} ORDER BY {column} {direction}, v.id {direction} LIMIT ? OFFSET ?"
        )
    }

    /// How many rows the played half holds: the played ids that pass
    /// `filters`. Binds: the list of played ids, then the filters.
    pub(super) fn played_count_sql(&self, filters: &str) -> String {
        let Self { rows, alias, .. } = self;
        format!(
            "SELECT COUNT(*) FROM json_each(?) p CROSS JOIN {rows} \
             WHERE {alias}.id = p.value AND {filters}"
        )
    }

    /// The rows with a play count of 0 that pass `filters`: those not in
    /// the played ids, read here as one list built once, or (`listed`)
    /// handed in as a JSON list. Binds: the filters, then the listener or
    /// the list.
    fn unplayed_from(&self, filters: &str, listed: bool) -> String {
        let Self {
            rows,
            alias,
            played_ids,
            ..
        } = self;
        let played = if listed {
            "SELECT value FROM json_each(?)"
        } else {
            played_ids
        };
        format!("FROM {rows} WHERE {filters} AND {alias}.id NOT IN ({played})")
    }

    /// One page of the rows with a play count of 0, in id order: a walk of
    /// an index on the id that stops at the page. Binds: the filters, the
    /// listener or the list (`listed`), limit, offset.
    pub(super) fn unplayed_sql(&self, filters: &str, descending: bool, listed: bool) -> String {
        let alias = self.alias;
        let direction = if descending { "DESC" } else { "ASC" };
        format!(
            "SELECT {alias}.id {} ORDER BY {alias}.id {direction} LIMIT ? OFFSET ?",
            self.unplayed_from(filters, listed)
        )
    }

    /// How many rows the unplayed half holds. Binds: the filters, then the
    /// list of played ids.
    pub(super) fn unplayed_count_sql(&self, filters: &str) -> String {
        format!("SELECT COUNT(*) {}", self.unplayed_from(filters, true))
    }

    /// Every statement of one order by `column`.
    fn statements(&self, filters: &str, column: &str, descending: bool) -> HistoryStatements {
        HistoryStatements {
            played: self.played_sql(filters, column, descending),
            played_ids: self.played_ids_sql().to_owned(),
            played_count: self.played_count_sql(filters),
            unplayed: self.unplayed_sql(filters, descending, false),
            unplayed_listed: self.unplayed_sql(filters, descending, true),
            unplayed_count: self.unplayed_count_sql(filters),
            listeners: self.listeners,
        }
    }
}

/// The albums with a streamable track the listener played most often
/// (latest play breaks ties) or most recently, then by id. Binds: the
/// listener twice, limit, offset.
pub(super) fn history_albums_sql(frequent: bool) -> String {
    let order = if frequent {
        "h.plays DESC, h.played_at DESC"
    } else {
        "h.played_at DESC"
    };
    format!(
        "SELECT a.id FROM ({ALBUM_PLAYS}) h CROSS JOIN local_albums a WHERE a.id = h.id \
         AND EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
         ORDER BY {order}, a.id LIMIT ? OFFSET ?"
    )
}

/// `query` with `before`, the filters' binds and `after` bound, in text
/// order.
fn bind_around<'q, O>(
    mut query: sqlx::query::QueryScalar<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments<'q>>,
    before: &[&'q str],
    clause: &'q Clause,
    after: &[&'q str],
) -> sqlx::query::QueryScalar<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments<'q>> {
    for value in before {
        query = query.bind(*value);
    }
    query = clause.bind_scalar(query);
    for value in after {
        query = query.bind(*value);
    }
    query
}

async fn history_ids(
    pool: &SqlitePool,
    sql: &str,
    (before, clause, after): (&[&str], &Clause, &[&str]),
    limit: u64,
    offset: u64,
) -> Result<Vec<String>, StoreError> {
    bind_around(sqlx::query_scalar(sql), before, clause, after)
        .bind(limit.min(i64::MAX as u64) as i64)
        .bind(offset.min(i64::MAX as u64) as i64)
        .fetch_all(pool)
        .await
        .map_err(read_error("player.history_page"))
}

async fn history_count(
    pool: &SqlitePool,
    sql: &str,
    (before, clause, after): (&[&str], &Clause, &[&str]),
) -> Result<u64, StoreError> {
    let total: i64 = bind_around(sqlx::query_scalar(sql), before, clause, after)
        .fetch_one(pool)
        .await
        .map_err(read_error("player.history_page.count"))?;
    Ok(total.max(0) as u64)
}

/// The played ids of a play-history order as a JSON list.
async fn played_list(
    pool: &SqlitePool,
    statements: &HistoryStatements,
    listener: &str,
) -> Result<String, StoreError> {
    let mut query = sqlx::query_scalar::<_, String>(&statements.played_ids);
    for _ in 0..statements.listeners.1 {
        query = query.bind(listener);
    }
    let ids = query
        .fetch_all(pool)
        .await
        .map_err(read_error("player.history_page.played"))?;
    serde_json::to_string(&ids)
        .map_err(|error| StoreError::Internal(format!("played ids: {error}")))
}

/// One page of ids in a play-history order, in two reads and never one
/// sort of the whole library: what the listener played, in the order of
/// their plays, and everything else by id, read only when the page
/// reaches it. Descending puts the played rows first, ascending last, as
/// one sort with the never played at zero would. The page past the first
/// half needs that half's size: known from a short page, counted only
/// when the page starts beyond it, from the played ids read once for the
/// count and the half after it.
async fn history_page(
    pool: &SqlitePool,
    statements: &HistoryStatements,
    clause: &Clause,
    listener: &str,
    descending: bool,
    limit: u64,
    offset: u64,
) -> Result<Vec<String>, StoreError> {
    let values = vec![listener; statements.listeners.0];
    let ids = vec![listener; statements.listeners.1];
    if descending {
        let mut page = history_ids(
            pool,
            &statements.played,
            (&values, clause, &[]),
            limit,
            offset,
        )
        .await?;
        let got = page.len() as u64;
        if got >= limit {
            return Ok(page);
        }
        let rest = if got > 0 || offset == 0 {
            history_ids(
                pool,
                &statements.unplayed,
                (&[], clause, &ids),
                limit - got,
                0,
            )
            .await?
        } else {
            let list = played_list(pool, statements, listener).await?;
            let played =
                history_count(pool, &statements.played_count, (&[&list], clause, &[])).await?;
            history_ids(
                pool,
                &statements.unplayed_listed,
                (&[], clause, &[&list]),
                limit,
                offset.saturating_sub(played),
            )
            .await?
        };
        page.extend(rest);
        Ok(page)
    } else {
        let mut page = history_ids(
            pool,
            &statements.unplayed,
            (&[], clause, &ids),
            limit,
            offset,
        )
        .await?;
        let got = page.len() as u64;
        if got >= limit {
            return Ok(page);
        }
        let unplayed = if got > 0 || offset == 0 {
            offset + got
        } else {
            let list = played_list(pool, statements, listener).await?;
            history_count(pool, &statements.unplayed_count, (&[], clause, &[&list])).await?
        };
        let rest = history_ids(
            pool,
            &statements.played,
            (&values, clause, &[]),
            limit - got,
            offset.saturating_sub(unplayed),
        )
        .await?;
        page.extend(rest);
        Ok(page)
    }
}

/// Prime modulus of the shuffle keys; above any realistic rowid, so every
/// intermediate product stays inside 64 bits.
const SHUFFLE_PRIME: i64 = 2_147_483_647;

/// A seeded shuffle key for `alias` rows: `h = (rowid * a + b) mod p`, then
/// `(k * h^2 + h) mod p`. The square makes it non-linear, so neighbouring
/// rowids land far apart instead of in a regular lattice; rowid breaks the
/// rare tie. One seed is one fixed order (Navidrome's SEEDEDRAND, without a
/// custom SQL function). The values are integers derived here, never caller
/// text.
fn shuffle_order(alias: &str, seed: u32) -> String {
    let seed = i64::from(seed);
    let a = seed % (SHUFFLE_PRIME - 1) + 1;
    let b = (seed * 7919) % SHUFFLE_PRIME;
    let k = (seed * 104_729) % (SHUFFLE_PRIME - 1) + 1;
    let h = format!("(({alias}.rowid * {a} + {b}) % {SHUFFLE_PRIME})");
    format!("((({h} * {h}) % {SHUFFLE_PRIME}) * {k} + {h}) % {SHUFFLE_PRIME}, {alias}.rowid")
}

/// Seeds for paged random lists, one per caller, kind and filter. The first
/// page (offset 0) draws a new seed and later pages reuse it, so a client
/// paging through "random" sees one shuffle with no repeats or gaps, and a
/// random track list never reshuffles a random album list. Seeds idle for
/// an hour are dropped once the map grows.
#[derive(Debug, Default)]
pub struct ShuffleSeeds {
    seeds: Mutex<HashMap<SeedKey, (u32, std::time::Instant)>>,
}

/// Caller, list kind (`track`/`album`) and a hash of the filter.
type SeedKey = (String, &'static str, u64);

impl ShuffleSeeds {
    /// Most lists remembered; past this the idle ones go, then the oldest.
    const CAP: usize = 1024;
    /// Idle time after which a seed may be pruned.
    const IDLE: std::time::Duration = std::time::Duration::from_secs(3600);

    /// The seed for a random track list at `offset`.
    pub fn tracks(&self, caller: &str, query: &TrackQuery, offset: u64) -> u32 {
        self.seed(caller, "track", &format!("{query:?}"), offset)
    }

    /// The seed for a random album list at `offset`.
    pub fn albums(&self, caller: &str, query: &AlbumQuery, offset: u64) -> u32 {
        self.seed(caller, "album", &format!("{query:?}"), offset)
    }

    fn seed(&self, caller: &str, kind: &'static str, filter: &str, offset: u64) -> u32 {
        use std::hash::{Hash as _, Hasher as _};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        filter.hash(&mut hasher);
        let key = (caller.to_owned(), kind, hasher.finish());
        let now = std::time::Instant::now();
        let mut seeds = self
            .seeds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !seeds.contains_key(&key) && seeds.len() >= Self::CAP {
            seeds.retain(|_, (_, used)| now.duration_since(*used) < Self::IDLE);
            while seeds.len() >= Self::CAP {
                let oldest = seeds
                    .iter()
                    .min_by_key(|(_, (_, used))| *used)
                    .map(|(key, _)| key.clone());
                match oldest {
                    Some(oldest) => seeds.remove(&oldest),
                    None => break,
                };
            }
        }
        let fresh = || uuid::Uuid::new_v4().as_u128() as u32;
        let entry = seeds.entry(key).or_insert_with(|| (fresh(), now));
        if offset == 0 {
            entry.0 = fresh();
        }
        entry.1 = now;
        entry.0
    }
}

#[cfg(test)]
mod shuffle_seed_tests {
    use super::*;

    #[test]
    fn seed_map_stays_capped() {
        let seeds = ShuffleSeeds::default();
        for caller in 0..ShuffleSeeds::CAP + 10 {
            seeds.tracks(&caller.to_string(), &TrackQuery::default(), 0);
        }
        let held = seeds
            .seeds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        assert_eq!(held, ShuffleSeeds::CAP);
    }
}

fn track_order_sql(order: TrackOrder) -> String {
    match order {
        TrackOrder::Album => "a.album_artist_name_folded ASC, a.title_folded ASC, a.id ASC, \
             t.disc_number ASC, t.track_number ASC, t.id ASC"
            .to_owned(),
        TrackOrder::Disc => "t.disc_number ASC, t.track_number ASC, t.id ASC".to_owned(),
        TrackOrder::Title => "t.title_folded ASC, t.id ASC".to_owned(),
        TrackOrder::Newest => "t.imported_at DESC, t.id ASC".to_owned(),
        TrackOrder::Random => "RANDOM()".to_owned(),
        TrackOrder::Shuffle(seed) => shuffle_order("t", seed),
        TrackOrder::Natural => "t.rowid ASC".to_owned(),
        TrackOrder::By(key, descending) => keyed_order(
            key,
            descending,
            ("t.imported_at", "t.title_folded", "t.year"),
            "t",
        ),
    }
}

/// The WHERE clause for an album query. `played` is the listener's played
/// albums as a JSON list, read once per request with [`played_albums_sql`],
/// for the played filter.
fn album_clause(query: &AlbumQuery, played: Option<&str>) -> Clause {
    let mut clause = Clause::default();
    // The unary plus keeps the planner off the retired index, so the
    // ordered pages walk the title and created-at indexes and stop early.
    clause.push(
        "+a.retired_into_album_id IS NULL AND EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.local_album_id = a.id AND t.availability = 'indexed')",
        [],
    );
    if let Some(q) = &query.q {
        let pattern = like_pattern(q);
        clause.push(
            "(a.title_folded LIKE ? ESCAPE '\\' OR a.album_artist_name_folded LIKE ? ESCAPE '\\')",
            [Bind::Text(pattern.clone()), Bind::Text(pattern)],
        );
    }
    if let Some(artist_id) = &query.artist_id {
        clause.push("a.album_artist_id = ?", [Bind::Text(artist_id.clone())]);
    }
    if !query.album_artist_ids.is_empty() {
        clause.push(
            &format!(
                "a.album_artist_id IN ({})",
                placeholders(query.album_artist_ids.len())
            ),
            texts(&query.album_artist_ids),
        );
    }
    if !query.appears_on.is_empty() {
        let marks = placeholders(query.appears_on.len());
        clause.push(
            &format!(
                "a.album_artist_id NOT IN ({marks}) AND EXISTS (SELECT 1 FROM local_tracks t \
                 JOIN local_track_artists x ON x.local_track_id = t.id \
                 WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
                 AND x.local_artist_id IN ({marks}))"
            ),
            texts(&query.appears_on)
                .into_iter()
                .chain(texts(&query.appears_on)),
        );
    }
    if let Some(genre) = &query.genre {
        let folded = droppedneedle_kernel::db::fold_text(genre.trim());
        clause.push(
            &format!(
                "EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
                 AND t.availability = 'indexed' AND {TRACK_GENRE_MATCH})"
            ),
            [Bind::Text(folded.clone()), Bind::Text(folded)],
        );
    }
    if let Some(from) = query.year_from {
        clause.push("a.year >= ?", [Bind::Int(from)]);
    }
    if let Some(to) = query.year_to {
        clause.push("a.year <= ?", [Bind::Int(to)]);
    }
    if let (Some(wanted), Some(played)) = (query.played, played) {
        let test = if wanted { "IN" } else { "NOT IN" };
        clause.push(
            &format!("a.id {test} (SELECT value FROM json_each(?))"),
            [Bind::Text(played.to_owned())],
        );
    }
    clause
}

fn album_order_sql(order: AlbumOrder) -> String {
    match order {
        AlbumOrder::Newest => "a.created_at DESC, a.id ASC".to_owned(),
        AlbumOrder::Title => "a.title_folded ASC, a.id ASC".to_owned(),
        AlbumOrder::Artist => {
            "a.album_artist_name_folded ASC, a.title_folded ASC, a.id ASC".to_owned()
        }
        AlbumOrder::YearAsc => {
            "(a.year IS NULL) ASC, a.year ASC, a.title_folded ASC, a.id ASC".to_owned()
        }
        AlbumOrder::YearDesc => {
            "(a.year IS NULL) ASC, a.year DESC, a.title_folded ASC, a.id ASC".to_owned()
        }
        AlbumOrder::Shuffle(seed) => shuffle_order("a", seed),
        AlbumOrder::Natural => "a.rowid ASC".to_owned(),
        AlbumOrder::By(key, descending) => keyed_order(
            key,
            descending,
            ("a.created_at", "a.title_folded", "a.year"),
            "a",
        ),
    }
}

/// Album extras appended to the shared album columns. The release date is
/// kept only when every indexed track has the same one, as Navidrome does.
const ALBUM_EXTRAS: &str = "a.primary_genre AS primary_genre, \
    a.album_artist_sort_name AS album_sort_name, \
    a.original_release_date AS original_release_date, \
    CASE WHEN COUNT(h.release_date) = COUNT(t.id) \
     AND MIN(h.release_date) = MAX(h.release_date) \
     THEN MIN(h.release_date) END AS release_date";

/// The edition hints of the tracks `ALBUM_JOINS` joins, at most one row
/// each, for the release date in `ALBUM_EXTRAS`.
const ALBUM_EXTRA_JOINS: &str = "LEFT JOIN local_track_edition_tags h ON h.local_track_id = t.id";

/// Player reads over the reader pool.
#[derive(Clone, Debug)]
pub struct SqlitePlayerCatalog {
    db: LibraryDb,
    /// Browseable album total at one catalog revision: counting it probes
    /// every album, and the open album list asks for it on every page.
    album_total: Arc<Mutex<Option<(i64, u64)>>>,
}

impl SqlitePlayerCatalog {
    /// Adapter over one handle.
    pub fn new(db: &LibraryDb) -> Self {
        Self {
            db: db.clone(),
            album_total: Arc::new(Mutex::new(None)),
        }
    }

    async fn tracks_in(
        pool: &SqlitePool,
        ids: &[String],
    ) -> Result<HashMap<String, PlayerTrack>, StoreError> {
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let sql = format!(
                "{TRACK_SELECT} WHERE t.availability = 'indexed' AND t.id IN ({})",
                placeholders(chunk.len())
            );
            let mut query = sqlx::query(&sql);
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(pool)
                .await
                .map_err(read_error("player.tracks_in"))?;
            for row in &rows {
                let track = map_track(row).map_err(read_error("player.tracks_in"))?;
                out.insert(track.id.clone(), track);
            }
        }
        Ok(out)
    }

    /// The open album total, recounted only when the catalog changed.
    async fn open_album_total(
        &self,
        pool: &SqlitePool,
        count_sql: &str,
    ) -> Result<u64, StoreError> {
        let revision = self.revision().await?;
        let cached = *self
            .album_total
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((at, total)) = cached
            && at == revision
        {
            return Ok(total);
        }
        let total: i64 = sqlx::query_scalar(count_sql)
            .fetch_one(pool)
            .await
            .map_err(read_error("player.albums.count"))?;
        let total = total.max(0) as u64;
        *self
            .album_total
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((revision, total));
        Ok(total)
    }

    /// The listener's played tracks as a JSON list, when `query` filters by
    /// them: one read per request, for the page and its total.
    async fn played_tracks(
        pool: &SqlitePool,
        query: &TrackQuery,
    ) -> Result<Option<String>, StoreError> {
        if query.played.is_none() {
            return Ok(None);
        }
        let listener = query.listener.as_deref().unwrap_or_default();
        let ids: Vec<String> = sqlx::query_scalar(&played_tracks_sql())
            .bind(listener)
            .bind(listener)
            .bind(listener)
            .fetch_all(pool)
            .await
            .map_err(read_error("player.tracks.played"))?;
        serde_json::to_string(&ids)
            .map(Some)
            .map_err(|error| StoreError::Internal(format!("played tracks: {error}")))
    }

    /// One page of track ids in order, or `None` when the text search
    /// matches nothing at all (the miss oracle).
    async fn track_ids(
        pool: &SqlitePool,
        query: &TrackQuery,
        played: Option<&str>,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> Result<Option<Vec<String>>, StoreError> {
        // Miss oracle: a query no track text contains matches nothing,
        // whatever the other filters say.
        if let Some(search) = query.q.as_deref().and_then(TextSearch::parse) {
            let hit: bool = sqlx::query_scalar(TRACK_MISS_ORACLE)
                .bind(search.fts)
                .fetch_one(pool)
                .await
                .map_err(read_error("player.tracks.search"))?;
            if !hit {
                return Ok(None);
            }
        }
        let row_check = matches!(
            order,
            TrackOrder::Natural
                | TrackOrder::By(
                    OrderKey::Year | OrderKey::LastPlayed | OrderKey::PlayCount,
                    _
                )
        );
        let clause = track_clause(query, row_check, played);
        let page_from = if track_query_reads_album(query) || order == TrackOrder::Album {
            "local_tracks t JOIN local_albums a ON a.id = t.local_album_id"
        } else {
            "local_tracks t"
        };
        if let TrackOrder::By(key, descending) = order
            && let Some(column) = history_column(key)
        {
            let statements =
                HistorySource::tracks(page_from).statements(&clause.render(), column, descending);
            let listener = query.listener.as_deref().unwrap_or_default();
            return history_page(
                pool,
                &statements,
                &clause,
                listener,
                descending,
                limit,
                offset,
            )
            .await
            .map(Some);
        }
        let order = track_order_sql(order);
        // Page the ids over the narrow join; the rich rows are read for the
        // page alone.
        let sql = format!(
            "SELECT t.id FROM {page_from} WHERE {} ORDER BY {} LIMIT ? OFFSET ?",
            clause.render(),
            order
        );
        clause
            .bind_scalar(sqlx::query_scalar(&sql))
            .bind(limit.min(i64::MAX as u64) as i64)
            .bind(offset.min(i64::MAX as u64) as i64)
            .fetch_all(pool)
            .await
            .map(Some)
            .map_err(read_error("player.tracks"))
    }

    /// The rich rows for `ids`, in `ids` order.
    async fn hydrate_tracks(
        pool: &SqlitePool,
        ids: &[String],
    ) -> Result<Vec<PlayerTrack>, StoreError> {
        let mut by_id = Self::tracks_in(pool, ids).await?;
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// How many tracks match `query`.
    async fn track_total(
        pool: &SqlitePool,
        query: &TrackQuery,
        played: Option<&str>,
    ) -> Result<u64, StoreError> {
        let total: i64 = if track_query_is_open(query) {
            // Maintained exact count of indexed tracks (migration 0005).
            let all: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(indexed_tracks), 0) FROM library_track_format_stats",
            )
            .fetch_one(pool)
            .await
            .map_err(read_error("player.tracks.count"))?;
            match (query.played, played) {
                // Counted from the listener's played tracks, not across the
                // library: one probe per played track.
                (Some(wanted), Some(played)) => {
                    let played: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM json_each(?) p CROSS JOIN local_tracks t \
                         WHERE t.id = p.value AND +t.availability = 'indexed'",
                    )
                    .bind(played)
                    .fetch_one(pool)
                    .await
                    .map_err(read_error("player.tracks.count"))?;
                    if wanted { played } else { all - played }
                }
                _ => all,
            }
        } else {
            let clause = track_clause(query, false, played);
            let from = if track_query_reads_album(query) {
                "local_tracks t JOIN local_albums a ON a.id = t.local_album_id"
            } else {
                "local_tracks t"
            };
            let count_sql = format!("SELECT COUNT(*) FROM {from} WHERE {}", clause.render());
            clause
                .bind_scalar(sqlx::query_scalar(&count_sql))
                .fetch_one(pool)
                .await
                .map_err(read_error("player.tracks.count"))?
        };
        Ok(total.max(0) as u64)
    }

    fn pool(&self) -> Result<&SqlitePool, StoreError> {
        self.db
            .live()
            .ok_or_else(|| StoreError::Internal("player catalog is not wired".to_owned()))
    }

    /// The listener's played albums as a JSON list, and how many there
    /// are, when `query` filters by them: one read per request, for the
    /// page and its total.
    async fn played_albums(
        pool: &SqlitePool,
        query: &AlbumQuery,
    ) -> Result<Option<(String, u64)>, StoreError> {
        if query.played.is_none() {
            return Ok(None);
        }
        let listener = query.listener.as_deref().unwrap_or_default();
        let ids: Vec<String> = sqlx::query_scalar(&played_albums_sql())
            .bind(listener)
            .bind(listener)
            .bind(listener)
            .fetch_all(pool)
            .await
            .map_err(read_error("player.albums.played"))?;
        let json = serde_json::to_string(&ids)
            .map_err(|error| StoreError::Internal(format!("played albums: {error}")))?;
        Ok(Some((json, ids.len() as u64)))
    }

    /// One page of albums, `played` being what [`Self::played_albums`]
    /// read.
    async fn album_page_in(
        &self,
        pool: &SqlitePool,
        query: &AlbumQuery,
        played: Option<&str>,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PlayerAlbum>, StoreError> {
        let clause = album_clause(query, played);
        let ids: Vec<String> = if let AlbumOrder::By(key, descending) = order
            && let Some(column) = history_column(key)
        {
            let statements =
                HistorySource::albums().statements(&clause.render(), column, descending);
            let listener = query.listener.as_deref().unwrap_or_default();
            history_page(
                pool,
                &statements,
                &clause,
                listener,
                descending,
                limit,
                offset,
            )
            .await?
        } else {
            let sql = format!(
                "SELECT a.id FROM local_albums a WHERE {} ORDER BY {} LIMIT ? OFFSET ?",
                clause.render(),
                album_order_sql(order)
            );
            clause
                .bind_scalar(sqlx::query_scalar(&sql))
                .bind(limit.min(i64::MAX as u64) as i64)
                .bind(offset.min(i64::MAX as u64) as i64)
                .fetch_all(pool)
                .await
                .map_err(read_error("player.albums.page"))?
        };
        let mut by_id = Self::albums_in(pool, &ids).await?;
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    async fn albums_in(
        pool: &SqlitePool,
        ids: &[String],
    ) -> Result<HashMap<String, PlayerAlbum>, StoreError> {
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let sql = format!(
                "SELECT {ALBUM_COLUMNS}, {ALBUM_EXTRAS} {ALBUM_JOINS} {ALBUM_EXTRA_JOINS} \
                 WHERE a.id IN ({}) GROUP BY a.id",
                placeholders(chunk.len())
            );
            let mut query = sqlx::query(&sql);
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(pool)
                .await
                .map_err(read_error("player.albums"))?;
            for row in &rows {
                let album = map_album(row).map_err(read_error("player.albums"))?;
                out.insert(album.record.id.clone(), album);
            }
        }
        Ok(out)
    }
}

impl PlayerCatalog for SqlitePlayerCatalog {
    fn tracks_by_ids<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<PlayerTrack>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            Ok(Self::tracks_in(pool, ids).await?.into_values().collect())
        })
    }

    fn tracks<'a>(
        &'a self,
        query: &'a TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<PlayerTrack>, u64), StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let played = Self::played_tracks(pool, query).await?;
            let played = played.as_deref();
            let Some(ids) = Self::track_ids(pool, query, played, order, limit, offset).await?
            else {
                return Ok((Vec::new(), 0));
            };
            let tracks = Self::hydrate_tracks(pool, &ids).await?;
            Ok((tracks, Self::track_total(pool, query, played).await?))
        })
    }

    fn track_page<'a>(
        &'a self,
        query: &'a TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<Vec<PlayerTrack>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let played = Self::played_tracks(pool, query).await?;
            match Self::track_ids(pool, query, played.as_deref(), order, limit, offset).await? {
                Some(ids) => Self::hydrate_tracks(pool, &ids).await,
                None => Ok(Vec::new()),
            }
        })
    }

    fn albums_by_ids<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<PlayerAlbum>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            Ok(Self::albums_in(pool, ids).await?.into_values().collect())
        })
    }

    fn albums<'a>(
        &'a self,
        query: &'a AlbumQuery,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<PlayerAlbum>, u64), StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let played = Self::played_albums(pool, query).await?;
            let json = played.as_ref().map(|(json, _)| json.as_str());
            let albums = self
                .album_page_in(pool, query, json, order, limit, offset)
                .await?;
            let clause = album_clause(query, json);
            let count_sql = format!(
                "SELECT COUNT(*) FROM local_albums a WHERE {}",
                clause.render()
            );
            let unfiltered = AlbumQuery {
                played: None,
                ..query.clone()
            };
            let total = if album_query_is_open(query) {
                self.open_album_total(pool, &count_sql).await?
            } else if let (Some(wanted), Some((_, count))) = (query.played, &played)
                && album_query_is_open(&unfiltered)
            {
                // Every played album has a streamable track, so the rest of
                // the open total is unplayed.
                if wanted {
                    *count
                } else {
                    let open_sql = format!(
                        "SELECT COUNT(*) FROM local_albums a WHERE {}",
                        album_clause(&unfiltered, None).render()
                    );
                    self.open_album_total(pool, &open_sql)
                        .await?
                        .saturating_sub(*count)
                }
            } else {
                let total: i64 = clause
                    .bind_scalar(sqlx::query_scalar(&count_sql))
                    .fetch_one(pool)
                    .await
                    .map_err(read_error("player.albums.count"))?;
                total.max(0) as u64
            };
            Ok((albums, total))
        })
    }

    fn album_page<'a>(
        &'a self,
        query: &'a AlbumQuery,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<Vec<PlayerAlbum>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let played = Self::played_albums(pool, query).await?;
            let json = played.as_ref().map(|(json, _)| json.as_str());
            self.album_page_in(pool, query, json, order, limit, offset)
                .await
        })
    }

    fn existing<'a>(
        &'a self,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let (table, extra) = match kind {
                "artist" => ("local_artists", "retired_into_artist_id IS NULL"),
                "album" => ("local_albums", "retired_into_album_id IS NULL"),
                "track" => ("local_tracks", "availability = 'indexed'"),
                _ => return Ok(HashSet::new()),
            };
            let mut out = HashSet::new();
            for chunk in ids.chunks(500) {
                let sql = format!(
                    "SELECT id FROM {table} WHERE {extra} AND id IN ({})",
                    placeholders(chunk.len())
                );
                let mut query = sqlx::query_scalar::<_, String>(&sql);
                for id in chunk {
                    query = query.bind(id);
                }
                out.extend(
                    query
                        .fetch_all(pool)
                        .await
                        .map_err(read_error("player.existing"))?,
                );
            }
            Ok(out)
        })
    }

    fn revision<'a>(&'a self) -> BoxFuture<'a, Result<i64, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let value: Option<i64> = sqlx::query_scalar(
                "SELECT value FROM library_catalog_revision WHERE singleton = 1",
            )
            .fetch_optional(pool)
            .await
            .map_err(read_error("player.revision"))?;
            Ok(value.unwrap_or(0))
        })
    }

    fn history_albums<'a>(
        &'a self,
        user_id: &'a str,
        frequent: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<Vec<String>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let sql = history_albums_sql(frequent);
            sqlx::query_scalar(&sql)
                .bind(user_id)
                .bind(user_id)
                .bind(limit.min(i64::MAX as u64) as i64)
                .bind(offset.min(i64::MAX as u64) as i64)
                .fetch_all(pool)
                .await
                .map_err(read_error("player.history_albums"))
        })
    }

    fn played_values<'a>(
        &'a self,
        user_id: &'a str,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, PlayedValues>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let mut out = HashMap::new();
            for chunk in ids.chunks(250) {
                let (sql, binds): (String, Vec<&str>) = match kind {
                    "track" => {
                        let half = std::iter::once(user_id).chain(chunk.iter().map(String::as_str));
                        (
                            track_values_sql(ValueScope::Tracks(chunk.len())),
                            half.clone().chain(half).collect(),
                        )
                    }
                    "album" => {
                        let ids = chunk.iter().map(String::as_str);
                        let half = ids.clone().chain(std::iter::once(user_id));
                        (
                            album_page_values_sql(chunk.len()),
                            half.clone()
                                .chain(half)
                                .chain(std::iter::once(user_id))
                                .chain(ids)
                                .collect(),
                        )
                    }
                    _ => return Ok(HashMap::new()),
                };
                let mut query = sqlx::query(&sql);
                for bind in binds {
                    query = query.bind(bind);
                }
                let rows = query
                    .fetch_all(pool)
                    .await
                    .map_err(read_error("player.played_values"))?;
                for row in &rows {
                    let read = || -> Result<(String, PlayedValues), sqlx::Error> {
                        let plays: i64 = row.try_get("plays")?;
                        let last: Option<i64> = row.try_get("played_at")?;
                        let played = if kind == "album" {
                            row.try_get("played")?
                        } else {
                            plays > 0
                        };
                        Ok((
                            row.try_get("id")?,
                            PlayedValues {
                                plays: plays.max(0) as u64,
                                last_played: last.map(|at| at as f64),
                                played,
                            },
                        ))
                    };
                    let (id, values) = read().map_err(read_error("player.played_values"))?;
                    out.insert(id, values);
                }
            }
            Ok(out)
        })
    }

    fn play_stats<'a>(
        &'a self,
        user_id: &'a str,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, PlayStat>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let mut out = HashMap::new();
            for chunk in ids.chunks(500) {
                let Some(sql) = play_stats_sql(kind, chunk.len()) else {
                    return Ok(HashMap::new());
                };
                let mut query = sqlx::query(&sql);
                // An album's plays are read in two parts, each with its
                // own listener and ids.
                for _ in 0..if kind == "album" { 2 } else { 1 } {
                    query = query.bind(user_id);
                    for id in chunk {
                        query = query.bind(id);
                    }
                }
                let rows = query
                    .fetch_all(pool)
                    .await
                    .map_err(read_error("player.play_stats"))?;
                for row in &rows {
                    let id: String = row.try_get("id").map_err(read_error("player.play_stats"))?;
                    let plays: i64 = row
                        .try_get("plays")
                        .map_err(read_error("player.play_stats"))?;
                    let last: Option<f64> = row
                        .try_get("last_played")
                        .map_err(read_error("player.play_stats"))?;
                    out.insert(id, (plays.max(0) as u64, last));
                }
            }
            Ok(out)
        })
    }

    fn player_ids<'a>(
        &'a self,
        album_ids: &'a [String],
        artist_ids: &'a [String],
    ) -> BoxFuture<'a, Result<PlayerIds, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let mut out = PlayerIds::default();
            for chunk in album_ids.chunks(500) {
                // An accepted album is one row with no tracks joined.
                let sql = format!(
                    "SELECT a.id, ae.release_mbid, ae.release_group_mbid, \
                     t.embedded_release_mbid, t.embedded_release_group_mbid \
                     FROM local_albums a \
                     LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id \
                     LEFT JOIN local_tracks t ON ae.local_album_id IS NULL \
                     AND t.local_album_id = a.id AND t.availability = 'indexed' \
                     WHERE a.id IN ({})",
                    placeholders(chunk.len())
                );
                let mut query = sqlx::query_as::<
                    _,
                    (
                        String,
                        Option<String>,
                        Option<String>,
                        Option<String>,
                        Option<String>,
                    ),
                >(&sql);
                for id in chunk {
                    query = query.bind(id);
                }
                let rows = query
                    .fetch_all(pool)
                    .await
                    .map_err(read_error("player.player_ids"))?;
                let (mut releases, mut groups) = (Agreement::default(), Agreement::default());
                for (id, release, group, tag_release, tag_group) in rows {
                    if group.is_some() {
                        out.albums.insert(
                            id,
                            AlbumIds {
                                accepted: true,
                                release_mbid: release,
                                release_group_mbid: group,
                            },
                        );
                    } else {
                        releases.note(&id, tag_release.as_deref());
                        groups.note(&id, tag_group.as_deref());
                    }
                }
                for id in chunk {
                    let (release, group) = (releases.agreed(id), groups.agreed(id));
                    if release.is_some() || group.is_some() {
                        out.albums.insert(
                            id.clone(),
                            AlbumIds {
                                accepted: false,
                                release_mbid: release,
                                release_group_mbid: group,
                            },
                        );
                    }
                }
            }
            for chunk in artist_ids.chunks(500) {
                let sql = format!(
                    "SELECT local_artist_id, provider_artist_id \
                     FROM local_artist_external_identities WHERE local_artist_id IN ({})",
                    placeholders(chunk.len())
                );
                let mut query = sqlx::query_as::<_, (String, String)>(&sql);
                for id in chunk {
                    query = query.bind(id);
                }
                let accepted = query
                    .fetch_all(pool)
                    .await
                    .map_err(read_error("player.player_ids"))?;
                out.artists.extend(accepted);
                let unmatched: Vec<&String> = chunk
                    .iter()
                    .filter(|id| !out.artists.contains_key(*id))
                    .collect();
                if unmatched.is_empty() {
                    continue;
                }
                let sql = ARTIST_TAG_IDS.replace("{marks}", &placeholders(unmatched.len()));
                let mut query = sqlx::query_as::<_, (String, String)>(&sql);
                for id in unmatched.iter().chain(&unmatched) {
                    query = query.bind(*id);
                }
                let rows = query
                    .fetch_all(pool)
                    .await
                    .map_err(read_error("player.player_ids"))?;
                let mut tags = Agreement::default();
                for (id, value) in &rows {
                    tags.note(id, Some(value));
                }
                for id in unmatched {
                    if let Some(mbid) = tags.agreed(id) {
                        out.artists.insert(id.clone(), mbid);
                    }
                }
            }
            Ok(out)
        })
    }

    fn album_artist_page<'a>(
        &'a self,
        q: Option<&'a str>,
        natural: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<Vec<ArtistRecord>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let order = if natural {
                "r.rowid ASC"
            } else {
                "r.folded_name ASC, r.id ASC"
            };
            let sql = format!(
                "SELECT r.id FROM local_artists r \
                 WHERE r.retired_into_artist_id IS NULL AND {LED_PREDICATE} \
                 AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\') \
                 ORDER BY {order} LIMIT ? OFFSET ?"
            );
            let pattern = q.map(like_pattern);
            let ids: Vec<String> = sqlx::query_scalar(&sql)
                .bind(pattern.as_deref())
                .bind(pattern.as_deref())
                .bind(limit.min(i64::MAX as u64) as i64)
                .bind(offset.min(i64::MAX as u64) as i64)
                .fetch_all(pool)
                .await
                .map_err(read_error("player.album_artists"))?;
            hydrate_artists(pool, &ids).await
        })
    }

    fn every_id<'a>(&'a self) -> BoxFuture<'a, Result<Vec<(&'static str, String)>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let mut out = Vec::new();
            for (kind, sql) in [
                (
                    "artist",
                    "SELECT id FROM local_artists WHERE retired_into_artist_id IS NULL",
                ),
                (
                    "album",
                    "SELECT id FROM local_albums WHERE retired_into_album_id IS NULL",
                ),
                (
                    "track",
                    "SELECT id FROM local_tracks WHERE availability = 'indexed'",
                ),
                (
                    "genre",
                    "SELECT name FROM local_track_genres UNION \
                     SELECT genre FROM local_tracks WHERE genre IS NOT NULL",
                ),
            ] {
                let ids: Vec<String> = sqlx::query_scalar(sql)
                    .fetch_all(pool)
                    .await
                    .map_err(read_error("player.every_id"))?;
                out.extend(ids.into_iter().map(|id| (kind, id)));
            }
            Ok(out)
        })
    }

    fn current_id<'a>(
        &'a self,
        kind: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, StoreError>> {
        Box::pin(async move { super::lookups::current_id(self.pool()?, kind, id).await })
    }

    fn merged_ids<'a>(&'a self) -> BoxFuture<'a, Result<Vec<(&'static str, String)>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let mut out = Vec::new();
            for (kind, sql) in [
                (
                    "album",
                    "SELECT alias FROM local_album_aliases WHERE kind = 'merged_album'",
                ),
                (
                    "artist",
                    "SELECT alias FROM local_artist_aliases WHERE kind = 'merged_artist'",
                ),
                ("track", "SELECT alias FROM local_track_aliases"),
            ] {
                let ids: Vec<String> = sqlx::query_scalar(sql)
                    .fetch_all(pool)
                    .await
                    .map_err(read_error("player.merged_ids"))?;
                out.extend(ids.into_iter().map(|id| (kind, id)));
            }
            Ok(out)
        })
    }

    fn album_years<'a>(&'a self) -> BoxFuture<'a, Result<Vec<i64>, StoreError>> {
        Box::pin(async move {
            sqlx::query_scalar(
                "SELECT DISTINCT a.year FROM local_albums a \
                 WHERE a.year IS NOT NULL AND a.retired_into_album_id IS NULL \
                 AND EXISTS (SELECT 1 FROM local_tracks t \
                 WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
                 ORDER BY a.year",
            )
            .fetch_all(self.pool()?)
            .await
            .map_err(read_error("player.album_years"))
        })
    }

    fn artist_durations<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<ArtistTracks, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            let mut out = HashMap::with_capacity(ids.len());
            for chunk in ids.chunks(500) {
                let marks = placeholders(chunk.len());
                // UNION keeps one row per (artist, track), so a track that
                // credits its album artist is counted once.
                let sql = format!(
                    "SELECT c.artist_id, COALESCE(SUM(t.duration_seconds), 0.0), COUNT(*) \
                     FROM (SELECT ta.local_artist_id AS artist_id, ta.local_track_id AS track_id \
                     FROM local_track_artists ta WHERE ta.local_artist_id IN ({marks}) \
                     UNION \
                     SELECT a.album_artist_id, lt.id FROM local_albums a \
                     JOIN local_tracks lt ON lt.local_album_id = a.id \
                     WHERE a.album_artist_id IN ({marks})) c \
                     JOIN local_tracks t ON t.id = c.track_id AND t.availability = 'indexed' \
                     JOIN local_albums a ON a.id = t.local_album_id \
                     AND a.retired_into_album_id IS NULL \
                     GROUP BY c.artist_id"
                );
                let mut query = sqlx::query_as::<_, (String, f64, i64)>(&sql);
                for id in chunk.iter().chain(chunk) {
                    query = query.bind(id);
                }
                let rows = query
                    .fetch_all(pool)
                    .await
                    .map_err(read_error("player.artist_durations"))?;
                out.extend(rows.into_iter().map(|(id, seconds, count)| {
                    (id, (seconds, u64::try_from(count).unwrap_or(0)))
                }));
            }
            Ok(out)
        })
    }
}
