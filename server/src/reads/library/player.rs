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
    ALBUM_COLUMNS, ALBUM_JOINS, LibraryDb, TRACK_MISS_ORACLE, TRACK_TEXT_MATCH, fts_match_phrase,
    sample_track_ids,
};
use super::stores::{AlbumRecord, BoxFuture, StoreError};

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
}

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
}

/// Track filters; every set field narrows.
#[derive(Debug, Clone, Default)]
pub struct TrackQuery {
    /// Title/artist/album substring.
    pub q: Option<String>,
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
    /// Only tracks this user has played; also the user the play-history
    /// orders read.
    pub played_by: Option<String>,
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
    /// The `played_by` user's last play.
    LastPlayed,
    /// The `played_by` user's play count.
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
    /// Only albums this user has played; also the user the play-history
    /// orders read.
    pub played_by: Option<String>,
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
    /// A fresh random order on every call.
    Random,
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

    /// Which of `ids` exist, for one kind (`artist`, `album`, `track`).
    fn existing<'a>(
        &'a self,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>>;

    /// Catalog revision, bumped by every catalog write.
    fn revision<'a>(&'a self) -> BoxFuture<'a, Result<i64, StoreError>>;

    /// Album ids from the user's play history: most recent first, or most
    /// played first when `frequent`.
    fn history_albums<'a>(
        &'a self,
        user_id: &'a str,
        frequent: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<Vec<String>, StoreError>>;

    /// The user's play counts and last plays for some ids of one kind
    /// (`artist`, `album`, `track`).
    fn play_stats<'a>(
        &'a self,
        user_id: &'a str,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, PlayStat>, StoreError>>;

    /// Every `(kind, id)` a player can address: artists, albums and
    /// streamable tracks by local id, genres by display name.
    fn every_id<'a>(&'a self) -> BoxFuture<'a, Result<Vec<(&'static str, String)>, StoreError>>;
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
    t.imported_at AS date_added, t.title_sort AS sort_name, t.disc_subtitle AS disc_subtitle, \
    COALESCE(te.recording_mbid, t.embedded_recording_mbid) AS recording_mbid, \
    tae.provider_artist_id AS artist_mbid, aae.provider_artist_id AS album_artist_mbid, \
    ae.release_group_mbid AS release_group_mbid, t.release_type AS release_type, \
    t.replaygain_track_gain AS rg_track_gain, t.replaygain_album_gain AS rg_album_gain, \
    t.replaygain_track_peak AS rg_track_peak, t.replaygain_album_peak AS rg_album_peak, \
    EXISTS (SELECT 1 FROM local_album_artwork w WHERE w.local_album_id = a.id) \
    AS cover_available \
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
        },
        genre: row.try_get("primary_genre")?,
        sort_name: row.try_get("album_sort_name")?,
        original_release_date: row.try_get("original_release_date")?,
    })
}

fn read_error(operation: &str) -> impl Fn(sqlx::Error) -> StoreError + '_ {
    move |error| StoreError::Internal(crate::db::map_sqlx_busy(operation, error).to_string())
}

fn placeholders(len: usize) -> String {
    vec!["?"; len].join(", ")
}

/// Folded LIKE pattern for a substring filter.
fn like_pattern(raw: &str) -> String {
    let folded = crate::db::fold_text(raw)
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

fn track_clause(query: &TrackQuery) -> Clause {
    let mut clause = Clause::default();
    clause.push("t.availability = 'indexed'", []);
    if let Some(q) = &query.q {
        // Words of 3+ characters search the FTS index (every word, any
        // column); anything else keeps the LIKE substring match.
        if let Some(expression) = fts_match_phrase(q) {
            clause.push(TRACK_TEXT_MATCH, [Bind::Text(expression)]);
            return finish_track_clause(clause, query);
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
    finish_track_clause(clause, query)
}

/// The non-text track filters.
fn finish_track_clause(mut clause: Clause, query: &TrackQuery) -> Clause {
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
        let folded = crate::db::fold_text(genre.trim());
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
        let folded = crate::db::fold_text(name.trim());
        clause.push(
            "(t.artist_name_folded = ? OR t.album_artist_name_folded = ?)",
            [Bind::Text(folded.clone()), Bind::Text(folded)],
        );
    }
    if let Some(user_id) = &query.played_by {
        clause.push(
            "t.id IN (SELECT h.local_track_id FROM library_play_history h WHERE h.user_id = ?)",
            [Bind::Text(user_id.clone())],
        );
    }
    clause
}

/// Whether the track filters read the album row.
fn track_query_reads_album(query: &TrackQuery) -> bool {
    !query.album_artist_ids.is_empty()
}

/// No filter beyond streamability: the maintained total applies.
fn track_query_is_open(query: &TrackQuery) -> bool {
    query.q.is_none()
        && query.album_id.is_none()
        && query.artist_ids.is_empty()
        && query.album_artist_ids.is_empty()
        && query.genre.is_none()
        && query.year_from.is_none()
        && query.year_to.is_none()
        && query.artist_name.is_none()
        && query.played_by.is_none()
}

/// No filter beyond "has a streamable track".
fn album_query_is_open(query: &AlbumQuery) -> bool {
    query.q.is_none()
        && query.artist_id.is_none()
        && query.album_artist_ids.is_empty()
        && query.appears_on.is_empty()
        && query.genre.is_none()
        && query.year_from.is_none()
        && query.year_to.is_none()
        && query.played_by.is_none()
}

/// An ORDER BY plus the user its play-history keys read.
struct Order {
    sql: String,
    user: Option<String>,
}

impl Order {
    fn fixed(sql: &str) -> Self {
        Self {
            sql: sql.to_owned(),
            user: None,
        }
    }

    /// `key` over one row kind (`t`/`local_track_id` or `a`/`local_album_id`).
    fn keyed(
        key: OrderKey,
        descending: bool,
        columns: (&str, &str, &str),
        alias: &str,
        history_column: &str,
        played_by: Option<&String>,
    ) -> Self {
        let (added, title, year) = columns;
        let direction = if descending { "DESC" } else { "ASC" };
        let history = |aggregate: &str| {
            format!(
                "(SELECT {aggregate} FROM library_play_history h \
                 WHERE h.user_id = ? AND h.{history_column} = {alias}.id)"
            )
        };
        let (expression, user) = match key {
            OrderKey::Added => (added.to_owned(), None),
            OrderKey::Title => (title.to_owned(), None),
            OrderKey::Year => (year.to_owned(), None),
            OrderKey::LastPlayed => (
                history("MAX(h.played_at)"),
                Some(played_by.cloned().unwrap_or_default()),
            ),
            OrderKey::PlayCount => (
                history("COUNT(*)"),
                Some(played_by.cloned().unwrap_or_default()),
            ),
        };
        Self {
            sql: format!("{expression} {direction}, {alias}.id {direction}"),
            user,
        }
    }
}

/// Prime modulus of the shuffle permutation; above any realistic rowid.
const SHUFFLE_PRIME: i64 = 2_147_483_647;

/// A seeded permutation of `alias` rows: `(rowid * a + b) mod p` is a
/// bijection for rowids below the prime, so one seed is one fixed order
/// with no ties, and different seeds give different orders (Navidrome's
/// SEEDEDRAND, without a custom SQL function). The values are integers
/// derived here, never caller text.
fn shuffle_order(alias: &str, seed: u32) -> String {
    let seed = i64::from(seed);
    let a = seed % (SHUFFLE_PRIME - 1) + 1;
    let b = (seed * 7919) % SHUFFLE_PRIME;
    format!("({alias}.rowid * {a} + {b}) % {SHUFFLE_PRIME}, {alias}.rowid")
}

/// Per-caller seeds for paged random lists. The first page (offset 0)
/// draws a new seed and later pages reuse it, so a client paging through
/// "random" sees one shuffle with no repeats or gaps. Seeds idle for an
/// hour are dropped once the map grows.
#[derive(Debug, Default)]
pub struct ShuffleSeeds {
    seeds: Mutex<HashMap<String, (u32, std::time::Instant)>>,
}

impl ShuffleSeeds {
    /// Most callers remembered before idle ones are pruned.
    const PRUNE_AT: usize = 1024;
    /// Idle time after which a caller's seed may be pruned.
    const IDLE: std::time::Duration = std::time::Duration::from_secs(3600);

    /// The seed for `caller` at `offset`.
    pub fn seed(&self, caller: &str, offset: u64) -> u32 {
        let now = std::time::Instant::now();
        let mut seeds = self
            .seeds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if seeds.len() >= Self::PRUNE_AT {
            seeds.retain(|_, (_, used)| now.duration_since(*used) < Self::IDLE);
        }
        let fresh = || uuid::Uuid::new_v4().as_u128() as u32;
        let entry = seeds
            .entry(caller.to_owned())
            .or_insert_with(|| (fresh(), now));
        if offset == 0 {
            entry.0 = fresh();
        }
        entry.1 = now;
        entry.0
    }
}

fn track_order_sql(order: TrackOrder, played_by: Option<&String>) -> Order {
    match order {
        TrackOrder::Album => Order::fixed(
            "a.album_artist_name_folded ASC, a.title_folded ASC, a.id ASC, \
             t.disc_number ASC, t.track_number ASC, t.id ASC",
        ),
        TrackOrder::Disc => Order::fixed("t.disc_number ASC, t.track_number ASC, t.id ASC"),
        TrackOrder::Title => Order::fixed("t.title_folded ASC, t.id ASC"),
        TrackOrder::Newest => Order::fixed("t.imported_at DESC, t.id ASC"),
        TrackOrder::Random => Order::fixed("RANDOM()"),
        TrackOrder::Shuffle(seed) => Order::fixed(&shuffle_order("t", seed)),
        TrackOrder::Natural => Order::fixed("t.rowid ASC"),
        TrackOrder::By(key, descending) => Order::keyed(
            key,
            descending,
            ("t.imported_at", "t.title_folded", "t.year"),
            "t",
            "local_track_id",
            played_by,
        ),
    }
}

fn album_clause(query: &AlbumQuery) -> Clause {
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
    if let Some(user_id) = &query.played_by {
        clause.push(
            "a.id IN (SELECT h.local_album_id FROM library_play_history h WHERE h.user_id = ?)",
            [Bind::Text(user_id.clone())],
        );
    }
    if let Some(genre) = &query.genre {
        let folded = crate::db::fold_text(genre.trim());
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
    clause
}

fn album_order_sql(order: AlbumOrder, played_by: Option<&String>) -> Order {
    match order {
        AlbumOrder::Newest => Order::fixed("a.created_at DESC, a.id ASC"),
        AlbumOrder::Title => Order::fixed("a.title_folded ASC, a.id ASC"),
        AlbumOrder::Artist => {
            Order::fixed("a.album_artist_name_folded ASC, a.title_folded ASC, a.id ASC")
        }
        AlbumOrder::YearAsc => {
            Order::fixed("(a.year IS NULL) ASC, a.year ASC, a.title_folded ASC, a.id ASC")
        }
        AlbumOrder::YearDesc => {
            Order::fixed("(a.year IS NULL) ASC, a.year DESC, a.title_folded ASC, a.id ASC")
        }
        AlbumOrder::Random => Order::fixed("RANDOM()"),
        AlbumOrder::Shuffle(seed) => Order::fixed(&shuffle_order("a", seed)),
        AlbumOrder::Natural => Order::fixed("a.rowid ASC"),
        AlbumOrder::By(key, descending) => Order::keyed(
            key,
            descending,
            ("a.created_at", "a.title_folded", "a.year"),
            "a",
            "local_album_id",
            played_by,
        ),
    }
}

/// Album extras appended to the shared album columns.
const ALBUM_EXTRAS: &str = "a.primary_genre AS primary_genre, \
    a.album_artist_sort_name AS album_sort_name, \
    a.original_release_date AS original_release_date";

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

    /// One page of track ids in order, or `None` when the text search
    /// matches nothing at all (the miss oracle).
    async fn track_ids(
        pool: &SqlitePool,
        query: &TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> Result<Option<Vec<String>>, StoreError> {
        // Miss oracle: a query no track text contains matches nothing,
        // whatever the other filters say.
        if let Some(expression) = query.q.as_deref().and_then(fts_match_phrase) {
            let hit: bool = sqlx::query_scalar(TRACK_MISS_ORACLE)
                .bind(expression)
                .fetch_one(pool)
                .await
                .map_err(read_error("player.tracks.search"))?;
            if !hit {
                return Ok(None);
            }
        }
        // A one-shot random list over the whole catalog samples rowids
        // instead of drawing a random key for every track.
        if order == TrackOrder::Random && track_query_is_open(query) {
            return sample_track_ids(pool, limit)
                .await
                .map(Some)
                .map_err(read_error("player.tracks.random"));
        }
        let mut clause = track_clause(query);
        // Insertion order walks the table itself and year order walks
        // (year, id); nearly every track is streamable, so with planner
        // statistics an availability index looks free and the planner would
        // drive from it and sort the whole catalog instead. The unary plus
        // keeps the filter a plain row check.
        if matches!(
            order,
            TrackOrder::Natural | TrackOrder::By(OrderKey::Year, _)
        ) {
            clause.sql[0] = "+t.availability = 'indexed'".to_owned();
        }
        let page_from = if track_query_reads_album(query) || order == TrackOrder::Album {
            "local_tracks t JOIN local_albums a ON a.id = t.local_album_id"
        } else {
            "local_tracks t"
        };
        let order = track_order_sql(order, query.played_by.as_ref());
        // Page the ids over the narrow join; the rich rows are read for the
        // page alone.
        let sql = format!(
            "SELECT t.id FROM {page_from} WHERE {} ORDER BY {} LIMIT ? OFFSET ?",
            clause.render(),
            order.sql
        );
        let mut page = clause.bind_scalar(sqlx::query_scalar(&sql));
        if let Some(user) = &order.user {
            page = page.bind(user.as_str());
        }
        page.bind(limit.min(i64::MAX as u64) as i64)
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
    async fn track_total(pool: &SqlitePool, query: &TrackQuery) -> Result<u64, StoreError> {
        let total: i64 = if track_query_is_open(query) {
            // Maintained exact count of indexed tracks (migration 0005).
            sqlx::query_scalar(
                "SELECT COALESCE(SUM(indexed_tracks), 0) FROM library_track_format_stats",
            )
            .fetch_one(pool)
            .await
            .map_err(read_error("player.tracks.count"))?
        } else {
            let clause = track_clause(query);
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

    async fn albums_in(
        pool: &SqlitePool,
        ids: &[String],
    ) -> Result<HashMap<String, PlayerAlbum>, StoreError> {
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let sql = format!(
                "SELECT {ALBUM_COLUMNS}, {ALBUM_EXTRAS} {ALBUM_JOINS} \
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
            let Some(ids) = Self::track_ids(pool, query, order, limit, offset).await? else {
                return Ok((Vec::new(), 0));
            };
            let tracks = Self::hydrate_tracks(pool, &ids).await?;
            Ok((tracks, Self::track_total(pool, query).await?))
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
            match Self::track_ids(pool, query, order, limit, offset).await? {
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
            let albums = self.album_page(query, order, limit, offset).await?;
            let clause = album_clause(query);
            let count_sql = format!(
                "SELECT COUNT(*) FROM local_albums a WHERE {}",
                clause.render()
            );
            let total = if album_query_is_open(query) {
                self.open_album_total(pool, &count_sql).await?
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
            let clause = album_clause(query);
            let where_sql = clause.render();
            let order = album_order_sql(order, query.played_by.as_ref());
            let sql = format!(
                "SELECT a.id FROM local_albums a WHERE {where_sql} ORDER BY {} LIMIT ? OFFSET ?",
                order.sql
            );
            let mut page = clause.bind_scalar(sqlx::query_scalar(&sql));
            if let Some(user) = &order.user {
                page = page.bind(user.as_str());
            }
            let ids: Vec<String> = page
                .bind(limit.min(i64::MAX as u64) as i64)
                .bind(offset.min(i64::MAX as u64) as i64)
                .fetch_all(pool)
                .await
                .map_err(read_error("player.albums.page"))?;
            let mut by_id = Self::albums_in(pool, &ids).await?;
            Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
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
            let order = if frequent {
                "COUNT(*) DESC, MAX(h.played_at) DESC"
            } else {
                "MAX(h.played_at) DESC"
            };
            let sql = format!(
                "SELECT h.local_album_id FROM library_play_history h \
                 JOIN local_albums a ON a.id = h.local_album_id \
                 WHERE h.user_id = ? AND h.local_album_id IS NOT NULL \
                 GROUP BY h.local_album_id ORDER BY {order}, h.local_album_id LIMIT ? OFFSET ?"
            );
            sqlx::query_scalar(&sql)
                .bind(user_id)
                .bind(limit.min(i64::MAX as u64) as i64)
                .bind(offset.min(i64::MAX as u64) as i64)
                .fetch_all(pool)
                .await
                .map_err(read_error("player.history_albums"))
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
            let column = match kind {
                "artist" => "local_artist_id",
                "album" => "local_album_id",
                "track" => "local_track_id",
                _ => return Ok(HashMap::new()),
            };
            let mut out = HashMap::new();
            for chunk in ids.chunks(500) {
                let sql = format!(
                    "SELECT {column} AS id, COUNT(*) AS plays, \
                     CAST(strftime('%s', MAX(played_at)) AS REAL) AS last_played \
                     FROM library_play_history WHERE user_id = ? AND {column} IN ({}) \
                     GROUP BY {column}",
                    placeholders(chunk.len())
                );
                let mut query = sqlx::query(&sql).bind(user_id);
                for id in chunk {
                    query = query.bind(id);
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
}
