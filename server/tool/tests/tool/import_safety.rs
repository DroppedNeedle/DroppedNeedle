//! Safety tests: atomic rollback, kill-mid-import recovery,
//! dry-run parity, and zero-write failures.
//!
//! Scratch state only: in-memory databases, temp-dir config folders.

use crate::import_support as support;

use serde_json::json;
use support::{
    Fixture, MBID, PASSPHRASE, count, import_request, migrated_pool, scratch_config, seed_settings,
    settings_rows, stored_settings, test_crypto,
};

use droppedneedle_tool::import::{ExitCode, run_import};

async fn entity_counts(pool: &sqlx::SqlitePool) -> Vec<i64> {
    let mut out = Vec::new();
    for table in [
        "auth_users",
        "auth_providers",
        "connect_app_passwords",
        "auth_password_recovery_codes",
        "user_followed_artists",
        "auto_download_approvals",
        "import_runs",
    ] {
        out.push(count(pool, table).await);
    }
    out
}

#[tokio::test]
async fn mid_import_failure_rolls_back_everything() {
    // A recovery hash colliding with a row already in the target (across
    // users, so no validator rule can see it) violates the UNIQUE column
    // mid-transaction, which must roll the whole import back.
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1"), fixture.user("u2")]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let pool = migrated_pool().await;
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('prior', 'Prior', 'user', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO auth_password_recovery_codes (user_id, code_hash, created_at, expires_at) \
         VALUES ('prior', 'hash-u2', '', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    seed_settings(&pool, json!({"wanted": {"enabled": false}})).await;
    let before = entity_counts(&pool).await;
    let settings_before = settings_rows(&pool).await;

    let (_dir, config_path, crypto) = scratch_config("rollback");
    let report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::FailedInternal);
    assert_eq!(entity_counts(&pool).await, before);
    assert_eq!(
        settings_rows(&pool).await,
        settings_before,
        "no settings row changed"
    );
}

#[tokio::test]
async fn kill_before_commit_recovers_clean() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["follows"] = json!([fixture.follow("u1", MBID)]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let pool = migrated_pool().await;
    seed_settings(&pool, json!({"wanted": {"enabled": false}})).await;
    let settings_before = settings_rows(&pool).await;
    let (_dir, config_path, crypto) = scratch_config("kill");
    let mut crashed = import_request(&export, pool.clone(), config_path.clone(), test_crypto());
    crashed.fault_before_commit = true;
    let report = run_import(crashed).await;
    assert_eq!(report.exit.code, ExitCode::FailedInternal);

    assert_eq!(entity_counts(&pool).await, vec![0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        settings_rows(&pool).await,
        settings_before,
        "settings must be untouched by the killed run"
    );

    // The same import retries cleanly after the crash.
    let retry = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;
    assert_eq!(retry.exit.code, ExitCode::Ok);
    assert_eq!(count(&pool, "auth_users").await, 1);
}

/// The settings commit with the records: a crash right after the commit
/// leaves the export's settings stored beside its users, and the retry
/// writes no row and no audit row.
#[tokio::test]
async fn crash_after_commit_converges_on_retry() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("split");
    let mut crashed = import_request(&export, pool.clone(), config_path.clone(), test_crypto());
    crashed.fault_after_commit = true;
    let report = run_import(crashed).await;
    assert_eq!(report.exit.code, ExitCode::FailedInternal);
    assert_eq!(
        count(&pool, "auth_users").await,
        1,
        "DB committed before the crash"
    );
    assert_eq!(
        stored_settings(&pool).await["wanted"],
        json!({"enabled": true}),
        "the settings committed with the users"
    );
    let rows = settings_rows(&pool).await;
    let runs = count(&pool, "import_runs").await;

    let retry = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;
    assert_eq!(retry.exit.code, ExitCode::Ok);
    assert_eq!(retry.entities["user"].skipped_identical, 1);
    assert_eq!(settings_rows(&pool).await, rows, "the retry writes no row");
    assert_eq!(count(&pool, "import_runs").await, runs, "and no audit row");
}

#[tokio::test]
async fn dry_run_matches_real_import() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1"), fixture.user("u2")]);
    export["follows"] = json!([fixture.follow("u1", MBID)]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("parity");
    let mut dry = import_request(&export, pool.clone(), config_path.clone(), test_crypto());
    dry.dry_run = true;
    let preview = run_import(dry).await;

    assert_eq!(preview.exit.code, ExitCode::Ok);
    assert!(preview.dry_run);
    assert_eq!(entity_counts(&pool).await, vec![0, 0, 0, 0, 0, 0, 0]);
    assert!(
        settings_rows(&pool).await.is_empty(),
        "dry-run writes nothing"
    );

    let real = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;
    assert_eq!(real.exit.code, ExitCode::Ok);
    assert_eq!(preview.entities, real.entities);
    assert_eq!(preview.settings_applied, real.settings_applied);
    assert_eq!(preview.settings_defaulted, real.settings_defaulted);
    assert_eq!(preview.secrets_reencrypted, real.secrets_reencrypted);
}

#[tokio::test]
async fn dry_run_matches_conflicts_too() {
    let fixture = Fixture::shared();
    let mut seed = fixture.shell();
    seed["users"] = json!([fixture.user("u1")]);
    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("parity-conflict");
    run_import(import_request(
        &seed,
        pool.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;

    let mut clash = fixture.shell();
    let mut second = fixture.user("u2");
    second["email"] = json!("u1@example.com");
    clash["users"] = json!([second]);
    let mut dry = import_request(&clash, pool.clone(), config_path.clone(), test_crypto());
    dry.dry_run = true;
    let preview = run_import(dry).await;
    let real = run_import(import_request(&clash, pool.clone(), config_path, crypto)).await;

    assert_eq!(preview.exit.code, ExitCode::OkWithDrops);
    assert_eq!(preview.entities, real.entities);
    assert_eq!(preview.entities["user"].nulled_field, 1);
    assert_eq!(real.entities["user"].nulled_field, 1);
}

#[tokio::test]
async fn wrong_passphrase_writes_nothing() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("passphrase");
    let mut request = import_request(&export, pool.clone(), config_path.clone(), crypto);
    request.passphrase = "the wrong passphrase".to_owned();
    let report = run_import(request).await;

    assert_eq!(report.exit.code, ExitCode::EnvelopeAuthFailed);
    assert_eq!(entity_counts(&pool).await, vec![0, 0, 0, 0, 0, 0, 0]);
    assert!(settings_rows(&pool).await.is_empty());
    assert_ne!(PASSPHRASE, "the wrong passphrase");
}

#[tokio::test]
async fn wrong_passphrase_preserves_existing_config() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let pool = migrated_pool().await;
    // The target's settings as they stand once its config.json was copied
    // in.
    seed_settings(&pool, json!({"wanted": {"enabled": false}})).await;
    let before = settings_rows(&pool).await;
    let (_dir, config_path, crypto) = scratch_config("passphrase-kept");
    let mut request = import_request(&export, pool.clone(), config_path.clone(), crypto);
    request.passphrase = "the wrong passphrase".to_owned();
    let report = run_import(request).await;

    assert_eq!(report.exit.code, ExitCode::EnvelopeAuthFailed);
    assert_eq!(entity_counts(&pool).await, vec![0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(settings_rows(&pool).await, before);
}

/// A target that stores no settings yet starts from its config.json; one
/// that does not read refuses the import before anything is written.
#[tokio::test]
async fn corrupt_v3_config_refuses_with_zero_db_writes() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("corrupt-config");
    std::fs::write(&config_path, "{not json").unwrap();
    let report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::FailedInternal);
    assert_eq!(entity_counts(&pool).await, vec![0, 0, 0, 0, 0, 0, 0]);
    assert!(settings_rows(&pool).await.is_empty());
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "{not json");
}

#[tokio::test]
async fn validation_failure_writes_nothing() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["_legacy_lidarr"] = json!({"api_key": "must-not-cross"});

    let pool = migrated_pool().await;
    seed_settings(&pool, json!({"wanted": {"enabled": false}})).await;
    let before = settings_rows(&pool).await;
    let (_dir, config_path, crypto) = scratch_config("no-write");
    let report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::FailedValidation);
    assert_eq!(entity_counts(&pool).await, vec![0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(settings_rows(&pool).await, before);
}

// A modified record, or a sealed blob moved to another user, fails the
// content digest before anything else runs: zero writes.
#[tokio::test]
async fn tampered_export_fails_the_checksum() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1"), fixture.user("u2")]);
    let signed: serde_json::Value = serde_json::from_slice(&support::signed(&export)).unwrap();

    let mut promoted = signed.clone();
    promoted["users"][0]["role"] = json!("admin");
    let mut swapped = signed.clone();
    swapped["users"][0]["app_passwords"][0]["secret"] =
        signed["users"][1]["app_passwords"][0]["secret"].clone();
    for mutant in [promoted, swapped] {
        let pool = migrated_pool().await;
        let (_dir, config_path, crypto) = scratch_config("checksum");
        let report = run_import(import_request(&mutant, pool.clone(), config_path, crypto)).await;
        assert_eq!(report.exit.code, ExitCode::FailedValidation);
        assert!(report.exit.message.contains("CHECKSUM_MISMATCH"));
        assert_eq!(entity_counts(&pool).await, vec![0, 0, 0, 0, 0, 0, 0]);
    }
}

// The import takes the write lock before it reads the snapshot: a writer
// that commits while the import waits is merged, not overwritten or
// collided with.
#[tokio::test]
async fn import_serializes_with_a_concurrent_writer() {
    use std::str::FromStr as _;
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["follows"] = json!([fixture.follow("u1", MBID)]);
    let (dir, config_path, crypto) = scratch_config("serialize");
    let db = dir.join("v3.db");
    let options =
        sqlx::sqlite::SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rwc", db.display()))
            .unwrap()
            .busy_timeout(std::time::Duration::from_secs(30));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    droppedneedle_kernel::schema::apply_migrations(&pool)
        .await
        .unwrap();

    let writer = rusqlite::Connection::open(&db).unwrap();
    writer
        .execute_batch(&format!(
            "BEGIN IMMEDIATE;
             INSERT INTO auth_users (id, display_name, role, created_at)
                 VALUES ('u1', 'Live', 'user', '');
             INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower,
                 artist_name, auto_download, followed_at, updated_at)
                 VALUES ('u1', '{MBID}', '{MBID}', 'Live', 1, 1, 1);"
        ))
        .unwrap();
    let import = tokio::spawn(run_import(import_request(
        &export,
        pool.clone(),
        config_path,
        crypto,
    )));
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    writer.execute_batch("COMMIT").unwrap();
    let report = import.await.unwrap();

    assert_ne!(report.exit.code, ExitCode::FailedInternal, "{report:?}");
    assert_eq!(report.entities["user"].conflict_kept_existing, 1);
    let auto: i64 = sqlx::query_scalar("SELECT auto_download FROM user_followed_artists")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(auto, 1, "the live writer's switch survives the merge");
}
