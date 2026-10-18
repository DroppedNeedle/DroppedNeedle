//! Schema: fresh migrate, re-run, upgrade from every earlier version,
//! legacy exclusions, merged tables, and foreign-key enforcement. The
//! scratch databases live only in memory, except the baseline upgrade,
//! which runs through the production runtime on a file.

use std::collections::BTreeMap;
use std::time::Instant;

use droppedneedle_kernel::db::statement::{Declared, Schema, StatementFile};
use droppedneedle_kernel::db::{DbConfig, open_runtime};
use droppedneedle_kernel::schema::{MIGRATOR, apply_migrations, assert_migrated, latest_version};
use regex::Regex;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use crate::common::sources::workspace_sources;

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

    assert_eq!(table_names(&pool).await.len(), 160);
    let triggers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(triggers, 47);

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

/// Dropped tables stay dropped: no lidarr marker, no rebuild leftovers, no legacy
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

/// Migration 0056 adds the folders scans found to be mounts, empty and
/// strict, keyed by root and path, and the index the guard check reads.
#[tokio::test]
async fn scan_mounts_arrive_empty_with_the_guard_index() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 56)
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
    assert!(stamped >= 56 && stamped == latest_version(), "{stamped}");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_scan_mounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let strict: i64 = sqlx::query_scalar(
        "SELECT strict FROM pragma_table_list WHERE name = 'library_scan_mounts'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(strict, 1);
    let key: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('library_scan_mounts') WHERE pk > 0 ORDER BY pk",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(key, ["root_id", "relative_path"]);
    let index: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_scan_failures_guard'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let index = index.expect("the guard index exists");
    assert!(
        index.contains("WHERE failure_code = 'MASS_MISSING_GUARD'"),
        "{index}"
    );
}

/// Migration 0057 drops the two identification tables nothing reads, rows
/// and all, and leaves the albums and tracks they named alone.
#[tokio::test]
async fn unused_credit_tables_drop_and_tracks_survive() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 57)
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
         INSERT INTO library_identify_credit_proofs (local_album_id, local_track_id, \
         source_local_artist_id, artist_mbid, release_mbid, album_identity_revision, \
         track_identity_revision) VALUES ('al1', 't1', 'a1', 'mb-a', 'mb-r', 1, 1); \
         INSERT INTO library_identify_track_credits (local_track_id, position, artist_mbid, \
         canonical_name, credited_name) VALUES ('t1', 0, 'mb-a', 'A', 'A');",
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
    assert!(stamped >= 57 && stamped == latest_version(), "{stamped}");
    let left: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE name IN ('library_identify_credit_proofs', \
         'library_identify_track_credits', 'idx_identify_credit_proofs_artist')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(left.is_empty(), "{left:?}");
    let kept: (String, String) = sqlx::query_as(
        "SELECT a.id, t.id FROM local_albums a JOIN local_tracks t ON t.local_album_id = a.id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kept, ("al1".to_owned(), "t1".to_owned()));
}

/// Migration 0058 gives each track's edition hints a release date. A row
/// from before keeps its hints, and its release date reads NULL until a
/// scan reads the file again.
#[tokio::test]
async fn edition_hints_keep_their_rows_and_gain_a_release_date() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 58)
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
         INSERT INTO local_track_edition_tags (local_track_id, media, barcode, catalog_number, \
         release_country, total_discs, total_tracks) VALUES \
         ('t1', 'CD', '0123', 'CAT-1', 'GB', 2, 12);",
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
    assert!(stamped >= 58 && stamped == latest_version(), "{stamped}");
    type Hints = (String, String, String, String, i64, i64, Option<String>);
    let row: Hints = sqlx::query_as(
        "SELECT media, barcode, catalog_number, release_country, total_discs, total_tracks, \
         release_date FROM local_track_edition_tags WHERE local_track_id = 't1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "CD".to_owned(),
            "0123".to_owned(),
            "CAT-1".to_owned(),
            "GB".to_owned(),
            2,
            12,
            None
        )
    );
}

/// Migration 0059 adds users' ratings, empty and strict. A database from
/// before keeps its users and favorites, and a rating goes with its user.
#[tokio::test]
async fn user_ratings_arrive_empty_and_go_with_their_user() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 59)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'T', 'user', 'now'); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('u1', 'track', 't1', 1.5);",
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
    assert!(stamped >= 59 && stamped == latest_version(), "{stamped}");
    let kept: (String, String) = sqlx::query_as(
        "SELECT u.id, f.item_id FROM auth_users u \
         JOIN library_user_favorites f ON f.user_id = u.id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kept, ("u1".to_owned(), "t1".to_owned()));
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_user_ratings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let strict: i64 = sqlx::query_scalar(
        "SELECT strict FROM pragma_table_list WHERE name = 'library_user_ratings'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(strict, 1);
    let key: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('library_user_ratings') WHERE pk > 0 ORDER BY pk",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(key, ["user_id", "item_kind", "item_id"]);
    let indexed: Vec<String> = sqlx::query_scalar(
        "SELECT c.name FROM pragma_index_info('idx_user_ratings_item') c ORDER BY c.seqno",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(indexed, ["item_kind", "item_id"]);

    for refused in [
        "INSERT INTO library_user_ratings VALUES ('u1', 'track', 't1', 0, 1)",
        "INSERT INTO library_user_ratings VALUES ('u1', 'track', 't1', 6, 1)",
        "INSERT INTO library_user_ratings VALUES ('u1', 'playlist', 'p1', 3, 1)",
    ] {
        assert!(
            sqlx::query(refused).execute(&pool).await.is_err(),
            "{refused}"
        );
    }
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO library_user_ratings VALUES ('u1', 'track', 't1', 4, 1)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM auth_users WHERE id = 'u1'")
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_user_ratings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

/// Migration 0060 adds the record of files matched by hand, empty: a strict
/// table whose row goes with its track and outlives the person who made
/// it, an index on that person, and the version stamp.
#[tokio::test]
async fn hand_matches_arrive_empty_and_strict() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 60)
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
            .contains(&"library_track_hand_matches".to_owned())
    );

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stamped >= 60 && stamped == latest_version(), "{stamped}");
    assert_eq!(
        column_names(&pool, "library_track_hand_matches").await,
        [
            "local_track_id",
            "release_mbid",
            "release_track_mbid",
            "prior_identity_json",
            "matched_by_user_id",
            "matched_at"
        ]
    );
    let strict: i64 = sqlx::query_scalar(
        "SELECT strict FROM pragma_table_list WHERE name = 'library_track_hand_matches'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(strict, 1);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_track_hand_matches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let mut keys: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT \"from\", \"table\", \"to\", on_delete \
         FROM pragma_foreign_key_list('library_track_hand_matches')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            (
                "local_track_id".to_owned(),
                "local_tracks".to_owned(),
                "id".to_owned(),
                "CASCADE".to_owned()
            ),
            (
                "matched_by_user_id".to_owned(),
                "auth_users".to_owned(),
                "id".to_owned(),
                "SET NULL".to_owned()
            ),
        ]
    );
    let indexed: Vec<String> = sqlx::query_scalar(
        "SELECT i.name FROM pragma_index_list('library_track_hand_matches') i \
         JOIN pragma_index_info(i.name) c WHERE c.name = 'matched_by_user_id'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        indexed,
        vec!["idx_library_track_hand_matches_user".to_owned()]
    );
}

/// Migration 0061 adds the lyrics marks and the lyrics sweep's checks,
/// empty and strict. A database from before keeps its tracks; a mark goes
/// with its track and a check with its album, and a mark's source is one
/// the sweep writes.
#[tokio::test]
async fn lyrics_marks_arrive_empty_and_go_with_their_track() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 61)
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
         'flac', 'scan', 0, 'automatic'), \
         ('t2', 'al1', 'r1', '/m/2.flac', '2.flac', 'h2', 1, 0, 's', 'U', 'u', 'T', 't', \
         'flac', 'scan', 0, 'automatic');",
    )
    .execute(&pool)
    .await
    .unwrap();
    async fn tracks(pool: &SqlitePool) -> sqlx::Result<Vec<String>> {
        sqlx::query_scalar("SELECT id FROM local_tracks ORDER BY id")
            .fetch_all(pool)
            .await
    }
    let kept = tracks(&pool).await.unwrap();
    assert_eq!(kept, ["t1", "t2"]);

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stamped >= 61 && stamped == latest_version(), "{stamped}");
    assert_eq!(tracks(&pool).await.unwrap(), kept);
    for (table, columns, parent) in [
        (
            "local_track_lyrics",
            vec!["local_track_id", "source"],
            ("local_track_id", "local_tracks"),
        ),
        (
            "local_album_lyrics_checks",
            vec!["local_album_id", "signature", "checked_at"],
            ("local_album_id", "local_albums"),
        ),
    ] {
        assert_eq!(column_names(&pool, table).await, columns);
        let strict: i64 = sqlx::query_scalar("SELECT strict FROM pragma_table_list WHERE name = ?")
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(strict, 1, "{table}");
        let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "{table}");
        let keys: Vec<(String, String, String)> = sqlx::query_as(&format!(
            "SELECT \"from\", \"table\", on_delete FROM pragma_foreign_key_list('{table}')"
        ))
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            keys,
            vec![(
                parent.0.to_owned(),
                parent.1.to_owned(),
                "CASCADE".to_owned()
            )],
            "{table}"
        );
    }

    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    let track = &kept[0];
    assert!(
        sqlx::query("INSERT INTO local_track_lyrics VALUES (?, 'lrclib')")
            .bind(track)
            .execute(&pool)
            .await
            .is_err(),
        "a source the sweep never writes is refused"
    );
    sqlx::query("INSERT INTO local_track_lyrics VALUES (?, 'sidecar')")
        .bind(track)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM local_tracks WHERE id = ?")
        .bind(track)
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_track_lyrics")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "the mark went with its track");
}

/// Migration 0062 adds users' played marks, empty and strict. A database
/// from before keeps its users, tracks and plays; a mark holds at least
/// one time and goes with its user or its track.
#[tokio::test]
async fn play_marks_arrive_empty_and_go_with_their_user_and_track() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 62)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
         ('u1', 'One', 'user', 'now'), ('u2', 'Two', 'user', 'now'); \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, ingest_source, imported_at, \
         membership_source) VALUES \
         ('t1', 'al1', 'r1', '/m/1.flac', '1.flac', 'h1', 1, 0, 's', 'T', 't', 'T', 't', \
         'flac', 'scan', 0, 'automatic'), \
         ('t2', 'al1', 'r1', '/m/2.flac', '2.flac', 'h2', 1, 0, 's', 'U', 'u', 'T', 't', \
         'flac', 'scan', 0, 'automatic'); \
         INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES \
         ('p1', 'u1', 't2', 'al1', 'U', 'A', '2026-01-01T00:00:00+00:00');",
    )
    .execute(&pool)
    .await
    .unwrap();
    async fn counts(pool: &SqlitePool) -> (i64, i64, i64) {
        sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM auth_users), (SELECT COUNT(*) FROM local_tracks), \
             (SELECT COUNT(*) FROM library_play_history)",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }
    async fn marks(pool: &SqlitePool) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT user_id, local_track_id FROM library_user_play_marks \
             ORDER BY user_id, local_track_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }
    assert_eq!(counts(&pool).await, (2, 2, 1));

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stamped >= 62 && stamped == latest_version(), "{stamped}");
    assert_eq!(counts(&pool).await, (2, 2, 1));
    let table = "library_user_play_marks";
    assert_eq!(
        column_names(&pool, table).await,
        [
            "user_id",
            "local_track_id",
            "marked_played_at",
            "marked_unplayed_at"
        ]
    );
    let strict: i64 = sqlx::query_scalar("SELECT strict FROM pragma_table_list WHERE name = ?")
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(strict, 1);
    assert!(marks(&pool).await.is_empty());
    let key: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('library_user_play_marks') WHERE pk > 0 ORDER BY pk",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(key, ["user_id", "local_track_id"]);
    let indexed: Vec<String> = sqlx::query_scalar(
        "SELECT c.name FROM pragma_index_info('idx_user_play_marks_track') c ORDER BY c.seqno",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(indexed, ["local_track_id"]);
    let mut keys: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT \"from\", \"table\", on_delete \
         FROM pragma_foreign_key_list('library_user_play_marks')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    keys.sort();
    assert_eq!(
        keys,
        [
            ("local_track_id", "local_tracks", "CASCADE"),
            ("user_id", "auth_users", "CASCADE")
        ]
        .map(|(from, to, rule)| (from.to_owned(), to.to_owned(), rule.to_owned()))
    );

    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query("INSERT INTO library_user_play_marks VALUES ('u1', 't1', NULL, NULL)")
            .execute(&pool)
            .await
            .is_err(),
        "a mark with no time is refused"
    );
    sqlx::raw_sql(
        "INSERT INTO library_user_play_marks VALUES ('u1', 't1', 5, NULL), \
         ('u1', 't2', NULL, 6), ('u2', 't1', 7, 3);",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM auth_users WHERE id = 'u2'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        marks(&pool).await,
        [("u1", "t1"), ("u1", "t2")].map(|(user, track)| (user.to_owned(), track.to_owned())),
        "the marks went with their user"
    );
    sqlx::query("DELETE FROM local_tracks WHERE id = 't1'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        marks(&pool).await,
        [("u1".to_owned(), "t2".to_owned())],
        "the mark went with its track"
    );
}

/// Migration 0063 adds the artist ids identification asks MusicBrainz
/// about, empty and strict, with a state listing exactly the code's values
/// and a merged id only on a merged row. Every album's artist stamp is
/// reset, so the matched albums are worked through again.
#[tokio::test]
async fn artist_id_checks_arrive_empty_and_every_album_is_worked_again() {
    use droppedneedle_library::identify::merged_ids::CheckState;

    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 63)
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
         INSERT INTO library_artist_reconciliation_state (local_album_id, input_revision, \
         state, reason_code, updated_at) VALUES ('al1', 'stamp', 'projected', \
         'CREDITS_LINE_UP', 1);",
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
    assert!(stamped >= 63 && stamped == latest_version(), "{stamped}");
    let reset: (String, String) = sqlx::query_as(
        "SELECT input_revision, state FROM library_artist_reconciliation_state \
         WHERE local_album_id = 'al1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reset, (String::new(), "projected".to_owned()));

    let table = "library_identify_artist_id_checks";
    assert_eq!(
        column_names(&pool, table).await,
        [
            "artist_mbid",
            "state",
            "canonical_mbid",
            "nominated_at",
            "checked_at"
        ]
    );
    let sql: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(sql.trim_end().ends_with("STRICT"), "{sql}");
    let marker = "CHECK(state IN (";
    let values = &sql[sql.find(marker).expect("state has a CHECK") + marker.len()..];
    let listed: Vec<String> = values[..values.find(')').expect("the list ends")]
        .split(',')
        .map(|value| value.trim().trim_matches('\'').to_owned())
        .collect();
    assert_eq!(
        listed,
        CheckState::ALL.map(|state| state.as_str().to_owned())
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_identify_artist_id_checks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    for (index, columns) in [
        ("idx_identify_artist_id_checks_state", "state,nominated_at"),
        ("idx_identify_artist_id_checks_canonical", "canonical_mbid"),
    ] {
        let found: String = sqlx::query_scalar(
            "SELECT group_concat(name, ',') FROM (SELECT name FROM pragma_index_info(?1) \
             ORDER BY seqno)",
        )
        .bind(index)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(found, columns, "{index}");
    }

    let id = "3ec87112-9081-467b-8327-e18286f1b020";
    let other = "ffbcb0c1-37ad-4df8-a78d-e31bd590d7ea";
    for (row, refused) in [
        (format!("('{id}', 'pending', NULL, 1, NULL)"), false),
        (format!("('{other}', 'merged', '{id}', 1, 2)"), false),
        (
            format!("('{}', 'pending', NULL, 1, NULL)", id.to_uppercase()),
            true,
        ),
        ("('not-an-id', 'pending', NULL, 1, NULL)".to_owned(), true),
        (format!("('{other}', 'gone', NULL, 1, 2)"), true),
        (format!("('{other}', 'kept', '{id}', 1, 2)"), true),
        (format!("('{other}', 'merged', NULL, 1, 2)"), true),
        (format!("('{other}', 'merged', '{other}', 1, 2)"), true),
        (format!("('{other}', 'pending', NULL, 1, 2)"), true),
        (format!("('{other}', 'kept', NULL, 1, NULL)"), true),
    ] {
        sqlx::query("DELETE FROM library_identify_artist_id_checks WHERE artist_mbid <> ?1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let written = sqlx::query(&format!(
            "INSERT OR REPLACE INTO library_identify_artist_id_checks VALUES {row}"
        ))
        .execute(&pool)
        .await;
        assert_eq!(written.is_err(), refused, "{row}");
    }
}

/// Migration 0064 puts re-identifications parked on a choice back in the
/// queue, with their stale candidates cleared, so the worker hands them to
/// identification. Stopped re-identifications and repair reports waiting
/// for Apply stay as they are.
#[tokio::test]
async fn parked_reidentifications_go_back_to_the_queue() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 64)
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
         INSERT INTO library_operation_jobs (id, kind, state, expected_work_count, \
         completed_count, succeeded_count, terminal_code, reidentification_attempt_count, \
         created_at, updated_at, row_revision, event_revision) VALUES \
         ('parked', 'explicit_reidentification', 'ready', 1, 1, 1, 'CANDIDATES_READY', 2, \
         1, 1, 4, 4), \
         ('stopped', 'explicit_reidentification', 'stopped', 1, 0, 0, 'STOPPED', 0, \
         1, 1, 2, 2), \
         ('report', 'repair', 'ready', 1, 1, 1, 'DRY_RUN_READY', 0, 1, 1, 3, 3); \
         INSERT INTO library_operation_work (job_id, ordinal, local_album_id, \
         expected_subject_revision, expected_input_revision, action, idempotency_key, state, \
         result_json, updated_at) VALUES \
         ('parked', 0, 'al1', 1, 'x', 'reidentify', 'parked:al1', 'succeeded', \
         '{\"outcome\":\"contradictory\"}', 1), \
         ('stopped', 0, 'al1', 1, 'x', 'reidentify', 'stopped:al1', 'pending', NULL, 1); \
         INSERT INTO library_reidentification_snapshots (job_id, local_album_id, \
         expected_album_revision, expected_input_revision, selected_candidate_key, \
         result_json, created_at) VALUES \
         ('parked', 'al1', 1, 'x', 'rg:rel', '{\"candidates\":[{\"candidate_key\":\"rg:rel\"}]}', 1), \
         ('stopped', 'al1', 1, 'x', NULL, NULL, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();
    async fn job(pool: &SqlitePool, id: &str) -> String {
        sqlx::query_scalar(
            "SELECT state || '|' || COALESCE(terminal_code, '') || '|' || control_request \
             || '|' || completed_count || '|' || succeeded_count || '|' || failed_count \
             || '|' || skipped_count || '|' || reidentification_attempt_count || '|' \
             || expected_work_count || '|' || row_revision || '|' || event_revision || '|' \
             || updated_at FROM library_operation_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }
    async fn work(pool: &SqlitePool, id: &str) -> (String, Option<String>, i64) {
        sqlx::query_as(
            "SELECT state, result_json, row_revision FROM library_operation_work \
             WHERE job_id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }
    async fn snapshot(pool: &SqlitePool, id: &str) -> (Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT selected_candidate_key, result_json \
             FROM library_reidentification_snapshots WHERE job_id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }
    let stopped = (
        job(&pool, "stopped").await,
        work(&pool, "stopped").await,
        snapshot(&pool, "stopped").await,
    );
    let report = job(&pool, "report").await;

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let parked = job(&pool, "parked").await;
    let (fields, updated_at) = parked.rsplit_once('|').expect("job fields");
    assert_eq!(fields, "queued||none|0|0|0|0|0|1|5|5");
    let updated_at: f64 = updated_at.parse().expect("updated_at");
    assert!(updated_at > 1_000_000_000.0, "updated now: {updated_at}");
    assert_eq!(work(&pool, "parked").await, ("pending".to_owned(), None, 2));
    assert_eq!(snapshot(&pool, "parked").await, (None, None));
    assert_eq!(
        (
            job(&pool, "stopped").await,
            work(&pool, "stopped").await,
            snapshot(&pool, "stopped").await,
        ),
        stopped,
        "a stopped re-identification keeps its meaning"
    );
    assert_eq!(
        job(&pool, "report").await,
        report,
        "a repair report still waits for Apply"
    );
    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stamped >= 64 && stamped == latest_version(), "{stamped}");
}

/// Migration 0065 works out every listener's album play values and each
/// album's `has_indexed_track` from the plays, marks and tracks already
/// there, and changes none of them. A play counts for its track's album,
/// not the album stored on it; a play with an unreadable time counts with
/// no time; a play with only an album counts for it; a play naming neither
/// counts for none; a played mark counts once. A play by a user id with no
/// account keeps its row up to date afterwards.
#[tokio::test]
async fn album_play_summary_arrives_worked_out_from_the_plays() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 65)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    let track = |id: &str, album: &str, availability: &str| {
        format!(
            "('{id}', '{album}', 'r1', '/m/{id}.flac', '{id}.flac', 'h-{id}', 1, 0, 's', 'T', \
             't', 'T', 't', 'flac', '{availability}', 'scan', 0, 'automatic')"
        )
    };
    sqlx::raw_sql(&format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
         ('u1', 'One', 'user', 'now'), ('u2', 'Two', 'user', 'now'); \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('al2', 'r1', 'g2', 'T', 't', 'a1', 'automatic', 1, 1), \
         ('al3', 'r1', 'g3', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, availability, ingest_source, \
         imported_at, membership_source) VALUES {}, {}, {}; \
         INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES \
         ('p1', 'u1', 't1', 'al2', 'x', 'x', '2026-01-01T00:00:00+00:00'), \
         ('p2', 'u1', 't2', 'al1', 'x', 'x', 'not a time'), \
         ('p3', 'u1', NULL, 'al3', 'x', 'x', '2026-01-03T00:00:00Z'), \
         ('p4', 'u2', NULL, NULL, 'x', 'x', '2026-01-04T00:00:00Z'), \
         ('p5', 'u2', 't3', 'al2', 'x', 'x', '2026-01-02 12:00:00'), \
         ('p6', 'ghost', 't1', 'al1', 'x', 'x', '2026-01-04T00:00:00+00:00'); \
         INSERT INTO library_user_play_marks (user_id, local_track_id, marked_played_at, \
         marked_unplayed_at) VALUES ('u2', 't1', 1767312000123, NULL);",
        track("t1", "al1", "indexed"),
        track("t2", "al1", "indexed"),
        track("t3", "al2", "missing"),
    ))
    .execute(&pool)
    .await
    .unwrap();
    async fn counts(pool: &SqlitePool) -> (i64, i64, i64) {
        sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM library_play_history), \
             (SELECT COUNT(*) FROM library_user_play_marks), (SELECT COUNT(*) FROM local_tracks)",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }
    async fn summary(pool: &SqlitePool) -> Vec<(String, String, i64, Option<i64>, i64)> {
        sqlx::query_as(
            "SELECT user_id, local_album_id, plays, last_played_at, played \
             FROM library_user_album_plays ORDER BY user_id, local_album_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }
    assert_eq!(counts(&pool).await, (6, 1, 3));

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();
    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stamped >= 65 && stamped == latest_version(), "{stamped}");
    assert_eq!(counts(&pool).await, (6, 1, 3));
    let flags: Vec<i64> =
        sqlx::query_scalar("SELECT has_indexed_track FROM local_albums ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(flags, [1, 0, 0]);
    // Midnight UTC on 2026-01-{day}, in milliseconds.
    let day = |day: i64| Some((1_767_225_600 + (day - 1) * 86_400) * 1000);
    let row = |user: &str, album: &str, plays: i64, last: Option<i64>, played: i64| {
        (user.to_owned(), album.to_owned(), plays, last, played)
    };
    assert_eq!(
        summary(&pool).await,
        [
            row("ghost", "al1", 1, day(4), 0),
            // p1 counts for t1's album; p2 counts with no time.
            row("u1", "al1", 2, day(1), 1),
            row("u1", "al3", 1, day(3), 0),
            // The played mark, in whole seconds, on an album with an
            // unplayed track.
            row("u2", "al1", 1, day(2), 0),
            // A missing track's play: no indexed track, so not played.
            row("u2", "al2", 1, day(2).map(|at| at + 43_200_000), 0),
        ]
    );

    let table = "library_user_album_plays";
    assert_eq!(
        column_names(&pool, table).await,
        [
            "user_id",
            "local_album_id",
            "plays",
            "last_played_at",
            "played"
        ]
    );
    let shape: (String, i64, i64) =
        sqlx::query_as("SELECT type, strict, wr FROM pragma_table_list WHERE name = ?")
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(shape, ("table".to_owned(), 1, 1), "STRICT, WITHOUT ROWID");
    let key: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('library_user_album_plays') WHERE pk > 0 ORDER BY pk",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(key, ["user_id", "local_album_id"]);
    for (index, columns) in [
        ("idx_user_album_plays_album", "local_album_id"),
        (
            "idx_user_album_plays_last_played",
            "user_id,last_played_at,local_album_id",
        ),
        ("idx_user_album_plays_count", "user_id,plays,local_album_id"),
    ] {
        let found: String = sqlx::query_scalar(
            "SELECT group_concat(name, ',') FROM (SELECT name FROM pragma_index_info(?1) \
             ORDER BY seqno)",
        )
        .bind(index)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(found, columns, "{index}");
    }
    let keys: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT \"from\", \"table\", on_delete \
         FROM pragma_foreign_key_list('library_user_album_plays')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        keys,
        [(
            "local_album_id".to_owned(),
            "local_albums".to_owned(),
            "CASCADE".to_owned()
        )]
    );

    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) \
         VALUES ('p7', 'ghost', 't2', 'al1', 'x', 'x', '2026-01-05T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        summary(&pool).await[0],
        row("ghost", "al1", 2, day(5), 1),
        "a play by a user id with no account moves its row"
    );
}

/// Migration 0066 drops the wakeup channel table, counters and all, and
/// keeps every job registry row as it was.
#[tokio::test]
async fn wakeup_channels_drop_and_job_rows_survive() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 66)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "UPDATE durable_work_wakeups SET seq = 3, consumed_seq = 2 WHERE channel = 'scan'; \
         INSERT INTO durable_job_registry (name, kind, state, last_heartbeat_at, updated_at) \
         VALUES ('download-worker', 'durable', 'running', 12.5, 13.0), \
         ('checkpoint', 'ephemeral', 'stopped', NULL, 14.0);",
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
    assert!(stamped >= 66 && stamped == latest_version(), "{stamped}");
    let left: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE name = 'durable_work_wakeups'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(left.is_empty(), "{left:?}");
    type JobRow = (String, String, Option<String>, String, Option<f64>, f64);
    let jobs: Vec<JobRow> = sqlx::query_as(
        "SELECT name, kind, wakeup_channel, state, last_heartbeat_at, updated_at \
         FROM durable_job_registry ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        jobs,
        vec![
            (
                "checkpoint".to_owned(),
                "ephemeral".to_owned(),
                None,
                "stopped".to_owned(),
                None,
                14.0,
            ),
            (
                "download-worker".to_owned(),
                "durable".to_owned(),
                None,
                "running".to_owned(),
                Some(12.5),
                13.0,
            ),
        ]
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
    apply_migrations(runtime.db().sqlx())
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
    apply_migrations(runtime.db().sqlx())
        .await
        .expect("migrations rerun cleanly");
    let conn = rusqlite::Connection::open(&db_path).expect("scratch reopens");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version reads");
    assert_eq!(version, latest_version());
    runtime.shutdown().await;
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
    ("kernel", droppedneedle_kernel::statement_files),
    ("catalog", droppedneedle_catalog::statement_files),
    ("library", droppedneedle_library::statement_files),
    ("tool", droppedneedle_tool::statement_files),
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

/// Decision 5: every declared statement, and every statement a family can
/// make, prepares against the migrated schema, or a `schema = V2` block's
/// against the v2 fixture database, so a renamed column or a dropped table
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
            Some((_, declared)) if declared.len() != *written => wrong.push(format!(
                "{path}: {written} declarations written, {} registered; declare SQL only inside its sql! block",
                declared.len()
            )),
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
    droppedneedle_tool::tooling::fixture::build_v2_fixture(&v2_root).expect("v2 fixture builds");
    let v2 = rusqlite::Connection::open(v2_root.join("cache").join("library.db"))
        .expect("v2 fixture opens");
    let (mut statements, mut families, mut made) = (0, 0, 0);
    for (schema, declared) in registered
        .values()
        .flat_map(|(schema, declared)| declared.iter().map(move |one| (*schema, one)))
    {
        let connection = match schema {
            Schema::App => &app,
            Schema::V2 => &v2,
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

/// The folders still allowed to hold raw SQL, from
/// `server/sql-outside-registry.txt`.
fn outside_registry() -> Vec<String> {
    let list = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sql-outside-registry.txt"),
    )
    .expect("sql-outside-registry.txt reads");
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// A literal allowed outside `sql.rs` for good, and why.
struct AllowedLiteral {
    path: &'static str,
    starts_with: &'static str,
    why: &'static str,
}

const ALLOWED_LITERALS: &[AllowedLiteral] = &[AllowedLiteral {
    path: "kernel/src/db/analyze.rs",
    starts_with: "CREATE TABLE db_planner_stats_reload (x); DROP TABLE",
    why: "bumps the schema cookie so readers reload the planner statistics; the marker \
          table is never in the schema, so there is nothing to prepare it against",
}];

/// Source that may hold raw SQL for good: unit test files and test folders
/// (a unit test that needs raw SQL lives in one), and the builder of the v2
/// fixture database, which writes the v2 schema. `server/tests/`,
/// `server/testkit/` and `server/migrations/` are never walked.
fn exempt(path: &str) -> bool {
    path.ends_with("/tests.rs")
        || path.ends_with("_tests.rs")
        || path.contains("/tests/")
        || path == "tool/src/tooling/fixture.rs"
}

/// Raw SQL lives only in `sql.rs` files. A string literal that starts with
/// a statement verb, or holds ` FROM `, ` JOIN `, ` WHERE `, ` VALUES ` or
/// ` ORDER BY ` in capitals, fails anywhere else. One that starts with
/// `PRAGMA`, `ANALYZE`, `VACUUM` or transaction control fails outside
/// `kernel/src/db/`, which owns connection set-up and transactions. A table
/// name read from the database is quoted by
/// `db::statement::quoted_identifier`. The folders listed in
/// `server/sql-outside-registry.txt` have not moved yet; an area that moves
/// deletes its line.
#[test]
fn raw_sql_stays_in_sql_files() {
    let verb = Regex::new(r"^\s*(?:SELECT|INSERT|UPDATE|DELETE|REPLACE|WITH|CREATE|DROP|ALTER)\b")
        .unwrap();
    let fragment = Regex::new(r"(?:^|\s)(?:FROM|JOIN|WHERE|VALUES|ORDER\s+BY)(?:\s|$)").unwrap();
    let control =
        Regex::new(r"^\s*(?:PRAGMA|ANALYZE|VACUUM|BEGIN|COMMIT|ROLLBACK|SAVEPOINT|RELEASE)\b")
            .unwrap();
    let outside = outside_registry();
    let sources = workspace_sources();
    let mut used = vec![false; outside.len()];
    let mut allowed_used = vec![false; ALLOWED_LITERALS.len()];
    let mut wrong = Vec::new();
    for source in &sources {
        if exempt(&source.path) {
            continue;
        }
        let listed = outside
            .iter()
            .position(|prefix| source.path.starts_with(prefix.as_str()));
        for (line, value) in split_literals(&source.text).1 {
            let raw = if control.is_match(&value) {
                !source.path.starts_with("kernel/src/db/")
            } else {
                !is_sql_file(&source.path) && (verb.is_match(&value) || fragment.is_match(&value))
            };
            if !raw {
                continue;
            }
            if let Some(at) = listed {
                used[at] = true;
                continue;
            }
            if let Some(at) = ALLOWED_LITERALS.iter().position(|allowed| {
                allowed.path == source.path && value.starts_with(allowed.starts_with)
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
    for (prefix, used) in outside.iter().zip(used) {
        if !sources
            .iter()
            .any(|source| source.path.starts_with(prefix.as_str()))
        {
            wrong.push(format!(
                "{prefix}: listed in sql-outside-registry.txt, but no longer exists"
            ));
        } else if !used {
            wrong.push(format!(
                "{prefix}: listed in sql-outside-registry.txt, but holds no raw SQL; delete the line"
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
