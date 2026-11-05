//! Schema: the baseline and the runner, the schema conventions, the
//! declared statements, and the rule for rebuilding a referenced table.
//! The scratch databases live in memory, except where a test needs a file.

use std::collections::BTreeMap;
use std::time::Instant;

use droppedneedle_kernel::db::statement::{Declared, Schema, StatementFile};
use droppedneedle_kernel::db::{Chain, SchemaError, migrate, migrate_file};
use droppedneedle_kernel::schema::{BRIDGE_FROM, CHAIN, MIGRATIONS, latest_version};
use regex::Regex;
use rusqlite::Connection;

use crate::common::ScratchDir;
use crate::common::sources::workspace_sources;

/// A fresh database from the baseline, in memory.
fn migrated() -> Connection {
    migrate(Connection::open_in_memory().unwrap(), &CHAIN).unwrap()
}

/// One number a statement answers.
fn scalar(connection: &Connection, sql: &str) -> i64 {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// The text values one column of a statement answers, in order.
fn texts(connection: &Connection, sql: &str, params: impl rusqlite::Params) -> Vec<String> {
    connection
        .prepare(sql)
        .unwrap()
        .query_map(params, |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn table_names(connection: &Connection) -> Vec<String> {
    texts(
        connection,
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' ORDER BY name",
        [],
    )
}

fn column_names(connection: &Connection, table: &str) -> Vec<String> {
    texts(
        connection,
        "SELECT name FROM pragma_table_info(?1) ORDER BY cid",
        [table],
    )
}

/// Every `sqlite_master` row, sorted.
fn schema_rows(connection: &Connection) -> Vec<(String, String, String, Option<String>)> {
    connection
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY 1, 2")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A fresh database stamps the latest version and holds the singleton and
/// sentinel rows the services expect to exist. Running the migrations again
/// is a clean no-op: version stable, seeds single.
#[test]
fn fresh_migrate_marks_version_and_seeds() {
    let connection = migrated();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), latest_version());
    assert_eq!(table_names(&connection).len(), 175);
    assert_eq!(
        scalar(
            &connection,
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'"
        ),
        73
    );

    let catalog: (i64, i64) = connection
        .query_row(
            "SELECT singleton, value FROM library_catalog_revision",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(catalog, (1, 1));
    let watermark: (i64, i64, String) = connection
        .query_row(
            "SELECT singleton, identified_at, identified_job_id FROM library_repair_watermarks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(watermark, (1, 0, String::new()));
    assert_eq!(
        texts(
            &connection,
            "SELECT id || ' ' || kind FROM local_artists ORDER BY id",
            []
        ),
        [
            "00000000-0000-4000-8000-000000000001 various_artists",
            "00000000-0000-4000-8000-000000000002 unknown",
        ]
    );
    for (table, want) in [
        ("library_event_stream_revisions", 3),
        ("download_activity_global_revision", 1),
    ] {
        assert_eq!(
            scalar(&connection, &format!("SELECT COUNT(*) FROM {table}")),
            want,
            "seed rows in {table}"
        );
    }

    let connection = migrate(connection, &CHAIN).unwrap();
    assert_eq!(
        scalar(&connection, "PRAGMA user_version"),
        latest_version(),
        "rerun"
    );
    assert_eq!(
        scalar(&connection, "SELECT COUNT(*) FROM local_artists"),
        2,
        "rerun"
    );
}

/// Dropped tables stay dropped: no lidarr marker, no rebuild leftovers, no
/// legacy catalog tables, no transient swap tables, no `_sqlx_migrations`.
#[test]
fn legacy_and_swap_tables_are_absent() {
    let tables = table_names(&migrated());

    for name in [
        "_sqlx_migrations",
        "download_quarantine_legacy",
        "download_attempts_new",
        "youtube_links_old",
        "youtube_track_links_old",
        "ignored_releases_legacy",
        "cache_meta",
        "library_artists",
        "library_albums",
        "library_files",
        "manual_review_queue",
        "library_album_meta",
    ] {
        assert!(!tables.contains(&name.to_owned()), "{name} must be absent");
    }
    for name in &tables {
        assert!(
            !name.ends_with("__management_v1")
                && !name.ends_with("__edition_tier_v1")
                && !name.ends_with("__resolved_v1")
                && !name.ends_with("_new"),
            "{name} is a transient rebuild name"
        );
    }
    assert!(
        !tables.iter().any(|name| name.contains("lidarr")),
        "no lidarr tombstone"
    );
}

/// Each dual-owner table exists once with the merged shape: the store side
/// won on auth FKs, CHECKs merged, ratchets folded in.
#[test]
fn dual_owner_tables_carry_the_merged_shape() {
    let connection = migrated();

    for (table, columns) in [
        (
            "catalog_playlists",
            vec!["source_ref", "user_id", "is_public"],
        ),
        ("auth_users", vec!["username", "username_display"]),
        ("auth_tokens", vec!["last_seen_at", "session_kind"]),
        ("local_albums", vec!["management_schedule_pending"]),
        (
            "local_tracks",
            vec!["release_type", "tag_album_title", "tag_album_artist_name"],
        ),
        (
            "library_identity_repair_findings",
            vec![
                "suggested_release_mbid",
                "suggested_release_group_mbid",
                "suggested_edition_json",
            ],
        ),
        (
            "library_scan_runs",
            vec!["phase_started_at", "phase_timings_json"],
        ),
        ("user_listening_prefs", vec!["auto_request_personal_mix"]),
        (
            "request_history",
            vec!["dispatch_authorized", "generation", "request_kind"],
        ),
    ] {
        let have = column_names(&connection, table);
        for column in columns {
            assert!(
                have.contains(&column.to_owned()),
                "{table}.{column} missing"
            );
        }
    }
    for table in [
        "local_album_external_identities",
        "local_track_external_identities",
    ] {
        assert!(
            column_names(&connection, table).contains(&"provider_base_url".to_owned()),
            "{table}.provider_base_url missing"
        );
    }

    // Store-side auth FKs won the merge.
    for table in ["compat_bookmarks", "compat_play_queues"] {
        let parents = texts(
            &connection,
            "SELECT \"table\" FROM pragma_foreign_key_list(?1)",
            [table],
        );
        assert!(
            parents.iter().any(|parent| parent == "auth_users"),
            "{table}.user_id must reference auth_users"
        );
    }

    // Merged revision row: discovery DEFAULT 0 plus native range CHECK.
    let default: Option<String> = connection
        .query_row(
            "SELECT dflt_value FROM pragma_table_info('library_catalog_revision') \
             WHERE name = 'value'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(default.as_deref(), Some("0"));
    let sql = &texts(
        &connection,
        "SELECT sql FROM sqlite_master WHERE name = 'library_catalog_revision'",
        [],
    )[0];
    assert!(sql.contains("BETWEEN 0 AND 9223372036854775807"), "{sql}");

    // Per-user playlist uniqueness survived the merge.
    let index_sql = &texts(
        &connection,
        "SELECT sql FROM sqlite_master WHERE name = 'idx_catalog_playlists_user_source_ref'",
        [],
    )[0];
    assert!(index_sql.contains("UNIQUE"), "{index_sql}");
}

/// Declared FKs bite under enforcement: orphans fail, cascades clean up.
#[test]
fn foreign_keys_bite_under_enforcement() {
    let connection = migrated();
    connection
        .execute_batch("PRAGMA foreign_keys = ON")
        .unwrap();

    let orphan = connection.execute(
        "INSERT INTO compat_bookmarks (user_id, file_id, position_ms, created_at, changed_at) \
         VALUES ('ghost', 'f1', 0, 1, 1)",
        [],
    );
    assert!(orphan.is_err(), "orphan row must fail with FKs on");

    connection
        .execute_batch(
            "INSERT INTO auth_users (id, display_name, created_at) VALUES ('u1', 'T', 0);
             INSERT INTO compat_bookmarks (user_id, file_id, position_ms, created_at, changed_at)
                 VALUES ('u1', 'f1', 0, 1, 1);
             DELETE FROM auth_users WHERE id = 'u1';",
        )
        .unwrap();
    assert_eq!(
        scalar(&connection, "SELECT COUNT(*) FROM compat_bookmarks"),
        0,
        "bookmarks cascade with their user"
    );
}

/// SQLite is built without its memory counters (`.cargo/config.toml`). Lost
/// silently, every parallel test queues on their one mutex.
#[test]
fn sqlite_is_built_without_memory_counters() {
    let connection = Connection::open_in_memory().unwrap();
    let options = texts(&connection, "PRAGMA compile_options", []);
    assert!(
        options.iter().any(|option| option == "DEFAULT_MEMSTATUS=0"),
        "{options:?}"
    );
}

/// The SHA-256 of every file in `server/migrations/`. A file on `v3` is
/// never edited, renumbered or removed: a schema change is a new file, and
/// a new file adds its line here.
const SHIPPED: &[(&str, &str)] = &[
    (
        "0135_baseline.sql",
        "f6ce70cd4b72a35a9a63cd4703e7eee02223b1e698c90e486d70ede20bbe5e23",
    ),
    (
        "0136_acquire_held_reasons.sql",
        "52c71063cc661dc3c72195099a8c4ba31cfb430e3d25030a0ad12824483c30f0",
    ),
    (
        "0137_library_scan_indexes.sql",
        "5fe5a1867a748067bc2576d474c08c9eb87a587125d58f88e3f01a1fbfd31a84",
    ),
    (
        "0138_lookup_indexes.sql",
        "13b2851a45637374ae7110ac736df4af04c4df675c873ad71dbaad14b35bab5d",
    ),
    (
        "0139_library_album_listing_revision.sql",
        "69eb3c40823acb9fb3587426ccd6c5a5e488c80e1c92ee95d367e830ac24b085",
    ),
    (
        "0140_artist_join_undo.sql",
        "f3fc8b836d93a8189b46d92fd63eb010ec44d69dc084a0bebf1f498fd0d826c2",
    ),
];

/// The files in `server/migrations/`, by name.
fn migration_files() -> Vec<(String, Vec<u8>)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

/// Each shipped migration file still has the bytes it shipped with.
#[test]
fn shipped_migrations_keep_their_bytes() {
    use sha2::{Digest as _, Sha256};
    let found: Vec<(String, String)> = migration_files()
        .into_iter()
        .map(|(name, bytes)| {
            let digest = Sha256::digest(&bytes);
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            (name, hex)
        })
        .collect();
    let pinned: Vec<(String, String)> = SHIPPED
        .iter()
        .map(|(name, hash)| ((*name).to_owned(), (*hash).to_owned()))
        .collect();
    assert_eq!(found, pinned);
}

/// Every file in `server/migrations/` is in the runner's list, in number
/// order, under its own number, and stamps that number, so applying them
/// in order on an empty database ends at the latest version.
#[test]
fn migrations_stamp_their_own_version_in_order() {
    let names: Vec<String> = migration_files()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let listed: Vec<&str> = MIGRATIONS.iter().map(|migration| migration.name).collect();
    assert_eq!(names, listed, "the runner's list and the folder agree");
    let connection = Connection::open_in_memory().unwrap();
    let mut last = 0;
    for migration in MIGRATIONS {
        assert!(
            migration.version > last,
            "{} is out of order",
            migration.name
        );
        last = migration.version;
        assert_eq!(
            migration.name.split('_').next(),
            Some(format!("{:04}", migration.version).as_str()),
            "{} is listed under another number",
            migration.name
        );
        connection.execute_batch(migration.sql).unwrap();
        assert_eq!(
            scalar(&connection, "PRAGMA user_version"),
            migration.version,
            "{}",
            migration.name
        );
    }
    assert_eq!(last, latest_version());
}

/// Every stamp the runner can meet, on a copy of a fresh database restamped:
/// an empty file takes the baseline; a database the old chain left at its
/// last number (the baseline's schema) crosses the bridge, loses
/// `_sqlx_migrations`, takes the later files and ends equal to a fresh one;
/// an older stamp and one newer than the binary are refused with the file
/// unchanged; and a second run changes nothing.
#[test]
fn the_runner_takes_each_stamp_it_can_meet() {
    let dir = ScratchDir::new("runner-stamps");
    let fresh = dir.join("fresh.db");
    migrate_file(&fresh, &CHAIN).unwrap();
    let read = |path: &std::path::Path| Connection::open(path).unwrap();
    assert_eq!(
        scalar(&read(&fresh), "PRAGMA user_version"),
        latest_version(),
        "an empty file takes the baseline"
    );
    let fresh_schema = schema_rows(&read(&fresh));
    // The old chain's last schema is the baseline's.
    let baseline = dir.join("baseline.db");
    migrate_file(&baseline, &Chain::new(&MIGRATIONS[..1], None)).unwrap();

    let restamped = |name: &str, stamp: i64, bookkeeping: bool| {
        let path = dir.join(name);
        std::fs::copy(if bookkeeping { &baseline } else { &fresh }, &path).unwrap();
        let connection = read(&path);
        if bookkeeping {
            // The old chain's bookkeeping table `_sqlx_migrations`, as its
            // runner created it.
            connection
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS _sqlx_migrations (
                        version BIGINT PRIMARY KEY,
                        description TEXT NOT NULL,
                        installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                        success BOOLEAN NOT NULL,
                        checksum BLOB NOT NULL,
                        execution_time BIGINT NOT NULL
                    );
                    INSERT INTO _sqlx_migrations (version, description, success, checksum,
                        execution_time) VALUES (134, 'loop states', TRUE, X'00', 1);",
                )
                .unwrap();
        }
        connection
            .pragma_update(None, "user_version", stamp)
            .unwrap();
        drop(connection);
        path
    };

    let bridged = restamped("bridged.db", BRIDGE_FROM, true);
    migrate_file(&bridged, &CHAIN).unwrap();
    let connection = read(&bridged);
    assert_eq!(scalar(&connection, "PRAGMA user_version"), latest_version());
    assert_eq!(
        schema_rows(&connection),
        fresh_schema,
        "the bridge drops `_sqlx_migrations` and leaves the schema of a fresh database"
    );
    drop(connection);

    for (name, stamp) in [
        ("older.db", BRIDGE_FROM - 1),
        ("newer.db", latest_version() + 1),
    ] {
        let path = restamped(name, stamp, false);
        let before = std::fs::read(&path).unwrap();
        let refused = migrate_file(&path, &CHAIN).unwrap_err();
        match (stamp > latest_version(), &refused) {
            (false, droppedneedle_kernel::db::DbError::Schema(SchemaError::TooOld { found })) => {
                assert_eq!(*found, stamp)
            }
            (
                true,
                droppedneedle_kernel::db::DbError::Schema(SchemaError::TooNew { found, latest }),
            ) => assert_eq!((*found, *latest), (stamp, latest_version())),
            _ => panic!("stamp {stamp}: {refused:?}"),
        }
        assert_eq!(std::fs::read(&path).unwrap(), before, "{name} is unchanged");
    }

    let before = std::fs::read(&fresh).unwrap();
    migrate_file(&fresh, &CHAIN).unwrap();
    assert_eq!(
        std::fs::read(&fresh).unwrap(),
        before,
        "a second run is a no-op"
    );
}

/// 0136 narrows the held-files reason list. A held row keeps its id and
/// every column, the rest of the schema is unchanged, and the narrowed list
/// refuses a download-only reason. A row holding a reason outside the list
/// is kept and reads as held by an earlier version.
#[test]
fn held_reasons_narrow_and_every_held_row_is_kept() {
    let dir = ScratchDir::new("held-reasons");
    let baseline = Chain::new(&MIGRATIONS[..1], None);
    let to_136 = Chain::new(&MIGRATIONS[..2], None);
    let held_at_135 = |name: &str, reason: &str| {
        let path = dir.join(name);
        let connection = migrate(Connection::open(&path).unwrap(), &baseline).unwrap();
        connection
            .execute_batch(
                "INSERT INTO auth_users (id, display_name, created_at) VALUES ('u1', 'U', 1);
                 INSERT INTO acquire_held_entries (id, user_id, origin, group_key, state,
                     created_at, expires_at) VALUES (7, 'u1', 'download', 'album:rg', 'held',
                     1, 2);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO acquire_held_files (id, entry_id, root, path, original_name,
                     track_title, source, source_task_id, reason, reason_detail, state,
                     created_at) VALUES (42, 7, 'held', 'a/01.flac', '01.flac', 'One',
                     'soulseek', 't1', ?1, 'why', 'held', 1000)",
                [reason],
            )
            .unwrap();
        drop(connection);
        path
    };
    let row = |connection: &Connection| -> Vec<String> {
        connection
            .query_row(
                "SELECT id, entry_id, root, path, original_name, track_title, source,
                     source_task_id, reason, reason_detail, state, created_at
                 FROM acquire_held_files",
                [],
                |row| {
                    (0..12)
                        .map(|at| Ok(format!("{:?}", row.get_ref(at)?)))
                        .collect()
                },
            )
            .unwrap()
    };
    let others = |connection: &Connection| {
        let mut rows = schema_rows(connection);
        rows.retain(|(kind, name, _, _)| !(kind == "table" && name == "acquire_held_files"));
        rows
    };

    let kept = held_at_135("kept.db", "weak_match");
    let connection = Connection::open(&kept).unwrap();
    let (before_row, before_schema) = (row(&connection), others(&connection));
    drop(connection);
    migrate_file(&kept, &to_136).unwrap();
    let connection = Connection::open(&kept).unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 136);
    assert_eq!(
        row(&connection),
        before_row,
        "the held row is kept as it was"
    );
    assert_eq!(
        others(&connection),
        before_schema,
        "indexes and triggers come back"
    );
    let refused = connection.execute(
        "UPDATE acquire_held_files SET reason = 'no_better_copy' WHERE id = 42",
        [],
    );
    assert!(refused.is_err(), "a download-only reason is no held reason");
    drop(connection);

    let outside = held_at_135("outside.db", "corrupt");
    migrate_file(&outside, &to_136).unwrap();
    let connection = Connection::open(&outside).unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 136);
    assert_eq!(
        texts(
            &connection,
            "SELECT reason || ':' || path FROM acquire_held_files",
            []
        ),
        ["held_by_earlier_version:a/01.flac"]
    );
}

/// 0137 makes the retired-albums index partial (only retired rows) and
/// indexes a run's album moves by track. Rows are untouched and nothing
/// else in the schema changes.
#[test]
fn scan_indexes_upgrade_and_keep_every_row() {
    let dir = ScratchDir::new("scan-indexes");
    let path = dir.join("app.db");
    let connection = migrate(
        Connection::open(&path).unwrap(),
        &Chain::new(&MIGRATIONS[..2], None),
    )
    .unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 136);
    connection
        .execute_batch(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
                 album_artist_id, grouping_source, retired_into_album_id, created_at,
                 updated_at)
             VALUES ('live', 'r', 'k1', 'A', 'a', '00000000-0000-4000-8000-000000000002',
                 'automatic', NULL, 1, 1),
                ('gone', 'r', 'k2', 'B', 'b', '00000000-0000-4000-8000-000000000002',
                 'automatic', 'live', 2, 2);
             INSERT INTO library_scan_runs (id, kind, trigger, state, phase, aggregate_scope,
                 queued_at, updated_at)
             VALUES ('run', 'incremental', 'manual', 'completed', 'reconciling', '[]', 1, 1);
             INSERT INTO library_scan_album_moves VALUES ('run', 'r', 'a/01.flac', 't1', 'gone');",
        )
        .unwrap();
    let rows = |connection: &Connection| {
        texts(
            connection,
            "SELECT id || ':' || COALESCE(retired_into_album_id, '-') FROM local_albums
             UNION ALL SELECT run_id || ':' || relative_path || ':' || local_track_id || ':'
                 || previous_album_id FROM library_scan_album_moves ORDER BY 1",
            [],
        )
    };
    let changed = [
        "idx_local_albums_retired",
        "idx_library_scan_album_moves_track",
    ];
    let others = |connection: &Connection| {
        let mut rows = schema_rows(connection);
        rows.retain(|(_, name, _, _)| !changed.contains(&name.as_str()));
        rows
    };
    let (before_rows, before_schema) = (rows(&connection), others(&connection));
    drop(connection);

    migrate_file(&path, &Chain::new(&MIGRATIONS[..3], None)).unwrap();
    let connection = Connection::open(&path).unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 137);
    assert_eq!(rows(&connection), before_rows, "rows are untouched");
    assert_eq!(others(&connection), before_schema, "nothing else changes");
    let shape = |index: &str| {
        let partial = texts(
            &connection,
            "SELECT tbl_name || ' ' || partial FROM sqlite_master
             JOIN pragma_index_list(tbl_name) ON pragma_index_list.name = sqlite_master.name
             WHERE sqlite_master.name = ?1",
            [index],
        );
        let columns = texts(
            &connection,
            "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
            [index],
        );
        (partial, columns)
    };
    assert_eq!(
        shape("idx_local_albums_retired"),
        (
            vec!["local_albums 1".to_owned()],
            vec!["retired_into_album_id".to_owned()]
        )
    );
    assert_eq!(
        texts(
            &connection,
            "SELECT id FROM local_albums INDEXED BY idx_local_albums_retired
             WHERE retired_into_album_id = 'live'",
            []
        ),
        ["gone"],
        "the partial index holds the retired rows"
    );
    assert_eq!(
        shape("idx_library_scan_album_moves_track"),
        (
            vec!["library_scan_album_moves 0".to_owned()],
            vec!["run_id".to_owned(), "local_track_id".to_owned()]
        )
    );
}

/// 0138 indexes favourites and their names by item, indexes the blocklist
/// by release group, drops the favourites index the primary key repeats
/// and drops the YouTube link's stored track count. Every row keeps its
/// other values and nothing else in the schema changes.
#[test]
fn lookup_indexes_upgrade_and_keep_every_row() {
    let dir = ScratchDir::new("lookup-indexes");
    let path = dir.join("app.db");
    let connection = migrate(
        Connection::open(&path).unwrap(),
        &Chain::new(&MIGRATIONS[..3], None),
    )
    .unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 137);
    connection
        .execute_batch(
            "INSERT INTO auth_users (id, display_name, created_at) VALUES ('u1', 'U', 1);
             INSERT INTO catalog_user_favorites VALUES ('u1', 'track', 't1', 5);
             INSERT INTO catalog_user_favorite_names VALUES ('u1', 'track', 't1', 'One');
             INSERT INTO download_quarantine (source, identity, release_group_mbid, reason,
                 quarantined_at) VALUES ('soulseek', 'peer/a', 'rg1', 'corrupt', 7);
             INSERT INTO catalog_youtube_links (album_id, video_id, album_name, artist_name,
                 embed_url, cover_url, created_at, is_manual, track_count)
             VALUES ('rg1', 'v1', 'A', 'B', 'https://e/a', NULL, 9, 1, 4);",
        )
        .unwrap();
    let rows = |connection: &Connection| {
        texts(
            connection,
            "SELECT user_id || ':' || item_kind || ':' || item_id || ':' || created_at
                 FROM catalog_user_favorites
             UNION ALL SELECT user_id || ':' || item_id || ':' || display_name
                 FROM catalog_user_favorite_names
             UNION ALL SELECT id || ':' || source || ':' || identity || ':'
                 || release_group_mbid || ':' || reason || ':' || quarantined_at
                 FROM download_quarantine
             UNION ALL SELECT album_id || ':' || video_id || ':' || album_name || ':'
                 || artist_name || ':' || embed_url || ':' || COALESCE(cover_url, '-') || ':'
                 || created_at || ':' || is_manual FROM catalog_youtube_links
             ORDER BY 1",
            [],
        )
    };
    let changed = [
        "idx_catalog_user_favorites_item",
        "idx_catalog_user_favorite_names_item",
        "idx_catalog_user_favorites_user_kind",
        "idx_download_quarantine_release_group",
        "catalog_youtube_links",
    ];
    let others = |connection: &Connection| {
        let mut rows = schema_rows(connection);
        rows.retain(|(_, name, _, _)| !changed.contains(&name.as_str()));
        rows
    };
    let link_columns = |connection: &Connection| {
        texts(
            connection,
            "SELECT name FROM pragma_table_info('catalog_youtube_links') ORDER BY cid",
            [],
        )
    };
    let (before_rows, before_schema, mut columns) = (
        rows(&connection),
        others(&connection),
        link_columns(&connection),
    );
    drop(connection);

    migrate_file(&path, &Chain::new(&MIGRATIONS[..4], None)).unwrap();
    let connection = Connection::open(&path).unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 138);
    assert_eq!(rows(&connection), before_rows, "rows are untouched");
    assert_eq!(others(&connection), before_schema, "nothing else changes");
    columns.retain(|name| name != "track_count");
    assert_eq!(
        link_columns(&connection),
        columns,
        "only the track count goes"
    );
    let index_columns = |index: &str| {
        texts(
            &connection,
            "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
            [index],
        )
    };
    assert_eq!(
        index_columns("idx_catalog_user_favorites_item"),
        ["item_kind", "item_id"]
    );
    assert_eq!(
        index_columns("idx_catalog_user_favorite_names_item"),
        ["item_kind", "item_id"]
    );
    assert_eq!(
        index_columns("idx_download_quarantine_release_group"),
        ["release_group_mbid"]
    );
    assert!(
        index_columns("idx_catalog_user_favorites_user_kind").is_empty(),
        "the repeated index is gone"
    );
}

/// 0139 adds an album's listing revision, at 0 on every carried row, and
/// moves it whenever the album is renamed or gains or loses a listed track:
/// a track added, deleted, moved to another album, or listed or unlisted.
/// A write that changes no listing moves nothing, the row revision never
/// moves, and the upgrade itself leaves every row as it was.
#[test]
fn album_listing_revision_upgrade_moves_on_listing_changes() {
    let dir = ScratchDir::new("album-listing-revision");
    let path = dir.join("app.db");
    let connection = migrate(
        Connection::open(&path).unwrap(),
        &Chain::new(&MIGRATIONS[..4], None),
    )
    .unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 138);
    connection
        .execute_batch(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
                 album_artist_id, grouping_source, created_at, updated_at, row_revision)
             VALUES ('a', 'r', 'k1', 'A', 'a', '00000000-0000-4000-8000-000000000002',
                 'automatic', 1, 1, 4),
                ('b', 'r', 'k2', 'B', 'b', '00000000-0000-4000-8000-000000000002',
                 'automatic', 1, 1, 7);",
        )
        .unwrap();
    let track = |connection: &Connection, id: &str, album: &str, availability: &str| {
        connection
            .execute(
                "INSERT INTO local_tracks (id, local_album_id, root_id, file_path,
                     relative_path, path_hash, file_size_bytes, file_mtime_ns,
                     stat_revision, title, title_folded, album_title, album_title_folded,
                     file_format, ingest_source, imported_at, membership_source,
                     availability)
                 VALUES (?1, ?2, 'r', '/m/' || ?1, ?1, ?1, 1, 1, 's', 'T', 't', 'A', 'a',
                     'flac', 'scan', 1, 'automatic', ?3)",
                [id, album, availability],
            )
            .unwrap();
    };
    track(&connection, "t1", "a", "indexed");
    let albums = |connection: &Connection| {
        texts(
            connection,
            "SELECT id || ':' || row_revision || ':' || title FROM local_albums ORDER BY id",
            [],
        )
    };
    let tracks = |connection: &Connection| {
        texts(
            connection,
            "SELECT id || ':' || local_album_id || ':' || availability || ':' || row_revision
             FROM local_tracks ORDER BY id",
            [],
        )
    };
    let (before_albums, before_tracks) = (albums(&connection), tracks(&connection));
    drop(connection);

    migrate_file(&path, &Chain::new(&MIGRATIONS[..5], None)).unwrap();
    let connection = Connection::open(&path).unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 139);
    assert_eq!(albums(&connection), before_albums, "rows are untouched");
    assert_eq!(tracks(&connection), before_tracks, "rows are untouched");
    let revisions = |connection: &Connection| {
        texts(
            connection,
            "SELECT id || ':' || row_revision || ':' || listing_revision
             FROM local_albums ORDER BY id",
            [],
        )
    };
    assert_eq!(revisions(&connection), ["a:4:0", "b:7:0"], "added at 0");

    let after = |connection: &Connection, case: &str, write: &str, expected: [&str; 2]| {
        connection.execute_batch(write).unwrap();
        assert_eq!(revisions(connection), expected, "{case}");
    };
    track(&connection, "t2", "a", "indexed");
    assert_eq!(
        revisions(&connection),
        ["a:4:1", "b:7:0"],
        "a listed track joins"
    );
    track(&connection, "t3", "b", "missing");
    assert_eq!(
        revisions(&connection),
        ["a:4:1", "b:7:0"],
        "an unlisted row lists nothing"
    );
    after(
        &connection,
        "the same values list nothing new",
        "UPDATE local_tracks SET local_album_id = 'a', availability = 'indexed', title = 'U'
         WHERE id = 't1';
         UPDATE local_albums SET title = 'A', album_artist_name = NULL, year = 1999
         WHERE id = 'a'",
        ["a:4:1", "b:7:0"],
    );
    after(
        &connection,
        "renamed",
        "UPDATE local_albums SET title = 'A2' WHERE id = 'a'",
        ["a:4:2", "b:7:0"],
    );
    after(
        &connection,
        "a new album artist name",
        "UPDATE local_albums SET album_artist_name = 'X' WHERE id = 'b'",
        ["a:4:2", "b:7:1"],
    );
    after(
        &connection,
        "a listed track moves",
        "UPDATE local_tracks SET local_album_id = 'b' WHERE id = 't2'",
        ["a:4:3", "b:7:2"],
    );
    after(
        &connection,
        "an unlisted track moves",
        "UPDATE local_tracks SET local_album_id = 'a' WHERE id = 't3'",
        ["a:4:3", "b:7:2"],
    );
    after(
        &connection,
        "unlisted between two states",
        "UPDATE local_tracks SET availability = 'excluded' WHERE id = 't3'",
        ["a:4:3", "b:7:2"],
    );
    after(
        &connection,
        "listed again",
        "UPDATE local_tracks SET availability = 'indexed' WHERE id = 't3'",
        ["a:4:4", "b:7:2"],
    );
    after(
        &connection,
        "unlisted",
        "UPDATE local_tracks SET availability = 'missing' WHERE id = 't1'",
        ["a:4:5", "b:7:2"],
    );
    after(
        &connection,
        "an unlisted row deleted",
        "DELETE FROM local_tracks WHERE id = 't1'",
        ["a:4:5", "b:7:2"],
    );
    after(
        &connection,
        "a listed row deleted",
        "DELETE FROM local_tracks WHERE id = 't2'",
        ["a:4:5", "b:7:3"],
    );
}

/// 0140 rebuilds the catalog action log to take the undo of an artist
/// join, keeping every row, id and rowid and every index, and gives artist
/// id checks a disambiguation column, empty on carried rows. Nothing else
/// changes.
#[test]
fn artist_join_undo_upgrade_keeps_the_action_log() {
    const ARTIST: &str = "00000000-0000-4000-8000-000000000002";
    let dir = ScratchDir::new("artist-join-undo");
    let path = dir.join("app.db");
    let connection = migrate(
        Connection::open(&path).unwrap(),
        &Chain::new(&MIGRATIONS[..5], None),
    )
    .unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 139);
    connection
        .execute_batch(&format!(
            "INSERT INTO library_catalog_actions (id, idempotency_key, action_kind,
                 local_artist_id, before_json, after_json, reason_code, created_at)
             VALUES ('m', NULL, 'merge_artist', '{ARTIST}', '{{}}', '{{\"kind\":\"x\"}}',
                 'AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE', 5),
                ('p', 'put-back:1', 'files_put_back', '{ARTIST}', '{{\"undoes\":\"m\"}}',
                 '{{}}', NULL, 6),
                ('l', NULL, 'artist_link_set', '{ARTIST}', '{{}}', '{{}}', 'R', 7);
             DELETE FROM library_catalog_actions WHERE id = 'l';
             INSERT INTO library_catalog_actions (id, action_kind, local_artist_id,
                 before_json, after_json, created_at)
             VALUES ('r', 'review_approve', '{ARTIST}', '{{}}', '{{}}', 8);
             INSERT INTO library_identify_artist_id_checks (artist_mbid, state,
                 nominated_at, checked_at)
             VALUES ('aaaaaaaa-0000-4000-8000-000000000001', 'pending', 1, NULL),
                ('aaaaaaaa-0000-4000-8000-000000000002', 'kept', 1, 2);"
        ))
        .unwrap();
    let actions = |connection: &Connection| {
        texts(
            connection,
            "SELECT rowid || ':' || id || ':' || COALESCE(idempotency_key, '-') || ':'
                 || action_kind || ':' || local_artist_id || ':' || before_json || ':'
                 || after_json || ':' || COALESCE(reason_code, '-') || ':' || created_at
             FROM library_catalog_actions ORDER BY rowid",
            [],
        )
    };
    let checks = |connection: &Connection| {
        texts(
            connection,
            "SELECT artist_mbid || ':' || state || ':' || nominated_at || ':'
                 || COALESCE(checked_at, '-') FROM library_identify_artist_id_checks
             ORDER BY artist_mbid",
            [],
        )
    };
    let changed = [
        "library_catalog_actions",
        "library_identify_artist_id_checks",
    ];
    let others = |connection: &Connection| {
        let mut rows = schema_rows(connection);
        rows.retain(|(_, _, table, _)| !changed.contains(&table.as_str()));
        rows
    };
    let indexes = |connection: &Connection| {
        texts(
            connection,
            "SELECT name FROM sqlite_master WHERE type = 'index'
                 AND tbl_name = 'library_catalog_actions' ORDER BY name",
            [],
        )
    };
    let (before_actions, before_checks, before_schema, before_indexes) = (
        actions(&connection),
        checks(&connection),
        others(&connection),
        indexes(&connection),
    );
    assert_eq!(before_actions.len(), 3);
    drop(connection);

    migrate_file(&path, &Chain::new(&MIGRATIONS[..6], None)).unwrap();
    let connection = Connection::open(&path).unwrap();
    assert_eq!(scalar(&connection, "PRAGMA user_version"), 140);
    assert_eq!(
        actions(&connection),
        before_actions,
        "every row and rowid kept"
    );
    assert_eq!(checks(&connection), before_checks, "checks kept");
    assert_eq!(others(&connection), before_schema, "nothing else changes");
    assert_eq!(indexes(&connection), before_indexes, "every index is back");
    assert_eq!(
        scalar(
            &connection,
            "SELECT COUNT(*) FROM library_catalog_actions
             WHERE json_extract(before_json, '$.undoes') = 'm'
             AND action_kind = 'files_put_back'"
        ),
        1
    );
    assert_eq!(
        scalar(
            &connection,
            "SELECT COUNT(*) FROM library_identify_artist_id_checks
             WHERE disambiguation IS NULL"
        ),
        2,
        "carried checks have no note"
    );
    connection
        .execute(
            "INSERT INTO library_catalog_actions (id, idempotency_key, action_kind,
                 local_artist_id, before_json, after_json, created_at)
             VALUES ('u', 'merge-artist-undone:m', 'merge_artist_undone', ?1, '{}', '{}', 9)",
            [ARTIST],
        )
        .unwrap();
    assert!(
        connection
            .execute(
                "INSERT INTO library_catalog_actions (id, action_kind, local_artist_id,
                     before_json, after_json, created_at)
                 VALUES ('w', 'no_such_kind', ?1, '{}', '{}', 9)",
                [ARTIST],
            )
            .is_err(),
        "the kind check still holds"
    );
    assert!(
        connection
            .execute(
                "INSERT INTO library_catalog_actions (id, idempotency_key, action_kind,
                     local_artist_id, before_json, after_json, created_at)
                 VALUES ('v', 'merge-artist-undone:m', 'merge_artist_undone', ?1, '{}',
                     '{}', 9)",
                [ARTIST],
            )
            .is_err(),
        "an idempotency key is still unique"
    );
}

/// Every foreign key and what deleting its parent does. A rebuild that
/// drops or changes a key's action, or adds a key, fails here: the list
/// changes in the same commit, on purpose.
#[rustfmt::skip]
const FOREIGN_KEYS: &[(&str, &str, &str, &str)] = &[
    ("acquire_discovery_batch_items", "batch_id", "acquire_discovery_batches", "CASCADE"),
    ("acquire_discovery_batches", "user_id", "auth_users", "CASCADE"),
    ("acquire_edition_acquires", "task_id", "download_tasks", "CASCADE"),
    ("acquire_followed_artists", "user_id", "auth_users", "CASCADE"),
    ("acquire_held_entries", "drop_item_id", "drop_import_items", "CASCADE"),
    ("acquire_held_entries", "user_id", "auth_users", "CASCADE"),
    ("acquire_held_files", "entry_id", "acquire_held_entries", "CASCADE"),
    ("app_section_prefs", "user_id", "auth_users", "CASCADE"),
    ("auth_app_passwords", "user_id", "auth_users", "CASCADE"),
    ("auth_password_recovery_codes", "user_id", "auth_users", "CASCADE"),
    ("auth_providers", "user_id", "auth_users", "CASCADE"),
    ("auth_tokens", "user_id", "auth_users", "CASCADE"),
    ("auto_download_approvals", "reviewed_by_id", "auth_users", "SET NULL"),
    ("auto_download_approvals", "user_id", "auth_users", "CASCADE"),
    ("catalog_cached_pages", "user_id", "auth_users", "CASCADE"),
    ("catalog_discover_activity", "user_id", "auth_users", "CASCADE"),
    ("catalog_event_cities", "user_id", "auth_users", "CASCADE"),
    ("catalog_event_seen_marks", "user_id", "auth_users", "CASCADE"),
    ("catalog_ignored_releases", "user_id", "auth_users", "CASCADE"),
    ("catalog_new_release_seen_marks", "user_id", "auth_users", "CASCADE"),
    ("catalog_playlist_covers", "playlist_id", "catalog_playlists", "CASCADE"),
    ("catalog_playlist_tracks", "playlist_id", "catalog_playlists", "CASCADE"),
    ("catalog_playlists", "user_id", "auth_users", "CASCADE"),
    ("catalog_user_favorite_names", "user_id, item_kind, item_id", "catalog_user_favorites", "CASCADE"),
    ("catalog_user_favorites", "user_id", "auth_users", "CASCADE"),
    ("catalog_user_play_marks", "local_track_id", "local_tracks", "CASCADE"),
    ("catalog_user_play_marks", "user_id", "auth_users", "CASCADE"),
    ("catalog_user_ratings", "user_id", "auth_users", "CASCADE"),
    ("compat_bookmarks", "user_id", "auth_users", "CASCADE"),
    ("compat_play_queue_items", "user_id", "compat_play_queues", "CASCADE"),
    ("compat_play_queues", "user_id", "auth_users", "CASCADE"),
    ("compat_user_settings", "user_id", "auth_users", "CASCADE"),
    ("download_idempotency_keys", "task_id", "download_tasks", "CASCADE"),
    ("download_import_decisions", "attempt_id", "download_attempts", "SET NULL"),
    ("download_import_decisions", "task_id", "download_tasks", "CASCADE"),
    ("download_tasks", "user_id", "auth_users", "CASCADE"),
    ("drop_import_items", "job_id", "drop_import_jobs", "CASCADE"),
    ("library_album_departures", "local_track_id", "local_tracks", "CASCADE"),
    ("library_album_departures", "previous_album_id", "local_albums", "CASCADE"),
    ("library_album_looks_wrong", "local_album_id", "local_albums", "CASCADE"),
    ("library_album_looks_wrong", "user_id", "auth_users", "CASCADE"),
    ("library_album_match_state", "local_album_id", "local_albums", "CASCADE"),
    ("library_album_takeovers", "local_album_id", "local_albums", "CASCADE"),
    ("library_artist_album_credits", "local_album_id", "local_albums", "CASCADE"),
    ("library_artist_album_credits", "local_artist_id", "local_artists", "CASCADE"),
    ("library_artist_counts", "local_artist_id", "local_artists", "CASCADE"),
    ("library_artist_credit_proofs", "local_album_id", "local_albums", "CASCADE"),
    ("library_artist_credit_proofs", "local_artist_id", "local_artists", "RESTRICT"),
    ("library_artist_credit_proofs", "local_track_id", "local_tracks", "CASCADE"),
    ("library_artist_credit_proofs", "source_local_artist_id", "local_artists", "RESTRICT"),
    ("library_artist_reconciliation_dismissals", "dismissed_by_user_id", "auth_users", "RESTRICT"),
    ("library_artist_reconciliation_dismissals", "left_artist_id", "local_artists", "RESTRICT"),
    ("library_artist_reconciliation_dismissals", "right_artist_id", "local_artists", "RESTRICT"),
    ("library_artist_reconciliation_state", "local_album_id", "local_albums", "CASCADE"),
    ("library_audio_fingerprint_outcomes", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_automatic_edition_undo", "job_id", "library_operation_jobs", "SET NULL"),
    ("library_automatic_edition_undo", "local_album_id", "local_albums", "CASCADE"),
    ("library_catalog_actions", "actor_user_id", "auth_users", "SET NULL"),
    ("library_catalog_actions", "local_album_id", "local_albums", "RESTRICT"),
    ("library_catalog_actions", "local_artist_id", "local_artists", "RESTRICT"),
    ("library_catalog_actions", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_catalog_actions", "operation_job_id", "library_operation_jobs", "RESTRICT"),
    ("library_contribution_callback_tokens", "contribution_id", "library_contribution_drafts", "CASCADE"),
    ("library_contribution_callback_tokens", "requested_by_user_id", "auth_users", "CASCADE"),
    ("library_contribution_drafts", "created_by_user_id", "auth_users", "SET NULL"),
    ("library_contribution_drafts", "local_album_id", "local_albums", "RESTRICT"),
    ("library_contribution_drafts", "updated_by_user_id", "auth_users", "SET NULL"),
    ("library_custom_edition_active", "local_album_id", "local_albums", "RESTRICT"),
    ("library_custom_edition_active", "manifest_id", "library_custom_edition_manifests", "RESTRICT"),
    ("library_custom_edition_manifests", "local_album_id", "local_albums", "RESTRICT"),
    ("library_custom_edition_manifests", "sealed_by_user_id", "auth_users", "RESTRICT"),
    ("library_custom_edition_tracks", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_custom_edition_tracks", "manifest_id", "library_custom_edition_manifests", "RESTRICT"),
    ("library_edition_choice_undo", "local_album_id", "local_albums", "CASCADE"),
    ("library_edition_remap_queue", "chosen_by_user_id", "auth_users", "SET NULL"),
    ("library_edition_remap_queue", "local_album_id", "local_albums", "CASCADE"),
    ("library_identification_attempts", "local_album_id", "local_albums", "RESTRICT"),
    ("library_identification_attempts", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_identification_attempts", "requested_by_user_id", "auth_users", "SET NULL"),
    ("library_identify_jobs", "local_album_id", "local_albums", "CASCADE"),
    ("library_identify_jobs", "requested_by_user_id", "auth_users", "SET NULL"),
    ("library_identify_reviews", "local_album_id", "local_albums", "CASCADE"),
    ("library_identify_reviews", "resolved_by_user_id", "auth_users", "SET NULL"),
    ("library_identity_repair_findings", "job_id", "library_operation_jobs", "CASCADE"),
    ("library_identity_repair_findings", "local_album_id", "local_albums", "RESTRICT"),
    ("library_management_automatic_queue", "local_album_id", "local_albums", "CASCADE"),
    ("library_management_baseline_purges", "actor_user_id", "auth_users", "SET NULL"),
    ("library_management_baselines", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_management_baselines", "semantic_snapshot_blob_sha256", "library_management_blobs", "RESTRICT"),
    ("library_management_exclusions", "excluded_by_user_id", "auth_users", "SET NULL"),
    ("library_management_exclusions", "local_album_id", "local_albums", "RESTRICT"),
    ("library_management_job_snapshots", "job_id", "library_operation_jobs", "CASCADE"),
    ("library_management_job_snapshots", "linked_operation_job_id", "library_operation_jobs", "SET NULL"),
    ("library_management_overrides", "actor_user_id", "auth_users", "SET NULL"),
    ("library_management_overrides", "local_album_id", "local_albums", "RESTRICT"),
    ("library_management_overrides", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_management_plan_items", "job_id", "library_management_job_snapshots", "CASCADE"),
    ("library_management_plan_items", "local_album_id", "local_albums", "RESTRICT"),
    ("library_management_plan_items", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_operation_control_idempotency", "job_id", "library_operation_jobs", "CASCADE"),
    ("library_operation_jobs", "requested_by_user_id", "auth_users", "SET NULL"),
    ("library_operation_work", "job_id", "library_operation_jobs", "CASCADE"),
    ("library_operation_work", "local_album_id", "local_albums", "RESTRICT"),
    ("library_operation_work", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_play_history", "local_album_id", "local_albums", "RESTRICT"),
    ("library_play_history", "local_artist_id", "local_artists", "RESTRICT"),
    ("library_play_history", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_publish_created_folders", "bundle_id", "library_publish_operations", "CASCADE"),
    ("library_publish_operations", "applied_by_user_id", "auth_users", "SET NULL"),
    ("library_publish_outputs", "bundle_id", "library_publish_operations", "CASCADE"),
    ("library_publish_outputs", "journal_id", "library_publish_journal", "CASCADE"),
    ("library_publish_replacements", "journal_id", "library_publish_journal", "CASCADE"),
    ("library_publish_resolutions", "resolved_by_user_id", "auth_users", "SET NULL"),
    ("library_recycle_bin_empties", "actor_user_id", "auth_users", "SET NULL"),
    ("library_reidentification_snapshots", "job_id", "library_operation_jobs", "CASCADE"),
    ("library_reidentification_snapshots", "local_album_id", "local_albums", "RESTRICT"),
    ("library_repair_materialization", "job_id", "library_operation_jobs", "CASCADE"),
    ("library_repair_snapshots", "job_id", "library_operation_jobs", "CASCADE"),
    ("library_retag_pairs", "new_album_id", "local_albums", "CASCADE"),
    ("library_retag_pairs", "old_album_id", "local_albums", "CASCADE"),
    ("library_review_exclusions", "local_track_id", "local_tracks", "CASCADE"),
    ("library_review_exclusions", "review_id", "library_identify_reviews", "CASCADE"),
    ("library_scan_album_moves", "run_id", "library_scan_runs", "CASCADE"),
    ("library_scan_failures", "run_id", "library_scan_runs", "CASCADE"),
    ("library_scan_inventory", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_scan_inventory", "run_id", "library_scan_runs", "CASCADE"),
    ("library_scan_run_scopes", "run_id", "library_scan_runs", "CASCADE"),
    ("library_scan_runs", "requested_by_user_id", "auth_users", "SET NULL"),
    ("library_scan_unjudged_tracks", "local_track_id", "local_tracks", "CASCADE"),
    ("library_track_hand_matches", "local_track_id", "local_tracks", "CASCADE"),
    ("library_track_hand_matches", "matched_by_user_id", "auth_users", "SET NULL"),
    ("library_track_management_identity", "local_track_id", "local_tracks", "CASCADE"),
    ("library_track_management_state", "baseline_id", "library_management_baselines", "RESTRICT"),
    ("library_track_management_state", "last_operation_job_id", "library_operation_jobs", "RESTRICT"),
    ("library_track_management_state", "local_track_id", "local_tracks", "RESTRICT"),
    ("library_user_album_plays", "local_album_id", "local_albums", "CASCADE"),
    ("library_work_control", "requested_by_user_id", "auth_users", "SET NULL"),
    ("local_album_aliases", "local_album_id", "local_albums", "RESTRICT"),
    ("local_album_artists", "local_album_id", "local_albums", "RESTRICT"),
    ("local_album_artists", "local_artist_id", "local_artists", "RESTRICT"),
    ("local_album_artwork", "local_album_id", "local_albums", "RESTRICT"),
    ("local_album_artwork_checks", "local_album_id", "local_albums", "CASCADE"),
    ("local_album_external_identities", "attempt_id", "library_identification_attempts", "RESTRICT"),
    ("local_album_external_identities", "local_album_id", "local_albums", "RESTRICT"),
    ("local_album_external_identities", "selected_by_user_id", "auth_users", "SET NULL"),
    ("local_album_lyrics_checks", "local_album_id", "local_albums", "CASCADE"),
    ("local_albums", "album_artist_id", "local_artists", "RESTRICT"),
    ("local_albums", "retired_into_album_id", "local_albums", "RESTRICT"),
    ("local_artist_aliases", "local_artist_id", "local_artists", "RESTRICT"),
    ("local_artist_external_identities", "attempt_id", "library_identification_attempts", "RESTRICT"),
    ("local_artist_external_identities", "local_artist_id", "local_artists", "RESTRICT"),
    ("local_artist_external_identities", "selected_by_user_id", "auth_users", "SET NULL"),
    ("local_artist_merge_candidates", "left_artist_id", "local_artists", "RESTRICT"),
    ("local_artist_merge_candidates", "right_artist_id", "local_artists", "RESTRICT"),
    ("local_artists", "retired_into_artist_id", "local_artists", "RESTRICT"),
    ("local_entity_source_links", "local_album_id", "local_albums", "CASCADE"),
    ("local_entity_source_links", "local_artist_id", "local_artists", "CASCADE"),
    ("local_entity_source_links", "local_track_id", "local_tracks", "CASCADE"),
    ("local_entity_source_links", "selected_by_user_id", "auth_users", "SET NULL"),
    ("local_track_aliases", "alias", "local_tracks", "CASCADE"),
    ("local_track_aliases", "local_track_id", "local_tracks", "RESTRICT"),
    ("local_track_artists", "local_artist_id", "local_artists", "RESTRICT"),
    ("local_track_artists", "local_track_id", "local_tracks", "RESTRICT"),
    ("local_track_edition_tags", "local_track_id", "local_tracks", "CASCADE"),
    ("local_track_external_identities", "attempt_id", "library_identification_attempts", "RESTRICT"),
    ("local_track_external_identities", "local_track_id", "local_tracks", "RESTRICT"),
    ("local_track_genres", "local_track_id", "local_tracks", "CASCADE"),
    ("local_track_lyrics", "local_track_id", "local_tracks", "CASCADE"),
    ("local_tracks", "local_album_id", "local_albums", "RESTRICT"),
    ("media_navidrome_folder_preferences", "user_id", "auth_users", "CASCADE"),
    ("personal_mix_approvals", "reviewed_by_id", "auth_users", "SET NULL"),
    ("personal_mix_approvals", "user_id", "auth_users", "CASCADE"),
    ("request_history", "reviewed_by_id", "auth_users", "SET NULL"),
    ("request_history", "user_id", "auth_users", "SET NULL"),
    ("request_history_dismissals", "musicbrainz_id_lower", "request_history", "CASCADE"),
    ("request_history_dismissals", "user_id", "auth_users", "CASCADE"),
    ("request_history_requesters", "musicbrainz_id_lower", "request_history", "CASCADE"),
    ("request_history_requesters", "user_id", "auth_users", "CASCADE"),
    ("search_jobs", "user_id", "auth_users", "CASCADE"),
    ("spotify_auth_requests", "user_id", "auth_users", "CASCADE"),
    ("user_connections", "user_id", "auth_users", "CASCADE"),
    ("user_listening_prefs", "user_id", "auth_users", "CASCADE"),
    ("user_quotas", "user_id", "auth_users", "CASCADE"),
    ("wanted_seen_candidates", "release_group_mbid_lower", "wanted_watches", "CASCADE"),
    ("wanted_watches", "user_id", "auth_users", "CASCADE"),
];

/// Every foreign key in the schema is listed in [`FOREIGN_KEYS`] with its
/// ON DELETE action, the list names no other key, and no key sets an
/// ON UPDATE action.
#[test]
fn every_foreign_key_keeps_its_delete_action() {
    let connection = migrated();
    let mut found = Vec::new();
    for table in table_names(&connection) {
        let rows: Vec<(i64, String, String, String, String)> = connection
            .prepare(
                "SELECT id, \"table\", \"from\", on_update, on_delete \
                 FROM pragma_foreign_key_list(?1) ORDER BY id, seq",
            )
            .unwrap()
            .query_map([&table], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut keys: BTreeMap<i64, (String, Vec<String>, String, String)> = BTreeMap::new();
        for (id, parent, column, on_update, on_delete) in rows {
            keys.entry(id)
                .or_insert_with(|| (parent, Vec::new(), on_update, on_delete))
                .1
                .push(column);
        }
        for (parent, columns, on_update, on_delete) in keys.into_values() {
            assert_eq!(
                on_update,
                "NO ACTION",
                "{table}({}) sets ON UPDATE; list it here first",
                columns.join(", ")
            );
            found.push((table.clone(), columns.join(", "), parent, on_delete));
        }
    }
    found.sort();
    let mut listed: Vec<(String, String, String, String)> = FOREIGN_KEYS
        .iter()
        .map(|(table, columns, parent, action)| {
            (
                (*table).to_owned(),
                (*columns).to_owned(),
                (*parent).to_owned(),
                (*action).to_owned(),
            )
        })
        .collect();
    listed.sort();
    let missing: Vec<_> = found.iter().filter(|key| !listed.contains(key)).collect();
    let extra: Vec<_> = listed.iter().filter(|key| !found.contains(key)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "in the schema, not listed: {missing:#?}\nlisted, not in the schema: {extra:#?}"
    );
}

/// The records a person relies on to undo or explain a decision cannot be
/// rewritten: each immutability trigger refuses its statement, and the row
/// stays as written.
#[test]
fn immutable_records_refuse_change() {
    let connection = migrated();
    // The rows stand alone: no album, track or user behind them.
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO library_management_metadata_snapshots (id, provider, entity_kind,
                 entity_id, input_hash, canonical_payload_json, payload_sha256, fetched_at)
                 VALUES ('s1', 'musicbrainz', 'release', 'r1', printf('%064d', 0), '{}',
                 printf('%064d', 1), 1);
             INSERT INTO library_custom_edition_manifests (id, local_album_id, version,
                 release_group_mbid, album_title, album_artist_name, source_album_revision,
                 input_revision, content_hash, sealed_by_user_id, sealed_at)
                 VALUES ('m1', 'al1', 1, 'rg1', 'A', 'B', 1, 'i', 'h', 'u1', 1);
             INSERT INTO library_custom_edition_tracks (manifest_id, ordinal, local_track_id,
                 source_track_revision, stat_revision, tag_revision, title, artist_name,
                 album_title, album_artist_name, disc_number, track_number)
                 VALUES ('m1', 0, 't1', 1, 's', 't', 'T', 'B', 'A', 'B', 1, 1);
             INSERT INTO library_identification_attempts (id, local_album_id, trigger,
                 input_tag_revision, input_policy_revision, input_file_revision,
                 matcher_version, state, terminal_reason_code, started_at, completed_at)
                 VALUES ('a1', 'al1', 'scan', 't', 'p', 'f', 'm', 'identified', 'OK', 1, 2);",
        )
        .unwrap();
    for (statement, message) in [
        (
            "UPDATE library_management_metadata_snapshots SET fetched_at = 2",
            "management metadata snapshots are immutable",
        ),
        (
            "UPDATE library_custom_edition_manifests SET album_title = 'X'",
            "custom edition manifests are immutable",
        ),
        (
            "DELETE FROM library_custom_edition_manifests",
            "custom edition manifests are immutable",
        ),
        (
            "UPDATE library_custom_edition_tracks SET title = 'X'",
            "custom edition tracks are immutable",
        ),
        (
            "DELETE FROM library_custom_edition_tracks",
            "custom edition tracks are immutable",
        ),
        (
            "UPDATE library_identification_attempts SET terminal_reason_code = 'X'",
            "identification attempts are immutable",
        ),
    ] {
        let refused = connection.execute(statement, []).unwrap_err();
        assert!(
            refused.to_string().contains(message),
            "{statement}: {refused}"
        );
    }
    assert_eq!(
        texts(
            &connection,
            "SELECT (SELECT fetched_at FROM library_management_metadata_snapshots) || ' ' ||
                    (SELECT album_title FROM library_custom_edition_manifests) || ' ' ||
                    (SELECT title FROM library_custom_edition_tracks) || ' ' ||
                    (SELECT terminal_reason_code FROM library_identification_attempts)",
            []
        ),
        ["1 A T OK"]
    );
}

/// Each table in `server/table-owners.txt` with the area listed above it.
/// A table listed twice or an area nobody knows fails here.
fn table_owners() -> BTreeMap<&'static str, &'static str> {
    const AREAS: [&str; 10] = [
        "kernel", "auth", "plugins", "app", "compat", "media", "tool", "acquire", "catalog",
        "library",
    ];
    let mut owners = BTreeMap::new();
    let mut area = None;
    for line in include_str!("../../table-owners.txt")
        .lines()
        .map(str::trim)
    {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            assert!(AREAS.contains(&name), "unknown area {name}");
            area = Some(name);
            continue;
        }
        let area = area.expect("every table is listed under an area");
        assert!(
            owners.insert(line, area).is_none(),
            "{line} is listed twice"
        );
    }
    owners
}

/// Every table in the migrated schema is listed once in
/// `server/table-owners.txt` under a known area, and the list names no
/// table the schema lacks.
#[test]
fn every_table_has_one_owner() {
    let owners = table_owners();

    // The search index keeps internal tables of its own under its name.
    let tables: Vec<String> = table_names(&migrated())
        .into_iter()
        .filter(|name| !name.starts_with("local_tracks_fts_"))
        .collect();
    let unowned: Vec<&String> = tables
        .iter()
        .filter(|name| !owners.contains_key(name.as_str()))
        .collect();
    assert!(
        unowned.is_empty(),
        "no owner in table-owners.txt: {unowned:?}"
    );
    let stale: Vec<&&str> = owners
        .keys()
        .filter(|name| !tables.iter().any(|table| table == *name))
        .collect();
    assert!(stale.is_empty(), "listed but not in the schema: {stale:?}");
}

/// Every foreign key has an index whose leading columns are the key's
/// columns: without one, deleting or rekeying a parent row scans the
/// child table once per row. A primary key or unique index counts; a
/// partial index counts only when it leaves out just the rows whose
/// leading column is null, which no parent's lookup reads.
#[test]
fn every_foreign_key_is_indexed() {
    let connection = migrated();
    let mut unindexed = Vec::new();
    for table in table_names(&connection) {
        let keys: Vec<(i64, String)> = connection
            .prepare("SELECT id, \"from\" FROM pragma_foreign_key_list(?1) ORDER BY id, seq")
            .unwrap()
            .query_map([&table], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        if keys.is_empty() {
            continue;
        }
        let mut by_key: BTreeMap<i64, Vec<String>> = BTreeMap::new();
        for (id, column) in keys {
            by_key.entry(id).or_default().push(column);
        }
        let indexes: Vec<(String, bool)> = connection
            .prepare("SELECT name, partial FROM pragma_index_list(?1)")
            .unwrap()
            .query_map([&table], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut leading = Vec::new();
        for (index, partial) in indexes {
            let columns: Vec<String> = connection
                .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
                .unwrap()
                .query_map([&index], |row| row.get::<_, Option<String>>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .into_iter()
                .flatten()
                .collect();
            if partial {
                let text = texts(
                    &connection,
                    "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    [&index],
                )
                .concat()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
                let not_null = columns
                    .first()
                    .is_some_and(|first| text.ends_with(&format!("where {first} is not null")));
                if !not_null {
                    continue;
                }
            }
            leading.push(columns);
        }
        // An INTEGER PRIMARY KEY is the rowid and has no index of its own.
        leading.push(texts(
            &connection,
            "SELECT name FROM pragma_table_info(?1) WHERE pk = 1 AND upper(type) = 'INTEGER' \
             AND (SELECT COUNT(*) FROM pragma_table_info(?1) WHERE pk > 0) = 1",
            [&table],
        ));
        for columns in by_key.into_values() {
            let mut wanted = columns.clone();
            wanted.sort();
            let covered = leading.iter().any(|index| {
                index.len() >= wanted.len() && {
                    let mut head = index[..wanted.len()].to_vec();
                    head.sort();
                    head == wanted
                }
            });
            if !covered {
                unindexed.push(format!("{table}({})", columns.join(", ")));
            }
        }
    }
    assert!(
        unindexed.is_empty(),
        "foreign keys with no index:\n{}",
        unindexed.join("\n")
    );
}

/// A state column and the values its CHECK must list, read from the enum
/// that reads and writes it.
struct StateColumn {
    table: &'static str,
    column: &'static str,
    values: Vec<&'static str>,
}

fn state<T: Copy>(
    table: &'static str,
    column: &'static str,
    all: &[T],
    text: fn(T) -> &'static str,
) -> StateColumn {
    StateColumn {
        table,
        column,
        values: all.iter().map(|value| text(*value)).collect(),
    }
}

/// Every state column held to its enum.
#[rustfmt::skip]
fn state_columns() -> Vec<StateColumn> {
    use droppedneedle_acquire::dispatch::DispatchOrigin;
    use droppedneedle_acquire::downloads::landing_rows::ImportOutcome;
    use droppedneedle_acquire::downloads::quarantine::QuarantineReason;
    use droppedneedle_acquire::downloads::state::{AttemptDisposition, AttemptState, TaskStatus};
    use droppedneedle_acquire::drop_import::store::{DropItemStatus, DropJobStatus};
    use droppedneedle_acquire::held::store::{EntryOrigin, EntryState, FileState, HeldRoot};
    use droppedneedle_acquire::reason::Reason;
    use droppedneedle_acquire::requests::batches::models::BatchOutcome;
    use droppedneedle_acquire::requests::ledger::{ApprovalState, RequestStatus, WatchKind, WatchState};
    use droppedneedle_acquire::requests::models::RequestKind;
    use droppedneedle_acquire::search_jobs::store::JobStatus;
    use droppedneedle_auth::federated::password_import::HashScheme;
    use droppedneedle_auth::session::store::SessionKind;
    use droppedneedle_auth::users::roles::Role;
    use droppedneedle_catalog::collections::store::favorites::FavoriteKind;
    use droppedneedle_catalog::collections::store::ratings::RatedKind;
    use droppedneedle_catalog::concerts::matching::{Confidence, TmBasis};
    use droppedneedle_catalog::concerts::models::{ConcertStatus, EventSource};
    use droppedneedle_catalog::concerts::store::CheckOutcome;
    use droppedneedle_catalog::discover::cache::PageKind;
    use droppedneedle_catalog::discover::scheduler::Feature;
    use droppedneedle_compat::jellyfin::ids::IdKind;
    use droppedneedle_kernel::db::durable::JobState as LoopState;
    use droppedneedle_kernel::jobs::queue::State as QueueState;
    use droppedneedle_kernel::runtime_config::ingest::IngestOutcome;
    use droppedneedle_kernel::runtime_config::sections::ListenSource;
    use droppedneedle_library::activity::store::{EventStream, QueueSwitch};
    use droppedneedle_library::catalog_actions::ActionKind;
    use droppedneedle_library::contrib::models::{
        AttemptState as IdentificationAttemptState, ContributionState, ResultSource,
    };
    use droppedneedle_library::corrections::models::{
        AlbumAliasKind, ArtistAliasKind, MergeCandidateState,
    };
    use droppedneedle_library::identify::artist_links::{CreditState, ProofSubject};
    use droppedneedle_library::identify::merged_ids::CheckState;
    use droppedneedle_library::identify::models::{
        FingerprintState, IdentifyKind, JobState, MatchFlagState, ReviewState,
    };
    use droppedneedle_library::identity::DecisionSource;
    use droppedneedle_library::identity::remap::RemapOrigin;
    use droppedneedle_library::jobs::models::Control;
    use droppedneedle_library::jobs::{
        ControlRequest, ManagementPhase, OperationKind, OperationState, WorkState,
    };
    use droppedneedle_library::management::models::{
        Eligibility, JobMode, Origin, OverrideMode, TagScope,
    };
    use droppedneedle_library::management::takeover::Cause;
    use droppedneedle_library::publish::OutputKind;
    use droppedneedle_library::publish::journal::{JournalKind, JournalState, MgmtState};
    use droppedneedle_library::publish::snapshots::BlobOwner;
    use droppedneedle_library::repairs::models::{FindingState, Phase as RepairPhase, Purpose};
    use droppedneedle_library::scan::RevisionKind;
    use droppedneedle_library::scan::models::{
        ArtistKind, ArtworkSource, Availability, EffectivePolicy, EndPassName, GenreSource, GroupingSource,
        InventoryState, Phase as StartedPhase, RequestedControl, ScanKind, ScanPhase, ScanState, ScanTrigger, ScopeDiscoveryState, TagProvenance, Verdict,
    };
    use droppedneedle_library::scan::sqlite_store::retag_pairs::{RetagPairReason, RetagPairState};
    use droppedneedle_library::tags::lyrics::LyricsSource;
    use droppedneedle_media::playback::ports::Visibility;
    use droppedneedle_media::remotes::folders::FolderMode;
    use droppedneedle_tool::export::sections::{LinkMode, RefKind};
    use droppedneedle_tool::import::report::ExitCode;

    vec![
        // acquire
        state("acquire_discovery_batch_items", "outcome", &BatchOutcome::ALL, BatchOutcome::as_str),
        state("acquire_held_entries", "origin", &EntryOrigin::ALL, EntryOrigin::as_str),
        state("acquire_held_entries", "state", &EntryState::ALL, EntryState::as_str),
        state("acquire_held_files", "reason", &Reason::HELD, Reason::code),
        state("acquire_held_files", "root", &HeldRoot::ALL, HeldRoot::as_str),
        state("acquire_held_files", "state", &FileState::ALL, FileState::as_str),
        state("auto_download_approvals", "state", &ApprovalState::ALL, ApprovalState::as_str),
        state("download_attempts", "disposition", &AttemptDisposition::ALL, AttemptDisposition::as_str),
        state("download_attempts", "state", &AttemptState::ALL, AttemptState::as_str),
        state("download_import_decisions", "outcome", &ImportOutcome::ALL, ImportOutcome::as_str),
        state("download_quarantine", "reason", &QuarantineReason::ALL, QuarantineReason::as_str),
        state("download_tasks", "origin", &DispatchOrigin::ALL, DispatchOrigin::as_str),
        state("download_tasks", "status", &TaskStatus::ALL, TaskStatus::as_str),
        state("drop_import_items", "status", &DropItemStatus::ALL, DropItemStatus::as_str),
        state("drop_import_jobs", "status", &DropJobStatus::ALL, DropJobStatus::as_str),
        state("personal_mix_approvals", "state", &ApprovalState::ALL, ApprovalState::as_str),
        state("request_history", "origin", &DispatchOrigin::ALL, DispatchOrigin::as_str),
        state("request_history", "request_kind", &RequestKind::ALL, RequestKind::as_str),
        state("request_history", "status", &RequestStatus::ALL, RequestStatus::as_str),
        state("search_jobs", "status", &JobStatus::ALL, JobStatus::as_str),
        state("wanted_watches", "kind", &WatchKind::ALL, WatchKind::as_str),
        state("wanted_watches", "state", &WatchState::ALL, WatchState::as_str),
        // catalog
        state("catalog_artist_event_checks", "last_status", &CheckOutcome::ALL, CheckOutcome::as_str),
        state("catalog_artist_tm_attractions", "match_basis", &TmBasis::ALL, TmBasis::as_str),
        state("catalog_cached_pages", "page", &PageKind::ALL, PageKind::as_str),
        state("catalog_discover_activity", "feature", &Feature::ALL, Feature::as_str),
        state("catalog_live_event_feed", "match_confidence", &Confidence::ALL, Confidence::as_str),
        state("catalog_live_event_feed", "source", &EventSource::ALL, EventSource::as_str),
        state("catalog_live_event_feed", "status", &ConcertStatus::ALL, ConcertStatus::as_str),
        state("catalog_user_favorites", "item_kind", &FavoriteKind::ALL, FavoriteKind::as_str),
        state("catalog_user_ratings", "item_kind", &RatedKind::ALL, RatedKind::as_str),
        // compat
        state("compat_jellyfin_ids", "kind", &IdKind::ALL, IdKind::as_str),
        // kernel
        state("config_ingests", "outcome", &IngestOutcome::ALL, IngestOutcome::as_str),
        state("durable_jobs", "state", &QueueState::ALL, QueueState::as_str),
        state("durable_loops", "state", &LoopState::ALL, LoopState::as_str),
        // media
        state("media_navidrome_folder_preferences", "mode", &FolderMode::ALL, FolderMode::as_str),
        state("user_listening_prefs", "now_playing_visibility", &Visibility::ALL, Visibility::as_str),
        state("user_listening_prefs", "primary_music_source", &ListenSource::ALL, ListenSource::as_str),
        // tool
        state("import_pending_links", "mode", &LinkMode::ALL, LinkMode::as_str),
        state("import_pending_links", "ref_kind", &RefKind::ALL, RefKind::as_str),
        state("import_runs", "exit_code", &ExitCode::ALL, ExitCode::as_str),
        // auth
        state("auth_providers", "hash_scheme", &HashScheme::ALL, |scheme| scheme.as_tag()),
        state("auth_tokens", "session_kind", &SessionKind::ALL, SessionKind::as_str),
        state("auth_users", "role", &Role::ALL, Role::as_str),
        // library
        state("library_album_match_state", "state", &MatchFlagState::ALL, MatchFlagState::as_str),
        state("library_album_takeovers", "cause", &Cause::ALL, Cause::as_str),
        state("library_artist_credit_proofs", "subject_kind", &ProofSubject::ALL, ProofSubject::as_str),
        state("library_artist_reconciliation_state", "state", &CreditState::ALL, CreditState::as_str),
        state("library_audio_fingerprint_outcomes", "state", &FingerprintState::ALL, FingerprintState::as_str),
        state("library_catalog_actions", "action_kind", &ActionKind::ALL, ActionKind::as_str),
        state("library_contribution_drafts", "result_source", &ResultSource::ALL, ResultSource::as_str),
        state("library_contribution_drafts", "state", &ContributionState::ALL, ContributionState::as_str),
        state("library_edition_remap_queue", "origin", &RemapOrigin::ALL, RemapOrigin::as_str),
        state("library_event_stream_revisions", "stream_kind", &EventStream::ALL, EventStream::as_str),
        state("library_identification_attempts", "state", &IdentificationAttemptState::ALL, IdentificationAttemptState::as_str),
        state("library_identify_artist_id_checks", "state", &CheckState::ALL, CheckState::as_str),
        state("library_identify_jobs", "kind", &IdentifyKind::ALL, IdentifyKind::as_str),
        state("library_identify_jobs", "state", &JobState::ALL, JobState::as_str),
        state("library_identify_reviews", "state", &ReviewState::ALL, ReviewState::as_str),
        state("library_identity_repair_findings", "state", &FindingState::ALL, FindingState::as_str),
        state("library_management_job_snapshots", "mode", &JobMode::ALL, JobMode::as_str),
        state("library_management_job_snapshots", "origin", &Origin::ALL, Origin::as_str),
        state("library_management_job_snapshots", "phase", &ManagementPhase::ALL, ManagementPhase::as_str),
        state("library_management_overrides", "mode", &OverrideMode::ALL, OverrideMode::as_str),
        state("library_management_overrides", "subject_kind", &TagScope::ALL, TagScope::as_str),
        state("library_management_plan_items", "eligibility", &Eligibility::ALL, Eligibility::as_str),
        state("library_operation_control_idempotency", "control", &Control::ALL, Control::as_str),
        state("library_operation_jobs", "control_request", &ControlRequest::ALL, ControlRequest::as_str),
        state("library_operation_jobs", "kind", &OperationKind::ALL, OperationKind::as_str),
        state("library_operation_jobs", "state", &OperationState::ALL, OperationState::as_str),
        state("library_operation_work", "state", &WorkState::ALL, WorkState::as_str),
        state("library_publish_journal", "commit_as", &MgmtState::ALL, MgmtState::as_str),
        state("library_publish_journal", "kind", &JournalKind::ALL, JournalKind::as_str),
        state("library_publish_journal", "state", &JournalState::ALL, JournalState::as_str),
        state("library_publish_blob_refs", "owner_kind", &BlobOwner::ALL, BlobOwner::as_str),
        state("library_publish_outputs", "kind", &OutputKind::ALL, OutputKind::as_str),
        state("library_repair_snapshots", "phase", &RepairPhase::ALL, RepairPhase::as_str),
        state("library_repair_snapshots", "purpose", &Purpose::ALL, Purpose::as_str),
        state("library_retag_pairs", "reason", &RetagPairReason::ALL, RetagPairReason::as_str),
        state("library_retag_pairs", "state", &RetagPairState::ALL, RetagPairState::as_str),
        state("library_scan_failures", "phase", &StartedPhase::ALL, StartedPhase::as_str),
        state("library_scan_inventory", "comparison_result", &Verdict::ALL, Verdict::as_str),
        state("library_scan_inventory", "effective_policy", &EffectivePolicy::ALL, EffectivePolicy::as_str),
        state("library_scan_inventory", "processing_state", &InventoryState::ALL, InventoryState::as_str),
        state("library_scan_run_scopes", "discovery_state", &ScopeDiscoveryState::ALL, ScopeDiscoveryState::as_str),
        state("library_scan_run_scopes", "effective_policy", &EffectivePolicy::ALL, EffectivePolicy::as_str),
        state("library_scan_runs", "kind", &ScanKind::ALL, ScanKind::as_str),
        state("library_scan_runs", "next_end_pass", &EndPassName::ALL, EndPassName::as_str),
        state("library_scan_runs", "phase", &ScanPhase::ALL, ScanPhase::as_str),
        state("library_scan_runs", "requested_control", &RequestedControl::ALL, RequestedControl::as_str),
        state("library_scan_runs", "resume_phase", &ScanPhase::ALL, ScanPhase::as_str),
        state("library_scan_runs", "state", &ScanState::ALL, ScanState::as_str),
        state("library_scan_runs", "trigger", &ScanTrigger::ALL, ScanTrigger::as_str),
        state("library_work_control", "state", &QueueSwitch::ALL, QueueSwitch::as_str),
        state("local_album_aliases", "kind", &AlbumAliasKind::ALL, AlbumAliasKind::as_str),
        state("local_album_artwork", "source", &ArtworkSource::ALL, ArtworkSource::as_str),
        state("local_album_external_identities", "decision_source", &DecisionSource::ALL, DecisionSource::as_str),
        state("local_albums", "grouping_source", &GroupingSource::ALL, GroupingSource::as_str),
        state("local_artist_aliases", "kind", &ArtistAliasKind::ALL, ArtistAliasKind::as_str),
        state("local_artist_external_identities", "decision_source", &DecisionSource::ALL, DecisionSource::as_str),
        state("local_artist_merge_candidates", "state", &MergeCandidateState::ALL, MergeCandidateState::as_str),
        state("local_artists", "kind", &ArtistKind::ALL, ArtistKind::as_str),
        state("local_track_external_identities", "decision_source", &DecisionSource::ALL, DecisionSource::as_str),
        state("local_track_genres", "source", &GenreSource::ALL, GenreSource::as_str),
        state("local_track_lyrics", "source", &LyricsSource::ALL, LyricsSource::as_str),
        state("local_tracks", "album_artist_provenance", &TagProvenance::ALL, TagProvenance::as_str),
        state("local_tracks", "album_title_provenance", &TagProvenance::ALL, TagProvenance::as_str),
        state("local_tracks", "applied_policy", &EffectivePolicy::ALL, EffectivePolicy::as_str),
        state("local_tracks", "availability", &Availability::ALL, Availability::as_str),
        state("local_tracks", "membership_source", &GroupingSource::ALL, GroupingSource::as_str),
        state("local_tracks", "stat_revision_kind", &RevisionKind::ALL, RevisionKind::as_str),
        state("local_tracks", "title_provenance", &TagProvenance::ALL, TagProvenance::as_str),
    ]
}

/// Why a download source column holds no closed set.
const DOWNLOAD_SOURCE: &str = "a download source; a plugin's is plugin:<name>";

/// State-like TEXT columns not held to an enum, with the reason: they hold
/// something else, or a set with no end.
#[rustfmt::skip]
const NOT_STATE: &[(&str, &str, &str)] = &[
    ("acquire_held_files", "source", DOWNLOAD_SOURCE),
    ("auth_oidc_states", "state", "a random sign-in token"),
    ("auto_download_approvals", "source", "a label for what filed the ask, carried from v2 as written"),
    ("catalog_cached_pages", "source", "the MusicBrainz source's cache namespace"),
    ("catalog_discover_activity", "source", "the MusicBrainz source's cache namespace"),
    ("catalog_user_favorite_names", "item_kind", "its foreign key holds it to catalog_user_favorites"),
    ("download_attempts", "source", DOWNLOAD_SOURCE),
    ("download_quarantine", "source", DOWNLOAD_SOURCE),
    ("download_tasks", "source", DOWNLOAD_SOURCE),
    ("durable_jobs", "kind", "a job kind each area names"),
    ("library_play_history", "source", "where a play was reported from, carried from v2 as written"),
    ("spotify_auth_requests", "state", "a random sign-in token"),
    ("library_management_baselines", "restore_status", "v2's, carried by the upgrade; nothing reads it"),
    ("library_management_blobs", "kind", "v2's, carried by the upgrade; nothing reads it"),
    ("library_management_metadata_snapshots", "entity_kind", "a provider's entity type"),
    ("library_work_control", "queue_kind", "the row's key; its CHECK allows one value"),
    ("local_album_artists", "role", "credits carried from v2 keep v2's role names"),
    ("local_album_external_identities", "provider_source_mode", "v2's, carried by the upgrade; nothing reads it"),
    ("local_artist_external_identities", "provider_source_mode", "v2's, carried by the upgrade; nothing reads it"),
    ("local_entity_source_links", "decision_source", "v2's, carried by the upgrade; nothing reads it"),
    ("local_track_artists", "role", "credits carried from v2 keep v2's role names"),
    ("local_track_external_identities", "provider_source_mode", "v2's, carried by the upgrade; nothing reads it"),
    ("local_tracks", "ingest_source", "how a file came in; v2's rows and provenance name their own"),
    ("wanted_seen_candidates", "source", DOWNLOAD_SOURCE),
];

/// Every state column's CHECK lists exactly its enum's values. A column
/// counts as state when it has a `CHECK (column IN (...))` list or is TEXT
/// with a name ending in a state word; each is registered with its enum or
/// exempt with a reason.
#[test]
fn every_state_column_lists_exactly_its_enums_values() {
    let named_state =
        Regex::new(r"(^|_)(state|status|phase|role|kind|mode|source|origin|disposition)$").unwrap();
    // `column NOT IN (...)` matches with `NOT` as the column, and is skipped.
    let listed_values =
        Regex::new(r#"(?i)\b"?(\w+)"?\s+IN\s*\(\s*('[^']*'(?:\s*,\s*'[^']*')*)\s*\)"#).unwrap();
    let connection = migrated();
    let tables: Vec<(String, String)> = connection
        .prepare(
            "SELECT name, sql FROM sqlite_master WHERE type = 'table' \
             AND sql NOT LIKE 'CREATE VIRTUAL%' AND name NOT LIKE 'sqlite_%' \
             AND name NOT LIKE 'local_tracks_fts_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    let mut checked: BTreeMap<(String, String), std::collections::BTreeSet<String>> =
        BTreeMap::new();
    let mut columns = std::collections::BTreeSet::new();
    let mut state_like = std::collections::BTreeSet::new();
    for (table, sql) in &tables {
        for found in listed_values.captures_iter(sql) {
            if found[1].eq_ignore_ascii_case("not") {
                continue;
            }
            let values = found[2]
                .split(',')
                .map(|value| value.trim().trim_matches('\'').to_owned());
            checked
                .entry((table.clone(), found[1].to_owned()))
                .or_default()
                .extend(values);
        }
        let info: Vec<(String, String)> = connection
            .prepare("SELECT name, upper(type) FROM pragma_table_info(?1)")
            .unwrap()
            .query_map([table], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for (column, kind) in info {
            if kind == "TEXT" && named_state.is_match(&column) {
                state_like.insert((table.clone(), column.clone()));
            }
            columns.insert((table.clone(), column));
        }
    }

    let mut problems = Vec::new();
    let registered = state_columns();
    let mut entries: Vec<(&str, &str, &str)> = registered
        .iter()
        .map(|row| (row.table, row.column, "registered"))
        .collect();
    entries.extend(
        NOT_STATE
            .iter()
            .map(|(table, column, _)| (*table, *column, "not state")),
    );
    let mut accounted = BTreeMap::new();
    for (table, column, list) in entries {
        let key = (table.to_owned(), column.to_owned());
        if !columns.contains(&key) {
            problems.push(format!("{table}.{column} ({list}) is not in the schema"));
        }
        if let Some(earlier) = accounted.insert(key, list) {
            problems.push(format!(
                "{table}.{column} is listed as {earlier} and as {list}"
            ));
        }
    }

    for row in &registered {
        let wanted: std::collections::BTreeSet<String> =
            row.values.iter().map(|value| (*value).to_owned()).collect();
        if wanted.len() != row.values.len() {
            problems.push(format!(
                "{}.{}: its enum spells two values the same",
                row.table, row.column
            ));
        }
        match checked.get(&(row.table.to_owned(), row.column.to_owned())) {
            None => problems.push(format!("{}.{} has no CHECK list", row.table, row.column)),
            Some(listed) if *listed != wanted => problems.push(format!(
                "{}.{}: only the CHECK lists {:?}, only the enum has {:?}",
                row.table,
                row.column,
                listed.difference(&wanted).collect::<Vec<_>>(),
                wanted.difference(listed).collect::<Vec<_>>(),
            )),
            Some(_) => {}
        }
    }
    for key in checked.keys().chain(state_like.iter()) {
        if !accounted.contains_key(key) {
            problems.push(format!(
                "{}.{} holds a state: register its enum here, or say why it is not one",
                key.0, key.1
            ));
        }
    }
    problems.sort();
    problems.dedup();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

// ---------------------------------------------------------------------------
// Declared statements prepare against the schema, and raw SQL stays out of
// the areas that have moved theirs into `sql.rs`.
// ---------------------------------------------------------------------------

/// A crate's list of its `sql.rs` files.
type StatementFiles = fn() -> Vec<StatementFile>;

/// Every crate that holds a `sql.rs`, by its folder under `server/` (`.`
/// for the server crate), with the function that lists its files.
const REGISTRIES: &[(&str, StatementFiles)] = &[
    (
        "kernel",
        droppedneedle_kernel::test_support::statement_files,
    ),
    ("auth", droppedneedle_auth::test_support::statement_files),
    (
        "catalog",
        droppedneedle_catalog::test_support::statement_files,
    ),
    (
        "acquire",
        droppedneedle_acquire::test_support::statement_files,
    ),
    (
        "library",
        droppedneedle_library::test_support::statement_files,
    ),
    (
        "plugins",
        droppedneedle_plugins::test_support::statement_files,
    ),
    ("media", droppedneedle_media::test_support::statement_files),
    (
        "compat",
        droppedneedle_compat::test_support::statement_files,
    ),
    ("tool", droppedneedle_tool::test_support::statement_files),
    (".", droppedneedle::test_support::statement_files),
];

/// The crate name of a folder under `server/`.
fn crate_of(folder: &str) -> String {
    if folder == "." {
        "droppedneedle".to_owned()
    } else {
        format!("droppedneedle_{folder}")
    }
}

/// The crate folder of a `sql.rs` on disk and the module it must register
/// as: `catalog/src/a/sql.rs` is `droppedneedle_catalog::a::sql`, and
/// `src/a/sql.rs` is `droppedneedle::a::sql`.
fn module_of(path: &str) -> (&str, String) {
    let (folder, rest) = match path.strip_prefix("src/") {
        Some(rest) => (".", rest),
        None => path.split_once("/src/").expect("a source path"),
    };
    let module = rest.trim_end_matches(".rs").replace('/', "::");
    (folder, format!("{}::{module}", crate_of(folder)))
}

fn is_sql_file(path: &str) -> bool {
    path.ends_with("/sql.rs") || path == "src/sql.rs"
}

/// Declarations written in a `sql.rs`: every `NAME = ...` line, inside the
/// `sql!` block or a constant beside it.
fn declarations_in(text: &str) -> usize {
    let (code, _) = split_literals(text);
    Regex::new(
        r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|static|fragment|family)\s+)?[A-Z][A-Z0-9_]*\s*(?::[^=\n]*)?=[^=>]",
    )
    .unwrap()
    .find_iter(&code)
    .count()
}

/// The importer's statements run on the target with the export bundle
/// attached.
const IMPORT_MODULE: &str = "droppedneedle_tool::import::";

/// A copy of the migrated template with a bundle attached that holds every
/// column of every section spec.
fn import_target(dir: &crate::common::ScratchDir) -> rusqlite::Connection {
    let bundle_path = dir.join("bundle.db");
    let bundle = rusqlite::Connection::open(&bundle_path).expect("bundle opens");
    for section in droppedneedle_tool::export::sections::ALL {
        let columns = section
            .columns
            .iter()
            .map(|column| format!("\"{}\"", column.name))
            .collect::<Vec<_>>()
            .join(", ");
        bundle
            .execute_batch(&format!("CREATE TABLE \"{}\" ({columns})", section.name))
            .expect("section table builds");
    }
    drop(bundle);
    let path = dir.join("import-target.db");
    crate::common::migrated_db(&path);
    let target = rusqlite::Connection::open(&path).expect("template copy opens");
    target
        .execute(
            "ATTACH DATABASE ?1 AS bundle",
            [bundle_path.to_string_lossy()],
        )
        .expect("bundle attaches");
    target
}

/// Prepare one statement, and refuse a text holding more than one:
/// `prepare` alone compiles the first and ignores the rest.
fn prepare_one(connection: &rusqlite::Connection, name: &str, text: &str) -> Option<String> {
    let mut batch = rusqlite::Batch::new(connection, text);
    match batch.next() {
        Err(error) => Some(format!("{name}: {error}")),
        Ok(None) => Some(format!("{name}: holds no statement")),
        Ok(Some(_)) => match batch.next() {
            Ok(None) => None,
            Ok(Some(_)) => Some(format!("{name}: holds more than one statement")),
            Err(error) => Some(format!("{name}: holds more than one statement ({error})")),
        },
    }
}

/// A `row` reads by position, so the columns a statement answers must be the
/// struct's fields, by name and in order.
fn row_mismatch(
    connection: &rusqlite::Connection,
    shape: &droppedneedle_kernel::db::statement::RowShape,
    statement: &str,
    text: &str,
) -> Option<String> {
    let prepared = connection.prepare(text).ok()?;
    let answers = prepared.column_names();
    (answers != shape.columns()).then(|| {
        format!(
            "{} reads {}: the statement answers {answers:?}, the struct has {:?}",
            shape.name(),
            statement,
            shape.columns()
        )
    })
}

/// Decision 5: every declared statement, and every statement a family can
/// make, prepares against the migrated schema, or a `schema = V2` block's
/// against the v2 fixture database, or the importer's against the migrated
/// schema with a bundle attached, so a renamed column or a dropped table
/// fails here. Every `sql.rs` on disk is registered under the
/// module its path names, with as many declarations as its text holds.
/// Preparing does not check how many values a call site binds, that a
/// column list matches its row mapper, or the query plan.
#[test]
fn every_registered_statement_prepares() {
    let started = Instant::now();
    let mut wrong = Vec::new();

    let mut on_disk = BTreeMap::new();
    for source in workspace_sources() {
        if !is_sql_file(&source.path) {
            continue;
        }
        let (folder, module) = module_of(&source.path);
        if !REGISTRIES.iter().any(|(row, _)| *row == folder) {
            wrong.push(format!(
                "{}: crate folder `{folder}` holds a sql.rs and has no row here",
                source.path
            ));
        }
        on_disk.insert(module, (source.path.clone(), declarations_in(&source.text)));
    }

    let mut registered = BTreeMap::new();
    for (folder, list) in REGISTRIES {
        let files = list();
        if files.is_empty() {
            wrong.push(format!("`{folder}` has a row but lists no sql.rs"));
        }
        for file in files {
            if !file.module.starts_with(&format!("{}::", crate_of(folder))) {
                wrong.push(format!(
                    "{}: listed by `{folder}`, another crate",
                    file.module
                ));
            }
            if registered
                .insert(file.module, (file.schema, file.declared))
                .is_some()
            {
                wrong.push(format!("{}: listed twice", file.module));
            }
        }
    }

    for (module, (path, written)) in &on_disk {
        match registered.get(module.as_str()) {
            None => wrong.push(format!("{path}: missing from its crate's list of sql.rs files")),
            Some((_, declared))
                if declared.iter().filter(|one| !matches!(one, Declared::Row(_))).count()
                    != *written =>
            {
                wrong.push(format!(
                    "{path}: {written} declarations written, {} registered; declare SQL only inside its sql! block",
                    declared.len()
                ))
            }
            Some(_) => {}
        }
    }
    for module in registered.keys() {
        if !on_disk.contains_key(*module) {
            wrong.push(format!(
                "{module}: registered, but no sql.rs is at its path (a sql.rs is never reached through #[path])"
            ));
        }
    }

    let dir = crate::common::ScratchDir::new("statements");
    let path = dir.join("app.db");
    crate::common::migrated_db(&path);
    let app = rusqlite::Connection::open(&path).expect("template copy opens");
    let v2_root = dir.join("v2");
    droppedneedle_tool::test_support::fixture::build_v2_fixture(&v2_root)
        .expect("v2 fixture builds");
    let v2 = rusqlite::Connection::open(v2_root.join("cache").join("library.db"))
        .expect("v2 fixture opens");
    let cache =
        rusqlite::Connection::open(dir.join("metadata-cache.db")).expect("cache file opens");
    cache
        .execute_batch(droppedneedle_kernel::db::cache::SCHEMA)
        .expect("cache schema builds");
    let import = import_target(&dir);
    let (mut statements, mut families, mut made) = (0, 0, 0);
    for (module, schema, declared) in registered.iter().flat_map(|(module, (schema, declared))| {
        declared.iter().map(move |one| (*module, *schema, one))
    }) {
        let connection = match schema {
            Schema::App if module.starts_with(IMPORT_MODULE) => &import,
            Schema::App => &app,
            Schema::V2 => &v2,
            Schema::Cache => &cache,
        };
        match declared {
            Declared::Statement(statement) => {
                statements += 1;
                wrong.extend(prepare_one(connection, statement.name(), statement.text()));
            }
            Declared::Family(family) => {
                families += 1;
                match family.texts() {
                    Err(combinations) => wrong.push(format!(
                        "{}: {combinations} combinations, over the limit; split the family",
                        family.name()
                    )),
                    Ok(texts) => {
                        for (at, text) in texts.iter().enumerate() {
                            made += 1;
                            let name = format!("{} (combination {at})", family.name());
                            wrong.extend(prepare_one(connection, &name, text));
                        }
                    }
                }
            }
            Declared::Row(shape) => {
                for statement in shape.statements() {
                    wrong.extend(row_mismatch(
                        connection,
                        shape,
                        statement.name(),
                        statement.text(),
                    ));
                }
                for family in shape.families() {
                    for text in family.texts().unwrap_or_default() {
                        wrong.extend(row_mismatch(connection, shape, family.name(), &text));
                    }
                }
            }
            Declared::Fragment(_) => {}
        }
    }
    eprintln!(
        "prepared {statements} statements and {families} families ({made} statements) from {} files in {:?}",
        registered.len(),
        started.elapsed()
    );
    assert!(wrong.is_empty(), "declared SQL:\n{}", wrong.join("\n"));
}

/// The registered statements that read every indexed track on purpose:
/// the library-wide track list and its total, the newest and oldest
/// imports, a random pick, the genres and one genre's tracks and total,
/// the playable totals across every folder, and the player apps' track
/// lists, totals and unplayed halves over the whole library (one album's
/// or one exact title's are families of their own, checked here), and
/// their lists of every track and genre. Also the upgrade worklist, which
/// reads every owned album's tracks to find each album's worst file, the
/// whole owned set (Weekly Mix's, and the groups the upgrade sweep leaves
/// to a person), which reads every tagged track for albums owned through
/// their tags, and the quota's library total, which sums every indexed track (10 ms on 100k
/// tracks). The acquire reads of one release group's or one recording's
/// tracks (the album and recording quality checks and "the library holds
/// it") walk only because no index covers lowered release-group or
/// recording ids; the expression indexes that cover them take them off
/// this list. The library's three: the scan's pass over every indexed track
/// of a live album, the continuation sweep's walk of every missing track
/// when no album is named, and the remap sweep over every identified album's
/// tracks.
/// Discover's three: radio's two track reads, which filter on ids worked out
/// per track over the whole library, and the check for any local file, which
/// stops at the first row.
/// The library policy settings' three: the save preview's totals, which count
/// every track under the scopes asked about, the list of folders the catalog
/// holds tracks for, and the check for any track, which stops at the first
/// row. An admin opens these by hand.
const TRACK_WALKS: &[&str] = &[
    "droppedneedle_library::scan::sql::ALBUM_TRACKS",
    "droppedneedle_library::corrections::sql::CANDIDATE_ALBUMS",
    "droppedneedle_library::corrections::sql::CANDIDATES",
    "droppedneedle_catalog::local::sql::TRACK_PAGE_LIKE",
    "droppedneedle_catalog::local::sql::TRACK_PAGE_WORDS",
    "droppedneedle_catalog::local::sql::TRACK_COUNT_LIKE",
    "droppedneedle_catalog::local::sql::TRACK_COUNT_WORDS",
    "droppedneedle_catalog::local::sql::NEWEST_TRACKS",
    "droppedneedle_catalog::local::sql::OLDEST_TRACKS",
    "droppedneedle_catalog::local::sql::RANDOM_TRACK_IDS",
    "droppedneedle_catalog::local::sql::GENRES",
    "droppedneedle_catalog::local::sql::GENRE_TRACKS",
    "droppedneedle_catalog::local::sql::GENRE_TRACK_COUNT",
    "droppedneedle_catalog::local::sql::PLAYABLE_COUNTS",
    "droppedneedle_catalog::local::sql::PLAYER_TRACK_PAGE",
    "droppedneedle_catalog::local::sql::PLAYER_TRACK_COUNT",
    "droppedneedle_catalog::local::sql::PLAYER_TRACKS_UNPLAYED",
    "droppedneedle_catalog::local::sql::PLAYER_TRACKS_UNPLAYED_COUNT",
    "droppedneedle_compat::jellyfin::ids::sql::NEW_TRACKS",
    "droppedneedle_compat::jellyfin::ids::sql::GENRE_NAMES",
    "droppedneedle_acquire::flows::sql::IDENTIFIED_TRACKS",
    "droppedneedle_acquire::flows::sql::FLAGGED_GROUPS",
    "droppedneedle_catalog::local::sql::OWNED_GROUPS",
    "droppedneedle_acquire::requests::sql::LIBRARY_BYTES",
    "droppedneedle_catalog::discover::sql::RADIO_TRACKS_BY_ARTISTS",
    "droppedneedle_catalog::discover::sql::RADIO_TRACKS_OF_ALBUMS",
    "droppedneedle_catalog::discover::sql::HAS_LOCAL_FILES",
    "droppedneedle::settings::sql::SCOPE_TOTALS",
    "droppedneedle::settings::sql::CATALOG_ROOTS",
    "droppedneedle::settings::sql::ANY_TRACK",
];

/// Before the first ANALYZE (a fresh install) the planner has no
/// statistics, and an index led by `availability` can look as good as a
/// key: a read of one album's tracks then walks every indexed track. Every
/// registered statement outside [`TRACK_WALKS`] reaches the track table by
/// a key, never by a walk of it or of an index led by `availability`. The
/// importer's are left out: they run offline, on the bundle's rows, with
/// nothing waiting on them.
#[test]
fn registered_statements_reach_tracks_by_key() {
    let dir = crate::common::ScratchDir::new("track-plans");
    let path = dir.join("app.db");
    crate::common::migrated_db(&path);
    let app = rusqlite::Connection::open(&path).expect("template copy opens");
    let has_stats: bool = app
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE name = 'sqlite_stat1')",
            [],
            |row| row.get(0),
        )
        .expect("schema reads");
    assert!(!has_stats, "the template holds no planner statistics");

    let alias = Regex::new(r"(?i)\blocal_tracks(?:\s+AS)?\s+([a-z_][a-z0-9_]*)").unwrap();
    let keyword = Regex::new(
        r"(?i)^(where|join|left|inner|cross|on|set|indexed|group|order|limit|union|using|values|returning|natural|not|and|or)$",
    )
    .unwrap();
    let walk = Regex::new(concat!(
        r"^(?:SCAN (\S+)|",
        r"SEARCH \S+ USING (?:COVERING )?INDEX idx_local_tracks_\w+ \(availability=\?\)$)"
    ))
    .unwrap();
    let mut checked = 0;
    let mut wrong = Vec::new();
    for (_, list) in REGISTRIES {
        for file in list() {
            if !matches!(file.schema, Schema::App) || file.module.starts_with(IMPORT_MODULE) {
                continue;
            }
            for declared in file.declared {
                let (name, texts) = match declared {
                    Declared::Statement(one) => (one.name(), vec![one.text().to_owned()]),
                    Declared::Family(family) => (family.name(), family.texts().unwrap_or_default()),
                    Declared::Fragment(_) | Declared::Row(_) => continue,
                };
                if TRACK_WALKS.contains(&name) {
                    continue;
                }
                for text in texts {
                    let mut tables = vec!["local_tracks".to_owned()];
                    tables.extend(
                        alias
                            .captures_iter(&text)
                            .map(|found| found[1].to_owned())
                            .filter(|found| !keyword.is_match(found)),
                    );
                    let mut statement = app
                        .prepare(&format!("EXPLAIN QUERY PLAN {text}"))
                        .expect("registered statement prepares");
                    let mut rows = statement.raw_query();
                    while let Some(row) = rows.next().expect("plan row reads") {
                        let detail: String = row.get(3).expect("plan detail reads");
                        let Some(found) = walk.captures(&detail) else {
                            continue;
                        };
                        let walks_tracks = match found.get(1) {
                            Some(table) => tables.iter().any(|one| one == table.as_str()),
                            None => true,
                        };
                        if walks_tracks {
                            wrong.push(format!("{name}: {detail}"));
                        }
                    }
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 0, "no statement checked");
    assert!(
        wrong.is_empty(),
        "statements that walk the track table with no statistics:\n{}",
        wrong.join("\n")
    );
}

/// With no statistics (every first scan), the scan's per-file album lookup
/// and the run end's moved-track check reach their rows by key. The album
/// lookup never walks the live albums (`retired_into_album_id IS NULL`
/// holds for nearly all of them), and the moved-track check never walks
/// every move of the run for each track. Either walk grows with the rows
/// already there: the first made a 100,000-file first scan five times
/// slower by its end, the second makes a large retag's run end quadratic.
#[test]
fn scan_lookups_reach_albums_and_moves_by_key() {
    let dir = crate::common::ScratchDir::new("scan-plans");
    let path = dir.join("app.db");
    crate::common::migrated_db(&path);
    let app = rusqlite::Connection::open(&path).expect("template copy opens");
    let walks = [
        (
            "droppedneedle_library::scan::sql::ALBUMS_FOR_GROUPING",
            Regex::new(r"^SCAN local_albums|idx_local_albums_retired").unwrap(),
        ),
        (
            "droppedneedle_library::scan::sql::DEPARTED_FROM",
            Regex::new(r"^SEARCH m USING (?:COVERING )?INDEX \w+ \(run_id=\?\)$").unwrap(),
        ),
    ];
    let texts: BTreeMap<&str, String> = droppedneedle_library::test_support::statement_files()
        .into_iter()
        .flat_map(|file| file.declared)
        .filter_map(|declared| match declared {
            Declared::Statement(one) => Some((one.name(), one.text().to_owned())),
            _ => None,
        })
        .collect();
    let mut wrong = Vec::new();
    for (name, walk) in &walks {
        let text = texts
            .get(name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        let mut statement = app
            .prepare(&format!("EXPLAIN QUERY PLAN {text}"))
            .expect("registered statement prepares");
        let mut rows = statement.raw_query();
        while let Some(row) = rows.next().expect("plan row reads") {
            let detail: String = row.get(3).expect("plan detail reads");
            if walk.is_match(&detail) {
                wrong.push(format!("{name}: {detail}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "scan lookups that walk:\n{}",
        wrong.join("\n")
    );
}

/// The code of a file with every string literal emptied, and each literal's
/// line and value. Comments are skipped, and `\` line continuations are
/// joined as the compiler joins them. It reads text and does not parse
/// Rust.
fn split_literals(text: &str) -> (String, Vec<(usize, String)>) {
    let chars: Vec<char> = text.chars().collect();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let (mut code, mut literals) = (String::new(), Vec::new());
    let (mut at, mut line) = (0, 1);
    while at < chars.len() {
        let c = chars[at];
        if c == '/' && chars.get(at + 1) == Some(&'/') {
            while at < chars.len() && chars[at] != '\n' {
                at += 1;
            }
            continue;
        }
        if c == '/' && chars.get(at + 1) == Some(&'*') {
            at += 2;
            while at + 1 < chars.len() && !(chars[at] == '*' && chars[at + 1] == '/') {
                if chars[at] == '\n' {
                    line += 1;
                    code.push('\n');
                }
                at += 1;
            }
            at += 2;
            continue;
        }
        if c == '\'' {
            if chars.get(at + 1) == Some(&'\\') {
                at += 2;
                while at < chars.len() && chars[at] != '\'' {
                    at += 1;
                }
                at += 1;
                code.push_str("' '");
                continue;
            }
            if chars.get(at + 2) == Some(&'\'') {
                at += 3;
                code.push_str("' '");
                continue;
            }
        }
        let raw = (c == 'r' || (c == 'b' && chars.get(at + 1) == Some(&'r')))
            && (at == 0 || !ident(chars[at - 1]));
        if raw {
            let mut quote = at + if c == 'b' { 2 } else { 1 };
            let mut hashes = 0;
            while chars.get(quote) == Some(&'#') {
                hashes += 1;
                quote += 1;
            }
            if chars.get(quote) == Some(&'"') {
                let start = quote + 1;
                let mut end = start;
                while end < chars.len()
                    && !(chars[end] == '"'
                        && (1..=hashes).all(|k| chars.get(end + k) == Some(&'#')))
                {
                    end += 1;
                }
                let value: String = chars[start..end.min(chars.len())].iter().collect();
                let newlines = value.matches('\n').count();
                literals.push((line, value));
                line += newlines;
                code.push_str("\"\"");
                code.push_str(&"\n".repeat(newlines));
                at = end + 1 + hashes;
                continue;
            }
        }
        if c == '"' {
            let (start_line, mut value) = (line, String::new());
            at += 1;
            while at < chars.len() && chars[at] != '"' {
                if chars[at] == '\\' {
                    match chars.get(at + 1) {
                        Some('\n') => {
                            at += 1;
                            while at < chars.len() && chars[at].is_whitespace() {
                                if chars[at] == '\n' {
                                    line += 1;
                                    code.push('\n');
                                }
                                at += 1;
                            }
                            continue;
                        }
                        Some('n') => value.push('\n'),
                        Some('t') => value.push('\t'),
                        Some(other) => value.push(*other),
                        None => {}
                    }
                    at += 2;
                    continue;
                }
                if chars[at] == '\n' {
                    line += 1;
                    code.push('\n');
                }
                value.push(chars[at]);
                at += 1;
            }
            literals.push((start_line, value));
            code.push_str("\"\"");
            at += 1;
            continue;
        }
        if c == '\n' {
            line += 1;
        }
        code.push(c);
        at += 1;
    }
    (code, literals)
}

/// A literal allowed outside `sql.rs` for good, and why.
struct AllowedLiteral {
    path: &'static str,
    starts_with: &'static str,
    /// The literal is `starts_with` and nothing more, as an HTTP method's
    /// name is, so a statement in the same file that starts with that word
    /// is still refused.
    whole: bool,
    why: &'static str,
}

const ALLOWED_LITERALS: &[AllowedLiteral] = &[
    AllowedLiteral {
        path: "kernel/src/db/analyze.rs",
        starts_with: "CREATE TABLE db_planner_stats_reload (x); DROP TABLE",
        whole: false,
        why: "bumps the schema cookie so readers reload the planner statistics; the marker \
              table is never in the schema, so there is nothing to prepare it against",
    },
    AllowedLiteral {
        path: "compat/src/jellyfin/router.rs",
        starts_with: "DELETE",
        whole: true,
        why: "the HTTP method's name in the Jellyfin route table, not a statement",
    },
    AllowedLiteral {
        path: "compat/src/shared/ratelimit.rs",
        starts_with: "DELETE",
        whole: true,
        why: "the HTTP method's name when classing a request as a mutation, not a statement",
    },
    AllowedLiteral {
        path: "plugins/src/manifest.rs",
        starts_with: "DELETE",
        whole: true,
        why: "the HTTP method's name in a plugin's route list, not a statement",
    },
    AllowedLiteral {
        path: "plugins/src/capabilities/routes.rs",
        starts_with: "DELETE",
        whole: true,
        why: "an HTTP method a plugin route may declare, not a statement",
    },
    AllowedLiteral {
        path: "src/bootstrap/data_dirs.rs",
        starts_with: "PRAGMA ",
        whole: false,
        why: "the stamp probe reads a file's version before the runtime opens it, and its \
              tests put a probed file in WAL mode; a pragma prepares only in kernel/src/db/",
    },
    AllowedLiteral {
        path: "src/bootstrap/data_dirs.rs",
        starts_with: "CREATE TABLE ",
        whole: false,
        why: "the stamp probe's tests build a small file of each kind it tells apart",
    },
    AllowedLiteral {
        path: "src/admin/backups.rs",
        starts_with: "PRAGMA ",
        whole: false,
        why: "the backup checks read a backup file's integrity and version, a file the \
              server never opens as its database",
    },
];

/// The export's bundle builder. Each statement is made at run time from the
/// columns the v2 database being exported has (an install of any age), into
/// bundle tables made the same way, so there is no fixed text to prepare;
/// its fixed reads are in `tool/src/export/sql.rs`.
const EXPORT_BUNDLE: &[&str] = &[
    "tool/src/export/artist_keys.rs",
    "tool/src/export/bundle.rs",
    "tool/src/export/library_keys.rs",
    "tool/src/export/sections/",
];

/// Source that may hold raw SQL for good: unit test files and test folders
/// (a unit test that needs raw SQL lives in one), the builder of the v2
/// fixture database, which writes the v2 schema, and [`EXPORT_BUNDLE`].
/// `server/tests/`, `server/testkit/` and `server/migrations/` are never
/// walked.
fn exempt(path: &str) -> bool {
    path.ends_with("/tests.rs")
        || path.ends_with("_tests.rs")
        || path.contains("/tests/")
        || path == "tool/src/test_support/fixture.rs"
        || EXPORT_BUNDLE.iter().any(|place| path.starts_with(place))
}

/// Raw SQL lives only in `sql.rs` files. A string literal that starts with
/// a statement verb, or holds ` FROM `, ` JOIN `, ` WHERE `, ` VALUES ` or
/// ` ORDER BY ` in capitals, fails anywhere else. One that starts with
/// `PRAGMA`, `ANALYZE`, `VACUUM` or transaction control fails outside
/// `kernel/src/db/`, which owns connection set-up and transactions. A table
/// name read from the database is quoted by
/// `db::statement::quoted_identifier`.
#[test]
fn raw_sql_stays_in_sql_files() {
    let verb = Regex::new(r"^\s*(?:SELECT|INSERT|UPDATE|DELETE|REPLACE|WITH|CREATE|DROP|ALTER)\b")
        .unwrap();
    let fragment = Regex::new(r"(?:^|\s)(?:FROM|JOIN|WHERE|VALUES|ORDER\s+BY)(?:\s|$)").unwrap();
    let control =
        Regex::new(r"^\s*(?:PRAGMA|ANALYZE|VACUUM|BEGIN|COMMIT|ROLLBACK|SAVEPOINT|RELEASE)\b")
            .unwrap();
    let sources = workspace_sources();
    let mut allowed_used = vec![false; ALLOWED_LITERALS.len()];
    let mut wrong = Vec::new();
    for source in &sources {
        if exempt(&source.path) {
            continue;
        }
        for (line, value) in split_literals(&source.text).1 {
            let raw = if control.is_match(&value) {
                !source.path.starts_with("kernel/src/db/")
            } else {
                !is_sql_file(&source.path) && (verb.is_match(&value) || fragment.is_match(&value))
            };
            if !raw {
                continue;
            }
            if let Some(at) = ALLOWED_LITERALS.iter().position(|allowed| {
                allowed.path == source.path
                    && if allowed.whole {
                        value == allowed.starts_with
                    } else {
                        value.starts_with(allowed.starts_with)
                    }
            }) {
                allowed_used[at] = true;
                continue;
            }
            let first: String = value.trim().chars().take(60).collect();
            wrong.push(format!(
                "{}:{line}: raw SQL outside sql.rs: {first}",
                source.path
            ));
        }
    }
    for (allowed, used) in ALLOWED_LITERALS.iter().zip(allowed_used) {
        if !used {
            wrong.push(format!(
                "{}: allowed literal no longer there; remove the entry ({})",
                allowed.path, allowed.why
            ));
        }
    }
    assert!(wrong.is_empty(), "raw SQL:\n{}", wrong.join("\n"));
}

/// The runner's `-- no-transaction` path and the table-rebuild recipe in the
/// kernel's `db::migrate` module doc, proven on a parent that cascade,
/// restrict and set-null children point at.
mod rebuild {
    use std::path::{Path, PathBuf};

    use droppedneedle_kernel::db::{Chain, Migration, SchemaError, migrate, open_connection};
    use regex::Regex;
    use rusqlite::Connection;

    use crate::common::ScratchDir;

    const BASE: &str = r#"
CREATE TABLE p (id TEXT PRIMARY KEY, name TEXT NOT NULL) STRICT;
CREATE INDEX p_name ON p (name);
CREATE TABLE p_log (id TEXT NOT NULL, name TEXT NOT NULL) STRICT;
CREATE TRIGGER p_renamed AFTER UPDATE OF name ON p
BEGIN INSERT INTO p_log (id, name) VALUES (NEW.id, NEW.name); END;
CREATE TABLE c_cascade (id INTEGER PRIMARY KEY,
    p_id TEXT NOT NULL REFERENCES p (id) ON DELETE CASCADE) STRICT;
CREATE INDEX c_cascade_p ON c_cascade (p_id);
CREATE TABLE c_restrict (id INTEGER PRIMARY KEY,
    p_id TEXT NOT NULL REFERENCES p (id) ON DELETE RESTRICT) STRICT;
CREATE INDEX c_restrict_p ON c_restrict (p_id);
CREATE TABLE c_set_null (id INTEGER PRIMARY KEY,
    p_id TEXT REFERENCES p (id) ON DELETE SET NULL) STRICT;
CREATE INDEX c_set_null_p ON c_set_null (p_id);
CREATE TABLE c_keyed (p_id TEXT NOT NULL REFERENCES p (id) ON DELETE CASCADE,
    tag TEXT NOT NULL, PRIMARY KEY (p_id, tag)) STRICT, WITHOUT ROWID;
CREATE TABLE other (id INTEGER PRIMARY KEY) STRICT;
CREATE TABLE other_child (id INTEGER PRIMARY KEY, other_id INTEGER REFERENCES other (id)) STRICT;
CREATE INDEX other_child_other ON other_child (other_id);
PRAGMA user_version = 1;
"#;

    const RECIPE: &str = r#"-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;
CREATE TEMP TABLE rebuild_guard (ok INTEGER NOT NULL CHECK (ok = 1)) STRICT;
INSERT INTO rebuild_guard SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO rebuild_guard SELECT (SELECT user_version FROM pragma_user_version) < 2;
CREATE TEMP TABLE rebuild_orphans AS
    SELECT "table" AS child, rowid AS row, parent, fkid FROM pragma_foreign_key_check;
CREATE TABLE p_new (id TEXT PRIMARY KEY, name TEXT NOT NULL,
    added_at INTEGER NOT NULL DEFAULT 0) STRICT;
INSERT INTO p_new (rowid, id, name, added_at) SELECT rowid, id, name, 0 FROM p;
DROP TABLE p;
ALTER TABLE p_new RENAME TO p;
CREATE INDEX p_name ON p (name);
CREATE TRIGGER p_renamed AFTER UPDATE OF name ON p
BEGIN INSERT INTO p_log (id, name) VALUES (NEW.id, NEW.name); END;
INSERT INTO rebuild_guard SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, fkid, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3, 4) AS now
    WHERE now.n > (SELECT COUNT(*) FROM rebuild_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent AND before.fkid = now.fkid));
DROP TABLE rebuild_guard;
DROP TABLE rebuild_orphans;
PRAGMA user_version = 2;
COMMIT;
PRAGMA foreign_keys=ON;
"#;

    const FK_OFF_GUARD: &str =
        "INSERT INTO rebuild_guard SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;\n";

    fn step(version: i64, sql: &str) -> Migration<'_> {
        Migration {
            version,
            name: "step",
            sql,
        }
    }

    fn run(
        connection: Connection,
        migrations: &[Migration<'_>],
    ) -> Result<Connection, SchemaError> {
        migrate(connection, &Chain::new(migrations, None))
    }

    fn rebuilt_by(recipe: &str) -> [Migration<'_>; 2] {
        [step(1, BASE), step(2, recipe)]
    }

    /// A file database at the base, on a connection opened as boot opens
    /// one: foreign keys on.
    fn based(dir: &ScratchDir) -> (Connection, PathBuf) {
        let path = dir.join("rebuild.db");
        let connection = run(open_connection(&path).unwrap(), &[step(1, BASE)]).unwrap();
        (connection, path)
    }

    /// Parents inserted out of key order, so rowid order is not key order.
    fn seed(connection: &Connection, restrict: bool) {
        let mut sql = String::from(
            "INSERT INTO p (id, name) VALUES ('c', 'Cee'), ('a', 'Ay'), ('b', 'Bee');
             INSERT INTO c_cascade (id, p_id) VALUES (1, 'a'), (2, 'b'), (3, 'c');
             INSERT INTO c_set_null (id, p_id) VALUES (1, 'a'), (2, 'c');",
        );
        if restrict {
            sql.push_str("INSERT INTO c_restrict (id, p_id) VALUES (1, 'b'), (2, 'c');");
        }
        connection.execute_batch(&sql).unwrap();
    }

    /// One orphan in a table the rebuild never touches.
    fn seed_unrelated_orphan(connection: &Connection) {
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 INSERT INTO other_child (id, other_id) VALUES (1, 99);
                 PRAGMA foreign_keys = ON;",
            )
            .unwrap();
    }

    type Child = (i64, Option<String>);

    fn rows(connection: &Connection, sql: &str) -> Vec<Child> {
        connection
            .prepare(sql)
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn children(connection: &Connection) -> Vec<Vec<Child>> {
        ["c_cascade", "c_restrict", "c_set_null"]
            .iter()
            .map(|table| {
                rows(
                    connection,
                    &format!("SELECT id, p_id FROM {table} ORDER BY id"),
                )
            })
            .collect()
    }

    /// Every schema row except the rebuilt table's own `CREATE TABLE`.
    fn schema_but_p(connection: &Connection) -> Vec<(String, String, String, String)> {
        connection
            .prepare(
                "SELECT type, name, tbl_name, COALESCE(sql, '') FROM sqlite_schema \
                 WHERE NOT (type = 'table' AND name = 'p') ORDER BY type, name",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn scalar(connection: &Connection, sql: &str) -> i64 {
        super::scalar(connection, sql)
    }

    /// Every row of every table and the stamp, read over a connection of
    /// its own.
    fn snapshot(path: &Path) -> Vec<String> {
        let connection = Connection::open(path).unwrap();
        let stamp: i64 = scalar(&connection, "PRAGMA user_version");
        let mut out = vec![format!("user_version {stamp}")];
        let schema: Vec<(String, String, Option<String>)> = connection
            .prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for (kind, name, sql) in schema {
            out.push(format!("{kind} {name} {sql:?}"));
            if kind != "table" {
                continue;
            }
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{name}\""))
                .unwrap();
            let width = statement.column_count();
            let mut table: Vec<String> = statement
                .query_map([], |row| {
                    (0..width)
                        .map(|at| row.get::<_, rusqlite::types::Value>(at))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|values| format!("{name} {values:?}"))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            table.sort();
            out.extend(table);
        }
        out
    }

    fn execute_failure(error: SchemaError) -> String {
        match error {
            SchemaError::Failed { version: 2, source } => source.to_string(),
            other => panic!("expected migration 2 to fail, got {other:?}"),
        }
    }

    /// The recipe rebuilds the parent through the production runner: every
    /// child row stays and points at its parent, the rest of the schema is
    /// unchanged, rowid order holds, foreign keys are back on for the same
    /// connection, and the keys still act.
    #[test]
    fn the_recipe_rebuilds_a_parent_and_keeps_every_child() {
        let dir = ScratchDir::new("rebuild-keep");
        let (connection, _) = based(&dir);
        seed(&connection, true);
        let children_before = children(&connection);
        let schema_before = schema_but_p(&connection);
        let order_before = rows(&connection, "SELECT rowid, id FROM p ORDER BY rowid");

        let connection = run(connection, &rebuilt_by(RECIPE)).unwrap();

        assert_eq!(children(&connection), children_before, "no child row moved");
        assert_eq!(
            scalar(&connection, "SELECT COUNT(*) FROM pragma_foreign_key_check"),
            0,
            "every child points at its parent"
        );
        assert_eq!(
            schema_but_p(&connection),
            schema_before,
            "indexes, triggers and the children's schema are unchanged"
        );
        assert_eq!(
            rows(&connection, "SELECT rowid, id FROM p ORDER BY rowid"),
            order_before
        );
        assert_eq!(scalar(&connection, "PRAGMA foreign_keys"), 1);
        assert_eq!(scalar(&connection, "PRAGMA user_version"), 2);
        assert_eq!(
            super::column_names(&connection, "p"),
            ["id", "name", "added_at"],
            "the rebuild took effect"
        );

        connection
            .execute("DELETE FROM p WHERE id = 'a'", [])
            .unwrap();
        assert_eq!(
            scalar(
                &connection,
                "SELECT COUNT(*) FROM c_cascade WHERE p_id = 'a'"
            ),
            0,
            "the cascade fires"
        );
        assert_eq!(
            rows(&connection, "SELECT id, p_id FROM c_set_null WHERE id = 1"),
            [(1, None)],
            "set null fires"
        );
        assert!(
            connection
                .execute("DELETE FROM p WHERE id = 'b'", [])
                .is_err(),
            "restrict still refuses"
        );
        connection
            .execute("UPDATE p SET name = 'Cy' WHERE id = 'c'", [])
            .unwrap();
        assert_eq!(
            scalar(&connection, "SELECT COUNT(*) FROM p_log"),
            1,
            "the trigger fires"
        );
    }

    /// An old orphan in another table does not block the rebuild; a rebuild
    /// that makes a new orphan is refused and leaves the database exactly
    /// as it was, its stamp included. A WITHOUT ROWID child reports every
    /// orphan with a NULL rowid, so one old orphan there must not let a new
    /// one through either.
    #[test]
    fn a_rebuild_refuses_every_new_orphan() {
        let dir = ScratchDir::new("rebuild-orphan");
        let (connection, _) = based(&dir);
        seed(&connection, true);
        seed_unrelated_orphan(&connection);
        let connection = run(connection, &rebuilt_by(RECIPE)).unwrap();
        assert_eq!(scalar(&connection, "PRAGMA user_version"), 2);

        let dir = ScratchDir::new("rebuild-new-orphan");
        let (connection, path) = based(&dir);
        seed(&connection, true);
        seed_unrelated_orphan(&connection);
        let before = snapshot(&path);
        let orphaning = RECIPE.replace("0 FROM p;", "0 FROM p WHERE id <> 'a';");
        assert_ne!(orphaning, RECIPE);

        let error = run(connection, &rebuilt_by(&orphaning)).unwrap_err();
        assert!(
            execute_failure(error).contains("CHECK constraint failed"),
            "the guard after the copy refuses"
        );
        assert_eq!(snapshot(&path), before, "the database is as it was");

        let dir = ScratchDir::new("rebuild-keyed-orphan");
        let (connection, path) = based(&dir);
        connection
            .execute_batch(
                "INSERT INTO p (id, name) VALUES ('c', 'Cee'), ('a', 'Ay'), ('b', 'Bee');
                 INSERT INTO c_keyed (p_id, tag) VALUES ('a', 'x'), ('b', 'x');
                 PRAGMA foreign_keys = OFF;
                 INSERT INTO c_keyed (p_id, tag) VALUES ('gone', 'x');
                 PRAGMA foreign_keys = ON;",
            )
            .unwrap();
        let before = snapshot(&path);
        let orphaning = RECIPE.replace("0 FROM p;", "0 FROM p WHERE id <> 'a';");

        let error = run(connection, &rebuilt_by(&orphaning)).unwrap_err();
        assert!(
            execute_failure(error).contains("CHECK constraint failed"),
            "without rowid: the guard after the copy refuses"
        );
        assert_eq!(
            snapshot(&path),
            before,
            "without rowid: the database is as it was"
        );
    }

    /// The naive versions of the recipe fail, so each step it adds is
    /// needed.
    #[test]
    fn the_naive_rebuilds_fail() {
        // Without the first line and its own BEGIN and COMMIT, the file runs
        // inside the runner's transaction, where foreign_keys=OFF does
        // nothing and DROP TABLE empties the children. The guard that
        // foreign keys are off is what catches that, so it goes too.
        let wrapped = RECIPE
            .replace("-- no-transaction\n", "")
            .replace("BEGIN IMMEDIATE;\n", "")
            .replace("COMMIT;\n", "")
            .replace(FK_OFF_GUARD, "");
        let dir = ScratchDir::new("rebuild-naive-wrapped");
        let (connection, _) = based(&dir);
        seed(&connection, false);
        let connection = run(connection, &rebuilt_by(&wrapped)).unwrap();
        assert_eq!(
            scalar(&connection, "SELECT COUNT(*) FROM c_cascade"),
            0,
            "the children cascade"
        );
        assert_eq!(
            scalar(
                &connection,
                "SELECT COUNT(*) FROM c_set_null WHERE p_id IS NULL"
            ),
            2,
            "the set-null children lose their parent"
        );

        // With that guard kept, the same wrapped file is refused instead.
        let guarded = wrapped.replace(
            "CREATE TEMP TABLE rebuild_orphans",
            &format!("{FK_OFF_GUARD}CREATE TEMP TABLE rebuild_orphans"),
        );
        let dir = ScratchDir::new("rebuild-naive-guarded");
        let (connection, path) = based(&dir);
        seed(&connection, true);
        let error = run(connection, &rebuilt_by(&guarded)).unwrap_err();
        assert!(execute_failure(error).contains("CHECK constraint failed"));
        let connection = Connection::open(&path).unwrap();
        assert_eq!(scalar(&connection, "SELECT COUNT(*) FROM c_cascade"), 3);
        drop(connection);

        // Renaming the old table first: SQLite rewrites every child's
        // REFERENCES to the old name, so its drop leaves them pointing at
        // nothing, and the guard after the copy refuses.
        let renamed_first = RECIPE.replace(
            "DROP TABLE p;\nALTER TABLE p_new RENAME TO p;\n",
            "ALTER TABLE p RENAME TO p_old;\nALTER TABLE p_new RENAME TO p;\nDROP TABLE p_old;\n",
        );
        assert_ne!(renamed_first, RECIPE);
        let dir = ScratchDir::new("rebuild-naive-renamed");
        let (connection, path) = based(&dir);
        seed(&connection, true);
        let before = snapshot(&path);
        let error = run(connection, &rebuilt_by(&renamed_first)).unwrap_err();
        assert!(execute_failure(error).contains("CHECK constraint failed"));
        assert_eq!(snapshot(&path), before);
        // Without that guard it commits, and the children name the old table.
        let after_copy =
            Regex::new(r"(?s)INSERT INTO rebuild_guard SELECT NOT EXISTS \(.*?\);\n").unwrap();
        let unguarded = after_copy.replace(&renamed_first, "").into_owned();
        assert_ne!(unguarded, renamed_first);
        let connection = run(open_connection(&path).unwrap(), &rebuilt_by(&unguarded)).unwrap();
        let child: String = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name = 'c_cascade'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(child.contains("p_old"), "{child}");

        // The recipe run inside the runner's transaction fails on its own
        // BEGIN and changes nothing.
        let inside = RECIPE.replace("-- no-transaction\n", "");
        let dir = ScratchDir::new("rebuild-naive-inside");
        let (connection, path) = based(&dir);
        seed(&connection, true);
        let before = snapshot(&path);
        let error = run(connection, &rebuilt_by(&inside)).unwrap_err();
        let error = execute_failure(error);
        assert!(
            error.contains("cannot start a transaction within a transaction"),
            "{error}"
        );
        assert_eq!(snapshot(&path), before);
    }

    /// The stamp is the only record. A no-transaction file whose COMMIT
    /// landed before a restart is not run again (its own guard would refuse
    /// a second run), and a stamp past the newest file is refused with
    /// nothing changed.
    #[test]
    fn a_committed_file_never_runs_again() {
        let dir = ScratchDir::new("rebuild-committed");
        let (connection, path) = based(&dir);
        seed(&connection, true);
        connection.execute_batch(RECIPE).unwrap();
        drop(connection);
        let committed = snapshot(&path);
        let rerun = Connection::open(&path)
            .unwrap()
            .execute_batch(RECIPE)
            .unwrap_err();
        assert!(
            rerun.to_string().contains("CHECK constraint failed"),
            "the file refuses to run twice"
        );

        let connection = run(open_connection(&path).unwrap(), &rebuilt_by(RECIPE)).unwrap();
        assert_eq!(scalar(&connection, "SELECT COUNT(*) FROM c_cascade"), 3);
        assert_eq!(scalar(&connection, "PRAGMA foreign_keys"), 1);
        drop(connection);
        assert_eq!(snapshot(&path), committed);

        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 3).unwrap();
        drop(connection);
        let ahead = snapshot(&path);
        let error = run(open_connection(&path).unwrap(), &rebuilt_by(RECIPE)).unwrap_err();
        assert!(
            matches!(
                error,
                SchemaError::TooNew {
                    found: 3,
                    latest: 2
                }
            ),
            "{error:?}"
        );
        assert_eq!(snapshot(&path), ahead, "nothing changed");
    }

    /// What is wrong with a no-transaction file's shape, by the recipe.
    fn shape_problems(migration: &Migration<'_>) -> Vec<String> {
        let lines: Vec<&str> = migration
            .sql
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let mut problems = Vec::new();
        let opening = [
            "-- no-transaction",
            "PRAGMA foreign_keys=OFF;",
            "BEGIN IMMEDIATE;",
        ];
        if lines.get(..3) != Some(&opening[..]) {
            problems.push("does not open with the recipe's three lines".to_owned());
        }
        let commits: Vec<usize> = (0..lines.len())
            .filter(|at| lines[*at] == "COMMIT;")
            .collect();
        let stamp = format!("PRAGMA user_version = {};", migration.version);
        match commits[..] {
            [at] if at > 0 && lines[at - 1] == stamp => {}
            [_] => problems.push(format!("COMMIT is not preceded by `{stamp}`")),
            _ => problems.push(format!("has {} COMMIT lines, not one", commits.len())),
        }
        if lines.last() != Some(&"PRAGMA foreign_keys=ON;") {
            problems.push("does not end with `PRAGMA foreign_keys=ON;`".to_owned());
        }
        problems
    }

    /// Every shipped no-transaction migration has the recipe's shape, and
    /// every migration that sets foreign keys is one.
    #[test]
    fn no_transaction_migrations_follow_the_recipe() {
        assert_eq!(shape_problems(&step(2, RECIPE)), Vec::<String>::new());
        assert_eq!(shape_problems(&step(3, RECIPE)).len(), 1);

        let sets_foreign_keys = Regex::new(r"(?i)PRAGMA\s+foreign_keys\s*=").unwrap();
        let mut wrong = Vec::new();
        for migration in droppedneedle_kernel::schema::MIGRATIONS {
            if migration.no_transaction() {
                for problem in shape_problems(migration) {
                    wrong.push(format!("{}: {problem}", migration.version));
                }
            } else if sets_foreign_keys.is_match(migration.sql) {
                wrong.push(format!(
                    "{}: sets foreign_keys without `-- no-transaction`",
                    migration.version
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}

/// The auth enums are exactly what the auth tables' CHECKs list, in order.
#[test]
fn auth_checks_list_exactly_the_code_values() {
    use droppedneedle_auth::Role;
    use droppedneedle_auth::federated::password_import::HashScheme;
    use droppedneedle_auth::session::store::SessionKind;

    let connection = migrated();
    let mut schemas = BTreeMap::new();
    for table in ["auth_users", "auth_tokens", "auth_providers"] {
        let sql = texts(
            &connection,
            "SELECT sql FROM sqlite_master WHERE name = ?1",
            [table],
        );
        schemas.insert(table, sql[0].clone());
    }
    let listed = |table: &str, column: &str| -> Vec<String> {
        let sql = &schemas[table];
        let marker = format!("CHECK({column} IN (");
        let values = &sql[sql.find(&marker).expect("the column has a CHECK") + marker.len()..];
        values[..values.find(')').expect("the list ends")]
            .split(',')
            .map(|value| value.trim().trim_matches('\'').to_owned())
            .collect()
    };
    assert_eq!(
        listed("auth_users", "role"),
        Role::ALL.map(|role| role.as_str().to_owned())
    );
    assert_eq!(
        listed("auth_tokens", "session_kind"),
        SessionKind::ALL.map(|kind| kind.as_str().to_owned())
    );
    assert_eq!(
        listed("auth_providers", "hash_scheme"),
        HashScheme::ALL.map(|scheme| scheme.as_tag().to_owned())
    );
    for kind in SessionKind::ALL {
        assert_eq!(SessionKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(SessionKind::parse("superuser"), None);
}

/// Every table is STRICT and every column named as a point in time holds
/// integer milliseconds. Only SQLite's own tables and the search index (a
/// virtual table and its shadow tables) are not STRICT.
#[test]
fn every_table_is_strict_and_every_time_is_an_integer() {
    let connection = migrated();
    let loose = texts(
        &connection,
        "SELECT name FROM pragma_table_list WHERE schema = 'main' AND type = 'table' \
         AND strict = 0 AND name NOT LIKE 'sqlite_%' ORDER BY name",
        [],
    );
    assert!(loose.is_empty(), "not STRICT: {loose:?}");
    let times = texts(
        &connection,
        "SELECT m.name || '.' || c.name || ' ' || c.type \
         FROM sqlite_master m, pragma_table_info(m.name) c \
         WHERE m.type = 'table' AND c.name LIKE '%!_at' ESCAPE '!' \
         AND upper(c.type) <> 'INTEGER' ORDER BY 1",
        [],
    );
    assert!(times.is_empty(), "times that are not integers: {times:?}");
}
