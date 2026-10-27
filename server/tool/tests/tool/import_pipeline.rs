//! Pipeline tests: conflicts, deleted IDs, idempotency,
//! settings replace, and the one-shot scan schedule carry.
//!
//! Every test imports into scratch state only: in-memory databases
//! and temp-dir config files.

use crate::import_support as support;

use serde_json::{Value, json};
use support::{
    BCRYPT_HASH, Fixture, MBID, count, import_request, migrated_pool, scratch_config,
    settings_rows, settings_store, stored_settings, test_crypto,
};

use droppedneedle_tool::import::{ExitCode, run_import};

fn counts(report: &droppedneedle_tool::import::ImportReport, entity: &str) -> (u64, u64, u64, u64) {
    let found = &report.entities[entity];
    (
        found.imported,
        found.skipped_identical,
        found.conflict_kept_existing,
        found.nulled_field,
    )
}

#[tokio::test]
async fn fresh_import_applies_everything() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["follows"] = json!([fixture.follow("u1", MBID)]);
    export["approvals"] = json!([fixture.approval("u1", MBID)]);
    export["settings"]["jellyfin_settings"] =
        json!({"api_key": fixture.seal("jelly-secret"), "jellyfin_url": "http://jelly:8096"});

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("fresh");
    let report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::Ok);
    assert_eq!(counts(&report, "user"), (1, 0, 0, 0));
    assert_eq!(counts(&report, "provider"), (1, 0, 0, 0));
    assert_eq!(counts(&report, "app_password"), (1, 0, 0, 0));
    assert_eq!(counts(&report, "recovery_code"), (1, 0, 0, 0));
    assert_eq!(counts(&report, "follow"), (1, 0, 0, 0));
    assert_eq!(counts(&report, "approval"), (1, 0, 0, 0));
    assert_eq!(report.secrets_reencrypted, 2);
    assert!(
        report
            .settings_applied
            .contains(&"jellyfin_settings".to_owned())
    );
    assert!(report.settings_defaulted.contains(&"wanted".to_owned()));

    let name: String = sqlx::query_scalar("SELECT display_name FROM auth_users WHERE id = 'u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "User u1");
    let data: String = sqlx::query_scalar(
        "SELECT provider_data FROM auth_providers WHERE provider_uid = 'local:u1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        data.contains(BCRYPT_HASH),
        "bcrypt hash must survive verbatim"
    );
    let stored: String = sqlx::query_scalar(
        "SELECT secret_encrypted FROM connect_app_passwords WHERE name = 'phone'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        stored.starts_with("v3:"),
        "app secret must be v3 ciphertext"
    );
    let hash: String = sqlx::query_scalar(
        "SELECT code_hash FROM auth_password_recovery_codes WHERE user_id = 'u1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(hash, "hash-u1");
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 1);

    let stored_config = stored_settings(&pool).await;
    assert!(
        !stored_config.to_string().contains("jelly-secret"),
        "plaintext must never reach the settings"
    );
    assert_eq!(stored_config["instance_id"], json!("instance-1"));
    let report_json = report.to_json();
    assert!(!report_json.contains("jelly-secret"));
    assert!(!report_json.contains("app-secret"));
}

#[tokio::test]
async fn provider_binding_conflict_keeps_first_mapping() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    let mut first = fixture.user("u1");
    first["email"] = json!("u1@example.com");
    first["username"] = json!("u1");
    let mut second = fixture.user("u2");
    second["email"] = json!("u2@example.com");
    second["username"] = json!("u2");
    second["providers"] = first["providers"].clone();
    // Same file, same binding twice would fail validation; stage the
    // conflict across two imports instead.
    second["providers"][0]["provider_uid"] = json!("local:u2");
    export["users"] = json!([first.clone(), second]);

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("provider-first");
    let first_report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;
    assert_eq!(first_report.exit.code, ExitCode::Ok);

    // Re-import a file where u2 claims u1's binding.
    let mut clash = fixture.shell();
    let mut u2 = fixture.user("u2");
    u2["email"] = json!("u2@example.com");
    u2["username"] = json!("u2");
    u2["providers"] = first["providers"].clone();
    u2["app_passwords"] = json!([]);
    u2["recovery_code"] = Value::Null;
    clash["users"] = json!([u2]);
    let crypto2 = test_crypto();
    let report = run_import(import_request(&clash, pool.clone(), config_path, crypto2)).await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["provider"].conflict_kept_existing, 1);
    assert_eq!(report.entities["provider"].imported, 0);
    let owner: String = sqlx::query_scalar(
        "SELECT user_id FROM auth_providers WHERE provider = 'local' AND provider_uid = 'local:u1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owner, "u1");
    let _ = crypto;
}

#[tokio::test]
async fn email_and_username_collisions_null_the_field() {
    let fixture = Fixture::shared();
    let mut seed = fixture.shell();
    seed["users"] = json!([fixture.user("u1")]);
    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("nulling");
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
    second["username"] = json!("u1");
    clash["users"] = json!([second]);
    let report = run_import(import_request(&clash, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["user"].imported, 1);
    assert_eq!(report.entities["user"].nulled_field, 2);
    let row: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT email, username FROM auth_users WHERE id = 'u2'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        row,
        (None, None),
        "colliding fields are nulled, account survives"
    );
}

#[tokio::test]
async fn follow_merge_never_downgrades() {
    let fixture = Fixture::shared();
    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("follow-merge");
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('u1', 'U', 'user', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower, artist_name, \
         auto_download, followed_at, updated_at) VALUES ('u1', ?, ?, 'Old Name', 1, 100.0, 200.0)",
    )
    .bind(MBID)
    .bind(MBID.to_lowercase())
    .execute(&pool)
    .await
    .unwrap();

    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let mut follow = fixture.follow("u1", MBID);
    follow["auto_download"] = json!(false);
    follow["followed_at"] = json!(50.0);
    follow["updated_at"] = json!(300.0);
    export["follows"] = json!([follow]);
    let report = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["follow"].conflict_kept_existing, 1);
    assert_eq!(
        report.entities["user"].conflict_kept_existing, 1,
        "the seeded same-id user row stands"
    );
    let row: (i64, f64, f64, String) = sqlx::query_as(
        "SELECT auto_download, followed_at, updated_at, artist_name FROM user_followed_artists \
         WHERE user_id = 'u1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (1, 50.0, 300.0, "Old Name".to_owned()),
        "OR never downgrades, min/max win, existing name kept"
    );
}

#[tokio::test]
async fn approval_merge_most_permissive_wins() {
    let fixture = Fixture::shared();
    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("approval-merge");
    let mut seed = fixture.shell();
    seed["users"] = json!([fixture.user("u1")]);
    let mut pending = fixture.approval("u1", MBID);
    pending["batch_id"] = json!("batch-old");
    seed["approvals"] = json!([pending]);
    run_import(import_request(
        &seed,
        pool.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;

    let mut upgrade = fixture.shell();
    upgrade["users"] = json!([fixture.user("u1")]);
    let mut approved = fixture.approval("u1", MBID);
    approved["state"] = json!("approved");
    approved["batch_id"] = json!("batch-new");
    approved["source"] = json!("bulk");
    upgrade["approvals"] = json!([approved]);
    let report = run_import(import_request(&upgrade, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    let row: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT state, batch_id, source FROM auto_download_approvals WHERE user_id = 'u1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "approved".to_owned(),
            Some("batch-new".to_owned()),
            Some("bulk".to_owned())
        ),
        "winning state carries its batch and source"
    );
}

#[tokio::test]
async fn dangling_reviewer_nulled_approval_survives() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let mut approval = fixture.approval("u1", MBID);
    approval["reviewed_by_id"] = json!("ghost");
    approval["reviewed_by_name"] = json!("Ghost");
    export["approvals"] = json!([approval]);

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("reviewer");
    let report = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["approval"].imported, 1);
    assert_eq!(report.entities["approval"].nulled_field, 1);
    let reviewer: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals WHERE user_id = 'u1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        reviewer,
        (None, Some("Ghost".to_owned())),
        "the id is nulled, the name stays as audit residue"
    );
}

#[tokio::test]
async fn dangling_user_refs_fail_the_whole_file() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["follows"] = json!([fixture.follow("ghost", MBID)]);
    export["approvals"] = json!([fixture.approval("ghost", MBID)]);

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("deleted-ids");
    let report = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::FailedValidation);
    assert_eq!(count(&pool, "auth_users").await, 0);
    assert_eq!(count(&pool, "user_followed_artists").await, 0);
    assert_eq!(count(&pool, "auto_download_approvals").await, 0);
    assert_eq!(count(&pool, "import_runs").await, 0);
}

#[tokio::test]
async fn reimport_is_identical_with_zero_writes() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["follows"] = json!([fixture.follow("u1", MBID)]);
    export["approvals"] = json!([fixture.approval("u1", MBID)]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let pool = migrated_pool().await;
    let (_dir, config_path, _crypto) = scratch_config("idempotent");
    let first = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;
    assert_eq!(first.exit.code, ExitCode::Ok);
    let config_before = settings_rows(&pool).await;
    let users_before = count(&pool, "auth_users").await;
    let runs_before = count(&pool, "import_runs").await;

    let crypto2 = test_crypto();
    let second = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto2,
    ))
    .await;

    assert_eq!(second.exit.code, ExitCode::Ok);
    for entity in [
        "user",
        "provider",
        "app_password",
        "recovery_code",
        "follow",
        "approval",
    ] {
        let found = &second.entities[entity];
        assert_eq!(found.imported, 0, "{entity} must not re-import");
        assert!(found.skipped_identical > 0, "{entity} must skip identical");
        assert_eq!(found.conflict_kept_existing, 0);
        assert_eq!(found.error, 0);
    }
    assert_eq!(second.secrets_reencrypted, 0);
    assert_eq!(second.settings_applied, first.settings_applied);
    assert_eq!(second.settings_defaulted, first.settings_defaulted);
    assert_eq!(count(&pool, "auth_users").await, users_before);
    assert_eq!(count(&pool, "import_runs").await, runs_before);
    assert_eq!(
        settings_rows(&pool).await,
        config_before,
        "no settings row may change on re-import"
    );
}

#[tokio::test]
async fn conflicted_reimport_converges_with_zero_writes() {
    // A nulled-then-NULL email never compares identical, so conflicted
    // re-imports report `conflict_kept_existing` (not `skipped_identical`)
    // and recount the ghost reviewer every run, but they converge: no
    // row changes, no settings row changes, no audit row, repeat runs agree.
    let fixture = Fixture::shared();
    let mut seed = fixture.shell();
    seed["users"] = json!([fixture.user("u1")]);
    seed["settings"]["wanted"] = json!({"enabled": true});
    let pool = migrated_pool().await;
    let (_dir, config_path, _) = scratch_config("conflict-converge");
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
    let mut approval = fixture.approval("u2", MBID);
    approval["reviewed_by_id"] = json!("ghost");
    clash["approvals"] = json!([approval]);
    clash["settings"]["wanted"] = json!({"enabled": true});
    let first = run_import(import_request(
        &clash,
        pool.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;
    assert_eq!(first.exit.code, ExitCode::OkWithDrops);
    assert_eq!(first.entities["user"].nulled_field, 1);
    assert_eq!(first.entities["approval"].nulled_field, 1);

    let users_before = count(&pool, "auth_users").await;
    let approvals_before = count(&pool, "auto_download_approvals").await;
    let runs_before = count(&pool, "import_runs").await;
    let config_before = settings_rows(&pool).await;

    for _ in 0..2 {
        let again = run_import(import_request(
            &clash,
            pool.clone(),
            config_path.clone(),
            test_crypto(),
        ))
        .await;
        assert_eq!(again.exit.code, ExitCode::OkWithDrops);
        assert_eq!(again.entities["user"].conflict_kept_existing, 1);
        assert_eq!(again.entities["user"].skipped_identical, 0);
        assert_eq!(again.entities["approval"].skipped_identical, 1);
        assert_eq!(again.entities["approval"].nulled_field, 1);
        for entity in [
            "user",
            "provider",
            "app_password",
            "recovery_code",
            "follow",
            "approval",
        ] {
            assert_eq!(
                again.entities[entity].imported, 0,
                "{entity} wrote nothing twice"
            );
        }
        assert_eq!(count(&pool, "auth_users").await, users_before);
        assert_eq!(
            count(&pool, "auto_download_approvals").await,
            approvals_before
        );
        assert_eq!(count(&pool, "import_runs").await, runs_before);
        assert_eq!(
            settings_rows(&pool).await,
            config_before,
            "settings rows stay stable across conflicted re-imports"
        );
    }
}

#[tokio::test]
async fn bare_plugin_strings_pass_through_verbatim() {
    // Plugin secret positions live only in the v2 manifest, so the
    // importer cannot tell a plaintext plugin secret from a plaintext
    // plugin setting: bare strings land verbatim, unencrypted.
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["plugins"] = json!({
        "myplug": {"enabled": true, "settings": {"token": "bare-secret"}}
    });

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("plugin-passthrough");
    let report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::Ok);
    let stored: Value = stored_settings(&pool).await;
    assert_eq!(
        stored["plugins"]["myplug"]["settings"]["token"],
        json!("bare-secret")
    );
}

#[tokio::test]
async fn app_password_name_collision_disambiguates() {
    let fixture = Fixture::shared();
    let mut seed = fixture.shell();
    seed["users"] = json!([fixture.user("u1")]);
    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("app-pw-name");
    run_import(import_request(
        &seed,
        pool.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;

    let mut clash = fixture.shell();
    let mut second = fixture.user("u1");
    second["app_passwords"][0]["created_at"] = json!("2026-05-01T00:00:00+00:00");
    second["app_passwords"][0]["secret"] = fixture.seal("a-different-secret");
    clash["users"] = json!([second]);
    let report = run_import(import_request(&clash, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["app_password"].conflict_kept_existing, 1);
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM connect_app_passwords WHERE user_id = 'u1' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(names.len(), 2);
    assert!(names.iter().any(|name| name.contains("2026-05-01")));
}

#[tokio::test]
async fn settings_replace_filters_and_preserves() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["wanted"] = json!({"enabled": true});
    export["settings"]["lastfm_settings"] =
        json!({"enabled": true, "api_key": fixture.seal("lastfm-key")});
    export["settings"]["musicbrainz_settings"] = json!({
        "source_mode": "official",
        "pending_brainzmash": {"x": 1},
        "source_quarantined": true,
        "quarantine_reason": "flaky",
    });
    export["settings"]["plugins"] = json!({
        "demo": {"enabled": true, "settings": {"k": "v"}, "junk": 1},
    });
    export["settings"]["indexers"] =
        json!([{"name": "first", "api_key": fixture.seal("idx-1")}, {"name": "second"}]);
    export["settings"]["advanced_settings"] = json!({"audiodb_api_key": fixture.seal("audio-key")});
    export["settings"]["spotify_settings"] =
        json!({"client_id": "v2-app", "client_secret": fixture.seal("sp-secret"), "enabled": true});

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("settings");
    std::fs::write(
        &config_path,
        r#"{"lyrics_settings": {"enabled": true}, "wanted": {"enabled": false}}"#,
    )
    .unwrap();
    let report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::Ok);
    assert!(report.settings_applied.contains(&"wanted".to_owned()));
    assert!(report.settings_defaulted.contains(&"events".to_owned()));
    assert!(
        !report
            .settings_defaulted
            .contains(&"lyrics_settings".to_owned())
    );
    let stored: Value = stored_settings(&pool).await;
    assert_eq!(stored["wanted"], json!({"enabled": true}));
    assert_eq!(stored["lyrics_settings"], json!({"enabled": true}));
    // The instance Last.fm key carries over, re-sealed under the v3 key.
    assert_eq!(stored["lastfm_settings"]["enabled"], json!(true));
    assert!(
        stored["lastfm_settings"]["api_key"]
            .as_str()
            .is_some_and(|cipher| cipher.starts_with("v3:"))
    );
    assert!(
        stored["musicbrainz_settings"]
            .get("pending_brainzmash")
            .is_none()
    );
    assert!(
        stored["musicbrainz_settings"]
            .get("quarantine_reason")
            .is_none()
    );
    assert_eq!(
        stored["plugins"]["demo"],
        json!({"enabled": false, "settings": {"k": "v"}})
    );
    assert!(report.items.iter().any(|item| item.key == "plugins.demo"
        && item.outcome == "needs_porting"
        && item.detail == "demo needs porting before it can run"));
    // A Spotify app configured on v2 keeps its registered v2 callback.
    assert_eq!(stored["spotify_settings"]["legacy_callback"], json!(true));
    assert_eq!(stored["indexers"][0]["name"], json!("first"));
    assert_eq!(stored["indexers"][1]["name"], json!("second"));
    let cipher = stored["indexers"][0]["api_key"].as_str().unwrap();
    assert!(cipher.starts_with("v3:"));
    let text = serde_json::to_string(&stored).unwrap();
    assert!(!text.contains("lastfm-key"));
    assert!(!text.contains("idx-1"));
    assert!(!text.contains("audio-key"));
}

#[tokio::test]
async fn schedule_carry_moves_sync_frequency_once() {
    let fixture = Fixture::shared();
    let export = fixture.shell();
    let pool = migrated_pool().await;
    let (dir, config_path, crypto) = scratch_config("schedule-carry");
    let v2_config = dir.join("v2-config.json");
    std::fs::write(
        &v2_config,
        r#"{"library_sync_settings": {"sync_frequency": "6hr"}}"#,
    )
    .unwrap();

    let mut request = import_request(&export, pool.clone(), config_path.clone(), test_crypto());
    request.v2_config_path = Some(v2_config.clone());
    let report = run_import(request).await;
    assert_eq!(report.exit.code, ExitCode::Ok);
    let stored: Value = stored_settings(&pool).await;
    assert_eq!(
        stored["library_scan_schedule"]["scan_frequency"],
        json!("6hr")
    );

    // Re-import converges instead of flip-flopping: the carry is
    // judged on the export alone, so the same value lands again.
    let mut again = import_request(&export, pool.clone(), config_path.clone(), crypto);
    again.v2_config_path = Some(v2_config);
    run_import(again).await;
    let kept: Value = stored_settings(&pool).await;
    assert_eq!(
        kept["library_scan_schedule"]["scan_frequency"],
        json!("6hr")
    );

    // The export wins whenever it carries the section.
    let mut with_schedule = fixture.shell();
    with_schedule["settings"]["library_scan_schedule"] =
        json!({"scan_frequency": "daily", "daily_scan_time": "03:00"});
    let pool2 = migrated_pool().await;
    let (_dir2, config2, crypto2) = scratch_config("schedule-carry-export-wins");
    let mut direct = import_request(&with_schedule, pool2.clone(), config2.clone(), crypto2);
    direct.v2_config_path = Some(dir.join("v2-config.json"));
    run_import(direct).await;
    let direct_stored = stored_settings(&pool2).await;
    assert_eq!(
        direct_stored["library_scan_schedule"]["scan_frequency"],
        json!("daily")
    );
}

#[tokio::test]
async fn schedule_carry_ignores_unknown_values() {
    assert_eq!(
        droppedneedle_tool::import::schedule_carry::read_sync_frequency(&json!({
            "library_sync_settings": {"sync_frequency": "fortnightly"}
        })),
        None
    );
    assert_eq!(
        droppedneedle_tool::import::schedule_carry::read_sync_frequency(&json!({
            "library_sync_settings": {"sync_frequency": "12hr"}
        })),
        Some("12hr".to_owned())
    );
}

#[tokio::test]
async fn reused_secret_keeps_first_registration() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    let first = fixture.user("u1");
    let mut second = fixture.user("u2");
    second["app_passwords"][0]["secret"] = first["app_passwords"][0]["secret"].clone();
    export["users"] = json!([first, second]);

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("reused-secret");
    let report = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["app_password"].imported, 1);
    assert_eq!(report.entities["app_password"].conflict_kept_existing, 1);
    assert_eq!(count(&pool, "auth_users").await, 2);
    assert_eq!(count(&pool, "connect_app_passwords").await, 1);
}

#[tokio::test]
async fn report_shape_matches_spec() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("shape");
    let report = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;

    let parsed: Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(parsed["format"], json!("droppedneedle-import-report"));
    assert_eq!(parsed["format_version"], json!(2));
    assert_eq!(parsed["dry_run"], json!(false));
    // Every counter exists even when zero; nonzero `dropped_unknown_user`
    // is unreachable through `run_import` (fail closed), and `error` has
    // no producer, so presence is what this pins.
    for entity in [
        "user",
        "provider",
        "app_password",
        "recovery_code",
        "follow",
        "approval",
        "connection",
        "playlist",
        "play_history",
        "download_task",
    ] {
        for counter in [
            "imported",
            "skipped_identical",
            "conflict_kept_existing",
            "dropped_unknown_user",
            "dropped_invalid",
            "nulled_field",
            "error",
        ] {
            assert!(
                parsed["entities"][entity][counter].is_number(),
                "{entity}.{counter} must be present"
            );
        }
    }
    assert!(parsed["pending_links"].is_number());
    assert!(parsed["left_behind"].is_array());
    assert_eq!(parsed["export_file"]["instance_id"], json!("instance-1"));
    assert!(parsed["items"].as_array().unwrap().iter().all(|item| {
        item["entity"].is_string() && item["key"].is_string() && item["outcome"].is_string()
    }));
}

#[tokio::test]
async fn pending_import_reopens_a_rejected_row() {
    // Most-permissive ordering, pinned: pending outranks rejected, so a
    // pending import reopens the decision instead of keeping it.
    let fixture = Fixture::shared();
    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("rejected-reopen");
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('u1', 'U', 'user', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, state, requested_at) VALUES ('u1', ?, ?, 'Test Artist', 'rejected', 200000)",
    )
    .bind(MBID)
    .bind(MBID.to_lowercase())
    .execute(&pool)
    .await
    .unwrap();

    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let mut pending = fixture.approval("u1", MBID);
    pending["requested_at"] = json!(100.0);
    export["approvals"] = json!([pending]);
    let report = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    let row: (String, i64) = sqlx::query_as(
        "SELECT state, requested_at FROM auto_download_approvals WHERE user_id = 'u1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, ("pending".to_owned(), 100_000));
}

#[tokio::test]
async fn rejected_and_unknown_states_never_win_a_merge() {
    let fixture = Fixture::shared();
    let pool = migrated_pool().await;
    let (_dir, config_path, _crypto) = scratch_config("rejected-kept");
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('u1', 'U', 'user', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, state, requested_at) VALUES ('u1', ?, ?, 'Test Artist', 'pending', 100000)",
    )
    .bind(MBID)
    .bind(MBID.to_lowercase())
    .execute(&pool)
    .await
    .unwrap();

    for state in ["rejected", "archived"] {
        let mut export = fixture.shell();
        export["users"] = json!([fixture.user("u1")]);
        let mut incoming = fixture.approval("u1", MBID);
        incoming["state"] = json!(state);
        incoming["requested_at"] = json!(50.0);
        export["approvals"] = json!([incoming]);
        let report = run_import(import_request(
            &export,
            pool.clone(),
            config_path.clone(),
            test_crypto(),
        ))
        .await;
        assert_eq!(report.entities["approval"].imported, 0);
        let row: (String, i64) = sqlx::query_as(
            "SELECT state, requested_at FROM auto_download_approvals WHERE user_id = 'u1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            row,
            ("pending".to_owned(), 50_000),
            "state {state} must not win, earliest request still applies"
        );
    }
}

#[tokio::test]
async fn valid_reviewer_survives_verbatim() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1"), fixture.user("u2")]);
    let mut approval = fixture.approval("u1", MBID);
    approval["reviewed_by_id"] = json!("u2");
    approval["reviewed_by_name"] = json!("User u2");
    export["approvals"] = json!([approval]);

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("reviewer-kept");
    let report = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::Ok);
    assert_eq!(report.entities["approval"].nulled_field, 0);
    let reviewer: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals WHERE user_id = 'u1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        reviewer,
        (Some("u2".to_owned()), Some("User u2".to_owned()))
    );
}

#[tokio::test]
async fn unnamed_app_password_drops_counted() {
    // The one reachable `dropped_invalid` path: an empty name passes
    // validation (no rule covers it) and drops at import time.
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    let mut user = fixture.user("u1");
    user["app_passwords"][0]["name"] = json!("");
    export["users"] = json!([user]);

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("unnamed-secret");
    let report = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["app_password"].dropped_invalid, 1);
    assert_eq!(report.entities["app_password"].imported, 0);
    assert_eq!(count(&pool, "connect_app_passwords").await, 0);
    assert_eq!(count(&pool, "auth_users").await, 1);
}

#[tokio::test]
async fn unknown_settings_section_with_corrupt_blob_is_ignored() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["future_section"] = json!({"api_key": {"$sealed": "!!!not-b64!!!"}});

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("unknown-section");
    let report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::Ok);
    assert!(
        report
            .items
            .iter()
            .any(|item| item.detail.contains("UNKNOWN_SETTINGS_SECTION")),
        "the warning must ride along in the report"
    );
    let stored: Value = stored_settings(&pool).await;
    assert!(stored.get("future_section").is_none());
}

#[tokio::test]
async fn schedule_carry_instance_mismatch_refuses_with_zero_writes() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let pool = migrated_pool().await;
    let (dir, config_path, crypto) = scratch_config("schedule-carry-mismatch");
    let v2_config = dir.join("v2-config.json");
    std::fs::write(
        &v2_config,
        r#"{"instance_id": "someone-else", "library_sync_settings": {"sync_frequency": "6hr"}}"#,
    )
    .unwrap();

    let mut request = import_request(&export, pool.clone(), config_path.clone(), crypto);
    request.v2_config_path = Some(v2_config);
    let report = run_import(request).await;

    assert_eq!(report.exit.code, ExitCode::FailedValidation);
    assert!(report.exit.message.contains("INSTANCE_MISMATCH"));
    assert_eq!(count(&pool, "auth_users").await, 0);
    assert_eq!(count(&pool, "import_runs").await, 0);
    assert!(settings_rows(&pool).await.is_empty());
}

#[test]
fn schedule_carry_missing_file_skips_silently() {
    let (dir, _, _) = scratch_config("schedule-carry-missing");
    let missing = dir.join("no-such-config.json");
    assert_eq!(
        droppedneedle_tool::import::schedule_carry::carry_frequency(&missing, false, "instance-1"),
        Ok(None)
    );
}

#[test]
fn schedule_carry_corrupt_file_fails() {
    let (dir, _, _) = scratch_config("schedule-carry-corrupt");
    let path = dir.join("v2-config.json");
    std::fs::write(&path, "{not json").unwrap();
    let error =
        droppedneedle_tool::import::schedule_carry::carry_frequency(&path, false, "instance-1")
            .unwrap_err();
    assert!(matches!(
        error,
        droppedneedle_tool::import::schedule_carry::ScheduleCarryError::InvalidJson { .. }
    ));
}

#[tokio::test]
async fn reimport_with_secrets_leaves_settings_rows_untouched() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["jellyfin_settings"] =
        json!({"api_key": fixture.seal("jelly-secret"), "jellyfin_url": "http://jelly:8096"});

    let pool = migrated_pool().await;
    let (_dir, config_path, _crypto) = scratch_config("secret-stable");
    let first = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;
    assert_eq!(first.exit.code, ExitCode::Ok);
    let rows_before = settings_rows(&pool).await;

    let second = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;
    assert_eq!(second.exit.code, ExitCode::Ok);
    assert_eq!(
        settings_rows(&pool).await,
        rows_before,
        "fresh nonces must not rewrite a row"
    );
}

/// Every managed field name v2 accepted.
const V2_MANAGED_FIELDS: &[&str] = &[
    "album",
    "title",
    "album_sort",
    "title_sort",
    "artist",
    "album_artist",
    "artist_sort",
    "album_artist_sort",
    "track_number",
    "total_tracks",
    "disc_number",
    "total_discs",
    "disc_subtitle",
    "date",
    "original_date",
    "release_status",
    "release_country",
    "release_type",
    "media",
    "compilation",
    "label",
    "catalog_number",
    "barcode",
    "asin",
    "isrc",
    "musicbrainz_recording_id",
    "musicbrainz_release_track_id",
    "musicbrainz_release_id",
    "musicbrainz_release_group_id",
    "musicbrainz_artist_id",
    "musicbrainz_album_artist_id",
    "work",
    "musicbrainz_work_id",
    "movement",
    "movement_number",
    "movement_count",
    "composer",
    "lyricist",
    "conductor",
    "performer",
    "arranger",
    "remixer",
    "producer",
    "acoustid_id",
    "acoustid_fingerprint",
];

/// A v2 profile carries over whole: every entry keeps its mode and its
/// clear, a field it did not list stays off, `artists` and
/// `album_artists` follow `artist` and `album_artist`, preserved names
/// match in any case and cover nothing else, and the section reads.
#[tokio::test]
async fn v2_management_fields_carry_over_as_they_were() {
    use droppedneedle_kernel::runtime_config::sections::FieldMode;
    use droppedneedle_library::management::settings::service::LibraryManagementService;
    use droppedneedle_library::management::settings::{
        PICARD_ORGANIZER_PROFILE_ID, initial_settings,
    };
    use std::sync::Arc;

    let mut management = serde_json::to_value(initial_settings("")).unwrap();
    let fields: Vec<Value> = V2_MANAGED_FIELDS
        .iter()
        .filter(|name| **name != "artist_sort")
        .map(|name| {
            let mode = match *name {
                "artist" => "merge",
                "title_sort" => "disabled",
                _ => "replace",
            };
            let clear = *name == "album_sort";
            json!({"field": name, "mode": mode, "clear_when_canonical_missing": clear})
        })
        .collect();
    let profiles = management["profiles"].as_array_mut().unwrap();
    let picard = profiles
        .iter_mut()
        .find(|profile| profile["id"] == PICARD_ORGANIZER_PROFILE_ID)
        .unwrap();
    picard["metadata"]["fields"] = Value::Array(fields);
    picard["metadata"]["preserve_fields"] = json!(["Artist", "album", "date"]);

    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["library_management"] = management;
    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("management-fields");
    let report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;
    assert_eq!(report.exit.code, ExitCode::Ok, "{}", report.to_json());
    assert!(
        !report
            .items
            .iter()
            .any(|item| item.key.contains(".metadata.fields")),
        "{}",
        report.to_json()
    );

    let store = Arc::new(settings_store(&pool, &config_path).await);
    let service = LibraryManagementService::new(store);
    let settings = service.get_settings().expect("the section reads").settings;
    let picard = settings
        .profiles
        .iter()
        .find(|profile| profile.id == PICARD_ORGANIZER_PROFILE_ID)
        .unwrap();
    let mode = |name: &str| {
        picard
            .metadata
            .fields
            .iter()
            .find(|field| field.field == name)
            .map(|field| field.mode)
    };
    let album_sort = picard
        .metadata
        .fields
        .iter()
        .find(|field| field.field == "album_sort")
        .unwrap();
    assert_eq!(album_sort.mode, FieldMode::Replace);
    assert!(album_sort.clear_when_canonical_missing, "v2's clear stays");
    assert_eq!(mode("title_sort"), Some(FieldMode::Disabled));
    assert_eq!(mode("artist"), Some(FieldMode::Merge));
    assert_eq!(mode("artists"), Some(FieldMode::Merge));
    assert_eq!(mode("album_artists"), Some(FieldMode::Replace));
    assert_eq!(
        mode("artist_sort"),
        Some(FieldMode::Disabled),
        "v2 left it off"
    );
    assert_eq!(mode("comment"), Some(FieldMode::Disabled));
    let mut preserved = picard.metadata.preserve_fields.clone();
    preserved.sort();
    assert_eq!(preserved, ["album", "artist", "artists", "date"]);
}

/// A v2 profile that ticked Other gets the five Other role fields as
/// replace with clear off, and one that did not gets them off. v2 never
/// listed these fields, so the carry leaves them to completion.
#[tokio::test]
async fn v2_profile_that_ticked_other_gets_the_role_fields() {
    use droppedneedle_kernel::runtime_config::sections::FieldMode;
    use droppedneedle_library::management::settings::service::LibraryManagementService;
    use droppedneedle_library::management::settings::{
        COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID, PICARD_ORGANIZER_PROFILE_ID, initial_settings,
    };
    use std::sync::Arc;

    let mut management = serde_json::to_value(initial_settings("")).unwrap();
    let fields: Vec<Value> = V2_MANAGED_FIELDS
        .iter()
        .map(
            |name| json!({"field": name, "mode": "replace", "clear_when_canonical_missing": false}),
        )
        .collect();
    let profiles = management["profiles"].as_array_mut().unwrap();
    let picard = profiles
        .iter_mut()
        .find(|profile| profile["id"] == PICARD_ORGANIZER_PROFILE_ID)
        .unwrap();
    picard["metadata"]["fields"] = Value::Array(fields.clone());
    picard["metadata"]["relationships"] = json!({"enabled": true, "types": ["composer", "other"]});
    let complete = profiles
        .iter_mut()
        .find(|profile| profile["id"] == COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID)
        .unwrap();
    complete["metadata"]["fields"] = Value::Array(fields);
    complete["metadata"]["relationships"] = json!({"enabled": true, "types": ["composer"]});

    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["library_management"] = management;
    let (_dir, config_path, crypto) = scratch_config("management-other-roles");
    let pool = migrated_pool().await;
    let report = run_import(import_request(
        &export,
        pool.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;
    assert_eq!(report.exit.code, ExitCode::Ok, "{}", report.to_json());

    let store = Arc::new(settings_store(&pool, &config_path).await);
    let service = LibraryManagementService::new(store);
    let settings = service.get_settings().expect("the section reads").settings;
    let state = |profile_id: &str, field: &str| {
        let profile = settings
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .unwrap();
        let entry = profile
            .metadata
            .fields
            .iter()
            .find(|entry| entry.field == field)
            .unwrap();
        (entry.mode, entry.clear_when_canonical_missing)
    };
    for name in ["engineer", "mixer", "dj_mixer", "writer", "director"] {
        assert_eq!(
            state(PICARD_ORGANIZER_PROFILE_ID, name),
            (FieldMode::Replace, false),
            "{name} on a profile that ticked Other"
        );
        assert_eq!(
            state(COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID, name),
            (FieldMode::Disabled, false),
            "{name} on a profile that did not"
        );
    }
}

/// A v2 export carrying settings v3 removed imports, never refused: the
/// report names each removed setting as dropped, the fields kept beside
/// them land, and the stored download policy still decodes.
#[tokio::test]
async fn removed_settings_import_and_are_reported() {
    use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;

    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["advanced_settings"] = json!({
        "discover_queue_size": 12,
        "http_timeout": 30,
        "batch_albums": 4,
        "cache_ttl_album_library": 86400,
        "frontend_ttl_home": 300000,
    });
    export["settings"]["download_policy"] = json!({
        "flac_mp3_only": true,
        "verify_downloads": true,
        "quality_recipe": [
            {"format": "mp3", "quality": "custom", "min_bitrate_kbps": 192,
             "target_bitrate_kbps": 256, "max_bitrate_kbps": 320},
        ],
    });
    export["settings"]["download_clients"] =
        json!({"sabnzbd": {"url": "http://sab:8080", "post_processing": 3}});
    export["settings"]["indexers"] =
        json!([{"name": "one", "url": "https://one.example", "type": "newznab"}]);
    export["settings"]["_internal"] =
        json!({"plex_client_id": "plex-1", "brainzmash_consent_admin": "u1"});
    export["settings"]["scrobble_settings"] = json!({"scrobble_to_lastfm": true});

    let pool = migrated_pool().await;
    let (_dir, config_path, crypto) = scratch_config("removed");
    let report = run_import(import_request(&export, pool.clone(), config_path, crypto)).await;

    assert_eq!(report.exit.code, ExitCode::Ok, "{}", report.to_json());
    let mut dropped: Vec<&str> = report
        .items
        .iter()
        .filter(|item| item.entity == "settings" && item.outcome == "dropped_setting")
        .map(|item| item.key.as_str())
        .collect();
    dropped.sort_unstable();
    assert_eq!(
        dropped,
        [
            "_internal.brainzmash_consent_admin",
            "advanced_settings.batch_albums",
            "advanced_settings.cache_ttl_album_library",
            "advanced_settings.frontend_ttl_home",
            "advanced_settings.http_timeout",
            "download_clients.sabnzbd.post_processing",
            "download_policy.quality_recipe[0].target_bitrate_kbps",
            "download_policy.verify_downloads",
            "indexers[0].type",
            "scrobble_settings",
        ]
    );

    let stored = stored_settings(&pool).await;
    assert_eq!(
        stored["advanced_settings"],
        json!({"discover_queue_size": 12})
    );
    assert_eq!(stored["_internal"], json!({"plex_client_id": "plex-1"}));
    assert!(stored.get("scrobble_settings").is_none());
    let policy: DownloadPolicy =
        serde_json::from_value(stored["download_policy"].clone()).expect("the policy decodes");
    assert_eq!(policy.quality_recipe[0].max_bitrate_kbps, Some(320));
}
