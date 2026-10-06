//! Read budget tests: the six standard reads hold the 10 ms p95 budget on
//! a 100k catalog.
//!
//! The rewritten shapes prove equal to the legacy SQL they replace on an
//! adversarial fixture (differential tests), the maintained totals prove
//! equal to live aggregation across a mutation battery, the miss oracle
//! proves equal to LIKE across a hostile query matrix, the hot plans prove
//! index-driven via EXPLAIN, and the ignored acceptance test times all six
//! endpoints on a seeded 100k catalog. Nothing here touches the network;
//! every database is a scratch file.

use crate::reads::library;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::db::{DbConfig, DbRuntime, Lane, OpError, fold_text, open_runtime};
use crate::schema::{MIGRATOR, apply_migrations, latest_version};
use library::sqlite::{
    ALBUM_COLUMNS, ALBUM_FILTER, ALBUM_JOINS, ARTIST_CREDIT_COUNTS, LibraryDb, STATS_FORMATS,
    STATS_MAIN, SqliteCatalog, TRACK_COLUMNS, TRACK_FILTER, TRACK_JOINS, TRACK_MISS_ORACLE,
    album_order, artist_order, artist_scope_predicate, fts_match_phrase, track_order,
};
use library::stores::{
    AlbumFilter, AlbumRecord, AlbumSort, ArtistRecord, ArtistScope, ArtistSort, LibraryCatalog,
    StatsRecord, TrackFilter, TrackRecord, TrackSort,
};
use sqlx::Row as _;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

static PERF_SEQ: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    runtime: DbRuntime,
    pool: SqlitePool,
    catalog: SqliteCatalog,
    /// Dropped last: removes the scratch database.
    _dir: crate::tooling::scratch::ScratchDir,
}

async fn seed_small() -> Fixture {
    let seq = PERF_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = crate::tooling::scratch::ScratchDir::new(&format!("reads-perf-{seq}"))
        .expect("scratch dir");
    let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
        .await
        .expect("scratch runtime opens");
    let pool = runtime.pool().clone();
    let seed = seed_small_sql();
    runtime
        .lane()
        .write(Lane::Foreground, "perf seed", move |tx| {
            tx.execute_batch(&seed).map_err(OpError::from)?;
            Ok(())
        })
        .await
        .expect("seed batch runs");
    let db = LibraryDb::new(&pool);
    Fixture {
        runtime,
        pool,
        catalog: SqliteCatalog::new(&db),
        _dir: dir,
    }
}

/// Adversarial catalog: a double credit (DISTINCT), a guest appearance, a
/// zero-track album, missing/excluded tracks, a retired artist, LIKE-tricky
/// text (quotes, percent, underscore, apostrophe, unicode), short titles,
/// and a NULL artist fold. Folded values go through the same fold the
/// readers use, exactly like production writes.
fn seed_small_sql() -> String {
    fn sql(text: String) -> String {
        text.replace('\'', "''")
    }
    let a3 = sql(fold_text("Gæst \"Quoted\" 100%"));
    let al5 = sql(fold_text("100% \"Live\" _Unplugged_ 'Encore'"));
    let t7 = sql(fold_text("Æther \"Mix\" 100%_Sure"));
    let t9 = sql(fold_text("Ox \"Q\" %"));
    format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) VALUES \
         ('a1', 'Aurora', 'aurora', 'person', 1000, 1000), \
         ('a2', 'Boreal', 'boreal', 'group', 1500, 1500), \
         ('a3', 'Gæst \"Quoted\" 100%', '{a3}', 'person', 1600, 1600), \
         ('ax', 'Retired', 'retired', 'person', 900, 900);\n\
         UPDATE local_artists SET retired_into_artist_id = 'a1' WHERE id = 'ax';\n\
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, \
         grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'First Light', 'first light', 'Aurora', 'aurora', 'a1', 1994, 'automatic', 1000, 1000), \
         ('al2', 'r1', 'g2', 'Second Dawn', 'second dawn', 'Aurora', 'aurora', 'a1', 2001, 'automatic', 2000, 2000), \
         ('al3', 'r1', 'g3', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', 'a2', 1994, 'automatic', 1500, 1500), \
         ('al4', 'r1', 'g4', 'Empty Halls', 'empty halls', 'Aurora', 'aurora', 'a1', 1994, 'automatic', 1200, 1200), \
         ('al5', 'r1', 'g5', '100% \"Live\" _Unplugged_ ''Encore''', '{al5}', 'Boreal', 'boreal', 'a2', 1987, 'automatic', 1800, 1800);\n\
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, \
         file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, artist_name, artist_name_folded, \
         album_title, album_title_folded, album_artist_name, album_artist_name_folded, \
         disc_number, track_number, year, duration_seconds, file_format, availability, \
         ingest_source, imported_at, membership_source) VALUES \
         ('t1', 'al1', 'r1', '/m/t1.flac', 't1.flac', 'h1', 4000000, 1, 's1', 'Opener', 'opener', \
          'Aurora', 'aurora', 'First Light', 'first light', 'Aurora', 'aurora', 1, 1, 1994, 200.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic'), \
         ('t2', 'al1', 'r1', '/m/t2.flac', 't2.flac', 'h2', 4000000, 1, 's2', 'Closer', 'closer', \
          'Aurora', 'aurora', 'First Light', 'first light', 'Aurora', 'aurora', 1, 2, 1994, 200.0, 'flac', 'indexed', 'scan', 1100.0, 'automatic'), \
         ('t3', 'al1', 'r1', '/m/t3.flac', 't3.flac', 'h3', 4000000, 1, 's3', 'Gone', 'gone', \
          'Aurora', 'aurora', 'First Light', 'first light', 'Aurora', 'aurora', 1, 3, 1994, 200.0, 'flac', 'missing', 'scan', 1150.0, 'automatic'), \
         ('t4', 'al2', 'r1', '/m/t4.mp3', 't4.mp3', 'h4', 3000000, 1, 's4', 'Single', 'single', \
          'Aurora', 'aurora', 'Second Dawn', 'second dawn', 'Aurora', 'aurora', 1, 1, 2001, 180.0, 'mp3', 'indexed', 'scan', 2000.0, 'automatic'), \
         ('t5', 'al2', 'r1', '/m/t5.mp3', 't5.mp3', 'h5', 3000000, 1, 's5', 'Skipped', 'skipped', \
          'Aurora', 'aurora', 'Second Dawn', 'second dawn', 'Aurora', 'aurora', 1, 2, 2001, 180.0, 'mp3', 'excluded', 'scan', 2050.0, 'automatic'), \
         ('t6', 'al3', 'r1', '/m/t6.flac', 't6.flac', 'h6', 5000000, 1, 's6', 'Peak', 'peak', \
          'Boreal', 'boreal', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', 1, 1, 1994, 220.0, 'flac', 'indexed', 'scan', 1500.0, 'automatic'), \
         ('t7', 'al3', 'r1', '/m/t7.ogg', 't7.ogg', 'h7', 1000000, 1, 's7', 'Æther \"Mix\" 100%_Sure', '{t7}', \
          'Aurora', 'aurora', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', 1, 2, 1994, 210.0, 'ogg', 'indexed', 'scan', 1600.0, 'automatic'), \
         ('t8', 'al5', 'r1', '/m/t8.opus', 't8.opus', 'h8', 2000000, 1, 's8', 'Up', 'up', \
          'Boreal', 'boreal', '100% \"Live\" _Unplugged_ ''Encore''', '{al5}', 'Boreal', 'boreal', 1, 1, 1987, 190.0, 'opus', 'indexed', 'scan', 1800.0, 'automatic'), \
         ('t9', 'al5', 'r1', '/m/t9.opus', 't9.opus', 'h9', 2000000, 1, 's9', 'Ox \"Q\" %', '{t9}', \
          NULL, NULL, '100% \"Live\" _Unplugged_ ''Encore''', '{al5}', 'Boreal', 'boreal', 1, 2, 1987, 195.0, 'opus', 'indexed', 'scan', 1900.0, 'automatic');\n\
         INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) VALUES \
         ('t1', 0, 'a1', 'main'), ('t2', 0, 'a1', 'main'), ('t3', 0, 'a1', 'main'), \
         ('t4', 0, 'a1', 'main'), ('t5', 0, 'a1', 'main'), \
         ('t6', 0, 'a2', 'main'), ('t6', 1, 'a3', 'guest'), \
         ('t7', 0, 'a1', 'main'), ('t7', 1, 'a1', 'main'), \
         ('t8', 0, 'a2', 'main'), ('t9', 0, 'a2', 'main');\n\
         INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) VALUES \
         ('al1', 0, 'a1', 'main'), ('al2', 0, 'a1', 'main'), ('al3', 0, 'a2', 'main'), \
         ('al4', 0, 'a1', 'main'), ('al5', 0, 'a2', 'main');\n\
         INSERT INTO local_album_external_identities \
         (local_album_id, provider, release_group_mbid, release_mbid, decision_source, selected_at) VALUES \
         ('al1', 'musicbrainz', 'rg1', 'r1', 'manual', 1000);\n\
         INSERT INTO local_artist_external_identities \
         (local_artist_id, provider, provider_artist_id, decision_source, selected_at) VALUES \
         ('a1', 'musicbrainz', 'm1', 'manual', 1000);\n\
         INSERT INTO local_album_artwork (local_album_id, source, updated_at) VALUES \
         ('al1', 'embedded', 1000);\n\
         INSERT INTO local_track_genres (local_track_id, position, name, folded_name, source) VALUES \
         ('t1', 0, 'Rock', 'rock', 'local'), ('t7', 0, 'Jazz', 'jazz', 'local');\n"
    )
}

// Legacy oracles: independent copies of the pre-0005 query shapes. The
// differential tests run these beside the rewritten adapters and demand
// identical pages and totals. If the reads ever change shape again, these
// fail loudly and force a conscious oracle review.

const LEGACY_ALBUM_COLUMNS: &str = "a.id AS id, a.title AS title, \
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

const LEGACY_ALBUM_JOINS: &str = "FROM local_albums a \
    LEFT JOIN local_artists an ON an.id = a.album_artist_id \
    LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id \
    LEFT JOIN local_artist_external_identities rte ON rte.local_artist_id = a.album_artist_id \
    LEFT JOIN local_album_artwork w ON w.local_album_id = a.id \
    LEFT JOIN local_tracks t ON t.local_album_id = a.id AND t.availability = 'indexed'";

const LEGACY_ALBUM_FILTER: &str = "(? IS NULL OR a.title_folded LIKE ? ESCAPE '\\' \
    OR a.album_artist_name_folded LIKE ? ESCAPE '\\') \
    AND (? IS NULL OR a.album_artist_id = ?) \
    AND (? IS NULL OR (a.year >= ? AND a.year < ? + 10)) \
    AND (? IS NULL OR LOWER((SELECT t2.file_format FROM local_tracks t2 \
     WHERE t2.local_album_id = a.id AND t2.availability = 'indexed' \
     GROUP BY t2.file_format ORDER BY COUNT(*) DESC, t2.file_format LIMIT 1)) = ?)";

const LEGACY_TRACK_COLUMNS: &str = "t.id AS id, t.title AS title, \
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

const LEGACY_TRACK_JOINS: &str = "FROM local_tracks t \
    LEFT JOIN local_track_artists ta \
        ON ta.local_track_id = t.id AND ta.position = 0 \
    LEFT JOIN local_album_artwork w ON w.local_album_id = t.local_album_id";

const LEGACY_TRACK_FILTER: &str = "t.availability = 'indexed' \
    AND (? IS NULL OR t.title_folded LIKE ? ESCAPE '\\' \
    OR t.artist_name_folded LIKE ? ESCAPE '\\' \
    OR t.album_title_folded LIKE ? ESCAPE '\\') \
    AND (? IS NULL OR t.local_album_id = ?) \
    AND (? IS NULL OR EXISTS (SELECT 1 FROM local_track_artists x \
    WHERE x.local_track_id = t.id AND x.local_artist_id = ?)) \
    AND (? IS NULL OR EXISTS (SELECT 1 FROM local_track_genres g \
    WHERE g.local_track_id = t.id AND g.folded_name = ?) \
    OR t.genre_folded = ?)";

const LEGACY_ARTIST_COLUMNS: &str = "r.id AS id, r.display_name AS name, \
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

const LEGACY_ARTIST_JOINS: &str = "FROM local_artists r \
    LEFT JOIN local_artist_external_identities e ON e.local_artist_id = r.id";

const LEGACY_LED: &str = "EXISTS (SELECT 1 FROM local_albums la WHERE la.album_artist_id = r.id)";

const LEGACY_CREDITED: &str = "(EXISTS (SELECT 1 FROM local_album_artists laa \
    WHERE laa.local_artist_id = r.id) \
    OR EXISTS (SELECT 1 FROM local_track_artists lta WHERE lta.local_artist_id = r.id) \
    OR EXISTS (SELECT 1 FROM local_albums la WHERE la.album_artist_id = r.id))";

fn legacy_like_pattern(raw: &str) -> String {
    let folded = fold_text(raw)
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{folded}%")
}

fn legacy_album_order(sort: AlbumSort, descending: bool) -> &'static str {
    match (sort, descending) {
        (AlbumSort::Name, false) => "a.title_folded ASC, a.id ASC",
        (AlbumSort::Name, true) => "a.title_folded DESC, a.id ASC",
        (AlbumSort::DateAdded, false) => "a.created_at ASC, a.id ASC",
        (AlbumSort::DateAdded, true) => "a.created_at DESC, a.id ASC",
        (AlbumSort::Year, false) => "(a.year IS NULL) ASC, a.year ASC, a.id ASC",
        (AlbumSort::Year, true) => "(a.year IS NULL) ASC, a.year DESC, a.id ASC",
        (AlbumSort::Artist, false) => {
            "a.album_artist_name_folded ASC, a.title_folded ASC, a.id ASC"
        }
        (AlbumSort::Artist, true) => {
            "a.album_artist_name_folded DESC, a.title_folded DESC, a.id ASC"
        }
        (AlbumSort::Random, _) => "RANDOM()",
        (AlbumSort::Rediscover, false) => "a.created_at ASC, a.id ASC",
        (AlbumSort::Rediscover, true) => "a.created_at DESC, a.id ASC",
    }
}

fn legacy_artist_order(sort: ArtistSort, descending: bool) -> &'static str {
    match (sort, descending) {
        (ArtistSort::Name, false) => "r.folded_name ASC, r.id ASC",
        (ArtistSort::Name, true) => "r.folded_name DESC, r.id ASC",
        (ArtistSort::AlbumCount, false) => "album_count ASC, r.folded_name ASC, r.id ASC",
        (ArtistSort::AlbumCount, true) => "album_count DESC, r.folded_name ASC, r.id ASC",
        (ArtistSort::DateAdded, false) => "r.created_at ASC, r.id ASC",
        (ArtistSort::DateAdded, true) => "r.created_at DESC, r.id ASC",
    }
}

fn legacy_track_order(sort: TrackSort, descending: bool) -> &'static str {
    match (sort, descending) {
        (TrackSort::Title, false) => "t.title_folded ASC, t.id ASC",
        (TrackSort::Title, true) => "t.title_folded DESC, t.id ASC",
        (TrackSort::DateAdded, false) => "t.imported_at ASC, t.id ASC",
        (TrackSort::DateAdded, true) => "t.imported_at DESC, t.id ASC",
    }
}

fn legacy_scope(scope: ArtistScope) -> String {
    let base = match scope {
        ArtistScope::All => "1 = 1",
        ArtistScope::AlbumArtists => "(LED)",
        ArtistScope::Contributors => "(NOT (LED) AND (CREDITED))",
    };
    base.replace("(LED)", LEGACY_LED)
        .replace("(CREDITED)", LEGACY_CREDITED)
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

async fn legacy_albums(
    pool: &SqlitePool,
    filter: &AlbumFilter,
    sort: AlbumSort,
    descending: bool,
    limit: u64,
    offset: u64,
) -> (Vec<AlbumRecord>, u64) {
    let pattern = filter.q.as_deref().map(legacy_like_pattern);
    let order = legacy_album_order(sort, descending);
    let rows = sqlx::query(&format!(
        "SELECT {LEGACY_ALBUM_COLUMNS} {LEGACY_ALBUM_JOINS} WHERE {LEGACY_ALBUM_FILTER} \
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
    .bind(filter.format.as_deref())
    .bind(filter.format.as_deref())
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(pool)
    .await
    .expect("oracle lists");
    let row = sqlx::query(&format!(
        "SELECT COUNT(*) AS total FROM local_albums a WHERE {LEGACY_ALBUM_FILTER}"
    ))
    .bind(pattern.as_deref())
    .bind(pattern.as_deref())
    .bind(pattern.as_deref())
    .bind(filter.artist_id.as_deref())
    .bind(filter.artist_id.as_deref())
    .bind(filter.decade)
    .bind(filter.decade)
    .bind(filter.decade)
    .bind(filter.format.as_deref())
    .bind(filter.format.as_deref())
    .fetch_one(pool)
    .await
    .expect("oracle counts");
    let total: i64 = row.get("total");
    (rows.iter().map(map_album).collect(), total.max(0) as u64)
}

async fn legacy_artists(
    pool: &SqlitePool,
    scope: ArtistScope,
    q: Option<&str>,
    sort: ArtistSort,
    descending: bool,
    limit: u64,
    offset: u64,
) -> (Vec<ArtistRecord>, u64, u64, u64) {
    let pattern = q.map(legacy_like_pattern);
    let scope_sql = legacy_scope(scope);
    let order = legacy_artist_order(sort, descending);
    let rows = sqlx::query(&format!(
        "SELECT {LEGACY_ARTIST_COLUMNS} {LEGACY_ARTIST_JOINS} \
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
    .expect("oracle lists");
    let totals = sqlx::query(&format!(
        "SELECT COUNT(*) AS total, \
         COALESCE(SUM(CASE WHEN {LEGACY_LED} THEN 1 ELSE 0 END), 0) AS led, \
         COALESCE(SUM(CASE WHEN NOT ({LEGACY_LED}) AND ({LEGACY_CREDITED}) \
         THEN 1 ELSE 0 END), 0) AS contributors \
         {LEGACY_ARTIST_JOINS} WHERE r.retired_into_artist_id IS NULL \
         AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\')"
    ))
    .bind(pattern.as_deref())
    .bind(pattern.as_deref())
    .fetch_one(pool)
    .await
    .expect("oracle totals");
    let mut total: i64 = totals.get("total");
    let led: i64 = totals.get("led");
    let contributors: i64 = totals.get("contributors");
    if !matches!(scope, ArtistScope::All) {
        let scoped = sqlx::query(&format!(
            "SELECT COUNT(*) AS total {LEGACY_ARTIST_JOINS} \
             WHERE r.retired_into_artist_id IS NULL AND ({scope_sql}) \
             AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\')"
        ))
        .bind(pattern.as_deref())
        .bind(pattern.as_deref())
        .fetch_one(pool)
        .await
        .expect("oracle scoped counts");
        total = scoped.get("total");
    }
    (
        rows.iter().map(map_artist).collect(),
        total.max(0) as u64,
        led.max(0) as u64,
        contributors.max(0) as u64,
    )
}

async fn legacy_artist(pool: &SqlitePool, id: &str) -> Option<ArtistRecord> {
    let row = sqlx::query(&format!(
        "SELECT {LEGACY_ARTIST_COLUMNS} {LEGACY_ARTIST_JOINS} \
         WHERE r.id = ? AND r.retired_into_artist_id IS NULL"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .expect("oracle reads");
    row.as_ref().map(map_artist)
}

async fn legacy_tracks(
    pool: &SqlitePool,
    filter: &TrackFilter,
    sort: TrackSort,
    descending: bool,
    limit: u64,
    offset: u64,
) -> (Vec<TrackRecord>, u64) {
    let pattern = filter.q.as_deref().map(legacy_like_pattern);
    let genre = filter.genre.as_deref().map(|name| fold_text(name.trim()));
    let order = legacy_track_order(sort, descending);
    let rows = sqlx::query(&format!(
        "SELECT {LEGACY_TRACK_COLUMNS} {LEGACY_TRACK_JOINS} WHERE {LEGACY_TRACK_FILTER} \
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
    .expect("oracle lists");
    let row = sqlx::query(&format!(
        "SELECT COUNT(*) AS total FROM local_tracks t WHERE {LEGACY_TRACK_FILTER}"
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
    .expect("oracle counts");
    let total: i64 = row.get("total");
    (rows.iter().map(map_track).collect(), total.max(0) as u64)
}

/// Live aggregation over the base tables: the ground truth the maintained
/// totals must match after every mutation.
async fn ground_stats(pool: &SqlitePool) -> StatsRecord {
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
    .expect("ground totals read");
    let rows = sqlx::query(
        "SELECT file_format AS format, COUNT(*) AS total FROM local_tracks \
         WHERE availability = 'indexed' GROUP BY file_format",
    )
    .fetch_all(pool)
    .await
    .expect("ground formats read");
    let mut format_breakdown = HashMap::new();
    for row in &rows {
        let format: String = row.get("format");
        let total: i64 = row.get("total");
        format_breakdown.insert(format, total.max(0) as u64);
    }
    let albums: i64 = row.get("albums");
    let artists: i64 = row.get("artists");
    let tracks: i64 = row.get("tracks");
    StatsRecord {
        total_albums: albums.max(0) as u64,
        total_artists: artists.max(0) as u64,
        total_tracks: tracks.max(0) as u64,
        total_size_bytes: row.get("size_bytes"),
        format_breakdown,
    }
}

async fn write_batch(fixture: &Fixture, label: &'static str, sql: String) {
    fixture
        .runtime
        .lane()
        .write(Lane::Foreground, label, move |tx| {
            tx.execute_batch(&sql).map_err(OpError::from)?;
            Ok(())
        })
        .await
        .expect("write batch runs");
}

/// One EXPLAIN bind in position: text (NULL when absent) or integer.
#[derive(Clone, Copy)]
enum ExplainBind<'a> {
    Text(Option<&'a str>),
    Int(i64),
}

async fn explain(pool: &SqlitePool, sql: &str, binds: &[ExplainBind<'_>]) -> Vec<String> {
    let text = format!("EXPLAIN QUERY PLAN {sql}");
    let mut query = sqlx::query_as::<_, (i64, i64, i64, String)>(&text);
    for bind in binds {
        query = match *bind {
            ExplainBind::Text(value) => query.bind(value),
            ExplainBind::Int(value) => query.bind(value),
        };
    }
    query
        .fetch_all(pool)
        .await
        .expect("plan reads")
        .into_iter()
        .map(|row| row.3)
        .collect()
}

async fn migrate_through(pool: &SqlitePool, version: i64) {
    let selected: Vec<_> = MIGRATOR
        .migrations
        .iter()
        .filter(|migration| migration.version <= version)
        .cloned()
        .collect();
    let base = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(selected),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    base.run(pool).await.expect("base migrates");
    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await
        .expect("stamp reads");
    assert_eq!(stamped, version);
}

/// Page-first album reads return exactly the legacy pages: every sort in
/// both directions across filters, paging edges, and the zero-track album.
/// Random order compares as a set plus total; every other sort compares in
/// order, proving the id-page reassembly.
#[tokio::test]
async fn albums_page_first_matches_legacy() {
    let fixture = seed_small().await;
    let sorts = [
        (AlbumSort::Name, false),
        (AlbumSort::Name, true),
        (AlbumSort::DateAdded, false),
        (AlbumSort::DateAdded, true),
        (AlbumSort::Year, false),
        (AlbumSort::Year, true),
        (AlbumSort::Artist, false),
        (AlbumSort::Artist, true),
        (AlbumSort::Rediscover, false),
        (AlbumSort::Rediscover, true),
    ];
    let cases: Vec<(AlbumFilter, u64, u64)> = vec![
        (AlbumFilter::default(), 50, 0),
        (
            AlbumFilter {
                q: Some("light".to_owned()),
                ..Default::default()
            },
            50,
            0,
        ),
        (
            AlbumFilter {
                q: Some("zzz-no-such-album".to_owned()),
                ..Default::default()
            },
            50,
            0,
        ),
        (
            AlbumFilter {
                q: Some("100% \"live\"".to_owned()),
                ..Default::default()
            },
            50,
            0,
        ),
        (
            AlbumFilter {
                artist_id: Some("a1".to_owned()),
                ..Default::default()
            },
            50,
            0,
        ),
        (
            AlbumFilter {
                decade: Some(1990),
                ..Default::default()
            },
            50,
            0,
        ),
        (
            AlbumFilter {
                format: Some("flac".to_owned()),
                ..Default::default()
            },
            50,
            0,
        ),
        (AlbumFilter::default(), 2, 1),
        (AlbumFilter::default(), 50, 99),
    ];
    for (sort, descending) in sorts {
        for (filter, limit, offset) in &cases {
            let got = fixture
                .catalog
                .list_albums(filter, sort, descending, *limit, *offset)
                .await
                .expect("lists");
            let want =
                legacy_albums(&fixture.pool, filter, sort, descending, *limit, *offset).await;
            assert_eq!(
                got, want,
                "sort={sort:?} desc={descending} filter={filter:?} limit={limit} offset={offset}"
            );
        }
    }
    let (mut got, got_total) = fixture
        .catalog
        .list_albums(&AlbumFilter::default(), AlbumSort::Random, false, 50, 0)
        .await
        .expect("lists");
    let (mut want, want_total) = legacy_albums(
        &fixture.pool,
        &AlbumFilter::default(),
        AlbumSort::Random,
        false,
        50,
        0,
    )
    .await;
    assert_eq!(got_total, want_total);
    assert_eq!(got_total, 5);
    got.sort_by(|a, b| a.id.cmp(&b.id));
    want.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(got, want);
}

/// Batched artist reads return exactly the legacy pages and totals: every
/// scope and sort in both directions, text filters, the double-credit
/// DISTINCT case, zero-count album sorts, and single gets including retired
/// and unknown ids.
#[tokio::test]
async fn artists_batched_matches_legacy() {
    let fixture = seed_small().await;
    let sorts = [
        (ArtistSort::Name, false),
        (ArtistSort::Name, true),
        (ArtistSort::AlbumCount, false),
        (ArtistSort::AlbumCount, true),
        (ArtistSort::DateAdded, false),
        (ArtistSort::DateAdded, true),
    ];
    let scopes = [
        ArtistScope::All,
        ArtistScope::AlbumArtists,
        ArtistScope::Contributors,
    ];
    for scope in scopes {
        for (sort, descending) in sorts {
            for (q, limit, offset) in [
                (None, 50, 0),
                (None, 2, 1),
                (None, 50, 99),
                (Some("a"), 50, 0),
            ] {
                let got = fixture
                    .catalog
                    .list_artists(scope, q, sort, descending, limit, offset)
                    .await
                    .expect("lists");
                let want =
                    legacy_artists(&fixture.pool, scope, q, sort, descending, limit, offset).await;
                assert_eq!(
                    got, want,
                    "scope={scope:?} sort={sort:?} desc={descending} q={q:?}"
                );
            }
        }
    }
    for q in ["zzz-no-such-artist", "gæst", "100%", "\"quoted\""] {
        let got = fixture
            .catalog
            .list_artists(ArtistScope::All, Some(q), ArtistSort::Name, false, 50, 0)
            .await
            .expect("lists");
        let want = legacy_artists(
            &fixture.pool,
            ArtistScope::All,
            Some(q),
            ArtistSort::Name,
            false,
            50,
            0,
        )
        .await;
        assert_eq!(got, want, "q={q:?}");
    }
    // Pin the interesting counts directly, not just differentially.
    let (records, total, led, contributors) = fixture
        .catalog
        .list_artists(ArtistScope::All, None, ArtistSort::Name, false, 50, 0)
        .await
        .expect("lists");
    assert_eq!((total, led, contributors), (5, 2, 1));
    let by_id: HashMap<&str, &ArtistRecord> = records
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    assert_eq!(
        (by_id["a1"].album_count, by_id["a1"].track_count),
        (3, 4),
        "double credit counts once"
    );
    assert_eq!(by_id["a1"].appearance_album_count, 1);
    assert_eq!(
        (
            by_id["a3"].album_count,
            by_id["a3"].track_count,
            by_id["a3"].appearance_album_count
        ),
        (0, 1, 1)
    );
    for id in ["a1", "a2", "a3", "ax", "nope"] {
        let got = fixture.catalog.get_artist(id).await.expect("reads");
        let want = legacy_artist(&fixture.pool, id).await;
        assert_eq!(got, want, "id={id}");
    }
    assert_eq!(fixture.catalog.get_artist("ax").await.expect("reads"), None);
}

/// The trigram miss oracle agrees with LIKE on every query shape: misses,
/// hits, 1-2 character queries (LIKE path), LIKE metacharacters, quotes,
/// unicode, case folding, NUL bytes, and misses combined with narrowing
/// filters.
#[tokio::test]
async fn tracks_miss_oracle_matches_like() {
    let fixture = seed_small().await;
    assert_eq!(fts_match_phrase("pe"), None);
    assert_eq!(fts_match_phrase("perf"), Some("\"perf\"".to_owned()));
    assert_eq!(fts_match_phrase("a\"b"), Some("\"a\"\"b\"".to_owned()));
    assert_eq!(fts_match_phrase("ab\0cd"), None);
    assert_eq!(fts_match_phrase("æø"), None);
    let queries = [
        "zzz-no-such-thing-zzz",
        "opener",
        "aurora",
        "lone peak",
        "æther",
        "ÆTHER",
        "\"mix\"",
        "100%",
        "_sure",
        "%",
        "_",
        "\\",
        "a\"b",
        "up",
        "o",
        "e",
        "al",
        "zz",
        "UP",
        "Ox \"Q\"",
        "Gæst",
        "  spaced  ",
        "ab\0cd",
        "peak tunggal",
        "NUL",
    ];
    for q in queries {
        for (sort, descending) in [(TrackSort::Title, false), (TrackSort::DateAdded, true)] {
            let filter = TrackFilter {
                q: Some(q.to_owned()),
                ..Default::default()
            };
            let got = fixture
                .catalog
                .list_tracks(&filter, sort, descending, 20, 0)
                .await
                .expect("lists");
            let want = legacy_tracks(&fixture.pool, &filter, sort, descending, 20, 0).await;
            assert_eq!(got, want, "q={q:?} sort={sort:?} desc={descending}");
        }
    }
    // A text miss stays empty under narrowing filters.
    for filter in [
        TrackFilter {
            q: Some("zzz-no-such-thing-zzz".to_owned()),
            album_id: Some("al1".to_owned()),
            ..Default::default()
        },
        TrackFilter {
            q: Some("zzz-no-such-thing-zzz".to_owned()),
            artist_id: Some("a1".to_owned()),
            ..Default::default()
        },
        TrackFilter {
            q: Some("zzz-no-such-thing-zzz".to_owned()),
            genre: Some("rock".to_owned()),
            ..Default::default()
        },
    ] {
        let (items, total) = fixture
            .catalog
            .list_tracks(&filter, TrackSort::Title, false, 20, 0)
            .await
            .expect("lists");
        assert!(items.is_empty() && total == 0, "filter={filter:?}");
    }
    // Filtered (non-text) pages still match the oracle exactly.
    for filter in [
        TrackFilter {
            album_id: Some("al1".to_owned()),
            ..Default::default()
        },
        TrackFilter {
            artist_id: Some("a1".to_owned()),
            ..Default::default()
        },
        TrackFilter {
            genre: Some("jazz".to_owned()),
            ..Default::default()
        },
        TrackFilter {
            q: Some("a".to_owned()),
            genre: Some("rock".to_owned()),
            ..Default::default()
        },
    ] {
        let got = fixture
            .catalog
            .list_tracks(&filter, TrackSort::Title, false, 50, 0)
            .await
            .expect("lists");
        let want = legacy_tracks(&fixture.pool, &filter, TrackSort::Title, false, 50, 0).await;
        assert_eq!(got, want, "filter={filter:?}");
    }
}

/// Maintained format totals track every mutation: inserts in each
/// availability, transitions between them, format and size changes,
/// unrelated updates, deletes, and format extinction. Stats and the
/// unfiltered track total match live aggregation after each step.
#[tokio::test]
async fn stats_maintained_totals_track_mutations() {
    let fixture = seed_small().await;
    async fn check(fixture: &Fixture, step: &str) {
        let stats = fixture.catalog.stats().await.expect("stats read");
        let ground = ground_stats(&fixture.pool).await;
        assert_eq!(stats, ground, "stats at {step}");
        let (_, total) = fixture
            .catalog
            .list_tracks(&TrackFilter::default(), TrackSort::Title, false, 50, 0)
            .await
            .expect("tracks list");
        assert_eq!(total, ground.total_tracks, "unfiltered total at {step}");
    }
    check(&fixture, "seed").await;
    write_batch(
        &fixture,
        "insert indexed",
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, \
         file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, album_title, \
         album_title_folded, disc_number, track_number, duration_seconds, file_format, \
         availability, ingest_source, imported_at, membership_source) VALUES \
         ('m1', 'al1', 'r1', '/m/m1', 'm1', 'hm1', 7000000, 1, 'sm1', 'Fresh', 'fresh', \
         'First Light', 'first light', 1, 9, 200.0, 'wav', 'indexed', 'scan', 3000.0, 'automatic');"
            .to_owned(),
    )
    .await;
    check(&fixture, "insert indexed").await;
    write_batch(
        &fixture,
        "insert missing and excluded",
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, \
         file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, album_title, \
         album_title_folded, disc_number, track_number, duration_seconds, file_format, \
         availability, ingest_source, imported_at, membership_source) VALUES \
         ('m2', 'al1', 'r1', '/m/m2', 'm2', 'hm2', 100, 1, 'sm2', 'Lost', 'lost', \
         'First Light', 'first light', 1, 10, 200.0, 'wav', 'missing', 'scan', 3001.0, 'automatic'), \
         ('m3', 'al1', 'r1', '/m/m3', 'm3', 'hm3', 100, 1, 'sm3', 'Skipped', 'skipped', \
         'First Light', 'first light', 1, 11, 200.0, 'wav', 'excluded', 'scan', 3002.0, 'automatic');"
            .to_owned(),
    )
    .await;
    check(&fixture, "insert missing and excluded").await;
    for (step, sql) in [
        (
            "to missing",
            "UPDATE local_tracks SET availability = 'missing' WHERE id = 'm1';",
        ),
        (
            "to excluded",
            "UPDATE local_tracks SET availability = 'excluded' WHERE id = 'm1';",
        ),
        (
            "back to indexed",
            "UPDATE local_tracks SET availability = 'indexed' WHERE id = 'm1';",
        ),
        (
            "format and size change",
            "UPDATE local_tracks SET file_format = 'flac', file_size_bytes = 100 WHERE id = 'm1';",
        ),
        (
            "size-only change",
            "UPDATE local_tracks SET file_size_bytes = 200 WHERE id = 'm1';",
        ),
        (
            "unrelated update",
            "UPDATE local_tracks SET title = 'Renamed', title_folded = 'renamed' WHERE id = 'm1';",
        ),
        (
            "missing to indexed",
            "UPDATE local_tracks SET availability = 'indexed' WHERE id = 'm2';",
        ),
    ] {
        write_batch(&fixture, "transition", sql.to_owned()).await;
        check(&fixture, step).await;
    }
    // Extinct the ogg format: the breakdown drops it like GROUP BY.
    write_batch(
        &fixture,
        "delete tracks",
        "DELETE FROM local_track_artists WHERE local_track_id IN ('m1', 'm2', 't7'); \
         DELETE FROM local_tracks WHERE id IN ('m1', 'm2', 't7');"
            .to_owned(),
    )
    .await;
    check(&fixture, "delete with extinction").await;
    let stats = fixture.catalog.stats().await.expect("stats read");
    assert!(!stats.format_breakdown.contains_key("ogg"));
}

/// Migration 0005 lands its objects on fresh databases and backfills them
/// over catalogs migrated through 0004: the FTS oracle finds pre-existing
/// text, the format totals match live aggregation, and later writes stay
/// maintained through the triggers.
#[tokio::test]
async fn migration_0005_backfills_on_upgrade() {
    assert!(latest_version() >= 5);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("scratch pool opens");
    apply_migrations(&pool).await.expect("fresh migrates");
    for name in [
        "idx_local_albums_artist_id",
        "idx_local_tracks_availability_title",
        "idx_local_tracks_id_availability_album",
        "local_tracks_fts",
        "local_tracks_fts_data",
        "local_tracks_fts_idx",
        "local_tracks_fts_docsize",
        "local_tracks_fts_config",
        "library_track_format_stats",
        "trg_tracks_fts_insert",
        "trg_tracks_fts_delete",
        "trg_tracks_fts_update",
        "trg_track_stats_insert",
        "trg_track_stats_delete",
        "trg_track_stats_remove",
        "trg_track_stats_add",
    ] {
        let found: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE name = ?")
                .bind(name)
                .fetch_optional(&pool)
                .await
                .expect("master reads");
        assert_eq!(found.as_deref(), Some(name), "object {name} exists");
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("scratch pool opens");
    migrate_through(&pool, 4).await;
    for seed in [
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'Aurora', 'aurora', 'person', 1000, 1000)",
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, grouping_source, \
         created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'First Light', 'first light', 'Aurora', 'aurora', 'a1', 1994, \
         'automatic', 1000, 1000)",
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, disc_number, track_number, duration_seconds, \
         file_format, availability, ingest_source, imported_at, membership_source) VALUES \
         ('t1', 'al1', 'r1', '/m/t1', 't1', 'h1', 400, 1, 's1', 'Opener', 'opener', \
         'First Light', 'first light', 1, 1, 200.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic'), \
         ('t2', 'al1', 'r1', '/m/t2', 't2', 'h2', 300, 1, 's2', 'B-Side', 'b-side', \
         'First Light', 'first light', 1, 2, 180.0, 'mp3', 'indexed', 'scan', 1100.0, 'automatic'), \
         ('t3', 'al1', 'r1', '/m/t3', 't3', 'h3', 999, 1, 's3', 'Gone', 'gone', \
         'First Light', 'first light', 1, 3, 200.0, 'flac', 'missing', 'scan', 1150.0, 'automatic')",
    ] {
        sqlx::query(seed)
            .execute(&pool)
            .await
            .expect("v4 catalog seeds");
    }
    apply_migrations(&pool).await.expect("upgrade migrates");
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .expect("stamp reads");
    assert_eq!(version, latest_version());
    let fts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM local_tracks_fts WHERE local_tracks_fts MATCH ?")
            .bind("\"b-side\"")
            .fetch_one(&pool)
            .await
            .expect("oracle reads");
    assert_eq!(fts, 1);
    let miss: bool = sqlx::query_scalar(TRACK_MISS_ORACLE)
        .bind("\"zzz-no-such-thing\"")
        .fetch_one(&pool)
        .await
        .expect("oracle reads");
    assert!(!miss);
    let formats: Vec<(String, i64, i64)> =
        sqlx::query_as("SELECT file_format, indexed_tracks, indexed_bytes FROM library_track_format_stats ORDER BY 1")
            .fetch_all(&pool)
            .await
            .expect("stats read");
    assert_eq!(
        formats,
        vec![("flac".to_owned(), 1, 400), ("mp3".to_owned(), 1, 300),]
    );
    // Writes after the upgrade stay maintained.
    sqlx::query("UPDATE local_tracks SET availability = 'indexed' WHERE id = 't3'")
        .execute(&pool)
        .await
        .expect("transition writes");
    let flac: (i64, i64) = sqlx::query_as(
        "SELECT indexed_tracks, indexed_bytes FROM library_track_format_stats WHERE file_format = 'flac'",
    )
    .fetch_one(&pool)
    .await
    .expect("stats re-read");
    assert_eq!(flac, (2, 1399));
}

/// The hot shapes stay index-driven: the artist credit aggregation drives
/// from the credit index over covering probes, the title-order track page
/// walks its index with no sort, stats and the track miss oracle never
/// scan the track table, and the album id page never table-scans. Plan
/// shapes are scale-free, so the small fixture pins them.
#[tokio::test]
async fn hot_plans_avoid_scans_and_sorts() {
    let fixture = seed_small().await;
    // Without planner statistics (a fresh database). The 100k acceptance
    // test below re-checks the same plans with statistics: on this tiny
    // fixture ANALYZE rightly prefers scanning a few rows.
    assert_hot_plans(&fixture.pool).await;
}

async fn assert_hot_plans(pool: &SqlitePool) {
    let grouped = ARTIST_CREDIT_COUNTS.replace("{placeholders}", "?, ?");
    let plan = explain(
        pool,
        &grouped,
        &[ExplainBind::Text(Some("a1")), ExplainBind::Text(Some("a2"))],
    )
    .await;
    assert!(
        plan.iter()
            .any(|line| line
                .contains("SEARCH ata USING COVERING INDEX idx_local_track_artists_reverse")),
        "credits drive from the artist index: {plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line.contains("SCAN ")),
        "no scan in the credit aggregation: {plan:?}"
    );

    let order = track_order(TrackSort::Title, false);
    let list = format!(
        "SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE {TRACK_FILTER} ORDER BY {order} LIMIT ? OFFSET ?"
    );
    let nulls = [ExplainBind::Text(None); 11];
    let mut binds: Vec<ExplainBind> = nulls.into_iter().collect();
    binds.push(ExplainBind::Int(48));
    binds.push(ExplainBind::Int(0));
    let plan = explain(pool, &list, &binds).await;
    assert!(
        plan.iter()
            .any(|line| line.contains("idx_local_tracks_availability_title")),
        "title page walks its index: {plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|line| line.contains("TEMP B-TREE FOR ORDER BY")),
        "title page sorts nothing: {plan:?}"
    );

    for sql in [STATS_MAIN, STATS_FORMATS] {
        let plan = explain(pool, sql, &[]).await;
        assert!(
            !plan.iter().any(|line| line.contains("SCAN local_tracks")),
            "stats never scans tracks: {plan:?}"
        );
    }
    let plan = explain(
        pool,
        TRACK_MISS_ORACLE,
        &[ExplainBind::Text(Some("\"zzz-no-such-thing\""))],
    )
    .await;
    assert!(
        plan.iter()
            .any(|line| line.contains("SCAN local_tracks_fts VIRTUAL TABLE INDEX")),
        "oracle probes the FTS index: {plan:?}"
    );

    let order = album_order(AlbumSort::Name, false);
    let ids = format!(
        "SELECT a.id FROM local_albums a WHERE {ALBUM_FILTER} ORDER BY {order} LIMIT ? OFFSET ?"
    );
    let binds = [
        ExplainBind::Text(None),
        ExplainBind::Text(None),
        ExplainBind::Text(None),
        ExplainBind::Text(None),
        ExplainBind::Text(None),
        ExplainBind::Text(None),
        ExplainBind::Text(None),
        ExplainBind::Text(None),
        ExplainBind::Text(None),
        ExplainBind::Text(None),
        ExplainBind::Int(50),
        ExplainBind::Int(0),
    ];
    let plan = explain(pool, &ids, &binds).await;
    assert!(
        plan.iter()
            .all(|line| !line.contains("SCAN ") || line.contains("USING")),
        "album id page never table-scans: {plan:?}"
    );

    // The page aggregate probes each album's tracks for the modal format;
    // the probe must stay on the album index, never an availability scan.
    let agg = format!("SELECT {ALBUM_COLUMNS} {ALBUM_JOINS} WHERE a.id IN (?, ?) GROUP BY a.id");
    let plan = explain(
        pool,
        &agg,
        &[
            ExplainBind::Text(Some("al1")),
            ExplainBind::Text(Some("al2")),
        ],
    )
    .await;
    assert!(
        plan.iter()
            .any(|line| line.contains("SEARCH t2 USING INDEX idx_local_tracks_album_availability")),
        "format probe stays on the album index: {plan:?}"
    );

    let scope = artist_scope_predicate(ArtistScope::All);
    let order = artist_order(ArtistSort::Name, false);
    let page = format!(
        "SELECT r.id FROM local_artists r WHERE r.retired_into_artist_id IS NULL AND ({scope}) \
         AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\') ORDER BY {order} LIMIT ? OFFSET ?"
    );
    let plan = explain(
        pool,
        &page,
        &[
            ExplainBind::Text(None),
            ExplainBind::Text(None),
            ExplainBind::Int(50),
            ExplainBind::Int(0),
        ],
    )
    .await;
    assert!(
        !plan
            .iter()
            .any(|line| line.contains("SCAN local_albums") || line.contains("SCAN local_tracks")),
        "artist page never scans the catalog: {plan:?}"
    );
}

/// Seed 500 artists, 10,000 albums, and 100,000 tracks through the writer
/// lane, so the FTS oracle and format totals populate exactly like
/// production writes. Deterministic names; the miss query hits nothing.
async fn seed_100k(fixture: &Fixture) {
    let mut artists = String::from(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) VALUES ",
    );
    for artist in 0..500 {
        if artist > 0 {
            artists.push_str(", ");
        }
        artists.push_str(&format!(
            "('p-a{artist:04}', 'Perf Artist {artist:04}', 'perf artist {artist:04}', 'person', 1000, 1000)"
        ));
    }
    artists.push(';');
    write_batch(fixture, "perf artists", artists).await;
    for chunk in 0..10 {
        let mut albums = String::from(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, year, grouping_source, \
             created_at, updated_at) VALUES ",
        );
        for album in 0..1000 {
            let id = chunk * 1000 + album;
            if album > 0 {
                albums.push_str(", ");
            }
            albums.push_str(&format!(
                "('p-al{id:05}', 'r1', 'pg{id:05}', 'Perf Album {id:05}', 'perf album {id:05}', \
                 'Perf Artist {:04}', 'perf artist {:04}', 'p-a{:04}', 1994, 'automatic', 1000, 1000)",
                id / 20,
                id / 20,
                id / 20
            ));
        }
        albums.push(';');
        write_batch(fixture, "perf albums", albums).await;
        let mut credits = String::from(
            "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) VALUES ",
        );
        for album in 0..1000 {
            let id = chunk * 1000 + album;
            if album > 0 {
                credits.push_str(", ");
            }
            credits.push_str(&format!("('p-al{id:05}', 0, 'p-a{:04}', 'main')", id / 20));
        }
        credits.push(';');
        write_batch(fixture, "perf album credits", credits).await;
    }
    for chunk in 0..10 {
        let mut tracks = String::from(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             artist_name, artist_name_folded, album_title, album_title_folded, album_artist_name, \
             album_artist_name_folded, disc_number, track_number, year, duration_seconds, \
             file_format, availability, ingest_source, imported_at, membership_source) VALUES ",
        );
        let mut credits = String::from(
            "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) VALUES ",
        );
        for track in 0..10000 {
            let id = chunk * 10000 + track;
            if track > 0 {
                tracks.push_str(", ");
                credits.push_str(", ");
            }
            let format = ["flac", "mp3", "ogg", "opus", "m4a"][id % 5];
            tracks.push_str(&format!(
                "('p-t{id:06}', 'p-al{:05}', 'r1', '/m/p-t{id:06}', 'p-t{id:06}', 'ph{id:06}', \
                 4000, 1, 'ps{id:06}', 'Perf Track {id:06}', 'perf track {id:06}', \
                 'Perf Artist {:04}', 'perf artist {:04}', 'Perf Album {:05}', 'perf album {:05}', \
                 'Perf Artist {:04}', 'perf artist {:04}', 1, {}, 1994, 200.0, '{format}', 'indexed', \
                 'scan', 1000.0, 'automatic')",
                id / 10,
                (id / 10) / 20,
                (id / 10) / 20,
                id / 10,
                id / 10,
                (id / 10) / 20,
                (id / 10) / 20,
                id % 10 + 1,
            ));
            credits.push_str(&format!(
                "('p-t{id:06}', 0, 'p-a{:04}', 'main')",
                (id / 10) / 20
            ));
        }
        tracks.push(';');
        credits.push(';');
        write_batch(fixture, "perf tracks", tracks).await;
        write_batch(fixture, "perf track credits", credits).await;
    }
}

fn percentile(sorted: &mut [f64], pct: f64) -> f64 {
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let rank = (pct / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.max(1).min(sorted.len()) - 1]
}

/// Acceptance: every standard read holds the 10 ms p95 budget on a seeded
/// 100k catalog, 50 samples after 3 warmup per endpoint. Ignored by
/// default: timing is machine-dependent, so CI guards the mechanisms
/// (differential, totals, and plan tests above) while this runs explicitly
/// with --release on reference hardware, matching the HTTP bench
/// shape (checkpointed store, optimized sqlx). Debug runs only report
/// timings: unoptimized sqlx adds milliseconds per call there.
#[tokio::test]
#[ignore = "100k acceptance timing; run explicitly with --release on reference hardware"]
async fn perf_100k_endpoints_hold_p95() {
    let fixture = seed_small().await;
    seed_100k(&fixture).await;
    // Settle the store like production: the seed leaves megabytes of WAL
    // no steady-state server carries, and the bench seeds checkpoint too.
    let checkpoint = fixture.runtime.checkpoint().clone();
    for _ in 0..5 {
        let pass = tokio::task::spawn_blocking({
            let checkpoint = checkpoint.clone();
            move || checkpoint.run_once()
        })
        .await
        .expect("checkpoint runs");
        if pass.active_bytes <= 0 {
            break;
        }
    }
    let stats = fixture.catalog.stats().await.expect("stats read");
    assert_eq!(stats.total_tracks, 100007);
    assert_eq!(stats.total_albums, 10005);

    let miss_albums = AlbumFilter {
        q: Some("zzz-no-such-thing-zzz".to_owned()),
        ..Default::default()
    };
    let miss_tracks = TrackFilter {
        q: Some("zzz-no-such-thing-zzz".to_owned()),
        ..Default::default()
    };
    let mut report = Vec::new();
    // (name, samples)
    let mut endpoints: Vec<(&str, Vec<f64>)> = vec![
        ("library_albums", Vec::new()),
        ("library_artists", Vec::new()),
        ("library_tracks", Vec::new()),
        ("library_stats", Vec::new()),
        ("local_albums", Vec::new()),
        ("local_search_miss", Vec::new()),
    ];
    for _ in 0..3 {
        fixture
            .catalog
            .list_albums(&AlbumFilter::default(), AlbumSort::Name, false, 50, 0)
            .await
            .expect("warmup");
        fixture
            .catalog
            .list_artists(ArtistScope::All, None, ArtistSort::Name, false, 50, 0)
            .await
            .expect("warmup");
        fixture
            .catalog
            .list_tracks(&TrackFilter::default(), TrackSort::Title, false, 48, 0)
            .await
            .expect("warmup");
        fixture.catalog.stats().await.expect("warmup");
        fixture
            .catalog
            .list_albums(&miss_albums, AlbumSort::Name, false, 20, 0)
            .await
            .expect("warmup");
        fixture
            .catalog
            .list_tracks(&miss_tracks, TrackSort::Title, false, 20, 0)
            .await
            .expect("warmup");
    }
    for _ in 0..50 {
        let tick = std::time::Instant::now();
        fixture
            .catalog
            .list_albums(&AlbumFilter::default(), AlbumSort::Name, false, 50, 0)
            .await
            .expect("lists");
        endpoints[0].1.push(tick.elapsed().as_secs_f64() * 1000.0);
        let tick = std::time::Instant::now();
        fixture
            .catalog
            .list_artists(ArtistScope::All, None, ArtistSort::Name, false, 50, 0)
            .await
            .expect("lists");
        endpoints[1].1.push(tick.elapsed().as_secs_f64() * 1000.0);
        let tick = std::time::Instant::now();
        fixture
            .catalog
            .list_tracks(&TrackFilter::default(), TrackSort::Title, false, 48, 0)
            .await
            .expect("lists");
        endpoints[2].1.push(tick.elapsed().as_secs_f64() * 1000.0);
        let tick = std::time::Instant::now();
        fixture.catalog.stats().await.expect("stats read");
        endpoints[3].1.push(tick.elapsed().as_secs_f64() * 1000.0);
        let tick = std::time::Instant::now();
        fixture
            .catalog
            .list_albums(&AlbumFilter::default(), AlbumSort::Name, false, 50, 0)
            .await
            .expect("lists");
        endpoints[4].1.push(tick.elapsed().as_secs_f64() * 1000.0);
        let tick = std::time::Instant::now();
        fixture
            .catalog
            .list_albums(&miss_albums, AlbumSort::Name, false, 20, 0)
            .await
            .expect("lists");
        fixture
            .catalog
            .list_tracks(&miss_tracks, TrackSort::Title, false, 20, 0)
            .await
            .expect("lists");
        endpoints[5].1.push(tick.elapsed().as_secs_f64() * 1000.0);
    }
    for (name, samples) in &mut endpoints {
        let mut sorted = samples.clone();
        let p95 = percentile(&mut sorted, 95.0);
        let p50 = percentile(&mut sorted, 50.0);
        report.push(format!("{name}: p50 {p50:.2} ms, p95 {p95:.2} ms"));
        // The budget binds release runs; debug runs report only, since
        // unoptimized sqlx adds milliseconds per call there.
        if !cfg!(debug_assertions) {
            assert!(
                p95 < 10.0,
                "{name} p95 {p95:.2} ms exceeds the 10 ms budget:\n{}",
                report.join("\n")
            );
        }
    }
    eprintln!("100k catalog p95:\n{}", report.join("\n"));
}

/// Acceptance: the player pages the Jellyfin and Subsonic browse routes
/// read (default track and album pages, a title-search miss, the newest
/// albums) hold the same 10 ms p95 budget on the seeded 100k catalog.
#[tokio::test]
#[ignore = "100k acceptance timing; run explicitly with --release on reference hardware"]
async fn perf_100k_player_pages_hold_p95() {
    use library::player::{
        AlbumOrder, AlbumQuery, OrderKey, PlayerCatalog, SqlitePlayerCatalog, TrackOrder,
        TrackQuery,
    };

    let fixture = seed_small().await;
    seed_100k(&fixture).await;
    // Production statistics: the maintenance ANALYZE, then the hot plans
    // must still walk their indexes.
    crate::db::AnalyzeService::new(fixture.runtime.lane().clone(), fixture.pool.clone())
        .run()
        .await
        .expect("analyze runs");
    assert_hot_plans(&fixture.pool).await;
    let player = SqlitePlayerCatalog::new(&LibraryDb::new(&fixture.pool));
    let miss = TrackQuery {
        q: Some("zzz-no-such-thing-zzz".to_owned()),
        ..TrackQuery::default()
    };
    let hit = TrackQuery {
        q: Some("track 0123".to_owned()),
        ..TrackQuery::default()
    };
    let common = TrackQuery {
        q: Some("perf artist 01".to_owned()),
        ..TrackQuery::default()
    };
    let mut report = Vec::new();
    let mut over = Vec::new();
    for (name, read) in [
        ("player_tracks_title", 0),
        ("player_tracks_search_miss", 1),
        ("player_albums_title", 2),
        ("player_albums_newest", 3),
        ("search3_sync_page_offset_99500", 4),
        ("search3_songs_hit_page", 5),
        ("search3_songs_common_page", 6),
        ("player_tracks_random_50", 7),
        ("player_tracks_shuffle_page", 8),
        ("player_albums_shuffle_page_9000", 9),
        ("player_tracks_year_page", 10),
    ] {
        let mut samples = Vec::new();
        for round in 0..53 {
            let tick = std::time::Instant::now();
            let rows = match read {
                0 => player
                    .tracks(&TrackQuery::default(), TrackOrder::Title, 100, 0)
                    .await
                    .map(|page| page.0.len()),
                1 => player
                    .tracks(&miss, TrackOrder::Title, 100, 0)
                    .await
                    .map(|page| page.0.len()),
                2 => player
                    .albums(&AlbumQuery::default(), AlbumOrder::Title, 100, 0)
                    .await
                    .map(|page| page.0.len()),
                3 => player
                    .albums(
                        &AlbumQuery::default(),
                        AlbumOrder::By(OrderKey::Added, true),
                        10,
                        0,
                    )
                    .await
                    .map(|page| page.0.len()),
                4 => player
                    .track_page(&TrackQuery::default(), TrackOrder::Natural, 500, 99_500)
                    .await
                    .map(|page| page.len()),
                5 => player
                    .track_page(&hit, TrackOrder::Album, 20, 0)
                    .await
                    .map(|page| page.len()),
                6 => player
                    .track_page(&common, TrackOrder::Album, 20, 0)
                    .await
                    .map(|page| page.len()),
                7 => player
                    .track_page(&TrackQuery::default(), TrackOrder::Random, 50, 0)
                    .await
                    .map(|page| page.len()),
                8 => player
                    .tracks(&TrackQuery::default(), TrackOrder::Shuffle(7), 100, 0)
                    .await
                    .map(|page| page.0.len()),
                9 => player
                    .album_page(&AlbumQuery::default(), AlbumOrder::Shuffle(7), 50, 9_000)
                    .await
                    .map(|page| page.len()),
                _ => player
                    .tracks(
                        &TrackQuery::default(),
                        TrackOrder::By(OrderKey::Year, false),
                        100,
                        0,
                    )
                    .await
                    .map(|page| page.0.len()),
            };
            rows.expect("page reads");
            // The first rounds warm the page cache.
            if round >= 3 {
                samples.push(tick.elapsed().as_secs_f64() * 1000.0);
            }
        }
        let p95 = percentile(&mut samples, 95.0);
        let p50 = percentile(&mut samples, 50.0);
        report.push(format!("{name}: p50 {p50:.2} ms, p95 {p95:.2} ms"));
        if p95 >= 10.0 {
            over.push(name);
        }
    }
    eprintln!("100k player pages:\n{}", report.join("\n"));
    // The budget binds release runs; debug runs report only.
    if !cfg!(debug_assertions) {
        assert!(over.is_empty(), "over budget:\n{}", report.join("\n"));
    }
}
