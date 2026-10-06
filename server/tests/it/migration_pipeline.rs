//! Stage-11 pipeline E2E briefs: a live v2 fixture dir travels export →
//! validator → dry-run → import, then the migrated instance proves itself.
//!
//! One behavior per brief, each on its own scratch fixture: refusal of the
//! orphaned export, the operator repair, imported counts, login with v2
//! passwords (rehashing to Argon2id), follows intact, compat re-auth,
//! settings landed, and idempotent re-import. Nothing here touches the
//! network; every database is a scratch file.

use std::path::{Path, PathBuf};

use droppedneedle::db::WriteLane;
use droppedneedle::runtime_config::{Crypto, Secret};
use droppedneedle::tooling::fixture::{UNKNOWN_USER_ID, build_v2_fixture, delete_orphan_rows};

/// Scratch directory per brief, removed on drop.
fn scratch_dir(tag: &str) -> crate::common::ScratchDir {
    crate::common::ScratchDir::new(&format!("pipeline-{tag}"))
}

fn export_request(v2_root: &Path, passphrase: &str) -> droppedneedle::export::ExportRequest {
    droppedneedle::export::ExportRequest {
        v2_root: v2_root.to_path_buf(),
        db_path: None,
        passphrase: Secret::new(passphrase.to_owned()),
        exported_at: None,
        v2_commit: None,
    }
}

fn issue_codes(report: &droppedneedle::r#import::ValidationReport) -> (Vec<&str>, Vec<&str>) {
    (
        report.errors.iter().map(|issue| issue.code).collect(),
        report.warnings.iter().map(|issue| issue.code).collect(),
    )
}

const PASSPHRASE: &str = "operator-passphrase";

/// A fixture plus its first export, orphans included.
fn orphaned_export(root: &Path) -> (droppedneedle::tooling::fixture::V2Fixture, String) {
    let v2_root = root.join("v2");
    let fixture = build_v2_fixture(&v2_root).expect("fixture builds");
    let export_path = root.join("export.json");
    droppedneedle::export::export_v2_to_file(&export_request(&v2_root, PASSPHRASE), &export_path)
        .expect("export writes");
    let text = std::fs::read_to_string(&export_path).expect("export reads back");
    (fixture, text)
}

/// A fixture plus its export after the operator repair: the orphan rows
/// are dropped from v2 and the file is re-exported clean.
fn repaired_export(root: &Path) -> (droppedneedle::tooling::fixture::V2Fixture, String) {
    let (fixture, _) = orphaned_export(root);
    let export_path = root.join("export.json");
    assert_eq!(
        delete_orphan_rows(&root.join("v2").join("cache").join("library.db")).expect("repair runs"),
        2
    );
    droppedneedle::export::export_v2_to_file(
        &export_request(&root.join("v2"), PASSPHRASE),
        &export_path,
    )
    .expect("re-export writes");
    let text = std::fs::read_to_string(&export_path).expect("re-export reads back");
    (fixture, text)
}

/// A migrated scratch v3 target with the conflict seeds: strangers holding
/// both fixture emails, and a Plex binding under a third account. A
/// writable pool: the serving runtime's pool is read-only by design, and
/// the offline import owns its writes while the server is stopped.
async fn seeded_target(root: &Path) -> (PathBuf, sqlx::SqlitePool) {
    let v3_root = root.join("v3");
    std::fs::create_dir_all(v3_root.join("config")).expect("v3 config dir");
    std::fs::create_dir_all(v3_root.join("cache")).expect("v3 cache dir");
    let db_path = v3_root.join("cache").join("library.db");
    let pool = writable_pool(&db_path).await;
    droppedneedle::schema::apply_migrations(&pool)
        .await
        .expect("scratch schema migrates");
    seed_conflicts(&pool).await;
    (v3_root, pool)
}

#[tokio::test]
async fn orphaned_export_refuses_with_zero_writes() {
    let root = scratch_dir("refuses");
    let (_fixture, text) = orphaned_export(&root);
    let parsed = droppedneedle::export::parse_export(&text).expect("envelope parses");
    assert_eq!(parsed.doc.users.len(), 2);
    assert_eq!(parsed.doc.follows.len(), 4);
    assert_eq!(parsed.doc.approvals.len(), 4);

    let root_value: serde_json::Value = serde_json::from_str(&text).expect("json parses");
    let validation = droppedneedle::r#import::validate_export(&root_value);
    assert!(!validation.valid());
    let (errors, warnings) = issue_codes(&validation);
    assert!(errors.contains(&"DANGLING_USER_REF"), "errors: {errors:?}");
    assert!(
        warnings.contains(&"DANGLING_REVIEWER"),
        "warnings: {warnings:?}"
    );
    assert!(
        warnings.contains(&"REVOKED_APP_PASSWORD_KEPT"),
        "warnings: {warnings:?}"
    );

    let (v3_root, pool) = seeded_target(&root).await;
    let counts_before = table_counts(&pool).await;
    for dry_run in [true, false] {
        let report = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, dry_run).await;
        assert_eq!(
            report.exit.code,
            droppedneedle::r#import::ExitCode::FailedValidation,
            "dry_run={dry_run}"
        );
    }
    assert_eq!(table_counts(&pool).await, counts_before);
    assert!(!v3_root.join("config").join("config.json").exists());
}

#[tokio::test]
async fn operator_repair_converges_to_clean_import() {
    let root = scratch_dir("repair");
    let (_fixture, text) = repaired_export(&root);
    let root_value: serde_json::Value = serde_json::from_str(&text).expect("json parses");
    let validation = droppedneedle::r#import::validate_export(&root_value);
    assert!(validation.valid(), "errors: {:?}", validation.errors);
    let (_, warnings) = issue_codes(&validation);
    assert!(warnings.contains(&"DANGLING_REVIEWER"));

    let (v3_root, pool) = seeded_target(&root).await;
    let counts_before = table_counts(&pool).await;
    let dry_report = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, true).await;
    assert_eq!(
        dry_report.exit.code,
        droppedneedle::r#import::ExitCode::OkWithDrops
    );
    assert_eq!(table_counts(&pool).await, counts_before);
    assert!(!v3_root.join("config").join("config.json").exists());
    let report = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert_eq!(
        report.exit.code,
        droppedneedle::r#import::ExitCode::OkWithDrops
    );
    assert_eq!(report.entities, dry_report.entities, "dry-run parity");
    assert_eq!(
        report.secrets_reencrypted, dry_report.secrets_reencrypted,
        "dry-run parity"
    );
}

#[tokio::test]
async fn imported_counts_match_the_fixture() {
    // Two users in (both emails nulled onto the pre-seeded strangers), two
    // local providers in, the Plex binding kept on its existing owner,
    // three app passwords, one recovery code, three follows, three
    // approvals with one reviewer nulled.
    let root = scratch_dir("counts");
    let (_fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    let report = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert_eq!(
        report.exit.code,
        droppedneedle::r#import::ExitCode::OkWithDrops
    );
    let count = |entity: &str| report.entities.get(entity).expect("entity counted").clone();
    assert_eq!(count("user").imported, 2);
    assert_eq!(count("user").nulled_field, 2);
    assert_eq!(count("provider").imported, 2);
    assert_eq!(count("provider").conflict_kept_existing, 1);
    assert_eq!(count("app_password").imported, 3);
    assert_eq!(count("recovery_code").imported, 1);
    assert_eq!(count("follow").imported, 3);
    assert_eq!(count("approval").imported, 3);
    assert_eq!(count("approval").nulled_field, 1);
    assert_eq!(count("event_city").imported, 2);
    assert_eq!(count("event_seen").imported, 1);
    // Nine settings seals (slskd, sabnzbd, two indexers, AudioDB, one
    // plugin token, one plugin setting with no manifest, the Last.fm key
    // pair), three app passwords and four per-user connections; v2's
    // global Last.fm session key is dropped and counts none.
    assert_eq!(report.secrets_reencrypted, 16);
    // The v2 instance Last.fm key pair opens under the v3 key, so linking
    // and scrobbling keep working after the move.
    let config_dir = v3_root.join("config");
    let store = droppedneedle::runtime_config::ConfigStore::open(
        &config_dir.join("config.json"),
        Crypto::load(&config_dir).expect("v3 key loads"),
    )
    .expect("v3 config opens");
    let lastfm: droppedneedle::runtime_config::secret_sections::LastFmSettings =
        store.get_raw().expect("lastfm settings read");
    assert_eq!(lastfm.api_key.expose(), "lastfm-public-key");
    assert_eq!(lastfm.shared_secret.expose(), "lastfm-shared-plaintext");

    let revoked: Vec<(String, i64)> =
        sqlx::query_as("SELECT name, revoked FROM connect_app_passwords WHERE revoked = 1")
            .fetch_all(&pool)
            .await
            .expect("revoked rows read");
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].0, "old-laptop");

    // No sessions survive the import itself: the file carries none.
    let tokens: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_tokens")
        .fetch_one(&pool)
        .await
        .expect("tokens count");
    assert_eq!(tokens, 0);
}

#[tokio::test]
async fn reviewers_land_kept_or_nulled() {
    let root = scratch_dir("reviewers");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;

    // Bob's approval was reviewed by Alice: a valid reviewer survives
    // verbatim.
    let kept: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals WHERE user_id = ?",
    )
    .bind(&fixture.bob_id)
    .fetch_one(&pool)
    .await
    .expect("bob approval reads");
    assert_eq!(
        kept,
        (Some(fixture.alice_id.clone()), Some("Alice".to_owned()))
    );

    // Alice's second approval was reviewed by a deleted user: the id nulls
    // and the name stays as audit residue.
    let nulled: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals \
         WHERE user_id = ? AND artist_mbid = ?",
    )
    .bind(&fixture.alice_id)
    .bind(&fixture.artist_two_mbid)
    .fetch_one(&pool)
    .await
    .expect("alice approval reads");
    assert_eq!(nulled, (None, Some("Gone".to_owned())));
}

#[tokio::test]
async fn v2_passwords_log_in_and_rehash() {
    let root = scratch_dir("login");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    let lane =
        WriteLane::open(&v3_root.join("cache").join("library.db")).expect("writer lane opens");
    login_rehashes(
        &pool,
        &lane,
        "alice",
        &fixture.alice_id,
        &fixture.secrets.alice_password,
    )
    .await;
    login_rehashes(
        &pool,
        &lane,
        "bob",
        &fixture.bob_id,
        &fixture.secrets.bob_password,
    )
    .await;
}

#[tokio::test]
async fn follows_survive_intact() {
    let root = scratch_dir("follows");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert_follows_intact(&pool, &fixture).await;
}

#[tokio::test]
async fn compat_clients_reauth_on_surviving_secrets() {
    let root = scratch_dir("compat");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    let lane =
        WriteLane::open(&v3_root.join("cache").join("library.db")).expect("writer lane opens");
    assert_compat_reauth(&pool, &lane, &v3_root, &fixture).await;
}

#[tokio::test]
async fn settings_land_with_r8_and_no_plaintext() {
    let root = scratch_dir("settings");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert_settings_landed(&v3_root, &fixture);

    let config_text = std::fs::read_to_string(v3_root.join("config").join("config.json"))
        .expect("v3 config reads");
    for secret in [
        fixture.secrets.slskd_key.as_str(),
        fixture.secrets.audiodb_key.as_str(),
        fixture.secrets.indexer_keys[0].as_str(),
        fixture.secrets.indexer_keys[1].as_str(),
        fixture.secrets.plugin_token.as_str(),
        fixture.secrets.alice_phone_secret.as_str(),
        fixture.secrets.alice_old_secret.as_str(),
        fixture.secrets.bob_legacy_secret.as_str(),
    ] {
        assert!(
            !config_text.contains(secret),
            "v3 config leaks a plaintext secret"
        );
    }
}

#[tokio::test]
async fn reimport_writes_nothing() {
    let root = scratch_dir("reimport");
    let (_fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_runs")
        .fetch_one(&pool)
        .await
        .expect("import runs count");
    assert_eq!(runs, 1);

    let config_path = v3_root.join("config").join("config.json");
    let bytes_before = std::fs::read(&config_path).expect("v3 config reads");
    let again = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    for (entity, counts) in &again.entities {
        assert_eq!(counts.imported, 0, "entity {entity} wrote nothing twice");
    }
    assert_eq!(
        std::fs::read(&config_path).expect("v3 config re-reads"),
        bytes_before,
        "config bytes stay stable across re-imports"
    );
    let runs_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_runs")
        .fetch_one(&pool)
        .await
        .expect("import runs recount");
    assert_eq!(runs_after, 1, "a pure re-import writes no audit row");
}

/// The user data beyond accounts: every section lands with its v2 library
/// references settled against the carried catalog, a duplicate v2 playlist is counted
/// instead of breaking the import, sealed connections open under the v3
/// key, an interrupted carry resumes where it stopped, and a repeat import
/// writes nothing at all and brings back nothing the user deleted.
#[tokio::test]
async fn user_data_carries_resumes_and_repeats_as_noop() {
    use droppedneedle::tooling::fixture::{AVATAR_BYTES, COVER_BYTES, PLAYLIST_ID};

    let root = scratch_dir("carry");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    let rows = |table: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&pool)
                .await
                .expect("table counts")
        }
    };

    // A crash after three sections keeps them; the rest has not landed.
    // The second playlist from the same source is a conflict, and its entry
    // is left out with a count rather than failing the section.
    let crashed = run_pipeline_with(&text, PASSPHRASE, &v3_root, &pool, false, Some(3)).await;
    assert_eq!(
        crashed.exit.code,
        droppedneedle::r#import::ExitCode::FailedInternal
    );
    assert_eq!(rows("user_connections").await, 4);
    assert_eq!(rows("playlists").await, 2);
    assert_eq!(rows("playlist_tracks").await, 2);
    assert_eq!(rows("library_play_history").await, 0);
    let playlists = &crashed.entities["playlist"];
    assert_eq!(
        (playlists.imported, playlists.conflict_kept_existing),
        (2, 1)
    );
    let entries = &crashed.entities["playlist_track"];
    assert_eq!((entries.imported, entries.dropped_invalid), (2, 1));
    // The failed run's audit row holds the sections it did commit.
    let (exit, counts): (String, String) =
        sqlx::query_as("SELECT exit_code, entity_counts FROM import_runs")
            .fetch_one(&pool)
            .await
            .expect("audit row reads");
    assert_eq!(exit, "FAILED_INTERNAL");
    let counts: serde_json::Value = serde_json::from_str(&counts).expect("counts json");
    assert_eq!(counts["playlist_track"]["imported"], 2);

    let resumed = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert_eq!(
        resumed.exit.code,
        droppedneedle::r#import::ExitCode::OkWithDrops,
        "{:?}",
        resumed.exit
    );
    let skipped: Vec<&str> = resumed
        .items
        .iter()
        .filter(|item| item.outcome == "already_applied")
        .map(|item| item.key.as_str())
        .collect();
    assert_eq!(skipped, ["connection", "playlist", "playlist_track"]);
    for (table, expected) in [
        ("playlists", 2),
        ("playlist_covers", 1),
        ("library_user_favorites", 2),
        ("library_play_history", 2),
        ("request_history", 1),
        ("request_history_requesters", 1),
        ("request_history_dismissals", 1),
        ("download_tasks", 1),
        ("download_attempts", 1),
        ("library_user_favorite_names", 2),
        ("wanted_watches", 1),
        ("wanted_seen_candidates", 1),
        ("user_quotas", 1),
        ("download_quarantine", 1),
        ("user_listening_prefs", 1),
        ("personal_mix_approvals", 1),
        ("user_section_prefs", 1),
        ("user_navidrome_folder_preferences", 1),
        ("user_new_release_seen", 1),
        ("compat_play_queues", 1),
        ("compat_play_queue_items", 2),
        ("compat_bookmarks", 1),
        ("artist_known_releases", 1),
        ("new_release_feed", 1),
    ] {
        assert_eq!(rows(table).await, expected, "{table}");
    }
    // Library ids are settled once the catalog lands after the user data:
    // the history row now names the carried track, and only the queued
    // track v2 itself no longer had still waits.
    assert_eq!(rows("import_pending_links").await, 1);
    let history: Option<String> =
        sqlx::query_scalar("SELECT local_track_id FROM library_play_history WHERE id = 'listen-1'")
            .fetch_one(&pool)
            .await
            .expect("history reads");
    assert_eq!(
        history.as_deref(),
        Some(droppedneedle::tooling::fixture::V2_TRACK_ID)
    );
    let cover: Vec<u8> =
        sqlx::query_scalar("SELECT image FROM playlist_covers WHERE playlist_id = ?")
            .bind(PLAYLIST_ID)
            .fetch_one(&pool)
            .await
            .expect("cover reads");
    assert_eq!(cover, COVER_BYTES);
    let avatar = v3_root
        .join("cache")
        .join("avatars")
        .join(format!("{}.png", fixture.alice_id));
    assert_eq!(std::fs::read(avatar).expect("avatar file"), AVATAR_BYTES);
    for (table, left) in [
        ("request_history", 1),
        ("download_tasks", 1),
        ("download_attempts", 2),
        ("auth_tokens", 1),
    ] {
        assert!(
            resumed
                .left_behind
                .iter()
                .any(|item| item.table == table && item.rows == left),
            "{table} listed as left behind"
        );
    }

    // Sealed links open under the v3 key, in the shapes v3 reads.
    let crypto = Crypto::load(&v3_root.join("config")).expect("v3 key loads");
    let link = |service: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT connection_data FROM user_connections WHERE service = ?",
            )
            .bind(service)
            .fetch_one(&pool)
            .await
            .expect("link reads")
        }
    };
    let listenbrainz: serde_json::Value = serde_json::from_str(
        &crypto
            .decrypt(&link("listenbrainz").await)
            .expect("listenbrainz link opens"),
    )
    .expect("listenbrainz json");
    assert_eq!(
        listenbrainz["user_token"],
        fixture.secrets.listenbrainz_token.as_str()
    );
    // Spotify keeps v2's field names, sealed whole like the media servers,
    // and v3's Spotify store reads the carried link as it is.
    let spotify = {
        use droppedneedle::acquire::imports::spotify::SpotifyConnectionStore as _;
        let lane =
            WriteLane::open(&v3_root.join("cache").join("library.db")).expect("writer lane opens");
        let key = Crypto::load(&v3_root.join("config")).expect("v3 key loads");
        droppedneedle::acquire::imports::spotify_store::SqliteSpotifyLinks::new(
            droppedneedle::acquire::db::AcquireDb::new(pool.clone(), lane),
            std::sync::Arc::new(key),
        )
        .get(&fixture.bob_id)
        .await
        .expect("spotify link reads")
        .expect("spotify link carried")
    };
    assert_eq!(spotify.refresh_token, fixture.secrets.spotify_refresh);
    assert_eq!(spotify.spotify_user_id, "bob-sp-id");
    // 2025-06-01T01:00:00+00:00, v2's ISO expiry.
    assert_eq!(spotify.expires_at_unix, 1_748_739_600);
    let lastfm: serde_json::Value =
        serde_json::from_str(&link("lastfm").await).expect("last.fm doc");
    assert_eq!(lastfm["configured"], false);
    assert_eq!(
        crypto
            .decrypt(lastfm["session_key"].as_str().expect("session key"))
            .expect("session key opens"),
        fixture.secrets.lastfm_session
    );
    // A plugin setting sealed without a v2 manifest reads back plain.
    let store = droppedneedle::runtime_config::ConfigStore::open(
        &v3_root.join("config").join("config.json"),
        crypto,
    )
    .expect("v3 config opens");
    let orphan = store
        .get_plugin_raw("orphan", &std::collections::HashSet::new())
        .expect("plugin reads");
    assert_eq!(
        orphan.settings.get("mode").map(String::as_str),
        Some(fixture.secrets.orphan_plugin_mode.as_str())
    );

    // A repeat import writes nothing anywhere, and a playlist deleted in v3
    // since does not come back: each section is carried once.
    sqlx::query("DELETE FROM playlists WHERE id = 'playlist-bob'")
        .execute(&pool)
        .await
        .expect("playlist deletes");
    let before = table_counts(&pool).await;
    let again = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert!(again.entities.values().all(|counts| counts.imported == 0));
    assert_eq!(table_counts(&pool).await, before);
}

/// Pre-seed the target with the conflict cases: strangers holding both
/// fixture emails, and a Plex binding under a third account that collides
/// with Bob's.
async fn seed_conflicts(pool: &sqlx::SqlitePool) {
    for (id, email, username) in [
        ("carol-id", "alice@example.com", "carol"),
        ("dave-id", "bob@example.com", "dave"),
    ] {
        sqlx::query(
            "INSERT INTO auth_users (id, display_name, email, role, created_at, username)
             VALUES (?, ?, ?, 'user', '2026-01-01T00:00:00+00:00', ?)",
        )
        .bind(id)
        .bind(username)
        .bind(email)
        .bind(username)
        .execute(pool)
        .await
        .expect("conflict user seeds");
    }
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at, username)
         VALUES ('mallory-id', 'Mallory', 'user', '2026-01-01T00:00:00+00:00', 'mallory')",
    )
    .execute(pool)
    .await
    .expect("mallory seeds");
    sqlx::query(
        "INSERT INTO auth_providers (id, user_id, provider, provider_uid, provider_data,
                                     created_at)
         VALUES ('prov-mallory', 'mallory-id', 'plex', 'plex-uid-bob',
                 '{\"token\": \"mallory\"}', '2026-01-01T00:00:00+00:00')",
    )
    .execute(pool)
    .await
    .expect("plex binding seeds");
}

/// Row counts per migrated table, for the zero-writes proofs.
async fn table_counts(pool: &sqlx::SqlitePool) -> Vec<(String, i64)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .expect("tables list");
    let mut counts = Vec::new();
    for table in tables {
        let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM \"{table}\""))
            .fetch_one(pool)
            .await
            .expect("table counts");
        counts.push((table, total));
    }
    counts
}

/// One-connection writable pool over a scratch file.
async fn writable_pool(db_path: &std::path::Path) -> sqlx::SqlitePool {
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite:{}?mode=rwc", db_path.display()))
        .await
        .expect("writable pool opens")
}

/// Drive the real import pipeline (dry-run or real) over the scratch pool.
async fn run_pipeline(
    export_text: &str,
    passphrase: &str,
    v3_root: &Path,
    pool: &sqlx::SqlitePool,
    dry_run: bool,
) -> droppedneedle::r#import::ImportReport {
    run_pipeline_with(export_text, passphrase, v3_root, pool, dry_run, None).await
}

/// [`run_pipeline`] with a simulated crash after some carried sections.
async fn run_pipeline_with(
    export_text: &str,
    passphrase: &str,
    v3_root: &Path,
    pool: &sqlx::SqlitePool,
    dry_run: bool,
    fault_after_sections: Option<usize>,
) -> droppedneedle::r#import::ImportReport {
    let config_dir = v3_root.join("config");
    let crypto = if dry_run {
        Crypto::from_key_bytes(&[9u8; 32]).expect("dry key builds")
    } else {
        Crypto::load_or_generate(&config_dir).expect("v3 key loads")
    };
    droppedneedle::r#import::run_import(droppedneedle::r#import::ImportRequest {
        export_bytes: export_text.as_bytes().to_vec(),
        passphrase: passphrase.to_owned(),
        pool: pool.clone(),
        config_path: config_dir.join("config.json"),
        crypto,
        v2_config_path: Some(
            v3_root
                .join("..")
                .join("v2")
                .join("config")
                .join("config.json"),
        ),
        attachments_dir: Some(v3_root.join("..")),
        cache_dir: Some(v3_root.join("cache")),
        dry_run,
        fault_before_commit: false,
        fault_after_commit: false,
        fault_after_sections,
    })
    .await
}

/// One user logs in with their v2 password through the production login
/// path; success persists the Argon2id rehash, and a wrong password still
/// fails.
async fn login_rehashes(
    pool: &sqlx::SqlitePool,
    lane: &droppedneedle::db::WriteLane,
    username: &str,
    user_id: &str,
    password: &str,
) {
    use droppedneedle::auth::session::login::{
        LoginContext, LoginError, LoginRequest, LoginService, TransportParam,
    };
    use droppedneedle::auth::sqlite::{AuthDb, SqliteCredentialLookup, SqliteSessionStore};

    let before: String = sqlx::query_scalar(
        "SELECT provider_data FROM auth_providers WHERE user_id = ? AND provider = 'local'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("provider row reads");
    assert!(before.contains("$2b$"), "bcrypt carried verbatim: {before}");

    let auth_db = AuthDb::new(pool, lane);
    let hasher = droppedneedle::auth::passwords::Argon2idHasher::new();
    let mut sessions = SqliteSessionStore::new(&auth_db);
    sessions.set_rehash_queue(hasher.rehash_queue().clone());
    let login = LoginService::new(sessions, hasher, SqliteCredentialLookup::new(&auth_db));
    let context = || LoginContext {
        user_agent: None,
        now_unix: 1_769_000_000,
    };
    let success = login
        .login(
            LoginRequest {
                username: username.to_owned(),
                password: password.to_owned(),
                transport: TransportParam::Bearer,
            },
            context(),
        )
        .await
        .expect("v2 password logs in");
    assert_eq!(success.user_id, user_id);

    let after: String = sqlx::query_scalar(
        "SELECT provider_data FROM auth_providers WHERE user_id = ? AND provider = 'local'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("provider row re-reads");
    assert!(after.contains("$argon2id$"), "rehash persisted: {after}");
    assert!(
        after.contains("\"scheme\":\"argon2id\""),
        "tag flipped: {after}"
    );

    let denied = login
        .login(
            LoginRequest {
                username: username.to_owned(),
                password: "wrong-password".to_owned(),
                transport: TransportParam::Bearer,
            },
            context(),
        )
        .await;
    assert!(matches!(denied, Err(LoginError::InvalidCredentials)));
}

/// Every imported follow reads back with its v2 values.
async fn assert_follows_intact(
    pool: &sqlx::SqlitePool,
    fixture: &droppedneedle::tooling::fixture::V2Fixture,
) {
    let rows: Vec<(String, String, String, i64, f64, f64)> = sqlx::query_as(
        "SELECT user_id, artist_mbid, artist_name, auto_download, followed_at, updated_at
         FROM user_followed_artists ORDER BY user_id, artist_mbid",
    )
    .fetch_all(pool)
    .await
    .expect("follows read back");
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|row| row.0 == fixture.alice_id
        && row.1 == fixture.artist_one_mbid
        && row.2 == "Fixture Artist One"
        && row.3 == 1));
    assert!(
        rows.iter()
            .any(|row| row.0 == fixture.alice_id && row.1 == fixture.artist_two_mbid && row.3 == 0)
    );
    assert!(
        rows.iter()
            .any(|row| row.0 == fixture.bob_id && row.1 == fixture.artist_one_mbid)
    );
    assert!(rows.iter().all(|row| row.0 != UNKNOWN_USER_ID));
}

/// The compat re-auth production path: the surviving secret resolves by its
/// recomputed SHA-256, decrypts under the v3 key, stamps use; the revoked
/// secret resolves nowhere.
async fn assert_compat_reauth(
    pool: &sqlx::SqlitePool,
    lane: &droppedneedle::db::WriteLane,
    v3_root: &Path,
    fixture: &droppedneedle::tooling::fixture::V2Fixture,
) {
    use droppedneedle::auth::session::tokens::hash_token;
    use droppedneedle::auth::sqlite::{AuthDb, SqliteAppPasswordStore};
    use droppedneedle::auth::users::stores::AppPasswordStore;

    let auth_db = AuthDb::new(pool, lane);
    let store = SqliteAppPasswordStore::new(&auth_db);
    let crypto = Crypto::load(&v3_root.join("config")).expect("v3 key loads");

    let row = store
        .get_active_by_sha256(&hash_token(&fixture.secrets.alice_phone_secret))
        .await
        .expect("lookup runs")
        .expect("surviving secret resolves");
    assert_eq!(row.user_id, fixture.alice_id);
    assert_eq!(row.name, "phone");
    assert_eq!(
        crypto
            .decrypt(&row.secret_encrypted)
            .expect("v3 ciphertext opens"),
        fixture.secrets.alice_phone_secret
    );
    store
        .touch(&row.secret_sha256, 1_769_000_100, Some("fixture-client"))
        .await
        .expect("touch runs");
    let touched = store
        .get_active_by_sha256(&row.secret_sha256)
        .await
        .expect("re-lookup runs")
        .expect("row still active");
    assert_eq!(touched.last_client.as_deref(), Some("fixture-client"));

    let revoked = store
        .get_active_by_sha256(&hash_token(&fixture.secrets.alice_old_secret))
        .await
        .expect("revoked lookup runs");
    assert!(revoked.is_none(), "revocation survived the import");

    let legacy = store
        .get_active_by_sha256(&hash_token(&fixture.secrets.bob_legacy_secret))
        .await
        .expect("legacy lookup runs")
        .expect("legacy-plaintext secret resolves");
    assert_eq!(legacy.user_id, fixture.bob_id);
}

/// Settings assertions: R8 carried the legacy frequency, secrets decrypt
/// under the v3 key, dropped fields stayed behind, order and ids survived.
fn assert_settings_landed(v3_root: &Path, fixture: &droppedneedle::tooling::fixture::V2Fixture) {
    let text = std::fs::read_to_string(v3_root.join("config").join("config.json"))
        .expect("v3 config reads");
    let config: serde_json::Value = serde_json::from_str(&text).expect("v3 config parses");
    assert_eq!(
        config
            .get("instance_id")
            .and_then(serde_json::Value::as_str),
        Some(fixture.instance_id.as_str())
    );
    assert_eq!(
        config
            .get("library_scan_schedule")
            .and_then(|section| section.get("scan_frequency"))
            .and_then(serde_json::Value::as_str),
        Some("6hr"),
        "R8 carried the legacy frequency"
    );
    let crypto = Crypto::load(&v3_root.join("config")).expect("v3 key loads");
    let slskd = config
        .get("download_client")
        .and_then(|section| section.get("api_key"))
        .and_then(serde_json::Value::as_str)
        .expect("slskd key present");
    assert_eq!(
        crypto.decrypt(slskd).expect("slskd key opens"),
        fixture.secrets.slskd_key
    );
    let audio = config
        .get("advanced_settings")
        .and_then(|section| section.get("audiodb_api_key"))
        .and_then(serde_json::Value::as_str)
        .expect("audiodb key present");
    assert_eq!(
        crypto.decrypt(audio).expect("audiodb key opens"),
        fixture.secrets.audiodb_key
    );
    assert!(
        config
            .get("advanced_settings")
            .and_then(|section| section.get("artist_discovery_warm_interval"))
            .is_none(),
        "dropped advanced field stayed behind"
    );
    let indexers = config
        .get("indexers")
        .and_then(serde_json::Value::as_array)
        .expect("indexers present");
    assert_eq!(indexers.len(), 2);
    assert_eq!(
        indexers[0].get("name").and_then(serde_json::Value::as_str),
        Some("first"),
        "indexer order survived"
    );
}
