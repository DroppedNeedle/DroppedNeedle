//! Safety tests: atomic rollback, kill-mid-import recovery,
//! dry-run parity, and zero-write failures.
//!
//! Scratch state only: in-memory databases, temp-dir config folders.

use crate::common::Probe;
use crate::import_support as support;
use std::path::Path;

use serde_json::json;
use support::{
    Fixture, MBID, count, import_request, migrated_file, scratch_config, seed_settings,
    settings_rows, stored_settings, test_crypto,
};

use droppedneedle_tool::import::{ExitCode, run_import};

fn entity_counts(target_db: &Path) -> Vec<i64> {
    let mut out = Vec::new();
    for table in [
        "auth_users",
        "auth_providers",
        "auth_app_passwords",
        "auth_password_recovery_codes",
        "acquire_followed_artists",
        "auto_download_approvals",
        "import_runs",
    ] {
        out.push(count(target_db, table));
    }
    out
}

#[tokio::test]
async fn mid_import_failure_rolls_back_everything() {
    // The database refuses a row mid-transaction, after earlier rows were
    // written, which must roll the whole import back.
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1"), fixture.user("u2")]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let (target_db, _target) = migrated_file();
    target_db.exec(
        "CREATE TRIGGER refuse_u2 BEFORE INSERT ON auth_app_passwords \
         WHEN NEW.user_id = 'u2' BEGIN SELECT RAISE(ABORT, 'refused'); END",
        [],
    );
    seed_settings(&target_db, json!({"wanted": {"enabled": false}}));
    let before = entity_counts(&target_db);
    let settings_before = settings_rows(&target_db);

    let (_dir, config_path, crypto) = scratch_config("rollback");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::FailedInternal);
    assert_eq!(entity_counts(&target_db), before);
    assert_eq!(
        settings_rows(&target_db),
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

    let (target_db, _target) = migrated_file();
    seed_settings(&target_db, json!({"wanted": {"enabled": false}}));
    let settings_before = settings_rows(&target_db);
    let (_dir, config_path, crypto) = scratch_config("kill");
    let mut crashed = import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    );
    crashed.fault_before_commit = true;
    let report = run_import(crashed).await;
    assert_eq!(report.exit.code, ExitCode::FailedInternal);

    assert_eq!(entity_counts(&target_db), vec![0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        settings_rows(&target_db),
        settings_before,
        "settings must be untouched by the killed run"
    );

    // The same import retries cleanly after the crash.
    let retry = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;
    assert_eq!(retry.exit.code, ExitCode::Ok);
    assert_eq!(count(&target_db, "auth_users"), 1);
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("split");
    let mut crashed = import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    );
    crashed.fault_after_commit = true;
    let report = run_import(crashed).await;
    assert_eq!(report.exit.code, ExitCode::FailedInternal);
    assert_eq!(
        count(&target_db, "auth_users"),
        1,
        "DB committed before the crash"
    );
    assert_eq!(
        stored_settings(&target_db)["wanted"],
        json!({"enabled": true}),
        "the settings committed with the users"
    );
    let rows = settings_rows(&target_db);
    let runs = count(&target_db, "import_runs");

    let retry = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;
    assert_eq!(retry.exit.code, ExitCode::Ok);
    assert_eq!(retry.entities["user"].skipped_identical, 1);
    assert_eq!(settings_rows(&target_db), rows, "the retry writes no row");
    assert_eq!(count(&target_db, "import_runs"), runs, "and no audit row");
}

/// A dry run writes nothing and reports what the real import then does, on
/// a fresh target and on one where an email collides.
#[tokio::test]
async fn dry_run_matches_the_real_import() {
    let fixture = Fixture::shared();

    let case = "fresh target";
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1"), fixture.user("u2")]);
    export["follows"] = json!([fixture.follow("u1", MBID)]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("parity");
    let mut dry = import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    );
    dry.dry_run = true;
    let preview = run_import(dry).await;

    assert_eq!(preview.exit.code, ExitCode::Ok, "case: {case}");
    assert!(preview.dry_run, "case: {case}");
    assert_eq!(
        entity_counts(&target_db),
        vec![0, 0, 0, 0, 0, 0, 0],
        "case: {case}"
    );
    assert!(
        settings_rows(&target_db).is_empty(),
        "case: {case}: dry-run writes nothing"
    );

    let real = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;
    assert_eq!(real.exit.code, ExitCode::Ok, "case: {case}");
    assert_eq!(preview.entities, real.entities, "case: {case}");
    assert_eq!(
        preview.settings_applied, real.settings_applied,
        "case: {case}"
    );
    assert_eq!(
        preview.settings_defaulted, real.settings_defaulted,
        "case: {case}"
    );
    assert_eq!(
        preview.secrets_reencrypted, real.secrets_reencrypted,
        "case: {case}"
    );

    let case = "colliding email";
    let mut seed = fixture.shell();
    seed["users"] = json!([fixture.user("u1")]);
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("parity-conflict");
    run_import(import_request(
        &seed,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;

    let mut clash = fixture.shell();
    let mut second = fixture.user("u2");
    second["email"] = json!("u1@example.com");
    clash["users"] = json!([second]);
    let mut dry = import_request(
        &clash,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    );
    dry.dry_run = true;
    let preview = run_import(dry).await;
    let real = run_import(import_request(
        &clash,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(preview.exit.code, ExitCode::OkWithDrops, "case: {case}");
    assert_eq!(preview.entities, real.entities, "case: {case}");
    assert_eq!(preview.entities["user"].nulled_field, 1, "case: {case}");
    assert_eq!(real.entities["user"].nulled_field, 1, "case: {case}");
}

#[tokio::test]
async fn wrong_passphrase_preserves_existing_config() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let (target_db, _target) = migrated_file();
    // The target's settings as they stand once its config.json was copied
    // in.
    seed_settings(&target_db, json!({"wanted": {"enabled": false}}));
    let before = settings_rows(&target_db);
    let (_dir, config_path, crypto) = scratch_config("passphrase-kept");
    let mut request = import_request(&export, target_db.clone(), config_path.clone(), crypto);
    request.passphrase = "the wrong passphrase".to_owned();
    let report = run_import(request).await;

    assert_eq!(report.exit.code, ExitCode::EnvelopeAuthFailed);
    assert_eq!(entity_counts(&target_db), vec![0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(settings_rows(&target_db), before);
}

/// A target that stores no settings yet starts from its config.json; one
/// that does not read refuses the import before anything is written.
#[tokio::test]
async fn corrupt_v3_config_refuses_with_zero_db_writes() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("corrupt-config");
    std::fs::write(&config_path, "{not json").unwrap();
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::FailedInternal);
    assert_eq!(entity_counts(&target_db), vec![0, 0, 0, 0, 0, 0, 0]);
    assert!(settings_rows(&target_db).is_empty());
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "{not json");
}

#[tokio::test]
async fn validation_failure_writes_nothing() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["_legacy_lidarr"] = json!({"api_key": "must-not-cross"});

    let (target_db, _target) = migrated_file();
    seed_settings(&target_db, json!({"wanted": {"enabled": false}}));
    let before = settings_rows(&target_db);
    let (_dir, config_path, crypto) = scratch_config("no-write");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::FailedValidation);
    assert_eq!(entity_counts(&target_db), vec![0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(settings_rows(&target_db), before);
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
        let (target_db, _target) = migrated_file();
        let (_dir, config_path, crypto) = scratch_config("checksum");
        let report = run_import(import_request(
            &mutant,
            target_db.clone(),
            config_path,
            crypto,
        ))
        .await;
        assert_eq!(report.exit.code, ExitCode::FailedValidation);
        assert!(report.exit.message.contains("CHECKSUM_MISMATCH"));
        assert_eq!(entity_counts(&target_db), vec![0, 0, 0, 0, 0, 0, 0]);
    }
}

// The import takes the write lock before it reads the snapshot: a writer
// that commits while the import waits is merged, not overwritten or
// collided with.
#[tokio::test]
async fn import_serializes_with_a_concurrent_writer() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["follows"] = json!([fixture.follow("u1", MBID)]);
    let (dir, config_path, crypto) = scratch_config("serialize");
    let db = dir.join("v3.db");
    droppedneedle_kernel::db::migrate_file(&db, &droppedneedle_kernel::schema::CHAIN).unwrap();
    let target_db = db.clone();

    let writer = rusqlite::Connection::open(&db).unwrap();
    writer
        .execute_batch(&format!(
            "BEGIN IMMEDIATE;
             INSERT INTO auth_users (id, display_name, role, created_at)
                 VALUES ('u1', 'Live', 'user', 0);
             INSERT INTO acquire_followed_artists (user_id, artist_mbid, artist_mbid_lower,
                 artist_name, auto_download, followed_at, updated_at)
                 VALUES ('u1', '{MBID}', '{MBID}', 'Live', 1, 1, 1);"
        ))
        .unwrap();
    let import = tokio::spawn(run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    )));
    // Long enough for the import to derive its key and wait on the lock,
    // well inside its 5 s busy timeout.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    writer.execute_batch("COMMIT").unwrap();
    let report = import.await.unwrap();

    assert_ne!(report.exit.code, ExitCode::FailedInternal, "{report:?}");
    assert_eq!(report.entities["user"].conflict_kept_existing, 1);
    let auto: i64 = target_db.one("SELECT auto_download FROM acquire_followed_artists", []);
    assert_eq!(auto, 1, "the live writer's switch survives the merge");
}
