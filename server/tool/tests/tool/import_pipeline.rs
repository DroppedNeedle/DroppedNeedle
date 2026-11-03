//! Pipeline tests: conflicts, deleted IDs, idempotency,
//! settings replace, and the one-shot scan schedule carry.
//!
//! Every test imports into scratch state only: in-memory databases
//! and temp-dir config files.

use crate::common::Probe;
use crate::import_support as support;

use serde_json::{Value, json};
use support::{
    BCRYPT_HASH, Fixture, MBID, count, import_request, migrated_file, scratch_config,
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("fresh");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::Ok);
    assert_eq!(counts(&report, "user"), (1, 0, 0, 0));
    assert_eq!(counts(&report, "provider"), (1, 0, 0, 0));
    assert_eq!(counts(&report, "app_password"), (1, 0, 0, 0));
    assert_eq!(counts(&report, "follow"), (1, 0, 0, 0));
    assert_eq!(counts(&report, "approval"), (1, 0, 0, 0));
    assert_eq!(report.secrets_reencrypted, 2);
    assert!(
        report
            .settings_applied
            .contains(&"jellyfin_settings".to_owned())
    );
    assert!(report.settings_defaulted.contains(&"wanted".to_owned()));

    let name: String = target_db.one("SELECT display_name FROM auth_users WHERE id = 'u1'", []);
    assert_eq!(name, "User u1");
    let local: (Option<String>, String, String) = target_db.row(
        "SELECT provider_data, hash_scheme, password_hash FROM auth_providers \
         WHERE provider_uid = 'local:u1'",
        [],
    );
    assert_eq!(
        local,
        (None, "bcrypt".to_owned(), BCRYPT_HASH.to_owned()),
        "bcrypt hash must survive verbatim"
    );
    let stored: String = target_db.one(
        "SELECT secret_encrypted FROM auth_app_passwords WHERE name = 'phone'",
        [],
    );
    assert!(
        stored.starts_with("v3:"),
        "app secret must be v3 ciphertext"
    );
    assert_eq!(count(&target_db, "auth_password_recovery_codes"), 0);
    assert!(
        report.items.iter().any(|item| item.outcome == "not_carried"
            && item.detail == "recovery codes from v2 were not carried; make a new one"),
        "the report says so"
    );
    let runs: i64 = target_db.one("SELECT COUNT(*) FROM import_runs", []);
    assert_eq!(runs, 1);

    let stored_config = stored_settings(&target_db);
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("provider-first");
    let first_report = run_import(import_request(
        &export,
        target_db.clone(),
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
    let report = run_import(import_request(
        &clash,
        target_db.clone(),
        config_path,
        crypto2,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["provider"].conflict_kept_existing, 1);
    assert_eq!(report.entities["provider"].imported, 0);
    let owner: String = target_db.one(
        "SELECT user_id FROM auth_providers WHERE provider = 'local' AND provider_uid = 'local:u1'",
        [],
    );
    assert_eq!(owner, "u1");
    let _ = crypto;
}

#[tokio::test]
async fn email_and_username_collisions_null_the_field() {
    let fixture = Fixture::shared();
    let mut seed = fixture.shell();
    seed["users"] = json!([fixture.user("u1")]);
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("nulling");
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
    second["username"] = json!("u1");
    clash["users"] = json!([second]);
    let report = run_import(import_request(
        &clash,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["user"].imported, 1);
    assert_eq!(report.entities["user"].nulled_field, 2);
    let row: (Option<String>, Option<String>) =
        target_db.row("SELECT email, username FROM auth_users WHERE id = 'u2'", []);
    assert_eq!(
        row,
        (None, None),
        "colliding fields are nulled, account survives"
    );
}

#[tokio::test]
async fn follow_merge_never_downgrades() {
    let fixture = Fixture::shared();
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("follow-merge");
    target_db.exec(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('u1', 'U', 'user', 0)",
        [],
    );
    target_db.exec("INSERT INTO acquire_followed_artists (user_id, artist_mbid, artist_mbid_lower, artist_name, \
         auto_download, followed_at, updated_at) VALUES ('u1', ?, ?, 'Old Name', 1, 100000, 200000)", rusqlite::params![MBID, MBID.to_lowercase()]);

    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let mut follow = fixture.follow("u1", MBID);
    follow["auto_download"] = json!(false);
    follow["followed_at"] = json!(50.0);
    follow["updated_at"] = json!(300.0);
    export["follows"] = json!([follow]);
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["follow"].conflict_kept_existing, 1);
    assert_eq!(
        report.entities["user"].conflict_kept_existing, 1,
        "the seeded same-id user row stands"
    );
    let row: (i64, i64, i64, String) = target_db.row(
        "SELECT auto_download, followed_at, updated_at, artist_name FROM acquire_followed_artists \
         WHERE user_id = 'u1'",
        [],
    );
    assert_eq!(
        row,
        (1, 50_000, 300_000, "Old Name".to_owned()),
        "OR never downgrades, min/max win, existing name kept"
    );
}

#[tokio::test]
async fn approval_merge_most_permissive_wins() {
    let fixture = Fixture::shared();
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("approval-merge");
    let mut seed = fixture.shell();
    seed["users"] = json!([fixture.user("u1")]);
    let mut pending = fixture.approval("u1", MBID);
    pending["batch_id"] = json!("batch-old");
    seed["approvals"] = json!([pending]);
    run_import(import_request(
        &seed,
        target_db.clone(),
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
    let report = run_import(import_request(
        &upgrade,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    let row: (String, Option<String>, Option<String>) = target_db.row(
        "SELECT state, batch_id, source FROM auto_download_approvals WHERE user_id = 'u1'",
        [],
    );
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("reviewer");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["approval"].imported, 1);
    assert_eq!(report.entities["approval"].nulled_field, 1);
    let reviewer: (Option<String>, Option<String>) = target_db.row(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals WHERE user_id = 'u1'",
        [],
    );
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("deleted-ids");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::FailedValidation);
    assert_eq!(count(&target_db, "auth_users"), 0);
    assert_eq!(count(&target_db, "acquire_followed_artists"), 0);
    assert_eq!(count(&target_db, "auto_download_approvals"), 0);
    assert_eq!(count(&target_db, "import_runs"), 0);
}

#[tokio::test]
async fn reimport_is_identical_with_zero_writes() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["follows"] = json!([fixture.follow("u1", MBID)]);
    export["approvals"] = json!([fixture.approval("u1", MBID)]);
    export["settings"]["wanted"] = json!({"enabled": true});

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, _crypto) = scratch_config("idempotent");
    let first = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;
    assert_eq!(first.exit.code, ExitCode::Ok);
    let config_before = settings_rows(&target_db);
    let users_before = count(&target_db, "auth_users");
    let runs_before = count(&target_db, "import_runs");

    let crypto2 = test_crypto();
    let second = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        crypto2,
    ))
    .await;

    assert_eq!(second.exit.code, ExitCode::Ok);
    for entity in ["user", "provider", "app_password", "follow", "approval"] {
        let found = &second.entities[entity];
        assert_eq!(found.imported, 0, "{entity} must not re-import");
        assert!(found.skipped_identical > 0, "{entity} must skip identical");
        assert_eq!(found.conflict_kept_existing, 0);
        assert_eq!(found.error, 0);
    }
    assert_eq!(second.secrets_reencrypted, 0);
    assert_eq!(second.settings_applied, first.settings_applied);
    assert_eq!(second.settings_defaulted, first.settings_defaulted);
    assert_eq!(count(&target_db, "auth_users"), users_before);
    assert_eq!(count(&target_db, "import_runs"), runs_before);
    assert_eq!(
        settings_rows(&target_db),
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
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, _) = scratch_config("conflict-converge");
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
    let mut approval = fixture.approval("u2", MBID);
    approval["reviewed_by_id"] = json!("ghost");
    clash["approvals"] = json!([approval]);
    clash["settings"]["wanted"] = json!({"enabled": true});
    let first = run_import(import_request(
        &clash,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;
    assert_eq!(first.exit.code, ExitCode::OkWithDrops);
    assert_eq!(first.entities["user"].nulled_field, 1);
    assert_eq!(first.entities["approval"].nulled_field, 1);

    let users_before = count(&target_db, "auth_users");
    let approvals_before = count(&target_db, "auto_download_approvals");
    let runs_before = count(&target_db, "import_runs");
    let config_before = settings_rows(&target_db);

    for _ in 0..2 {
        let again = run_import(import_request(
            &clash,
            target_db.clone(),
            config_path.clone(),
            test_crypto(),
        ))
        .await;
        assert_eq!(again.exit.code, ExitCode::OkWithDrops);
        assert_eq!(again.entities["user"].conflict_kept_existing, 1);
        assert_eq!(again.entities["user"].skipped_identical, 0);
        assert_eq!(again.entities["approval"].skipped_identical, 1);
        assert_eq!(again.entities["approval"].nulled_field, 1);
        for entity in ["user", "provider", "app_password", "follow", "approval"] {
            assert_eq!(
                again.entities[entity].imported, 0,
                "{entity} wrote nothing twice"
            );
        }
        assert_eq!(count(&target_db, "auth_users"), users_before);
        assert_eq!(
            count(&target_db, "auto_download_approvals"),
            approvals_before
        );
        assert_eq!(count(&target_db, "import_runs"), runs_before);
        assert_eq!(
            settings_rows(&target_db),
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("plugin-passthrough");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::Ok);
    let stored: Value = stored_settings(&target_db);
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
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("app-pw-name");
    run_import(import_request(
        &seed,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;

    let mut clash = fixture.shell();
    let mut second = fixture.user("u1");
    second["app_passwords"][0]["created_at"] = json!("2026-05-01T00:00:00+00:00");
    second["app_passwords"][0]["secret"] = fixture.seal("a-different-secret");
    clash["users"] = json!([second]);
    let report = run_import(import_request(
        &clash,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["app_password"].conflict_kept_existing, 1);
    let names: Vec<String> = target_db.column(
        "SELECT name FROM auth_app_passwords WHERE user_id = 'u1' ORDER BY name",
        [],
    );
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
    export["settings"]["oidc_settings"] = json!({
        "enabled": true,
        "issuer": "https://sso.example.com",
        "client_id": "droppedneedle",
        "redirect_uri": "https://music.example.com/api/v1/auth/oidc/callback",
    });

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("settings");
    std::fs::write(
        &config_path,
        r#"{"lyrics_settings": {"enabled": true}, "wanted": {"enabled": false}}"#,
    )
    .unwrap();
    let report = run_import(import_request(
        &export,
        target_db.clone(),
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
    let stored: Value = stored_settings(&target_db);
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
    // v3 does not answer v2's single sign-on callback, so the address moves.
    let oidc_v3 = "https://music.example.com/api/v3/auth/oidc/callback";
    assert_eq!(stored["oidc_settings"]["redirect_uri"], json!(oidc_v3));
    assert!(
        report
            .notes
            .contains(&format!("Single sign-on now redirects to {oidc_v3}.")),
        "{:?}",
        report.notes
    );
    assert_eq!(stored["indexers"][0]["name"], json!("first"));
    assert_eq!(stored["indexers"][1]["name"], json!("second"));
    let cipher = stored["indexers"][0]["api_key"].as_str().unwrap();
    assert!(cipher.starts_with("v3:"));
    let text = serde_json::to_string(&stored).unwrap();
    assert!(!text.contains("lastfm-key"));
    assert!(!text.contains("idx-1"));
    assert!(!text.contains("audio-key"));
}

/// A single sign-on address that does not end in v2's callback path (a
/// proxy's own path) is kept as it is, and the summary says to set v3's.
#[tokio::test]
async fn single_sign_on_address_off_the_v2_path_is_kept() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    let proxied = "https://music.example.com/sso/cb";
    export["settings"]["oidc_settings"] = json!({
        "enabled": true,
        "issuer": "https://sso.example.com",
        "client_id": "droppedneedle",
        "redirect_uri": proxied,
    });
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("oidc-kept");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::Ok);
    let stored: Value = stored_settings(&target_db);
    assert_eq!(stored["oidc_settings"]["redirect_uri"], json!(proxied));
    let note = format!(
        "Single sign-on still redirects to {proxied}, v2's address. Enter v3's under \
         Settings > Security."
    );
    assert!(report.notes.contains(&note), "{:?}", report.notes);
}

#[tokio::test]
async fn schedule_carry_moves_sync_frequency_once() {
    let fixture = Fixture::shared();
    let export = fixture.shell();
    let (target_db, _target) = migrated_file();
    let (dir, config_path, crypto) = scratch_config("schedule-carry");
    let v2_config = dir.join("v2-config.json");
    std::fs::write(
        &v2_config,
        r#"{"library_sync_settings": {"sync_frequency": "6hr"}}"#,
    )
    .unwrap();

    let mut request = import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    );
    request.v2_config_path = Some(v2_config.clone());
    let report = run_import(request).await;
    assert_eq!(report.exit.code, ExitCode::Ok);
    let stored: Value = stored_settings(&target_db);
    assert_eq!(
        stored["library_scan_schedule"]["scan_frequency"],
        json!("6hr")
    );

    // Re-import converges instead of flip-flopping: the carry is
    // judged on the export alone, so the same value lands again.
    let mut again = import_request(&export, target_db.clone(), config_path.clone(), crypto);
    again.v2_config_path = Some(v2_config);
    run_import(again).await;
    let kept: Value = stored_settings(&target_db);
    assert_eq!(
        kept["library_scan_schedule"]["scan_frequency"],
        json!("6hr")
    );

    // The export wins whenever it carries the section.
    let mut with_schedule = fixture.shell();
    with_schedule["settings"]["library_scan_schedule"] =
        json!({"scan_frequency": "daily", "daily_scan_time": "03:00"});
    let (target_db2, _target2) = migrated_file();
    let (_dir2, config2, crypto2) = scratch_config("schedule-carry-export-wins");
    let mut direct = import_request(&with_schedule, target_db2.clone(), config2.clone(), crypto2);
    direct.v2_config_path = Some(dir.join("v2-config.json"));
    run_import(direct).await;
    let direct_stored = stored_settings(&target_db2);
    assert_eq!(
        direct_stored["library_scan_schedule"]["scan_frequency"],
        json!("daily")
    );
}

/// v3 has no 5 or 10 minute scan schedule. Either one in a v2 export or in
/// its `config.json` imports as 30 minutes, and the report says so for the
/// export's.
#[tokio::test]
async fn fast_scan_schedules_import_as_thirty_minutes() {
    let fixture = Fixture::shared();

    let mut export = fixture.shell();
    export["settings"]["library_scan_schedule"] =
        json!({"scan_frequency": "5min", "daily_scan_time": "04:00"});
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("schedule-fast-export");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;
    assert_eq!(report.exit.code, ExitCode::Ok);
    let stored = stored_settings(&target_db);
    assert_eq!(
        stored["library_scan_schedule"],
        json!({"scan_frequency": "30min", "daily_scan_time": "04:00"})
    );
    assert!(
        report
            .to_json()
            .contains("library_scan_schedule.scan_frequency"),
        "the report names the rewrite"
    );

    let export = fixture.shell();
    let (target_db, _target) = migrated_file();
    let (dir, config_path, crypto) = scratch_config("schedule-fast-carry");
    let v2_config = dir.join("v2-config.json");
    std::fs::write(
        &v2_config,
        r#"{"library_sync_settings": {"sync_frequency": "10min"}}"#,
    )
    .unwrap();
    let mut request = import_request(&export, target_db.clone(), config_path, crypto);
    request.v2_config_path = Some(v2_config);
    assert_eq!(run_import(request).await.exit.code, ExitCode::Ok);
    assert_eq!(
        stored_settings(&target_db)["library_scan_schedule"]["scan_frequency"],
        json!("30min")
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("reused-secret");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["app_password"].imported, 1);
    assert_eq!(report.entities["app_password"].conflict_kept_existing, 1);
    assert_eq!(count(&target_db, "auth_users"), 2);
    assert_eq!(count(&target_db, "auth_app_passwords"), 1);
}

#[tokio::test]
async fn report_shape_matches_spec() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("shape");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

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
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("rejected-reopen");
    target_db.exec(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('u1', 'U', 'user', 0)",
        [],
    );
    target_db.exec(
        "INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, state, requested_at) VALUES ('u1', ?, ?, 'Test Artist', 'rejected', 200000)",
        rusqlite::params![MBID, MBID.to_lowercase()],
    );

    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let mut pending = fixture.approval("u1", MBID);
    pending["requested_at"] = json!(100.0);
    export["approvals"] = json!([pending]);
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    let row: (String, i64) = target_db.row(
        "SELECT state, requested_at FROM auto_download_approvals WHERE user_id = 'u1'",
        [],
    );
    assert_eq!(row, ("pending".to_owned(), 100_000));
}

#[tokio::test]
async fn rejected_and_unknown_states_never_win_a_merge() {
    let fixture = Fixture::shared();
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, _crypto) = scratch_config("rejected-kept");
    target_db.exec(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('u1', 'U', 'user', 0)",
        [],
    );
    target_db.exec(
        "INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, state, requested_at) VALUES ('u1', ?, ?, 'Test Artist', 'pending', 100000)",
        rusqlite::params![MBID, MBID.to_lowercase()],
    );

    for state in ["rejected", "archived"] {
        let mut export = fixture.shell();
        export["users"] = json!([fixture.user("u1")]);
        let mut incoming = fixture.approval("u1", MBID);
        incoming["state"] = json!(state);
        incoming["requested_at"] = json!(50.0);
        export["approvals"] = json!([incoming]);
        let report = run_import(import_request(
            &export,
            target_db.clone(),
            config_path.clone(),
            test_crypto(),
        ))
        .await;
        assert_eq!(report.entities["approval"].imported, 0);
        let row: (String, i64) = target_db.row(
            "SELECT state, requested_at FROM auto_download_approvals WHERE user_id = 'u1'",
            [],
        );
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("reviewer-kept");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::Ok);
    assert_eq!(report.entities["approval"].nulled_field, 0);
    let reviewer: (Option<String>, Option<String>) = target_db.row(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals WHERE user_id = 'u1'",
        [],
    );
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("unnamed-secret");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::OkWithDrops);
    assert_eq!(report.entities["app_password"].dropped_invalid, 1);
    assert_eq!(report.entities["app_password"].imported, 0);
    assert_eq!(count(&target_db, "auth_app_passwords"), 0);
    assert_eq!(count(&target_db, "auth_users"), 1);
}

#[tokio::test]
async fn unknown_settings_section_with_corrupt_blob_is_ignored() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["settings"]["future_section"] = json!({"api_key": {"$sealed": "!!!not-b64!!!"}});

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("unknown-section");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
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
    let stored: Value = stored_settings(&target_db);
    assert!(stored.get("future_section").is_none());
}

#[tokio::test]
async fn schedule_carry_instance_mismatch_refuses_with_zero_writes() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let (target_db, _target) = migrated_file();
    let (dir, config_path, crypto) = scratch_config("schedule-carry-mismatch");
    let v2_config = dir.join("v2-config.json");
    std::fs::write(
        &v2_config,
        r#"{"instance_id": "someone-else", "library_sync_settings": {"sync_frequency": "6hr"}}"#,
    )
    .unwrap();

    let mut request = import_request(&export, target_db.clone(), config_path.clone(), crypto);
    request.v2_config_path = Some(v2_config);
    let report = run_import(request).await;

    assert_eq!(report.exit.code, ExitCode::FailedValidation);
    assert!(report.exit.message.contains("INSTANCE_MISMATCH"));
    assert_eq!(count(&target_db, "auth_users"), 0);
    assert_eq!(count(&target_db, "import_runs"), 0);
    assert!(settings_rows(&target_db).is_empty());
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

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, _crypto) = scratch_config("secret-stable");
    let first = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;
    assert_eq!(first.exit.code, ExitCode::Ok);
    let rows_before = settings_rows(&target_db);

    let second = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        test_crypto(),
    ))
    .await;
    assert_eq!(second.exit.code, ExitCode::Ok);
    assert_eq!(
        settings_rows(&target_db),
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

    let mut management = serde_json::to_value(initial_settings()).unwrap();
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
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("management-fields");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
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

    let store = Arc::new(settings_store(&target_db, &config_path));
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

    let mut management = serde_json::to_value(initial_settings()).unwrap();
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
    let (target_db, _target) = migrated_file();
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path.clone(),
        crypto,
    ))
    .await;
    assert_eq!(report.exit.code, ExitCode::Ok, "{}", report.to_json());

    let store = Arc::new(settings_store(&target_db, &config_path));
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

/// Every shape a v2 download policy can reach the importer in ends as a
/// recipe that keeps the range it accepted, and the report says what changed.
#[tokio::test]
async fn a_v2_policy_lands_as_a_recipe_with_its_range() {
    let fixture = Fixture::shared();
    let steps = |policy: &Value| -> Vec<String> {
        policy["quality_recipe"]
            .as_array()
            .expect("a recipe")
            .iter()
            .map(|step| format!("{} {}", step["format"], step["quality"]).replace('"', ""))
            .collect()
    };
    let import = async |name: &str, settings: Value| {
        let mut export = fixture.shell();
        for (section, value) in settings.as_object().expect("sections") {
            export["settings"][section] = value.clone();
        }
        let (target_db, _target) = migrated_file();
        let (_dir, config_path, crypto) = scratch_config(name);
        let report = run_import(import_request(
            &export,
            target_db.clone(),
            config_path,
            crypto,
        ))
        .await;
        assert_eq!(report.exit.code, ExitCode::Ok, "{}", report.to_json());
        let details: Vec<(String, String)> = report
            .items
            .iter()
            .filter(|item| item.entity == "settings" && item.key.starts_with("download_policy"))
            .map(|item| (item.key.clone(), item.detail.clone()))
            .collect();
        (
            stored_settings(&target_db)["download_policy"].clone(),
            details,
        )
    };

    // v2's preference order is kept when it fits the range. The settings a
    // recipe cannot hold are named once, only those changed from v2's default.
    let (policy, lines) = import(
        "policy-order",
        json!({"download_policy": {
            "quality_min": "mp3_192",
            "quality_max": "lossless",
            "quality_cutoff": "mp3_320",
            "flac_mp3_only": false,
            "quality_recipe": [],
            "quality_preference_order": ["mp3_320", "lossless", "mp3_256", "mp3_192"],
            "lossless_preference": "cd",
            "lossless_max_bit_depth": 16,
            "lossy_min_bitrate_kbps": null,
            "unknown_quality_behavior": "allow_as_fallback",
        }}),
    )
    .await;
    assert_eq!(
        steps(&policy),
        ["mp3 320_plus", "flac hi_res", "mp3 256_319", "mp3 192_255"]
    );
    assert_eq!(policy["quality_min"], "mp3_192");
    assert_eq!(policy["quality_max"], "lossless");
    assert_eq!(policy["quality_cutoff"], "mp3_320");
    assert!(policy.get("flac_mp3_only").is_none());
    assert!(policy.get("lossless_preference").is_none());
    let line = |key: &str| {
        lines
            .iter()
            .find(|(found, _)| found == key)
            .map(|(_, detail)| detail.clone())
    };
    assert!(line("download_policy.flac_mp3_only").is_some(), "{lines:?}");
    assert_eq!(
        line("download_policy.quality_settings").as_deref(),
        Some("these v2 settings are not applied: lossless_preference, lossless_max_bit_depth")
    );

    // An order that does not fit the range gives best first, and is named.
    // A recipe v3 cannot read gets the conversion of its range.
    let (policy, lines) = import(
        "policy-bad-recipe",
        json!({"download_policy": {
            "quality_min": "low",
            "quality_max": "mp3_256",
            "quality_recipe": [{"format": "flac", "quality": "lossless"}],
            "quality_preference_order": ["lossless", "low"],
        }}),
    )
    .await;
    assert_eq!(
        steps(&policy),
        ["mp3 256_319", "mp3 192_255", "mp3 below_192"]
    );
    assert_eq!(policy["quality_max"], "mp3_256");
    assert!(
        lines
            .iter()
            .any(|(key, detail)| key == "download_policy.quality_recipe"
                && detail.starts_with("v3 cannot read the v2 recipe")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|(_, detail)| detail.ends_with("not applied: quality_preference_order")),
        "{lines:?}"
    );

    // Both caps at CD quality, v2's seed for a new install, give FLAC up to CD
    // quality: only the settings the recipe does not hold are named.
    let (policy, lines) = import(
        "policy-capped",
        json!({"download_policy": {
            "quality_min": "mp3_192",
            "quality_max": "lossless",
            "flac_mp3_only": true,
            "quality_preference_order": ["lossless", "mp3_320", "mp3_256", "mp3_192"],
            "preferred_lossy_bitrate_kbps": 320,
            "lossless_preference": "cd",
            "lossless_max_bit_depth": 16,
            "lossless_max_sample_rate_hz": 48000,
            "unknown_quality_behavior": "review",
            "source_selection_mode": "source_first",
        }}),
    )
    .await;
    assert_eq!(
        steps(&policy),
        ["flac cd", "mp3 320_plus", "mp3 256_319", "mp3 192_255"]
    );
    assert_eq!(policy["quality_max"], "lossless");
    assert!(
        lines
            .iter()
            .any(|(key, detail)| key == "download_policy.quality_recipe"
                && detail.contains("the recipe FLAC, up to CD quality, then MP3 320_plus")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|(key, detail)| key == "download_policy.quality_settings"
                && detail.ends_with(
                    "not applied: preferred_lossy_bitrate_kbps, unknown_quality_behavior"
                )),
        "{lines:?}"
    );

    // A recipe keeps its own FLAC step, so the caps are named instead.
    let (policy, lines) = import(
        "policy-capped-recipe",
        json!({"download_policy": {
            "quality_recipe": [{"format": "flac", "quality": "hi_res"}],
            "lossless_max_bit_depth": 16,
            "lossless_max_sample_rate_hz": 48000,
        }}),
    )
    .await;
    assert_eq!(steps(&policy), ["flac hi_res"]);
    assert!(
        lines.iter().any(|(_, detail)| detail
            .ends_with("not applied: lossless_max_bit_depth, lossless_max_sample_rate_hz")),
        "{lines:?}"
    );

    // v2 ran on the range while other formats were allowed, recipe or not.
    let (policy, lines) = import(
        "policy-formats-open",
        json!({"download_policy": {
            "quality_min": "mp3_320",
            "quality_max": "mp3_320",
            "flac_mp3_only": false,
            "quality_recipe": [{"format": "flac", "quality": "cd"}],
        }}),
    )
    .await;
    assert_eq!(steps(&policy), ["mp3 320_plus"]);
    assert_eq!(policy["quality_max"], "mp3_320");
    assert!(
        lines
            .iter()
            .any(|(key, detail)| key == "download_policy.quality_recipe"
                && detail.starts_with("v2 did not use its recipe")),
        "{lines:?}"
    );

    // A recipe v3 can read stays, and sets the range.
    let recipe = json!([{"format": "mp3", "quality": "256_319"}]);
    let (policy, lines) = import(
        "policy-kept",
        json!({"download_policy": {"quality_min": "low", "quality_recipe": recipe}}),
    )
    .await;
    assert_eq!(steps(&policy), ["mp3 256_319"]);
    assert_eq!(policy["quality_min"], "mp3_256");
    assert_eq!(policy["quality_max"], "mp3_256");
    assert!(lines.is_empty(), "{lines:?}");

    // With no policy section v2 ran on the copies under the download client.
    let client = json!({
        "enabled": true,
        "quality_min": "mp3_256",
        "quality_max": "mp3_320",
        "preflight_score_auto_accept": 0.9,
        "preflight_score_manual_min": 0.6,
        "download_stall_timeout_minutes": 45,
        "download_queued_timeout_minutes": 300,
        "preferred_quality_wait_minutes": 5,
        "max_failover_attempts": 7,
    });
    let (policy, _) = import("policy-client", json!({"download_client": client})).await;
    assert_eq!(steps(&policy), ["mp3 320_plus", "mp3 256_319"]);
    assert_eq!(policy["preflight_score_auto_accept"], 0.9);
    assert_eq!(policy["preflight_score_manual_min"], 0.6);
    assert_eq!(policy["download_stall_timeout_minutes"], 45);
    assert_eq!(policy["download_queued_timeout_minutes"], 300);
    assert_eq!(policy["preferred_quality_wait_minutes"], 5);
    assert_eq!(policy["max_failover_attempts"], 7);

    // Beside a policy section v2 never read them, so they do not win.
    let (policy, _) = import(
        "policy-both",
        json!({"download_client": client, "download_policy": {"quality_max": "lossless"}}),
    )
    .await;
    assert_eq!(steps(&policy), ["flac hi_res", "mp3 320_plus"]);
    assert!(policy.get("preflight_score_auto_accept").is_none());
}

/// A v2 export carrying settings v3 removed imports, never refused: the
/// report names each removed setting as dropped, the fields kept beside
/// them land, and the stored download policy still decodes. Its recycle
/// bin path moves to Library Management, where v3 sets the bin, over a path
/// removals never used, and the report names the path dropped.
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
        "recycle_bin_path": "/srv/bin",
        "recycle_retention_days": 12,
        "quality_recipe": [
            {"format": "mp3", "quality": "custom", "min_bitrate_kbps": 192,
             "target_bitrate_kbps": 256, "max_bitrate_kbps": 320},
        ],
    });
    export["settings"]["library_management"] = json!({"recycle_bin_path": "/srv/stale"});
    export["settings"]["download_clients"] =
        json!({"sabnzbd": {"url": "http://sab:8080", "post_processing": 3}});
    export["settings"]["indexers"] =
        json!([{"name": "one", "url": "https://one.example", "type": "newznab"}]);
    export["settings"]["_internal"] =
        json!({"plex_client_id": "plex-1", "brainzmash_consent_admin": "u1"});
    export["settings"]["scrobble_settings"] = json!({"scrobble_to_lastfm": true});
    export["settings"]["primary_music_source"] = json!({"source": "lastfm"});

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("removed");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

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
            "library_management.recycle_bin_path (/srv/stale)",
            "primary_music_source",
            "scrobble_settings",
        ]
    );

    let stored = stored_settings(&target_db);
    assert_eq!(
        stored["advanced_settings"],
        json!({"discover_queue_size": 12})
    );
    assert_eq!(stored["_internal"], json!({"plex_client_id": "plex-1"}));
    assert!(stored.get("scrobble_settings").is_none());
    assert!(stored.get("primary_music_source").is_none());
    let policy: DownloadPolicy =
        serde_json::from_value(stored["download_policy"].clone()).expect("the policy decodes");
    assert_eq!(policy.quality_recipe[0].max_bitrate_kbps, Some(320));
    assert!(stored["download_policy"].get("recycle_bin_path").is_none());
    assert!(
        stored["download_policy"]
            .get("recycle_retention_days")
            .is_none()
    );
    assert_eq!(
        stored["library_management"],
        json!({"recycle_bin_path": "/srv/bin", "recycle_retention_days": 12})
    );
}

/// With a blank download policy path, the path a person set in v2's Library
/// Management stays.
#[tokio::test]
async fn a_null_section_neither_loses_nor_invents_a_retention() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["download_policy"] = json!({"recycle_retention_days": 365});
    export["settings"]["library_management"] = Value::Null;
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("null-management");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;
    assert_eq!(report.exit.code, ExitCode::Ok, "{}", report.to_json());
    assert_eq!(
        stored_settings(&target_db)["library_management"]["recycle_retention_days"],
        365
    );

    let mut export = fixture.shell();
    export["settings"]["download_policy"] = Value::Null;
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("null-policy");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;
    assert_eq!(report.exit.code, ExitCode::Ok, "{}", report.to_json());
    assert!(
        !report
            .to_json()
            .to_string()
            .contains("recycle_retention_days"),
        "{}",
        report.to_json()
    );
    assert!(
        stored_settings(&target_db)["library_management"]
            .get("recycle_retention_days")
            .is_none()
    );
}

#[tokio::test]
async fn a_chosen_bin_stays_when_the_download_policy_names_none() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["download_policy"] = json!({"recycle_bin_path": " "});
    export["settings"]["library_management"] = json!({"recycle_bin_path": "/srv/chosen"});

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("chosen-bin");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;

    assert_eq!(report.exit.code, ExitCode::Ok, "{}", report.to_json());
    let stored = stored_settings(&target_db);
    assert_eq!(
        stored["library_management"]["recycle_bin_path"],
        "/srv/chosen"
    );
    assert!(stored["download_policy"].get("recycle_bin_path").is_none());
    assert!(
        stored["library_management"]
            .get("recycle_retention_days")
            .is_none(),
        "with none stored, the default holds"
    );
}

#[tokio::test]
async fn event_cities_import_inside_the_city_limits() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let city = |name: &str, latitude: f64, radius_km: f64| {
        json!({"user_id": "u1", "city_name": name, "country_code": "GB",
               "latitude": latitude, "longitude": -1.0, "radius_km": radius_km, "position": 0})
    };
    export["event_cities"] = json!([
        city("Near", 51.0, 0.0),
        city("Far", 52.0, 10_000.0),
        city("Middle", 53.0, 30.0),
    ]);

    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("event-cities");
    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;
    assert_eq!(report.exit.code, ExitCode::Ok);

    let stored: Vec<(String, f64, i64)> = target_db.rows(
        "SELECT city_name, radius_km, position FROM catalog_event_cities \
         WHERE user_id = 'u1' ORDER BY position",
        [],
    );
    assert_eq!(
        stored,
        vec![
            ("Near".to_owned(), 1.0, 0),
            ("Far".to_owned(), 500.0, 1),
            ("Middle".to_owned(), 30.0, 2),
        ]
    );
}

/// An export is imported with only what identifies each record: anything
/// else a record lacks takes its empty value with a report line, never a
/// refused file, and the re-import still writes nothing.
#[tokio::test]
async fn a_file_with_only_the_required_fields_imports() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([{"id": "u1"}]);
    export["follows"] = json!([{"user_id": "u1", "artist_mbid": MBID}]);
    export["approvals"] = json!([{"user_id": "u1", "artist_mbid": MBID}]);
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("minimal");
    let request = |crypto| import_request(&export, target_db.clone(), config_path.clone(), crypto);

    let first = run_import(request(crypto)).await;
    assert_eq!(first.exit.code, ExitCode::Ok, "{}", first.exit.message);
    assert_eq!(counts(&first, "user"), (1, 0, 0, 0));
    let state: String = target_db.one("SELECT state FROM auto_download_approvals", []);
    assert_eq!(state, "pending");
    assert!(
        first.items.iter().any(|item| item.entity == "approval"
            && item.outcome == "converted"
            && item.detail.contains("no state")),
        "a state the record lacks is reported"
    );

    let runs = count(&target_db, "import_runs");
    let second = run_import(request(test_crypto())).await;
    assert_eq!(second.exit.code, ExitCode::Ok, "{}", second.exit.message);
    assert_eq!(counts(&second, "user"), (0, 1, 0, 0));
    assert_eq!(counts(&second, "follow"), (0, 1, 0, 0));
    assert_eq!(counts(&second, "approval"), (0, 1, 0, 0));
    assert_eq!(count(&target_db, "import_runs"), runs, "nothing written");
}

/// What an area would refuse in a v2 value never fails the import and never
/// lands as a silent default.
#[tokio::test]
async fn values_an_area_would_refuse_are_converted_or_left_behind_with_a_line() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    let mut user = fixture.user("u1");
    user["role"] = json!("owner");
    user["created_at"] = json!("not a time");
    user["last_login_at"] = json!("nor this");
    export["users"] = json!([user]);
    let mut approval = fixture.approval("u1", MBID);
    approval["state"] = json!("maybe");
    export["approvals"] = json!([approval]);
    export["event_cities"] = json!([{
        "user_id": "u1", "city_name": "Far", "latitude": 1.0, "longitude": 2.0,
        "radius_km": 900.0, "position": 0,
    }]);
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("refused");

    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;
    assert_eq!(
        report.exit.code,
        ExitCode::OkWithDrops,
        "{}",
        report.exit.message
    );

    let (role, created, login): (String, i64, Option<i64>) = target_db.row(
        "SELECT role, created_at, last_login_at FROM auth_users WHERE id = 'u1'",
        [],
    );
    assert_eq!((role.as_str(), created, login), ("user", 0, None));
    let radius: f64 = target_db.one("SELECT radius_km FROM catalog_event_cities", []);
    assert_eq!(radius, 500.0);
    assert_eq!(report.entities["approval"].dropped_invalid, 1);
    assert_eq!(count(&target_db, "auto_download_approvals"), 0);
    let converted = |entity: &str, words: &str| {
        report.items.iter().any(|item| {
            item.entity == entity && item.outcome == "converted" && item.detail.contains(words)
        })
    };
    assert!(converted("user", "role"));
    assert!(converted("user", "created_at"));
    assert!(converted("user", "last_login_at"));
    assert!(converted("event_city", "radius"));
}

/// A local row keeps its uid and its password when the account has no
/// username, which a username taken by someone else leaves it with.
#[tokio::test]
async fn a_local_row_keeps_its_uid_on_an_account_with_no_username() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    let mut user = fixture.user("u1");
    user["username"] = Value::Null;
    export["users"] = json!([user]);
    let (target_db, _target) = migrated_file();
    let (_dir, config_path, crypto) = scratch_config("no-username");

    let report = run_import(import_request(
        &export,
        target_db.clone(),
        config_path,
        crypto,
    ))
    .await;
    assert_eq!(report.exit.code, ExitCode::Ok, "{}", report.exit.message);
    let (uid, hash, scheme): (String, String, String) = target_db.row(
        "SELECT provider_uid, password_hash, hash_scheme FROM auth_providers WHERE user_id = 'u1'",
        [],
    );
    assert_eq!(uid, "local:u1");
    assert_eq!(hash, BCRYPT_HASH);
    assert_eq!(scheme, "bcrypt");
}
