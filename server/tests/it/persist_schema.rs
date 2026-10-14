//! Schema: fresh migrate, re-run, upgrade from every earlier version,
//! legacy exclusions, merged tables, and foreign-key enforcement. The
//! scratch databases live only in memory, except the baseline upgrade,
//! which runs through the production runtime on a file.

use droppedneedle::db::{DbConfig, open_runtime};
use droppedneedle::schema::{MIGRATOR, apply_migrations, assert_migrated, latest_version};
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

    assert_eq!(table_names(&pool).await.len(), 151);
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
        "local_track_aliases",
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
    use droppedneedle::library::scan::sqlite_store::retag_pairs::{
        RetagPairReason, RetagPairState,
    };

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

/// The causes a takeover may carry in the database are exactly the ones
/// the code names, in both directions.
#[tokio::test]
async fn takeover_check_lists_exactly_the_code_values() {
    use droppedneedle::library::management::takeover::Cause;

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
    i64::try_from(droppedneedle::schema::MIGRATOR.migrations.len()).unwrap_or(i64::MAX)
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
