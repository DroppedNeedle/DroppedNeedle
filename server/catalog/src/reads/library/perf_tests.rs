//! Read budget tests: the six standard reads hold the 10 ms p95 budget on
//! a 100k catalog.
//!
//! The library pages return the pages an adversarial fixture expects, the
//! maintained totals prove equal to live aggregation across a mutation
//! battery, the miss oracle agrees with the substring match across a
//! hostile query matrix, the store's own registered statements prove
//! index-driven via EXPLAIN, and the ignored acceptance tests time the
//! endpoints on a seeded 100k catalog. Nothing here touches the network;
//! every database is a scratch file.

use crate::reads::library;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::{Db, DbConfig, DbError, DbRuntime, Lane, fold_text, open_runtime};
use library::sql;
use library::sqlite::fts_match_phrase;
use library::store::LocalCatalog;
use library::stores::{
    After, AlbumFilter, AlbumRecord, AlbumSort, ArtistRecord, ArtistScope, ArtistSort, TrackFilter,
    TrackRecord, TrackSort,
};
use rusqlite::types::Value;

static PERF_SEQ: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    runtime: DbRuntime,
    catalog: LocalCatalog,
    /// Dropped last: removes the scratch database.
    _dir: droppedneedle_kernel::db::scratch::ScratchDir,
}

impl Fixture {
    fn db(&self) -> &Db {
        self.runtime.db()
    }
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
    let seed = seed_small_sql();
    runtime
        .db()
        .write(Lane::Foreground, "perf seed", move |tx| {
            tx.execute_batch(&seed)?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("seed batch runs");
    Fixture {
        catalog: LocalCatalog::new(runtime.db().clone()),
        runtime,
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

/// Every row `text` answers over `values`, mapped: a test read through the
/// request pool.
async fn fetch<T: Send + 'static>(
    db: &Db,
    text: &str,
    values: Vec<Value>,
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Vec<T> {
    let text = text.to_owned();
    db.read("perf test read", move |conn| -> Result<Vec<T>, DbError> {
        Ok(library::store::rows(conn, &text, &values, map)?)
    })
    .await
    .expect("test read")
}

/// The one row `text` answers over `values`, mapped.
async fn fetch_one<T: Send + 'static>(
    db: &Db,
    text: &str,
    values: Vec<Value>,
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> T {
    fetch(db, text, values, map)
        .await
        .into_iter()
        .next()
        .expect("one row")
}

/// A text value.
fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

/// The track totals live aggregation gives over the base tables: what the
/// maintained totals must match after every mutation.
async fn ground_totals(db: &Db) -> (u64, i64, HashMap<String, u64>) {
    let (tracks, size_bytes): (i64, i64) = fetch_one(
        db,
        "SELECT COUNT(*), COALESCE(SUM(file_size_bytes), 0) FROM local_tracks \
         WHERE availability = 'indexed'",
        Vec::new(),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .await;
    let formats: Vec<(String, i64)> = fetch(
        db,
        "SELECT file_format, COUNT(*) FROM local_tracks \
         WHERE availability = 'indexed' GROUP BY file_format",
        Vec::new(),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .await;
    let formats = formats
        .into_iter()
        .map(|(format, total)| (format, total.max(0) as u64))
        .collect();
    (tracks.max(0) as u64, size_bytes, formats)
}

async fn write_batch(fixture: &Fixture, label: &'static str, sql: String) {
    fixture
        .db()
        .write(Lane::Foreground, label, move |tx| {
            tx.execute_batch(&sql)?;
            Ok::<_, DbError>(())
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

/// The plan's detail lines for `text` over `binds`.
async fn explain(db: &Db, text: &str, binds: &[ExplainBind<'_>]) -> Vec<String> {
    let values = binds
        .iter()
        .map(|bind| match *bind {
            ExplainBind::Text(value) => value.map_or(Value::Null, self::text),
            ExplainBind::Int(value) => Value::Integer(value),
        })
        .collect();
    fetch(db, &format!("EXPLAIN QUERY PLAN {text}"), values, |row| {
        row.get(3)
    })
    .await
}

/// The plan's detail lines for `text` with every parameter bound NULL: a
/// plan does not depend on the values.
async fn explain_unbound(db: &Db, text: &str) -> Vec<String> {
    let binds = vec![Value::Null; text.matches('?').count()];
    fetch(db, &format!("EXPLAIN QUERY PLAN {text}"), binds, |row| {
        row.get(3)
    })
    .await
}

/// The plan lines of `text`'s own query, without its subqueries', every
/// parameter bound NULL.
async fn explain_outer(db: &Db, text: &str) -> Vec<String> {
    let binds = vec![Value::Null; text.matches('?').count()];
    let lines: Vec<(i64, String)> =
        fetch(db, &format!("EXPLAIN QUERY PLAN {text}"), binds, |row| {
            Ok((row.get(1)?, row.get(3)?))
        })
        .await;
    lines
        .into_iter()
        .filter(|(parent, _)| *parent == 0)
        .map(|(_, line)| line)
        .collect()
}

/// Every track sort.
const TRACK_SORTS: [TrackSort; 5] = [
    TrackSort::Title,
    TrackSort::DateAdded,
    TrackSort::Artist,
    TrackSort::Album,
    TrackSort::Random,
];

/// Every artist sort.
const ARTIST_SORTS: [ArtistSort; 4] = [
    ArtistSort::Name,
    ArtistSort::DateAdded,
    ArtistSort::AlbumCount,
    ArtistSort::AppearanceCount,
];

/// The album list's id order for each sort and direction over every album
/// of the small fixture.
const ALBUM_ORDERS: [(AlbumSort, bool, [&str; 5]); 10] = [
    (AlbumSort::Name, false, ["al5", "al4", "al1", "al3", "al2"]),
    (AlbumSort::Name, true, ["al2", "al3", "al1", "al4", "al5"]),
    (
        AlbumSort::DateAdded,
        false,
        ["al1", "al4", "al3", "al5", "al2"],
    ),
    (
        AlbumSort::DateAdded,
        true,
        ["al2", "al5", "al3", "al4", "al1"],
    ),
    (AlbumSort::Year, false, ["al5", "al1", "al3", "al4", "al2"]),
    (AlbumSort::Year, true, ["al2", "al4", "al3", "al1", "al5"]),
    (
        AlbumSort::Artist,
        false,
        ["al4", "al1", "al2", "al5", "al3"],
    ),
    (AlbumSort::Artist, true, ["al3", "al5", "al2", "al1", "al4"]),
    (
        AlbumSort::Rediscover,
        false,
        ["al1", "al4", "al3", "al5", "al2"],
    ),
    (
        AlbumSort::Rediscover,
        true,
        ["al2", "al5", "al3", "al4", "al1"],
    ),
];

/// The small fixture's albums as the default page reads them.
const ALBUM_RECORDS: [&str; 5] = [
    "AlbumRecord { id: \"al5\", title: \"100% \\\"Live\\\" _Unplugged_ 'Encore'\", \
     artist_name: \"Boreal\", artist_id: \"a2\", release_group_mbid: None, release_mbid: None, \
     artist_mbid: None, linked: false, track_count: 2, total_duration_seconds: 385.0, \
     total_size_bytes: 4000000, format: Some(\"opus\"), year: Some(1987), \
     is_compilation: false, cover_available: false, art_version: None, date_added: Some(1800.0), \
     contribution_id: None, contribution_state: None }",
    "AlbumRecord { id: \"al4\", title: \"Empty Halls\", artist_name: \"Aurora\", \
     artist_id: \"a1\", release_group_mbid: None, release_mbid: None, \
     artist_mbid: Some(\"m1\"), linked: false, track_count: 0, total_duration_seconds: 0.0, \
     total_size_bytes: 0, format: None, year: Some(1994), is_compilation: false, \
     cover_available: false, art_version: None, date_added: Some(1200.0), contribution_id: None, \
     contribution_state: None }",
    "AlbumRecord { id: \"al1\", title: \"First Light\", artist_name: \"Aurora\", \
     artist_id: \"a1\", release_group_mbid: Some(\"rg1\"), release_mbid: Some(\"r1\"), \
     artist_mbid: Some(\"m1\"), linked: true, track_count: 2, total_duration_seconds: 400.0, \
     total_size_bytes: 8000000, format: Some(\"flac\"), year: Some(1994), \
     is_compilation: false, cover_available: true, art_version: Some(1), date_added: Some(1000.0), \
     contribution_id: None, contribution_state: None }",
    "AlbumRecord { id: \"al3\", title: \"Lone Peak\", artist_name: \"Boreal\", \
     artist_id: \"a2\", release_group_mbid: None, release_mbid: None, artist_mbid: None, \
     linked: false, track_count: 2, total_duration_seconds: 430.0, \
     total_size_bytes: 6000000, format: Some(\"flac\"), year: Some(1994), \
     is_compilation: false, cover_available: false, art_version: None, date_added: Some(1500.0), \
     contribution_id: None, contribution_state: None }",
    "AlbumRecord { id: \"al2\", title: \"Second Dawn\", artist_name: \"Aurora\", \
     artist_id: \"a1\", release_group_mbid: None, release_mbid: None, \
     artist_mbid: Some(\"m1\"), linked: false, track_count: 1, total_duration_seconds: 180.0, \
     total_size_bytes: 3000000, format: Some(\"mp3\"), year: Some(2001), \
     is_compilation: false, cover_available: false, art_version: None, date_added: Some(2000.0), \
     contribution_id: None, contribution_state: None }",
];

fn album_ids(albums: &[AlbumRecord]) -> Vec<&str> {
    albums.iter().map(|album| album.id.as_str()).collect()
}

/// Every album the list gives, walked `limit` at a time from the first
/// page by each page's key, with each page's total.
async fn walk_albums(
    fixture: &Fixture,
    sort: AlbumSort,
    descending: bool,
    limit: u64,
    seed: u32,
) -> (Vec<String>, Vec<u64>) {
    let mut after = After { seed, key: None };
    let (mut ids, mut totals) = (Vec::new(), Vec::new());
    loop {
        let page = fixture
            .catalog
            .list_albums(&AlbumFilter::default(), sort, descending, limit, &after)
            .await
            .expect("lists");
        assert!(page.items.len() as u64 <= limit);
        ids.extend(page.items.iter().map(|album| album.id.clone()));
        totals.push(page.total);
        match page.next {
            Some(key) => after.key = Some(key),
            None => return (ids, totals),
        }
    }
}

/// The album list answers the fixture's pages: every sort in both
/// directions across filters, walked two at a time by cursor, the album
/// without files listed with no tracks. A filtered page is the sort's
/// order of every album kept to the filter's albums; one random walk
/// gives every album once.
#[tokio::test]
async fn album_pages_match_the_fixture() {
    let fixture = seed_small().await;
    let filter =
        |q: Option<&str>, artist: Option<&str>, decade, format: Option<&str>| AlbumFilter {
            q: q.map(str::to_owned),
            artist_id: artist.map(str::to_owned),
            decade,
            format: format.map(str::to_owned),
        };
    let filters: [(AlbumFilter, &[&str]); 7] = [
        (AlbumFilter::default(), &["al1", "al2", "al3", "al4", "al5"]),
        (filter(Some("light"), None, None, None), &["al1"]),
        (filter(Some("zzz-no-such-album"), None, None, None), &[]),
        (filter(Some("100% \"live\""), None, None, None), &["al5"]),
        (filter(None, Some("a1"), None, None), &["al1", "al2", "al4"]),
        (filter(None, None, Some(1990), None), &["al1", "al3", "al4"]),
        (filter(None, None, None, Some("flac")), &["al1", "al3"]),
    ];
    for (sort, descending, order) in ALBUM_ORDERS {
        for (filter, kept) in &filters {
            let page = fixture
                .catalog
                .list_albums(filter, sort, descending, 50, &After::default())
                .await
                .expect("lists");
            let want: Vec<&str> = order.into_iter().filter(|id| kept.contains(id)).collect();
            assert_eq!(
                (album_ids(&page.items), page.total, page.next),
                (want.clone(), want.len() as u64, None),
                "sort={sort:?} desc={descending} filter={filter:?}"
            );
        }
        assert_eq!(
            walk_albums(&fixture, sort, descending, 2, 0).await,
            (order.map(str::to_owned).to_vec(), vec![5, 5, 5]),
            "sort={sort:?} desc={descending}"
        );
    }
    let (mut ids, totals) = walk_albums(&fixture, AlbumSort::Random, false, 2, 42).await;
    assert_eq!(totals, [5, 5, 5]);
    ids.sort_unstable();
    assert_eq!(ids, ["al1", "al2", "al3", "al4", "al5"]);
    let page = fixture
        .catalog
        .list_albums(
            &AlbumFilter::default(),
            AlbumSort::Name,
            false,
            50,
            &After::default(),
        )
        .await
        .expect("lists");
    let records: Vec<String> = page
        .items
        .iter()
        .map(|album| format!("{album:?}"))
        .collect();
    assert_eq!(records, ALBUM_RECORDS);
}

/// A filtered player total runs its count once per catalog revision: a
/// change made without moving the revision is not counted again, and the
/// next revision counts afresh.
#[tokio::test]
async fn filtered_player_totals_count_once_per_revision() {
    use library::player::{AlbumOrder, AlbumQuery, TrackOrder, TrackQuery};

    let fixture = seed_small().await;
    let tracks = TrackQuery {
        album_id: Some("al2".to_owned()),
        ..TrackQuery::default()
    };
    let albums = AlbumQuery {
        artist_id: Some("a1".to_owned()),
        ..AlbumQuery::default()
    };
    let totals = || async {
        let (_, tracks) = fixture
            .catalog
            .tracks(&tracks, TrackOrder::Disc, 10, 0)
            .await
            .expect("track page");
        let (_, albums) = fixture
            .catalog
            .albums(&albums, AlbumOrder::Title, 10, 0)
            .await
            .expect("album page");
        (tracks, albums)
    };
    assert_eq!(totals().await, (1, 2));
    write_batch(
        &fixture,
        "change tracks without the revision",
        "UPDATE local_tracks SET availability = 'indexed' WHERE id = 't5'; \
         UPDATE local_tracks SET availability = 'missing' WHERE id IN ('t1', 't2');"
            .to_owned(),
    )
    .await;
    assert_eq!(totals().await, (1, 2), "counted again at one revision");
    write_batch(
        &fixture,
        "move the revision",
        "INSERT INTO library_catalog_revision (singleton, value) VALUES (1, 1) \
         ON CONFLICT (singleton) DO UPDATE SET value = value + 1;"
            .to_owned(),
    )
    .await;
    assert_eq!(totals().await, (2, 1), "not counted at the next revision");
}

/// The artist list's id order for each sort and direction over every
/// credited artist of the small fixture.
const ARTIST_ORDERS: [(ArtistSort, bool, [&str; 3]); 8] = [
    (ArtistSort::Name, false, ["a1", "a2", "a3"]),
    (ArtistSort::Name, true, ["a3", "a2", "a1"]),
    (ArtistSort::AlbumCount, false, ["a3", "a1", "a2"]),
    (ArtistSort::AlbumCount, true, ["a1", "a2", "a3"]),
    (ArtistSort::DateAdded, false, ["a1", "a2", "a3"]),
    (ArtistSort::DateAdded, true, ["a3", "a2", "a1"]),
    (ArtistSort::AppearanceCount, false, ["a2", "a1", "a3"]),
    (ArtistSort::AppearanceCount, true, ["a1", "a3", "a2"]),
];

/// The small fixture's credited artists with their counts: a double credit
/// counts once, and Empty Halls has no files and is not counted.
const ARTIST_RECORDS: [&str; 3] = [
    "ArtistRecord { id: \"a1\", name: \"Aurora\", artist_mbid: Some(\"m1\"), linked: true, \
     album_count: 2, track_count: 4, appearance_album_count: 1, appearance_track_count: 1, \
     date_added: Some(1000.0) }",
    "ArtistRecord { id: \"a2\", name: \"Boreal\", artist_mbid: None, linked: false, \
     album_count: 2, track_count: 3, appearance_album_count: 0, appearance_track_count: 0, \
     date_added: Some(1500.0) }",
    "ArtistRecord { id: \"a3\", name: \"Gæst \\\"Quoted\\\" 100%\", artist_mbid: None, \
     linked: false, album_count: 0, track_count: 1, appearance_album_count: 1, \
     appearance_track_count: 1, date_added: Some(1600.0) }",
];

fn artist_ids(artists: &[ArtistRecord]) -> Vec<&str> {
    artists.iter().map(|artist| artist.id.as_str()).collect()
}

/// Every artist the list gives in one scope, walked `limit` at a time by
/// each page's key, with each page's totals.
async fn walk_artists(
    fixture: &Fixture,
    scope: ArtistScope,
    q: Option<&str>,
    sort: ArtistSort,
    descending: bool,
    limit: u64,
) -> (Vec<String>, Vec<(u64, u64, u64)>) {
    let mut after = After::default();
    let (mut ids, mut totals) = (Vec::new(), Vec::new());
    loop {
        let (page, led, contributors) = fixture
            .catalog
            .artist_list(scope, q, sort, descending, limit, &after)
            .await
            .expect("lists");
        ids.extend(page.items.iter().map(|artist| artist.id.clone()));
        totals.push((page.total, led, contributors));
        match page.next {
            Some(key) => after.key = Some(key),
            None => return (ids, totals),
        }
    }
}

/// The artist list answers the fixture's pages: every scope and sort in
/// both directions, a name filter, walked by cursor one page and two at a
/// time, with the scope totals; text filters with LIKE metacharacters and
/// quotes; single gets of live, retired and unknown ids. The retired artist
/// and the uncredited placeholders are never listed. The player apps' page
/// at an offset reads the same order.
#[tokio::test]
async fn artist_pages_match_the_fixture() {
    let fixture = seed_small().await;
    let scopes: [(ArtistScope, &[&str]); 4] = [
        (ArtistScope::All, &["a1", "a2", "a3"]),
        (ArtistScope::AlbumArtists, &["a1", "a2"]),
        (ArtistScope::Contributors, &["a3"]),
        (ArtistScope::WithMusic, &["a1", "a2", "a3"]),
    ];
    for (scope, members) in scopes {
        for (sort, descending, order) in ARTIST_ORDERS {
            let what = format!("scope={scope:?} sort={sort:?} desc={descending}");
            let listed: Vec<&str> = order
                .into_iter()
                .filter(|id| members.contains(id))
                .collect();
            for (q, limit) in [(None, 50), (None, 2), (Some("a"), 50)] {
                let named: Vec<String> = listed
                    .iter()
                    .filter(|id| q.is_none() || ["a1", "a2"].contains(id))
                    .map(|id| (*id).to_owned())
                    .collect();
                let totals = match q {
                    None => (named.len() as u64, 2, 1),
                    Some(_) => (named.len() as u64, 2, 0),
                };
                let pages = named.len().div_ceil(limit as usize).max(1);
                assert_eq!(
                    walk_artists(&fixture, scope, q, sort, descending, limit).await,
                    (named.clone(), vec![totals; pages]),
                    "{what} q={q:?} limit={limit}"
                );
                let (artists, total, led, contributors) = fixture
                    .catalog
                    .list_artists(scope, q, sort, descending, limit, 1)
                    .await
                    .expect("lists at an offset");
                assert_eq!(
                    (artist_ids(&artists), (total, led, contributors)),
                    (
                        named
                            .iter()
                            .skip(1)
                            .take(limit as usize)
                            .map(String::as_str)
                            .collect(),
                        totals
                    ),
                    "{what} q={q:?} limit={limit} offset=1"
                );
            }
        }
    }
    for (q, want) in [
        ("zzz-no-such-artist", (vec![], (0, 0, 0))),
        ("gæst", (vec!["a3"], (1, 0, 1))),
        ("100%", (vec!["a3"], (1, 0, 1))),
        ("\"quoted\"", (vec!["a3"], (1, 0, 1))),
    ] {
        let (page, led, contributors) = fixture
            .catalog
            .artist_list(
                ArtistScope::All,
                Some(q),
                ArtistSort::Name,
                false,
                50,
                &After::default(),
            )
            .await
            .expect("lists");
        assert_eq!(
            (artist_ids(&page.items), (page.total, led, contributors)),
            want,
            "q={q:?}"
        );
    }
    let (page, ..) = fixture
        .catalog
        .artist_list(
            ArtistScope::All,
            None,
            ArtistSort::Name,
            false,
            50,
            &After::default(),
        )
        .await
        .expect("lists");
    let records: Vec<String> = page
        .items
        .iter()
        .map(|artist| format!("{artist:?}"))
        .collect();
    assert_eq!(records, ARTIST_RECORDS);
    for (id, want) in [
        ("a1", Some(ARTIST_RECORDS[0])),
        ("a2", Some(ARTIST_RECORDS[1])),
        ("a3", Some(ARTIST_RECORDS[2])),
        ("ax", None),
        ("nope", None),
    ] {
        let got = fixture.catalog.get_artist(id).await.expect("reads");
        assert_eq!(
            got.map(|artist| format!("{artist:?}")).as_deref(),
            want,
            "id={id}"
        );
    }
}

/// The small fixture's indexed tracks as the default title page reads
/// them.
const TRACK_RECORDS: [&str; 7] = [
    "TrackRecord { id: \"t2\", title: \"Closer\", album_id: \"al1\", album_title: \"First Light\", \
     artist_name: \"Aurora\", artist_id: Some(\"a1\"), album_artist_name: \"Aurora\", \
     album_artist_id: Some(\"a1\"), recording_mbid: None, release_group_mbid: Some(\"rg1\"), \
     artist_mbid: Some(\"m1\"), album_artist_mbid: Some(\"m1\"), disc_number: 1, track_number: 2, \
     year: Some(1994), genre: None, duration_seconds: Some(200.0), format: \"flac\", \
     bit_rate: None, sample_rate: None, bit_depth: None, channels: None, \
     file_size_bytes: 4000000, date_added: Some(1100.0), cover_available: true }",
    "TrackRecord { id: \"t1\", title: \"Opener\", album_id: \"al1\", album_title: \"First Light\", \
     artist_name: \"Aurora\", artist_id: Some(\"a1\"), album_artist_name: \"Aurora\", \
     album_artist_id: Some(\"a1\"), recording_mbid: None, release_group_mbid: Some(\"rg1\"), \
     artist_mbid: Some(\"m1\"), album_artist_mbid: Some(\"m1\"), disc_number: 1, track_number: 1, \
     year: Some(1994), genre: Some(\"Rock\"), duration_seconds: Some(200.0), format: \"flac\", \
     bit_rate: None, sample_rate: None, bit_depth: None, channels: None, \
     file_size_bytes: 4000000, date_added: Some(1000.0), cover_available: true }",
    "TrackRecord { id: \"t9\", title: \"Ox \\\"Q\\\" %\", album_id: \"al5\", \
     album_title: \"100% \\\"Live\\\" _Unplugged_ 'Encore'\", artist_name: \"\", \
     artist_id: Some(\"a2\"), album_artist_name: \"Boreal\", album_artist_id: Some(\"a2\"), \
     recording_mbid: None, release_group_mbid: None, artist_mbid: None, \
     album_artist_mbid: None, disc_number: 1, track_number: 2, year: Some(1987), genre: None, \
     duration_seconds: Some(195.0), format: \"opus\", bit_rate: None, sample_rate: None, \
     bit_depth: None, channels: None, file_size_bytes: 2000000, date_added: Some(1900.0), \
     cover_available: false }",
    "TrackRecord { id: \"t6\", title: \"Peak\", album_id: \"al3\", album_title: \"Lone Peak\", \
     artist_name: \"Boreal\", artist_id: Some(\"a2\"), album_artist_name: \"Boreal\", \
     album_artist_id: Some(\"a2\"), recording_mbid: None, release_group_mbid: None, \
     artist_mbid: None, album_artist_mbid: None, disc_number: 1, track_number: 1, \
     year: Some(1994), genre: None, duration_seconds: Some(220.0), format: \"flac\", \
     bit_rate: None, sample_rate: None, bit_depth: None, channels: None, \
     file_size_bytes: 5000000, date_added: Some(1500.0), cover_available: false }",
    "TrackRecord { id: \"t4\", title: \"Single\", album_id: \"al2\", album_title: \"Second Dawn\", \
     artist_name: \"Aurora\", artist_id: Some(\"a1\"), album_artist_name: \"Aurora\", \
     album_artist_id: Some(\"a1\"), recording_mbid: None, release_group_mbid: None, \
     artist_mbid: Some(\"m1\"), album_artist_mbid: Some(\"m1\"), disc_number: 1, track_number: 1, \
     year: Some(2001), genre: None, duration_seconds: Some(180.0), format: \"mp3\", \
     bit_rate: None, sample_rate: None, bit_depth: None, channels: None, \
     file_size_bytes: 3000000, date_added: Some(2000.0), cover_available: false }",
    "TrackRecord { id: \"t8\", title: \"Up\", album_id: \"al5\", \
     album_title: \"100% \\\"Live\\\" _Unplugged_ 'Encore'\", artist_name: \"Boreal\", \
     artist_id: Some(\"a2\"), album_artist_name: \"Boreal\", album_artist_id: Some(\"a2\"), \
     recording_mbid: None, release_group_mbid: None, artist_mbid: None, \
     album_artist_mbid: None, disc_number: 1, track_number: 1, year: Some(1987), genre: None, \
     duration_seconds: Some(190.0), format: \"opus\", bit_rate: None, sample_rate: None, \
     bit_depth: None, channels: None, file_size_bytes: 2000000, date_added: Some(1800.0), \
     cover_available: false }",
    "TrackRecord { id: \"t7\", title: \"Æther \\\"Mix\\\" 100%_Sure\", album_id: \"al3\", \
     album_title: \"Lone Peak\", artist_name: \"Aurora\", artist_id: Some(\"a1\"), \
     album_artist_name: \"Boreal\", album_artist_id: Some(\"a2\"), recording_mbid: None, \
     release_group_mbid: None, artist_mbid: Some(\"m1\"), album_artist_mbid: None, \
     disc_number: 1, track_number: 2, year: Some(1994), genre: Some(\"Jazz\"), \
     duration_seconds: Some(210.0), format: \"ogg\", bit_rate: None, sample_rate: None, \
     bit_depth: None, channels: None, file_size_bytes: 1000000, date_added: Some(1600.0), \
     cover_available: false }",
];

fn track_ids(tracks: &[TrackRecord]) -> Vec<&str> {
    tracks.iter().map(|track| track.id.as_str()).collect()
}

/// The track list's first page, unfiltered but for `filter`.
async fn first_tracks(
    fixture: &Fixture,
    filter: &TrackFilter,
    sort: TrackSort,
    descending: bool,
    limit: u64,
) -> (Vec<TrackRecord>, u64) {
    let page = fixture
        .catalog
        .list_tracks(filter, sort, descending, limit, &After::default())
        .await
        .expect("lists");
    (page.items, page.total)
}

/// Every indexed track the list gives, walked `limit` at a time by each
/// page's key.
async fn walk_tracks(
    fixture: &Fixture,
    sort: TrackSort,
    descending: bool,
    limit: u64,
    seed: u32,
) -> Vec<String> {
    let mut after = After { seed, key: None };
    let mut ids = Vec::new();
    loop {
        let page = fixture
            .catalog
            .list_tracks(&TrackFilter::default(), sort, descending, limit, &after)
            .await
            .expect("lists");
        assert_eq!(page.total, 7);
        ids.extend(page.items.iter().map(|track| track.id.clone()));
        match page.next {
            Some(key) => after.key = Some(key),
            None => return ids,
        }
    }
}

/// The track list's order for each sort over every indexed track of the
/// small fixture. By artist, the track with no artist comes first and
/// tracks keep disc and track order within their album either way.
const TRACK_ORDERS: [(TrackSort, bool, [&str; 7]); 8] = [
    (
        TrackSort::Title,
        false,
        ["t2", "t1", "t9", "t6", "t4", "t8", "t7"],
    ),
    (
        TrackSort::Title,
        true,
        ["t7", "t8", "t4", "t6", "t9", "t1", "t2"],
    ),
    (
        TrackSort::DateAdded,
        false,
        ["t1", "t2", "t6", "t7", "t8", "t9", "t4"],
    ),
    (
        TrackSort::DateAdded,
        true,
        ["t4", "t9", "t8", "t7", "t6", "t2", "t1"],
    ),
    (
        TrackSort::Artist,
        false,
        ["t9", "t1", "t2", "t7", "t4", "t8", "t6"],
    ),
    (
        TrackSort::Artist,
        true,
        ["t6", "t8", "t4", "t7", "t1", "t2", "t9"],
    ),
    (
        TrackSort::Album,
        false,
        ["t8", "t9", "t1", "t2", "t6", "t7", "t4"],
    ),
    (
        TrackSort::Album,
        true,
        ["t4", "t6", "t7", "t1", "t2", "t8", "t9"],
    ),
];

/// A player search's short words each have to be found in the title, the
/// artist or the album: a track with no artist name matches on the other
/// two only, however many short words the search holds.
#[tokio::test]
async fn player_short_words_need_a_column_that_holds_them() {
    use library::player::{TrackOrder, TrackQuery};

    let fixture = seed_small().await;
    write_batch(
        &fixture,
        "track with no artist name",
        format!(
            "{TRACK_INSERT}('nt1', 'al1', 'r1', '/m/nt1.flac', 'nt1.flac', 'h-nt1', 1000, 1, \
             's-nt1', 'Love Song', 'love song', NULL, NULL, 'Greatest', 'greatest', 'Aurora', \
             'aurora', 1, 9, 2001, 100.0, 'flac', 'indexed', 'scan', 3000.0, 'automatic');"
        ),
    )
    .await;
    for (q, found) in [
        ("love so", true),
        ("love so on g", true),
        ("love me", false),
        ("love me do it", false),
    ] {
        let query = TrackQuery {
            q: Some(q.to_owned()),
            ..TrackQuery::default()
        };
        let page = fixture
            .catalog
            .track_page(&query, TrackOrder::Title, 50, 0)
            .await
            .expect("page reads");
        assert_eq!(
            page.iter().any(|track| track.id == "nt1"),
            found,
            "{q}: {page:?}"
        );
    }
}

/// Track search answers the fixture's pages on every query shape: misses,
/// hits, 1-2 character queries (the substring path), LIKE metacharacters,
/// quotes, unicode, case folding and NUL bytes, in title order and newest
/// first; a text miss stays empty under narrowing filters; the album,
/// artist and genre filters keep their tracks.
#[tokio::test]
async fn track_search_matches_the_fixture() {
    let fixture = seed_small().await;
    assert_eq!(fts_match_phrase("pe"), None);
    assert_eq!(fts_match_phrase("perf"), Some("\"perf\"".to_owned()));
    assert_eq!(fts_match_phrase("a\"b"), Some("\"a\"\"b\"".to_owned()));
    assert_eq!(fts_match_phrase("ab\0cd"), None);
    assert_eq!(fts_match_phrase("æø"), None);
    let every = ["t2", "t1", "t9", "t6", "t4", "t8", "t7"];
    let every_newest = ["t4", "t9", "t8", "t7", "t6", "t2", "t1"];
    let pages: [(&str, &[&str], &[&str]); 25] = [
        ("zzz-no-such-thing-zzz", &[], &[]),
        ("opener", &["t1"], &["t1"]),
        (
            "aurora",
            &["t2", "t1", "t4", "t7"],
            &["t4", "t7", "t2", "t1"],
        ),
        ("lone peak", &["t6", "t7"], &["t7", "t6"]),
        ("æther", &["t7"], &["t7"]),
        ("ÆTHER", &["t7"], &["t7"]),
        ("\"mix\"", &["t7"], &["t7"]),
        ("100%", &["t9", "t8", "t7"], &["t9", "t8", "t7"]),
        ("_sure", &["t7"], &["t7"]),
        ("%", &["t9", "t8", "t7"], &["t9", "t8", "t7"]),
        ("_", &["t9", "t8", "t7"], &["t9", "t8", "t7"]),
        ("\\", &[], &[]),
        ("a\"b", &[], &[]),
        ("up", &["t8"], &["t8"]),
        ("o", &every, &every_newest),
        ("e", &every, &every_newest),
        ("al", &["t6", "t8"], &["t8", "t6"]),
        ("zz", &[], &[]),
        ("UP", &["t8"], &["t8"]),
        ("Ox \"Q\"", &["t9"], &["t9"]),
        ("Gæst", &[], &[]),
        ("  spaced  ", &[], &[]),
        ("ab\0cd", &[], &[]),
        ("peak tunggal", &[], &[]),
        ("NUL", &[], &[]),
    ];
    for (q, by_title, newest) in pages {
        for (sort, descending, want) in [
            (TrackSort::Title, false, by_title),
            (TrackSort::DateAdded, true, newest),
        ] {
            let filter = TrackFilter {
                q: Some(q.to_owned()),
                ..Default::default()
            };
            let (tracks, total) = first_tracks(&fixture, &filter, sort, descending, 20).await;
            assert_eq!(
                (track_ids(&tracks), total),
                (want.to_vec(), want.len() as u64),
                "q={q:?} sort={sort:?} desc={descending}"
            );
        }
    }
    let narrowed = |album: Option<&str>, artist: Option<&str>, genre: Option<&str>| TrackFilter {
        q: None,
        album_id: album.map(str::to_owned),
        artist_id: artist.map(str::to_owned),
        genre: genre.map(str::to_owned),
    };
    for filter in [
        narrowed(Some("al1"), None, None),
        narrowed(None, Some("a1"), None),
        narrowed(None, None, Some("rock")),
    ] {
        let filter = TrackFilter {
            q: Some("zzz-no-such-thing-zzz".to_owned()),
            ..filter
        };
        let (tracks, total) = first_tracks(&fixture, &filter, TrackSort::Title, false, 20).await;
        assert!(tracks.is_empty() && total == 0, "filter={filter:?}");
    }
    for (filter, want) in [
        (narrowed(Some("al1"), None, None), vec!["t2", "t1"]),
        (
            narrowed(None, Some("a1"), None),
            vec!["t2", "t1", "t4", "t7"],
        ),
        (narrowed(None, None, Some("jazz")), vec!["t7"]),
        (
            TrackFilter {
                q: Some("a".to_owned()),
                ..narrowed(None, None, Some("rock"))
            },
            vec!["t1"],
        ),
    ] {
        let (tracks, total) = first_tracks(&fixture, &filter, TrackSort::Title, false, 50).await;
        assert_eq!(
            (track_ids(&tracks), total),
            (want.clone(), want.len() as u64),
            "filter={filter:?}"
        );
    }
    let (tracks, _) = first_tracks(
        &fixture,
        &TrackFilter::default(),
        TrackSort::Title,
        false,
        50,
    )
    .await;
    let records: Vec<String> = tracks.iter().map(|track| format!("{track:?}")).collect();
    assert_eq!(records, TRACK_RECORDS);
    for (sort, descending, order) in TRACK_ORDERS {
        assert_eq!(
            walk_tracks(&fixture, sort, descending, 3, 0).await,
            order,
            "sort={sort:?} desc={descending}"
        );
    }
    let mut shuffled = walk_tracks(&fixture, TrackSort::Random, false, 3, 42).await;
    shuffled.sort_unstable();
    assert_eq!(shuffled, ["t1", "t2", "t4", "t6", "t7", "t8", "t9"]);
}

/// Maintained format totals track every mutation: inserts in each
/// availability, transitions between them, format and size changes,
/// unrelated updates, deletes, and format extinction. The stats' track
/// totals and the unfiltered track total match live aggregation after
/// each step.
#[tokio::test]
async fn stats_maintained_totals_track_mutations() {
    let fixture = seed_small().await;
    async fn check(fixture: &Fixture, step: &str) {
        let stats = fixture.catalog.stats().await.expect("stats read");
        let ground = ground_totals(fixture.db()).await;
        assert_eq!(
            (
                stats.total_tracks,
                stats.total_size_bytes,
                stats.format_breakdown
            ),
            ground,
            "stats at {step}"
        );
        let (_, total) = first_tracks(
            fixture,
            &TrackFilter::default(),
            TrackSort::Title,
            false,
            50,
        )
        .await;
        assert_eq!(total, ground.0, "unfiltered total at {step}");
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

/// The store's hot statements stay index-driven: the artist credit
/// aggregation drives from the credit index over covering probes, the
/// title-order track page walks its index with no sort, stats and the track
/// miss oracle never scan the track table, the album id page never
/// table-scans, the album rows probe each album's tracks by index, the
/// artist page never scans the catalog, and the batched album resolution
/// reaches every album by key. Plan shapes are scale-free, so the small
/// fixture pins them.
#[tokio::test]
async fn hot_plans_avoid_scans_and_sorts() {
    let fixture = seed_small().await;
    // Without planner statistics (a fresh database). The 100k acceptance
    // test below re-checks the same plans with statistics: on this tiny
    // fixture ANALYZE rightly prefers scanning a few rows.
    assert_hot_plans(fixture.db()).await;
}

/// Song search reads candidates through the FTS index, or for a query of
/// short words through the title and artist-order indexes, and never
/// scans the track table.
#[tokio::test]
async fn song_search_plans_avoid_track_scans() {
    use crate::reads::search::service::song_query;

    let fixture = seed_small().await;
    for (query, wants) in [
        ("track 0123", "local_tracks_fts VIRTUAL TABLE"),
        ("pe", "idx_local_tracks_availability_title"),
        ("pe", "idx_local_tracks_artist_order"),
    ] {
        let (statement, _) = song_query(query, 24, 0);
        let plan = explain_unbound(fixture.db(), &statement).await;
        assert!(
            plan.iter().any(|line| line.contains(wants)),
            "{query}: no {wants} in {plan:?}"
        );
        assert!(
            !plan.iter().any(|line| {
                let mut words = line.split_whitespace();
                let verb = words.next();
                let tracks = matches!(words.next(), Some("t" | "local_tracks"));
                // Walking every indexed track by availability alone is a scan.
                tracks && (verb == Some("SCAN") || line.ends_with("(availability=?)"))
            }),
            "{query}: scans the track table: {plan:?}"
        );
    }
}

async fn assert_hot_plans(db: &Db) {
    let plan = explain(
        db,
        &sql::artist_credit_counts(ListLen::new(2)),
        &[ExplainBind::Text(Some("a1")), ExplainBind::Text(Some("a2"))],
    )
    .await;
    assert!(
        plan.iter()
            .any(|line| line.contains("SEARCH p USING PRIMARY KEY (local_artist_id=?)")),
        "credit counts read the kept pairs by artist: {plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line.contains("SCAN ")),
        "no scan in the credit counts: {plan:?}"
    );

    // The kept artist counts work one artist out from that artist's own
    // album pairs and led albums, never a walk of the catalog.
    let plan = explain(
        db,
        "SELECT * FROM library_artist_counts_rule WHERE local_artist_id = ?",
        &[ExplainBind::Text(Some("a1"))],
    )
    .await;
    assert!(
        plan.iter().any(|line| line
            .contains("USING COVERING INDEX idx_local_albums_owned_leader (album_artist_id=?"))
            && !plan.iter().any(|line| line.contains("SCAN ")),
        "the artist rule reads one artist by key: {plan:?}"
    );

    // Long lists page by key, never by offset, so a deep page costs what
    // the first does.
    let mut lists = Vec::new();
    for after in [false, true] {
        lists.push(sql::led_album_page(after));
        lists.push(sql::appearance_page(after));
        for descending in [false, true] {
            for sort in [
                AlbumSort::Name,
                AlbumSort::DateAdded,
                AlbumSort::Year,
                AlbumSort::Artist,
                AlbumSort::Random,
                AlbumSort::Rediscover,
            ] {
                lists.push(sql::album_page(sort, descending, after));
            }
            for sort in TRACK_SORTS {
                lists.push(sql::track_page_like(sort, descending, after));
                lists.push(sql::track_page_words(
                    ListLen::new(1),
                    sort,
                    descending,
                    after,
                ));
            }
            for scope in [
                ArtistScope::All,
                ArtistScope::AlbumArtists,
                ArtistScope::Contributors,
                ArtistScope::WithMusic,
            ] {
                for sort in ARTIST_SORTS {
                    lists.push(sql::artist_page(scope, sort, descending, after));
                }
            }
        }
    }
    for text in &lists {
        assert!(!text.contains("OFFSET"), "a list pages by offset: {text}");
    }

    // The first page and a deep page of each order an index holds walk that
    // index and sort nothing.
    for (name, text, index) in [
        (
            "tracks by title",
            sql::track_page_like(TrackSort::Title, false, false),
            "idx_local_tracks_availability_title",
        ),
        (
            "tracks by title, deep",
            sql::track_page_like(TrackSort::Title, false, true),
            "idx_local_tracks_availability_title",
        ),
        (
            "newest tracks, deep",
            sql::track_page_like(TrackSort::DateAdded, true, true),
            "idx_local_tracks_recent",
        ),
        (
            "tracks by artist",
            sql::track_page_like(TrackSort::Artist, false, false),
            "idx_local_tracks_artist_order",
        ),
        (
            "tracks by artist, deep",
            sql::track_page_like(TrackSort::Artist, false, true),
            "idx_local_tracks_artist_order",
        ),
        (
            "tracks by album, deep",
            sql::track_page_like(TrackSort::Album, false, true),
            "idx_local_tracks_album_order_folded",
        ),
        (
            "newest albums, deep",
            sql::album_page(AlbumSort::DateAdded, true, true),
            "idx_local_albums_created",
        ),
        (
            "recently added",
            sql::RECENT_ALBUM_IDS.text().to_owned(),
            "idx_local_albums_created",
        ),
    ] {
        let plan = explain_outer(db, &text).await;
        // A deep page seeks its place in the index; a first page walks it
        // from the top.
        let seeks = |line: &String| {
            !name.ends_with("deep")
                || [")>(", ")<(", ">?", "<?"]
                    .iter()
                    .any(|range| line.contains(range))
        };
        assert!(
            plan.iter().any(|line| line.contains(index) && seeks(line)),
            "{name} walks {index}: {plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|line| line.contains("TEMP B-TREE FOR ORDER BY")),
            "{name} sorts nothing: {plan:?}"
        );
    }

    // A deep album page by name starts in the title index, not at the top.
    let plan = explain_unbound(db, &sql::album_page(AlbumSort::Name, false, true)).await;
    assert!(
        plan.iter()
            .any(|line| line.contains("idx_local_albums_search (title_folded>?)")),
        "a deep album page seeks its title: {plan:?}"
    );

    // The count sorts walk the kept counts in order, one index per order:
    // a seek on the count where the scope bounds it, a seek on the cursor
    // after one, and nothing sorted or counted per artist.
    for scope in [
        ArtistScope::All,
        ArtistScope::AlbumArtists,
        ArtistScope::Contributors,
    ] {
        for (sort, index) in [
            (ArtistSort::AlbumCount, "idx_artist_counts_albums"),
            (ArtistSort::AppearanceCount, "idx_artist_counts_appearances"),
        ] {
            for descending in [false, true] {
                for after in [false, true] {
                    let index = if descending {
                        format!("{index}_desc")
                    } else {
                        index.to_owned()
                    };
                    let bounded = matches!(sort, ArtistSort::AlbumCount)
                        && !matches!(scope, ArtistScope::All);
                    let plan =
                        explain_unbound(db, &sql::artist_page(scope, sort, descending, after))
                            .await;
                    let walk = if after || bounded {
                        "SEARCH s"
                    } else {
                        "SCAN s"
                    };
                    assert!(
                        plan.len() == 1
                            && plan[0].starts_with(walk)
                            && plan[0].split_whitespace().any(|word| word == index)
                            && (!after || plan[0].contains(",folded_name,local_artist_id)>(")),
                        "{scope:?} by {sort:?}, descending {descending}, after {after}: {plan:?}"
                    );
                }
            }
        }
    }

    // An artist's appearances start from the artist's credits.
    for after in [false, true] {
        let plan = explain_unbound(db, &sql::appearance_page(after)).await;
        assert!(
            !plan
                .iter()
                .any(|line| line.contains("SCAN a ") || line.contains("SCAN local_albums")),
            "appearances never walk every album: {plan:?}"
        );
    }

    for statement in [sql::STATS, sql::STATS_FORMATS, sql::TRACK_TOTAL] {
        let plan = explain(db, statement.text(), &[]).await;
        assert!(
            !plan.iter().any(|line| line.contains("SCAN local_tracks")),
            "{} never scans tracks: {plan:?}",
            statement.name()
        );
    }
    let plan = explain(
        db,
        sql::TRACK_MISS.text(),
        &[ExplainBind::Text(Some("\"zzz-no-such-thing\""))],
    )
    .await;
    assert!(
        plan.iter()
            .any(|line| line.contains("SCAN local_tracks_fts VIRTUAL TABLE INDEX")),
        "oracle probes the FTS index: {plan:?}"
    );

    let plan = explain_unbound(db, &sql::album_page(AlbumSort::Name, false, false)).await;
    assert!(
        plan.iter()
            .all(|line| !line.contains("SCAN ") || line.contains("USING")),
        "album id page never table-scans: {plan:?}"
    );

    // The page aggregate probes each album's tracks for the modal format;
    // the probe must stay on the album index, never an availability scan.
    let plan = explain(
        db,
        &sql::album_rows(ListLen::new(2)),
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

    let plan = explain_unbound(
        db,
        &sql::artist_page(ArtistScope::All, ArtistSort::Name, false, false),
    )
    .await;
    assert!(
        !plan
            .iter()
            .any(|line| line.contains("SCAN local_albums") || line.contains("SCAN local_tracks")),
        "artist page never scans the catalog: {plan:?}"
    );

    // The credited scope reaches tracks from the artist's credits, one
    // track by id each, never by walking the indexed tracks per artist.
    for (name, text, binds) in [
        (
            "artist page",
            sql::artist_page(ArtistScope::All, ArtistSort::Name, false, false),
            vec![
                ExplainBind::Text(None),
                ExplainBind::Text(None),
                ExplainBind::Int(50),
            ],
        ),
        (
            "artist totals",
            sql::ARTIST_TOTALS.text().to_owned(),
            vec![ExplainBind::Text(None), ExplainBind::Text(None)],
        ),
        ("stats", sql::STATS.text().to_owned(), Vec::new()),
    ] {
        let plan = explain(db, &text, &binds).await;
        let track_probes: Vec<&String> = plan
            .iter()
            .filter(|line| line.starts_with("SEARCH lct "))
            .collect();
        assert!(
            !track_probes.is_empty() && track_probes.iter().all(|line| line.contains("(id=?)")),
            "{name} reaches credited tracks by id: {plan:?}"
        );
    }

    // Track resolution names every asked album in one statement, each by
    // key: the album, its alias, the album it was retired into.
    let asked: Vec<String> = (0..200).map(|n| format!("al{n}")).collect();
    let binds: Vec<ExplainBind> = asked.iter().map(|id| ExplainBind::Text(Some(id))).collect();
    let plan = explain(db, &sql::current_albums(ListLen::new(asked.len())), &binds).await;
    assert!(
        !plan.iter().any(|line| line.contains("SCAN local_albums")
            || line.contains("SCAN local_album_aliases")
            || line.contains("SCAN a ")
            || line.contains("SCAN b ")
            || line.contains("SCAN x ")),
        "album resolution never scans: {plan:?}"
    );

    // Player apps read every artist's tag ids in chunks of 500: each chunk
    // probes those artists' credits, never a walk over the tracks.
    let ids: Vec<String> = (0..500).map(|n| format!("ar{n}")).collect();
    let tag_ids = sql::artist_tag_ids(ListLen::new(ids.len()));
    let binds: Vec<ExplainBind> = ids
        .iter()
        .chain(&ids)
        .map(|id| ExplainBind::Text(Some(id)))
        .collect();
    let plan = explain(db, &tag_ids, &binds).await;
    assert!(
        !plan.iter().any(|line| line.contains("SCAN ")),
        "artist tag ids never scan: {plan:?}"
    );
    assert!(
        plan.iter()
            .filter(|line| line.starts_with("SEARCH t ") || line.starts_with("SEARCH a "))
            .all(|line| line.contains("(id=?")),
        "artist tag ids reach tracks and albums by id from the credits: {plan:?}"
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

/// Played marks on the small fixture, with plays of their own in the forms
/// v2 wrote. `u1` marked t1 unplayed in the second of its middle play (so
/// only the last counts), t6 unplayed after all its plays, t7 played with
/// no play, and t2 played after an earlier unplayed mark. `u3` has plays
/// with a space for the `T`, fractional seconds and `Z`, and an offset:
/// t8 marked unplayed just before its play, and t9 in the second of its
/// later play, which with the earlier one no longer counts.
async fn seed_small_marks(fixture: &Fixture) {
    let ms = |at: &str| format!("CAST(strftime('%s', '{at}') AS INTEGER) * 1000");
    let marks = [
        ("u1", "t1", None, Some(ms("2026-01-02T00:00:00Z"))),
        ("u1", "t6", None, Some(ms("2026-01-10T00:00:00Z"))),
        ("u1", "t7", Some(ms("2026-01-07T00:00:00Z")), None),
        (
            "u1",
            "t2",
            Some(ms("2026-01-08T00:00:00Z")),
            Some(ms("2026-01-01T00:00:00Z")),
        ),
        ("u3", "t8", None, Some(ms("2026-01-05T11:59:59Z"))),
        (
            "u3",
            "t9",
            None,
            Some(format!("{} + 900", ms("2026-01-06T10:00:00Z"))),
        ),
    ];
    let mut sql = String::from(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
         ('u1', 'One', 'user', 'now'), ('u3', 'Three', 'user', 'now'); \
         INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES \
         ('v2-1', 'u3', 't8', 'al5', 'x', 'x', '2026-01-05 12:00:00'), \
         ('v2-2', 'u3', 't9', 'al5', 'x', 'x', '2026-01-06T10:00:00.500Z'), \
         ('v2-3', 'u3', 't9', 'al5', 'x', 'x', '2026-01-04T10:00:00Z'), \
         ('v2-4', 'u3', 't4', 'al2', 'x', 'x', '2026-01-03T09:00:00+01:00'); \
         INSERT INTO library_user_play_marks (user_id, local_track_id, marked_played_at, \
         marked_unplayed_at) VALUES ",
    );
    for (index, (user, track, played, unplayed)) in marks.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        let time = |at: &Option<String>| at.clone().unwrap_or_else(|| "NULL".to_owned());
        sql.push_str(&format!(
            "('{user}', '{track}', {}, {})",
            time(played),
            time(unplayed)
        ));
    }
    write_batch(fixture, "small marks", sql).await;
}

/// The values the rule gives each track and album for `user`, worked out
/// here one play at a time: `(play count, last play)` per id, the count 0
/// for a row with none.
async fn rule_values(
    db: &Db,
    user: &str,
) -> (
    HashMap<String, (u64, Option<i64>)>,
    HashMap<String, (u64, Option<i64>)>,
) {
    let tracks: Vec<(String, String)> = fetch(
        db,
        "SELECT id, local_album_id FROM local_tracks",
        Vec::new(),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .await;
    let plays: Vec<(Option<String>, Option<String>, Option<i64>)> = fetch(
        db,
        "SELECT local_track_id, local_album_id, CAST(strftime('%s', played_at) AS INTEGER) \
         FROM library_play_history WHERE user_id = ?",
        vec![text(user)],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .await;
    let marks: Vec<(String, Option<i64>, Option<i64>)> = fetch(
        db,
        "SELECT local_track_id, marked_played_at, marked_unplayed_at \
         FROM library_user_play_marks WHERE user_id = ?",
        vec![text(user)],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .await;
    let marks: HashMap<String, (Option<i64>, Option<i64>)> = marks
        .into_iter()
        .map(|(track, played, unplayed)| (track, (played, unplayed)))
        .collect();
    let mut by_track = HashMap::new();
    let mut by_album: HashMap<String, (u64, Option<i64>)> = HashMap::new();
    for (track, album) in &tracks {
        let (played_mark, unplayed_mark) = marks.get(track).copied().unwrap_or((None, None));
        let counted: Vec<Option<i64>> = plays
            .iter()
            .filter(|(played, _, at)| {
                played.as_deref() == Some(track.as_str())
                    && unplayed_mark.is_none_or(|mark| at.is_some_and(|at| at > mark / 1000))
            })
            .map(|(_, _, at)| *at)
            .collect();
        let (count, last) = if !counted.is_empty() {
            let latest = counted.iter().flatten().max().copied();
            (
                counted.len() as u64,
                latest.or(played_mark.map(|mark| mark / 1000)),
            )
        } else if let Some(mark) = played_mark {
            (1, Some(mark / 1000))
        } else {
            (0, None)
        };
        by_track.insert(track.clone(), (count, last));
        let entry = by_album.entry(album.clone()).or_default();
        entry.0 += count;
        entry.1 = entry.1.max(last);
    }
    for (track, album, at) in &plays {
        if let (None, Some(album)) = (track, album) {
            let entry = by_album.entry(album.clone()).or_default();
            entry.0 += 1;
            entry.1 = entry.1.max(*at);
        }
    }
    (by_track, by_album)
}

/// `ids` as one sort by `key` would order them, ties by id in the same
/// direction, the rows with a play count of 0 lowest.
fn one_sort(
    ids: Vec<String>,
    values: &HashMap<String, (u64, Option<i64>)>,
    key: library::player::OrderKey,
    descending: bool,
) -> Vec<String> {
    let mut keyed: Vec<(Option<i64>, String)> = ids
        .into_iter()
        .map(|id| {
            let (count, last) = values.get(&id).copied().unwrap_or((0, None));
            let value = match key {
                _ if count == 0 => None,
                library::player::OrderKey::PlayCount => Some(count as i64),
                _ => last,
            };
            (value, id)
        })
        .collect();
    keyed.sort();
    if descending {
        keyed.reverse();
    }
    keyed.into_iter().map(|(_, id)| id).collect()
}

/// A play-history order pages exactly as one sort of the whole catalog
/// would, with the rows of play count 0 lowest: no row repeated or skipped
/// across pages, ties in a fixed order, in both directions, for tracks and
/// albums, for a user with plays and marks, one whose plays v2 wrote, and
/// one with neither. The values are the rule's, worked out here one play
/// at a time: a track's plays count after its unplayed mark, in whole
/// seconds; a played mark with no play counts once. An album's are its
/// tracks', and those filed under it with no track. The played filters
/// agree with them, and Subsonic's play counts and album lists do not
/// read the marks.
#[tokio::test]
async fn history_orders_page_like_one_sort() {
    use library::player::{AlbumOrder, AlbumQuery, OrderKey, TrackOrder, TrackQuery};

    let fixture = seed_small().await;
    seed_small_plays(&fixture).await;
    seed_small_marks(&fixture).await;
    let player = fixture.catalog.clone();
    let streamable_tracks: Vec<String> = fetch(
        fixture.db(),
        "SELECT id FROM local_tracks WHERE availability = 'indexed' ORDER BY id",
        Vec::new(),
        |row| row.get(0),
    )
    .await;
    let browsable_albums: Vec<String> = fetch(
        fixture.db(),
        "SELECT r.id FROM local_albums r WHERE r.retired_into_album_id IS NULL AND EXISTS \
         (SELECT 1 FROM local_tracks t WHERE t.local_album_id = r.id \
          AND t.availability = 'indexed') ORDER BY r.id",
        Vec::new(),
        |row| row.get(0),
    )
    .await;
    for user in ["u1", "u3", "nobody"] {
        let (track_values, album_values) = rule_values(fixture.db(), user).await;
        for key in [OrderKey::LastPlayed, OrderKey::PlayCount] {
            for descending in [true, false] {
                let what = format!("{user} {key:?} descending {descending}");
                let expected = one_sort(streamable_tracks.clone(), &track_values, key, descending);
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

                let expected = one_sort(browsable_albums.clone(), &album_values, key, descending);
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

        // A page's own values are the rule's too.
        let values = player
            .played_values(user, "track", &streamable_tracks)
            .await
            .expect("track values");
        for id in &streamable_tracks {
            let (count, last) = track_values[id];
            let got = values.get(id).copied().unwrap_or_default();
            assert_eq!(
                (got.plays, got.last_played, got.played),
                (count, last.map(|at| at as f64), count > 0),
                "{user} {id}"
            );
        }
        let values = player
            .played_values(user, "album", &browsable_albums)
            .await
            .expect("album values");
        for id in &browsable_albums {
            let (count, last) = album_values[id];
            let got = values.get(id).copied().unwrap_or_default();
            assert_eq!(
                (got.plays, got.last_played),
                (count, last.map(|at| at as f64)),
                "{user} {id}"
            );
        }
    }

    // The values of one listener with marks.
    let values = player
        .played_values("u1", "track", &["t1", "t2", "t6", "t7"].map(str::to_owned))
        .await
        .expect("track values");
    // Midnight UTC on 2026-01-{day}.
    let at = |day: i64| Some((1_767_225_600 + (day - 1) * 86_400) as f64);
    let plays = |id: &str| values.get(id).map(|got| (got.plays, got.last_played));
    assert_eq!(
        plays("t1"),
        Some((1, at(3))),
        "the play in the mark's second is cut"
    );
    assert_eq!(
        plays("t2"),
        Some((1, at(5))),
        "the last counted play, not the mark"
    );
    assert_eq!(plays("t6"), None, "every play is before the mark");
    assert_eq!(plays("t7"), Some((1, at(7))), "a played mark with no play");

    // The played filters split the catalog in two, and their totals agree.
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
    assert_eq!(played, ["t1", "t2", "t4", "t7"]);
    assert_eq!(played_total, 4);
    let (unplayed, unplayed_total) = player
        .tracks(&filtered(false), TrackOrder::Title, 100, 0)
        .await
        .expect("unplayed tracks");
    let mut unplayed: Vec<String> = unplayed.into_iter().map(|track| track.id).collect();
    unplayed.sort();
    assert_eq!(unplayed, ["t6", "t8", "t9"]);
    assert_eq!(unplayed_total, 3);
    // An album is played when every streamable track on it is: al1's
    // missing track and al5's play with no track do not weigh in.
    let filtered = |played: bool| AlbumQuery {
        listener: Some("u1".to_owned()),
        played: Some(played),
        ..AlbumQuery::default()
    };
    for (played, expected) in [(true, ["al1", "al2"]), (false, ["al3", "al5"])] {
        let (albums, total) = player
            .albums(&filtered(played), AlbumOrder::Title, 100, 0)
            .await
            .expect("filtered albums");
        let mut albums: Vec<String> = albums.into_iter().map(|album| album.record.id).collect();
        albums.sort();
        assert_eq!(albums, expected, "played {played}");
        assert_eq!(total, 2, "played {played}");
    }
    let values = player
        .played_values("u1", "album", &browsable_albums)
        .await
        .expect("album values");
    let flags: Vec<(&str, bool)> = browsable_albums
        .iter()
        .map(|id| {
            (
                id.as_str(),
                values.get(id).is_some_and(|values| values.played),
            )
        })
        .collect();
    assert_eq!(
        flags,
        [("al1", true), ("al2", true), ("al3", false), ("al5", false)]
    );

    // Subsonic's counts read the plays alone: a track that moved to
    // another album took its plays along, and a play with no track still
    // counts for its album.
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

/// Per album, whether it is played for the listener whose track values
/// are `by_track`: it has an indexed track, and every indexed track on it
/// has a play count above 0.
async fn rule_played(
    db: &Db,
    by_track: &HashMap<String, (u64, Option<i64>)>,
) -> HashMap<String, bool> {
    let tracks: Vec<(String, String, bool)> = fetch(
        db,
        "SELECT id, local_album_id, availability = 'indexed' FROM local_tracks",
        Vec::new(),
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .await;
    let mut played: HashMap<String, bool> = HashMap::new();
    let mut indexed: HashMap<String, bool> = HashMap::new();
    for (track, album, streamable) in tracks {
        if !streamable {
            continue;
        }
        indexed.insert(album.clone(), true);
        let counted = by_track.get(&track).is_some_and(|(count, _)| *count > 0);
        let entry = played.entry(album).or_insert(true);
        *entry = *entry && counted;
    }
    played.retain(|album, _| indexed.contains_key(album));
    played
}

/// The album play summary holds, for every listener with a play or a mark,
/// exactly the albums whose play count by the rule is above 0, with the
/// rule's count, last play (milliseconds) and played flag, and no row for
/// anyone else; every album's `has_indexed_track` says whether it has an
/// indexed track.
async fn assert_album_summary(db: &Db, step: &str) {
    let users: Vec<String> = fetch(
        db,
        "SELECT user_id FROM library_play_history \
         UNION SELECT user_id FROM library_user_play_marks",
        Vec::new(),
        |row| row.get(0),
    )
    .await;
    for user in &users {
        let (by_track, by_album) = rule_values(db, user).await;
        let played = rule_played(db, &by_track).await;
        let mut expected: Vec<(String, i64, Option<i64>, bool)> = by_album
            .into_iter()
            .filter(|(_, (count, _))| *count > 0)
            .map(|(album, (count, last))| {
                let flag = played.get(&album).copied().unwrap_or(false);
                (album, count as i64, last.map(|at| at * 1000), flag)
            })
            .collect();
        expected.sort();
        let got: Vec<(String, i64, Option<i64>, bool)> = fetch(
            db,
            "SELECT local_album_id, plays, last_played_at, played \
             FROM library_user_album_plays WHERE user_id = ? ORDER BY local_album_id",
            vec![text(user)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .await;
        assert_eq!(got, expected, "{step}: {user}");
    }
    let others: i64 = fetch_one(
        db,
        "SELECT COUNT(*) FROM library_user_album_plays WHERE user_id NOT IN \
         (SELECT user_id FROM library_play_history \
          UNION SELECT user_id FROM library_user_play_marks)",
        Vec::new(),
        |row| row.get(0),
    )
    .await;
    assert_eq!(others, 0, "{step}: rows for no listener");
    let wrong: Vec<String> = fetch(
        db,
        "SELECT id FROM local_albums a WHERE has_indexed_track IS NOT EXISTS \
         (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
          AND t.availability = 'indexed')",
        Vec::new(),
        |row| row.get(0),
    )
    .await;
    assert!(
        wrong.is_empty(),
        "{step}: has_indexed_track wrong on {wrong:?}"
    );
}

/// A track row on the small fixture's root, for the summary steps.
fn track_values(id: &str, album: &str, availability: &str) -> String {
    format!(
        "('{id}', '{album}', 'r1', '/m/{id}.flac', '{id}.flac', 'h-{id}', 1000, 1, 's-{id}', \
         '{id}', '{id}', 'Aurora', 'aurora', 'x', 'x', 'Aurora', 'aurora', 1, 1, 2001, 100.0, \
         'flac', '{availability}', 'scan', 3000.0, 'automatic')"
    )
}

const TRACK_INSERT: &str = "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
     relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
     artist_name, artist_name_folded, album_title, album_title_folded, album_artist_name, \
     album_artist_name_folded, disc_number, track_number, year, duration_seconds, file_format, \
     availability, ingest_source, imported_at, membership_source) VALUES ";

/// A play recorded the way playback records one: the track's album and
/// first credited artist looked up with it, all empty for an unknown track.
fn recorded_play(id: &str, user: &str, track: &str, at: &str) -> String {
    format!(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         local_artist_id, track_name, artist_name, album_name, recording_mbid, \
         release_group_mbid, duration_ms, source, played_at) \
         SELECT '{id}', '{user}', t.id, t.local_album_id, \
         COALESCE((SELECT x.local_artist_id FROM local_track_artists x \
                   WHERE x.local_track_id = t.id ORDER BY x.position LIMIT 1), a.album_artist_id), \
         'x', 'x', NULL, NULL, NULL, NULL, NULL, '{at}' \
         FROM (SELECT 1) LEFT JOIN local_tracks t ON t.id = '{track}' \
         LEFT JOIN local_albums a ON a.id = t.local_album_id;"
    )
}

/// A mark as a Jellyfin app sets one: `tracks` selects the track ids it
/// covers, `at` is the server's time in milliseconds.
fn marked(user: &str, tracks: &str, played: bool, at: i64) -> String {
    let (values, update) = if played {
        ("?2, NULL", "marked_played_at = excluded.marked_played_at")
    } else {
        (
            "NULL, ?2",
            "marked_unplayed_at = excluded.marked_unplayed_at, marked_played_at = NULL",
        )
    };
    let values = values.replace("?2", &at.to_string());
    format!(
        "INSERT INTO library_user_play_marks \
         (user_id, local_track_id, marked_played_at, marked_unplayed_at) \
         SELECT '{user}', id, {values} FROM ({tracks}) WHERE true \
         ON CONFLICT (user_id, local_track_id) DO UPDATE SET {update};"
    )
}

/// The album play summary and `has_indexed_track` equal the rule worked
/// out from the plays, marks and tracks after every kind of write that
/// changes them, each in the shape of the statement that makes it.
#[tokio::test]
async fn album_play_summary_follows_every_write() {
    let fixture = seed_small().await;
    let db = fixture.db();
    let flag = |user: &'static str, album: &'static str| async move {
        fetch::<bool>(
            db,
            "SELECT played FROM library_user_album_plays WHERE user_id = ? AND local_album_id = ?",
            vec![text(user), text(album)],
            |row| row.get(0),
        )
        .await
        .into_iter()
        .next()
    };

    // Plays with a track, with only an album, in every v2 time form, and
    // marks.
    seed_small_plays(&fixture).await;
    seed_small_marks(&fixture).await;
    assert_album_summary(db, "seeded").await;

    // A new play, and one of a track the library does not have.
    let sql = recorded_play("new-1", "u2", "t9", "2026-02-01T00:00:00+00:00")
        + &recorded_play("new-2", "u2", "nope", "2026-02-01T00:00:01+00:00");
    write_batch(&fixture, "summary new play", sql).await;
    assert_album_summary(db, "new play").await;

    // Plays that named nothing, filed by the repair (three ids at once)
    // and by the import's link step (the track, then the album).
    write_batch(
        &fixture,
        "summary loose plays",
        "INSERT INTO library_play_history (id, user_id, track_name, artist_name, played_at) \
         VALUES ('loose-1', 'u2', 'x', 'x', '2026-02-02T00:00:00Z'), \
         ('loose-2', 'u3', 'x', 'x', '2026-02-03 08:00:00'); \
         UPDATE library_play_history SET local_track_id = t.id, \
         local_album_id = t.local_album_id, \
         local_artist_id = COALESCE((SELECT x.local_artist_id FROM local_track_artists x \
                                     WHERE x.local_track_id = t.id ORDER BY x.position LIMIT 1), \
                                    a.album_artist_id) \
         FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id \
         WHERE t.id = 't1' \
         AND library_play_history.rowid = (SELECT rowid FROM library_play_history \
                                           WHERE id = 'loose-1') \
         AND library_play_history.local_track_id IS NULL \
         AND library_play_history.local_album_id IS NULL \
         AND library_play_history.local_artist_id IS NULL; \
         INSERT INTO import_pending_links (target_table, target_key, ref_column, ref_kind, \
         v2_id, mode) VALUES \
         ('library_play_history', json_array('loose-2'), 'local_track_id', 'track', 't6', \
          'until_resolved'), \
         ('library_play_history', json_array('loose-2'), 'local_album_id', 'album', 'al3', \
          'until_resolved');"
            .to_owned(),
    )
    .await;
    assert_album_summary(db, "repair").await;
    let fill = |column: &str| {
        let pending = format!(
            "FROM main.import_pending_links p WHERE p.target_table = 'library_play_history' \
             AND p.ref_column = '{column}' AND p.mode = 'until_resolved' \
             AND p.target_key = json_array(t.\"id\")"
        );
        format!(
            "UPDATE main.\"library_play_history\" AS t SET \"{column}\" = \
             (SELECT p.v2_id {pending}) WHERE t.\"{column}\" IS NULL AND EXISTS (SELECT 1 {pending})"
        )
    };
    write_batch(&fixture, "summary link track", fill("local_track_id")).await;
    assert_album_summary(db, "link fill, track").await;
    write_batch(&fixture, "summary link album", fill("local_album_id")).await;
    assert_album_summary(db, "link fill, album").await;

    // Played and unplayed marks, on one track and on a whole album, and a
    // listener with marks alone.
    let one = |id: &str| format!("SELECT id FROM local_tracks WHERE id = '{id}'");
    let album = |id: &str| {
        format!(
            "SELECT id FROM local_tracks WHERE local_album_id = '{id}' \
             AND id NOT IN (SELECT alias FROM local_track_aliases)"
        )
    };
    let sql = "INSERT INTO auth_users (id, display_name, role, created_at) \
               VALUES ('u4', 'Four', 'user', 'now');"
        .to_owned()
        + &marked("u3", &one("t4"), true, 1_800_000_000_000)
        + &marked("u1", &album("al2"), false, 1_800_000_001_000)
        + &marked("u1", &one("t3"), true, 1_800_000_002_000)
        + &marked("u3", &one("t3"), true, 1_800_000_003_000)
        + &marked("u4", &album("al3"), true, 1_800_000_004_000);
    write_batch(&fixture, "summary marks", sql).await;
    assert_album_summary(db, "marks").await;
    write_batch(
        &fixture,
        "summary mark again",
        marked("u1", &album("al2"), true, 1_800_000_005_000),
    )
    .await;
    assert_album_summary(db, "marked played after unplayed").await;

    // A missing track continues into the track that took its place: its
    // plays move, one listener's mark moves, another's merges into the
    // mark already on the new track.
    let sql = format!("{TRACK_INSERT}{};", track_values("t10", "al1", "indexed"))
        + &marked("u3", &one("t10"), false, 1_800_000_006_000)
        + "UPDATE library_play_history SET local_track_id = 't10' WHERE local_track_id = 't3'; \
           UPDATE library_user_play_marks SET local_track_id = 't10' \
           WHERE user_id = 'u1' AND local_track_id = 't3'; \
           UPDATE library_user_play_marks SET marked_played_at = NULL, \
           marked_unplayed_at = 1800000006000 WHERE user_id = 'u3' AND local_track_id = 't10'; \
           DELETE FROM library_user_play_marks WHERE user_id = 'u3' AND local_track_id = 't3';";
    write_batch(&fixture, "summary continuation", sql).await;
    assert_album_summary(db, "continuation").await;

    // A track moved to another album by hand, then an album merged into
    // another: its tracks move, it is retired, and the plays filed under
    // it with no track follow.
    write_batch(
        &fixture,
        "summary track move",
        "UPDATE local_tracks SET local_album_id = 'al4', membership_source = 'manual', \
         membership_locked = 1, row_revision = row_revision + 1 WHERE id IN ('t7')"
            .to_owned(),
    )
    .await;
    assert_album_summary(db, "track moved").await;
    write_batch(
        &fixture,
        "summary album merge",
        "UPDATE local_tracks SET local_album_id = 'al2' WHERE local_album_id = 'al5'; \
         UPDATE local_albums SET retired_into_album_id = 'al2', grouping_locked = 1, \
         updated_at = 5000, row_revision = row_revision + 1 WHERE id = 'al5'; \
         UPDATE library_play_history SET local_album_id = 'al2' WHERE local_album_id = 'al5';"
            .to_owned(),
    )
    .await;
    assert_album_summary(db, "album merged").await;

    // A track going missing completes its album, then comes back; the
    // album is excluded by review and let back in; an update that sets
    // availability to itself.
    let sql = "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
               album_artist_name, album_artist_name_folded, album_artist_id, year, \
               grouping_source, created_at, updated_at) VALUES \
               ('al6', 'r1', 'g6', 'Sixth', 'sixth', 'Aurora', 'aurora', 'a1', 2001, \
               'automatic', 3000, 3000);"
        .to_owned()
        + &format!(
            "{TRACK_INSERT}{}, {};",
            track_values("t11", "al6", "indexed"),
            track_values("t12", "al6", "indexed")
        )
        + &recorded_play("new-3", "u3", "t11", "2026-02-04T00:00:00Z");
    write_batch(&fixture, "summary sixth album", sql).await;
    assert_album_summary(db, "sixth album").await;
    assert_eq!(flag("u3", "al6").await, Some(false));
    write_batch(
        &fixture,
        "summary missing",
        "UPDATE local_tracks SET availability = 'missing', missing_since = 4000 \
         WHERE root_id = 'r1' AND relative_path = 't12.flac' AND availability = 'indexed'"
            .to_owned(),
    )
    .await;
    assert_album_summary(db, "missing").await;
    assert_eq!(flag("u3", "al6").await, Some(true), "its last track went");
    write_batch(
        &fixture,
        "summary back",
        "UPDATE local_tracks SET availability = 'indexed', missing_since = NULL \
         WHERE id = 't12'"
            .to_owned(),
    )
    .await;
    assert_album_summary(db, "back").await;
    assert_eq!(flag("u3", "al6").await, Some(false), "the track is back");
    write_batch(
        &fixture,
        "summary excluded",
        "UPDATE local_tracks SET manual_excluded = 1, \
         availability = CASE WHEN availability = 'indexed' THEN 'excluded' \
                        ELSE availability END, \
         excluded_at = 4100, row_revision = row_revision + 1 \
         WHERE local_album_id = 'al6' AND manual_excluded = 0"
            .to_owned(),
    )
    .await;
    assert_album_summary(db, "excluded").await;
    write_batch(
        &fixture,
        "summary let back in",
        "UPDATE local_tracks SET manual_excluded = 0, \
         availability = CASE WHEN availability = 'excluded' AND applied_policy <> 'excluded' \
                        THEN 'indexed' ELSE availability END, \
         excluded_at = CASE WHEN applied_policy = 'excluded' THEN excluded_at ELSE NULL END, \
         row_revision = row_revision + 1 \
         WHERE manual_excluded = 1 AND local_album_id = 'al6'"
            .to_owned(),
    )
    .await;
    assert_album_summary(db, "let back in").await;
    write_batch(
        &fixture,
        "summary no-op",
        "UPDATE local_tracks SET availability = availability".to_owned(),
    )
    .await;
    assert_album_summary(db, "no-op").await;

    // A replaced file's track retires (missing, its path hidden), and the
    // new file's track takes its path on the same album.
    let sql = "UPDATE local_tracks SET availability = 'missing', missing_since = 4200, \
               relative_path = '.dn/replaced/t11/t11.flac', file_path = '/m/.dn/t11.flac', \
               path_hash = 'h-hidden', row_revision = row_revision + 1 \
               WHERE id = 't11' AND root_id = 'r1' AND relative_path = 't11.flac';"
        .to_owned()
        + &format!(
            "{TRACK_INSERT}{};",
            track_values("t13", "al6", "indexed").replace("'t13.flac'", "'t11.flac'")
        );
    write_batch(&fixture, "summary retire", sql).await;
    assert_album_summary(db, "retired").await;

    // A track with only a mark is deleted (the mark goes with it), then
    // its emptied album.
    let sql = "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
               album_artist_name, album_artist_name_folded, album_artist_id, year, \
               grouping_source, created_at, updated_at) VALUES \
               ('al7', 'r1', 'g7', 'Seventh', 'seventh', 'Aurora', 'aurora', 'a1', 2001, \
               'automatic', 3000, 3000);"
        .to_owned()
        + &format!("{TRACK_INSERT}{};", track_values("t14", "al7", "indexed"))
        + &marked("u1", &one("t14"), true, 1_800_000_007_000);
    write_batch(&fixture, "summary seventh album", sql).await;
    assert_album_summary(db, "seventh album").await;
    assert_eq!(flag("u1", "al7").await, Some(true));
    write_batch(
        &fixture,
        "summary track deleted",
        "DELETE FROM local_tracks WHERE id = 't14'".to_owned(),
    )
    .await;
    assert_album_summary(db, "track deleted").await;
    assert_eq!(
        flag("u1", "al7").await,
        None,
        "the mark went with its track"
    );
    write_batch(
        &fixture,
        "summary album deleted",
        "DELETE FROM local_albums WHERE id = 'al7'".to_owned(),
    )
    .await;
    assert_album_summary(db, "album deleted").await;

    // Deleted listeners take their marks and keep their plays.
    write_batch(
        &fixture,
        "summary users deleted",
        "DELETE FROM auth_users WHERE id IN ('u3', 'u4')".to_owned(),
    )
    .await;
    assert_album_summary(db, "users deleted").await;
    let left: Vec<String> = fetch(
        db,
        "SELECT DISTINCT user_id FROM library_user_album_plays ORDER BY user_id",
        Vec::new(),
        |row| row.get(0),
    )
    .await;
    assert_eq!(left, ["u1", "u2", "u3"], "u4 had marks alone");
}

/// One kept artist and album pair: artist, album, indexed tracks, live,
/// appearance.
type ArtistAlbumPair = (String, String, i64, bool, bool);
/// One kept artist row: artist, folded name, album count, appearance
/// count, and each negated count added to its count.
type ArtistCountRow = (String, String, i64, i64, i64, i64);

fn pair_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtistAlbumPair> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn count_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtistCountRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

/// The kept artist and album pairs and artist counts equal the rule
/// worked out from the credits, tracks, albums and artists with plain
/// grouped reads: the pairs from every credit on an indexed track, the
/// counts as the artist list counted them for every live credited artist.
async fn assert_artist_counts(db: &Db, step: &str) {
    let kept = fetch(
        db,
        "SELECT local_artist_id, local_album_id, indexed_tracks, live, appearance \
         FROM library_artist_album_credits ORDER BY 1, 2",
        Vec::new(),
        pair_row,
    )
    .await;
    let worked = fetch(
        db,
        "SELECT c.local_artist_id, t.local_album_id, COUNT(DISTINCT t.id), \
         b.retired_into_album_id IS NULL, \
         b.retired_into_album_id IS NULL AND b.album_artist_id != c.local_artist_id \
         FROM local_track_artists c JOIN local_tracks t ON t.id = c.local_track_id \
         JOIN local_albums b ON b.id = t.local_album_id \
         WHERE t.availability = 'indexed' GROUP BY 1, 2 ORDER BY 1, 2",
        Vec::new(),
        pair_row,
    )
    .await;
    assert_eq!(kept, worked, "{step}: artist and album pairs");
    let kept = fetch(
        db,
        "SELECT local_artist_id, folded_name, album_count, appearance_count, \
         album_count + album_count_neg, appearance_count + appearance_count_neg \
         FROM library_artist_counts ORDER BY 1",
        Vec::new(),
        count_row,
    )
    .await;
    let worked = fetch(
        db,
        &format!(
            "SELECT r.id, r.folded_name, \
             (SELECT COUNT(*) FROM local_albums lo WHERE lo.album_artist_id = r.id \
              AND lo.retired_into_album_id IS NULL AND lo.has_indexed_track = 1), \
             (SELECT COUNT(DISTINCT t.local_album_id) FROM local_track_artists c \
              JOIN local_tracks t ON t.id = c.local_track_id \
              JOIN local_albums b ON b.id = t.local_album_id \
              WHERE c.local_artist_id = r.id AND t.availability = 'indexed' \
              AND b.retired_into_album_id IS NULL AND b.album_artist_id != r.id), 0, 0 \
             FROM local_artists r WHERE r.retired_into_artist_id IS NULL AND {} ORDER BY 1",
            sql::CREDITED_PREDICATE
        ),
        Vec::new(),
        count_row,
    )
    .await;
    assert_eq!(kept, worked, "{step}: artist counts");
}

const ALBUM_INSERT: &str = "INSERT INTO local_albums (id, root_id, grouping_key, title, \
     title_folded, album_artist_name, album_artist_name_folded, album_artist_id, year, \
     grouping_source, created_at, updated_at, retired_into_album_id) VALUES ";

/// An album row on the small fixture's root, led by `artist`.
fn album_values(id: &str, artist: &str, retired_into: &str) -> String {
    format!(
        "('{id}', 'r1', 'g-{id}', '{id}', '{id}', '{artist}', '{artist}', '{artist}', 2001, \
         'automatic', 3000, 3000, {retired_into})"
    )
}

/// The kept artist pairs and counts equal the rule worked out from the
/// source rows after every kind of write that changes them, each in the
/// shape of the statement that makes it.
#[tokio::test]
async fn artist_counts_follow_every_write() {
    let fixture = seed_small().await;
    let db = fixture.db();
    assert_artist_counts(db, "seeded").await;
    let step = |label: &'static str, sql: String| {
        let fixture = &fixture;
        async move {
            write_batch(fixture, label, sql).await;
            assert_artist_counts(fixture.db(), label).await;
        }
    };

    // A scan adds an album: its artists, the album and its credits (one
    // artist credited on the album only), then tracks and their credits.
    step(
        "new album",
        format!(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('a4', 'Delta', 'delta', 'person', 3000, 3000), \
             ('a5', 'Echo', 'echo', 'person', 3000, 3000); \
             {ALBUM_INSERT}{}; \
             INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
             VALUES ('al6', 0, 'a4', 'main'), ('al6', 1, 'a5', 'main'); \
             {TRACK_INSERT}{}, {}; \
             INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
             VALUES ('t10', 0, 'a4', 'main'), ('t11', 0, 'a4', 'main'), \
             ('t11', 1, 'a3', 'guest');",
            album_values("al6", "a4", "NULL"),
            track_values("t10", "al6", "indexed"),
            track_values("t11", "al6", "indexed"),
        ),
    )
    .await;
    // A rescan rewrites a track's credits: the guest moves to another
    // artist, and an artist is credited twice on one track.
    step(
        "credits rewritten",
        "DELETE FROM local_track_artists WHERE local_track_id = 't6'; \
         INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
         VALUES ('t6', 0, 'a2', 'main'), ('t6', 1, 'a4', 'guest'), ('t6', 2, 'a4', 'guest');"
            .to_owned(),
    )
    .await;
    // The missing sweep takes the album's last indexed tracks: it stops
    // being owned, and its album-credited artist leaves the list.
    step(
        "last tracks missing",
        "UPDATE local_tracks SET availability = 'missing', missing_since = 4000 \
         WHERE id IN ('t10', 't11') AND availability = 'indexed';"
            .to_owned(),
    )
    .await;
    let listed: i64 = fetch_one(
        db,
        "SELECT COUNT(*) FROM library_artist_counts WHERE local_artist_id = 'a5'",
        Vec::new(),
        |row| row.get(0),
    )
    .await;
    assert_eq!(
        listed, 0,
        "an artist credited on an album with no files is not listed"
    );
    step(
        "tracks back",
        "UPDATE local_tracks SET availability = 'indexed', missing_since = NULL \
         WHERE id IN ('t10', 't11');"
            .to_owned(),
    )
    .await;
    step(
        "track excluded",
        "UPDATE local_tracks SET availability = 'excluded', excluded_at = 4100 \
         WHERE id = 't11';"
            .to_owned(),
    )
    .await;
    // A track moved by hand to an album with no files, then an album merged
    // into another: its tracks move and it is retired.
    step(
        "track moved",
        "UPDATE local_tracks SET local_album_id = 'al4', membership_source = 'manual', \
         membership_locked = 1, row_revision = row_revision + 1 WHERE id IN ('t7')"
            .to_owned(),
    )
    .await;
    step(
        "album merged",
        "UPDATE local_tracks SET local_album_id = 'al2' WHERE local_album_id = 'al5'; \
         UPDATE local_albums SET retired_into_album_id = 'al2', grouping_locked = 1, \
         updated_at = 5000, row_revision = row_revision + 1 WHERE id = 'al5';"
            .to_owned(),
    )
    .await;
    // Albums retired into the merged one follow it on: nothing they hold
    // changes.
    step(
        "retired album retargeted",
        "UPDATE local_albums SET retired_into_album_id = 'al1' \
         WHERE retired_into_album_id = 'al2';"
            .to_owned(),
    )
    .await;
    step(
        "album brought back",
        "UPDATE local_albums SET retired_into_album_id = NULL, grouping_source = 'automatic', \
         grouping_locked = 0, updated_at = 5100, row_revision = row_revision + 1 \
         WHERE id = 'al5';"
            .to_owned(),
    )
    .await;
    // An album with files, retired as an import can bring it: its artist
    // has a pair that counts for nothing.
    step(
        "credited only on a retired album",
        format!(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('a6', 'Foxtrot', 'foxtrot', 'person', 3000, 3000); \
             {ALBUM_INSERT}{}; \
             {TRACK_INSERT}{}; \
             INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
             VALUES ('t12', 0, 'a6', 'main');",
            album_values("al7", "a1", "'al1'"),
            track_values("t12", "al7", "indexed"),
        ),
    )
    .await;
    // Brought back with its file: the pair counts again and its artist,
    // credited on someone else's album, is listed.
    step(
        "album with files brought back",
        "UPDATE local_albums SET retired_into_album_id = NULL, grouping_source = 'automatic', \
         grouping_locked = 0, updated_at = 5150, row_revision = row_revision + 1 \
         WHERE id = 'al7';"
            .to_owned(),
    )
    .await;
    step(
        "leader changed",
        "UPDATE local_albums SET album_artist_id = 'a3', updated_at = 5200 WHERE id = 'al3';"
            .to_owned(),
    )
    .await;
    step(
        "album credit added",
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
         VALUES ('al1', 1, 'a5', 'main');"
            .to_owned(),
    )
    .await;
    step(
        "album credit removed",
        "DELETE FROM local_album_artists WHERE local_album_id = 'al1' AND position = 1;".to_owned(),
    )
    .await;
    step(
        "artist renamed",
        "UPDATE local_artists SET display_name = 'Aardvark', folded_name = 'aardvark' \
         WHERE id = 'a2';"
            .to_owned(),
    )
    .await;
    // A scan follows a retagged album onto its old row: the old row takes
    // the new row's values (another leader), the tracks and album credits
    // move over, and the new row goes.
    step(
        "album followed",
        format!(
            "{ALBUM_INSERT}{}; \
             INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
             VALUES ('al8', 0, 'a5', 'main'); \
             {TRACK_INSERT}{}; \
             INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
             VALUES ('t13', 0, 'a5', 'main'), ('t13', 1, 'a1', 'guest');",
            album_values("al8", "a5", "NULL"),
            track_values("t13", "al8", "indexed"),
        ),
    )
    .await;
    step(
        "album followed onto its old row",
        "UPDATE local_albums SET (root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, updated_at) = \
         (SELECT root_id, grouping_key || '-kept', title, title_folded, album_artist_name, \
          album_artist_name_folded, album_artist_id, year, updated_at \
          FROM local_albums WHERE id = 'al8'), row_revision = row_revision + 1 \
         WHERE id = 'al6'; \
         UPDATE local_tracks SET local_album_id = 'al6' WHERE local_album_id = 'al8'; \
         DELETE FROM local_album_artists WHERE local_album_id = 'al6'; \
         UPDATE local_album_artists SET local_album_id = 'al6' WHERE local_album_id = 'al8'; \
         DELETE FROM local_albums WHERE id = 'al8';"
            .to_owned(),
    )
    .await;
    // An artist merged into another: album credits, track credits and led
    // albums move, then the artist is retired.
    step(
        "artist merged",
        "UPDATE local_album_artists SET local_artist_id = 'a1', row_revision = row_revision + 1 \
         WHERE local_artist_id = 'a2'; \
         UPDATE local_track_artists SET local_artist_id = 'a1', row_revision = row_revision + 1 \
         WHERE local_artist_id = 'a2'; \
         UPDATE local_albums SET album_artist_id = 'a1', updated_at = 6000, \
         row_revision = row_revision + 1 WHERE album_artist_id = 'a2'; \
         UPDATE local_artists SET retired_into_artist_id = 'a1', updated_at = 6000, \
         row_revision = row_revision + 1 WHERE id = 'a2';"
            .to_owned(),
    )
    .await;
}

/// Every statement that reads the play history or the marks for Jellyfin
/// reads them by key alone: the plays through the per-listener index over
/// listener, track and time, or the small one of plays with an album and
/// no track, and the marks through their primary key. Neither is ever
/// scanned, nor is any other table: only subqueries are read whole, and an
/// unplayed page walks the catalog's ids and stops at the page. The album
/// statements read neither: only the listener's album play summary, by its
/// key or one of its two order indexes, and no album page looks into the
/// tracks. This holds whether the planner has statistics or not (a fresh
/// install has none until the daily ANALYZE after the first plays).
/// Subsonic's album lists and play counts read the plays alone, through
/// the same two indexes.
async fn assert_history_plans(db: &Db, state: &str) {
    use library::player::{
        AlbumSequence, AlbumShape, HistoryKey, ItemKind, PlayedFilter, TrackKey, TrackShape,
    };
    const BY_TRACK: &str = "idx_play_history_user_track (user_id=?";
    const NO_TRACK: &str = "idx_play_history_user_album_no_track (user_id=?";
    const MARKS: &str = "sqlite_autoindex_library_user_play_marks_1 (user_id=?";
    const SUMMARY: [&str; 3] = [
        "USING PRIMARY KEY (user_id=?",
        "idx_user_album_plays_last_played (user_id=?",
        "idx_user_album_plays_count (user_id=?",
    ];

    let listener = ExplainBind::Text(Some("u1"));
    let id = ExplainBind::Text(Some("al1"));
    let (limit, offset) = (ExplainBind::Int(100), ExplainBind::Int(0));
    // The listener in every place the statement takes one, and the page
    // last: a statement's own count of them, so the plans read here are
    // the statements production runs.
    let page = |sql: &str, page: bool| {
        let marks = sql.matches('?').count() - if page { 2 } else { 0 };
        let mut binds = vec![listener; marks];
        if page {
            binds.extend([limit, offset]);
        }
        binds
    };
    // A page of ids, as a list asks for them: the planner weighs the
    // indexes differently for one id and for many.
    let ids = 50;
    let with_ids = |first: &[ExplainBind<'static>], second: &[ExplainBind<'static>]| {
        let mut binds = Vec::new();
        for half in [first, second] {
            for bind in half {
                match bind {
                    ExplainBind::Int(_) => binds.extend(std::iter::repeat_n(id, ids)),
                    text => binds.push(*text),
                }
            }
        }
        binds
    };
    // An `Int` here stands for the page of ids.
    let ids_mark = ExplainBind::Int(0);
    // The played ids as a JSON list, in every place a count takes a bind.
    let list = ExplainBind::Text(Some("[]"));
    let listed = |sql: &str| vec![list; sql.matches('?').count()];
    // No filter but streamability.
    let tracks = TrackShape::OPEN;
    // The browsable albums, as every album page filters them.
    let albums = AlbumShape {
        artist: false,
        album_artists: false,
        appears: false,
        genre: false,
        scoped: false,
        played: PlayedFilter::Any,
    };
    let sql = sql::player_tracks_played(tracks, TrackKey::Open, HistoryKey::LastPlayed, true);
    let mut statements: Vec<(&str, String, Vec<ExplainBind<'_>>)> =
        vec![("tracks played by last play", sql.clone(), page(&sql, true))];
    let sql = sql::player_tracks_played(tracks, TrackKey::Open, HistoryKey::PlayCount, true);
    statements.push(("tracks played by play count", sql.clone(), page(&sql, true)));
    let sql = sql::player_tracks_played_count(tracks, TrackKey::Open);
    statements.push(("tracks played count", sql.clone(), listed(&sql)));
    let sql = sql::player_tracks_unplayed(tracks, true, false);
    statements.push(("tracks unplayed", sql.clone(), page(&sql, true)));
    let sql = sql::player_tracks_unplayed(tracks, true, true);
    statements.push((
        "tracks unplayed after a count",
        sql.clone(),
        page(&sql, true),
    ));
    let sql = sql::player_tracks_unplayed_count(tracks);
    statements.push(("tracks unplayed count", sql.clone(), listed(&sql)));
    let played = sql::PLAYER_PLAYED_TRACKS.text();
    statements.extend([
        ("played tracks", played.to_owned(), page(played, false)),
        (
            "track values",
            sql::player_track_values(ListLen::new(ids)),
            with_ids(&[listener, ids_mark], &[listener, ids_mark]),
        ),
    ]);
    let track_statements = statements.len();
    let sql = sql::player_albums_played(albums, HistoryKey::LastPlayed, true);
    statements.push(("albums played by last play", sql.clone(), page(&sql, true)));
    let sql = sql::player_albums_played(albums, HistoryKey::PlayCount, true);
    statements.push(("albums played by play count", sql.clone(), page(&sql, true)));
    let sql = sql::player_albums_played_count(albums);
    statements.push(("albums played count", sql.clone(), listed(&sql)));
    let sql = sql::player_albums_unplayed(albums, true, false);
    statements.push(("albums unplayed", sql.clone(), page(&sql, true)));
    let sql = sql::player_albums_unplayed(albums, true, true);
    statements.push((
        "albums unplayed after a count",
        sql.clone(),
        page(&sql, true),
    ));
    let sql = sql::player_albums_unplayed_count(albums);
    statements.push(("albums unplayed count", sql.clone(), listed(&sql)));
    let played_ids = sql::PLAYER_ALBUM_PLAYED_IDS.text();
    let played = sql::PLAYER_PLAYED_ALBUMS.text();
    statements.extend([
        (
            "played album ids",
            played_ids.to_owned(),
            page(played_ids, false),
        ),
        ("played albums", played.to_owned(), page(played, false)),
        (
            "album values",
            sql::player_album_values(ListLen::new(ids)),
            with_ids(&[listener, ids_mark], &[]),
        ),
    ]);
    let sql = sql::player_album_page(albums, AlbumSequence::Title);
    statements.push(("albums page by title", sql.clone(), page(&sql, true)));
    let sql = sql::player_album_page(albums, AlbumSequence::Shuffle);
    statements.push(("albums page shuffled", sql.clone(), page(&sql, true)));
    for (index, (what, sql, binds)) in statements.iter().enumerate() {
        let plan = explain(db, sql, binds).await;
        let what = format!("{what}, {state}: {plan:?}");
        let album = index >= track_statements;
        let mut read = (0, 0);
        for line in &plan {
            let plays = line.starts_with("SEARCH h ")
                || line.starts_with("SCAN h")
                || line.contains("library_play_history");
            let marks = line.starts_with("SEARCH m ")
                || line.starts_with("SCAN m")
                || line.contains("library_user_play_marks");
            if album {
                assert!(
                    !plays && !marks,
                    "an album statement reads neither plays nor marks: {line}; {what}"
                );
                if line.contains("library_user_album_plays") {
                    assert!(
                        line.starts_with("SEARCH ") && SUMMARY.iter().any(|key| line.contains(key)),
                        "the summary is read by the listener's key: {line}; {what}"
                    );
                }
                assert!(
                    !line.starts_with("SEARCH t "),
                    "an album statement does not look into the tracks: {line}; {what}"
                );
                continue;
            }
            if plays {
                read.0 += 1;
                assert!(
                    line.starts_with("SEARCH h ")
                        && (line.contains(BY_TRACK) || line.contains(NO_TRACK)),
                    "plays are read through a per-listener index: {line}; {what}"
                );
            }
            if marks {
                read.1 += 1;
                assert!(
                    line.starts_with("SEARCH m ") && line.contains(MARKS),
                    "marks are read by key: {line}; {what}"
                );
            }
        }
        // A count, and the half after it, read the played ids the page
        // read once, as a list; album pages read no plays.
        let listed = what.contains("count") || what.contains("page");
        if album {
            assert!(
                listed
                    || plan
                        .iter()
                        .any(|line| line.contains("library_user_album_plays")),
                "the summary is read: {what}"
            );
        } else {
            assert!(
                listed || (read.0 > 0 && read.1 > 0),
                "plays and marks are read: {what}"
            );
        }
        let subqueries: Vec<&str> = plan
            .iter()
            .filter_map(|line| {
                line.strip_prefix("CO-ROUTINE ")
                    .or_else(|| line.strip_prefix("MATERIALIZE "))
            })
            .collect();
        let walks = what.contains("unplayed") || what.contains("page");
        // A shuffle's seed is one constant row, scanned once.
        assert!(
            plan.iter()
                .filter_map(|line| line.strip_prefix("SCAN "))
                .all(|scanned| subqueries.contains(&scanned)
                    || scanned.contains("VIRTUAL TABLE")
                    || scanned == "CONSTANT ROW"
                    || (walks && matches!(scanned.split(' ').next(), Some("t" | "a")))),
            "no table is scanned but the catalog an unplayed half or a page walks: {what}"
        );
        // The played ids are one list read once.
        if what.contains("unplayed") && !what.contains("count,") {
            assert!(
                !plan
                    .iter()
                    .any(|line| line.contains("TEMP B-TREE FOR ORDER BY")
                        || line.starts_with("CORRELATED LIST SUBQUERY")),
                "the unplayed page is neither sorted nor probed per row: {what}"
            );
        }
        if what.contains(" played by") {
            assert!(
                plan.iter()
                    .any(|line| line.starts_with("SEARCH t USING")
                        || line.starts_with("SEARCH a USING")),
                "each played row is looked up by id: {what}"
            );
        }
        // An album's played page walks the order index for its key and
        // stops at the page.
        if album && what.contains(" played by") {
            assert!(
                !plan
                    .iter()
                    .any(|line| line.contains("TEMP B-TREE FOR ORDER BY")),
                "the played albums are not sorted: {what}"
            );
        }
    }

    // Subsonic: plays alone, as before.
    let subsonic = [
        ("recently played", sql::player_history_albums(false)),
        ("most played", sql::player_history_albums(true)),
    ];
    for (what, sql) in &subsonic {
        let plan = explain(db, sql, &page(sql, true)).await;
        let what = format!("albums {what}, {state}: {plan:?}");
        let history: Vec<&String> = plan
            .iter()
            .filter(|line| {
                line.contains("library_play_history") || line.contains("idx_play_history")
            })
            .collect();
        assert!(
            matches!(history.as_slice(), [by_track, no_track]
            if by_track.starts_with(
                "SEARCH library_play_history USING COVERING INDEX idx_play_history_user_track \
                 (user_id=?"
            ) && no_track.starts_with(
                "SEARCH library_play_history USING INDEX idx_play_history_user_album_no_track \
                 (user_id=?"
            )),
            "the listener's plays come from the two history indexes alone: {what}"
        );
        let subqueries: Vec<&str> = plan
            .iter()
            .filter_map(|line| {
                line.strip_prefix("CO-ROUTINE ")
                    .or_else(|| line.strip_prefix("MATERIALIZE "))
            })
            .collect();
        assert!(
            plan.iter()
                .filter_map(|line| line.strip_prefix("SCAN "))
                .all(|scanned| subqueries.contains(&scanned)),
            "no table is scanned: {what}"
        );
        assert!(
            plan.iter().any(|line| line.starts_with("SEARCH a USING")),
            "each played album is looked up by id: {what}"
        );
    }
    let probe = "(user_id=? AND local_track_id=?)";
    let sql = sql::player_play_stats(ItemKind::Track, ListLen::new(1));
    let binds = [ExplainBind::Text(Some("u1")), ExplainBind::Text(Some("t1"))];
    let plan = explain(db, &sql, &binds).await;
    assert!(
        plan.len() == 1 && plan[0].contains("COVERING INDEX idx_play_history_user_track"),
        "track counts come from the index alone, {state}: {plan:?}"
    );
    let sql = sql::player_play_stats(ItemKind::Album, ListLen::new(ids));
    let mut half = vec![ExplainBind::Text(Some("u1"))];
    half.resize(ids + 1, ExplainBind::Text(Some("al1")));
    let binds = half.repeat(2);
    let plan = explain(db, &sql, &binds).await;
    assert!(
        plan.iter().any(|line| line
            .starts_with("SEARCH h USING COVERING INDEX idx_play_history_user_track")
            && line.contains(probe))
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
    assert_history_plans(fixture.db(), "no statistics").await;

    // Statistics over a history shared by many listeners. A connection
    // loads them when the schema changes, so the plans are read after a
    // change.
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
    // Marks for a quarter of the listeners, so the statistics cover the
    // marks table too.
    let mut marks =
        String::from("INSERT INTO auth_users (id, display_name, role, created_at) VALUES ");
    for user in 0..50 {
        if user > 0 {
            marks.push_str(", ");
        }
        marks.push_str(&format!("('u{user}', 'U', 'user', 'now')"));
    }
    marks.push_str(
        "; INSERT INTO library_user_play_marks (user_id, local_track_id, marked_played_at, \
         marked_unplayed_at) VALUES ",
    );
    for user in 0..50 {
        if user > 0 {
            marks.push_str(", ");
        }
        marks.push_str(&format!(
            "('u{user}', 't1', NULL, 1767225600000), ('u{user}', 't7', 1767225600000, NULL)"
        ));
    }
    write_batch(&fixture, "plan marks", marks).await;
    // A whole ANALYZE, then a schema change so every pooled connection
    // reloads the statistics before it plans again.
    write_batch(
        &fixture,
        "plan statistics",
        "ANALYZE; CREATE TABLE perf_reload_statistics (x); DROP TABLE perf_reload_statistics"
            .to_owned(),
    )
    .await;
    let statistics: i64 = fetch_one(
        fixture.db(),
        "SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'library_play_history'",
        Vec::new(),
        |row| row.get(0),
    )
    .await;
    assert!(statistics > 0, "the history table has statistics");
    assert_history_plans(fixture.db(), "with statistics").await;
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

/// Marks over the 100k catalog for `p-u1`: 500 tracks it played marked
/// unplayed after their plays, and 500 tracks it never played marked
/// played, nine on each of 50 albums (completing them with the track it
/// played there) and one on each of 50 more.
async fn seed_100k_marks(fixture: &Fixture) {
    let mut sql = String::from(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('p-u1', 'P', 'user', 'now'); \
         INSERT INTO library_user_play_marks (user_id, local_track_id, marked_played_at, \
         marked_unplayed_at) VALUES ",
    );
    let mut marks = Vec::new();
    for step in 1500..2000 {
        marks.push((step * 50, "NULL", "1800000000000"));
    }
    for album in 0..50 {
        for offset in 1..10 {
            marks.push((album * 50 + offset, "1800000000000", "NULL"));
        }
    }
    for album in 50..100 {
        marks.push((album * 50 + 25, "1800000000000", "NULL"));
    }
    for (index, (track, played, unplayed)) in marks.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("('p-u1', 'p-t{track:06}', {played}, {unplayed})"));
    }
    write_batch(fixture, "perf marks", sql).await;
}

/// Guests over the 100k catalog: 1,000 artists who lead nothing, the n-th
/// credited second on one track of each of `n % 10 + 1` albums led by
/// others (5,500 credits), so the contributors view has artists to count.
async fn seed_100k_guests(fixture: &Fixture) {
    let mut artists = String::from(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) VALUES ",
    );
    let mut credits = String::from(
        "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) VALUES ",
    );
    for guest in 0..1000 {
        if guest > 0 {
            artists.push_str(", ");
        }
        artists.push_str(&format!(
            "('p-g{guest:04}', 'Perf Guest {guest:04}', 'perf guest {guest:04}', 'person', 1000, 1000)"
        ));
        for album in 0..(guest % 10 + 1) {
            if guest > 0 || album > 0 {
                credits.push_str(", ");
            }
            let track = guest * 100 + album * 10;
            credits.push_str(&format!("('p-t{track:06}', 1, 'p-g{guest:04}', 'guest')"));
        }
    }
    artists.push(';');
    credits.push(';');
    write_batch(fixture, "perf guests", artists).await;
    write_batch(fixture, "perf guest credits", credits).await;
}

fn percentile(sorted: &mut [f64], pct: f64) -> f64 {
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let rank = (pct / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.max(1).min(sorted.len()) - 1]
}

/// Acceptance: every standard read holds the 10 ms p95 budget on a seeded
/// 100k catalog, 50 samples after 3 warmup per endpoint. Ignored by
/// default: timing is machine-dependent, so CI guards the mechanisms
/// (fixture pages, totals, and plan tests above) while this runs explicitly
/// with --release on reference hardware, matching the HTTP bench
/// shape (checkpointed store, optimized build). Debug runs only report
/// timings: an unoptimized build adds milliseconds per call there.
#[tokio::test]
#[ignore = "100k acceptance timing; run explicitly with --release on reference hardware"]
async fn perf_100k_endpoints_hold_p95() {
    let fixture = seed_small().await;
    seed_100k(&fixture).await;
    seed_100k_guests(&fixture).await;
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
    // Owned albums only: the small fixture's Empty Halls has no files.
    assert_eq!(stats.total_albums, 10004);

    // A deep page starts after the key of the row the old offset paging
    // reached: album 9,000 by name, track 99,500 by title.
    let deep_albums = After {
        seed: 0,
        key: Some(
            fetch(
                fixture.db(),
                "SELECT title_folded, id FROM local_albums WHERE retired_into_album_id IS NULL \
                 ORDER BY title_folded, id LIMIT 1 OFFSET 8999",
                Vec::new(),
                |row| Ok(vec![row.get::<_, Value>(0)?, row.get::<_, Value>(1)?]),
            )
            .await
            .remove(0),
        ),
    };
    let deep_tracks = After {
        seed: 0,
        key: Some(
            fetch(
                fixture.db(),
                "SELECT title_folded, id FROM local_tracks WHERE availability = 'indexed' \
                 ORDER BY title_folded, id LIMIT 1 OFFSET 99499",
                Vec::new(),
                |row| Ok(vec![row.get::<_, Value>(0)?, row.get::<_, Value>(1)?]),
            )
            .await
            .remove(0),
        ),
    };
    let deep_by_artist = After {
        seed: 0,
        key: Some(
            fetch(
                fixture.db(),
                "SELECT COALESCE(artist_name_folded, ''), album_title_folded, local_album_id, \
                 disc_number, track_number, id FROM local_tracks \
                 WHERE availability = 'indexed' ORDER BY 1, 2, 3, 4, 5, 6 LIMIT 1 OFFSET 99499",
                Vec::new(),
                |row| (0..6).map(|index| row.get::<_, Value>(index)).collect(),
            )
            .await
            .remove(0),
        ),
    };
    let deep = (deep_albums, deep_tracks, deep_by_artist);
    // (name, samples). Every row holds the budget.
    let mut endpoints: Vec<(&str, Vec<f64>)> = [
        "library_albums",
        "library_artists",
        "library_tracks",
        "library_stats",
        "local_albums",
        "local_search_miss",
        "library_albums_deep_9000",
        "library_tracks_deep_99500",
        "library_tracks_by_artist",
        "library_tracks_by_artist_deep_99500",
        "library_recently_added",
        "library_contributors_by_appearances",
        "library_artists_by_appearances",
        "library_artists_by_album_count",
        "search_songs_hit",
        "search_songs_miss",
        "search_songs_short",
    ]
    .into_iter()
    .map(|name| (name, Vec::new()))
    .collect();
    let search = crate::reads::search::service::SearchService::new(fixture.db().sqlx().clone());
    // Timed and reported, not budgeted: random track pages key every track,
    // and a song search whose words are in every track ("perf", "artist")
    // reads the whole FTS index and ranks every track, as the player's
    // common-word search page does.
    let mut reported: Vec<(&str, Vec<f64>)> = [
        "library_tracks_random",
        "search_songs_common",
        "search_songs_deep_offset",
    ]
    .into_iter()
    .map(|name| (name, Vec::new()))
    .collect();
    for _ in 0..3 {
        for (name, _) in endpoints.iter().chain(&reported) {
            read_timed(&fixture.catalog, &search, name, &deep).await;
        }
    }
    for _ in 0..50 {
        for (name, samples) in endpoints.iter_mut().chain(&mut reported) {
            let tick = std::time::Instant::now();
            read_timed(&fixture.catalog, &search, name, &deep).await;
            samples.push(tick.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let mut report = Vec::new();
    for (name, samples) in reported.iter_mut().chain(&mut endpoints) {
        let p95 = percentile(samples, 95.0);
        let p50 = percentile(samples, 50.0);
        report.push(format!("{name}: p50 {p50:.2} ms, p95 {p95:.2} ms"));
    }
    eprintln!("100k catalog p95:\n{}", report.join("\n"));
    for (name, samples) in &mut endpoints {
        let p95 = percentile(samples, 95.0);
        // The budget binds release runs; debug runs report only, since
        // an unoptimized build adds milliseconds per call there.
        if !cfg!(debug_assertions) {
            assert!(
                p95 < 10.0,
                "{name} p95 {p95:.2} ms exceeds the 10 ms budget:\n{}",
                report.join("\n")
            );
        }
    }
}

/// One read of a timed endpoint: a song search page, or a library read.
async fn read_timed(
    catalog: &LocalCatalog,
    search: &crate::reads::search::service::SearchService,
    name: &str,
    deep: &(After, After, After),
) {
    let page = match name {
        "search_songs_hit" => Some(("track 0123", 0)),
        "search_songs_common" => Some(("perf artist 01", 0)),
        "search_songs_miss" => Some(("zzz-no-such-thing-zzz", 0)),
        "search_songs_short" => Some(("pe", 0)),
        "search_songs_deep_offset" => Some(("perf artist 01", 480)),
        _ => None,
    };
    let Some((query, offset)) = page else {
        return read_endpoint(catalog, name, deep).await;
    };
    let page = search
        .search_bucket(
            crate::reads::search::service::Bucket::Tracks,
            query,
            24,
            offset,
        )
        .await
        .expect("song search reads");
    assert_eq!(
        page.results.is_empty(),
        name == "search_songs_miss",
        "{name}"
    );
}

/// One read of a timed library endpoint.
async fn read_endpoint(catalog: &LocalCatalog, name: &str, deep: &(After, After, After)) {
    let first = After::default();
    let random = After {
        seed: 42,
        key: None,
    };
    let albums = AlbumFilter::default();
    let tracks = TrackFilter::default();
    let miss_albums = AlbumFilter {
        q: Some("zzz-no-such-thing-zzz".to_owned()),
        ..Default::default()
    };
    let miss_tracks = TrackFilter {
        q: Some("zzz-no-such-thing-zzz".to_owned()),
        ..Default::default()
    };
    let artists = |scope, sort| catalog.artist_list(scope, None, sort, true, 48, &first);
    match name {
        "library_albums" | "local_albums" => {
            catalog
                .list_albums(&albums, AlbumSort::Name, false, 50, &first)
                .await
                .expect("lists");
        }
        "library_albums_deep_9000" => {
            catalog
                .list_albums(&albums, AlbumSort::Name, false, 50, &deep.0)
                .await
                .expect("lists");
        }
        "library_artists" => {
            catalog
                .artist_list(ArtistScope::All, None, ArtistSort::Name, false, 50, &first)
                .await
                .expect("lists");
        }
        "library_contributors_by_appearances" => {
            artists(ArtistScope::Contributors, ArtistSort::AppearanceCount)
                .await
                .expect("lists");
        }
        "library_artists_by_appearances" => {
            artists(ArtistScope::All, ArtistSort::AppearanceCount)
                .await
                .expect("lists");
        }
        "library_artists_by_album_count" => {
            artists(ArtistScope::All, ArtistSort::AlbumCount)
                .await
                .expect("lists");
        }
        "library_tracks" => {
            catalog
                .list_tracks(&tracks, TrackSort::Title, false, 48, &first)
                .await
                .expect("lists");
        }
        "library_tracks_deep_99500" => {
            catalog
                .list_tracks(&tracks, TrackSort::Title, false, 48, &deep.1)
                .await
                .expect("lists");
        }
        "library_tracks_by_artist" => {
            catalog
                .list_tracks(&tracks, TrackSort::Artist, false, 48, &first)
                .await
                .expect("lists");
        }
        "library_tracks_by_artist_deep_99500" => {
            catalog
                .list_tracks(&tracks, TrackSort::Artist, false, 48, &deep.2)
                .await
                .expect("lists");
        }
        "library_tracks_random" => {
            catalog
                .list_tracks(&tracks, TrackSort::Random, false, 48, &random)
                .await
                .expect("lists");
        }
        "library_stats" => {
            catalog.stats().await.expect("stats read");
        }
        "library_recently_added" => {
            catalog.recently_added(20).await.expect("recent");
        }
        "local_search_miss" => {
            catalog
                .list_albums(&miss_albums, AlbumSort::Name, false, 20, &first)
                .await
                .expect("lists");
            catalog
                .list_tracks(&miss_tracks, TrackSort::Title, false, 20, &first)
                .await
                .expect("lists");
        }
        other => panic!("no timed endpoint {other}"),
    }
}

/// Acceptance: the player pages the Jellyfin and Subsonic browse routes
/// read (default track and album pages, a title-search miss, the newest
/// albums) hold the same 10 ms p95 budget on the seeded 100k catalog.
#[tokio::test]
#[ignore = "100k acceptance timing; run explicitly with --release on reference hardware"]
async fn perf_100k_player_pages_hold_p95() {
    use library::player::{AlbumOrder, AlbumQuery, OrderKey, TrackOrder, TrackQuery};

    let fixture = seed_small().await;
    seed_100k(&fixture).await;
    seed_100k_plays(&fixture).await;
    seed_100k_marks(&fixture).await;
    // Production statistics: the maintenance ANALYZE, then the hot plans
    // must still walk their indexes.
    droppedneedle_kernel::db::AnalyzeService::new(
        fixture.runtime.db().clone(),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    )
    .run()
    .await
    .expect("analyze runs");
    assert_hot_plans(fixture.db()).await;
    assert_history_plans(fixture.db(), "100k with statistics").await;
    let player = fixture.catalog.clone();
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
    let played_albums = |played: bool| AlbumQuery {
        played: Some(played),
        ..album_listener.clone()
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
        ("player_albums_played_only", 22),
        ("player_albums_unplayed_only_9000", 23),
        ("player_track_values_page", 24),
        ("player_album_values_page", 25),
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
                21 => player
                    .play_stats("p-u1", "album", &stat_albums)
                    .await
                    .map(|stats| stats.len()),
                22 => player
                    .albums(&played_albums(true), AlbumOrder::Title, 100, 0)
                    .await
                    .map(|page| page.0.len()),
                24 => player
                    .played_values("p-u1", "track", &stat_tracks)
                    .await
                    .map(|values| values.len()),
                25 => player
                    .played_values("p-u1", "album", &stat_albums)
                    .await
                    .map(|values| values.len()),
                _ => player
                    .albums(&played_albums(false), AlbumOrder::Title, 100, 9_000)
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
