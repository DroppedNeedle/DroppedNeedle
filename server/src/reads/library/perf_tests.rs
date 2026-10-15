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

use droppedneedle_kernel::db::{DbConfig, DbRuntime, Lane, OpError, fold_text, open_runtime};
use droppedneedle_kernel::schema::{MIGRATOR, apply_migrations, latest_version};
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
    _dir: droppedneedle_kernel::db::scratch::ScratchDir,
}

async fn seed_small() -> Fixture {
    let seq = PERF_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = droppedneedle_kernel::db::scratch::ScratchDir::new(&format!("reads-perf-{seq}"))
        .expect("scratch dir");
    let path = dir.join("app.db");
    droppedneedle_kernel::schema::template::copy_to(&path).expect("migrated database copies");
    let runtime = open_runtime(&DbConfig::new(&path))
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
    tal.album_artist_id AS album_artist_id, \
    tri.recording_mbid AS recording_mbid, tgi.release_group_mbid AS release_group_mbid, \
    tai.provider_artist_id AS artist_mbid, tbi.provider_artist_id AS album_artist_mbid, \
    t.disc_number AS disc_number, t.track_number AS track_number, t.year AS year, \
    COALESCE((SELECT g.name FROM local_track_genres g \
     WHERE g.local_track_id = t.id ORDER BY g.position LIMIT 1), t.genre) AS genre, \
    t.duration_seconds AS duration_seconds, t.file_format AS format, \
    t.bit_rate AS bit_rate, t.sample_rate AS sample_rate, \
    t.bit_depth AS bit_depth, t.channels AS channels, \
    t.file_size_bytes AS file_size_bytes, t.imported_at AS date_added, \
    (w.local_album_id IS NOT NULL) AS cover_available";

const LEGACY_TRACK_JOINS: &str = "FROM local_tracks t \
    LEFT JOIN local_track_artists ta \
        ON ta.local_track_id = t.id AND ta.position = 0 \
    LEFT JOIN local_album_artwork w ON w.local_album_id = t.local_album_id \
    LEFT JOIN local_albums tal ON tal.id = t.local_album_id \
    LEFT JOIN local_track_external_identities tri \
        ON tri.local_track_id = t.id AND tri.provider = 'musicbrainz' \
    LEFT JOIN local_album_external_identities tgi \
        ON tgi.local_album_id = t.local_album_id AND tgi.provider = 'musicbrainz' \
    LEFT JOIN local_artist_external_identities tai \
        ON tai.local_artist_id = COALESCE(ta.local_artist_id, tal.album_artist_id) \
        AND tai.provider = 'musicbrainz' \
    LEFT JOIN local_artist_external_identities tbi \
        ON tbi.local_artist_id = tal.album_artist_id AND tbi.provider = 'musicbrainz'";

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
        (ArtistSort::AppearanceCount, false) => {
            "appearance_album_count ASC, r.folded_name ASC, r.id ASC"
        }
        (ArtistSort::AppearanceCount, true) => {
            "appearance_album_count DESC, r.folded_name ASC, r.id ASC"
        }
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
        contribution_id: None,
        contribution_state: None,
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
        album_artist_id: row.get("album_artist_id"),
        recording_mbid: row.get("recording_mbid"),
        release_group_mbid: row.get("release_group_mbid"),
        artist_mbid: row.get("artist_mbid"),
        album_artist_mbid: row.get("album_artist_mbid"),
        disc_number: row.get("disc_number"),
        track_number: row.get("track_number"),
        year: row.get("year"),
        genre: row.get("genre"),
        duration_seconds: row.get("duration_seconds"),
        format: row.get("format"),
        bit_rate: row.get("bit_rate"),
        sample_rate: row.get("sample_rate"),
        bit_depth: row.get("bit_depth"),
        channels: row.get("channels"),
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

/// Plays on the small fixture: `u1` has ties in both the last play (t2 and
/// t4) and the play count (t1 and t6), a play of a track that is gone
/// (t3), a play of t4 from when it was on another album (al1), and a play
/// filed under an album (al5) with no track. `u2` has a play of its own.
async fn seed_small_plays(fixture: &Fixture) {
    let plays = [
        ("u1", "'t1'", "al1", "2026-01-01T00:00:00+00:00"),
        ("u1", "'t1'", "al1", "2026-01-02T00:00:00+00:00"),
        ("u1", "'t1'", "al1", "2026-01-03T00:00:00+00:00"),
        ("u1", "'t2'", "al1", "2026-01-05T00:00:00+00:00"),
        ("u1", "'t3'", "al1", "2026-01-09T00:00:00+00:00"),
        ("u1", "'t4'", "al2", "2026-01-05T00:00:00+00:00"),
        ("u1", "'t4'", "al1", "2025-12-31T00:00:00+00:00"),
        ("u1", "'t6'", "al3", "2026-01-04T00:00:00+00:00"),
        ("u1", "'t6'", "al3", "2026-01-04T01:00:00+00:00"),
        ("u1", "'t6'", "al3", "2026-01-04T02:00:00+00:00"),
        ("u1", "NULL", "al5", "2026-01-06T00:00:00+00:00"),
        ("u2", "'t8'", "al5", "2026-01-08T00:00:00+00:00"),
    ];
    let mut sql = String::from(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES ",
    );
    for (index, (user, track, album, at)) in plays.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!(
            "('play-{index}', '{user}', {track}, '{album}', 'x', 'x', '{at}')"
        ));
    }
    write_batch(fixture, "small plays", sql).await;
}

/// A play-history order pages exactly as one sort of the whole catalog
/// would, with the never played at zero: no row repeated or skipped
/// across pages, ties in a fixed order, in both directions, for tracks
/// and albums, for a user with plays and a user with none. An album's
/// plays are those of the tracks on it now, and those filed under it with
/// no track.
#[tokio::test]
async fn history_orders_page_like_one_sort() {
    use library::player::{
        AlbumOrder, AlbumQuery, OrderKey, PlayerCatalog, SqlitePlayerCatalog, TrackOrder,
        TrackQuery,
    };

    let fixture = seed_small().await;
    seed_small_plays(&fixture).await;
    let player = SqlitePlayerCatalog::new(&LibraryDb::new(&fixture.pool));
    for user in ["u1", "nobody"] {
        for (key, aggregate) in [
            (OrderKey::LastPlayed, "MAX(h.played_at)"),
            (OrderKey::PlayCount, "COUNT(*)"),
        ] {
            for descending in [true, false] {
                let direction = if descending { "DESC" } else { "ASC" };
                let what = format!("{user} {key:?} {direction}");
                let one_sort = |from: &str, counts_for: &str| {
                    format!(
                        "SELECT r.id FROM {from} ORDER BY (SELECT {aggregate} \
                         FROM library_play_history h WHERE h.user_id = '{user}' \
                         AND {counts_for} = r.id) {direction}, r.id {direction}"
                    )
                };
                let expected: Vec<String> = sqlx::query_scalar(&one_sort(
                    "local_tracks r WHERE r.availability = 'indexed'",
                    "h.local_track_id",
                ))
                .fetch_all(&fixture.pool)
                .await
                .expect("tracks in one sort");
                let query = TrackQuery {
                    listener: Some(user.to_owned()),
                    ..TrackQuery::default()
                };
                let mut paged = Vec::new();
                for page in 0..4 {
                    let (tracks, total) = player
                        .tracks(&query, TrackOrder::By(key, descending), 2, page * 2)
                        .await
                        .expect("track page");
                    assert_eq!(total, expected.len() as u64, "{what}");
                    paged.extend(tracks.into_iter().map(|track| track.id));
                }
                assert_eq!(paged, expected, "tracks, {what}");

                let expected: Vec<String> = sqlx::query_scalar(&one_sort(
                    "local_albums r WHERE r.retired_into_album_id IS NULL AND EXISTS \
                     (SELECT 1 FROM local_tracks t WHERE t.local_album_id = r.id \
                      AND t.availability = 'indexed')",
                    "COALESCE((SELECT pt.local_album_id FROM local_tracks pt \
                      WHERE pt.id = h.local_track_id), h.local_album_id)",
                ))
                .fetch_all(&fixture.pool)
                .await
                .expect("albums in one sort");
                let query = AlbumQuery {
                    listener: Some(user.to_owned()),
                    ..AlbumQuery::default()
                };
                let mut paged = Vec::new();
                for page in 0..3 {
                    let (albums, total) = player
                        .albums(&query, AlbumOrder::By(key, descending), 2, page * 2)
                        .await
                        .expect("album page");
                    assert_eq!(total, expected.len() as u64, "{what}");
                    paged.extend(albums.into_iter().map(|album| album.record.id));
                }
                assert_eq!(paged, expected, "albums, {what}");
            }
        }
    }

    // The played filter splits the catalog in two, and its totals agree.
    let filtered = |played: bool| TrackQuery {
        listener: Some("u1".to_owned()),
        played: Some(played),
        ..TrackQuery::default()
    };
    let (played, played_total) = player
        .tracks(&filtered(true), TrackOrder::Title, 100, 0)
        .await
        .expect("played tracks");
    let mut played: Vec<String> = played.into_iter().map(|track| track.id).collect();
    played.sort();
    assert_eq!(played, ["t1", "t2", "t4", "t6"]);
    assert_eq!(played_total, 4);
    let (unplayed, unplayed_total) = player
        .tracks(&filtered(false), TrackOrder::Title, 100, 0)
        .await
        .expect("unplayed tracks");
    let mut unplayed: Vec<String> = unplayed.into_iter().map(|track| track.id).collect();
    unplayed.sort();
    assert_eq!(unplayed, ["t7", "t8", "t9"]);
    assert_eq!(unplayed_total, 3);

    // A track that moved to another album took its plays along, and a
    // play with no track still counts for its album.
    let albums = ["al1", "al2", "al5"].map(str::to_owned);
    let stats = player
        .play_stats("u1", "album", &albums)
        .await
        .expect("album plays");
    let plays = |album: &str| stats.get(album).map(|stat| stat.0);
    assert_eq!(
        (plays("al1"), plays("al2"), plays("al5")),
        (Some(5), Some(2), Some(1))
    );
    let frequent = player
        .history_albums("u1", true, 10, 0)
        .await
        .expect("most played albums");
    assert_eq!(frequent, ["al1", "al3", "al2", "al5"]);
    let recent = player
        .history_albums("u1", false, 10, 0)
        .await
        .expect("recently played albums");
    assert_eq!(recent, ["al1", "al5", "al2", "al3"]);
}

/// The played half of a play-history order reads the listener's plays
/// through a per-user index and looks each row up by id: for tracks from
/// the index over listener, track and time alone, for albums through each
/// play's track. It never walks the catalog, whether the planner has
/// statistics on the history table or not (a fresh install has none until
/// the daily ANALYZE after the first plays). The play counts of a page of
/// tracks or albums probe that index once per track.
async fn assert_history_plans(pool: &SqlitePool, state: &str) {
    use library::player::{
        HISTORY_ALBUMS, HISTORY_TRACKS, history_played_sql, history_unplayed_sql, play_stats_sql,
    };
    const BY_TRACK: &str = "SEARCH h USING COVERING INDEX idx_play_history_user_track";

    for (rows, filters, catalog, plays) in [
        (
            &HISTORY_TRACKS,
            "+t.availability = 'indexed'",
            "t",
            BY_TRACK,
        ),
        (
            &HISTORY_ALBUMS,
            "+a.retired_into_album_id IS NULL",
            "a",
            "SEARCH h USING INDEX",
        ),
    ] {
        for aggregate in ["MAX(h.played_at)", "COUNT(*)"] {
            let sql = history_played_sql(rows, filters, aggregate, true);
            let plan = explain(
                pool,
                &sql,
                &[
                    ExplainBind::Text(Some("u1")),
                    ExplainBind::Int(100),
                    ExplainBind::Int(0),
                ],
            )
            .await;
            let what = format!("{catalog} by {aggregate}, {state}: {plan:?}");
            assert!(
                plan.iter()
                    .any(|line| line.starts_with(plays) && line.contains("(user_id=?)")),
                "the played half reads the listener's plays by index: {what}"
            );
            assert!(
                plan.iter()
                    .any(|line| line.starts_with(&format!("SEARCH {catalog} USING"))),
                "each played row is looked up by id: {what}"
            );
            assert!(
                !plan.iter().any(|line| line.starts_with("SCAN ")),
                "nothing is scanned: {what}"
            );
        }
        // The unplayed half walks the ids and stops at the page: the
        // listener's plays are one list built once, and nothing is sorted.
        let sql = history_unplayed_sql(rows, filters, true);
        let plan = explain(
            pool,
            &sql,
            &[
                ExplainBind::Text(Some("u1")),
                ExplainBind::Int(100),
                ExplainBind::Int(0),
            ],
        )
        .await;
        assert!(
            !plan.iter().any(
                |line| line.contains("TEMP B-TREE FOR ORDER BY") || line.contains("CORRELATED")
            ),
            "the unplayed half of {catalog} is neither sorted nor probed per row, \
             {state}: {plan:?}"
        );
    }

    let probe = "(user_id=? AND local_track_id=?)";
    let sql = play_stats_sql("track", 1).expect("track counts");
    let binds = [ExplainBind::Text(Some("u1")), ExplainBind::Text(Some("t1"))];
    let plan = explain(pool, &sql, &binds).await;
    assert!(
        plan.len() == 1 && plan[0].contains("COVERING INDEX idx_play_history_user_track"),
        "track counts come from the index alone, {state}: {plan:?}"
    );
    // A page of albums, as a list asks for them: the planner weighs the
    // indexes differently for one id and for many.
    let page = 50;
    let sql = play_stats_sql("album", page).expect("album counts");
    let mut half = vec![ExplainBind::Text(Some("u1"))];
    half.resize(page + 1, ExplainBind::Text(Some("al1")));
    let binds = half.repeat(2);
    let plan = explain(pool, &sql, &binds).await;
    assert!(
        plan.iter()
            .any(|line| line.starts_with(BY_TRACK) && line.contains(probe))
            && plan.iter().any(|line| line.starts_with("SEARCH pt USING"))
            && !plan
                .iter()
                .any(|line| line.starts_with("SCAN ") && !line.contains("subquery")),
        "album counts probe the index once per track of the album, {state}: {plan:?}"
    );
    // The plays with no track come from the small index that holds only
    // those filed under an album, never from a walk of every play the
    // listener has that names no track.
    let trackless: Vec<&String> = plan
        .iter()
        .skip_while(|line| !line.starts_with("UNION ALL"))
        .skip(1)
        .take_while(|line| line.starts_with("SEARCH "))
        .collect();
    assert!(
        matches!(trackless.as_slice(), [only]
        if only.starts_with(
            "SEARCH h USING INDEX idx_play_history_user_album_no_track \
             (user_id=? AND local_album_id=?)"
        )),
        "album counts read the plays with no track from their own index, {state}: {plan:?}"
    );
}

#[tokio::test]
async fn history_plans_read_the_listeners_plays_first() {
    let fixture = seed_small().await;
    assert_history_plans(&fixture.pool, "no statistics").await;

    // Statistics over a history shared by many listeners. A connection
    // loads them when it opens, so the plans are read on a new one.
    let mut plays = String::from(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES ",
    );
    for user in 0..200 {
        for (index, (track, album)) in [("t1", "al1"), ("t4", "al2"), ("t6", "al3"), ("t8", "al5")]
            .iter()
            .enumerate()
        {
            if user + index > 0 {
                plays.push_str(", ");
            }
            plays.push_str(&format!(
                "('plan-{user}-{index}', 'u{user}', '{track}', '{album}', 'x', 'x', \
                 '2026-01-01T00:00:{index:02}+00:00')"
            ));
        }
    }
    write_batch(&fixture, "plan plays", plays).await;
    // What an upgrade leaves: for a quarter of the listeners, hundreds of
    // plays that name no track, some of them filed under one album, so
    // the statistics cover both indexes the track-less half could use.
    let mut trackless = String::from(
        "INSERT INTO library_play_history (id, user_id, local_album_id, track_name, \
         artist_name, played_at) VALUES ",
    );
    for user in 0..50 {
        for index in 0..300 {
            if user + index > 0 {
                trackless.push_str(", ");
            }
            let album = if index < 40 { "'al1'" } else { "NULL" };
            trackless.push_str(&format!(
                "('plan-none-{user}-{index}', 'u{user}', {album}, 'x', 'x', \
                 '2025-12-01T00:00:00+00:00')"
            ));
        }
    }
    write_batch(&fixture, "plan plays with no track", trackless).await;
    write_batch(&fixture, "plan statistics", "ANALYZE".to_owned()).await;
    let path = fixture._dir.join("app.db");
    let analyzed = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("a new connection opens");
    let statistics: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'library_play_history'")
            .fetch_one(&analyzed)
            .await
            .expect("statistics read");
    assert!(statistics > 0, "the history table has statistics");
    assert_history_plans(&analyzed, "with statistics").await;
    analyzed.close().await;
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

/// Plays over the 100k catalog: `p-u1` has played every 50th track one to
/// three times (2,000 tracks), and twenty other listeners have a thousand
/// plays each, so the history table is shared as a real one is. `p-u1`
/// also has 40,000 plays that name no track, one in eight of them filed
/// under an album: what an upgrade leaves when plays fit no single song.
async fn seed_100k_plays(fixture: &Fixture) {
    for chunk in 0..8 {
        let mut sql = String::from(
            "INSERT INTO library_play_history (id, user_id, local_album_id, track_name, \
             artist_name, played_at) VALUES ",
        );
        for index in 0..5000 {
            if index > 0 {
                sql.push_str(", ");
            }
            let album = if index % 8 == 0 {
                // An album the listener has played a track of, so the
                // number of played albums stays what the pages expect.
                format!("'p-al{:05}'", (index % 2000) * 5)
            } else {
                "NULL".to_owned()
            };
            sql.push_str(&format!(
                "('p-n{chunk}-{index}', 'p-u1', {album}, 'x', 'x', \
                 '2025-12-{:02}T{:02}:{:02}:00+00:00')",
                chunk + 1,
                index / 60 % 24,
                index % 60,
            ));
        }
        write_batch(fixture, "perf plays with no track", sql).await;
    }
    let mut rows = Vec::new();
    for step in 0..2000 {
        let track = step * 50;
        for play in 0..=(step % 3) {
            rows.push(("p-u1".to_owned(), track, step * 3 + play));
        }
    }
    for user in 0..20 {
        for step in 0..1000 {
            rows.push((
                format!("p-o{user:02}"),
                (step * 97 + user * 13) % 100_000,
                step,
            ));
        }
    }
    for (chunk_index, chunk) in rows.chunks(5000).enumerate() {
        let mut sql = String::from(
            "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
             track_name, artist_name, played_at) VALUES ",
        );
        for (index, (user, track, minute)) in chunk.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!(
                "('p-h{chunk_index}-{index}', '{user}', 'p-t{track:06}', 'p-al{:05}', 'x', 'x', \
                 '2026-01-{:02}T{:02}:{:02}:00+00:00')",
                track / 10,
                minute / 1440 + 1,
                (minute / 60) % 24,
                minute % 60,
            ));
        }
        write_batch(fixture, "perf plays", sql).await;
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
    seed_100k_plays(&fixture).await;
    // Production statistics: the maintenance ANALYZE, then the hot plans
    // must still walk their indexes.
    droppedneedle_kernel::db::AnalyzeService::new(
        fixture.runtime.lane().clone(),
        fixture.pool.clone(),
    )
    .run()
    .await
    .expect("analyze runs");
    assert_hot_plans(&fixture.pool).await;
    assert_history_plans(&fixture.pool, "100k with statistics").await;
    let player = SqlitePlayerCatalog::new(&LibraryDb::new(&fixture.pool));
    let listener = TrackQuery {
        listener: Some("p-u1".to_owned()),
        ..TrackQuery::default()
    };
    let played = |played: bool| TrackQuery {
        played: Some(played),
        ..listener.clone()
    };
    let album_listener = AlbumQuery {
        listener: Some("p-u1".to_owned()),
        ..AlbumQuery::default()
    };
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
    // A page of 100 tracks, half of them played, and a page of 100 albums
    // that each hold a played track.
    let stat_tracks: Vec<String> = (0..100)
        .map(|step| format!("p-t{:06}", step * 25))
        .collect();
    let stat_albums: Vec<String> = (0..100)
        .map(|step| format!("p-al{:05}", step * 5))
        .collect();
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
        ("player_tracks_last_played", 11),
        ("player_tracks_play_count", 12),
        ("player_tracks_last_played_ascending", 13),
        ("player_tracks_last_played_across_halves_1950", 14),
        ("player_tracks_played_only", 15),
        ("player_tracks_unplayed_only", 16),
        ("player_albums_last_played", 17),
        ("player_albums_play_count_across_halves_1950", 18),
        ("player_albums_recently_played", 19),
        ("player_track_play_counts_page", 20),
        ("player_album_play_counts_page", 21),
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
                10 => player
                    .tracks(
                        &TrackQuery::default(),
                        TrackOrder::By(OrderKey::Year, false),
                        100,
                        0,
                    )
                    .await
                    .map(|page| page.0.len()),
                11 => player
                    .tracks(
                        &listener,
                        TrackOrder::By(OrderKey::LastPlayed, true),
                        100,
                        0,
                    )
                    .await
                    .map(|page| page.0.len()),
                12 => player
                    .tracks(&listener, TrackOrder::By(OrderKey::PlayCount, true), 100, 0)
                    .await
                    .map(|page| page.0.len()),
                13 => player
                    .tracks(
                        &listener,
                        TrackOrder::By(OrderKey::LastPlayed, false),
                        100,
                        0,
                    )
                    .await
                    .map(|page| page.0.len()),
                14 => player
                    .tracks(
                        &listener,
                        TrackOrder::By(OrderKey::LastPlayed, true),
                        100,
                        1_950,
                    )
                    .await
                    .map(|page| page.0.len()),
                15 => player
                    .tracks(&played(true), TrackOrder::Title, 100, 0)
                    .await
                    .map(|page| page.0.len()),
                16 => player
                    .tracks(&played(false), TrackOrder::Title, 100, 0)
                    .await
                    .map(|page| page.0.len()),
                17 => player
                    .albums(
                        &album_listener,
                        AlbumOrder::By(OrderKey::LastPlayed, true),
                        100,
                        0,
                    )
                    .await
                    .map(|page| page.0.len()),
                18 => player
                    .albums(
                        &album_listener,
                        AlbumOrder::By(OrderKey::PlayCount, true),
                        100,
                        1_950,
                    )
                    .await
                    .map(|page| page.0.len()),
                19 => player
                    .history_albums("p-u1", false, 100, 0)
                    .await
                    .map(|page| page.len()),
                20 => player
                    .play_stats("p-u1", "track", &stat_tracks)
                    .await
                    .map(|stats| stats.len()),
                _ => player
                    .play_stats("p-u1", "album", &stat_albums)
                    .await
                    .map(|stats| stats.len()),
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

// Read pool replay: the 17 budgeted reads through the catalog on sqlx, and
// the same statements replayed through `Db::read` on the runtime's own
// request pool, one closure and so one checkout per statement.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use droppedneedle_kernel::db::{Db, DbError, WriteLane};
use library::player::{
    AlbumOrder, AlbumQuery, OrderKey, PlayerAlbum, PlayerCatalog, PlayerTrack, SqlitePlayerCatalog,
    TrackOrder, TrackQuery,
};
use rusqlite::types::Value;

/// The budgeted reads, in the order the two acceptance tests time them.
const READS: [&str; 17] = [
    "library_albums",
    "library_artists",
    "library_tracks",
    "library_stats",
    "local_albums",
    "local_search_miss",
    "player_tracks_title",
    "player_tracks_search_miss",
    "player_albums_title",
    "player_albums_newest",
    "search3_sync_page_offset_99500",
    "search3_songs_hit_page",
    "search3_songs_common_page",
    "player_tracks_random_50",
    "player_tracks_shuffle_page",
    "player_albums_shuffle_page_9000",
    "player_tracks_year_page",
];

/// The one read whose rows differ call to call.
const RANDOM_READ: usize = 13;

/// What one read hands back.
#[derive(Debug, PartialEq)]
enum Output {
    Nothing,
    Albums(Vec<AlbumRecord>),
    Artists(Vec<ArtistRecord>),
    Tracks(Vec<TrackRecord>),
    Stats(StatsRecord),
    PlayerTracks(Vec<PlayerTrack>),
    PlayerAlbums(Vec<PlayerAlbum>),
}

impl Output {
    fn len(&self) -> usize {
        match self {
            Output::Nothing => 0,
            Output::Stats(_) => 1,
            Output::Albums(rows) => rows.len(),
            Output::Artists(rows) => rows.len(),
            Output::Tracks(rows) => rows.len(),
            Output::PlayerTracks(rows) => rows.len(),
            Output::PlayerAlbums(rows) => rows.len(),
        }
    }
}

/// The 17 reads as the acceptance tests call them, over one sqlx pool.
struct Reads {
    catalog: SqliteCatalog,
    player: SqlitePlayerCatalog,
    miss_albums: AlbumFilter,
    miss_tracks: TrackFilter,
    miss: TrackQuery,
    hit: TrackQuery,
    common: TrackQuery,
}

impl Reads {
    fn over(pool: &SqlitePool) -> Self {
        let db = LibraryDb::new(pool);
        let search = |q: &str| TrackQuery {
            q: Some(q.to_owned()),
            ..TrackQuery::default()
        };
        Self {
            catalog: SqliteCatalog::new(&db),
            player: SqlitePlayerCatalog::new(&db),
            miss_albums: AlbumFilter {
                q: Some("zzz-no-such-thing-zzz".to_owned()),
                ..Default::default()
            },
            miss_tracks: TrackFilter {
                q: Some("zzz-no-such-thing-zzz".to_owned()),
                ..Default::default()
            },
            miss: search("zzz-no-such-thing-zzz"),
            hit: search("track 0123"),
            common: search("perf artist 01"),
        }
    }

    async fn run(&self, read: usize) -> Output {
        let (catalog, player) = (&self.catalog, &self.player);
        let open = TrackQuery::default();
        let albums = AlbumQuery::default();
        match read {
            0 | 4 => Output::Albums(
                catalog
                    .list_albums(&AlbumFilter::default(), AlbumSort::Name, false, 50, 0)
                    .await
                    .expect("lists")
                    .0,
            ),
            1 => Output::Artists(
                catalog
                    .list_artists(ArtistScope::All, None, ArtistSort::Name, false, 50, 0)
                    .await
                    .expect("lists")
                    .0,
            ),
            2 => Output::Tracks(
                catalog
                    .list_tracks(&TrackFilter::default(), TrackSort::Title, false, 48, 0)
                    .await
                    .expect("lists")
                    .0,
            ),
            3 => Output::Stats(catalog.stats().await.expect("stats read")),
            5 => {
                let albums = catalog
                    .list_albums(&self.miss_albums, AlbumSort::Name, false, 20, 0)
                    .await
                    .expect("lists");
                let tracks = catalog
                    .list_tracks(&self.miss_tracks, TrackSort::Title, false, 20, 0)
                    .await
                    .expect("lists");
                assert!(albums.0.is_empty() && tracks.0.is_empty());
                Output::Nothing
            }
            6 => Output::PlayerTracks(
                player
                    .tracks(&open, TrackOrder::Title, 100, 0)
                    .await
                    .expect("page reads")
                    .0,
            ),
            7 => Output::PlayerTracks(
                player
                    .tracks(&self.miss, TrackOrder::Title, 100, 0)
                    .await
                    .expect("page reads")
                    .0,
            ),
            8 => Output::PlayerAlbums(
                player
                    .albums(&albums, AlbumOrder::Title, 100, 0)
                    .await
                    .expect("page reads")
                    .0,
            ),
            9 => Output::PlayerAlbums(
                player
                    .albums(&albums, AlbumOrder::By(OrderKey::Added, true), 10, 0)
                    .await
                    .expect("page reads")
                    .0,
            ),
            10 => Output::PlayerTracks(
                player
                    .track_page(&open, TrackOrder::Natural, 500, 99_500)
                    .await
                    .expect("page reads"),
            ),
            11 => Output::PlayerTracks(
                player
                    .track_page(&self.hit, TrackOrder::Album, 20, 0)
                    .await
                    .expect("page reads"),
            ),
            12 => Output::PlayerTracks(
                player
                    .track_page(&self.common, TrackOrder::Album, 20, 0)
                    .await
                    .expect("page reads"),
            ),
            13 => Output::PlayerTracks(
                player
                    .track_page(&open, TrackOrder::Random, 50, 0)
                    .await
                    .expect("page reads"),
            ),
            14 => Output::PlayerTracks(
                player
                    .tracks(&open, TrackOrder::Shuffle(7), 100, 0)
                    .await
                    .expect("page reads")
                    .0,
            ),
            15 => Output::PlayerAlbums(
                player
                    .album_page(&albums, AlbumOrder::Shuffle(7), 50, 9_000)
                    .await
                    .expect("page reads"),
            ),
            _ => Output::PlayerTracks(
                player
                    .tracks(&open, TrackOrder::By(OrderKey::Year, false), 100, 0)
                    .await
                    .expect("page reads")
                    .0,
            ),
        }
    }
}

/// Statements seen by the recording connection: the text as prepared, and
/// the same text with its binds written in.
static TRACED: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// `sqlite3_trace_v2` callback for `SQLITE_TRACE_STMT`: the third argument
/// is the statement about to run.
unsafe extern "C" fn trace_statement(
    _event: std::os::raw::c_uint,
    _context: *mut std::os::raw::c_void,
    statement: *mut std::os::raw::c_void,
    _text: *mut std::os::raw::c_void,
) -> std::os::raw::c_int {
    use rusqlite::ffi;
    let statement = statement.cast::<ffi::sqlite3_stmt>();
    // SAFETY: SQLite passes a live statement, `sqlite3_sql` returns text the
    // statement owns, and the expanded text is ours to free.
    unsafe {
        let template = ffi::sqlite3_sql(statement);
        let expanded = ffi::sqlite3_expanded_sql(statement);
        if !template.is_null() && !expanded.is_null() {
            let pair = (
                std::ffi::CStr::from_ptr(template)
                    .to_string_lossy()
                    .into_owned(),
                std::ffi::CStr::from_ptr(expanded)
                    .to_string_lossy()
                    .into_owned(),
            );
            TRACED
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .push(pair);
        }
        if !expanded.is_null() {
            ffi::sqlite3_free(expanded.cast());
        }
    }
    0
}

/// The binds of one statement, read back by walking the prepared text
/// beside the expanded one: each `?` outside a string literal stands where
/// the expanded text has a literal.
fn split_binds(template: &str, expanded: &str) -> Vec<Value> {
    let (template, expanded) = (template.as_bytes(), expanded.as_bytes());
    let (mut at, mut seen) = (0, 0);
    let mut quoted = false;
    let mut binds = Vec::new();
    while at < template.len() {
        if template[at] == b'?' && !quoted {
            at += 1;
            while at < template.len() && template[at].is_ascii_digit() {
                at += 1;
            }
            if expanded[seen] == b'\'' {
                let mut text = Vec::new();
                seen += 1;
                loop {
                    if expanded[seen] == b'\'' {
                        if expanded.get(seen + 1) == Some(&b'\'') {
                            text.push(b'\'');
                            seen += 2;
                            continue;
                        }
                        seen += 1;
                        break;
                    }
                    text.push(expanded[seen]);
                    seen += 1;
                }
                binds.push(Value::Text(String::from_utf8(text).expect("utf-8 bind")));
            } else if expanded[seen..].starts_with(b"NULL") {
                seen += 4;
                binds.push(Value::Null);
            } else {
                let start = seen;
                while seen < expanded.len()
                    && (expanded[seen].is_ascii_digit()
                        || matches!(expanded[seen], b'-' | b'+' | b'.' | b'e' | b'E'))
                {
                    seen += 1;
                }
                let number = std::str::from_utf8(&expanded[start..seen]).expect("ascii number");
                binds.push(match number.parse::<i64>() {
                    Ok(whole) => Value::Integer(whole),
                    Err(_) => Value::Real(number.parse().expect("numeric bind")),
                });
            }
            continue;
        }
        if template[at] == b'\'' {
            quoted = !quoted;
        }
        assert_eq!(
            template[at],
            expanded[seen],
            "expanded text follows its template:\n{}\n{}",
            String::from_utf8_lossy(template),
            String::from_utf8_lossy(expanded)
        );
        at += 1;
        seen += 1;
    }
    assert_eq!(seen, expanded.len(), "expanded text ends with its template");
    binds
}

/// How a replayed statement's rows are decoded.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    /// One page of ids, in order.
    Ids,
    Albums,
    Tracks,
    ArtistRows,
    AlbumCounts,
    CreditCounts,
    StatsMain,
    Formats,
    PlayerTracks,
    PlayerAlbums,
    /// Counts, flags and anything else: read and dropped.
    Scalars,
}

impl Shape {
    fn of(columns: &[String]) -> Self {
        let has = |name: &str| columns.iter().any(|column| column == name);
        let is = |names: &[&str]| columns.iter().map(String::as_str).eq(names.iter().copied());
        if has("tag_genre") {
            Shape::PlayerTracks
        } else if has("primary_genre") {
            Shape::PlayerAlbums
        } else if has("contribution_state") {
            Shape::Albums
        } else if has("album_artist_mbid") {
            Shape::Tracks
        } else if is(&["id", "name", "artist_mbid", "linked", "date_added"]) {
            Shape::ArtistRows
        } else if has("size_bytes") {
            Shape::StatsMain
        } else if has("appearances") {
            Shape::CreditCounts
        } else if is(&["id", "total"]) {
            Shape::AlbumCounts
        } else if is(&["format", "total"]) {
            Shape::Formats
        } else if is(&["id"]) {
            Shape::Ids
        } else {
            Shape::Scalars
        }
    }

    /// Rows read for the ids of the page before them.
    fn follows_page(self) -> bool {
        matches!(
            self,
            Shape::Albums
                | Shape::Tracks
                | Shape::ArtistRows
                | Shape::PlayerTracks
                | Shape::PlayerAlbums
        )
    }
}

/// One recorded statement of a read.
struct Step {
    sql: String,
    binds: Vec<Value>,
    shape: Shape,
    columns: HashMap<String, usize>,
}

type Plan = Arc<Vec<Step>>;

/// Record the statements each read issues, on a connection of its own. Each
/// read runs once unrecorded first, so what is kept is the warm sequence the
/// timed calls issue.
async fn record_plans(path: &Path) -> Vec<Plan> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .read_only(true),
        )
        .await
        .expect("recording connection opens");
    {
        let mut connection = pool.acquire().await.expect("recording connection");
        let mut handle = connection.lock_handle().await.expect("handle locks");
        // SAFETY: the handle is locked for this call, and the callback is a
        // plain function that outlives the connection.
        let status = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                handle.as_raw_handle().as_ptr().cast(),
                rusqlite::ffi::SQLITE_TRACE_STMT as std::os::raw::c_uint,
                Some(trace_statement),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, rusqlite::ffi::SQLITE_OK, "trace installs");
    }
    let reads = Reads::over(&pool);
    let probe =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("probe connection opens");
    let mut plans = Vec::new();
    for (read, name) in READS.iter().enumerate() {
        reads.run(read).await;
        TRACED
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clear();
        reads.run(read).await;
        let traced = std::mem::take(&mut *TRACED.lock().unwrap_or_else(|held| held.into_inner()));
        assert!(!traced.is_empty(), "{name} issued no statement");
        let steps = traced
            .into_iter()
            // A statement SQLite runs inside another (the FTS index reading
            // its own tables) is traced as a comment. The outer statement
            // issues it again on replay.
            .filter(|(_, expanded)| !expanded.starts_with("-- "))
            .map(|(template, expanded)| {
                let binds = split_binds(&template, &expanded);
                let names: Vec<String> = probe
                    .prepare(&template)
                    .expect("recorded statement prepares")
                    .column_names()
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                Step {
                    binds,
                    shape: Shape::of(&names),
                    columns: names
                        .into_iter()
                        .enumerate()
                        .map(|(at, name)| (name, at))
                        .collect(),
                    sql: template,
                }
            })
            .collect();
        plans.push(Arc::new(steps));
    }
    pool.close().await;
    plans
}

/// One row read by column name, with the positions looked up once per
/// statement.
struct Cells<'a, 'row> {
    row: &'a rusqlite::Row<'row>,
    columns: &'a HashMap<String, usize>,
}

impl Cells<'_, '_> {
    fn get<T: rusqlite::types::FromSql>(&self, name: &str) -> rusqlite::Result<T> {
        self.row.get(self.columns[name])
    }
}

fn album_cells(cells: &Cells<'_, '_>, contributions: bool) -> rusqlite::Result<AlbumRecord> {
    let track_count: i64 = cells.get("track_count")?;
    Ok(AlbumRecord {
        id: cells.get("id")?,
        title: cells.get("title")?,
        artist_name: cells.get("artist_name")?,
        artist_id: cells.get("artist_id")?,
        release_group_mbid: cells.get("release_group_mbid")?,
        release_mbid: cells.get("release_mbid")?,
        artist_mbid: cells.get("artist_mbid")?,
        linked: cells.get("linked")?,
        track_count: track_count.max(0) as u64,
        total_duration_seconds: cells.get("total_duration_seconds")?,
        total_size_bytes: cells.get("total_size_bytes")?,
        format: cells.get("format")?,
        year: cells.get("year")?,
        is_compilation: cells.get("is_compilation")?,
        cover_available: cells.get("cover_available")?,
        date_added: cells.get("date_added")?,
        contribution_id: if contributions {
            cells.get("contribution_id")?
        } else {
            None
        },
        contribution_state: if contributions {
            cells.get("contribution_state")?
        } else {
            None
        },
    })
}

fn track_cells(cells: &Cells<'_, '_>) -> rusqlite::Result<TrackRecord> {
    Ok(TrackRecord {
        id: cells.get("id")?,
        title: cells.get("title")?,
        album_id: cells.get("album_id")?,
        album_title: cells.get("album_title")?,
        artist_name: cells.get("artist_name")?,
        artist_id: cells.get("artist_id")?,
        album_artist_name: cells.get("album_artist_name")?,
        album_artist_id: cells.get("album_artist_id")?,
        recording_mbid: cells.get("recording_mbid")?,
        release_group_mbid: cells.get("release_group_mbid")?,
        artist_mbid: cells.get("artist_mbid")?,
        album_artist_mbid: cells.get("album_artist_mbid")?,
        disc_number: cells.get("disc_number")?,
        track_number: cells.get("track_number")?,
        year: cells.get("year")?,
        genre: cells.get("genre")?,
        duration_seconds: cells.get("duration_seconds")?,
        format: cells.get("format")?,
        bit_rate: cells.get("bit_rate")?,
        sample_rate: cells.get("sample_rate")?,
        bit_depth: cells.get("bit_depth")?,
        channels: cells.get("channels")?,
        file_size_bytes: cells.get("file_size_bytes")?,
        date_added: cells.get("date_added")?,
        cover_available: cells.get("cover_available")?,
    })
}

fn player_track_cells(cells: &Cells<'_, '_>) -> rusqlite::Result<PlayerTrack> {
    let genres: Option<String> = cells.get("genres")?;
    let tag_genre: Option<String> = cells.get("tag_genre")?;
    let genres = match genres.filter(|value| !value.is_empty()) {
        Some(list) => list.split('\u{1f}').map(str::to_owned).collect(),
        None => tag_genre
            .into_iter()
            .filter(|genre| !genre.trim().is_empty())
            .collect(),
    };
    Ok(PlayerTrack {
        id: cells.get("id")?,
        title: cells.get("title")?,
        album_id: cells.get("album_id")?,
        album_title: cells.get("album_title")?,
        artist_name: cells.get("artist_name")?,
        artist_id: cells.get("artist_id")?,
        album_artist_id: cells.get("album_artist_id")?,
        album_artist_name: cells.get("album_artist_name")?,
        disc_number: cells.get("disc_number")?,
        track_number: cells.get("track_number")?,
        year: cells.get("year")?,
        genres,
        duration_seconds: cells.get("duration_seconds")?,
        format: cells.get("format")?,
        bit_rate: cells.get("bit_rate")?,
        sample_rate: cells.get("sample_rate")?,
        bit_depth: cells.get("bit_depth")?,
        channels: cells.get("channels")?,
        file_size_bytes: cells.get("file_size_bytes")?,
        relative_path: cells.get("relative_path")?,
        date_added: cells.get("date_added")?,
        sort_name: cells.get("sort_name")?,
        disc_subtitle: cells.get("disc_subtitle")?,
        recording_mbid: cells.get("recording_mbid")?,
        artist_mbid: cells.get("artist_mbid")?,
        album_artist_mbid: cells.get("album_artist_mbid")?,
        release_group_mbid: cells.get("release_group_mbid")?,
        release_type: cells.get("release_type")?,
        replaygain_track_gain: cells.get("rg_track_gain")?,
        replaygain_album_gain: cells.get("rg_album_gain")?,
        replaygain_track_peak: cells.get("rg_track_peak")?,
        replaygain_album_peak: cells.get("rg_album_peak")?,
        cover_available: cells.get("cover_available")?,
    })
}

/// Records keyed by id, put back in the order of the page that asked for
/// them, as the catalog does. With no page, row order stands.
fn in_page_order<T>(
    records: Vec<T>,
    page: Option<&Vec<String>>,
    id: impl Fn(&T) -> &String,
) -> Vec<T> {
    let Some(page) = page else {
        return records;
    };
    let mut by_id = HashMap::with_capacity(records.len());
    for record in records {
        by_id.insert(id(&record).clone(), record);
    }
    page.iter().filter_map(|id| by_id.remove(id)).collect()
}

/// What a read carries from one statement to the next: the page of ids and
/// the partly built records. It leaves one closure by value and enters the
/// next, as the catalog holds them between two sqlx calls.
struct Carried {
    page: Option<Vec<String>>,
    output: Output,
    artists: Option<Vec<ArtistRecord>>,
    album_counts: HashMap<String, u64>,
    credit_counts: HashMap<String, (u64, u64)>,
    stats: Option<StatsRecord>,
}

impl Carried {
    fn new() -> Self {
        Self {
            page: None,
            output: Output::Nothing,
            artists: None,
            album_counts: HashMap::new(),
            credit_counts: HashMap::new(),
            stats: None,
        }
    }

    /// The records the catalog returns, once every statement has run.
    fn finish(self) -> Output {
        let page = self.page;
        if let Some(mut records) = self.artists {
            for record in &mut records {
                record.album_count = self.album_counts.get(&record.id).copied().unwrap_or(0);
                let (tracks, appearances) = self
                    .credit_counts
                    .get(&record.id)
                    .copied()
                    .unwrap_or((0, 0));
                record.track_count = tracks;
                record.appearance_album_count = appearances;
            }
            return Output::Artists(in_page_order(records, page.as_ref(), |r| &r.id));
        }
        if let Some(stats) = self.stats {
            return Output::Stats(stats);
        }
        self.output
    }
}

/// One recorded statement on one pooled connection, decoded into the
/// records the catalog returns. Rows read for a page of ids take the ids
/// this run's page returned.
fn replay_step(
    connection: &rusqlite::Connection,
    step: &Step,
    carried: &mut Carried,
) -> rusqlite::Result<()> {
    let Carried {
        page,
        output,
        artists,
        album_counts,
        credit_counts,
        stats,
    } = carried;
    {
        let mut statement = connection.prepare_cached(&step.sql)?;
        let chained = step.shape.follows_page()
            && page
                .as_ref()
                .is_some_and(|ids| ids.len() == step.binds.len());
        let mut rows = match page.as_ref() {
            Some(ids) if chained => statement.query(rusqlite::params_from_iter(ids.iter()))?,
            _ => statement.query(rusqlite::params_from_iter(step.binds.iter()))?,
        };
        let columns = &step.columns;
        match step.shape {
            Shape::Ids => {
                let mut ids = Vec::new();
                while let Some(row) = rows.next()? {
                    ids.push(row.get::<_, String>(0)?);
                }
                *page = Some(ids);
            }
            Shape::Albums => {
                let mut records = Vec::new();
                while let Some(row) = rows.next()? {
                    records.push(album_cells(&Cells { row, columns }, true)?);
                }
                *output = Output::Albums(in_page_order(records, page.as_ref(), |r| &r.id));
            }
            Shape::Tracks => {
                let mut records = Vec::new();
                while let Some(row) = rows.next()? {
                    records.push(track_cells(&Cells { row, columns })?);
                }
                *output = Output::Tracks(records);
            }
            Shape::ArtistRows => {
                let mut records = Vec::new();
                while let Some(row) = rows.next()? {
                    let cells = Cells { row, columns };
                    records.push(ArtistRecord {
                        id: cells.get("id")?,
                        name: cells.get("name")?,
                        artist_mbid: cells.get("artist_mbid")?,
                        linked: cells.get("linked")?,
                        album_count: 0,
                        track_count: 0,
                        appearance_album_count: 0,
                        date_added: cells.get("date_added")?,
                    });
                }
                *artists = Some(records);
            }
            Shape::AlbumCounts => {
                while let Some(row) = rows.next()? {
                    let total: i64 = row.get(1)?;
                    album_counts.insert(row.get(0)?, total.max(0) as u64);
                }
            }
            Shape::CreditCounts => {
                while let Some(row) = rows.next()? {
                    let cells = Cells { row, columns };
                    let tracks: i64 = cells.get("tracks")?;
                    let appearances: i64 = cells.get("appearances")?;
                    credit_counts.insert(
                        cells.get("id")?,
                        (tracks.max(0) as u64, appearances.max(0) as u64),
                    );
                }
            }
            Shape::StatsMain => {
                while let Some(row) = rows.next()? {
                    let cells = Cells { row, columns };
                    let albums: i64 = cells.get("albums")?;
                    let artists: i64 = cells.get("artists")?;
                    let tracks: i64 = cells.get("tracks")?;
                    *stats = Some(StatsRecord {
                        total_albums: albums.max(0) as u64,
                        total_artists: artists.max(0) as u64,
                        total_tracks: tracks.max(0) as u64,
                        total_size_bytes: cells.get("size_bytes")?,
                        format_breakdown: HashMap::new(),
                    });
                }
            }
            Shape::Formats => {
                while let Some(row) = rows.next()? {
                    let total: i64 = row.get(1)?;
                    if let Some(stats) = stats.as_mut() {
                        stats
                            .format_breakdown
                            .insert(row.get(0)?, total.max(0) as u64);
                    }
                }
            }
            Shape::PlayerTracks => {
                let mut records = Vec::new();
                while let Some(row) = rows.next()? {
                    records.push(player_track_cells(&Cells { row, columns })?);
                }
                *output = Output::PlayerTracks(in_page_order(records, page.as_ref(), |r| &r.id));
            }
            Shape::PlayerAlbums => {
                let mut records = Vec::new();
                while let Some(row) = rows.next()? {
                    let cells = Cells { row, columns };
                    records.push(PlayerAlbum {
                        record: album_cells(&cells, false)?,
                        genre: cells.get("primary_genre")?,
                        sort_name: cells.get("album_sort_name")?,
                        original_release_date: cells.get("original_release_date")?,
                    });
                }
                *output =
                    Output::PlayerAlbums(in_page_order(records, page.as_ref(), |r| &r.record.id));
            }
            Shape::Scalars => {
                while let Some(row) = rows.next()? {
                    for at in 0..columns.len() {
                        std::hint::black_box(row.get::<_, Value>(at)?);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Everything a timed sample needs.
struct Replay {
    reads: Reads,
    plans: Vec<Plan>,
}

/// Which way a sample goes.
#[derive(Clone)]
enum Via {
    Sqlx,
    Pool(Db),
}

impl Replay {
    async fn build(fixture: &Fixture) -> Self {
        Self {
            reads: Reads::over(&fixture.pool),
            plans: record_plans(fixture.runtime.path()).await,
        }
    }

    async fn via_sqlx(&self, read: usize) -> Output {
        self.reads.run(read).await
    }

    /// One `read` per recorded statement, in order, as each sqlx call is one
    /// checkout: the read queues for a connection per statement and holds
    /// nothing between two.
    async fn via_pool(&self, db: &Db, read: usize) -> Output {
        let mut carried = Carried::new();
        for at in 0..self.plans[read].len() {
            let plan = Arc::clone(&self.plans[read]);
            carried = db
                .read("replay.step", move |connection| -> Result<_, DbError> {
                    let mut carried = carried;
                    replay_step(connection, &plan[at], &mut carried)?;
                    Ok(carried)
                })
                .await
                .expect("pooled read");
        }
        carried.finish()
    }

    /// One timed read, in milliseconds. The result is dropped inside the
    /// timing on both paths, as the acceptance tests do.
    async fn sample(&self, via: &Via, read: usize) -> f64 {
        let tick = Instant::now();
        match via {
            Via::Sqlx => drop(std::hint::black_box(self.via_sqlx(read).await)),
            Via::Pool(pool) => drop(std::hint::black_box(self.via_pool(pool, read).await)),
        }
        tick.elapsed().as_secs_f64() * 1000.0
    }

    /// Both paths return the same records for every read. The random read
    /// and an empty page are held to their row count.
    async fn assert_same_records(&self, pool: &Db) {
        for (read, name) in READS.iter().enumerate() {
            let catalog = self.via_sqlx(read).await;
            let replayed = self.via_pool(pool, read).await;
            // An empty page has no rows to decode, so the replay has no
            // record type to give it.
            if read == RANDOM_READ || catalog.len() == 0 {
                assert_eq!(catalog.len(), replayed.len(), "{name}: row count");
            } else {
                assert_eq!(catalog, replayed, "{name}: records");
            }
        }
    }

    /// Per read: warm-ups, then timed pairs, one sample of each path back
    /// to back with the order swapping every pair. Returns the p95 of each
    /// path per read.
    async fn serial(
        &self,
        first: &Via,
        second: &Via,
        warmups: usize,
        pairs: usize,
    ) -> Vec<(f64, f64)> {
        let mut out = Vec::with_capacity(READS.len());
        for read in 0..READS.len() {
            for _ in 0..warmups {
                self.sample(first, read).await;
                self.sample(second, read).await;
            }
            let (mut one, mut two) = (Vec::with_capacity(pairs), Vec::with_capacity(pairs));
            for pair in 0..pairs {
                if pair % 2 == 0 {
                    one.push(self.sample(first, read).await);
                    two.push(self.sample(second, read).await);
                } else {
                    two.push(self.sample(second, read).await);
                    one.push(self.sample(first, read).await);
                }
            }
            out.push((percentile(&mut one, 95.0), percentile(&mut two, 95.0)));
        }
        out
    }
}

const SERIAL_WARMUPS: usize = 10;
const SERIAL_PAIRS: usize = 200;
const CONCURRENT_TASKS: usize = 16;
const CONCURRENT_ROUNDS: usize = 20;
const WRITER_ROWS: i64 = 256;

/// One concurrent block: 16 tasks each run the 17 reads in turn for 20
/// rounds while one writer commits 256-row transactions without pause.
/// Returns the samples per read and the writer's commit count.
async fn concurrent_block(
    replay: &Arc<Replay>,
    via: &Via,
    lane: &WriteLane,
) -> (Vec<Vec<f64>>, u64) {
    let stop = Arc::new(AtomicBool::new(false));
    let writer = tokio::spawn({
        let (stop, lane) = (Arc::clone(&stop), lane.clone());
        async move {
            let mut commits: u64 = 0;
            while !stop.load(Ordering::Relaxed) {
                let stamp = commits as i64;
                lane.write(Lane::Background, "replay writer", move |tx| {
                    let mut insert = tx.prepare_cached(
                        "INSERT OR REPLACE INTO replay_writes (id, stamp) VALUES (?1, ?2)",
                    )?;
                    for id in 0..WRITER_ROWS {
                        insert.execute((id, stamp))?;
                    }
                    Ok(())
                })
                .await
                .expect("writer commits");
                commits += 1;
            }
            commits
        }
    });
    let mut tasks = Vec::with_capacity(CONCURRENT_TASKS);
    for _ in 0..CONCURRENT_TASKS {
        let (replay, via) = (Arc::clone(replay), via.clone());
        tasks.push(tokio::spawn(async move {
            let mut samples = vec![Vec::with_capacity(CONCURRENT_ROUNDS); READS.len()];
            for _ in 0..CONCURRENT_ROUNDS {
                for (read, bucket) in samples.iter_mut().enumerate() {
                    bucket.push(replay.sample(&via, read).await);
                }
            }
            samples
        }));
    }
    let mut merged = vec![Vec::new(); READS.len()];
    for task in tasks {
        for (read, samples) in task.await.expect("reader task").into_iter().enumerate() {
            merged[read].extend(samples);
        }
    }
    stop.store(true, Ordering::Relaxed);
    (merged, writer.await.expect("writer task"))
}

/// Blocks in the given order; a path's samples are its blocks together.
/// Returns the p95 per read for `first` and `second`, and the commits the
/// writer made beside each.
async fn concurrent(
    replay: &Arc<Replay>,
    first: &Via,
    second: &Via,
    lane: &WriteLane,
) -> (Vec<(f64, f64)>, (u64, u64)) {
    let mut samples = [vec![Vec::new(); READS.len()], vec![Vec::new(); READS.len()]];
    let mut commits = [0u64; 2];
    for which in [0, 1, 1, 0] {
        let via = if which == 0 { first } else { second };
        let (block, written) = concurrent_block(replay, via, lane).await;
        for (read, values) in block.into_iter().enumerate() {
            samples[which][read].extend(values);
        }
        commits[which] += written;
    }
    let [mut one, mut two] = samples;
    let p95 = (0..READS.len())
        .map(|read| {
            (
                percentile(&mut one[read], 95.0),
                percentile(&mut two[read], 95.0),
            )
        })
        .collect();
    (p95, (commits[0], commits[1]))
}

fn load_average() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .expect("load average reads")
        .split_whitespace()
        .next()
        .and_then(|first| first.parse().ok())
        .expect("load average parses")
}

/// The 1-minute load average the pass starts at. Waits up to ten minutes
/// for it to fall under 2.0 and refuses to start otherwise.
async fn wait_for_quiet_host() -> f64 {
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let load = load_average();
        if load < 2.0 {
            return load;
        }
        assert!(
            Instant::now() < deadline,
            "refused to start: the load average is {load:.2} after ten minutes of waiting; \
             nothing was timed"
        );
        eprintln!("load average {load:.2}, waiting for it to fall under 2.0");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Resident and peak resident memory of this process, in MiB.
fn resident_mib() -> (f64, f64) {
    let status = std::fs::read_to_string("/proc/self/status").expect("status reads");
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kib| kib.parse::<f64>().ok())
            .expect("memory field parses")
            / 1024.0
    };
    (field("VmRSS:"), field("VmHWM:"))
}

/// The p95 of both paths for every read, one pass.
#[derive(Clone, Default)]
struct Pass {
    /// Load average at the start, after the serial part and at the end.
    load: [f64; 3],
    /// (sqlx, pool) per read.
    serial: Vec<(f64, f64)>,
    concurrent: Vec<(f64, f64)>,
}

/// Where the passes are kept between invocations: one pass per invocation,
/// so a pass and its wait for a quiet host fit one command.
fn passes_file() -> PathBuf {
    std::env::var_os("READ_POOL_REPLAY_PASSES")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../read-pool-replay.tsv"))
}

/// Values are kept as their bits, so the verdict is worked from what was
/// measured and not from a rounded print.
fn load_passes(file: &Path) -> Vec<Pass> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let bits = |field: &str| f64::from_bits(u64::from_str_radix(field, 16).expect("bits parse"));
    let mut passes: Vec<Pass> = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["load", start, middle, end] => passes.push(Pass {
                load: [bits(start), bits(middle), bits(end)],
                ..Pass::default()
            }),
            [part, _name, sqlx, pool] => {
                let pass = passes.last_mut().expect("a pass starts with its load line");
                let pair = (bits(sqlx), bits(pool));
                if *part == "serial" {
                    pass.serial.push(pair);
                } else {
                    pass.concurrent.push(pair);
                }
            }
            _ => panic!("unreadable line in {}: {line}", file.display()),
        }
    }
    passes
}

fn save_pass(file: &Path, pass: &Pass) {
    use std::io::Write as _;
    let mut text = format!(
        "load\t{:x}\t{:x}\t{:x}\n",
        pass.load[0].to_bits(),
        pass.load[1].to_bits(),
        pass.load[2].to_bits()
    );
    for (part, values) in [("serial", &pass.serial), ("concurrent", &pass.concurrent)] {
        for (read, (sqlx, pool)) in values.iter().enumerate() {
            text.push_str(&format!(
                "{part}\t{}\t{:x}\t{:x}\n",
                READS[read],
                sqlx.to_bits(),
                pool.to_bits()
            ));
        }
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .expect("pass is saved");
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    values[values.len() / 2]
}

/// Pool against sqlx: at most 1.25 times, or at most 0.5 ms above.
fn keeps_pace(sqlx: f64, pool: f64) -> bool {
    pool <= sqlx * 1.25 || pool <= sqlx + 0.5
}

/// The serial rule for one read. Under budget on sqlx, the pool keeps pace
/// and stays under 10 ms. At or over budget on sqlx, the pool is held to
/// sqlx plus 0.5 ms.
fn serial_holds(sqlx: f64, pool: f64) -> bool {
    if sqlx >= 10.0 {
        pool <= sqlx + 0.5
    } else {
        keeps_pace(sqlx, pool) && pool < 10.0
    }
}

fn print_pass(number: usize, pass: &Pass) {
    eprintln!(
        "pass {number}: load average {:.2} at the start, {:.2} after the serial part, {:.2} at the end",
        pass.load[0], pass.load[1], pass.load[2]
    );
    eprintln!(
        "  read | serial sqlx p95, pool p95, ratio | concurrent sqlx p95, pool p95, ratio (ms)"
    );
    for (read, name) in READS.iter().enumerate() {
        let (ss, sp) = pass.serial[read];
        let (cs, cp) = pass.concurrent[read];
        eprintln!(
            "  {name} | {ss:.3}, {sp:.3}, {:.3} | {cs:.3}, {cp:.3}, {:.3}",
            sp / ss,
            cp / cs
        );
    }
}

/// The table of S and P (the median of the passes' p95) and the verdict.
fn print_verdict(passes: &[Pass]) {
    for (at, pass) in passes.iter().enumerate() {
        print_pass(at + 1, pass);
    }
    eprintln!("median of {} passes:", passes.len());
    eprintln!("  read | serial S, P, P/S, holds | concurrent S, P, P/S, holds (ms)");
    let mut trips = Vec::new();
    let mut over_budget = Vec::new();
    let mut worst_serial = (0.0_f64, "");
    let mut worst_concurrent = (0.0_f64, "");
    for (read, name) in READS.iter().enumerate() {
        let pick = |part: fn(&Pass) -> &Vec<(f64, f64)>| {
            (
                median(passes.iter().map(|pass| part(pass)[read].0).collect()),
                median(passes.iter().map(|pass| part(pass)[read].1).collect()),
            )
        };
        let (ss, sp) = pick(|pass| &pass.serial);
        let (cs, cp) = pick(|pass| &pass.concurrent);
        let serial_ok = serial_holds(ss, sp);
        let concurrent_ok = keeps_pace(cs, cp);
        eprintln!(
            "  {name} | {ss:.3}, {sp:.3}, {:.3}, {} | {cs:.3}, {cp:.3}, {:.3}, {}",
            sp / ss,
            if serial_ok { "yes" } else { "NO" },
            cp / cs,
            if concurrent_ok { "yes" } else { "NO" },
        );
        if sp / ss > worst_serial.0 {
            worst_serial = (sp / ss, name);
        }
        if cp / cs > worst_concurrent.0 {
            worst_concurrent = (cp / cs, name);
        }
        if ss >= 10.0 {
            over_budget.push(format!("{name} (sqlx {ss:.3} ms)"));
        }
        if !serial_ok {
            trips.push(format!("{name} serial: sqlx {ss:.3} ms, pool {sp:.3} ms"));
        }
        if !concurrent_ok {
            trips.push(format!(
                "{name} concurrent: sqlx {cs:.3} ms, pool {cp:.3} ms"
            ));
        }
    }
    eprintln!(
        "worst serial ratio {:.3} ({}), worst concurrent ratio {:.3} ({})",
        worst_serial.0, worst_serial.1, worst_concurrent.0, worst_concurrent.1
    );
    if !over_budget.is_empty() {
        eprintln!(
            "sqlx itself is at 10 ms or more, so the pool is held to sqlx plus 0.5 ms: {}",
            over_budget.join(", ")
        );
    }
    if trips.is_empty() {
        eprintln!("VERDICT: pass");
    } else {
        eprintln!("VERDICT: trip\n  {}", trips.join("\n  "));
    }
}

/// The scratch table the concurrent writer rewrites. It is no catalog
/// table, so the catalog revision the player reads cache against stays put.
async fn seed_replay(fixture: &Fixture) {
    write_batch(
        fixture,
        "replay writes table",
        "CREATE TABLE replay_writes (id INTEGER PRIMARY KEY, stamp INTEGER NOT NULL) STRICT;"
            .to_owned(),
    )
    .await;
}

async fn settle(fixture: &Fixture) {
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
}

/// The replay returns what the catalog returns, read for read, on the
/// small adversarial catalog.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replayed_reads_match_the_catalog() {
    let fixture = seed_small().await;
    seed_replay(&fixture).await;
    let replay = Arc::new(Replay::build(&fixture).await);
    let db = fixture.runtime.db().clone();
    replay.assert_same_records(&db).await;
    let (samples, commits) =
        concurrent_block(&replay, &Via::Pool(db), fixture.runtime.lane()).await;
    assert!(commits > 0, "the writer committed beside the reads");
    assert!(
        samples
            .iter()
            .all(|read| read.len() == CONCURRENT_TASKS * CONCURRENT_ROUNDS)
    );
}

/// One pass of the read pool measurement on the seeded 100k catalog; the
/// third pass prints the verdict. Ignored: run with --release, alone on the
/// host, once per pass.
#[test]
#[ignore = "100k read pool measurement; run explicitly with --release, one pass per invocation"]
fn read_pool_replay() {
    let file = passes_file();
    let done = load_passes(&file);
    if done.len() >= 3 {
        print_verdict(&done);
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime builds");
    // The pass runs as a task, so every sample starts on a worker thread
    // as a request handler does.
    let pass = runtime
        .block_on(runtime.spawn(replay_pass(done.len() + 1, file.clone())))
        .expect("pass runs");
    let passes = load_passes(&file);
    if passes.len() >= 3 {
        print_verdict(&passes);
    } else {
        print_pass(passes.len(), &pass);
        eprintln!("{} of 3 passes done; no verdict yet", passes.len());
    }
}

/// Seeds, times one pass and saves it to `file` as soon as it is timed.
async fn replay_pass(number: usize, file: PathBuf) -> Pass {
    let fixture = seed_small().await;
    seed_100k(&fixture).await;
    seed_replay(&fixture).await;
    // Production statistics, as the player acceptance test runs them.
    droppedneedle_kernel::db::AnalyzeService::new(
        fixture.runtime.lane().clone(),
        fixture.pool.clone(),
    )
    .run()
    .await
    .expect("analyze runs");
    settle(&fixture).await;
    let stats = fixture.catalog.stats().await.expect("stats read");
    assert_eq!(stats.total_tracks, 100007);
    let path = fixture.runtime.path().to_owned();
    let lane = fixture.runtime.lane().clone();
    let replay = Arc::new(Replay::build(&fixture).await);
    for read in 0..READS.len() {
        replay.via_sqlx(read).await;
    }
    let db = fixture.runtime.db().clone();
    replay.assert_same_records(&db).await;
    let before = resident_mib();
    let (today, pooled) = (Via::Sqlx, Via::Pool(db));

    let mut pass = Pass::default();
    pass.load[0] = wait_for_quiet_host().await;
    eprintln!("pass {number} starts at load average {:.2}", pass.load[0]);
    pass.serial = replay
        .serial(&today, &pooled, SERIAL_WARMUPS, SERIAL_PAIRS)
        .await;
    pass.load[1] = load_average();
    let (judged, commits) = concurrent(&replay, &today, &pooled, &lane).await;
    pass.concurrent = judged;
    pass.load[2] = load_average();
    save_pass(&file, &pass);
    let warmed = resident_mib();
    let wal_mib = std::fs::metadata(format!("{}-wal", path.display()))
        .map(|wal| wal.len() as f64 / 1_048_576.0)
        .unwrap_or(0.0);
    eprintln!(
        "writer commits beside the concurrent reads: {} beside sqlx, {} beside the pool; \
         the WAL file is {wal_mib:.1} MiB afterwards",
        commits.0, commits.1
    );

    // Report only; it does not reach the verdict.
    eprintln!(
        "report only, resident memory (MiB, current and peak): {:.1} and {:.1} with both paths \
         warm; {:.1} and {:.1} after the timed parts",
        before.0, before.1, warmed.0, warmed.1
    );
    pass
}

/// The rule the verdict applies, and the passes surviving their file.
#[test]
fn replay_rule_and_saved_passes() {
    assert!(keeps_pace(1.0, 1.25) && keeps_pace(0.3, 0.8) && !keeps_pace(4.0, 5.01));
    assert!(serial_holds(8.0, 9.9) && !serial_holds(8.0, 10.0) && !serial_holds(4.0, 5.01));
    assert!(serial_holds(12.0, 12.5) && !serial_holds(12.0, 12.51));
    assert_eq!(median(vec![3.0, 1.0, 2.0]), 2.0);

    let dir =
        droppedneedle_kernel::db::scratch::ScratchDir::new("replay-passes").expect("scratch dir");
    let file = dir.join("passes.tsv");
    assert!(load_passes(&file).is_empty());
    for pass in 0..3 {
        let value = |read: usize| {
            (
                0.1 + read as f64 / 3.0 + pass as f64,
                0.2 + read as f64 / 7.0,
            )
        };
        save_pass(
            &file,
            &Pass {
                load: [0.5, 1.5, 4.25],
                serial: (0..READS.len()).map(value).collect(),
                concurrent: (0..READS.len()).map(value).rev().collect(),
            },
        );
    }
    let passes = load_passes(&file);
    assert_eq!(passes.len(), 3);
    assert_eq!(passes[2].load, [0.5, 1.5, 4.25]);
    assert_eq!(
        passes[1].serial[16],
        (0.1 + 16.0 / 3.0 + 1.0, 0.2 + 16.0 / 7.0)
    );
    assert_eq!(passes[0].concurrent[0], passes[0].serial[16]);
    print_verdict(&passes);
}
