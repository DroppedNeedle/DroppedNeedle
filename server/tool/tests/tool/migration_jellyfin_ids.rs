//! The Jellyfin ids v2 stored, through export, import and the fill: the id
//! an app holds still opens its item, and an album or artist id opens only
//! for someone signed in.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_compat::jellyfin::ids::{IdKind, JellyfinIds};
use droppedneedle_kernel::runtime_config::Crypto;
use droppedneedle_library::scan::naming::artist_key;
use droppedneedle_tool::tooling::fixture::{V2_ALBUM_ID, V2_TRACK_ID};
use rusqlite::{Connection, params};

use crate::common::ScratchDir;
use crate::migration_artists::{export, import, import_with, v2_artist, v2_instance, v2_track};

const ENTITY: &str = "jellyfin_id_carry";
/// The fixture's track, under an id `derive` would not give it.
const CARRIED: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// A track with two ids; the smaller is the one v2 handed out.
const TWO_IDS: &str = "v2-two-ids";
const SMALLER: &str = "00000000000000000000000000000001";
/// A track with its unkeyed id and a larger one: the unkeyed id stays.
const UNKEYED_FIRST: &str = "v2-unkeyed-first";
const LARGER: &str = "ffffffffffffffffffffffffffffffff";
const ALBUM_ALIAS: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const ARTIST_ALIAS: &str = "cccccccccccccccccccccccccccccccc";
/// A track whose stored id is the unkeyed id of `CLASH_VICTIM`.
const CLASHING: &str = "v2-clashing";
const CLASH_VICTIM: &str = "v2-clash-victim";
const NOTHING: &str = "dddddddddddddddddddddddddddddddd";
const TOMBSTONE: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const UPPERCASE: &str = "ABCDEFABCDEFABCDEFABCDEFABCDEFAB";
const WRONG_GENRE: &str = "99999999999999999999999999999999";
/// An artist the export gives a new id.
const REKEYED_ARTIST: &str = "v2-rekeyed-artist";

fn derive(kind: &str, internal: &str) -> String {
    JellyfinIds::derive(kind, internal)
}

/// Every row kind the rules name, in v2's table.
fn v2_rows(db: &Connection) {
    v2_artist(db, REKEYED_ARTIST, "Clairo", None, None);
    let artist = (REKEYED_ARTIST, "Clairo");
    for (track, path) in [
        (TWO_IDS, "c/two.flac"),
        (UNKEYED_FIRST, "c/first.flac"),
        (CLASHING, "c/clashing.flac"),
        (CLASH_VICTIM, "c/victim.flac"),
    ] {
        v2_track(db, track, V2_ALBUM_ID, path, artist);
    }
    let rows = [
        (CARRIED.to_owned(), "track", V2_TRACK_ID),
        (SMALLER.to_owned(), "track", TWO_IDS),
        (derive("track", TWO_IDS), "track", TWO_IDS),
        (derive("track", UNKEYED_FIRST), "track", UNKEYED_FIRST),
        (LARGER.to_owned(), "track", UNKEYED_FIRST),
        (derive("track", CLASH_VICTIM), "track", CLASHING),
        (ALBUM_ALIAS.to_owned(), "album", V2_ALBUM_ID),
        (ARTIST_ALIAS.to_owned(), "artist", REKEYED_ARTIST),
        (NOTHING.to_owned(), "track", "v2-gone"),
        (TOMBSTONE.to_owned(), "local_track", V2_TRACK_ID),
        (UPPERCASE.to_owned(), "track", V2_TRACK_ID),
        (WRONG_GENRE.to_owned(), "genre", "jazz"),
    ];
    for (id, kind, internal) in rows {
        db.execute(
            "INSERT INTO library_compat_id_map (jf_id, kind, internal_id) VALUES (?1, ?2, ?3)",
            params![id, kind, internal],
        )
        .expect("id row");
    }
}

/// The ids as a new process holds them, once its first fill pass has run.
async fn process(db_path: &Path) -> JellyfinIds {
    let handle = droppedneedle_kernel::db::Db::open_for_tests(db_path).expect("database opens");
    let config_dir = db_path
        .parent()
        .and_then(Path::parent)
        .expect("v3 folder")
        .join("config");
    let crypto = Arc::new(Crypto::load(&config_dir).expect("v3 key loads"));
    let ids = JellyfinIds::new(handle, crypto, Duration::from_millis(1));
    ids.fill().pass().await.expect("first pass");
    ids
}

async fn opens(ids: &JellyfinIds, id: &str, signed_in: bool) -> Option<(IdKind, String)> {
    ids.resolve(&[id.to_owned()], signed_in)
        .await
        .expect("resolves")
        .pop()
        .flatten()
}

fn stored(db_path: &Path) -> Vec<(String, String, String, i64)> {
    let db = Connection::open(db_path).expect("v3 db");
    let mut statement = db
        .prepare(
            "SELECT jellyfin_id, kind, internal_id, current FROM compat_jellyfin_ids \
             ORDER BY jellyfin_id",
        )
        .expect("prepares");
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("reads")
        .collect::<Result<_, _>>()
        .expect("rows")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_jellyfin_ids_keep_opening_their_items() {
    let scratch = ScratchDir::new("migration-jellyfin-ids");
    let root = scratch.to_path_buf();
    let (v2_root, _alice, db) = v2_instance(&root);
    v2_rows(&db);
    drop(db);
    let export_path = export(&root, &v2_root);

    let (preview, _) = import_with(&root, &export_path, true).await;
    let (report, db_path) = import(&root, &export_path).await;

    let ids = process(&db_path).await;
    let track = |id: &str| Some((IdKind::Track, id.to_owned()));
    // The id v2 handed out is the id, for anyone, and is what answers carry.
    assert_eq!(opens(&ids, CARRIED, false).await, track(V2_TRACK_ID));
    assert_eq!(ids.handed_out("track", V2_TRACK_ID), CARRIED);
    // Of two ids the smaller is current, and the other still opens signed in.
    let two_ids_unkeyed = derive("track", TWO_IDS);
    assert_eq!(ids.handed_out("track", TWO_IDS), SMALLER);
    assert_eq!(opens(&ids, SMALLER, false).await, track(TWO_IDS));
    assert_eq!(opens(&ids, &two_ids_unkeyed, false).await, None);
    assert_eq!(opens(&ids, &two_ids_unkeyed, true).await, track(TWO_IDS));
    let unkeyed = derive("track", UNKEYED_FIRST);
    assert_eq!(ids.handed_out("track", UNKEYED_FIRST), unkeyed);
    assert_eq!(opens(&ids, LARGER, true).await, track(UNKEYED_FIRST));
    // Album and artist ids open only for someone signed in, the artist's on
    // the id v3 gave it.
    assert_eq!(
        opens(&ids, ALBUM_ALIAS, true).await,
        Some((IdKind::Album, V2_ALBUM_ID.to_owned()))
    );
    assert_eq!(opens(&ids, ALBUM_ALIAS, false).await, None);
    let rekeyed = artist_key("Clairo", None);
    assert_ne!(rekeyed, REKEYED_ARTIST);
    assert_eq!(
        opens(&ids, ARTIST_ALIAS, true).await,
        Some((IdKind::Artist, rekeyed))
    );
    assert_eq!(opens(&ids, ARTIST_ALIAS, false).await, None);
    // An id that is another track's unkeyed id stays that track's, and the
    // track that stored it gets its own. This pins the tool's copy of the
    // formula: a different one would not see the clash.
    let victim = derive("track", CLASH_VICTIM);
    assert_eq!(opens(&ids, &victim, false).await, track(CLASH_VICTIM));
    assert_eq!(
        opens(&ids, &derive("track", CLASHING), false).await,
        track(CLASHING)
    );
    for dropped in [NOTHING, TOMBSTONE, WRONG_GENRE] {
        assert_eq!(opens(&ids, dropped, true).await, None, "{dropped}");
    }

    let counts = &report.entities[ENTITY];
    assert_eq!(counts.imported, 7, "{:?}", report.items);
    assert_eq!(counts.dropped_invalid, 3, "{:?}", report.items);
    assert_eq!(counts.conflict_kept_existing, 2, "{:?}", report.items);
    assert_eq!(
        &preview.entities[ENTITY], counts,
        "a dry run counts the same"
    );
    let left_behind: Vec<&str> = report
        .left_behind
        .iter()
        .map(|entry| entry.table.as_str())
        .collect();
    assert!(!left_behind.contains(&"library_compat_id_map"));

    // Nothing moves on a second pass or a repeat import.
    let settled = stored(&db_path);
    assert_eq!(ids.fill().pass().await.expect("second pass").rows, 0);
    import(&root, &export_path).await;
    assert_eq!(stored(&db_path), settled);

    // With the fill run first, the track keeps the id it has and the carried
    // one lands as an alias.
    let db = Connection::open(&db_path).expect("v3 db");
    db.execute_batch(
        "DELETE FROM compat_jellyfin_ids; \
         DELETE FROM import_progress WHERE section = 'jellyfin_id_carry';",
    )
    .expect("forgets the carry");
    drop(db);
    process(&db_path).await;
    let (again, _) = import(&root, &export_path).await;
    assert!(
        again
            .items
            .iter()
            .any(|item| item.entity == ENTITY && item.outcome == "landed_as_alias"),
        "{:?}",
        again.items
    );
    let ids = process(&db_path).await;
    assert_eq!(
        ids.handed_out("track", V2_TRACK_ID),
        derive("track", V2_TRACK_ID)
    );
    assert_eq!(opens(&ids, CARRIED, true).await, track(V2_TRACK_ID));
    assert_eq!(opens(&ids, CARRIED, false).await, None);
}
