//! Schema: fresh migrate, re-run, upgrade from every earlier version,
//! legacy exclusions, merged tables, and foreign-key enforcement. The
//! scratch databases live only in memory, except the baseline upgrade,
//! which runs through the production runtime on a file.

use droppedneedle_kernel::db::{DbConfig, open_runtime};
use droppedneedle_kernel::schema::{MIGRATOR, apply_migrations, assert_migrated, latest_version};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

/// One-connection in-memory pool. A single connection keeps `:memory:`
/// on one database; the pool exists so the briefs exercise the same
/// `SqlitePool` surface the runtime uses.
async fn scratch_pool() -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap()
}

async fn migrated_pool() -> SqlitePool {
    let pool = scratch_pool().await;
    apply_migrations(&pool).await.unwrap();
    pool
}

async fn table_names(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations' \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn column_names(pool: &SqlitePool, table: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info(?) ORDER BY cid")
        .bind(table)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Fresh migrate stamps the latest version, records one row per migration,
/// singleton and sentinel rows the services expect to exist.
#[tokio::test]
async fn fresh_migrate_marks_version_and_seeds() {
    let pool = migrated_pool().await;

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, latest_version());
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied, migration_count());

    assert_eq!(table_names(&pool).await.len(), 155);
    let triggers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(triggers, 37);

    let catalog: (i64, i64) =
        sqlx::query_as("SELECT singleton, value FROM library_catalog_revision")
            .fetch_one(&pool)
            .await
            .unwrap();
    // 0026 bumps the revision once so caches drop pre-edition reads.
    assert_eq!(catalog, (1, 1));
    let sentinels: Vec<(String, String)> =
        sqlx::query_as("SELECT id, kind FROM local_artists ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        sentinels,
        vec![
            (
                "00000000-0000-4000-8000-000000000001".to_owned(),
                "various_artists".to_owned(),
            ),
            (
                "00000000-0000-4000-8000-000000000002".to_owned(),
                "unknown".to_owned(),
            ),
        ]
    );
    let wakeups: Vec<String> =
        sqlx::query_scalar("SELECT channel FROM durable_work_wakeups ORDER BY channel")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        wakeups,
        vec!["contribution", "identification", "operation", "scan"]
    );
    for (table, want) in [
        ("library_event_stream_revisions", 3),
        ("download_activity_global_revision", 1),
    ] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, want, "seed rows in {table}");
    }
}

/// Re-running the migrator is a clean no-op: version stable, seeds single.
#[tokio::test]
async fn migrate_rerun_is_clean() {
    let pool = migrated_pool().await;
    apply_migrations(&pool).await.unwrap();

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, latest_version());
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied, migration_count());
    let artists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_artists")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(artists, 2);
}

/// D1/D13/D14 exclusions: no lidarr marker, no rebuild leftovers, no legacy
/// catalog tables, no transient swap tables.
#[tokio::test]
async fn legacy_and_swap_tables_are_absent() {
    let pool = migrated_pool().await;
    let tables = table_names(&pool).await;

    for name in [
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
                && !name.ends_with("__resolved_v1"),
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
#[tokio::test]
async fn dual_owner_tables_carry_the_merged_shape() {
    let pool = migrated_pool().await;

    for (table, columns) in [
        (("playlists"), vec!["source_ref", "user_id", "is_public"]),
        (("auth_users"), vec!["username", "username_display"]),
        (("auth_tokens"), vec!["last_seen_at", "session_kind"]),
        (("local_albums"), vec!["management_schedule_pending"]),
        (
            ("local_tracks"),
            vec!["release_type", "tag_album_title", "tag_album_artist_name"],
        ),
        (
            ("library_identity_repair_findings"),
            vec![
                "suggested_release_mbid",
                "suggested_release_group_mbid",
                "suggested_edition_json",
            ],
        ),
        (
            ("library_scan_runs"),
            vec!["phase_started_at", "phase_timings_json"],
        ),
        (("user_listening_prefs"), vec!["auto_request_personal_mix"]),
        (
            ("request_history"),
            vec!["dispatch_authorized", "generation", "request_kind"],
        ),
    ] {
        let have = column_names(&pool, table).await;
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
            column_names(&pool, table)
                .await
                .contains(&"provider_base_url".to_owned()),
            "{table}.provider_base_url missing"
        );
    }

    // Store-side auth FKs won the merge.
    for (table, column) in [
        ("compat_bookmarks", "user_id"),
        ("compat_play_queues", "user_id"),
    ] {
        let parents: Vec<(i64, i64, String)> =
            sqlx::query_as("SELECT id, seq, \"table\" FROM pragma_foreign_key_list(?)")
                .bind(table)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            parents.iter().any(|parent| parent.2 == "auth_users"),
            "{table}.{column} must reference auth_users"
        );
    }

    // Merged revision row: discovery DEFAULT 0 plus native range CHECK.
    let default: Option<String> = sqlx::query_scalar(
        "SELECT dflt_value FROM pragma_table_info('library_catalog_revision') \
         WHERE name = 'value'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(default.as_deref(), Some("0"));
    let sql: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = 'library_catalog_revision'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(sql.contains("BETWEEN 0 AND 9223372036854775807"), "{sql}");

    // Per-user playlist uniqueness survived the merge.
    let index_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE name = 'idx_playlists_user_source_ref'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(index_sql.contains("UNIQUE"), "{index_sql}");
}

/// Declared FKs bite under enforcement: orphans fail, cascades clean up.
#[tokio::test]
async fn foreign_keys_bite_under_enforcement() {
    let pool = migrated_pool().await;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();

    let orphan = sqlx::query(
        "INSERT INTO compat_bookmarks (user_id, file_id, position_ms, created_at, changed_at) \
         VALUES ('ghost', 'f1', 0, 1, 1)",
    )
    .execute(&pool)
    .await;
    assert!(orphan.is_err(), "orphan row must fail with FKs on");

    sqlx::query("INSERT INTO auth_users (id, display_name, created_at) VALUES ('u1', 'T', 'now')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO compat_bookmarks (user_id, file_id, position_ms, created_at, changed_at) \
         VALUES ('u1', 'f1', 0, 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM auth_users WHERE id = 'u1'")
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM compat_bookmarks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "bookmarks cascade with their user");
}

/// Every earlier schema upgrades to the latest one in place and keeps its
/// rows: migrate through version N, write an account, apply the rest.
#[tokio::test]
async fn every_earlier_version_upgrades_and_keeps_data() {
    for version in 1..latest_version() {
        let pool = scratch_pool().await;
        let through = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                MIGRATOR
                    .migrations
                    .iter()
                    .filter(|migration| migration.version <= version)
                    .cloned()
                    .collect(),
            ),
            ignore_missing: false,
            locking: true,
            no_tx: false,
        };
        through.run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('u1', 'Kept', 'user', '2024-01-01T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        apply_migrations(&pool).await.unwrap();
        assert_migrated(&pool).await.unwrap();
        let name: String =
            sqlx::query_scalar("SELECT display_name FROM auth_users WHERE id = 'u1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "Kept", "upgrade from {version}");
    }
}

/// Migration 0013 gives albums indexed before it their persistent key:
/// the release MBID when a track carries one, the tagged names when both
/// came from tags, and the old folder key otherwise.
#[tokio::test]
async fn album_keys_backfill_on_upgrade() {
    let pool = scratch_pool().await;
    let through = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 13)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    through.run(&pool).await.unwrap();
    let artist = "00000000-0000-4000-8000-000000000002";
    for (album, title, provenance, mbid) in [
        ("tagged-mbid", "first", "tag", Some(" ABC-1 ")),
        ("tagged-names", "second", "tag", None),
        ("from-path", "third", "parsed", None),
    ] {
        sqlx::query(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
             created_at, updated_at) VALUES (?1, 'r', 'dir' || char(0) || ?2, ?2, ?2, \
             'artist', 'artist', ?3, 'automatic', 0, 0)",
        )
        .bind(album)
        .bind(title)
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, embedded_release_mbid, file_format, \
             ingest_source, imported_at, membership_source, album_title_provenance, \
             album_artist_provenance) VALUES (?1, ?1, 'r', ?1, ?1, ?1, 1, 1, '1', 't', 't', \
             ?2, ?2, ?3, 'flac', 'scan', 0, 'automatic', ?4, ?4)",
        )
        .bind(album)
        .bind(title)
        .bind(mbid)
        .bind(provenance)
        .execute(&pool)
        .await
        .unwrap();
    }

    apply_migrations(&pool).await.unwrap();

    let keys: Vec<(String, String)> =
        sqlx::query_as("SELECT id, grouping_key FROM local_albums ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        keys,
        vec![
            ("from-path".to_owned(), "dir\0third".to_owned()),
            ("tagged-mbid".to_owned(), "mbid:abc-1".to_owned()),
            (
                "tagged-names".to_owned(),
                "tag:artist\u{1f}second".to_owned()
            ),
        ]
    );
}

/// The album art migration rebuilds `local_album_artwork`: a row written
/// before it survives with its version, and the three genre artwork
/// triggers come back.
#[tokio::test]
async fn album_art_rebuild_keeps_rows_and_triggers() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 15)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_album_artwork (local_album_id, source, source_locator, version, \
         updated_at) VALUES ('al1', 'provider', 'rg-1', 4, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    let kept: (String, Option<String>, i64) = sqlx::query_as(
        "SELECT source, source_locator, version FROM local_album_artwork \
         WHERE local_album_id = 'al1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kept, ("provider".to_owned(), Some("rg-1".to_owned()), 4));
    let triggers: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'trigger' \
         AND tbl_name = 'local_album_artwork' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        triggers,
        vec![
            "trg_genre_artwork_artwork_delete",
            "trg_genre_artwork_artwork_insert",
            "trg_genre_artwork_artwork_update",
        ]
    );
}

/// Migration 0040 drops the tables nothing uses and the triggers that fed
/// them. Rows in the tables that stay survive, a repair finding loses only
/// its unused evidence column, and follows still save once the triggers
/// that wrote the dropped tables are gone.
#[tokio::test]
async fn unused_tables_drop_and_live_rows_survive() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 40)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    // The finding's identity revision holds text, which the rebuilt table
    // would refuse uncast.
    sqlx::raw_sql(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'Kept', 'user', '2024-01-01T00:00:00+00:00'); \
         INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, followed_at, updated_at) VALUES ('u1', 'M1', 'm1', 'Band', 1, 1); \
         INSERT INTO play_history (id, user_id, track_name, artist_name, played_at) \
         VALUES ('h1', 'u1', 't', 'a', 'now'); \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO library_operation_jobs (id, kind, state, created_at, updated_at) \
         VALUES ('j1', 'repair', 'succeeded', 1, 1); \
         INSERT INTO library_identity_repair_findings (id, job_id, local_album_id, \
         expected_album_revision, expected_identity_revision, finding_code, confidence, \
         created_at, updated_at) VALUES ('f1', 'j1', 'al1', 7, 'unknown', \
         'exact_release_suggested', 'complete', 1.5, 2.5);",
    )
    .execute(&pool)
    .await
    .unwrap();
    let due: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM follow_due")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(due, 1, "the follow trigger filled a table about to go");

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let tables = table_names(&pool).await;
    for gone in [
        "play_history",
        "follow_due",
        "follow_inventory",
        "download_landed_groups",
        "library_identification_evidence",
    ] {
        assert!(!tables.contains(&gone.to_owned()), "{gone} must be dropped");
    }
    let dead_triggers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' \
         AND (name LIKE 'follow!_%' ESCAPE '!' OR name LIKE 'download!_landed!_%' ESCAPE '!')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(dead_triggers, 0);

    // A trigger left behind would fail these writes on its missing table.
    sqlx::raw_sql(
        "INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, followed_at, updated_at) VALUES ('u1', 'M2', 'm2', 'Other', 2, 2); \
         INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, requested_at) VALUES ('u1', 'M2', 'm2', 'Other', 2); \
         DELETE FROM user_followed_artists WHERE artist_mbid_lower = 'm2';",
    )
    .execute(&pool)
    .await
    .unwrap();
    let follows: Vec<String> =
        sqlx::query_scalar("SELECT artist_mbid_lower FROM user_followed_artists")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(follows, vec!["m1"]);

    let finding: (String, String, i64, i64, String, f64, f64) = sqlx::query_as(
        "SELECT job_id, local_album_id, expected_album_revision, expected_identity_revision, \
         state, created_at, updated_at FROM library_identity_repair_findings WHERE id = 'f1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        finding,
        (
            "j1".to_owned(),
            "al1".to_owned(),
            7,
            0,
            "open".to_owned(),
            1.5,
            2.5
        )
    );
    assert!(
        !column_names(&pool, "library_identity_repair_findings")
            .await
            .contains(&"evidence_id".to_owned())
    );
    let broken: Vec<(String,)> = sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(broken.is_empty(), "{broken:?}");
}

/// Migration 0041 drops the tables no code fills, children before their
/// parents and whether or not a row is in them. Artist and album aliases
/// stay, and no table left behind points at one that went.
#[tokio::test]
async fn empty_tables_drop_and_aliases_survive() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 41)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_artist_aliases (alias, local_artist_id, kind, created_at) \
         VALUES ('a-old', 'a1', 'merged_artist', 1); \
         INSERT INTO local_album_aliases (alias, local_album_id, kind, created_at) \
         VALUES ('al-old', 'al1', 'merged_album', 1); \
         INSERT INTO library_playlists (id, name, created_at, updated_at) \
         VALUES ('p1', 'Old', 'now', 'now'); \
         INSERT INTO library_playlist_tracks (id, playlist_id, position, track_name, \
         artist_name, album_name, source_type, created_at, local_artist_id) \
         VALUES ('e1', 'p1', 0, 't', 'a', 'b', 'local', 'now', 'a1');",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let tables = table_names(&pool).await;
    for gone in [
        "discovery_optional_progress",
        "user_favorites",
        "library_playlist_tracks",
        "library_playlists",
        "library_compat_id_map",
        "library_edition_conversion_jobs",
        "library_scan_grouping_groups",
        "library_scan_grouping_contexts",
        "library_management_blob_references",
        "library_file_mutation_journal",
        "library_management_operation_snapshots",
        "library_migration_provenance",
        "library_migration_runs",
    ] {
        assert!(!tables.contains(&gone.to_owned()), "{gone} must be dropped");
    }
    let aliases: Vec<(String, String)> = sqlx::query_as(
        "SELECT alias, local_artist_id FROM local_artist_aliases \
         UNION ALL SELECT alias, local_album_id FROM local_album_aliases ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        aliases,
        vec![
            ("a-old".to_owned(), "a1".to_owned()),
            ("al-old".to_owned(), "al1".to_owned())
        ]
    );
    let orphaned: Vec<(String, String)> = sqlx::query_as(
        "SELECT m.name, f.\"table\" FROM sqlite_master m, pragma_foreign_key_list(m.name) f \
         WHERE m.type = 'table' \
         AND f.\"table\" NOT IN (SELECT name FROM sqlite_master WHERE type = 'table')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(orphaned.is_empty(), "{orphaned:?}");
}

/// Migration 0042 adds the index over a listener, a track and the play
/// time, and a small one over the plays that name an album and no track.
/// Every play row is still there afterwards, filed or not. A listener's
/// plays of one track are read from the first index alone, and their
/// plays of one album with no track are found through the second.
#[tokio::test]
async fn play_history_gains_the_listener_and_track_index() {
    const INDEX: &str = "idx_play_history_user_track";
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 42)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, ingest_source, imported_at, \
         membership_source) VALUES ('t1', 'al1', 'r1', '/m/t1', 't1', 'h1', 1, 1, 's1', \
         'One', 'one', 'T', 't', 'flac', 'scan', 1, 'automatic'); \
         INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         local_artist_id, track_name, artist_name, played_at) VALUES \
         ('p1', 'u1', 't1', 'al1', 'a1', 'One', 'A', '2026-01-01T00:00:00+00:00'), \
         ('p2', 'u1', NULL, NULL, NULL, 'By name', 'A', '2026-01-02T00:00:00+00:00'), \
         ('p3', 'u2', 't1', 'al1', 'a1', 'One', 'A', '2026-01-03T00:00:00+00:00'), \
         ('p4', 'u1', NULL, 'al1', 'a1', 'Gone', 'A', '2026-01-04T00:00:00+00:00');",
    )
    .execute(&pool)
    .await
    .unwrap();
    let rows = "SELECT id, user_id, COALESCE(local_track_id, ''), track_name, played_at \
                FROM library_play_history ORDER BY id";
    let kept: Vec<(String, String, String, String, String)> =
        sqlx::query_as(rows).fetch_all(&pool).await.unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_index_info(?) ORDER BY seqno")
            .bind(INDEX)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(columns, ["user_id", "local_track_id", "played_at"]);
    let after: Vec<(String, String, String, String, String)> =
        sqlx::query_as(rows).fetch_all(&pool).await.unwrap();
    assert_eq!(after, kept);
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT COUNT(*), MAX(played_at) FROM library_play_history \
         WHERE user_id = 'u1' AND local_track_id = 't1'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.3.contains(&format!("COVERING INDEX {INDEX}"))),
        "{plan:?}"
    );
    const BY_ALBUM: &str = "idx_play_history_user_album_no_track";
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT COUNT(*), MAX(played_at) FROM library_play_history \
         WHERE user_id = 'u1' AND local_track_id IS NULL AND local_album_id IN ('al1')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        plan.iter().any(|step| step
            .3
            .contains(&format!("USING INDEX {BY_ALBUM} (user_id=?"))),
        "{plan:?}"
    );
    let partial: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?")
            .bind(BY_ALBUM)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        partial.contains("WHERE local_track_id IS NULL AND local_album_id IS NOT NULL"),
        "{partial}"
    );
    // The older indexes stay: other reads and the merges use them.
    let older: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
         AND name LIKE 'idx_target_history_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(older, 4);
}

/// Migration 0043 adds the table that records two album rows which look
/// like one retagged album. A database from before it keeps its albums
/// and gains the table, empty. A pair goes with either of its albums and
/// never keeps one from being deleted.
#[tokio::test]
async fn retag_pairs_arrive_empty_and_go_with_their_albums() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 43)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('old', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('new', 'r1', 'g2', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('third', 'r1', 'g3', 'T', 't', 'a1', 'automatic', 1, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let count = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(count("SELECT COUNT(*) FROM local_albums").await, 3);
    assert_eq!(count("SELECT COUNT(*) FROM library_retag_pairs").await, 0);

    sqlx::raw_sql(
        "PRAGMA foreign_keys = ON; \
         INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
         recorded_at, updated_at) VALUES \
         ('old', 'new', 'mostly_missing', 'open', 1000, 1000), \
         ('third', 'old', 'store_error', 'open', 1000, 1000); \
         DELETE FROM local_albums WHERE id = 'new';",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(count("SELECT COUNT(*) FROM library_retag_pairs").await, 1);
    sqlx::raw_sql("DELETE FROM local_albums WHERE id = 'third'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count("SELECT COUNT(*) FROM library_retag_pairs").await, 0);
    assert_eq!(count("SELECT COUNT(*) FROM local_albums").await, 1);
}

/// The reasons and states a retag pair may carry in the database are
/// exactly the ones the code names, in both directions.
#[tokio::test]
async fn retag_pair_checks_list_exactly_the_code_values() {
    use droppedneedle_library::scan::sqlite_store::retag_pairs::{RetagPairReason, RetagPairState};

    let pool = migrated_pool().await;
    let sql: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = 'library_retag_pairs'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let listed = |column: &str| -> Vec<String> {
        let marker = format!("CHECK({column} IN (");
        let values = &sql[sql.find(&marker).expect("the column has a CHECK") + marker.len()..];
        values[..values.find(')').expect("the list ends")]
            .split(',')
            .map(|value| value.trim().trim_matches('\'').to_owned())
            .collect()
    };

    assert_eq!(
        listed("reason"),
        RetagPairReason::ALL.map(|reason| reason.as_str().to_owned())
    );
    assert_eq!(
        listed("state"),
        RetagPairState::ALL.map(|state| state.as_str().to_owned())
    );
    for reason in RetagPairReason::ALL {
        assert_eq!(RetagPairReason::parse(reason.as_str()), Some(reason));
    }
    for state in RetagPairState::ALL {
        assert_eq!(RetagPairState::parse(state.as_str()), Some(state));
    }
    assert_eq!(RetagPairReason::parse("open"), None);
    assert_eq!(RetagPairState::parse("store_error"), None);
}

/// Migration 0044 adds the table of albums the app stepped back from and
/// two columns. A database from before it keeps its rows, gains the table
/// empty and the columns NULL, so an upgrade marks nothing by itself. A
/// mark goes with its album and never keeps one from being deleted.
#[tokio::test]
async fn album_takeovers_arrive_empty_and_go_with_their_album() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 44)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, ingest_source, imported_at, \
         membership_source) VALUES ('t1', 'al1', 'r1', '/m/t1', 't1', 'h1', 1, 1, 's1', \
         'One', 'one', 'T', 't', 'flac', 'scan', 1, 'automatic'); \
         INSERT INTO library_track_management_state (local_track_id, managed_path_revision, \
         last_outcome) VALUES ('t1', 'v2-value', 'succeeded'); \
         INSERT INTO library_management_automatic_queue (local_album_id, queued_at, not_before) \
         VALUES ('al1', 1, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let count = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM library_album_takeovers").await,
        0
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM library_track_management_state WHERE local_track_id = 't1' \
             AND managed_path_revision = 'v2-value' AND written_sha256 IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM library_management_automatic_queue \
             WHERE local_album_id = 'al1' AND retag_followed_at IS NULL"
        )
        .await,
        1
    );

    sqlx::raw_sql(
        "PRAGMA foreign_keys = ON; \
         INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES ('al1', 'files_changed', 1000); \
         DELETE FROM library_track_management_state; \
         DELETE FROM local_tracks; \
         DELETE FROM local_albums WHERE id = 'al1';",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        count("SELECT COUNT(*) FROM library_album_takeovers").await,
        0
    );
    assert_eq!(count("SELECT COUNT(*) FROM local_albums").await, 0);
}

/// Migration 0045 adds the table of tracks that left an album, empty. An
/// album merged into one that was merged itself then points at the album
/// the chain ends on; a chain that loops is left as it is. The two kinds of
/// retag pair the scan now joins by itself are gone, the others stay.
#[tokio::test]
async fn departures_arrive_empty_and_merge_chains_end_on_a_live_album() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 45)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('last', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('middle', 'r1', 'g2', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('first', 'r1', 'g3', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('loop-a', 'r1', 'g4', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('loop-b', 'r1', 'g5', 'T', 't', 'a1', 'automatic', 1, 1); \
         UPDATE local_albums SET retired_into_album_id = 'last' WHERE id = 'middle'; \
         UPDATE local_albums SET retired_into_album_id = 'middle' WHERE id = 'first'; \
         UPDATE local_albums SET retired_into_album_id = 'loop-b' WHERE id = 'loop-a'; \
         UPDATE local_albums SET retired_into_album_id = 'loop-a' WHERE id = 'loop-b'; \
         INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
         recorded_at, updated_at) VALUES \
         ('first', 'last', 'mostly_missing', 'open', 1, 1), \
         ('middle', 'last', 'new_files_added', 'open', 1, 1), \
         ('loop-a', 'last', 'tracks_elsewhere', 'open', 1, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let retired_into = |id: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(retired_into("first").await.as_deref(), Some("last"));
    assert_eq!(retired_into("middle").await.as_deref(), Some("last"));
    assert_eq!(retired_into("last").await, None);
    assert_eq!(retired_into("loop-a").await.as_deref(), Some("loop-b"));
    assert_eq!(retired_into("loop-b").await.as_deref(), Some("loop-a"));
    let reasons: Vec<String> = sqlx::query_scalar("SELECT reason FROM library_retag_pairs")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(reasons, vec!["tracks_elsewhere".to_owned()]);
    let departures: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_album_departures")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(departures, 0);
}

/// Migration 0047 adds the track aliases, empty, on a database that had
/// none: a strict table whose alias belongs to its old track and whose
/// target must outlive it, an index on the target, and the version stamp.
#[tokio::test]
async fn track_aliases_arrive_empty_and_strict() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 47)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    assert!(
        !table_names(&pool)
            .await
            .contains(&"local_track_aliases".to_owned())
    );

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(version >= 47, "{version}");
    let strict: i64 = sqlx::query_scalar(
        "SELECT strict FROM pragma_table_list WHERE name = 'local_track_aliases'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(strict, 1);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_track_aliases")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let mut keys: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT \"from\", \"table\", \"to\", on_delete \
         FROM pragma_foreign_key_list('local_track_aliases')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            (
                "alias".to_owned(),
                "local_tracks".to_owned(),
                "id".to_owned(),
                "CASCADE".to_owned()
            ),
            (
                "local_track_id".to_owned(),
                "local_tracks".to_owned(),
                "id".to_owned(),
                "RESTRICT".to_owned()
            ),
        ]
    );
    let indexed: Vec<String> = sqlx::query_scalar(
        "SELECT i.name FROM pragma_index_list('local_track_aliases') i \
         JOIN pragma_index_info(i.name) c WHERE c.name = 'local_track_id'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(indexed, vec!["idx_local_track_aliases_track".to_owned()]);
}

/// Migration 0048 gives the edition undo a place for the album's hold. An
/// undo kept before it keeps every column and has no hold, so undoing it
/// restores none.
#[tokio::test]
async fn edition_undo_keeps_its_rows_and_gains_the_hold() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 48)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    assert!(
        !column_names(&pool, "library_edition_choice_undo")
            .await
            .contains(&"prior_hold_json".to_owned())
    );
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO library_edition_choice_undo (id, local_album_id, action_id, \
         prior_identity_json, prior_tracks_json, prior_match_state_json, \
         expected_identity_revision, created_at) \
         VALUES ('u1', 'al1', 'act-1', '{\"release_group_mbid\":\"rg\"}', '[]', NULL, 3, 4.5);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    type UndoRow = (
        String,
        String,
        Option<String>,
        String,
        i64,
        f64,
        Option<String>,
    );
    let rows: Vec<UndoRow> = sqlx::query_as(
        "SELECT id, action_id, prior_identity_json, prior_tracks_json, \
         expected_identity_revision, created_at, prior_hold_json \
         FROM library_edition_choice_undo",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(
            "u1".to_owned(),
            "act-1".to_owned(),
            Some("{\"release_group_mbid\":\"rg\"}".to_owned()),
            "[]".to_owned(),
            3,
            4.5,
            None
        )]
    );
}

/// Migration 0049 marks whether a track alias takes the old row's field
/// overrides along. Every alias made before it continued a track into the
/// same track, so it keeps doing so.
#[tokio::test]
async fn track_aliases_keep_moving_overrides() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 49)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, ingest_source, imported_at, \
         membership_source) VALUES \
         ('old', 'al1', 'r1', '/m/1.flac', '1.flac', 'h1', 1, 0, 's', 'T', 't', 'T', 't', \
         'flac', 'scan', 0, 'automatic'), \
         ('new', 'al1', 'r1', '/m/2.flac', '2.flac', 'h2', 1, 0, 's', 'T', 't', 'T', 't', \
         'flac', 'scan', 0, 'automatic'); \
         INSERT INTO local_track_aliases (alias, local_track_id, created_at) \
         VALUES ('old', 'new', 7);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT alias, local_track_id, created_at, moves_overrides FROM local_track_aliases",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![("old".to_owned(), "new".to_owned(), 7, 1)]);
    let refused = sqlx::query("UPDATE local_track_aliases SET moves_overrides = 2")
        .execute(&pool)
        .await;
    assert!(refused.is_err(), "only 0 or 1");
}

/// Migration 0050 records the MusicBrainz artist id each credit's file
/// gave and the id an automatic merge proved. Credits and artists made
/// before it keep every row with no id; only a lowercased 36-character id
/// is accepted; the four lookups are partial indexes.
#[tokio::test]
async fn artist_credits_gain_their_evidence() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 50)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, ingest_source, imported_at, \
         membership_source) VALUES \
         ('t1', 'al1', 'r1', '/m/1.flac', '1.flac', 'h1', 1, 0, 's', 'T', 't', 'T', 't', \
         'flac', 'scan', 0, 'automatic'); \
         INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role, \
         credited_name, join_phrase) VALUES ('t1', 0, 'a1', 'main', 'A', ''); \
         INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
         credited_name, join_phrase) VALUES ('al1', 0, 'a1', 'main', 'A', '');",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let kept: (
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT t.local_artist_id, t.artist_mbid, b.local_artist_id, b.artist_mbid, \
         a.converged_mbid FROM local_track_artists t, local_album_artists b, local_artists a \
         WHERE a.id = 'a1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kept, ("a1".to_owned(), None, "a1".to_owned(), None, None));
    let good = "aaaaaaaa-0000-4000-8000-000000000001";
    for (table, column) in [
        ("local_track_artists", "artist_mbid"),
        ("local_album_artists", "artist_mbid"),
        ("local_artists", "converged_mbid"),
    ] {
        for bad in ["AAAAAAAA-0000-4000-8000-000000000001", "not-an-id"] {
            let refused = sqlx::query(&format!("UPDATE {table} SET {column} = ?1"))
                .bind(bad)
                .execute(&pool)
                .await;
            assert!(refused.is_err(), "{table}.{column} refuses {bad}");
        }
        sqlx::query(&format!("UPDATE {table} SET {column} = ?1"))
            .bind(good)
            .execute(&pool)
            .await
            .unwrap();
    }
    let indexes: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_master WHERE type = 'index' \
         AND name IN ('idx_local_track_artists_mbid', 'idx_local_track_artists_artist_mbid', \
         'idx_local_album_artists_mbid', 'idx_local_album_artists_artist_mbid') ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(indexes.len(), 4);
    for (name, sql) in indexes {
        assert!(
            sql.contains("WHERE artist_mbid IS NOT NULL"),
            "{name} is partial"
        );
    }
}

/// Migration 0051 records who applied a published operation. Operations
/// from before it keep no author, so only an admin may undo them, and
/// deleting the author clears it again.
#[tokio::test]
async fn publish_operations_keep_their_rows_and_gain_an_author() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 51)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO library_publish_operations (bundle_id, bundle_json, created_day) \
         VALUES ('old', '{}', 7); \
         INSERT INTO auth_users (id, display_name, created_at) VALUES ('u1', 'T', 'now');",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO library_publish_operations \
         (bundle_id, bundle_json, created_day, applied_by_user_id) VALUES ('new', '{}', 8, 'u1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let authors = || async {
        sqlx::query_as::<_, (String, i64, Option<String>)>(
            "SELECT bundle_id, created_day, applied_by_user_id FROM library_publish_operations \
             ORDER BY bundle_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    assert_eq!(
        authors().await,
        vec![
            ("new".to_owned(), 8, Some("u1".to_owned())),
            ("old".to_owned(), 7, None),
        ]
    );
    sqlx::query("DELETE FROM auth_users WHERE id = 'u1'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        authors().await,
        vec![("new".to_owned(), 8, None), ("old".to_owned(), 7, None)]
    );
}

/// The causes a takeover may carry in the database are exactly the ones
/// the code names, in both directions.
#[tokio::test]
async fn takeover_check_lists_exactly_the_code_values() {
    use droppedneedle_library::management::takeover::Cause;

    let pool = migrated_pool().await;
    let sql: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = 'library_album_takeovers'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let marker = "CHECK(cause IN (";
    let values = &sql[sql.find(marker).expect("the column has a CHECK") + marker.len()..];
    let listed: Vec<String> = values[..values.find(')').expect("the list ends")]
        .split(',')
        .map(|value| value.trim().trim_matches('\'').to_owned())
        .collect();
    assert_eq!(listed, Cause::ALL.map(|cause| cause.as_str().to_owned()));
    for cause in Cause::ALL {
        assert_eq!(Cause::parse(cause.as_str()), Some(cause));
    }
    assert_eq!(Cause::parse("your_choice"), None);
}

/// Migration 0046 rebuilds the edition remap queue to take a third origin.
/// A database from before it keeps every row with its origin; the new
/// origin is accepted after it, and one nobody named is still refused.
#[tokio::test]
async fn remap_queue_keeps_its_rows_and_takes_place_only() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 46)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('al2', 'r1', 'g2', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('al3', 'r1', 'g3', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('al4', 'r1', 'g4', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, \
         chosen_by_user_id, queued_at, attempts, not_before, last_code, origin) VALUES \
         ('al1', 'rel-1', 'u1', 1.5, 2, 9.5, 'MUSICBRAINZ_UNAVAILABLE', 'album_pin'), \
         ('al2', 'rel-2', NULL, 2.5, 0, 0, NULL, 'group_pin');",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    type QueueRow = (String, String, f64, i64, f64, Option<String>, String);
    let rows: Vec<QueueRow> = sqlx::query_as(
        "SELECT local_album_id, release_mbid, queued_at, attempts, not_before, last_code, \
         origin FROM library_edition_remap_queue ORDER BY local_album_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "al1".to_owned(),
                "rel-1".to_owned(),
                1.5,
                2,
                9.5,
                Some("MUSICBRAINZ_UNAVAILABLE".to_owned()),
                "album_pin".to_owned()
            ),
            (
                "al2".to_owned(),
                "rel-2".to_owned(),
                2.5,
                0,
                0.0,
                None,
                "group_pin".to_owned()
            ),
        ]
    );
    sqlx::raw_sql(
        "INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, queued_at, \
         origin) VALUES ('al3', 'rel-3', 3, 'place_only');",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        sqlx::raw_sql(
            "INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, queued_at, \
             origin) VALUES ('al4', 'rel-4', 4, 'x');",
        )
        .execute(&pool)
        .await
        .is_err(),
        "an origin nobody named is refused"
    );
    let default: String = sqlx::query_scalar(
        "SELECT dflt_value FROM pragma_table_info('library_edition_remap_queue') \
         WHERE name = 'origin'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(default, "'album_pin'");
    let chosen_by: Option<String> = sqlx::query_scalar(
        "SELECT chosen_by_user_id FROM library_edition_remap_queue WHERE local_album_id = 'al1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(chosen_by.as_deref(), Some("u1"));
    let sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE name = 'library_edition_remap_queue'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        sql.contains("REFERENCES local_albums(id) ON DELETE CASCADE"),
        "{sql}"
    );
}

/// Migration 0052 rebuilds the takeover marks and the retag pairs to take
/// `replaced_hands_off`. A database from before it keeps every row and
/// column; the new value is taken by both, one nobody named is refused by
/// both, both still go with their album, stay STRICT and keep the index.
#[tokio::test]
async fn takeovers_and_retag_pairs_take_replaced_hands_off() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 52)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('al2', 'r1', 'g2', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('al3', 'r1', 'g3', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('al4', 'r1', 'g4', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at, \
         handed_back_at, hand_back_tries, hand_back_retry_at) VALUES \
         ('al1', 'files_changed', 10, 20, 2, 30), ('al2', 'run_undone', 11, NULL, 0, NULL); \
         INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
         recorded_at, updated_at) VALUES ('al1', 'al2', 'tracks_elsewhere', 'dismissed', 5, 6);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    type Mark = (String, String, i64, Option<i64>, i64, Option<i64>);
    let marks: Vec<Mark> = sqlx::query_as(
        "SELECT local_album_id, cause, noticed_at, handed_back_at, hand_back_tries, \
         hand_back_retry_at FROM library_album_takeovers ORDER BY local_album_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        marks,
        vec![
            (
                "al1".to_owned(),
                "files_changed".to_owned(),
                10,
                Some(20),
                2,
                Some(30)
            ),
            ("al2".to_owned(), "run_undone".to_owned(), 11, None, 0, None),
        ]
    );
    let pairs: Vec<(String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT old_album_id, new_album_id, reason, state, recorded_at, updated_at \
         FROM library_retag_pairs",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        pairs,
        vec![(
            "al1".to_owned(),
            "al2".to_owned(),
            "tracks_elsewhere".to_owned(),
            "dismissed".to_owned(),
            5,
            6
        )]
    );

    sqlx::raw_sql(
        "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES ('al3', 'replaced_hands_off', 12); \
         INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
         recorded_at, updated_at) VALUES ('al4', 'al3', 'replaced_hands_off', 'open', 7, 7);",
    )
    .execute(&pool)
    .await
    .unwrap();
    for refused in [
        "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES ('al4', 'x', 1)",
        "INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
         recorded_at, updated_at) VALUES ('al3', 'al4', 'x', 'open', 1, 1)",
        "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES ('al4', 'files_changed', 'soon')",
    ] {
        assert!(
            sqlx::raw_sql(refused).execute(&pool).await.is_err(),
            "{refused}"
        );
    }

    sqlx::raw_sql("DELETE FROM local_albums WHERE id = 'al3'")
        .execute(&pool)
        .await
        .unwrap();
    let left: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM library_album_takeovers WHERE local_album_id = 'al3'), \
         (SELECT COUNT(*) FROM library_retag_pairs WHERE new_album_id = 'al3')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, (0, 0));

    let index: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'index' \
         AND name = 'idx_library_retag_pairs_new' AND tbl_name = 'library_retag_pairs'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(index.is_some_and(|sql| sql.contains("(new_album_id)")));
    for table in ["library_album_takeovers", "library_retag_pairs"] {
        let sql: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1")
                .bind(table)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(sql.trim_end().ends_with("STRICT"), "{table}: {sql}");
    }
}

/// Migration 0053 carries the folders a scan holds back at upgrade: a
/// folder held back with no later completed scan that walked a folder
/// covering it without holding that one back, from the start of the
/// earliest scan that held it since. The table is strict and keyed by
/// root and folder.
#[tokio::test]
async fn held_scopes_arrive_with_the_holds_that_stand() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 53)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    // (run, started_at, root, folder walked, held back)
    let runs = [
        ("g1", 100, "r1", ".", true),
        ("g2", 200, "r1", ".", true),
        ("g3", 100, "r2", "x", true),
        ("c1", 300, "r2", ".", false),
        ("g4", 100, "r3", ".", true),
        ("c2", 300, "r3", "y", false),
        ("g5", 100, "r4", ".", true),
        ("c3", 200, "r4", ".", false),
        ("g6", 300, "r4", ".", true),
    ];
    for (run, started, root, folder, held) in runs {
        let mut sql = format!(
            "INSERT INTO library_scan_runs (id, kind, trigger, state, phase, aggregate_scope, \
             queued_at, started_at, updated_at) VALUES ('{run}', 'incremental', 'manual', \
             'completed', 'reconciling', '{root}', {started}, {started}, {started}); \
             INSERT INTO library_scan_run_scopes (run_id, scope_sequence, root_id, \
             relative_path, effective_policy, policy_revision, discovery_state) VALUES \
             ('{run}', 0, '{root}', '{folder}', 'automatic', 'p', 'completed');"
        );
        if held {
            sql.push_str(&format!(
                "INSERT INTO library_scan_failures (run_id, root_id, relative_path, \
                 failure_code, phase, recorded_at) VALUES ('{run}', '{root}', '{folder}', \
                 'MASS_MISSING_GUARD', 'discovering', {started});"
            ));
        }
        sqlx::raw_sql(&sql).execute(&pool).await.unwrap();
    }

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let held: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT root_id, relative_path, held_at FROM library_scan_held_scopes \
         ORDER BY root_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        held,
        vec![
            ("r1".to_owned(), ".".to_owned(), 100_000),
            ("r3".to_owned(), ".".to_owned(), 100_000),
            ("r4".to_owned(), ".".to_owned(), 300_000),
        ]
    );
    let sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'library_scan_held_scopes'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(sql.trim_end().ends_with("STRICT"), "{sql}");
    assert!(
        sql.contains("PRIMARY KEY (root_id, relative_path)"),
        "{sql}"
    );
}

/// The origins a remap may carry in the database are exactly the ones the
/// code names, in both directions.
#[tokio::test]
async fn remap_origin_check_lists_exactly_the_code_values() {
    use droppedneedle_library::operations::choice::RemapOrigin;

    let pool = migrated_pool().await;
    let sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE name = 'library_edition_remap_queue'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let marker = "CHECK(origin IN (";
    let values = &sql[sql.find(marker).expect("the column has a CHECK") + marker.len()..];
    let listed: Vec<String> = values[..values.find(')').expect("the list ends")]
        .split(',')
        .map(|value| value.trim().trim_matches('\'').to_owned())
        .collect();
    assert_eq!(
        listed,
        RemapOrigin::ALL.map(|origin| origin.as_str().to_owned())
    );
    for origin in RemapOrigin::ALL {
        assert_eq!(RemapOrigin::parse(origin.as_str()), Some(origin));
    }
    assert_eq!(RemapOrigin::parse("x"), None);
}

/// Migration 0054 makes the artist proof and album state tables again,
/// strict: a proof and a state go with their album or track, an artist
/// with a proof cannot be deleted, a proof's MusicBrainz id is a
/// lowercase UUID, and the state lists exactly the code's values.
#[tokio::test]
async fn artist_proofs_and_states_go_with_their_album() {
    use droppedneedle_library::identify::artist_links::CreditState;

    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 54)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();
    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stamped >= 54 && stamped == latest_version(), "{stamped}");

    assert_eq!(
        column_names(&pool, "library_artist_credit_proofs").await,
        [
            "subject_kind",
            "subject_id",
            "local_album_id",
            "local_track_id",
            "credit_position",
            "source_local_artist_id",
            "local_artist_id",
            "artist_mbid",
            "canonical_name",
            "credited_name",
            "sort_name",
            "join_phrase",
            "release_mbid",
            "release_track_mbid",
            "album_identity_revision",
            "track_identity_revision",
            "evidence_hash",
            "created_at",
            "updated_at",
            "row_revision",
        ]
    );
    assert_eq!(
        column_names(&pool, "library_artist_reconciliation_state").await,
        [
            "local_album_id",
            "input_revision",
            "state",
            "reason_code",
            "updated_at"
        ]
    );
    let mut state_sql = String::new();
    for table in [
        "library_artist_credit_proofs",
        "library_artist_reconciliation_state",
    ] {
        let sql: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1")
                .bind(table)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(sql.trim_end().ends_with("STRICT"), "{table}: {sql}");
        state_sql = sql;
    }
    let marker = "CHECK(state IN (";
    let values = &state_sql[state_sql.find(marker).expect("state has a CHECK") + marker.len()..];
    let listed: Vec<String> = values[..values.find(')').expect("the list ends")]
        .split(',')
        .map(|value| value.trim().trim_matches('\'').to_owned())
        .collect();
    assert_eq!(
        listed,
        CreditState::ALL.map(|state| state.as_str().to_owned())
    );
    for state in CreditState::ALL {
        assert_eq!(CreditState::parse(state.as_str()), Some(state));
    }

    let keys: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT 'proofs', \"table\", \"from\", on_delete \
         FROM pragma_foreign_key_list('library_artist_credit_proofs') \
         UNION ALL SELECT 'state', \"table\", \"from\", on_delete \
         FROM pragma_foreign_key_list('library_artist_reconciliation_state') ORDER BY 1, 3",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let key = |owner: &str, table: &str, column: &str, action: &str| {
        (
            owner.to_owned(),
            table.to_owned(),
            column.to_owned(),
            action.to_owned(),
        )
    };
    assert_eq!(
        keys,
        vec![
            key("proofs", "local_albums", "local_album_id", "CASCADE"),
            key("proofs", "local_artists", "local_artist_id", "RESTRICT"),
            key("proofs", "local_tracks", "local_track_id", "CASCADE"),
            key(
                "proofs",
                "local_artists",
                "source_local_artist_id",
                "RESTRICT"
            ),
            key("state", "local_albums", "local_album_id", "CASCADE"),
        ]
    );
    let indexes: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL \
         AND tbl_name IN ('library_artist_credit_proofs', 'library_artist_reconciliation_state') \
         ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let columns: Vec<(&str, &str)> = indexes
        .iter()
        .map(|(name, sql)| {
            let open = sql.rfind('(').expect("index columns");
            (name.as_str(), &sql[open..])
        })
        .collect();
    assert_eq!(
        columns,
        vec![
            (
                "idx_artist_credit_proof_album",
                "(local_album_id, subject_kind, subject_id)"
            ),
            (
                "idx_artist_credit_proof_mbid",
                "(artist_mbid, source_local_artist_id)"
            ),
            (
                "idx_artist_credit_proof_resolved",
                "(local_artist_id, artist_mbid)"
            ),
            (
                "idx_artist_credit_proof_source",
                "(source_local_artist_id, artist_mbid)"
            ),
            ("idx_artist_credit_proof_track", "(local_track_id)"),
            ("idx_artist_reconciliation_state_status", "(state)"),
        ]
    );

    sqlx::raw_sql(
        "PRAGMA foreign_keys = ON; \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('al2', 'r1', 'g2', 'U', 'u', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, ingest_source, imported_at, \
         membership_source) VALUES \
         ('t1', 'al1', 'r1', '/m/1.flac', '1.flac', 'h1', 1, 0, 's', 'T', 't', 'T', 't', \
         'flac', 'scan', 0, 'automatic');",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mbid = "aaaaaaaa-0000-4000-8000-000000000001";
    let hash = "0".repeat(64);
    let proof = |kind: &str, subject: &str, album: &str, track: Option<&str>, artist_mbid: &str| {
        let (release_track, track_revision) = match track {
            Some(_) => ("'rt'", "1"),
            None => ("NULL", "NULL"),
        };
        format!(
            "INSERT INTO library_artist_credit_proofs (subject_kind, subject_id, local_album_id, \
             local_track_id, credit_position, source_local_artist_id, local_artist_id, \
             artist_mbid, canonical_name, credited_name, release_mbid, release_track_mbid, \
             album_identity_revision, track_identity_revision, evidence_hash, created_at, \
             updated_at) VALUES ('{kind}', '{subject}', '{album}', {}, 0, 'a1', 'a1', \
             '{artist_mbid}', 'A', 'A', 'r', {release_track}, 1, {track_revision}, '{hash}', \
             1000, 1000)",
            track.map_or("NULL".to_owned(), |track| format!("'{track}'"))
        )
    };
    for bad in ["AAAAAAAA-0000-4000-8000-000000000001", "not-an-id"] {
        assert!(
            sqlx::raw_sql(&proof("album", "al1", "al1", None, bad))
                .execute(&pool)
                .await
                .is_err(),
            "{bad} refused"
        );
    }
    sqlx::raw_sql(&format!(
        "{}; {}; {}; \
         INSERT INTO library_artist_reconciliation_state (local_album_id, input_revision, state, \
         reason_code, updated_at) VALUES ('al1', 'x', 'projected', 'R', 1000), \
         ('al2', 'y', 'projected', 'R', 1000);",
        proof("album", "al1", "al1", None, mbid),
        proof("track", "t1", "al1", Some("t1"), mbid),
        proof("album", "al2", "al2", None, mbid)
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        sqlx::raw_sql(
            "INSERT INTO library_artist_reconciliation_state (local_album_id, input_revision, \
             state, reason_code, updated_at) VALUES ('al1', 'x', 'provider_conflict', 'R', 1)"
        )
        .execute(&pool)
        .await
        .is_err(),
        "a state the code does not have is refused"
    );
    assert!(
        sqlx::raw_sql("DELETE FROM local_artists WHERE id = 'a1'")
            .execute(&pool)
            .await
            .is_err(),
        "an artist with a proof stays"
    );

    let left = |pool: &SqlitePool| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT (SELECT COUNT(*) FROM library_artist_credit_proofs), \
                 (SELECT COUNT(*) FROM library_artist_reconciliation_state)",
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    sqlx::raw_sql("DELETE FROM local_tracks WHERE id = 't1'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(left(&pool).await, (2, 2), "the track's proof went with it");
    sqlx::raw_sql("DELETE FROM local_albums WHERE id = 'al2'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        left(&pool).await,
        (1, 1),
        "the album's proof and state went with it"
    );
}

/// Migration 0055 adds the files a lifted hold could not judge, empty and
/// strict, a row going with its track. A hold that stands is kept as it
/// was.
#[tokio::test]
async fn unjudged_tracks_arrive_empty_and_strict() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 55)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO library_scan_held_scopes (root_id, relative_path, held_at) \
         VALUES ('r1', 'x', 100000)",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stamped >= 55 && stamped == latest_version(), "{stamped}");
    let held: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT root_id, relative_path, held_at FROM library_scan_held_scopes")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(held, vec![("r1".to_owned(), "x".to_owned(), 100_000)]);
    assert_eq!(
        column_names(&pool, "library_scan_unjudged_tracks").await,
        ["local_track_id", "held_at"]
    );
    let strict: i64 = sqlx::query_scalar(
        "SELECT strict FROM pragma_table_list WHERE name = 'library_scan_unjudged_tracks'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(strict, 1);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_scan_unjudged_tracks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let keys: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT \"from\", \"table\", \"to\", on_delete \
         FROM pragma_foreign_key_list('library_scan_unjudged_tracks')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        keys,
        vec![(
            "local_track_id".to_owned(),
            "local_tracks".to_owned(),
            "id".to_owned(),
            "CASCADE".to_owned()
        )]
    );
}

/// Every table in the migrated schema is listed once in
/// `server/table-owners.txt` under a known area, and the list names no
/// table the schema lacks.
#[tokio::test]
async fn every_table_has_one_owner() {
    const AREAS: [&str; 10] = [
        "kernel", "auth", "plugins", "app", "compat", "media", "tool", "acquire", "catalog",
        "library",
    ];
    let mut owners = std::collections::BTreeMap::new();
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

    // The search index keeps internal tables of its own under its name.
    let pool = migrated_pool().await;
    let tables: Vec<String> = table_names(&pool)
        .await
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

/// Each migration stamps its own number, so applying them in order on an
/// empty database ends at the latest version. A file renumbered without
/// its `user_version` stamp (or the reverse) fails here, before boot would
/// refuse the database it leaves behind.
#[tokio::test]
async fn migrations_stamp_their_own_version_in_order() {
    let pool = scratch_pool().await;
    for migration in MIGRATOR.migrations.iter() {
        sqlx::raw_sql(&migration.sql).execute(&pool).await.unwrap();
        let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stamped, migration.version, "{}", migration.description);
    }
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, latest_version());
}

/// Number of migrations the binary carries.
fn migration_count() -> i64 {
    i64::try_from(droppedneedle_kernel::schema::MIGRATOR.migrations.len()).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// Migrations apply cleanly on top of the baseline and rerun idempotently.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migrations_upgrade_from_baseline() {
    let dir = crate::common::ScratchDir::new("migrate");
    let db_path = dir.join("migrate.db");

    // Baseline only, as the oldest database would hold it.
    let baseline = include_str!("../../migrations/0001_baseline.sql");
    {
        let conn = rusqlite::Connection::open(&db_path).expect("scratch opens");
        conn.execute_batch(baseline).expect("baseline applies");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version reads");
        assert_eq!(version, 1);
    }
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime opens");
    apply_migrations(runtime.pool())
        .await
        .expect("later migrations apply on top of 0001");
    let conn = rusqlite::Connection::open(&db_path).expect("scratch reopens");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version reads");
    assert_eq!(version, latest_version());
    for name in ["download_idempotency_keys", "acquire_follow_cursors"] {
        let table: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .expect("table lookup runs");
        assert_eq!(table.as_deref(), Some(name));
    }
    drop(conn);

    // A second run is a clean no-op (the boot path reruns every start).
    apply_migrations(runtime.pool())
        .await
        .expect("migrations rerun cleanly");
    let conn = rusqlite::Connection::open(&db_path).expect("scratch reopens");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version reads");
    assert_eq!(version, latest_version());
    runtime.shutdown().await;
}
