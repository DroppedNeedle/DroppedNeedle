//! Migration tests: the import-application migration.
//!
//! The stage plan reserved the 0002 slot for this; 0002 and 0003 landed
//! first, so the import record is migration 0004. Tests cover a fresh
//! migrate plus an upgrade over a database migrated through 0003.

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use droppedneedle_kernel::schema::{MIGRATOR, apply_migrations, latest_version};

async fn scratch_pool() -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap()
}

#[tokio::test]
async fn fresh_migrate_creates_import_runs() {
    let pool = scratch_pool().await;
    apply_migrations(&pool).await.unwrap();

    assert!(latest_version() >= 5);
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, latest_version());
    let table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'import_runs'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(table.as_deref(), Some("import_runs"));
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('import_runs') ORDER BY cid")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        columns,
        vec![
            "id",
            "instance_id",
            "exported_at",
            "exit_code",
            "entity_counts",
            "applied_at"
        ]
    );
    let index: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_import_runs_instance'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(index.as_deref(), Some("idx_import_runs_instance"));
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
    base.run(pool).await.unwrap();
    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(stamped, version);
}

#[tokio::test]
async fn upgrade_from_0001_keeps_data() {
    let pool = scratch_pool().await;
    migrate_through(&pool, 1).await;

    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'U', 'user', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, latest_version());
    let name: String = sqlx::query_scalar("SELECT display_name FROM auth_users WHERE id = 'u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "U");
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
}

/// An album a v2 curator pinned to an edition and left unmanaged comes
/// over with both: the pin is the album's chosen edition and its files are
/// queued to be placed on it, and placing them changes nothing else. The
/// album stays unmanaged, chosen by the same person at the same time. A
/// pinned album with an active custom edition is not queued.
#[tokio::test]
async fn pinned_album_left_unmanaged_in_v2_stays_so_once_its_files_are_placed() {
    use std::sync::Arc;

    use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto, Secret};
    use droppedneedle_library::identify::models::RecallResult;
    use droppedneedle_library::matching::{Release, ReleaseTrack};
    use droppedneedle_tool::tooling::fixture::{
        MUSIC_ROOT_ID, V2_ALBUM_ID, build_v2_fixture, delete_orphan_rows,
    };

    use crate::common::library::library_at;

    const PASSPHRASE: &str = "operator-passphrase";
    const PINNED: &str = "rel-blue-jp";
    /// A pinned album that also has an active custom edition.
    const CUSTOM: &str = "v2-album-custom";
    let scratch = crate::common::ScratchDir::new("import-pinned-unmanaged");
    let root = scratch.to_path_buf();
    let v2_root = root.join("v2");
    let fixture = build_v2_fixture(&v2_root).expect("fixture builds");
    let v2_db = v2_root.join("cache").join("library.db");
    delete_orphan_rows(&v2_db).expect("repair runs");
    let alice = fixture.alice_id;
    rusqlite::Connection::open(&v2_db)
        .expect("v2 db")
        .execute_batch(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, release_group_mbid, \
             release_mbid, decision_source, selected_at) \
             VALUES ('{V2_ALBUM_ID}', 'rg-blue', 'rel-blue-us', 'automatic', 1700000500.0); \
             INSERT INTO library_album_release_pins (local_album_id, release_group_mbid, \
             release_mbid, set_by_user_id, set_at) \
             VALUES ('{V2_ALBUM_ID}', 'rg-blue', '{PINNED}', '{alice}', '2025-06-01T00:00:00Z'); \
             CREATE TABLE IF NOT EXISTS library_management_exclusions ( \
             local_album_id TEXT PRIMARY KEY, reason TEXT NOT NULL, \
             excluded_by_user_id TEXT NOT NULL, excluded_at REAL NOT NULL, \
             row_revision INTEGER NOT NULL DEFAULT 1); \
             INSERT INTO library_management_exclusions (local_album_id, reason, \
             excluded_by_user_id, excluded_at) \
             VALUES ('{V2_ALBUM_ID}', 'leave_unmanaged', '{alice}', 1700000600.0); \
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
             created_at, updated_at) VALUES ('{CUSTOM}', '{MUSIC_ROOT_ID}', 'k-custom', \
             'Giant Steps', 'giant steps', 'John Coltrane', 'john coltrane', 'v2-artist-1', \
             'automatic', 1700000000.0, 1700000000.0); \
             INSERT INTO local_album_external_identities (local_album_id, release_group_mbid, \
             release_mbid, decision_source, selected_at) \
             VALUES ('{CUSTOM}', 'rg-giant', 'rel-giant-us', 'automatic', 1700000500.0); \
             INSERT INTO library_album_release_pins (local_album_id, release_group_mbid, \
             release_mbid, set_by_user_id, set_at) \
             VALUES ('{CUSTOM}', 'rg-giant', 'rel-giant-jp', '{alice}', '2025-06-01T00:00:00Z'); \
             CREATE TABLE IF NOT EXISTS library_custom_edition_manifests ( \
             id TEXT PRIMARY KEY, local_album_id TEXT NOT NULL, version INTEGER NOT NULL, \
             release_group_mbid TEXT NOT NULL, album_title TEXT NOT NULL, \
             album_artist_name TEXT NOT NULL, artist_mbid TEXT, \
             album_metadata_json TEXT NOT NULL DEFAULT '{{}}', \
             source_album_revision INTEGER NOT NULL, source_identity_revision INTEGER, \
             input_revision TEXT NOT NULL, content_hash TEXT NOT NULL, \
             selected_candidate_key TEXT, sealed_by_user_id TEXT NOT NULL, \
             sealed_at REAL NOT NULL); \
             INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
             release_group_mbid, album_title, album_artist_name, source_album_revision, \
             input_revision, content_hash, sealed_by_user_id, sealed_at) VALUES \
             ('custom-1', '{CUSTOM}', 1, 'rg-giant', 'Giant Steps', 'John Coltrane', 1, \
             'in-1', 'hash-1', '{alice}', 1700000700.0); \
             CREATE TABLE IF NOT EXISTS library_custom_edition_active ( \
             local_album_id TEXT PRIMARY KEY, manifest_id TEXT NOT NULL, \
             activated_at REAL NOT NULL, row_revision INTEGER NOT NULL DEFAULT 1); \
             INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
             activated_at) VALUES ('{CUSTOM}', 'custom-1', 1700000700.0);"
        ))
        .expect("v2 pins, exclusion and custom edition");

    let export_path = root.join("export.json");
    droppedneedle_tool::export::export_v2_to_file(
        &droppedneedle_tool::export::ExportRequest {
            v2: droppedneedle_tool::export::v2dir::V2Dirs::under_root(&v2_root),
            db_path: None,
            passphrase: Secret::new(PASSPHRASE.to_owned()),
            exported_at: None,
            v2_commit: None,
        },
        &export_path,
    )
    .expect("export writes");
    let v3 = root.join("v3");
    let config_dir = v3.join("config");
    std::fs::create_dir_all(&config_dir).expect("v3 config dir");
    std::fs::create_dir_all(v3.join("cache")).expect("v3 cache dir");
    let db_path = v3.join("cache").join("library.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite:{}?mode=rwc", db_path.display()))
        .await
        .expect("pool opens");
    apply_migrations(&pool).await.expect("schema migrates");
    let report =
        droppedneedle_tool::r#import::run_import(droppedneedle_tool::r#import::ImportRequest {
            export_bytes: std::fs::read(&export_path).expect("export reads"),
            passphrase: PASSPHRASE.to_owned(),
            pool: pool.clone(),
            legacy: droppedneedle_kernel::runtime_config::LegacyConfig::at(&config_dir, false),
            crypto: Crypto::load_or_generate(&config_dir).expect("v3 key").0,
            v2_config_path: None,
            attachments_dir: Some(root.clone()),
            cache_dir: Some(v3.join("cache")),
            dry_run: false,
            fault_before_commit: false,
            fault_after_commit: false,
            fault_after_sections: None,
        })
        .await;
    assert!(
        matches!(
            report.exit.code,
            droppedneedle_tool::r#import::ExitCode::Ok
                | droppedneedle_tool::r#import::ExitCode::OkWithDrops
        ),
        "{:?}",
        report.exit
    );
    pool.close().await;

    let db = droppedneedle_kernel::db::open_connection(&db_path).expect("v3 db opens");
    let chosen = || -> (String, Option<String>, Option<String>, f64) {
        db.query_row(
            "SELECT decision_source, release_mbid, selected_by_user_id, selected_at \
             FROM local_album_external_identities WHERE local_album_id = ?1",
            [V2_ALBUM_ID],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("identity row")
    };
    let carried = chosen();
    assert_eq!(
        carried,
        (
            "manual".to_owned(),
            Some(PINNED.to_owned()),
            Some(alice.clone()),
            1700000500.0
        )
    );
    let queued: String = db
        .query_row(
            "SELECT origin FROM library_edition_remap_queue WHERE local_album_id = ?1",
            [V2_ALBUM_ID],
            |row| row.get(0),
        )
        .expect("queued");
    assert_eq!(queued, "place_only");
    let custom: (i64, i64) = db
        .query_row(
            "SELECT (SELECT COUNT(*) FROM library_custom_edition_active \
             WHERE local_album_id = ?1), \
             (SELECT COUNT(*) FROM library_edition_remap_queue WHERE local_album_id = ?1)",
            [CUSTOM],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("custom edition album reads");
    assert_eq!(
        custom,
        (1, 0),
        "a pinned album with a custom edition is not queued"
    );

    let handle = droppedneedle_kernel::db::Db::open_for_tests(&db_path).expect("database opens");
    let config = Arc::new(
        ConfigStore::open_blocking(
            handle.clone(),
            Crypto::load(&config_dir).expect("v3 key loads"),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
            &droppedneedle_kernel::runtime_config::LegacyConfig::None,
        )
        .expect("settings open"),
    );
    let platform = crate::common::TestPlatform::on(&handle, &config_dir)
        .settings(config)
        .build()
        .await;
    let (library, scripted) = library_at(&platform);
    let facts = library
        .identify_store
        .album_facts(V2_ALBUM_ID)
        .unwrap()
        .expect("album facts");
    scripted.set_recall(RecallResult {
        releases: vec![Release {
            id: PINNED.to_owned(),
            release_group_id: "rg-blue".to_owned(),
            title: facts.title.clone(),
            tracks: facts
                .tracks
                .iter()
                .enumerate()
                .map(|(index, track)| ReleaseTrack {
                    id: format!("rt-{index}"),
                    recording_id: track
                        .recording_mbid
                        .clone()
                        .unwrap_or_else(|| format!("rec-{index}")),
                    title: track.title.clone(),
                    disc: track.disc_number.max(1),
                    position: track.track_number,
                    absolute_position: index as u32 + 1,
                    length_ms: track.duration_secs.map(|seconds| seconds * 1000),
                    ..ReleaseTrack::default()
                })
                .collect(),
            ..Release::default()
        }],
        ..RecallResult::default()
    });
    assert!(
        droppedneedle_library::wiring::LibraryServices::build(&library)
            .operations
            .remap_next()
            .await
            .expect("remap step")
    );

    let placed: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM local_track_external_identities i \
             JOIN local_tracks t ON t.id = i.local_track_id \
             WHERE t.local_album_id = ?1 AND i.release_mbid = ?2",
            [V2_ALBUM_ID, PINNED],
            |row| row.get(0),
        )
        .expect("placements read");
    assert!(placed > 0, "the files are on the pinned edition");
    assert_eq!(
        chosen(),
        carried,
        "chosen by the same person, at the same time"
    );
    let excluded: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_management_exclusions WHERE local_album_id = ?1",
            [V2_ALBUM_ID],
            |row| row.get(0),
        )
        .expect("exclusion reads");
    assert_eq!(excluded, 1, "still left unmanaged");
}

#[tokio::test]
async fn upgrade_over_0003_keeps_data() {
    let pool = scratch_pool().await;
    migrate_through(&pool, 3).await;

    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'U', 'user', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, latest_version());
    let name: String = sqlx::query_scalar("SELECT display_name FROM auth_users WHERE id = 'u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "U");
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
}
