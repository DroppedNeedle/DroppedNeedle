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

use super::sqlite::{ALBUM_COLUMNS, ALBUM_JOINS, LibraryDb};
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
}

/// Track orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOrder {
    /// Album artist, album, disc, track.
    Album,
    /// Title.
    Title,
    /// Newest import first.
    Newest,
    /// Random.
    Random,
}

/// Album filters; every set field narrows.
#[derive(Debug, Clone, Default)]
pub struct AlbumQuery {
    /// Title/artist substring.
    pub q: Option<String>,
    /// One album artist.
    pub artist_id: Option<String>,
    /// Genre, matched folded on any track.
    pub genre: Option<String>,
    /// Lowest year, inclusive.
    pub year_from: Option<i64>,
    /// Highest year, inclusive.
    pub year_to: Option<i64>,
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
    /// Random.
    Random,
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
    t.replaygain_track_peak AS rg_track_peak, t.replaygain_album_peak AS rg_album_peak \
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

    fn bind<'q>(
        &'q self,
        mut query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    ) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
        for bind in &self.binds {
            query = match bind {
                Bind::Text(value) => query.bind(value.as_str()),
                Bind::Int(value) => query.bind(*value),
            };
        }
        query
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
    clause
}

fn track_order_sql(order: TrackOrder) -> &'static str {
    match order {
        TrackOrder::Album => {
            "a.album_artist_name_folded ASC, a.title_folded ASC, a.id ASC, \
             t.disc_number ASC, t.track_number ASC, t.id ASC"
        }
        TrackOrder::Title => "t.title_folded ASC, t.id ASC",
        TrackOrder::Newest => "t.imported_at DESC, t.id ASC",
        TrackOrder::Random => "RANDOM()",
    }
}

fn album_clause(query: &AlbumQuery) -> Clause {
    let mut clause = Clause::default();
    clause.push(
        "a.retired_into_album_id IS NULL AND EXISTS (SELECT 1 FROM local_tracks t \
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

fn album_order_sql(order: AlbumOrder) -> &'static str {
    match order {
        AlbumOrder::Newest => "a.created_at DESC, a.id ASC",
        AlbumOrder::Title => "a.title_folded ASC, a.id ASC",
        AlbumOrder::Artist => "a.album_artist_name_folded ASC, a.title_folded ASC, a.id ASC",
        AlbumOrder::YearAsc => "(a.year IS NULL) ASC, a.year ASC, a.title_folded ASC, a.id ASC",
        AlbumOrder::YearDesc => "(a.year IS NULL) ASC, a.year DESC, a.title_folded ASC, a.id ASC",
        AlbumOrder::Random => "RANDOM()",
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
}

impl SqlitePlayerCatalog {
    /// Adapter over one handle.
    pub fn new(db: &LibraryDb) -> Self {
        Self { db: db.clone() }
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
            let mut out = Vec::with_capacity(ids.len());
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
                    .map_err(read_error("player.tracks_by_ids"))?;
                for row in &rows {
                    out.push(map_track(row).map_err(read_error("player.tracks_by_ids"))?);
                }
            }
            Ok(out)
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
            let clause = track_clause(query);
            let where_sql = clause.render();
            let sql = format!(
                "{TRACK_SELECT} WHERE {where_sql} ORDER BY {} LIMIT ? OFFSET ?",
                track_order_sql(order)
            );
            let rows = clause
                .bind(sqlx::query(&sql))
                .bind(limit.min(i64::MAX as u64) as i64)
                .bind(offset.min(i64::MAX as u64) as i64)
                .fetch_all(pool)
                .await
                .map_err(read_error("player.tracks"))?;
            let tracks = rows
                .iter()
                .map(map_track)
                .collect::<Result<Vec<_>, _>>()
                .map_err(read_error("player.tracks"))?;
            let count_sql = format!(
                "SELECT COUNT(*) FROM local_tracks t JOIN local_albums a \
                 ON a.id = t.local_album_id WHERE {where_sql}"
            );
            let total: i64 = clause
                .bind_scalar(sqlx::query_scalar(&count_sql))
                .fetch_one(pool)
                .await
                .map_err(read_error("player.tracks.count"))?;
            Ok((tracks, total.max(0) as u64))
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
            let clause = album_clause(query);
            let where_sql = clause.render();
            let sql = format!(
                "SELECT a.id FROM local_albums a WHERE {where_sql} ORDER BY {} LIMIT ? OFFSET ?",
                album_order_sql(order)
            );
            let ids: Vec<String> = clause
                .bind_scalar(sqlx::query_scalar(&sql))
                .bind(limit.min(i64::MAX as u64) as i64)
                .bind(offset.min(i64::MAX as u64) as i64)
                .fetch_all(pool)
                .await
                .map_err(read_error("player.albums.page"))?;
            let mut by_id = Self::albums_in(pool, &ids).await?;
            let albums = ids.iter().filter_map(|id| by_id.remove(id)).collect();
            let count_sql = format!("SELECT COUNT(*) FROM local_albums a WHERE {where_sql}");
            let total: i64 = clause
                .bind_scalar(sqlx::query_scalar(&count_sql))
                .fetch_one(pool)
                .await
                .map_err(read_error("player.albums.count"))?;
            Ok((albums, total.max(0) as u64))
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
