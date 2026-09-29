//! Production SQLite adapters over the 0001 baseline tables.
//!
//! [`SqliteCatalog`] implements [`LibraryCatalog`](super::stores::LibraryCatalog)
//! over the `local_*` tables plus artwork and identity rows; [`SqliteFavorites`]
//! implements [`FavoriteReads`](super::stores::FavoriteReads) over
//! `library_user_favorites`. Reads only: both hold the reader pool, never the
//! writer lane.
//!
//! Conventions:
//!
//! - Every track read filters `local_tracks.availability = 'indexed'`.
//!   Missing or excluded files are invisible here, which is what keeps all
//!   counts streamable-only.
//! - Text filters match the pre-folded columns (`title_folded`,
//!   `album_artist_name_folded`, `folded_name`) with an escaped LIKE pattern
//!   folded by the shared [`fold_text`](droppedneedle::db::fold_text), the
//!   same fold the scan path writes.
//! - Optional filters bind NULL-means-absent so every statement keeps a
//!   static bind list; only the favorite-id list builds placeholders.
//! - Retired artists (`retired_into_artist_id` set) are merged away and read
//!   as absent everywhere.

use sqlx::{Row as _, SqlitePool};

use super::stores::{
    AlbumFilter, AlbumRecord, AlbumSort, ArtistListing, ArtistRecord, ArtistScope, ArtistSort,
    BoxFuture, DecadeRecord, GenreRecord, StatsRecord, StoreError, TrackFilter, TrackRecord,
    TrackSort,
};
use super::stores::{FavoriteReads, LibraryCatalog};

/// Shared SQLite handle for the library adapters: the reader pool.
/// `Clone` is cheap; clones share the pool.
#[derive(Clone, Debug, Default)]
pub struct LibraryDb {
    inner: Option<SqlitePool>,
}

impl LibraryDb {
    /// Live handle over the runtime's reader pool.
    pub fn new(pool: &SqlitePool) -> Self {
        Self {
            inner: Some(pool.clone()),
        }
    }

    /// Skeleton-boot handle: every operation fails closed. Only pre-boot
    /// state builds this; production uses [`LibraryDb::new`].
    pub fn unwired() -> Self {
        Self::default()
    }

    fn live(&self) -> Option<&SqlitePool> {
        self.inner.as_ref()
    }
}

fn internal(op: &str, error: sqlx::Error) -> StoreError {
    StoreError::Internal(droppedneedle::db::map_sqlx_busy(op, error).to_string())
}

fn unwired_store() -> StoreError {
    StoreError::Internal("library store is not wired".to_owned())
}

/// Escape the LIKE metacharacters in a folded filter, then wrap for
/// substring matching.
fn like_pattern(raw: &str) -> String {
    let mut folded = droppedneedle::db::fold_text(raw);
    folded = folded
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{folded}%")
}

/// One album's aggregate columns, shared by every album SELECT.
const ALBUM_COLUMNS: &str = "a.id AS id, a.title AS title, \
    COALESCE(an.display_name, a.album_artist_name, '') AS artist_name, \
    a.album_artist_id AS artist_id, \
    ae.release_group_mbid AS release_group_mbid, ae.release_mbid AS release_mbid, \
    rte.provider_artist_id AS artist_mbid, \
    (ae.local_album_id IS NOT NULL) AS linked, \
    COUNT(t.id) AS track_count, \
    COALESCE(SUM(t.duration_seconds), 0.0) AS total_duration_seconds, \
    COALESCE(SUM(t.file_size_bytes), 0) AS total_size_bytes, \
    (SELECT t2.file_format FROM local_tracks t2 \
     WHERE t2.local_album_id = a.id AND t2.availability = 'indexed' \
     GROUP BY t2.file_format ORDER BY COUNT(*) DESC, t2.file_format LIMIT 1) AS format, \
    a.year AS year, a.is_compilation AS is_compilation, \
    (w.local_album_id IS NOT NULL) AS cover_available, \
    a.created_at AS date_added";

/// Joins shared by every album SELECT. Track joins filter to streamable.
const ALBUM_JOINS: &str = "FROM local_albums a \
    LEFT JOIN local_artists an ON an.id = a.album_artist_id \
    LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id \
    LEFT JOIN local_artist_external_identities rte ON rte.local_artist_id = a.album_artist_id \
    LEFT JOIN local_album_artwork w ON w.local_album_id = a.id \
    LEFT JOIN local_tracks t ON t.local_album_id = a.id AND t.availability = 'indexed'";

/// Filters shared by the album list and its count.
const ALBUM_FILTER: &str = "(? IS NULL OR a.title_folded LIKE ? ESCAPE '\\' \
    OR a.album_artist_name_folded LIKE ? ESCAPE '\\') \
    AND (? IS NULL OR a.album_artist_id = ?) \
    AND (? IS NULL OR (a.year >= ? AND a.year < ? + 10))";

fn album_order(sort: AlbumSort, descending: bool) -> &'static str {
    match (sort, descending) {
        (AlbumSort::Name, false) => "a.title_folded ASC, a.id ASC",
        (AlbumSort::Name, true) => "a.title_folded DESC, a.id ASC",
        (AlbumSort::DateAdded, false) => "a.created_at ASC, a.id ASC",
        (AlbumSort::DateAdded, true) => "a.created_at DESC, a.id ASC",
        (AlbumSort::Year, false) => "(a.year IS NULL) ASC, a.year ASC, a.id ASC",
        (AlbumSort::Year, true) => "(a.year IS NULL) ASC, a.year DESC, a.id ASC",
        (AlbumSort::Random, _) => "RANDOM()",
        (AlbumSort::Rediscover, false) => "a.created_at ASC, a.id ASC",
        (AlbumSort::Rediscover, true) => "a.created_at DESC, a.id ASC",
    }
}

fn map_album(row: &sqlx::sqlite::SqliteRow) -> AlbumRecord {
    let track_count: i64 = row.get("track_count");
    let total_size_bytes: i64 = row.get("total_size_bytes");
    AlbumRecord {
        id: row.get("id"),
        title: row.get("title"),
        artist_name: row.get("artist_name"),
        artist_id: row.get("artist_id"),
        release_group_mbid: row.get("release_group_mbid"),
        release_mbid: row.get("release_mbid"),
        artist_mbid: row.get("artist_mbid"),
        linked: row.get("linked"),
        track_count: track_count.max(0) as u64,
        total_duration_seconds: row.get("total_duration_seconds"),
        total_size_bytes,
        format: row.get("format"),
        year: row.get("year"),
        is_compilation: row.get("is_compilation"),
        cover_available: row.get("cover_available"),
        date_added: row.get("date_added"),
    }
}

/// One track's columns, shared by every track SELECT.
const TRACK_COLUMNS: &str = "t.id AS id, t.title AS title, \
    t.local_album_id AS album_id, t.album_title AS album_title, \
    COALESCE(t.artist_name, '') AS artist_name, \
    ta.local_artist_id AS artist_id, \
    COALESCE(t.album_artist_name, '') AS album_artist_name, \
    t.disc_number AS disc_number, t.track_number AS track_number, t.year AS year, \
    COALESCE((SELECT g.name FROM local_track_genres g \
     WHERE g.local_track_id = t.id ORDER BY g.position LIMIT 1), t.genre) AS genre, \
    t.duration_seconds AS duration_seconds, t.file_format AS format, \
    t.bit_rate AS bit_rate, t.sample_rate AS sample_rate, \
    t.file_size_bytes AS file_size_bytes, t.imported_at AS date_added, \
    (w.local_album_id IS NOT NULL) AS cover_available";

/// Joins shared by every track SELECT. Callers add the streamability filter.
const TRACK_JOINS: &str = "FROM local_tracks t \
    LEFT JOIN local_track_artists ta \
        ON ta.local_track_id = t.id AND ta.position = 0 \
    LEFT JOIN local_album_artwork w ON w.local_album_id = t.local_album_id";

fn map_track(row: &sqlx::sqlite::SqliteRow) -> TrackRecord {
    TrackRecord {
        id: row.get("id"),
        title: row.get("title"),
        album_id: row.get("album_id"),
        album_title: row.get("album_title"),
        artist_name: row.get("artist_name"),
        artist_id: row.get("artist_id"),
        album_artist_name: row.get("album_artist_name"),
        disc_number: row.get("disc_number"),
        track_number: row.get("track_number"),
        year: row.get("year"),
        genre: row.get("genre"),
        duration_seconds: row.get("duration_seconds"),
        format: row.get("format"),
        bit_rate: row.get("bit_rate"),
        sample_rate: row.get("sample_rate"),
        file_size_bytes: row.get("file_size_bytes"),
        date_added: row.get("date_added"),
        cover_available: row.get("cover_available"),
    }
}

/// One artist's aggregate columns, shared by every artist SELECT.
const ARTIST_COLUMNS: &str = "r.id AS id, r.display_name AS name, \
    e.provider_artist_id AS artist_mbid, (e.local_artist_id IS NOT NULL) AS linked, \
    (SELECT COUNT(*) FROM local_albums la WHERE la.album_artist_id = r.id) AS album_count, \
    (SELECT COUNT(DISTINCT ct.id) FROM local_tracks ct \
     JOIN local_track_artists cta ON cta.local_track_id = ct.id \
     WHERE cta.local_artist_id = r.id AND ct.availability = 'indexed') AS track_count, \
    (SELECT COUNT(DISTINCT at.local_album_id) FROM local_tracks at \
     JOIN local_track_artists ata ON ata.local_track_id = at.id \
     JOIN local_albums aa ON aa.id = at.local_album_id \
     WHERE ata.local_artist_id = r.id AND at.availability = 'indexed' \
     AND aa.album_artist_id != r.id) AS appearance_album_count, \
    r.created_at AS date_added";

const ARTIST_JOINS: &str = "FROM local_artists r \
    LEFT JOIN local_artist_external_identities e ON e.local_artist_id = r.id";

/// Artists leading at least one album.
const LED_PREDICATE: &str =
    "EXISTS (SELECT 1 FROM local_albums la WHERE la.album_artist_id = r.id)";

/// Artists credited anywhere (album or track level).
const CREDITED_PREDICATE: &str = "(EXISTS (SELECT 1 FROM local_album_artists laa \
    WHERE laa.local_artist_id = r.id) \
    OR EXISTS (SELECT 1 FROM local_track_artists lta WHERE lta.local_artist_id = r.id) \
    OR EXISTS (SELECT 1 FROM local_albums la WHERE la.album_artist_id = r.id))";

fn artist_scope_predicate(scope: ArtistScope) -> &'static str {
    match scope {
        ArtistScope::All => "1 = 1",
        ArtistScope::AlbumArtists => LED_PREDICATE,
        ArtistScope::Contributors => "(NOT (LED) AND (CREDITED))",
    }
}

fn artist_order(sort: ArtistSort, descending: bool) -> &'static str {
    match (sort, descending) {
        (ArtistSort::Name, false) => "r.folded_name ASC, r.id ASC",
        (ArtistSort::Name, true) => "r.folded_name DESC, r.id ASC",
        (ArtistSort::AlbumCount, false) => "album_count ASC, r.folded_name ASC, r.id ASC",
        (ArtistSort::AlbumCount, true) => "album_count DESC, r.folded_name ASC, r.id ASC",
        (ArtistSort::DateAdded, false) => "r.created_at ASC, r.id ASC",
        (ArtistSort::DateAdded, true) => "r.created_at DESC, r.id ASC",
    }
}

fn map_artist(row: &sqlx::sqlite::SqliteRow) -> ArtistRecord {
    let album_count: i64 = row.get("album_count");
    let track_count: i64 = row.get("track_count");
    let appearance_album_count: i64 = row.get("appearance_album_count");
    ArtistRecord {
        id: row.get("id"),
        name: row.get("name"),
        artist_mbid: row.get("artist_mbid"),
        linked: row.get("linked"),
        album_count: album_count.max(0) as u64,
        track_count: track_count.max(0) as u64,
        appearance_album_count: appearance_album_count.max(0) as u64,
        date_added: row.get("date_added"),
    }
}

/// Catalog reads over the reader pool.
#[derive(Clone, Debug)]
pub struct SqliteCatalog {
    db: LibraryDb,
}

impl SqliteCatalog {
    /// Adapter over one handle.
    pub fn new(db: &LibraryDb) -> Self {
        Self { db: db.clone() }
    }
}

impl LibraryCatalog for SqliteCatalog {
    fn list_albums<'a>(
        &'a self,
        filter: &'a AlbumFilter,
        sort: AlbumSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<AlbumRecord>, u64), StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let pattern = filter.q.as_deref().map(like_pattern);
            let order = album_order(sort, descending);
            let rows = sqlx::query(&format!(
                "SELECT {ALBUM_COLUMNS} {ALBUM_JOINS} WHERE {ALBUM_FILTER} \
                 GROUP BY a.id ORDER BY {order} LIMIT ? OFFSET ?"
            ))
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(filter.artist_id.as_deref())
            .bind(filter.artist_id.as_deref())
            .bind(filter.decade)
            .bind(filter.decade)
            .bind(filter.decade)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.albums.list", error))?;
            let row = sqlx::query(&format!(
                "SELECT COUNT(*) AS total FROM local_albums a WHERE {ALBUM_FILTER}"
            ))
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(filter.artist_id.as_deref())
            .bind(filter.artist_id.as_deref())
            .bind(filter.decade)
            .bind(filter.decade)
            .bind(filter.decade)
            .fetch_one(pool)
            .await
            .map_err(|error| internal("library.albums.count", error))?;
            let total: i64 = row.get("total");
            Ok((rows.iter().map(map_album).collect(), total.max(0) as u64))
        })
    }

    fn get_album<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let row = sqlx::query(&format!(
                "SELECT {ALBUM_COLUMNS} {ALBUM_JOINS} WHERE a.id = ? GROUP BY a.id"
            ))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal("library.albums.get", error))?;
            Ok(row.as_ref().map(map_album))
        })
    }

    fn album_tracks<'a>(
        &'a self,
        album_id: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<TrackRecord>, u64), StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(&format!(
                "SELECT {TRACK_COLUMNS} {TRACK_JOINS} \
                 WHERE t.local_album_id = ? AND t.availability = 'indexed' \
                 ORDER BY t.disc_number ASC, t.track_number ASC, t.id ASC \
                 LIMIT ? OFFSET ?"
            ))
            .bind(album_id)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.albums.tracks", error))?;
            let row = sqlx::query(
                "SELECT COUNT(*) AS total FROM local_tracks t \
                 WHERE t.local_album_id = ? AND t.availability = 'indexed'",
            )
            .bind(album_id)
            .fetch_one(pool)
            .await
            .map_err(|error| internal("library.albums.tracks.count", error))?;
            let total: i64 = row.get("total");
            Ok((rows.iter().map(map_track).collect(), total.max(0) as u64))
        })
    }

    fn album_copies<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<AlbumRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(&format!(
                "SELECT {ALBUM_COLUMNS} {ALBUM_JOINS} \
                 WHERE ae.release_group_mbid = \
                 (SELECT release_group_mbid FROM local_album_external_identities \
                  WHERE local_album_id = ?) \
                 AND a.id != ? GROUP BY a.id ORDER BY a.created_at ASC, a.id ASC"
            ))
            .bind(album_id)
            .bind(album_id)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.albums.copies", error))?;
            Ok(rows.iter().map(map_album).collect())
        })
    }

    fn list_artists<'a>(
        &'a self,
        scope: ArtistScope,
        q: Option<&'a str>,
        sort: ArtistSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<ArtistListing, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let pattern = q.map(like_pattern);
            let scope_sql = artist_scope_predicate(scope)
                .replace("(LED)", LED_PREDICATE)
                .replace("(CREDITED)", CREDITED_PREDICATE);
            let order = artist_order(sort, descending);
            let rows = sqlx::query(&format!(
                "SELECT {ARTIST_COLUMNS} {ARTIST_JOINS} \
                 WHERE r.retired_into_artist_id IS NULL AND ({scope_sql}) \
                 AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\') \
                 ORDER BY {order} LIMIT ? OFFSET ?"
            ))
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.artists.list", error))?;
            let totals = sqlx::query(&format!(
                "SELECT COUNT(*) AS total, \
                 COALESCE(SUM(CASE WHEN {LED_PREDICATE} THEN 1 ELSE 0 END), 0) AS led, \
                 COALESCE(SUM(CASE WHEN NOT ({LED_PREDICATE}) AND ({CREDITED_PREDICATE}) \
                 THEN 1 ELSE 0 END), 0) AS contributors \
                 {ARTIST_JOINS} WHERE r.retired_into_artist_id IS NULL \
                 AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\')"
            ))
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .fetch_one(pool)
            .await
            .map_err(|error| internal("library.artists.count", error))?;
            let mut total: i64 = totals.get("total");
            let led: i64 = totals.get("led");
            let contributors: i64 = totals.get("contributors");
            if !matches!(scope, ArtistScope::All) {
                let scoped = sqlx::query(&format!(
                    "SELECT COUNT(*) AS total {ARTIST_JOINS} \
                     WHERE r.retired_into_artist_id IS NULL AND ({scope_sql}) \
                     AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\')"
                ))
                .bind(pattern.as_deref())
                .bind(pattern.as_deref())
                .fetch_one(pool)
                .await
                .map_err(|error| internal("library.artists.count", error))?;
                total = scoped.get("total");
            }
            Ok((
                rows.iter().map(map_artist).collect(),
                total.max(0) as u64,
                led.max(0) as u64,
                contributors.max(0) as u64,
            ))
        })
    }

    fn get_artist<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ArtistRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let row = sqlx::query(&format!(
                "SELECT {ARTIST_COLUMNS} {ARTIST_JOINS} \
                 WHERE r.id = ? AND r.retired_into_artist_id IS NULL"
            ))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal("library.artists.get", error))?;
            Ok(row.as_ref().map(map_artist))
        })
    }

    fn artist_albums<'a>(
        &'a self,
        artist_id: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<AlbumRecord>, u64), StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(&format!(
                "SELECT {ALBUM_COLUMNS} {ALBUM_JOINS} WHERE a.album_artist_id = ? \
                 GROUP BY a.id ORDER BY a.title_folded ASC, a.id ASC LIMIT ? OFFSET ?"
            ))
            .bind(artist_id)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.artists.albums", error))?;
            let row = sqlx::query(
                "SELECT COUNT(*) AS total FROM local_albums a \
                 WHERE a.album_artist_id = ?",
            )
            .bind(artist_id)
            .fetch_one(pool)
            .await
            .map_err(|error| internal("library.artists.albums.count", error))?;
            let total: i64 = row.get("total");
            Ok((rows.iter().map(map_album).collect(), total.max(0) as u64))
        })
    }

    fn artist_appearances<'a>(
        &'a self,
        artist_id: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<AlbumRecord>, u64), StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let filter = "a.album_artist_id != ? AND EXISTS \
                (SELECT 1 FROM local_tracks at \
                 JOIN local_track_artists ata ON ata.local_track_id = at.id \
                 WHERE at.local_album_id = a.id AND ata.local_artist_id = ? \
                 AND at.availability = 'indexed')";
            let rows = sqlx::query(&format!(
                "SELECT {ALBUM_COLUMNS} {ALBUM_JOINS} WHERE {filter} \
                 GROUP BY a.id ORDER BY a.title_folded ASC, a.id ASC LIMIT ? OFFSET ?"
            ))
            .bind(artist_id)
            .bind(artist_id)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.artists.appearances", error))?;
            let row = sqlx::query(&format!(
                "SELECT COUNT(*) AS total FROM local_albums a WHERE {filter}"
            ))
            .bind(artist_id)
            .bind(artist_id)
            .fetch_one(pool)
            .await
            .map_err(|error| internal("library.artists.appearances.count", error))?;
            let total: i64 = row.get("total");
            Ok((rows.iter().map(map_album).collect(), total.max(0) as u64))
        })
    }

    fn list_tracks<'a>(
        &'a self,
        filter: &'a TrackFilter,
        sort: TrackSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<TrackRecord>, u64), StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let pattern = filter.q.as_deref().map(like_pattern);
            let genre = filter
                .genre
                .as_deref()
                .map(|name| droppedneedle::db::fold_text(name.trim()));
            let order = match (sort, descending) {
                (TrackSort::Title, false) => "t.title_folded ASC, t.id ASC",
                (TrackSort::Title, true) => "t.title_folded DESC, t.id ASC",
                (TrackSort::DateAdded, false) => "t.imported_at ASC, t.id ASC",
                (TrackSort::DateAdded, true) => "t.imported_at DESC, t.id ASC",
            };
            let filter_sql = "t.availability = 'indexed' \
                AND (? IS NULL OR t.title_folded LIKE ? ESCAPE '\\' \
                OR t.artist_name_folded LIKE ? ESCAPE '\\' \
                OR t.album_title_folded LIKE ? ESCAPE '\\') \
                AND (? IS NULL OR t.local_album_id = ?) \
                AND (? IS NULL OR EXISTS (SELECT 1 FROM local_track_artists x \
                WHERE x.local_track_id = t.id AND x.local_artist_id = ?)) \
                AND (? IS NULL OR EXISTS (SELECT 1 FROM local_track_genres g \
                WHERE g.local_track_id = t.id AND g.folded_name = ?) \
                OR t.genre_folded = ?)";
            let rows = sqlx::query(&format!(
                "SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE {filter_sql} \
                 ORDER BY {order} LIMIT ? OFFSET ?"
            ))
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(filter.album_id.as_deref())
            .bind(filter.album_id.as_deref())
            .bind(filter.artist_id.as_deref())
            .bind(filter.artist_id.as_deref())
            .bind(genre.as_deref())
            .bind(genre.as_deref())
            .bind(genre.as_deref())
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.tracks.list", error))?;
            let row = sqlx::query(&format!(
                "SELECT COUNT(*) AS total FROM local_tracks t WHERE {filter_sql}"
            ))
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(pattern.as_deref())
            .bind(filter.album_id.as_deref())
            .bind(filter.album_id.as_deref())
            .bind(filter.artist_id.as_deref())
            .bind(filter.artist_id.as_deref())
            .bind(genre.as_deref())
            .bind(genre.as_deref())
            .bind(genre.as_deref())
            .fetch_one(pool)
            .await
            .map_err(|error| internal("library.tracks.count", error))?;
            let total: i64 = row.get("total");
            Ok((rows.iter().map(map_track).collect(), total.max(0) as u64))
        })
    }

    fn get_track<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<TrackRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let row = sqlx::query(&format!(
                "SELECT {TRACK_COLUMNS} {TRACK_JOINS} \
                 WHERE t.id = ? AND t.availability = 'indexed'"
            ))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal("library.tracks.get", error))?;
            Ok(row.as_ref().map(map_track))
        })
    }

    fn stats<'a>(&'a self) -> BoxFuture<'a, Result<StatsRecord, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let row = sqlx::query(
                "SELECT (SELECT COUNT(*) FROM local_albums) AS albums, \
                 (SELECT COUNT(*) FROM local_artists \
                  WHERE retired_into_artist_id IS NULL) AS artists, \
                 (SELECT COUNT(*) FROM local_tracks \
                  WHERE availability = 'indexed') AS tracks, \
                 (SELECT COALESCE(SUM(file_size_bytes), 0) FROM local_tracks \
                  WHERE availability = 'indexed') AS size_bytes",
            )
            .fetch_one(pool)
            .await
            .map_err(|error| internal("library.stats", error))?;
            let albums: i64 = row.get("albums");
            let artists: i64 = row.get("artists");
            let tracks: i64 = row.get("tracks");
            let rows = sqlx::query(
                "SELECT file_format AS format, COUNT(*) AS total FROM local_tracks \
                 WHERE availability = 'indexed' GROUP BY file_format",
            )
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.stats.formats", error))?;
            let mut format_breakdown = std::collections::HashMap::new();
            for row in &rows {
                let format: String = row.get("format");
                let total: i64 = row.get("total");
                format_breakdown.insert(format, total.max(0) as u64);
            }
            Ok(StatsRecord {
                total_albums: albums.max(0) as u64,
                total_artists: artists.max(0) as u64,
                total_tracks: tracks.max(0) as u64,
                total_size_bytes: row.get("size_bytes"),
                format_breakdown,
            })
        })
    }

    fn recently_added<'a>(
        &'a self,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<AlbumRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(&format!(
                "SELECT {ALBUM_COLUMNS} {ALBUM_JOINS} \
                 GROUP BY a.id ORDER BY a.created_at DESC, a.id ASC LIMIT ?"
            ))
            .bind(limit as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.albums.recent", error))?;
            Ok(rows.iter().map(map_album).collect())
        })
    }

    fn genres<'a>(&'a self) -> BoxFuture<'a, Result<Vec<GenreRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(
                "WITH tagged(track_id, folded, name) AS ( \
                 SELECT local_track_id, folded_name, name FROM local_track_genres \
                 UNION \
                 SELECT id, genre_folded, genre FROM local_tracks \
                 WHERE genre_folded IS NOT NULL AND genre_folded != '') \
                 SELECT g.folded AS folded_name, MIN(g.name) AS name, \
                 COUNT(DISTINCT t.id) AS track_count, \
                 COUNT(DISTINCT t.local_album_id) AS album_count \
                 FROM tagged g JOIN local_tracks t ON t.id = g.track_id \
                 AND t.availability = 'indexed' \
                 GROUP BY g.folded ORDER BY track_count DESC, name ASC",
            )
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.genres", error))?;
            Ok(rows
                .iter()
                .map(|row| {
                    let track_count: i64 = row.get("track_count");
                    let album_count: i64 = row.get("album_count");
                    GenreRecord {
                        name: row.get("name"),
                        folded_name: row.get("folded_name"),
                        track_count: track_count.max(0) as u64,
                        album_count: album_count.max(0) as u64,
                    }
                })
                .collect())
        })
    }

    fn genre_tracks<'a>(
        &'a self,
        genre_folded: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<TrackRecord>, u64), StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let folded = droppedneedle::db::fold_text(genre_folded.trim());
            let filter = "t.availability = 'indexed' AND \
                (EXISTS (SELECT 1 FROM local_track_genres g \
                 WHERE g.local_track_id = t.id AND g.folded_name = ?) \
                 OR t.genre_folded = ?)";
            let rows = sqlx::query(&format!(
                "SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE {filter} \
                 ORDER BY t.artist_name_folded ASC, t.album_title_folded ASC, \
                 t.disc_number ASC, t.track_number ASC, t.id ASC LIMIT ? OFFSET ?"
            ))
            .bind(&folded)
            .bind(&folded)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.genres.tracks", error))?;
            let row = sqlx::query(&format!(
                "SELECT COUNT(*) AS total FROM local_tracks t WHERE {filter}"
            ))
            .bind(&folded)
            .bind(&folded)
            .fetch_one(pool)
            .await
            .map_err(|error| internal("library.genres.tracks.count", error))?;
            let total: i64 = row.get("total");
            Ok((rows.iter().map(map_track).collect(), total.max(0) as u64))
        })
    }

    fn decades<'a>(&'a self) -> BoxFuture<'a, Result<Vec<DecadeRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(
                "SELECT (year / 10 * 10) AS decade, COUNT(*) AS album_count \
                 FROM local_albums WHERE year IS NOT NULL \
                 GROUP BY decade ORDER BY decade ASC",
            )
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.decades", error))?;
            Ok(rows
                .iter()
                .map(|row| {
                    let album_count: i64 = row.get("album_count");
                    DecadeRecord {
                        decade: row.get("decade"),
                        album_count: album_count.max(0) as u64,
                    }
                })
                .collect())
        })
    }

    fn newest_tracks<'a>(
        &'a self,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(&format!(
                "SELECT {TRACK_COLUMNS} {TRACK_JOINS} \
                 WHERE t.availability = 'indexed' \
                 ORDER BY t.imported_at DESC, t.id ASC LIMIT ?"
            ))
            .bind(limit as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.tracks.newest", error))?;
            Ok(rows.iter().map(map_track).collect())
        })
    }

    fn oldest_tracks<'a>(
        &'a self,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(&format!(
                "SELECT {TRACK_COLUMNS} {TRACK_JOINS} \
                 WHERE t.availability = 'indexed' \
                 ORDER BY t.imported_at ASC, t.id ASC LIMIT ?"
            ))
            .bind(limit as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.tracks.oldest", error))?;
            Ok(rows.iter().map(map_track).collect())
        })
    }

    fn random_tracks<'a>(
        &'a self,
        limit: u64,
        decade: Option<i64>,
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(&format!(
                "SELECT {TRACK_COLUMNS} {TRACK_JOINS} \
                 WHERE t.availability = 'indexed' \
                 AND (? IS NULL OR (t.year >= ? AND t.year < ? + 10)) \
                 ORDER BY RANDOM() LIMIT ?"
            ))
            .bind(decade)
            .bind(decade)
            .bind(decade)
            .bind(limit as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.tracks.random", error))?;
            Ok(rows.iter().map(map_track).collect())
        })
    }
}

/// Favorite reads over the reader pool.
#[derive(Clone, Debug)]
pub struct SqliteFavorites {
    db: LibraryDb,
}

impl SqliteFavorites {
    /// Adapter over one handle.
    pub fn new(db: &LibraryDb) -> Self {
        Self { db: db.clone() }
    }
}

impl FavoriteReads for SqliteFavorites {
    fn filter_favorites<'a>(
        &'a self,
        user_id: &'a str,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<std::collections::HashSet<String>, StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            if ids.is_empty() {
                return Ok(std::collections::HashSet::new());
            }
            let placeholders = vec!["?"; ids.len()].join(", ");
            let sql = format!(
                "SELECT item_id FROM library_user_favorites \
                 WHERE user_id = ? AND item_kind = ? AND item_id IN ({placeholders})"
            );
            let mut query = sqlx::query(&sql).bind(user_id).bind(kind);
            for id in ids {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(pool)
                .await
                .map_err(|error| internal("library.favorites.filter", error))?;
            Ok(rows
                .iter()
                .map(|row| row.get::<String, _>("item_id"))
                .collect())
        })
    }

    fn favorite_counts<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(u64, u64, u64), StoreError>> {
        Box::pin(async move {
            let Some(pool) = self.db.live() else {
                return Err(unwired_store());
            };
            let rows = sqlx::query(
                "SELECT item_kind AS kind, COUNT(*) AS total \
                 FROM library_user_favorites WHERE user_id = ? GROUP BY item_kind",
            )
            .bind(user_id)
            .fetch_all(pool)
            .await
            .map_err(|error| internal("library.favorites.counts", error))?;
            let mut albums = 0;
            let mut artists = 0;
            let mut tracks = 0;
            for row in &rows {
                let kind: String = row.get("kind");
                let total: i64 = row.get("total");
                match kind.as_str() {
                    "album" => albums = total.max(0) as u64,
                    "artist" => artists = total.max(0) as u64,
                    "track" => tracks = total.max(0) as u64,
                    _ => {}
                }
            }
            Ok((albums, artists, tracks))
        })
    }
}
