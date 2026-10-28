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

    assert_eq!(table_names(&pool).await.len(), 173);
    let triggers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(triggers, 67);

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

/// Migration 0068 records when each managed write was made. Writes from
/// before it take the start of their day, so none waits longer than the
/// day it kept.
#[tokio::test]
async fn publish_operations_take_the_start_of_their_day() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 68)
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
         VALUES ('early', '{\"id\":\"early\"}', 7), ('late', '{}', 20000);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let rows = sqlx::query_as::<_, (String, String, i64, i64)>(
        "SELECT bundle_id, bundle_json, created_day, created_at \
         FROM library_publish_operations ORDER BY bundle_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "early".to_owned(),
                "{\"id\":\"early\"}".to_owned(),
                7,
                7 * 86_400_000
            ),
            (
                "late".to_owned(),
                "{}".to_owned(),
                20000,
                20000 * 86_400_000
            ),
        ]
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

/// The shipped migrations up to and including `last`.
fn migrator_through(last: i64) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version <= last)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    }
}

/// Migration 0066 drops the wakeup channel table, counters and all, and
/// keeps every job registry row as it was. 0069 drops the registry later,
/// so this runs the migrations through 0066 only.
#[tokio::test]
async fn wakeup_channels_drop_and_job_rows_survive() {
    let pool = scratch_pool().await;
    migrator_through(65).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "UPDATE durable_work_wakeups SET seq = 3, consumed_seq = 2 WHERE channel = 'scan'; \
         INSERT INTO durable_job_registry (name, kind, state, last_heartbeat_at, updated_at) \
         VALUES ('download-worker', 'durable', 'running', 12.5, 13.0), \
         ('checkpoint', 'ephemeral', 'stopped', NULL, 14.0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    migrator_through(66).run(&pool).await.unwrap();

    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stamped, 66);
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

/// Migration 0069 replaces the job registry with the loop rows. Nothing is
/// carried over: the supervisor clears and rebuilds the rows at every
/// start. The new table is STRICT and checks its state.
#[tokio::test]
async fn job_registry_gives_way_to_loop_rows() {
    let pool = scratch_pool().await;
    migrator_through(68).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO durable_job_registry (name, kind, state, last_heartbeat_at, updated_at) \
         VALUES ('download-worker', 'durable', 'running', 12.5, 13.0);",
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
    assert!(stamped >= 69 && stamped == latest_version(), "{stamped}");
    let left: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE name = 'durable_job_registry'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(left.is_empty(), "{left:?}");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM durable_loops")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    sqlx::query(
        "INSERT INTO durable_loops (name, state, last_heartbeat_at, last_error, updated_at) \
         VALUES ('wal-checkpoint', 'running', 1700000000000, NULL, 1700000000000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for refused in [
        "INSERT INTO durable_loops (name, state, updated_at) VALUES ('a', 'paused', 1)",
        "INSERT INTO durable_loops (name, state, updated_at) VALUES ('b', 'idle', 'soon')",
    ] {
        assert!(
            sqlx::query(refused).execute(&pool).await.is_err(),
            "{refused}"
        );
    }
}

/// Migration 0070 adds the jobs table. It is STRICT and checks its state;
/// a key holds at most one queued row, while a running row and finished
/// rows never block one.
#[tokio::test]
async fn jobs_table_holds_one_queued_row_per_key() {
    let pool = scratch_pool().await;
    migrator_through(69).run(&pool).await.unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stamped >= 70 && stamped == latest_version(), "{stamped}");
    let insert = |state: &str, finished: &str| {
        format!(
            "INSERT INTO durable_jobs (kind, idempotency_key, state, not_before_at, lease_owner, \
             lease_expires_at, payload_json, created_at, updated_at, finished_at) \
             VALUES ('k', 'key', '{state}', 1, 'o', 2, '{{}}', 1, 1, {finished})"
        )
    };
    for allowed in [
        insert("queued", "NULL"),
        insert("running", "NULL"),
        insert("done", "3"),
        insert("done", "4"),
        insert("failed", "5"),
    ] {
        sqlx::query(&allowed).execute(&pool).await.unwrap();
    }
    for refused in [
        insert("queued", "NULL"),
        insert("paused", "NULL"),
        insert("done", "NULL"),
        "INSERT INTO durable_jobs (kind, idempotency_key, state, not_before_at, payload_json, \
         created_at, updated_at) VALUES ('k', 'other', 'queued', 'soon', '{}', 1, 1)"
            .to_owned(),
    ] {
        assert!(
            sqlx::query(&refused).execute(&pool).await.is_err(),
            "{refused}"
        );
    }
}

/// Migration 0071 adds the settings tables, empty: the server copies
/// `config.json` in at its first start, never the migration. Both tables are
/// STRICT, keep only JSON in their JSON columns and refuse an unknown ingest
/// outcome or a second ingest row.
#[tokio::test]
async fn settings_tables_arrive_empty() {
    let pool = scratch_pool().await;
    migrator_through(70).run(&pool).await.unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    for table in ["config_sections", "config_ingests"] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
    sqlx::query(
        "INSERT INTO config_sections (key, value_json, revision, updated_at) \
         VALUES ('user_preferences', '{}', 1, 1700000000000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO config_ingests (singleton, outcome, file_sha256, ingested_at, keys_copied, keys_left) \
         VALUES (1, 'ingested', 'ab', 1, '[]', '[]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    for refused in [
        "INSERT INTO config_sections (key, value_json, revision, updated_at) VALUES ('a', 'not json', 1, 1)",
        "INSERT INTO config_sections (key, value_json, revision, updated_at) VALUES ('b', '{}', 0, 1)",
        "INSERT INTO config_ingests (singleton, outcome, file_sha256, ingested_at, keys_copied, keys_left) \
         VALUES (2, 'ingested', 'ab', 1, '[]', '[]')",
        "UPDATE config_ingests SET outcome = 'renamed'",
        "UPDATE config_ingests SET keys_left = 'x'",
    ] {
        assert!(
            sqlx::query(refused).execute(&pool).await.is_err(),
            "{refused}"
        );
    }
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

/// Every foreign key has an index whose leading columns are the key's
/// columns: without one, deleting or rekeying a parent row scans the
/// child table once per row. A primary key or unique index counts; a
/// partial index does not.
#[tokio::test]
async fn every_foreign_key_is_indexed() {
    let pool = migrated_pool().await;
    let mut unindexed = Vec::new();
    for table in table_names(&pool).await {
        let keys: Vec<(i64, i64, String)> = sqlx::query_as(
            "SELECT id, seq, \"from\" FROM pragma_foreign_key_list(?) ORDER BY id, seq",
        )
        .bind(&table)
        .fetch_all(&pool)
        .await
        .unwrap();
        if keys.is_empty() {
            continue;
        }
        let mut by_key: BTreeMap<i64, Vec<String>> = BTreeMap::new();
        for (id, _, column) in keys {
            by_key.entry(id).or_default().push(column);
        }
        let indexes: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_index_list(?) WHERE partial = 0")
                .bind(&table)
                .fetch_all(&pool)
                .await
                .unwrap();
        let mut leading = Vec::new();
        for index in indexes {
            let columns: Vec<Option<String>> =
                sqlx::query_scalar("SELECT name FROM pragma_index_info(?) ORDER BY seqno")
                    .bind(&index)
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            leading.push(columns.into_iter().flatten().collect::<Vec<_>>());
        }
        // An INTEGER PRIMARY KEY is the rowid and has no index of its own.
        let rowid: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info(?) WHERE pk = 1 AND upper(type) = 'INTEGER' \
             AND (SELECT COUNT(*) FROM pragma_table_info(?) WHERE pk > 0) = 1",
        )
        .bind(&table)
        .bind(&table)
        .fetch_all(&pool)
        .await
        .unwrap();
        leading.push(rowid);
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

/// Migration 0067 adds the foreign key indexes to a database at 66 and
/// keeps its rows.
#[tokio::test]
async fn foreign_key_indexes_arrive_on_upgrade() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 67)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO library_track_management_state (local_track_id, baseline_id) \
         VALUES ('t1', 'b1');",
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
    assert!(stamped >= 67 && stamped == latest_version(), "{stamped}");
    let index: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' \
         AND name = 'idx_library_track_management_state_baseline_id'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(index.is_some());
    let kept: Option<String> = sqlx::query_scalar(
        "SELECT local_track_id FROM library_track_management_state WHERE baseline_id = 'b1'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(kept.as_deref(), Some("t1"));
}

/// Migration 0082 adds the table of folders a publish made to a database
/// at 81, keeps its publish rows, and a folder row goes with its publish.
#[tokio::test]
async fn created_folders_arrive_on_upgrade() {
    let pool = scratch_pool().await;
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 82)
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
         VALUES ('b1', '{}', 1); \
         INSERT INTO library_publish_journal (id, bundle_id, kind, dest_root, dest_rel, \
         staged, staged_sha256, state) \
         VALUES ('j1', 'b1', 'audio', 'music', 'a/b.flac', '/t', 'sha', 'staged');",
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
    assert!(stamped >= 82 && stamped == latest_version(), "{stamped}");
    let sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' \
         AND name = 'library_publish_created_folders'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(sql.trim_end().ends_with("STRICT"), "{sql}");
    let journal: Option<String> =
        sqlx::query_scalar("SELECT state FROM library_publish_journal WHERE id = 'j1'")
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert_eq!(journal.as_deref(), Some("staged"));

    sqlx::raw_sql(
        "PRAGMA foreign_keys = ON; \
         INSERT INTO library_publish_created_folders (bundle_id, root_id, folder_rel) \
         VALUES ('b1', 'music', 'a'); \
         DELETE FROM library_publish_operations WHERE bundle_id = 'b1';",
    )
    .execute(&pool)
    .await
    .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_publish_created_folders")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "a folder row goes with its publish");
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
    ("auth", droppedneedle_auth::statement_files),
    ("catalog", droppedneedle_catalog::statement_files),
    ("acquire", droppedneedle_acquire::statement_files),
    ("library", droppedneedle_library::statement_files),
    ("media", droppedneedle_media::statement_files),
    ("compat", droppedneedle_compat::statement_files),
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
    let cache =
        rusqlite::Connection::open(dir.join("metadata-cache.db")).expect("cache file opens");
    cache
        .execute_batch(droppedneedle_kernel::db::cache::SCHEMA)
        .expect("cache schema builds");
    let (mut statements, mut families, mut made) = (0, 0, 0);
    for (schema, declared) in registered
        .values()
        .flat_map(|(schema, declared)| declared.iter().map(move |one| (*schema, one)))
    {
        let connection = match schema {
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
/// reads every identified track to find each album's worst file, and the
/// quota's library total, which sums every indexed track (10 ms on 100k
/// tracks). The acquire reads of one release group's or one recording's
/// tracks (the album and recording quality checks and "the library holds
/// it") walk only because no index covers lowered release-group or
/// recording ids; the expression indexes that cover them take them off
/// this list.
const TRACK_WALKS: &[&str] = &[
    "droppedneedle_catalog::reads::library::sql::TRACK_PAGE_LIKE",
    "droppedneedle_catalog::reads::library::sql::TRACK_PAGE_WORDS",
    "droppedneedle_catalog::reads::library::sql::TRACK_COUNT_LIKE",
    "droppedneedle_catalog::reads::library::sql::TRACK_COUNT_WORDS",
    "droppedneedle_catalog::reads::library::sql::NEWEST_TRACKS",
    "droppedneedle_catalog::reads::library::sql::OLDEST_TRACKS",
    "droppedneedle_catalog::reads::library::sql::RANDOM_TRACK_IDS",
    "droppedneedle_catalog::reads::library::sql::GENRES",
    "droppedneedle_catalog::reads::library::sql::GENRE_TRACKS",
    "droppedneedle_catalog::reads::library::sql::GENRE_TRACK_COUNT",
    "droppedneedle_catalog::reads::library::sql::PLAYABLE_COUNTS",
    "droppedneedle_catalog::reads::library::sql::PLAYER_TRACK_PAGE",
    "droppedneedle_catalog::reads::library::sql::PLAYER_TRACK_COUNT",
    "droppedneedle_catalog::reads::library::sql::PLAYER_TRACKS_UNPLAYED",
    "droppedneedle_catalog::reads::library::sql::PLAYER_TRACKS_UNPLAYED_COUNT",
    "droppedneedle_compat::adapters::jellyfin_ids::sql::NEW_TRACKS",
    "droppedneedle_compat::adapters::jellyfin_ids::sql::GENRE_NAMES",
    "droppedneedle_acquire::flows::sql::IDENTIFIED_TRACKS",
    "droppedneedle_acquire::requests::sql::LIBRARY_BYTES",
];

/// Before the first ANALYZE (a fresh install) the planner has no
/// statistics, and an index led by `availability` can look as good as a
/// key: a read of one album's tracks then walks every indexed track. Every
/// registered statement outside [`TRACK_WALKS`] reaches the track table by
/// a key, never by a walk of it or of an index led by `availability`.
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
            if !matches!(file.schema, Schema::App) {
                continue;
            }
            for declared in file.declared {
                let (name, texts) = match declared {
                    Declared::Statement(one) => (one.name(), vec![one.text().to_owned()]),
                    Declared::Family(family) => (family.name(), family.texts().unwrap_or_default()),
                    Declared::Fragment(_) => continue,
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

/// Migration 0072 moves contribution verification onto the jobs table.
/// A queued and a running verification become queued jobs with their
/// attempts, due time and code; the reason of one that went to review
/// stays on its contribution; the old table is gone.
#[tokio::test]
async fn contribution_verification_moves_onto_jobs() {
    let pool = scratch_pool().await;
    migrator_through(71).run(&pool).await.unwrap();
    // The rows stand alone: no album or user behind them.
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO library_contribution_drafts (id, local_album_id, state, \
             album_row_revision, input_revision, local_snapshot_json, resolved_draft_json, \
             source_selection_json, created_at, updated_at) \
         VALUES ('c-queued', 'a1', 'verifying', 1, 'r', '{}', '{}', '{}', 1.0, 1.0), \
                ('c-running', 'a2', 'verifying', 1, 'r', '{}', '{}', '{}', 1.0, 1.0), \
                ('c-review', 'a3', 'needs_review', 1, 'r', '{}', '{}', '{}', 1.0, 1.0); \
         INSERT INTO library_contribution_verification_jobs (id, contribution_id, state, \
             attempt_count, not_before, last_failure_code, lease_owner, lease_expires_at, \
             created_at, updated_at, terminal_at) \
         VALUES ('j1', 'c-queued', 'queued', 2, 1700000100.5, \
                 'MUSICBRAINZ_RELEASE_NOT_PROPAGATED', NULL, NULL, 1700000000.0, \
                 1700000050.0, NULL), \
                ('j2', 'c-running', 'running', 1, 1700000000.0, NULL, 'worker', \
                 1700000090.0, 1700000000.0, 1700000010.0, NULL), \
                ('j3', 'c-review', 'needs_review', 10, 0, 'ATTACHMENT_NAME_MISMATCH', NULL, \
                 NULL, 1700000000.0, 1700000020.0, 1700000020.0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let jobs: Vec<(String, String, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT idempotency_key, state, attempts, not_before_at, last_error FROM durable_jobs \
         WHERE kind = 'library.contribution_verify' ORDER BY idempotency_key",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        jobs,
        vec![
            (
                "c-queued".to_owned(),
                "queued".to_owned(),
                2,
                1_700_000_100_500,
                Some("MUSICBRAINZ_RELEASE_NOT_PROPAGATED".to_owned()),
            ),
            (
                "c-running".to_owned(),
                "queued".to_owned(),
                1,
                1_700_000_000_000,
                None,
            ),
        ]
    );
    let reason: Option<String> = sqlx::query_scalar(
        "SELECT last_verification_failure FROM library_contribution_drafts WHERE id = 'c-review'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reason.as_deref(), Some("ATTACHMENT_NAME_MISMATCH"));
    let left: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE name LIKE '%contribution_verification_jobs%' \
         OR name LIKE 'idx_contribution_job_%'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(left.is_empty(), "{left:?}");
}

/// Migration 0073 deletes the rows of the two removed settings sections and
/// takes `target_bitrate_kbps` out of each stored quality recipe step, so
/// the download policy still decodes. Every other settings row is left
/// byte for byte.
#[tokio::test]
async fn removed_settings_leave_the_stored_rows() {
    use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;

    let pool = scratch_pool().await;
    migrator_through(72).run(&pool).await.unwrap();
    let policy = r#"{"quality_min":"mp3_320","flac_mp3_only":true,"quality_recipe":[{"format":"flac","quality":"cd","min_bitrate_kbps":null,"target_bitrate_kbps":null,"max_bitrate_kbps":null,"bit_depth":null,"sample_rate_hz":null},{"format":"mp3","quality":"custom","min_bitrate_kbps":192,"target_bitrate_kbps":256,"max_bitrate_kbps":320,"bit_depth":null,"sample_rate_hz":null}],"verify_downloads":false}"#;
    let kept = [
        (
            "advanced_settings",
            r#"{"http_timeout":30,"discover_queue_size":12}"#,
        ),
        ("wanted", r#"{"enabled":true,"dormant_after_days":90}"#),
    ];
    let mut rows = vec![
        ("home_settings", r#"{"show_whats_hot":false}"#),
        ("scrobble_settings", r#"{"scrobble_to_lastfm":true}"#),
        ("download_policy", policy),
    ];
    rows.extend(kept);
    for (key, value) in &rows {
        sqlx::query(
            "INSERT INTO config_sections (key, value_json, revision, updated_at) \
             VALUES (?, ?, 4, 1700000000000)",
        )
        .bind(key)
        .bind(value)
        .execute(&pool)
        .await
        .unwrap();
    }

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let stored: BTreeMap<String, (String, i64, i64)> =
        sqlx::query_as::<_, (String, String, i64, i64)>(
            "SELECT key, value_json, revision, updated_at FROM config_sections",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(key, value, revision, at)| (key, (value, revision, at)))
        .collect();
    let keys: Vec<&str> = stored.keys().map(String::as_str).collect();
    assert_eq!(keys, ["advanced_settings", "download_policy", "wanted"]);
    for (key, value) in kept {
        assert_eq!(
            stored[key],
            (value.to_owned(), 4, 1_700_000_000_000),
            "{key}"
        );
    }
    let (value, revision, _) = &stored["download_policy"];
    assert_eq!(*revision, 5);
    assert!(!value.contains("target_bitrate_kbps"), "{value}");
    let decoded: DownloadPolicy = serde_json::from_str(value).expect("the policy decodes");
    let steps: Vec<(&str, &str, Option<i64>, Option<i64>)> = decoded
        .quality_recipe
        .iter()
        .map(|step| {
            (
                step.format.as_str(),
                step.quality.as_str(),
                step.min_bitrate_kbps,
                step.max_bitrate_kbps,
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            ("flac", "cd", None, None),
            ("mp3", "custom", Some(192), Some(320))
        ]
    );
}

/// Migration 0074 copies the bundle id a work row kept inside its result
/// into the new `bundle_id` column. A row with no bundle in its result,
/// no result, or a result that is not JSON gets NULL; the result itself
/// is left as it was, and the column is indexed.
#[tokio::test]
async fn work_bundle_ids_move_into_a_column() {
    let pool = scratch_pool().await;
    migrator_through(73).run(&pool).await.unwrap();
    let applied = r#"{"bundle_id":"bundle-1","outcome":"committed","recycled_to":["a.flac"]}"#;
    // The rows stand alone: no job or album behind them.
    sqlx::raw_sql(&format!(
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO library_operation_work (job_id, ordinal, local_album_id, \
             expected_subject_revision, expected_input_revision, action, idempotency_key, \
             state, result_json, updated_at) \
         VALUES ('j', 0, 'a0', 1, 'r', 'apply', 'k0', 'succeeded', '{applied}', 1.0), \
                ('j', 1, 'a1', 1, 'r', 'apply', 'k1', 'failed', \
                 '{{\"reason\":{{\"code\":\"PROFILE_CHANGED\"}}}}', 1.0), \
                ('j', 2, 'a2', 1, 'r', 'apply', 'k2', 'pending', NULL, 1.0), \
                ('j', 3, 'a3', 1, 'r', 'apply', 'k3', 'failed', 'not json', 1.0);"
    ))
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let rows: Vec<(i64, Option<String>)> =
        sqlx::query_as("SELECT ordinal, bundle_id FROM library_operation_work ORDER BY ordinal")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            (0, Some("bundle-1".to_owned())),
            (1, None),
            (2, None),
            (3, None)
        ]
    );
    let kept: String =
        sqlx::query_scalar("SELECT result_json FROM library_operation_work WHERE ordinal = 0")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kept, applied);
    let index: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'index' \
         AND name = 'idx_library_operation_work_bundle_id'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(
        index
            .as_deref()
            .is_some_and(|sql| sql.contains("library_operation_work(bundle_id)")),
        "the bundle id is indexed: {index:?}"
    );
}

/// Migration 0075 gives every replaced file's row an `expired_at`, NULL
/// on rows already there, and indexes the rows not yet expired by when
/// their copy reached the bin.
#[tokio::test]
async fn recycled_rows_gain_an_expiry_column() {
    let pool = scratch_pool().await;
    migrator_through(74).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO library_publish_journal (id, bundle_id, kind, dest_root, dest_rel, \
             staged, staged_sha256, state) \
         VALUES ('j1', 'b1', 'remove', 'music', 'a.flac', '.dn-aside', 'abc', 'cleaned'); \
         INSERT INTO library_publish_replacements (journal_id, replaced_rel, replaced_sha256, \
             replaced_track_id, recycle_to, recycled_at) \
         VALUES ('j1', 'a.flac', 'abc', 't1', '/bin/20250101T000000-0a1b2c3d/a.flac', \
             1735689600.5);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let row: (String, f64, Option<i64>) = sqlx::query_as(
        "SELECT journal_id, recycled_at, expired_at FROM library_publish_replacements",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, ("j1".to_owned(), 1_735_689_600.5, None));
    let index: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'index' \
         AND name = 'idx_library_publish_replacements_due'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(
        index.as_deref().is_some_and(|sql| sql
            .contains("library_publish_replacements(recycled_at, journal_id)")
            && sql.contains("WHERE expired_at IS NULL")),
        "the due rows are indexed: {index:?}"
    );
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(version >= 75, "user_version {version}");
}

/// Migration 0077 adds `plugin_library_writes` to a database at 0076: a
/// STRICT table with one row per recorded change, and the rows one
/// plugin's allowance reads indexed by time.
#[tokio::test]
async fn plugin_library_writes_table_is_added() {
    let pool = scratch_pool().await;
    migrator_through(76).run(&pool).await.unwrap();
    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    for (values, refused) in [
        ("('b1', 'toy', 5, 1735689600000)", false),
        ("('b1', 'toy', 6, 1735689600000)", true),
        ("('b2', 'toy', 'five', 1735689600000)", true),
        ("('b3', 'toy', -1, 1735689600000)", true),
    ] {
        let sql = format!(
            "INSERT INTO plugin_library_writes (bundle_id, plugin, bytes, written_at) \
             VALUES {values}"
        );
        let outcome = sqlx::raw_sql(&sql).execute(&pool).await;
        assert_eq!(outcome.is_err(), refused, "{values}: {outcome:?}");
    }
    let row: (String, String, i64, i64) =
        sqlx::query_as("SELECT bundle_id, plugin, bytes, written_at FROM plugin_library_writes")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        row,
        ("b1".to_owned(), "toy".to_owned(), 5, 1_735_689_600_000)
    );
    let index: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'index' \
         AND name = 'idx_plugin_library_writes_plugin'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(
        index
            .as_deref()
            .is_some_and(|sql| sql.contains("plugin_library_writes(plugin, written_at)")),
        "a plugin's rows are indexed by time: {index:?}"
    );
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(version >= 77, "user_version {version}");
}

/// Migration 0078 adds `library_recycle_bin_empties` to a database at
/// 0077: a STRICT table with one row per empty of the bin, unfinished until
/// it says when, whose counts are never negative and whose admin is
/// cleared, not deleted with, when the account goes.
#[tokio::test]
async fn recycle_bin_empties_table_is_added() {
    let pool = scratch_pool().await;
    migrator_through(77).run(&pool).await.unwrap();
    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    for (values, refused) in [
        ("('e1', 'admin', 1, 2, 3, 4, 0, 1, 2, '[]')", false),
        ("('e2', NULL, 1, 2, -1, 4, 0, 1, 2, '[]')", true),
        ("('e3', NULL, 1, 2, 'three', 4, 0, 1, 2, '[]')", true),
        ("('e4', NULL, 1, NULL, 0, 0, 0, 0, 0, '[]')", false),
    ] {
        let sql = format!(
            "INSERT INTO library_recycle_bin_empties (id, actor_user_id, before_at, \
             finished_at, deleted, freed_bytes, older_folders, kept, waited, kept_json) \
             VALUES {values}"
        );
        let outcome = sqlx::raw_sql(&sql).execute(&pool).await;
        assert_eq!(outcome.is_err(), refused, "{values}: {outcome:?}");
    }
    sqlx::raw_sql("PRAGMA foreign_keys = ON; DELETE FROM auth_users WHERE id = 'admin';")
        .execute(&pool)
        .await
        .unwrap();
    let row: (String, Option<String>, i64) = sqlx::query_as(
        "SELECT id, actor_user_id, deleted FROM library_recycle_bin_empties WHERE id = 'e1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, ("e1".to_owned(), None, 3));
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(version >= 78, "user_version {version}");
}

/// Migration 0079 adds the indexes the track list's artist and album
/// orders walk, on a database at 0078.
#[tokio::test]
async fn track_list_orders_walk_an_index() {
    let pool = scratch_pool().await;
    migrator_through(78).run(&pool).await.unwrap();
    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    for (name, columns) in [
        (
            "idx_local_tracks_artist_order",
            "availability, COALESCE(artist_name_folded, ''), album_title_folded",
        ),
        (
            "idx_local_tracks_album_order_folded",
            "availability, album_title_folded, local_album_id",
        ),
    ] {
        let sql: Option<String> =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?")
                .bind(name)
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(
            sql.as_deref().is_some_and(|sql| sql.contains(columns)),
            "{name}: {sql:?}"
        );
    }
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(version >= 79, "user_version {version}");
}

/// The artist and album pairs and the artist counts as the artist list
/// counts them, worked out from the source rows: what migration 0080 keeps.
const WORKED_PAIRS: &str = "SELECT c.local_artist_id, t.local_album_id, \
     COUNT(DISTINCT t.id), b.retired_into_album_id IS NULL, \
     b.retired_into_album_id IS NULL AND b.album_artist_id != c.local_artist_id \
     FROM local_track_artists c JOIN local_tracks t ON t.id = c.local_track_id \
     JOIN local_albums b ON b.id = t.local_album_id \
     WHERE t.availability = 'indexed' GROUP BY 1, 2 ORDER BY 1, 2";
const WORKED_COUNTS: &str = "SELECT r.id, r.folded_name, \
     (SELECT COUNT(*) FROM local_albums lo WHERE lo.album_artist_id = r.id \
      AND lo.retired_into_album_id IS NULL AND lo.has_indexed_track = 1), \
     (SELECT COUNT(DISTINCT t.local_album_id) FROM local_track_artists c \
      JOIN local_tracks t ON t.id = c.local_track_id \
      JOIN local_albums b ON b.id = t.local_album_id \
      WHERE c.local_artist_id = r.id AND t.availability = 'indexed' \
      AND b.retired_into_album_id IS NULL AND b.album_artist_id != r.id) \
     FROM local_artists r WHERE r.retired_into_artist_id IS NULL AND ( \
     EXISTS (SELECT 1 FROM local_album_artists laa JOIN local_albums x ON x.id = laa.local_album_id \
      WHERE laa.local_artist_id = r.id AND x.retired_into_album_id IS NULL \
      AND x.has_indexed_track = 1) \
     OR EXISTS (SELECT 1 FROM local_track_artists c JOIN local_tracks t ON t.id = c.local_track_id \
      JOIN local_albums x ON x.id = t.local_album_id WHERE c.local_artist_id = r.id \
      AND t.availability = 'indexed' AND x.retired_into_album_id IS NULL) \
     OR EXISTS (SELECT 1 FROM local_albums x WHERE x.album_artist_id = r.id \
      AND x.retired_into_album_id IS NULL AND x.has_indexed_track = 1)) ORDER BY 1";

/// Migration 0080 works out every artist's album and appearance counts and
/// the artist and album pairs behind them from the catalog already there:
/// a leader, a guest on a live album and on a retired one, an artist on a
/// retired album alone, one credited only on an album, one credited only on
/// an album with no file, one credited twice on a track, a retired artist,
/// a missing track and an artist with no credits. Both tables are STRICT
/// and WITHOUT ROWID, and a write afterwards keeps them.
#[tokio::test]
async fn artist_counts_arrive_worked_out_from_the_catalog() {
    let pool = scratch_pool().await;
    migrator_through(79).run(&pool).await.unwrap();
    let track = |id: &str, album: &str, availability: &str| {
        format!(
            "('{id}', '{album}', 'r1', '/m/{id}.flac', '{id}.flac', 'h-{id}', 1, 0, 's', 'T', \
             't', 'T', 't', 'flac', '{availability}', 'scan', 0, 'automatic')"
        )
    };
    sqlx::raw_sql(&format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('lead', 'Lead', 'lead', 'group', 1, 1), ('guest', 'Guest', 'guest', 'person', 1, 1), \
         ('gone', 'Gone', 'gone', 'person', 1, 1), ('co', 'Co', 'co', 'person', 1, 1), \
         ('idle', 'Idle', 'idle', 'person', 1, 1), ('ghost', 'Ghost', 'ghost', 'person', 1, 1), \
         ('old', 'Old', 'old', 'person', 1, 1); \
         UPDATE local_artists SET retired_into_artist_id = 'lead' WHERE id = 'old'; \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'T', 't', 'lead', 'automatic', 1, 1), \
         ('al2', 'r1', 'g2', 'T', 't', 'lead', 'automatic', 1, 1), \
         ('al3', 'r1', 'g3', 'T', 't', 'lead', 'automatic', 1, 1), \
         ('al4', 'r1', 'g4', 'T', 't', 'lead', 'automatic', 1, 1); \
         UPDATE local_albums SET retired_into_album_id = 'al1' WHERE id = 'al3'; \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, availability, ingest_source, \
         imported_at, membership_source) VALUES {}, {}, {}, {}, {}; \
         INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) VALUES \
         ('t1', 0, 'lead', 'main'), ('t1', 1, 'guest', 'guest'), ('t1', 2, 'guest', 'guest'), \
         ('t2', 0, 'lead', 'main'), ('t2', 1, 'ghost', 'guest'), \
         ('t3', 0, 'guest', 'main'), ('t3', 1, 'gone', 'main'), ('t4', 0, 'lead', 'main'), \
         ('t5', 0, 'guest', 'main'); \
         INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) VALUES \
         ('al1', 0, 'lead', 'main'), ('al1', 1, 'co', 'main'), ('al2', 0, 'idle', 'main');",
        track("t1", "al1", "indexed"),
        track("t2", "al1", "missing"),
        track("t3", "al3", "indexed"),
        track("t4", "al4", "indexed"),
        track("t5", "al2", "excluded"),
    ))
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();
    type Pair = (String, String, i64, bool, bool);
    type Count = (String, String, i64, i64);
    async fn kept(pool: &SqlitePool) -> (Vec<Pair>, Vec<Count>) {
        let pairs = sqlx::query_as(
            "SELECT local_artist_id, local_album_id, indexed_tracks, live, appearance \
             FROM library_artist_album_credits ORDER BY 1, 2",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        let counts = sqlx::query_as(
            "SELECT local_artist_id, folded_name, album_count, appearance_count \
             FROM library_artist_counts ORDER BY 1",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        (pairs, counts)
    }
    async fn worked(pool: &SqlitePool) -> (Vec<Pair>, Vec<Count>) {
        let pairs = sqlx::query_as(WORKED_PAIRS).fetch_all(pool).await.unwrap();
        let counts = sqlx::query_as(WORKED_COUNTS).fetch_all(pool).await.unwrap();
        (pairs, counts)
    }
    let pair = |artist: &str, album: &str, tracks: i64, live: bool, appearance: bool| {
        (
            artist.to_owned(),
            album.to_owned(),
            tracks,
            live,
            appearance,
        )
    };
    let count = |artist: &str, albums: i64, appearances: i64| {
        (artist.to_owned(), artist.to_owned(), albums, appearances)
    };
    let (pairs, counts) = kept(&pool).await;
    assert_eq!(
        pairs,
        [
            pair("gone", "al3", 1, false, false),
            pair("guest", "al1", 1, true, true),
            pair("guest", "al3", 1, false, false),
            pair("lead", "al1", 1, true, false),
            pair("lead", "al4", 1, true, false),
        ]
    );
    // `co` is credited on an owned album; `idle` only on one with no
    // indexed track; `gone` only on a retired one; `ghost` only on a
    // missing track; `old` is retired.
    assert_eq!(
        counts,
        [count("co", 0, 0), count("guest", 0, 1), count("lead", 2, 0)]
    );
    assert_eq!((pairs, counts), worked(&pool).await);

    for (table, key) in [
        (
            "library_artist_album_credits",
            "local_artist_id,local_album_id",
        ),
        ("library_artist_counts", "local_artist_id"),
    ] {
        let shape: (String, i64, i64) =
            sqlx::query_as("SELECT type, strict, wr FROM pragma_table_list WHERE name = ?")
                .bind(table)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            shape,
            ("table".to_owned(), 1, 1),
            "{table}: STRICT, WITHOUT ROWID"
        );
        let found: String = sqlx::query_scalar(
            "SELECT group_concat(name, ',') FROM (SELECT name FROM pragma_table_info(?1) \
             WHERE pk > 0 ORDER BY pk)",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(found, key, "{table}");
    }
    for (index, columns) in [
        ("idx_artist_album_credits_album", "local_album_id"),
        ("idx_artist_album_credits_appearances", "local_artist_id"),
        (
            "idx_artist_counts_albums",
            "album_count,folded_name,local_artist_id",
        ),
        (
            "idx_artist_counts_albums_desc",
            "album_count_neg,folded_name,local_artist_id",
        ),
        (
            "idx_artist_counts_appearances",
            "appearance_count,folded_name,local_artist_id,album_count",
        ),
        (
            "idx_artist_counts_appearances_desc",
            "appearance_count_neg,folded_name,local_artist_id,album_count",
        ),
        (
            "idx_local_albums_owned_leader",
            "album_artist_id,retired_into_album_id,has_indexed_track",
        ),
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

    // The idle artist's album gains a file; the guest's live track goes.
    sqlx::raw_sql(&format!(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, availability, ingest_source, \
         imported_at, membership_source) VALUES {}; \
         INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
         VALUES ('t6', 0, 'lead', 'main'); \
         DELETE FROM local_track_artists WHERE local_track_id = 't1' AND position > 0;",
        track("t6", "al2", "indexed"),
    ))
    .execute(&pool)
    .await
    .unwrap();
    let (pairs, counts) = kept(&pool).await;
    assert!(counts.contains(&count("idle", 0, 0)), "{counts:?}");
    assert!(!counts.iter().any(|row| row.0 == "guest"), "{counts:?}");
    assert_eq!((pairs, counts), worked(&pool).await);
}

/// Migration 0081 pins the genre artwork triggers' track lookup to the
/// album's index. On a database at 0080 with no planner statistics, each
/// trigger's lookup reads by album, not every indexed track, and the
/// triggers still bump the revisions of the album's indexed genres only.
#[tokio::test]
async fn genre_artwork_triggers_read_the_album_tracks_index() {
    let pool = scratch_pool().await;
    migrator_through(80).run(&pool).await.unwrap();
    let track = |id: &str, availability: &str| {
        format!(
            "('{id}', 'al1', 'r1', '/m/{id}.flac', '{id}.flac', 'h-{id}', 1, 0, 's', 'T', \
             't', 'T', 't', 'flac', '{availability}', 'scan', 0, 'automatic')"
        )
    };
    sqlx::raw_sql(&format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'A', 'a', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('al1', 'r1', 'g1', 'T', 't', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, availability, ingest_source, \
         imported_at, membership_source) VALUES {}, {}; \
         INSERT INTO local_track_genres (local_track_id, position, name, folded_name, source) \
         VALUES ('t1', 0, 'Rock', 'rock', 'local'), ('t2', 0, 'Jazz', 'jazz', 'local');",
        track("t1", "indexed"),
        track("t2", "missing"),
    ))
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();
    let statistics: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = 'sqlite_stat1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(statistics, 0);

    let triggers: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_master WHERE type = 'trigger' \
         AND name LIKE 'trg_genre_artwork_%' AND sql LIKE '%FROM local_tracks t%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let names: Vec<&str> = triggers.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "trg_genre_artwork_album_update",
            "trg_genre_artwork_artwork_delete",
            "trg_genre_artwork_artwork_insert",
            "trg_genre_artwork_artwork_update",
        ]
    );
    // The lookup each trigger runs, with its row's album id filled in.
    let row_value = Regex::new(r"\b(?:NEW|OLD)\.\w+").unwrap();
    for (name, sql) in &triggers {
        let start = sql.find("SELECT DISTINCT").unwrap();
        let end = sql.find("ON CONFLICT").unwrap();
        let lookup = row_value.replace_all(&sql[start..end], "'al1'");
        let plan: Vec<(i64, i64, i64, String)> =
            sqlx::query_as(&format!("EXPLAIN QUERY PLAN {lookup}"))
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            plan.iter().any(|step| step.3.contains(
                "SEARCH t USING INDEX idx_local_tracks_album_availability (local_album_id=? AND availability=?)"
            )),
            "{name}: {plan:?}"
        );
    }

    async fn revisions(pool: &SqlitePool) -> Vec<(String, i64)> {
        sqlx::query_as("SELECT genre_folded, value FROM library_genre_artwork_revisions ORDER BY 1")
            .fetch_all(pool)
            .await
            .unwrap()
    }
    let before = revisions(&pool).await;
    assert_eq!(before, [("jazz".to_owned(), 1), ("rock".to_owned(), 1)]);
    for (write, rock) in [
        ("UPDATE local_albums SET title = 'U' WHERE id = 'al1'", 2),
        (
            "INSERT INTO local_album_artwork (local_album_id, source, version, updated_at) \
             VALUES ('al1', 'folder', 1, 1)",
            3,
        ),
        (
            "UPDATE local_album_artwork SET version = 2 WHERE local_album_id = 'al1'",
            4,
        ),
        (
            "DELETE FROM local_album_artwork WHERE local_album_id = 'al1'",
            5,
        ),
    ] {
        sqlx::raw_sql(write).execute(&pool).await.unwrap();
        assert_eq!(
            revisions(&pool).await,
            [("jazz".to_owned(), 1), ("rock".to_owned(), rock)],
            "{write}"
        );
    }
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(version >= 81, "user_version {version}");
}

/// Migration 0076 rebuilds the request, watch and approval tables as
/// STRICT tables with checked states and millisecond times. A database
/// before it, with a row in every state and times in every stored form,
/// keeps each row with its time to the millisecond. Its dormant watch
/// watches again and is due, rows pointing at a user or request that is
/// gone are cleared or dropped instead of failing the upgrade, and no
/// trigger comes back on the approvals.
#[tokio::test]
async fn request_watch_and_approval_rows_keep_their_meaning_and_states_are_checked() {
    let pool = scratch_pool().await;
    migrator_through(75).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'Asker', 'user', '2024-01-01'), ('u2', 'Admin', 'admin', '2024-01-01'); \
         INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, year, requested_at, completed_at, status, user_id, reviewed_by_id, \
         reviewed_at, request_kind, duration_seconds) VALUES \
         ('r-pending', 'R', 'a', 'b', 1999, '2026-10-07T22:43:09.330331+00:00', NULL, \
          'pending', 'u1', 'ghost', NULL, 'album', NULL), \
         ('r-queued', 'R', 'a', 'b', '1999', '1700000000', NULL, 'queued', 'u1', NULL, \
          NULL, 'album', NULL), \
         ('r-downloading', 'R', 'a', 'b', 1999.0, '1700000000.25', NULL, 'downloading', \
          'u1', NULL, NULL, 'album', NULL), \
         ('r-awaiting', 'R', 'a', 'b', 'soon', 1700000001, NULL, 'awaiting_approval', \
          'ghost', NULL, NULL, 'album', NULL), \
         ('r-cancelling', 'R', 'a', 'b', NULL, '2024-01-01 10:00:00', NULL, 'cancelling', \
          'u1', NULL, NULL, 'album', NULL), \
         ('r-cancelled', 'R', 'a', 'b', NULL, '1700000000', '1700000002', 'cancelled', \
          'u1', NULL, NULL, 'album', NULL), \
         ('r-failed', 'R', 'a', 'b', NULL, 'garbage', '1700000500', 'failed', 'u1', NULL, \
          NULL, 'album', NULL), \
         ('r-imported', 'R', 'a', 'b', NULL, '1700000000', '1700000600', 'imported', 'u1', \
          'u2', '2023-11-14T22:13:20Z', 'album', NULL), \
         ('r-incomplete', 'R', 'a', 'b', NULL, '1700000000', NULL, 'incomplete', 'u1', NULL, \
          NULL, 'album', NULL), \
         ('track:r-rejected', 'R', 'a', 'b', NULL, '1700000000', '1700000700', 'rejected', \
          'u1', 'u2', '1700000700', 'track', 241.6); \
         INSERT INTO request_history_requesters (user_id, musicbrainz_id_lower, requested_at) \
         VALUES ('u1', 'r-pending', '2026-10-07T22:43:09.330331+00:00'), \
         ('u2', 'r-pending', 'garbage'), ('ghost', 'r-pending', '1'), ('u1', 'gone', '1'); \
         INSERT INTO request_history_dismissals (user_id, musicbrainz_id_lower) \
         VALUES ('u1', 'r-imported'), ('ghost', 'r-imported'), ('u1', 'gone'); \
         INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, user_id, \
         artist_name, album_title, kind, state, created_at, last_checked_at, next_check_at) \
         VALUES \
         ('w-watching', 'W', 'u1', 'a', 'b', 'missing', 'watching', 1700000000.5, NULL, \
          1800000000.25), \
         ('w-dormant', 'W', 'u1', 'a', 'b', 'missing', 'dormant', 1600000000.125, \
          1600000001.0, 4000000000.0), \
         ('w-stopped', 'W', 'u1', 'a', 'b', 'partial', 'stopped', 1700000000.0, NULL, \
          1700000000.0), \
         ('w-fulfilled', 'W', 'u1', 'a', 'b', 'missing', 'fulfilled', 1700000000.0, \
          1700000100.75, 1700000000.0), \
         ('w-orphan', 'W', 'ghost', 'a', 'b', 'missing', 'watching', 1.0, NULL, 2.0); \
         INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, state, requested_at, reviewed_by_id, reviewed_at) VALUES \
         ('u1', 'A1', 'a1', 'n', 'pending', 1700000000.125, NULL, NULL), \
         ('u1', 'A2', 'a2', 'n', 'approved', 1700000000.0, 'u2', 1700000010.5), \
         ('u1', 'A3', 'a3', 'n', 'rejected', 1700000000.0, 'ghost', 1700000020.0), \
         ('u1', 'A4', 'a4', 'n', 'revoked', 1700000000.0, 'u2', 1700000030.0);",
    )
    .execute(&pool)
    .await
    .unwrap();
    let before_ms = now_ms();

    migrator_through(76).run(&pool).await.unwrap();

    let after_ms = now_ms();
    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stamped, 76);

    let requests: Vec<RequestRow> = sqlx::query_as(
        "SELECT musicbrainz_id_lower, status, requested_at, completed_at, user_id, \
         reviewed_by_id, reviewed_at, origin, year, duration_seconds \
         FROM request_history ORDER BY musicbrainz_id_lower",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let u1 = Some("u1");
    let u2 = Some("u2");
    assert_eq!(
        requests,
        vec![
            request(
                "r-awaiting",
                "awaiting_approval",
                1_700_000_001_000,
                None,
                None
            ),
            request(
                "r-cancelled",
                "cancelled",
                1_700_000_000_000,
                Some(1_700_000_002_000),
                u1
            ),
            request("r-cancelling", "cancelling", 1_704_103_200_000, None, u1),
            request("r-downloading", "downloading", 1_700_000_000_250, None, u1).year(1999),
            // An unreadable ask time takes the finish time.
            request(
                "r-failed",
                "failed",
                1_700_000_500_000,
                Some(1_700_000_500_000),
                u1
            ),
            request(
                "r-imported",
                "imported",
                1_700_000_000_000,
                Some(1_700_000_600_000),
                u1
            )
            .reviewed(u2, 1_700_000_000_000),
            request("r-incomplete", "incomplete", 1_700_000_000_000, None, u1),
            request("r-pending", "pending", 1_791_412_989_330, None, u1).year(1999),
            request("r-queued", "queued", 1_700_000_000_000, None, u1).year(1999),
            request(
                "track:r-rejected",
                "rejected",
                1_700_000_000_000,
                Some(1_700_000_700_000),
                u1
            )
            .reviewed(u2, 1_700_000_700_000)
            .duration(242),
        ]
    );

    let requesters: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT user_id, musicbrainz_id_lower, requested_at FROM request_history_requesters \
         ORDER BY user_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        requesters,
        vec![
            ("u1".to_owned(), "r-pending".to_owned(), 1_791_412_989_330),
            ("u2".to_owned(), "r-pending".to_owned(), 0),
        ],
        "a requester of a gone user or request is dropped; an unreadable time is 0"
    );
    let dismissals: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT user_id, musicbrainz_id_lower, dismissed_at FROM request_history_dismissals",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(dismissals.len(), 1, "{dismissals:?}");
    assert_eq!(
        (dismissals[0].0.as_str(), dismissals[0].1.as_str()),
        ("u1", "r-imported")
    );
    // The old default wrote datetime('now'), whole seconds.
    assert!(
        dismissals[0].2 > before_ms - 2_000 && dismissals[0].2 <= after_ms,
        "{dismissals:?}"
    );

    let watches: Vec<(String, String, i64, Option<i64>, i64)> = sqlx::query_as(
        "SELECT release_group_mbid_lower, state, created_at, last_checked_at, next_check_at \
         FROM wanted_watches ORDER BY release_group_mbid_lower",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        watches.len(),
        4,
        "the watch of a gone user is dropped: {watches:?}"
    );
    let dormant = &watches[0];
    assert_eq!(
        (dormant.0.as_str(), dormant.1.as_str(), dormant.2, dormant.3),
        (
            "w-dormant",
            "watching",
            1_600_000_000_125,
            Some(1_600_000_001_000)
        )
    );
    assert!(
        dormant.4 >= before_ms && dormant.4 <= after_ms,
        "the dormant watch is due now: {dormant:?}"
    );
    let watch = |key: &str, state: &str, created: i64, checked: Option<i64>, next: i64| {
        (key.to_owned(), state.to_owned(), created, checked, next)
    };
    assert_eq!(
        watches[1..].to_vec(),
        vec![
            watch(
                "w-fulfilled",
                "fulfilled",
                1_700_000_000_000,
                Some(1_700_000_100_750),
                1_700_000_000_000
            ),
            watch(
                "w-stopped",
                "stopped",
                1_700_000_000_000,
                None,
                1_700_000_000_000
            ),
            watch(
                "w-watching",
                "watching",
                1_700_000_000_500,
                None,
                1_800_000_000_250
            ),
        ]
    );

    type ApprovalRow = (String, String, i64, Option<String>, Option<i64>);
    let approvals: Vec<ApprovalRow> = sqlx::query_as(
        "SELECT artist_mbid_lower, state, requested_at, reviewed_by_id, reviewed_at \
         FROM auto_download_approvals ORDER BY artist_mbid_lower",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let approval = |key: &str, state: &str, asked: i64, by: Option<&str>, at: Option<i64>| {
        (
            key.to_owned(),
            state.to_owned(),
            asked,
            by.map(str::to_owned),
            at,
        )
    };
    assert_eq!(
        approvals,
        vec![
            approval("a1", "pending", 1_700_000_000_125, None, None),
            approval(
                "a2",
                "approved",
                1_700_000_000_000,
                u2,
                Some(1_700_000_010_500)
            ),
            approval(
                "a3",
                "rejected",
                1_700_000_000_000,
                None,
                Some(1_700_000_020_000)
            ),
            approval(
                "a4",
                "revoked",
                1_700_000_000_000,
                u2,
                Some(1_700_000_030_000)
            ),
        ]
    );

    let tables = [
        "request_history",
        "request_history_requesters",
        "request_history_dismissals",
        "wanted_watches",
        "auto_download_approvals",
    ];
    let mut keys: Vec<(String, String, String, String)> = Vec::new();
    for table in tables {
        let strict: i64 = sqlx::query_scalar("SELECT strict FROM pragma_table_list WHERE name = ?")
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(strict, 1, "{table}");
        let triggers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(triggers, 0, "{table}");
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT \"from\", \"table\", on_delete FROM pragma_foreign_key_list(?) \
             ORDER BY \"from\"",
        )
        .bind(table)
        .fetch_all(&pool)
        .await
        .unwrap();
        keys.extend(
            rows.into_iter()
                .map(|(from, parent, action)| (table.to_owned(), from, parent, action)),
        );
    }
    let key = |table: &str, from: &str, parent: &str, action: &str| {
        (
            table.to_owned(),
            from.to_owned(),
            parent.to_owned(),
            action.to_owned(),
        )
    };
    assert_eq!(
        keys,
        vec![
            key(
                "request_history",
                "reviewed_by_id",
                "auth_users",
                "SET NULL"
            ),
            key("request_history", "user_id", "auth_users", "SET NULL"),
            key(
                "request_history_requesters",
                "musicbrainz_id_lower",
                "request_history",
                "CASCADE"
            ),
            key(
                "request_history_requesters",
                "user_id",
                "auth_users",
                "CASCADE"
            ),
            key(
                "request_history_dismissals",
                "musicbrainz_id_lower",
                "request_history",
                "CASCADE"
            ),
            key(
                "request_history_dismissals",
                "user_id",
                "auth_users",
                "CASCADE"
            ),
            key("wanted_watches", "user_id", "auth_users", "CASCADE"),
            key(
                "auto_download_approvals",
                "reviewed_by_id",
                "auth_users",
                "SET NULL"
            ),
            key(
                "auto_download_approvals",
                "user_id",
                "auth_users",
                "CASCADE"
            ),
        ]
    );
    let broken: Vec<(String,)> = sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(broken.is_empty(), "{broken:?}");

    for refused in [
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, requested_at, status) VALUES ('x', 'X', 'a', 'b', 1, 'ignored')",
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, requested_at, status, origin) \
         VALUES ('x', 'X', 'a', 'b', 1, 'pending', 'robot')",
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, requested_at, status) VALUES ('x', 'X', 'a', 'b', '2024-01-01', 'pending')",
        "UPDATE wanted_watches SET state = 'dormant'",
        "UPDATE auto_download_approvals SET state = 'archived'",
    ] {
        assert!(
            sqlx::query(refused).execute(&pool).await.is_err(),
            "{refused}"
        );
    }
}

/// One `request_history` row as the 0076 upgrade test reads it.
type RequestRow = (
    String,
    String,
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<i64>,
    String,
    Option<i64>,
    Option<i64>,
);

/// An expected request row: user origin, no reviewer, year or length.
fn request(
    key: &str,
    status: &str,
    requested: i64,
    completed: Option<i64>,
    user: Option<&str>,
) -> RequestRow {
    (
        key.to_owned(),
        status.to_owned(),
        requested,
        completed,
        user.map(str::to_owned),
        None,
        None,
        "user".to_owned(),
        None,
        None,
    )
}

/// Adjustments to an expected request row.
trait ExpectedRequest {
    fn reviewed(self, by: Option<&str>, at: i64) -> Self;
    fn year(self, year: i64) -> Self;
    fn duration(self, seconds: i64) -> Self;
}

impl ExpectedRequest for RequestRow {
    fn reviewed(mut self, by: Option<&str>, at: i64) -> Self {
        self.5 = by.map(str::to_owned);
        self.6 = Some(at);
        self
    }

    fn year(mut self, year: i64) -> Self {
        self.8 = Some(year);
        self
    }

    fn duration(mut self, seconds: i64) -> Self {
        self.9 = Some(seconds);
        self
    }
}

/// The request status, watch state, approval state and request origin the
/// database accepts are exactly the ones the code names, and each enum's
/// text reads back to itself.
#[tokio::test]
async fn request_state_checks_list_exactly_the_code_values() {
    use droppedneedle_acquire::dispatch::DispatchOrigin;
    use droppedneedle_acquire::requests::ledger::{ApprovalState, RequestStatus, WatchState};

    let pool = migrated_pool().await;
    let mut schemas = BTreeMap::new();
    for table in [
        "request_history",
        "wanted_watches",
        "auto_download_approvals",
        "personal_mix_approvals",
    ] {
        let sql: String = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = ?")
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
        schemas.insert(table, sql);
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
        listed("request_history", "status"),
        RequestStatus::ALL.map(|status| status.as_str().to_owned())
    );
    assert_eq!(
        listed("request_history", "origin"),
        DispatchOrigin::ALL.map(|origin| origin.as_str().to_owned())
    );
    assert_eq!(
        listed("wanted_watches", "state"),
        WatchState::ALL.map(|state| state.as_str().to_owned())
    );
    assert_eq!(
        listed("auto_download_approvals", "state"),
        ApprovalState::ALL.map(|state| state.as_str().to_owned())
    );
    assert_eq!(
        listed("personal_mix_approvals", "state"),
        ApprovalState::ALL.map(|state| state.as_str().to_owned())
    );
    for status in RequestStatus::ALL {
        assert_eq!(RequestStatus::parse(status.as_str()), Some(status));
    }
    for state in WatchState::ALL {
        assert_eq!(WatchState::parse(state.as_str()), Some(state));
    }
    for state in ApprovalState::ALL {
        assert_eq!(ApprovalState::parse(state.as_str()), Some(state));
    }
    for origin in DispatchOrigin::ALL {
        assert_eq!(DispatchOrigin::parse(origin.as_str()), Some(origin));
    }
    assert_eq!(WatchState::parse("dormant"), None);
}

/// Migration 0083 types the acquire reasons. A task's outcome code comes
/// from the start of the worker's text, else from its newest decision
/// (whose retired codes read as held by an earlier version); text that
/// names nothing leaves it empty. The merged drop spellings are rewritten,
/// and the stored sentence and action columns go.
#[tokio::test]
async fn acquire_reasons_are_typed_on_upgrade() {
    let pool = scratch_pool().await;
    migrator_through(82).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, \
             album_title, status, error_message, created_at, updated_at) VALUES \
         ('t-stalled', 'u1', 'rg', 'A', 'B', 'queued', 'transfer stalled', 1, 1), \
         ('t-track', 'u1', 'rg', 'A', 'B', 'failed', \
             'track_not_found: Nobody shares this track.', 1, 1), \
         ('t-moved', 'u1', 'rg', 'A', 'B', 'queued', \
             'moved to the next source at your request', 1, 1), \
         ('t-held', 'u1', 'rg', 'A', 'B', 'failed', 'Held for review: no match', 1, 1), \
         ('t-retired', 'u1', 'rg', 'A', 'B', 'failed', 'Held for review: better', 1, 1), \
         ('t-held-bare', 'u1', 'rg', 'A', 'B', 'failed', 'Held for review: gone', 1, 1), \
         ('t-sentence', 'u1', 'rg', 'A', 'B', 'failed', \
             'Something on this server stopped the import.', 1, 1), \
         ('t-imported', 'u1', 'rg', 'A', 'B', 'partial', 'sqlite busy', 1, 1), \
         ('t-odd', 'u1', 'rg', 'A', 'B', 'failed', 'sqlite busy', 1, 1), \
         ('t-quiet', 'u1', 'rg', 'A', 'B', 'completed', NULL, 1, 1); \
         INSERT INTO download_import_decisions (task_id, outcome, reason_code, reason_text, \
             reason_action, decided_at) VALUES \
         ('t-held', 'rejected', 'wrong_album', 's', 'a', 1), \
         ('t-held', 'held', 'weak_match', 's', 'a', 2), \
         ('t-retired', 'held', 'upgrade_pending', 's', 'a', 1), \
         ('t-sentence', 'rejected', 'local_fault', 's', 'a', 1), \
         ('t-imported', 'imported', NULL, NULL, NULL, 1); \
         INSERT INTO drop_import_items (job_id, folder_name, status, reason_code, \
             reason_action, updated_at) VALUES \
         ('j', 'left', 'needs_review', 'leftover_files', 'a', 1), \
         ('j', 'broken', 'failed', 'unreadable', 'a', 1), \
         ('j', 'unknown', 'needs_review', 'no_album_match', 'a', 1), \
         ('j', 'done', 'imported', NULL, NULL, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();

    migrator_through(83).run(&pool).await.unwrap();

    let outcomes: BTreeMap<String, Option<String>> = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT id, outcome_code FROM download_tasks",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .collect();
    let expected: BTreeMap<String, Option<String>> = [
        ("t-stalled", Some("stalled")),
        ("t-track", Some("track_not_found")),
        ("t-moved", Some("moved_on")),
        ("t-held", Some("weak_match")),
        ("t-retired", Some("held_by_earlier_version")),
        ("t-held-bare", Some("held_for_review")),
        ("t-sentence", Some("local_fault")),
        ("t-imported", None),
        ("t-odd", None),
        ("t-quiet", None),
    ]
    .into_iter()
    .map(|(id, code)| (id.to_owned(), code.map(str::to_owned)))
    .collect();
    assert_eq!(outcomes, expected);
    let decisions: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT reason_code FROM download_import_decisions WHERE task_id = 't-retired'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(decisions, vec![Some("held_by_earlier_version".to_owned())]);
    let drops: BTreeMap<String, Option<String>> = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT folder_name, reason_code FROM drop_import_items",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .collect();
    let expected: BTreeMap<String, Option<String>> = [
        ("left", Some("tag_mismatch")),
        ("broken", Some("corrupt")),
        ("unknown", Some("no_album_match")),
        ("done", None),
    ]
    .into_iter()
    .map(|(name, code)| (name.to_owned(), code.map(str::to_owned)))
    .collect();
    assert_eq!(drops, expected);
    let gone = |columns: Vec<String>| {
        !columns
            .iter()
            .any(|column| column == "reason_text" || column == "reason_action")
    };
    assert!(gone(column_names(&pool, "drop_import_items").await));
    assert!(gone(column_names(&pool, "download_import_decisions").await));
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(version >= 83, "user_version {version}");
}

/// Migration 0084 puts a download's plan on its task row. A task queued
/// before it reads the empty plan of a plain download and stays queued
/// for the worker to dispatch.
#[tokio::test]
async fn queued_downloads_take_an_empty_plan_on_upgrade() {
    use droppedneedle_acquire::downloads::TaskPlan;

    let pool = scratch_pool().await;
    migrator_through(83).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'U', 'user', 'now'); \
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, \
             album_title, status, created_at, updated_at) \
         VALUES ('t-queued', 'u1', 'rg', 'A', 'B', 'queued', 1, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let (status, plan): (String, String) =
        sqlx::query_as("SELECT status, plan_json FROM download_tasks WHERE id = 't-queued'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "queued");
    assert_eq!(TaskPlan::decode(&plan).unwrap(), TaskPlan::default());
    assert!(
        TaskPlan::decode(r#"{"hold_on_wrong_track": true}"#).is_err(),
        "a field the plan does not have is refused"
    );
}

/// Migration 0085 moves every file waiting for a decision into one list: a
/// held download and an upload waiting for review become two entries with
/// their files, times in milliseconds, paths below their folders, a stored
/// reason this release does not hold files for read as held by an earlier
/// version, a staged path outside its job's folder left out, an item with
/// no file left settled, and nothing carried expires sooner than 90 days
/// after the upgrade.
/// Decided held rows go with their table.
#[tokio::test]
async fn held_files_and_waiting_uploads_become_one_list() {
    let pool = scratch_pool().await;
    migrator_through(84).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'U', 'user', 'now'); \
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, \
             album_title, status, created_at, updated_at) \
         VALUES ('t1', 'u1', 'RG-1', 'A', 'B', 'failed', 1, 1); \
         INSERT INTO held_imports (user_id, release_group_mbid, held_path, original_filename, \
             reason, source_task_id, status, created_at) VALUES \
         ('u1', 'RG-1', '/data/cache/held/aa_01.flac', 'CD1/01.flac', 'weak_match', 't1', \
             'held', 1700000000.5), \
         ('u1', 'RG-1', '/data/cache/held/bb_02.flac', '02.flac', 'wrong_track', 't1', \
             'held', 1700000001.0), \
         ('u1', 'RG-1', '/data/cache/held/cc_03.flac', '03.flac', 'weak_match', 't1', \
             'imported', 1600000000.0); \
         INSERT INTO drop_import_jobs (id, user_id, user_name, status, created_at, \
             upload_name, staging_dir) \
         VALUES ('job1', 'u1', 'U', 'completed', 1700000000.0, 'x.zip', \
             '/data/imports/drop/job1'); \
         INSERT INTO drop_import_items (id, job_id, folder_name, status, staging_paths, \
             updated_at, reason_code) \
         VALUES (5, 'job1', 'Odds', 'needs_review', \
             '[\"/data/imports/drop/job1/Odds/01.flac\",\"/data/imports/drop/job1/02.flac\",\
             \"/elsewhere/03.flac\"]', \
             1700000000.0, 'no_album_match'), \
         (6, 'job1', 'Done', 'imported', '[]', 1700000000.0, NULL), \
         (7, 'job1', 'Empty', 'needs_review', '[]', 1700000000.0, 'no_album_match');",
    )
    .execute(&pool)
    .await
    .unwrap();

    let upgraded_at = now_ms();
    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let entries: Vec<(String, String, Option<i64>, String, i64, i64)> = sqlx::query_as(
        "SELECT origin, group_key, drop_item_id, state, created_at, expires_at \
         FROM acquire_held_entries ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(entries.len(), 2, "{entries:?}");
    let ninety_days = 90 * 86_400_000;
    for (origin, key, item, state, created_at, expires_at) in &entries {
        assert_eq!(state, "held");
        assert!(
            *expires_at >= upgraded_at + ninety_days,
            "{key} expires {expires_at}, before 90 days after the upgrade"
        );
        match origin.as_str() {
            "download" => {
                assert_eq!((key.as_str(), *item), ("album:rg-1", None));
                assert_eq!(*created_at, 1_700_000_000_500);
            }
            _ => {
                assert_eq!((key.as_str(), *item), ("upload:5", Some(5)));
                assert_eq!(*created_at, 1_700_000_000_000);
            }
        }
    }
    let files: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT root, path, original_name, reason, created_at FROM acquire_held_files \
         ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        files,
        vec![
            (
                "held".to_owned(),
                "aa_01.flac".to_owned(),
                "CD1/01.flac".to_owned(),
                "weak_match".to_owned(),
                1_700_000_000_500
            ),
            (
                "held".to_owned(),
                "bb_02.flac".to_owned(),
                "02.flac".to_owned(),
                "held_by_earlier_version".to_owned(),
                1_700_000_001_000
            ),
            (
                "drop".to_owned(),
                "job1/Odds/01.flac".to_owned(),
                "Odds/01.flac".to_owned(),
                "no_album_match".to_owned(),
                1_700_000_000_000
            ),
            (
                "drop".to_owned(),
                "job1/02.flac".to_owned(),
                "02.flac".to_owned(),
                "no_album_match".to_owned(),
                1_700_000_000_000
            ),
        ]
    );
    let old: Option<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE name = 'held_imports'")
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert_eq!(old, None, "held_imports is gone");
    let empty: String = sqlx::query_scalar("SELECT status FROM drop_import_items WHERE id = 7")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        empty, "discarded",
        "an item with no file to show is settled"
    );
    for table in ["acquire_held_entries", "acquire_held_files"] {
        let sql: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(table)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(sql.trim_end().ends_with("STRICT"), "{sql}");
    }
}

/// Migration 0086 fences scans with the publish journal. Every journal
/// write that names a track (a removal names it through its replacement)
/// moves the library revision and records it on the row; a plugin's added
/// file moves nothing. A scope walked before the upgrade has no walk
/// revision, so its run marks nothing missing.
#[tokio::test]
async fn journal_writes_stamp_the_scan_fence_on_upgrade() {
    use droppedneedle_kernel::clock::SystemClock;
    use droppedneedle_library::scan::{RunStop, SqliteScanStore};

    let dir = crate::common::ScratchDir::new("scan-fence");
    let db_path = dir.join("fence.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    migrator_through(85).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES ('album', 'music', \
         'k', 'Album', 'album', '00000000-0000-4000-8000-000000000002', 'automatic', 0, 0); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, ingest_source, imported_at, \
         membership_source) VALUES ('gone', 'album', 'music', 'a/01.flac', 'a/01.flac', \
         'h', 0, 0, '0:0', 'T', 't', 'Album', 'album', 'flac', 'scan', 0, 'automatic'); \
         INSERT INTO library_scan_runs (id, kind, trigger, state, phase, aggregate_scope, \
         queued_at, started_at, updated_at) VALUES ('run', 'incremental', 'manual', \
         'completed', 'reconciling', 'music', 1, 1, 1); \
         INSERT INTO library_scan_run_scopes (run_id, scope_sequence, root_id, \
         relative_path, effective_policy, policy_revision, discovery_state) VALUES \
         ('run', 0, 'music', '.', 'automatic', 'p', 'completed');",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let walk: Option<i64> = sqlx::query_scalar("SELECT walk_revision FROM library_scan_run_scopes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        walk, None,
        "a scope walked before the upgrade has no walk revision"
    );
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime opens");
    let store = SqliteScanStore::new(runtime.db().clone(), std::sync::Arc::new(SystemClock));
    let missing = || {
        store
            .missing_catalog_paths(&RunStop::never(), "run", "music", ".")
            .expect("missing paths read")
    };
    assert!(missing().is_empty(), "the old walk marks nothing");

    let journal = |id: &str, kind: &str, track: Option<&str>| {
        format!(
            "INSERT INTO library_publish_journal (id, bundle_id, kind, dest_root, dest_rel, \
             staged, staged_sha256, track_id, state) VALUES ('{id}', 'b', '{kind}', 'music', \
             '{id}.flac', 's', 'h', {}, 'prepared');",
            track.map_or("NULL".to_owned(), |track| format!("'{track}'"))
        )
    };
    let stamps = |pool: SqlitePool| async move {
        let rows: Vec<(String, Option<i64>)> =
            sqlx::query_as("SELECT id, fence_revision FROM library_publish_journal ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        let counter: i64 = sqlx::query_scalar("SELECT revision FROM library_scan_fences")
            .fetch_one(&pool)
            .await
            .unwrap();
        (rows, counter)
    };
    sqlx::raw_sql(&format!(
        "{}{}{}",
        journal("audio", "audio", Some("t")),
        journal("plugin", "add", None),
        journal("remove", "remove", None)
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        stamps(pool.clone()).await,
        (
            vec![
                ("audio".to_owned(), Some(1)),
                ("plugin".to_owned(), None),
                ("remove".to_owned(), Some(2)),
            ],
            2
        )
    );
    sqlx::raw_sql(
        "UPDATE library_publish_journal SET state = 'staged' WHERE id = 'audio'; \
         UPDATE library_publish_journal SET state = 'prepared' WHERE id = 'remove'; \
         UPDATE library_publish_journal SET state = 'staged' WHERE id = 'plugin';",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        stamps(pool.clone()).await,
        (
            vec![
                ("audio".to_owned(), Some(3)),
                ("plugin".to_owned(), None),
                ("remove".to_owned(), Some(2)),
            ],
            3
        ),
        "a state change stamps; a write that keeps the state does not"
    );

    sqlx::raw_sql("UPDATE library_scan_run_scopes SET walk_revision = 3")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        missing(),
        vec![("a/01.flac".to_owned(), false)],
        "a scope with a walk revision judges the track it did not see"
    );
    runtime.shutdown().await;
}

/// Migration 0087 gives held entries an automatic retry: an open entry
/// carried over has no tries and no next try, so the first pass retries
/// it, and a change of its next try moves the download activity revision.
/// The held files table is rebuilt to take a new reason: a held file comes
/// through with its reason, pairing and evidence, and its triggers still
/// move the revision.
#[tokio::test]
async fn held_entries_gain_an_automatic_retry_on_upgrade() {
    let pool = scratch_pool().await;
    migrator_through(86).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'U', 'user', 'now'); \
         INSERT INTO acquire_held_entries (id, user_id, origin, group_key, state, \
             created_at, expires_at) \
         VALUES (1, 'u1', 'download', 'album:rg', 'held', 1, 2); \
         INSERT INTO acquire_held_files (id, entry_id, root, path, original_name, \
             release_mbid, release_track_mbid, recording_mbid, disc_number, track_number, \
             track_title, file_format, duration_seconds, expected_duration_seconds, \
             evidence_title, evidence_artist, evidence_score, source, source_task_id, \
             reason, reason_detail, state, created_at) \
         VALUES (7, 1, 'held', 'aa_01.flac', 'CD1/01.flac', 'rel', 'rt', 'rec', 1, 1, \
             'Mysterons', 'flac', 301.5, 302.0, 'Heard', 'Someone', 0.82, 'soulseek', 't1', \
             'fingerprint_mismatch', 'heard another', 'held', 5);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let retry: (i64, Option<i64>) =
        sqlx::query_as("SELECT retry_tries, retry_at FROM acquire_held_entries WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retry, (0, None));
    let revision = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM download_activity_global_revision WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let before = revision().await;
    sqlx::raw_sql("UPDATE acquire_held_entries SET retry_at = 5 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(revision().await, before + 1, "the list shows the next try");

    type Kept = (
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
    );
    let kept: Kept = sqlx::query_as(
        "SELECT entry_id, path, state, reason, reason_detail, source_task_id, created_at \
         FROM acquire_held_files WHERE id = 7",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        kept,
        (
            1,
            "aa_01.flac".to_owned(),
            "held".to_owned(),
            "fingerprint_mismatch".to_owned(),
            Some("heard another".to_owned()),
            Some("t1".to_owned()),
            5,
        )
    );
    type Paired = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<f64>,
        Option<String>,
        Option<String>,
        Option<f64>,
    );
    let paired: Paired = sqlx::query_as(
        "SELECT release_mbid, release_track_mbid, recording_mbid, track_number, track_title, \
         duration_seconds, evidence_title, evidence_artist, evidence_score \
         FROM acquire_held_files WHERE id = 7",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        paired,
        (
            Some("rel".to_owned()),
            Some("rt".to_owned()),
            Some("rec".to_owned()),
            Some(1),
            Some("Mysterons".to_owned()),
            Some(301.5),
            Some("Heard".to_owned()),
            Some("Someone".to_owned()),
            Some(0.82),
        )
    );
    let before = revision().await;
    sqlx::raw_sql("UPDATE acquire_held_files SET reason = 'owned_elsewhere' WHERE id = 7")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        revision().await,
        before + 1,
        "the new reason is taken and the list moves"
    );
}

/// Migration 0089 indexes tracks by the download task they came from, so the
/// storage quota reaches one user's tracks through that link. A track keeps
/// its link through the upgrade, and the index is in place afterwards.
#[tokio::test]
async fn tracks_gain_a_download_task_index_on_upgrade() {
    let pool = scratch_pool().await;
    migrator_through(88).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('ar1', 'A', 'a', 'person', 0, 0); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, album_artist_name, grouping_source, created_at, updated_at) \
         VALUES ('al1', 'r1', 'al1', 'T', 't', 'ar1', 'A', 'automatic', 0, 0); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, bit_rate, ingest_source, \
             imported_at, membership_source, availability, download_task_id) \
         VALUES ('t1', 'al1', 'r1', '/m/1.mp3', '1.mp3', 'p1', 1234, 0, 's', 'T', 't', 'T', \
             't', 'mp3', 320, 'scan', 0, 'automatic', 'indexed', 'task-1');",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let index: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'index' AND name = 'idx_local_tracks_download_task'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(index.as_deref(), Some("idx_local_tracks_download_task"));

    let kept: (String, Option<String>, i64) = sqlx::query_as(
        "SELECT id, download_task_id, file_size_bytes FROM local_tracks WHERE id = 't1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kept, ("t1".to_owned(), Some("task-1".to_owned()), 1234));
}

/// Migration 0091 adds the stored Jellyfin ids. On a catalog that already
/// holds an album, a track and a merged-away album (beside the artists the
/// migrations seed), the table arrives
/// empty, STRICT and with its partial unique index, and one fill pass then
/// stores each item's current id and the aliases that still resolve.
#[tokio::test]
async fn jellyfin_ids_arrive_empty_and_fill_on_upgrade() {
    use droppedneedle_compat::adapters::jellyfin_ids::JellyfinIds;

    let dir = crate::common::ScratchDir::new("jellyfin-ids-upgrade");
    let path = dir.join("app.db");
    let pool = &SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    migrator_through(90).run(pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('ar1', 'A', 'a', 'person', 0, 0); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, album_artist_name, grouping_source, created_at, updated_at) \
         VALUES ('al1', 'r1', 'al1', 'T', 't', 'ar1', 'A', 'automatic', 0, 0); \
         INSERT INTO local_album_aliases (alias, local_album_id, kind, created_at) \
         VALUES ('al0', 'al1', 'merged_album', 0); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, ingest_source, imported_at, \
             membership_source, availability, genre) \
         VALUES ('t1', 'al1', 'r1', '/m/1.mp3', '1.mp3', 'p1', 1234, 0, 's', 'T', 't', 'T', \
             't', 'mp3', 'scan', 0, 'automatic', 'indexed', 'Trip Hop');",
    )
    .execute(pool)
    .await
    .unwrap();

    apply_migrations(pool).await.unwrap();
    assert_migrated(pool).await.unwrap();
    let (table, index): (String, String) = sqlx::query_as(
        "SELECT t.sql, i.sql FROM sqlite_master t, sqlite_master i \
         WHERE t.name = 'compat_jellyfin_ids' AND i.name = 'idx_compat_jellyfin_ids_current'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(table.contains("STRICT"), "{table}");
    assert!(
        index.contains("UNIQUE") && index.contains("WHERE current = 1"),
        "{index}"
    );
    let stored = || async {
        sqlx::query_as::<_, (String, String, String, i64)>(
            "SELECT jellyfin_id, kind, internal_id, current FROM compat_jellyfin_ids \
             ORDER BY kind, internal_id, current, jellyfin_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    };
    assert!(stored().await.is_empty(), "the table arrives empty");

    let runtime = open_runtime(&DbConfig::new(&path))
        .await
        .expect("runtime opens");
    let crypto = std::sync::Arc::new(
        droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7; 32]).unwrap(),
    );
    let ids = JellyfinIds::new(runtime.db().clone(), crypto, std::time::Duration::ZERO);
    ids.fill().pass().await.unwrap();
    let row = |id: String, kind: &str, internal: &str, current: i64| {
        (id, kind.to_owned(), internal.to_owned(), current)
    };
    let mut expected = vec![
        row(ids.computed("album", "al0"), "album", "al0", 0),
        row(JellyfinIds::derive("album", "al0"), "album", "al0", 0),
        row(JellyfinIds::derive("album", "al1"), "album", "al1", 0),
        row(ids.computed("album", "al1"), "album", "al1", 1),
        row(JellyfinIds::derive("artist", "ar1"), "artist", "ar1", 0),
        row(ids.computed("artist", "ar1"), "artist", "ar1", 1),
        row(
            JellyfinIds::derive("genre", "trip-hop"),
            "genre",
            "trip-hop",
            1,
        ),
        row(
            JellyfinIds::derive("library", "music"),
            "library",
            "music",
            1,
        ),
        row(JellyfinIds::derive("track", "t1"), "track", "t1", 1),
    ];
    let seeded: Vec<String> = sqlx::query_scalar("SELECT id FROM local_artists WHERE id <> 'ar1'")
        .fetch_all(pool)
        .await
        .unwrap();
    for artist in &seeded {
        expected.push(row(ids.computed("artist", artist), "artist", artist, 1));
        expected.push(row(
            JellyfinIds::derive("artist", artist),
            "artist",
            artist,
            0,
        ));
    }
    expected.sort_by(|a, b| (&a.1, &a.2, a.3, &a.0).cmp(&(&b.1, &b.2, b.3, &b.0)));
    assert_eq!(stored().await, expected);
    runtime.shutdown().await;
}

/// Wall time in milliseconds, for checks against "due now".
fn now_ms() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(elapsed.as_millis()).unwrap()
}

/// Migration 0005 lands its objects on fresh databases and backfills them
/// over catalogs migrated through 0004: the FTS oracle finds pre-existing
/// text, the format totals match live aggregation, and later writes stay
/// maintained through the triggers.
#[tokio::test]
async fn migration_0005_backfills_on_upgrade() {
    assert!(latest_version() >= 5);
    let pool = migrated_pool().await;
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

    let pool = scratch_pool().await;
    migrator_through(4).run(&pool).await.unwrap();
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
    let miss: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM local_tracks_fts WHERE local_tracks_fts MATCH ?)",
    )
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

/// Migration 0088 clears MusicBrainz's special-purpose artist ids from
/// stored track and album credits, keeps every real id, and empties the
/// artist link stamp of each album whose credits it changed, so the next
/// sweep links the rows it freed.
#[tokio::test]
async fn placeholder_artist_ids_are_cleared_on_upgrade() {
    const UNKNOWN: &str = "125ec42a-7229-4250-afc5-e057484327fe";
    const VARIOUS: &str = "89ad4ac3-39f7-470e-963a-56509c546377";
    const REAL: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    let pool = scratch_pool().await;
    migrator_through(87).run(&pool).await.unwrap();
    let album = |id: &str| {
        format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) VALUES ('{id}', 'music', \
             '{id}', 'Album', 'album', '00000000-0000-4000-8000-000000000002', 'automatic', 0, 0); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, ingest_source, imported_at, \
             membership_source) VALUES ('{id}-t', '{id}', 'music', '{id}/01.flac', \
             '{id}/01.flac', '{id}', 0, 0, '0:0', 'T', 't', 'Album', 'album', 'flac', 'scan', 0, \
             'automatic'); \
             INSERT INTO library_artist_reconciliation_state (local_album_id, input_revision, \
             state, reason_code, updated_at) VALUES ('{id}', 'stamp', 'projected', \
             'CREDITS_LINE_UP', 0);"
        )
    };
    sqlx::raw_sql(&format!(
        "{} {} {} \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('artist', 'Artist', 'artist', 'group', 0, 0); \
         INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
         artist_mbid) VALUES ('by-album', 0, 'artist', 'primary', '{UNKNOWN}'), \
         ('by-album', 1, 'artist', 'primary', '{REAL}'), \
         ('by-track', 0, 'artist', 'primary', '{REAL}'), \
         ('real', 0, 'artist', 'primary', '{REAL}'); \
         INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role, \
         artist_mbid) VALUES ('by-album-t', 0, 'artist', 'primary', '{REAL}'), \
         ('by-track-t', 0, 'artist', 'primary', '{VARIOUS}'), \
         ('real-t', 0, 'artist', 'primary', '{REAL}');",
        album("by-album"),
        album("by-track"),
        album("real")
    ))
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stamped >= 88 && stamped == latest_version(), "{stamped}");
    let credits: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT local_album_id || ':' || position, artist_mbid FROM local_album_artists \
         UNION ALL SELECT local_track_id || ':' || position, artist_mbid FROM local_track_artists \
         ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let real = Some(REAL.to_owned());
    assert_eq!(
        credits,
        vec![
            ("by-album-t:0".to_owned(), real.clone()),
            ("by-album:0".to_owned(), None),
            ("by-album:1".to_owned(), real.clone()),
            ("by-track-t:0".to_owned(), None),
            ("by-track:0".to_owned(), real.clone()),
            ("real-t:0".to_owned(), real.clone()),
            ("real:0".to_owned(), real),
        ]
    );
    let stamps: Vec<(String, String)> = sqlx::query_as(
        "SELECT local_album_id, input_revision FROM library_artist_reconciliation_state \
         ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        stamps,
        vec![
            ("by-album".to_owned(), String::new()),
            ("by-track".to_owned(), String::new()),
            ("real".to_owned(), "stamp".to_owned()),
        ]
    );
}

/// Migration 0090 rebuilds the held files table so its reason CHECK takes
/// `queue_gave_way`: a held file comes through with its reason, pairing
/// and evidence, the new code is accepted, and the triggers still move the
/// download activity revision. Download tasks gain `picked_by_person`:
/// a task carried over reads 0, and only 0 or 1 is taken.
#[tokio::test]
async fn queued_copies_can_give_way_on_upgrade() {
    let pool = scratch_pool().await;
    migrator_through(89).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'U', 'user', 'now'); \
         INSERT INTO acquire_held_entries (id, user_id, origin, group_key, state, \
             created_at, expires_at) \
         VALUES (1, 'u1', 'download', 'album:rg', 'held', 1, 2); \
         INSERT INTO acquire_held_files (id, entry_id, root, path, original_name, \
             release_mbid, release_track_mbid, recording_mbid, disc_number, track_number, \
             track_title, file_format, duration_seconds, expected_duration_seconds, \
             evidence_title, evidence_artist, evidence_score, source, source_task_id, \
             reason, reason_detail, state, created_at) \
         VALUES (7, 1, 'held', 'aa_01.flac', 'CD1/01.flac', 'rel', 'rt', 'rec', 1, 1, \
             'Mysterons', 'flac', 301.5, 302.0, 'Heard', 'Someone', 0.82, 'soulseek', 't1', \
             'owned_elsewhere', 'another copy', 'held', 5); \
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, \
             album_title, status, created_at, updated_at) \
         VALUES ('t-linked', 'u1', 'rg', 'A', 'B', 'downloading', 1, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let picked: (i64, String) =
        sqlx::query_as("SELECT picked_by_person, status FROM download_tasks WHERE id = 't-linked'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        picked,
        (0, "downloading".to_owned()),
        "a carried-over task reads 0"
    );
    sqlx::raw_sql("UPDATE download_tasks SET picked_by_person = 1 WHERE id = 't-linked'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::raw_sql("UPDATE download_tasks SET picked_by_person = 2 WHERE id = 't-linked'")
            .execute(&pool)
            .await
            .is_err(),
        "only 0 or 1 is taken"
    );

    type Kept = (
        i64,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<f64>,
        i64,
    );
    let kept: Kept = sqlx::query_as(
        "SELECT entry_id, path, original_name, state, reason, reason_detail, \
         release_track_mbid, evidence_score, created_at FROM acquire_held_files WHERE id = 7",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        kept,
        (
            1,
            "aa_01.flac".to_owned(),
            "CD1/01.flac".to_owned(),
            "held".to_owned(),
            "owned_elsewhere".to_owned(),
            Some("another copy".to_owned()),
            Some("rt".to_owned()),
            Some(0.82),
            5,
        )
    );
    let revision = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM download_activity_global_revision WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let before = revision().await;
    sqlx::raw_sql("UPDATE acquire_held_files SET reason = 'queue_gave_way' WHERE id = 7")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        revision().await,
        before + 1,
        "the new reason is taken and the list moves"
    );
}

/// Migration 0092 gives repair snapshots a `purpose` column filled from
/// `scope_json`: a named purpose is kept, a scope naming none is an
/// identity repair (v2's rule), and a scope that does not decode or names
/// no known purpose is left without one. Only the four purposes are
/// taken. The cleanup watermark is seeded at the newest finished
/// identification job, or the start of time when there is none.
#[tokio::test]
async fn repair_purpose_becomes_a_column_on_upgrade() {
    let pool = scratch_pool().await;
    migrator_through(91).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO library_repair_snapshots (job_id, scope_json, target_matcher_version, \
             created_at) VALUES \
             ('named', '{\"root_ids\":[],\"purpose\":\"management_readiness\"}', 'v', 1), \
             ('none', '{\"root_ids\":[\"r\"]}', 'v', 1), \
             ('null', '{\"purpose\":null}', 'v', 1), \
             ('broken', 'not json', 'v', 1), \
             ('unknown', '{\"purpose\":\"artist_identity_reconciliation\"}', 'v', 1), \
             ('list', '[]', 'v', 1); \
         INSERT INTO library_identify_jobs (id, local_album_id, kind, priority, state, \
             input_revision, idempotency_key, created_ms, updated_ms) VALUES \
             ('a', 'al', 'automatic', 0, 'succeeded', 'r', 'ka', 1, 5), \
             ('b', 'al', 'automatic', 0, 'attention', 'r', 'kb', 1, 9), \
             ('c', 'al', 'automatic', 0, 'queued', 'r', 'kc', 1, 20);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let purposes: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT job_id, purpose FROM library_repair_snapshots ORDER BY job_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let purpose = |purpose: Option<&str>| purpose.map(str::to_owned);
    assert_eq!(
        purposes,
        vec![
            ("broken".to_owned(), None),
            ("list".to_owned(), None),
            ("named".to_owned(), purpose(Some("management_readiness"))),
            ("none".to_owned(), purpose(Some("existing_matches"))),
            ("null".to_owned(), purpose(Some("existing_matches"))),
            ("unknown".to_owned(), None),
        ]
    );
    assert!(
        sqlx::raw_sql(
            "UPDATE library_repair_snapshots SET purpose = 'bogus' WHERE job_id = 'none'"
        )
        .execute(&pool)
        .await
        .is_err(),
        "only the four purposes are taken"
    );

    let mark: (i64, String) =
        sqlx::query_as("SELECT identified_at, identified_job_id FROM library_repair_watermarks")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(mark, (9, "b".to_owned()), "the newest finished job");

    let empty = scratch_pool().await;
    apply_migrations(&empty).await.unwrap();
    let mark: (i64, String) =
        sqlx::query_as("SELECT identified_at, identified_job_id FROM library_repair_watermarks")
            .fetch_one(&empty)
            .await
            .unwrap();
    assert_eq!(mark, (0, String::new()), "no job: the start of time");
}

/// Migration 0093 indexes the lowered release group and recording ids of
/// tracks, so the library's tracks of a group or a recording are reached
/// by key. A track keeps its ids, in the case they were written in, and
/// the three indexes are in place afterwards.
#[tokio::test]
async fn lowered_mbid_indexes_arrive_on_upgrade() {
    let pool = scratch_pool().await;
    migrator_through(92).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('ar1', 'A', 'a', 'person', 0, 0); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, album_artist_name, grouping_source, created_at, updated_at) \
         VALUES ('al1', 'r1', 'al1', 'T', 't', 'ar1', 'A', 'automatic', 0, 0); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, bit_rate, ingest_source, \
             imported_at, membership_source, availability, embedded_release_group_mbid, \
             embedded_recording_mbid) \
         VALUES ('t1', 'al1', 'r1', '/m/1.mp3', '1.mp3', 'p1', 1, 0, 's', 'T', 't', 'T', \
             't', 'mp3', 320, 'scan', 0, 'automatic', 'indexed', 'GROUP-A', 'REC-A'); \
         INSERT INTO local_track_external_identities (local_track_id, recording_mbid, \
             decision_source, selected_at) VALUES ('t1', 'REC-B', 'embedded', 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name IN \
             ('idx_local_tracks_release_group_lower', 'idx_local_tracks_recording_lower', \
              'idx_local_track_identity_recording_lower') ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        indexes,
        [
            "idx_local_track_identity_recording_lower",
            "idx_local_tracks_recording_lower",
            "idx_local_tracks_release_group_lower",
        ]
    );

    let kept: (String, String, String) = sqlx::query_as(
        "SELECT t.embedded_release_group_mbid, t.embedded_recording_mbid, ti.recording_mbid \
         FROM local_tracks t JOIN local_track_external_identities ti \
           ON ti.local_track_id = t.id WHERE t.id = 't1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        kept,
        ("GROUP-A".to_owned(), "REC-A".to_owned(), "REC-B".to_owned())
    );
}

/// Migration 0094 gives every track a search key equal to its rowid, so
/// orders read from it stay as they were, and rebuilds the search index on
/// that key: a catalog seeded before it, with gaps where tracks were
/// deleted, finds the same tracks after. A track added later takes the
/// next key and is found by its text.
#[tokio::test]
async fn tracks_gain_a_search_key_on_upgrade() {
    let pool = scratch_pool().await;
    migrator_through(93).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('ar1', 'A', 'a', 'person', 0, 0); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, album_artist_name, grouping_source, created_at, updated_at) \
         VALUES ('al1', 'r1', 'al1', 'Blue Hour', 'blue hour', 'ar1', 'A', 'automatic', 0, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (step, title) in ["amber road", "cobalt sea", "amber sky", "dune", "amber"]
        .iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
                 path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
                 album_title, album_title_folded, file_format, ingest_source, imported_at, \
                 membership_source) \
             VALUES (?1, 'al1', 'r1', ?1, ?1, ?1, 1, 0, 's', ?2, ?2, 'Blue Hour', 'blue hour', \
                 'mp3', 'scan', 0, 'automatic')",
        )
        .bind(format!("t{step}"))
        .bind(title)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("DELETE FROM local_tracks WHERE id = 't1'")
        .execute(&pool)
        .await
        .unwrap();
    let rowids: Vec<(String, i64)> =
        sqlx::query_as("SELECT id, rowid FROM local_tracks ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let keys: Vec<(String, i64)> =
        sqlx::query_as("SELECT id, search_key FROM local_tracks ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(keys, rowids, "the key is the rowid each track had");
    let hits = |phrase: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT id FROM local_tracks WHERE search_key IN \
                 (SELECT rowid FROM local_tracks_fts WHERE local_tracks_fts MATCH ?) ORDER BY id",
            )
            .bind(phrase)
            .fetch_all(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(hits("\"amber\"").await, ["t0", "t2", "t4"]);
    assert_eq!(hits("\"blue hour\"").await, ["t0", "t2", "t3", "t4"]);
    assert!(hits("\"cobalt\"").await.is_empty());

    sqlx::query(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, ingest_source, imported_at, \
             membership_source) \
         VALUES ('t5', 'al1', 'r1', 't5', 't5', 't5', 1, 0, 's', 'Amber Late', 'amber late', \
             'Blue Hour', 'blue hour', 'mp3', 'scan', 0, 'automatic')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let next: i64 = sqlx::query_scalar("SELECT search_key FROM local_tracks WHERE id = 't5'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(next, 6, "one more than the largest key");
    assert_eq!(hits("\"amber\"").await, ["t0", "t2", "t4", "t5"]);
    sqlx::query("UPDATE local_tracks SET title_folded = 'ember' WHERE id = 't0'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(hits("\"amber\"").await, ["t2", "t4", "t5"]);
    sqlx::query(
        "INSERT INTO local_tracks_fts(local_tracks_fts, rank) VALUES ('integrity-check', 1)",
    )
    .execute(&pool)
    .await
    .expect("the index matches the table");
}

/// Migration 0095 rebuilds discover's tables to the conventions. Each
/// user's hidden releases carry over with their time in milliseconds and
/// go with their user; a hidden release of a user who no longer exists
/// does not. Remembered release-group answers carry over. Saved pages and
/// activity are caches and start empty.
#[tokio::test]
async fn discover_tables_rebuild_and_hidden_releases_carry() {
    let pool = scratch_pool().await;
    migrator_through(94).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'T', 'user', 'now'); \
         INSERT INTO ignored_releases (user_id, release_group_mbid_lower, release_group_mbid, \
         artist_mbid, release_name, artist_name, ignored_at) VALUES \
         ('u1', 'rg-1', 'RG-1', 'a-1', 'Hidden', 'Someone', 1700000800.25), \
         ('gone', 'rg-2', 'RG-2', 'a-2', 'Orphan', 'Nobody', 1700000900.0); \
         INSERT INTO mbid_resolution_map (source_mbid_lower, source_mbid, release_group_mbid) \
         VALUES ('rel-1', 'REL-1', 'rg-1'), ('rel-2', 'REL-2', NULL); \
         INSERT INTO discovery_snapshots (snapshot_key, user_id, payload, saved_at) \
         VALUES ('discover_response:u1:True:False', 'u1', x'7b7d', 1700000000.5); \
         INSERT INTO discovery_activity (user_id, feature, source, last_used) \
         VALUES ('u1', 'home', '[\"official\",\"mb\",1]', 1700000000.5);",
    )
    .execute(&pool)
    .await
    .unwrap();

    migrator_through(95).run(&pool).await.unwrap();

    let ignores: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT user_id, release_group_mbid, release_name, ignored_at \
         FROM catalog_ignored_releases ORDER BY user_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        ignores,
        vec![(
            "u1".to_owned(),
            "RG-1".to_owned(),
            "Hidden".to_owned(),
            1_700_000_800_250
        )]
    );
    let resolutions: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT source_mbid_lower, release_group_mbid FROM catalog_mbid_resolutions \
         ORDER BY source_mbid_lower",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        resolutions,
        vec![
            ("rel-1".to_owned(), Some("rg-1".to_owned())),
            ("rel-2".to_owned(), None)
        ]
    );
    let tables = table_names(&pool).await;
    for gone in [
        "discovery_snapshots",
        "discovery_activity",
        "ignored_releases",
        "mbid_resolution_map",
    ] {
        assert!(!tables.contains(&gone.to_owned()), "{gone} must be gone");
    }
    for empty in ["catalog_cached_pages", "catalog_discover_activity"] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {empty}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "{empty}");
    }

    sqlx::query("DELETE FROM auth_users WHERE id = 'u1'")
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_ignored_releases")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "hidden releases go with their user");
}

/// Migration 0096 rebuilds the remaining acquire tables as STRICT with
/// millisecond times. A download mid-transfer, its attempt, a held upload
/// of a drop job, a follow cursor and a seen candidate carry with equal
/// meaning; rows whose parent is already gone drop; and the download then
/// completes from its converted row, its triggers still moving the
/// activity revision.
#[tokio::test]
async fn acquire_rows_carry_onto_strict_tables_on_upgrade() {
    use droppedneedle_acquire::downloads::state::{AttemptState, TaskStatus};
    use droppedneedle_acquire::downloads::store::DownloadStore;

    let dir = crate::common::ScratchDir::new("acquire-0096");
    let path = dir.join("app.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    migrator_through(95).run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'U', 'user', 'now'), ('admin', 'A', 'admin', 'now'); \
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, \
             album_title, status, year, quality_snapshot_hash, remote_queued, created_at, \
             started_at, last_polled_at, updated_at) \
         VALUES ('t1', 'u1', 'rg', 'A', 'B', 'downloading', 'junk', 'h', 1, \
             1700000000.123, 1700000000.5, 1700000002.25, 1700000002.25); \
         INSERT INTO download_attempts (id, task_id, source, candidate_index, handle_json, \
             state, next_retry_at, lease_expires_at, created_at, updated_at) \
         VALUES ('a1', 't1', 'soulseek', 0, '{}', 'in_use', 0, 1700000060.0, \
             1700000000.5, 1700000001.0); \
         INSERT INTO download_idempotency_keys VALUES ('k1', 't1', 'dispatch', 1700000000.0); \
         INSERT INTO download_import_decisions (task_id, attempt_id, outcome, decided_at) \
         VALUES ('t1', 'a1', 'deferred', 1700000003.0); \
         INSERT INTO download_quarantine (source, identity, reason, quarantined_at) \
         VALUES ('soulseek', 'peer/x', 'corrupt', 1700000004.0); \
         INSERT INTO drop_import_jobs (id, user_id, status, created_at, upload_name, \
             staging_dir) VALUES ('j1', 'u1', 'failed', 1700000005.0, 'up', '/s'); \
         INSERT INTO drop_import_items (id, job_id, folder_name, status, updated_at) \
         VALUES (5, 'j1', 'f', 'needs_review', 1700000006.0); \
         INSERT INTO acquire_held_entries (id, user_id, origin, group_key, drop_item_id, \
             state, created_at, expires_at) \
         VALUES (9, 'u1', 'upload', 'upload:5', 5, 'held', 1, 2); \
         INSERT INTO acquire_held_files (id, entry_id, root, path, original_name, source, \
             reason, state, created_at) \
         VALUES (3, 9, 'drop', 'p', 'o', 'upload', 'weak_match', 'held', 1); \
         INSERT INTO search_jobs (id, user_id, artist_name, album_title, search_query, \
             quality_snapshot_summary, created_at, updated_at) \
         VALUES ('s1', 'u1', 'A', 'B', 'A - B', 'x', 1700000007.0, 1700000007.5); \
         INSERT INTO spotify_auth_requests VALUES ('st', 'u1', 'https://r', 1700000600); \
         INSERT INTO acquire_edition_acquires VALUES ('rg', 't1', 1700000008); \
         INSERT INTO personal_mix_approvals (user_id, state, requested_at, reviewed_by_id, \
             reviewed_at) VALUES ('u1', 'approved', 1700000009.5, 'admin', 1700000010.0); \
         INSERT INTO acquire_follow_cursors (artist_mbid_lower, baselined, next_poll_at, \
             followers) VALUES ('ar', 1, 1700000011, '[]'); \
         INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, user_id, \
             artist_name, album_title, kind, created_at, next_check_at) \
         VALUES ('w1', 'W1', 'u1', 'A', 'B', 'missing', 1, 2); \
         INSERT INTO wanted_seen_candidates VALUES ('w1', 'soulseek', 'peer/B', 1700000012.0); \
         PRAGMA foreign_keys = OFF; \
         INSERT INTO download_idempotency_keys VALUES ('k2', 'gone', 'dispatch', 1); \
         INSERT INTO download_import_decisions (task_id, outcome, decided_at) \
         VALUES ('gone', 'rejected', 1); \
         INSERT INTO search_jobs (id, user_id, artist_name, album_title, search_query, \
             created_at, updated_at) VALUES ('s2', 'ghost', 'A', 'B', 'q', 1, 1); \
         INSERT INTO acquire_edition_acquires VALUES ('rg2', 'gone', 1); \
         INSERT INTO wanted_seen_candidates VALUES ('nowatch', 'soulseek', 'p', 1); \
         PRAGMA foreign_keys = ON;",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    let ms = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let task: (i64, i64, i64, i64, Option<i64>, i64) = sqlx::query_as(
        "SELECT created_at, started_at, last_polled_at, updated_at, year, remote_queued \
         FROM download_tasks WHERE id = 't1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        task,
        (
            1_700_000_000_123,
            1_700_000_000_500,
            1_700_000_002_250,
            1_700_000_002_250,
            None,
            1
        ),
        "times are exact milliseconds and a year that is not a number reads none"
    );
    let attempt: (i64, i64, i64, String) = sqlx::query_as(
        "SELECT next_retry_at, lease_expires_at, created_at, state FROM download_attempts",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        attempt,
        (0, 1_700_000_060_000, 1_700_000_000_500, "in_use".to_owned())
    );
    assert_eq!(
        ms("SELECT created_at FROM download_idempotency_keys").await,
        1_700_000_000_000,
        "the key of a missing download drops"
    );
    assert_eq!(
        ms("SELECT decided_at FROM download_import_decisions").await,
        1_700_000_003_000,
        "the decision of a missing download drops"
    );
    assert_eq!(
        ms("SELECT quarantined_at FROM download_quarantine").await,
        1_700_000_004_000
    );
    assert_eq!(
        ms("SELECT created_at FROM drop_import_jobs").await,
        1_700_000_005_000
    );
    assert_eq!(
        ms("SELECT updated_at FROM drop_import_items WHERE id = 5").await,
        1_700_000_006_000
    );
    assert_eq!(
        ms("SELECT COUNT(*) FROM acquire_held_entries e \
            JOIN acquire_held_files f ON f.entry_id = e.id \
            WHERE e.id = 9 AND e.drop_item_id = 5 AND f.id = 3")
        .await,
        1,
        "the held upload of a drop item survives the rebuild"
    );
    assert_eq!(
        ms("SELECT created_at + updated_at FROM search_jobs WHERE id = 's1'").await,
        3_400_000_014_500
    );
    assert_eq!(
        ms("SELECT COUNT(*) FROM search_jobs").await,
        1,
        "the search job of a missing user drops"
    );
    assert_eq!(
        ms("SELECT expires_at FROM spotify_auth_requests").await,
        1_700_000_600_000
    );
    assert_eq!(
        ms("SELECT started_at FROM acquire_edition_acquires").await,
        1_700_000_008_000,
        "the mark of a missing download drops"
    );
    let mix: (String, i64, Option<String>, i64) = sqlx::query_as(
        "SELECT state, requested_at, reviewed_by_id, reviewed_at FROM personal_mix_approvals",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        mix,
        (
            "approved".to_owned(),
            1_700_000_009_500,
            Some("admin".to_owned()),
            1_700_000_010_000
        )
    );
    assert_eq!(
        ms("SELECT next_poll_at FROM acquire_follow_cursors").await,
        1_700_000_011_000
    );
    assert_eq!(
        ms("SELECT first_seen_at FROM wanted_seen_candidates").await,
        1_700_000_012_000,
        "the seen candidate of a missing watch drops"
    );
    for (table, column) in [
        ("download_tasks", "quality_snapshot_hash"),
        ("download_tasks", "quality_snapshot_summary"),
        ("search_jobs", "quality_snapshot_hash"),
        ("search_jobs", "quality_snapshot_summary"),
        ("acquire_follow_cursors", "followers"),
    ] {
        assert!(
            !column_names(&pool, table).await.iter().any(|c| c == column),
            "{table}.{column} is gone"
        );
    }
    for table in [
        "download_tasks",
        "download_attempts",
        "download_idempotency_keys",
        "download_import_decisions",
        "download_quarantine",
        "download_activity_global_revision",
        "download_activity_user_revisions",
        "drop_import_jobs",
        "drop_import_items",
        "search_jobs",
        "spotify_auth_requests",
        "acquire_edition_acquires",
        "personal_mix_approvals",
        "acquire_follow_cursors",
        "artist_known_releases",
        "user_quotas",
        "wanted_seen_candidates",
    ] {
        let strict: i64 = sqlx::query_scalar("SELECT strict FROM pragma_table_list WHERE name = ?")
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(strict, 1, "{table} is STRICT");
    }
    assert!(
        sqlx::raw_sql("UPDATE personal_mix_approvals SET state = 'dormant'")
            .execute(&pool)
            .await
            .is_err(),
        "only the approval states are taken"
    );
    let triggers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' \
         AND name LIKE 'download_activity_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(triggers, 15, "every activity trigger is back");
    let violations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(violations, 0);
    pool.close().await;

    // The download finishes from its converted row.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    let revision = || -> i64 {
        conn.query_row(
            "SELECT revision FROM download_activity_global_revision",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    let before = revision();
    let store = DownloadStore::new(&conn);
    let row = store.get_task("t1").unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::Downloading);
    assert_eq!(row.created_at, 1_700_000_000.123);
    assert_eq!(row.last_polled_at, Some(1_700_000_002.25));
    let attempt = store.get_attempt("a1").unwrap().unwrap();
    assert_eq!(attempt.lease_expires_at, Some(1_700_000_060.0));
    store
        .transition_task("t1", TaskStatus::Processing, 1_700_000_100.5, None)
        .unwrap();
    store
        .finalize_task_and_attempt(
            "t1",
            TaskStatus::Completed,
            1_700_000_200.25,
            None,
            Some("a1"),
            false,
        )
        .unwrap();
    let done = store.get_task("t1").unwrap().unwrap();
    assert_eq!(done.status, TaskStatus::Completed);
    assert_eq!(done.completed_at, Some(1_700_000_200.25));
    let stored: i64 = conn
        .query_row(
            "SELECT completed_at FROM download_tasks WHERE id = 't1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, 1_700_000_200_250);
    assert_eq!(
        store.get_attempt("a1").unwrap().unwrap().state,
        AttemptState::CleanupPending
    );
    assert!(revision() > before, "the activity triggers still fire");
}

/// Migration 0097 replaces the journal's `mgmt_state` text with a checked
/// `commit_as`, read as it always was (only an `imported:` value is an
/// import), shortens the stored outcomes, and adds the generated-files
/// table: backfilled with every output of a bundle that reached its
/// commit, and none of one that did not or whose text does not read. No
/// row's state, sequence or scan fence moves.
#[tokio::test]
async fn publish_journal_states_are_typed_and_outputs_backfilled() {
    let pool = scratch_pool().await;
    migrator_through(96).run(&pool).await.unwrap();
    let bundle = |id: &str, track: &str, rel: &str, kind: &str| {
        serde_json::json!({"id": id, "items": [{"track_id": track, "outputs": [
            {"dest_root": "music", "dest_rel": rel, "sha256": format!("{id}-sha"),
             "size": 3, "kind": kind}]}]})
        .to_string()
    };
    let journal = |id: &str,
                   bundle: &str,
                   kind: &str,
                   rel: &str,
                   sha: &str,
                   mgmt: Option<&str>,
                   state: &str| {
        format!(
            "INSERT INTO library_publish_journal (id, bundle_id, kind, dest_root, dest_rel, \
             staged, staged_sha256, mgmt_state, state, seq) VALUES ('{id}', '{bundle}', \
             '{kind}', 'music', '{rel}', '/t', '{sha}', {}, '{state}', 5);",
            mgmt.map_or("NULL".to_owned(), |mgmt| format!("'{mgmt}'"))
        )
    };
    let mut setup = String::from("PRAGMA foreign_keys = OFF;");
    for (id, rel, kind) in [
        ("done", "a/cover.jpg", "artwork"),
        ("flagged", "b/b.lrc", "lyrics"),
        ("open", "c/cover.jpg", "artwork"),
    ] {
        setup.push_str(&format!(
            "INSERT INTO library_publish_operations (bundle_id, bundle_json, created_day) \
             VALUES ('{id}', '{}', 1);",
            bundle(id, &format!("t-{id}"), rel, kind)
        ));
    }
    setup.push_str(
        "INSERT INTO library_publish_operations (bundle_id, bundle_json, created_day) \
         VALUES ('damaged', '{\"items\": [', 1);",
    );
    for statement in [
        journal(
            "done-a",
            "done",
            "audio",
            "a/a.flac",
            "x",
            Some("managed:6"),
            "cleaned",
        ),
        journal(
            "done-o",
            "done",
            "output",
            "a/cover.jpg",
            "done-sha",
            None,
            "cleaned",
        ),
        journal(
            "flagged-a",
            "flagged",
            "audio",
            "b/b.flac",
            "x",
            Some("imported:3"),
            "committed",
        ),
        journal(
            "flagged-o",
            "flagged",
            "output",
            "b/b.lrc",
            "flagged-sha",
            None,
            "needs_attention",
        ),
        journal(
            "open-a",
            "open",
            "audio",
            "c/c.flac",
            "x",
            Some("odd"),
            "published",
        ),
        journal(
            "open-o",
            "open",
            "output",
            "c/cover.jpg",
            "open-sha",
            None,
            "published",
        ),
        journal(
            "damaged-o",
            "damaged",
            "output",
            "d/cover.jpg",
            "d",
            None,
            "cleaned",
        ),
    ] {
        setup.push_str(&statement);
    }
    setup.push_str(
        "INSERT INTO library_track_management_state (local_track_id, last_outcome) \
         VALUES ('t1', 'managed:6'), ('t2', 'imported:3'), ('t3', 'succeeded');",
    );
    sqlx::raw_sql(&setup).execute(&pool).await.unwrap();
    type Kept = Vec<(String, String, i64, Option<i64>)>;
    let kept = || async {
        sqlx::query_as::<_, (String, String, i64, Option<i64>)>(
            "SELECT id, state, seq, fence_revision FROM library_publish_journal ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let before: Kept = kept().await;

    apply_migrations(&pool).await.unwrap();
    assert_migrated(&pool).await.unwrap();

    assert_eq!(kept().await, before, "no state, sequence or fence moves");
    let columns = column_names(&pool, "library_publish_journal").await;
    assert!(columns.contains(&"commit_as".to_owned()), "{columns:?}");
    assert!(!columns.contains(&"mgmt_state".to_owned()), "{columns:?}");
    let commit_as: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, commit_as FROM library_publish_journal ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let commit_as: BTreeMap<String, Option<String>> = commit_as.into_iter().collect();
    assert_eq!(commit_as["done-a"].as_deref(), Some("managed"));
    assert_eq!(commit_as["flagged-a"].as_deref(), Some("imported"));
    assert_eq!(
        commit_as["open-a"].as_deref(),
        Some("managed"),
        "read as before"
    );
    assert_eq!(commit_as["done-o"], None);
    let refused =
        sqlx::raw_sql("UPDATE library_publish_journal SET commit_as = 'other' WHERE id = 'done-a'")
            .execute(&pool)
            .await;
    assert!(refused.is_err(), "the check lists the two states");

    let outcomes: Vec<String> = sqlx::query_scalar(
        "SELECT last_outcome FROM library_track_management_state ORDER BY local_track_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(outcomes, ["managed", "imported", "succeeded"]);

    let sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'library_publish_outputs'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(sql.trim_end().ends_with("STRICT"), "{sql}");
    let outputs: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT journal_id, track_id, dest_rel, kind FROM library_publish_outputs \
         ORDER BY journal_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let row = |journal: &str, track: &str, rel: &str, kind: &str| {
        (
            journal.to_owned(),
            track.to_owned(),
            rel.to_owned(),
            kind.to_owned(),
        )
    };
    assert_eq!(
        outputs,
        [
            row("done-o", "t-done", "a/cover.jpg", "artwork"),
            row("flagged-o", "t-flagged", "b/b.lrc", "lyrics"),
        ],
        "a committed bundle's outputs, flagged ones included; none of an open \
         bundle or of one whose text does not read"
    );
}

/// Migration 0098 replaces v2's `artist_genres`, which nothing in v3
/// wrote, with `catalog_artist_genres`: STRICT, one row per artist and
/// folded genre, its time in integer milliseconds and indexed for the
/// prune.
#[tokio::test]
async fn artist_genres_rebuild_to_the_conventions_on_upgrade() {
    let pool = scratch_pool().await;
    migrator_through(96).run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO artist_genres (artist_mbid_lower, artist_mbid, genres_json) \
         VALUES ('a-1', 'A-1', '[\"Shoegaze\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();

    migrator_through(98).run(&pool).await.unwrap();

    let tables = table_names(&pool).await;
    assert!(!tables.contains(&"artist_genres".to_owned()));
    let sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'catalog_artist_genres'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(sql.trim_end().ends_with("STRICT"), "{sql}");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_artist_genres")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "nothing is carried");
    let insert = |artist: &'static str, genre: &'static str, at: f64| {
        sqlx::query(
            "INSERT INTO catalog_artist_genres (artist_mbid, genre_folded, genre, learned_at) \
             VALUES (?1, lower(?2), ?2, ?3)",
        )
        .bind(artist)
        .bind(genre)
        .bind(at)
        .execute(&pool)
    };
    insert("a-1", "Shoegaze", 1_700_000_000_000.0)
        .await
        .unwrap();
    assert!(
        insert("a-1", "shoegaze", 1_700_000_000_001.0)
            .await
            .is_err(),
        "one row per artist and genre"
    );
    assert!(
        insert("a-2", "Drone", 1_700_000_000.5).await.is_err(),
        "times are integer milliseconds"
    );
    let index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
         AND name = 'idx_catalog_artist_genres_learned'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(index, 1);
}
