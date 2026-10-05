//! Validator tests: one behavior per rule.
//!
//! Each test pins one error or warning of the standalone export
//! validator. No database, no passphrase, no filesystem.

use crate::import_support as support;

use serde_json::json;
use support::Fixture;

use droppedneedle::import::validate_export;

fn codes(report: &droppedneedle::import::ValidationReport) -> Vec<&str> {
    report.errors.iter().map(|issue| issue.code).collect()
}

fn warning_codes(report: &droppedneedle::import::ValidationReport) -> Vec<&str> {
    report.warnings.iter().map(|issue| issue.code).collect()
}

#[test]
fn valid_shell_passes_silent() {
    let fixture = Fixture::shared();
    let report = validate_export(&fixture.shell());
    assert!(report.valid(), "errors: {:?}", report.errors);
    assert!(report.warnings.is_empty());
}

#[test]
fn bad_format_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["format"] = json!("something-else");
    let report = validate_export(&export);
    assert!(!report.valid());
    assert!(codes(&report).contains(&"BAD_FORMAT"));
}

#[test]
fn future_format_version_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["format_version"] = json!(2);
    let report = validate_export(&export);
    assert!(codes(&report).contains(&"UNSUPPORTED_FORMAT_VERSION"));
}

#[test]
fn missing_required_key_rejected() {
    let fixture = Fixture::shared();
    // `format` and `format_version` report their value codes when absent,
    // so they are not in this loop.
    for key in [
        "exported_at",
        "instance_id",
        "secret_envelope",
        "users",
        "settings",
        "follows",
        "approvals",
    ] {
        let mut export = fixture.shell();
        export.as_object_mut().unwrap().remove(key);
        let report = validate_export(&export);
        assert!(
            codes(&report).contains(&"MISSING_KEY"),
            "key {key} must fail"
        );
    }
}

#[test]
fn bad_exported_at_rejected() {
    let fixture = Fixture::shared();
    for stamp in [
        "not-a-date",
        "2026-09-28T12:00:00+02:00",
        "2026-13-01T00:00:00Z",
    ] {
        let mut export = fixture.shell();
        export["exported_at"] = json!(stamp);
        let report = validate_export(&export);
        assert!(
            codes(&report).contains(&"BAD_EXPORTED_AT"),
            "stamp {stamp} must fail"
        );
    }
}

#[test]
fn unknown_envelope_scheme_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["secret_envelope"]["scheme"] = json!("rot13");
    let report = validate_export(&export);
    assert!(codes(&report).contains(&"UNKNOWN_ENVELOPE_SCHEME"));
}

#[test]
fn unpinned_kdf_rejected() {
    let fixture = Fixture::shared();
    for (param, bad) in [
        ("m", json!(1024)),
        ("t", json!(1)),
        ("p", json!(4)),
        ("algo", json!("scrypt")),
    ] {
        let mut export = fixture.shell();
        export["secret_envelope"]["kdf"][param] = bad;
        let report = validate_export(&export);
        assert!(
            codes(&report).contains(&"BAD_ENVELOPE_PARAMS"),
            "param {param} must fail"
        );
    }
}

#[test]
fn short_salt_and_nonce_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["secret_envelope"]["kdf"]["salt_b64"] = json!("AAAA");
    export["secret_envelope"]["nonce_b64"] = json!("AAAA");
    let report = validate_export(&export);
    let found = codes(&report);
    assert!(found.contains(&"BAD_ENVELOPE_SALT"));
    assert!(found.contains(&"BAD_ENVELOPE_NONCE"));
}

#[test]
fn unknown_top_level_key_warns() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["future_thing"] = json!({});
    let report = validate_export(&export);
    assert!(report.valid());
    assert!(warning_codes(&report).contains(&"UNKNOWN_TOP_LEVEL_KEY"));
}

#[test]
fn reserved_section_warns() {
    let fixture = Fixture::shared();
    for section in [
        "playlists",
        "favorites",
        "quotas",
        "user_prefs",
        "wanted_watches",
        "user_connections",
    ] {
        let mut export = fixture.shell();
        export[section] = json!([]);
        let report = validate_export(&export);
        assert!(report.valid());
        assert!(
            warning_codes(&report).contains(&"IGNORED_RESERVED_SECTION"),
            "section {section} must warn"
        );
    }
}

#[test]
fn missing_v2_commit_warns() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export.as_object_mut().unwrap().remove("v2_commit");
    let report = validate_export(&export);
    assert!(report.valid());
    assert!(warning_codes(&report).contains(&"MISSING_V2_COMMIT"));
}

#[test]
fn duplicate_user_id_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1"), fixture.user("u1")]);
    let report = validate_export(&export);
    assert!(codes(&report).contains(&"DUPLICATE_USER_ID"));
}

#[test]
fn email_and_username_collisions_rejected() {
    let fixture = Fixture::shared();
    let mut first = fixture.user("u1");
    let mut second = fixture.user("u2");
    second["email"] = first["email"].clone();
    let mut export = fixture.shell();
    export["users"] = json!([first.clone(), second]);
    assert!(codes(&validate_export(&export)).contains(&"USER_FIELD_COLLISION"));

    let mut third = fixture.user("u3");
    third["username"] = first["username"].clone();
    first["email"] = json!("unique@example.com");
    export["users"] = json!([first, third]);
    assert!(codes(&validate_export(&export)).contains(&"USER_FIELD_COLLISION"));
}

#[test]
fn duplicate_provider_rejected() {
    let fixture = Fixture::shared();
    let mut first = fixture.user("u1");
    let mut second = fixture.user("u2");
    second["providers"] = first["providers"].clone();
    second["email"] = json!("u2@example.com");
    second["username"] = json!("u2");
    first["email"] = json!("u1@example.com");
    first["username"] = json!("u1");
    let mut export = fixture.shell();
    export["users"] = json!([first, second]);
    assert!(codes(&validate_export(&export)).contains(&"DUPLICATE_PROVIDER"));
}

#[test]
fn local_provider_requires_bcrypt() {
    let fixture = Fixture::shared();
    let mut user = fixture.user("u1");
    user["providers"][0]["hash_scheme"] = json!("opaque");
    let mut export = fixture.shell();
    export["users"] = json!([user.clone()]);
    assert!(codes(&validate_export(&export)).contains(&"BAD_LOCAL_HASH"));

    user["providers"][0]["hash_scheme"] = json!("bcrypt");
    user["providers"][0]["provider_data"] = json!(r#"{"password_hash": "md5:abc"}"#);
    export["users"] = json!([user]);
    assert!(codes(&validate_export(&export)).contains(&"BAD_LOCAL_HASH"));
}

#[test]
fn app_password_secret_must_be_sealed() {
    let fixture = Fixture::shared();
    let mut user = fixture.user("u1");
    user["app_passwords"][0]["secret"] = json!("plaintext-secret");
    let mut export = fixture.shell();
    export["users"] = json!([user]);
    assert!(codes(&validate_export(&export)).contains(&"SECRET_NOT_SEALED"));
}

#[test]
fn revoked_app_password_warns() {
    let fixture = Fixture::shared();
    let mut user = fixture.user("u1");
    user["app_passwords"][0]["revoked"] = json!(true);
    let mut export = fixture.shell();
    export["users"] = json!([user]);
    let report = validate_export(&export);
    assert!(report.valid());
    assert!(warning_codes(&report).contains(&"REVOKED_APP_PASSWORD_KEPT"));
}

#[test]
fn dropped_sections_rejected() {
    let fixture = Fixture::shared();
    for section in [
        "library_sync_settings",
        "_legacy_lidarr",
        "local_files_settings",
    ] {
        let mut export = fixture.shell();
        export["settings"][section] = json!({});
        let report = validate_export(&export);
        assert!(
            codes(&report).contains(&"DROPPED_SECTION_PRESENT"),
            "section {section} must fail"
        );
    }
}

#[test]
fn dropped_env_names_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["cache_ttl_default"] = json!({});
    assert!(codes(&validate_export(&export)).contains(&"DROPPED_SECTION_PRESENT"));
    let mut export = fixture.shell();
    export["jellyfin_url"] = json!("http://mirror");
    assert!(codes(&validate_export(&export)).contains(&"DROPPED_SECTION_PRESENT"));
}

#[test]
fn advanced_settings_allowlist_enforced() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["advanced_settings"] =
        json!({"http_timeout": 30, "artist_discovery_warm_interval": 5});
    assert!(codes(&validate_export(&export)).contains(&"SETTINGS_FIELD_NOT_ALLOWED"));
    let mut export = fixture.shell();
    export["settings"]["advanced_settings"] =
        json!({"http_timeout": 30, "cache_ttl_something_new": 5});
    assert!(validate_export(&export).valid());
}

#[test]
fn internal_allowlist_enforced() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["_internal"] = json!({
        "plex_client_id": "x",
        "audiodb_sweep_cursor": "y",
    });
    assert!(codes(&validate_export(&export)).contains(&"SETTINGS_FIELD_NOT_ALLOWED"));
}

#[test]
fn secret_bare_string_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["jellyfin_settings"] = json!({"api_key": "bare-secret"});
    assert!(codes(&validate_export(&export)).contains(&"SECRET_NOT_SEALED"));

    let mut export = fixture.shell();
    export["settings"]["jellyfin_settings"] = json!({"api_key": fixture.seal("real-secret")});
    assert!(validate_export(&export).valid());
}

#[test]
fn sealed_blob_must_be_long_base64() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["jellyfin_settings"] = json!({"api_key": {"$sealed": "!!!not-b64!!!"}});
    assert!(codes(&validate_export(&export)).contains(&"SEALED_NOT_BASE64"));

    let mut export = fixture.shell();
    export["settings"]["jellyfin_settings"] = json!({"api_key": {"$sealed": "AAAA"}});
    assert!(codes(&validate_export(&export)).contains(&"SEALED_TOO_SHORT"));
}

#[test]
fn indexers_must_be_array() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["indexers"] = json!({"name": "x"});
    assert!(codes(&validate_export(&export)).contains(&"INDEXERS_NOT_ARRAY"));
}

#[test]
fn bad_mbid_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    for mbid in ["", "not-an-mbid", "01234567-89ab-cdef-0123-456789abcde!"] {
        let mut bad = export.clone();
        bad["follows"] = json!([fixture.follow("u1", mbid)]);
        assert!(
            codes(&validate_export(&bad)).contains(&"BAD_MBID"),
            "follow mbid {mbid:?} must fail"
        );
        let mut bad = export.clone();
        bad["approvals"] = json!([fixture.approval("u1", mbid)]);
        assert!(
            codes(&validate_export(&bad)).contains(&"BAD_MBID"),
            "approval mbid {mbid:?} must fail"
        );
    }
}

#[test]
fn missing_user_id_rejected() {
    let fixture = Fixture::shared();
    let mut follow = fixture.follow("u1", support::MBID);
    follow.as_object_mut().unwrap().remove("user_id");
    let mut export = fixture.shell();
    export["follows"] = json!([follow]);
    assert!(codes(&validate_export(&export)).contains(&"MISSING_USER_ID"));

    let mut approval = fixture.approval("u1", support::MBID);
    approval.as_object_mut().unwrap().remove("user_id");
    let mut export = fixture.shell();
    export["approvals"] = json!([approval]);
    assert!(codes(&validate_export(&export)).contains(&"MISSING_USER_ID"));
}

#[test]
fn dangling_user_ref_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["follows"] = json!([fixture.follow("ghost", support::MBID)]);
    assert!(codes(&validate_export(&export)).contains(&"DANGLING_USER_REF"));

    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    export["approvals"] = json!([fixture.approval("ghost", support::MBID)]);
    assert!(codes(&validate_export(&export)).contains(&"DANGLING_USER_REF"));
}

#[test]
fn dangling_reviewer_warns() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let mut approval = fixture.approval("u1", support::MBID);
    approval["reviewed_by_id"] = json!("ghost");
    export["approvals"] = json!([approval]);
    let report = validate_export(&export);
    assert!(report.valid());
    assert!(warning_codes(&report).contains(&"DANGLING_REVIEWER"));
}

#[test]
fn non_object_root_rejected() {
    for root in [json!([1, 2]), json!("export"), json!(null)] {
        let report = validate_export(&root);
        assert!(!report.valid());
        assert!(codes(&report).contains(&"EXPORT_NOT_OBJECT"));
    }
}

#[test]
fn wrong_format_version_rejected() {
    let fixture = Fixture::shared();
    for version in [json!(0), json!("1"), json!(null)] {
        let mut export = fixture.shell();
        export["format_version"] = version;
        assert!(
            codes(&validate_export(&export)).contains(&"BAD_FORMAT_VERSION"),
            "version must fail"
        );
    }
    let mut export = fixture.shell();
    export.as_object_mut().unwrap().remove("format_version");
    assert!(codes(&validate_export(&export)).contains(&"BAD_FORMAT_VERSION"));
}

#[test]
fn users_shape_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!({"u1": {}});
    assert!(codes(&validate_export(&export)).contains(&"USERS_NOT_ARRAY"));

    let mut export = fixture.shell();
    export["users"] = json!(["u1"]);
    assert!(codes(&validate_export(&export)).contains(&"USER_NOT_OBJECT"));

    for id in [json!(null), json!("")] {
        let mut user = fixture.user("u1");
        user["id"] = id;
        let mut export = fixture.shell();
        export["users"] = json!([user]);
        assert!(codes(&validate_export(&export)).contains(&"USER_MISSING_ID"));
    }
    let mut user = fixture.user("u1");
    user.as_object_mut().unwrap().remove("id");
    let mut export = fixture.shell();
    export["users"] = json!([user]);
    assert!(codes(&validate_export(&export)).contains(&"USER_MISSING_ID"));
}

#[test]
fn providers_shape_rejected() {
    let fixture = Fixture::shared();
    let mut user = fixture.user("u1");
    user["providers"] = json!({"local": {}});
    let mut export = fixture.shell();
    export["users"] = json!([user]);
    assert!(codes(&validate_export(&export)).contains(&"PROVIDERS_NOT_ARRAY"));

    let mut user = fixture.user("u1");
    user["providers"] = json!(["local"]);
    let mut export = fixture.shell();
    export["users"] = json!([user]);
    assert!(codes(&validate_export(&export)).contains(&"PROVIDER_NOT_OBJECT"));
}

#[test]
fn provider_without_a_binding_rejected() {
    let fixture = Fixture::shared();
    for field in ["provider", "provider_uid"] {
        let mut user = fixture.user("u1");
        user["providers"][0][field] = json!("");
        let mut export = fixture.shell();
        export["users"] = json!([user]);
        assert!(
            codes(&validate_export(&export)).contains(&"PROVIDER_MISSING_KEY"),
            "field {field} must fail"
        );
    }
}

#[test]
fn app_passwords_shape_rejected() {
    let fixture = Fixture::shared();
    let mut user = fixture.user("u1");
    user["app_passwords"] = json!({"phone": {}});
    let mut export = fixture.shell();
    export["users"] = json!([user]);
    assert!(codes(&validate_export(&export)).contains(&"APP_PASSWORDS_NOT_ARRAY"));

    let mut user = fixture.user("u1");
    user["app_passwords"] = json!(["phone"]);
    let mut export = fixture.shell();
    export["users"] = json!([user]);
    assert!(codes(&validate_export(&export)).contains(&"APP_PASSWORD_NOT_OBJECT"));
}

#[test]
fn settings_shape_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"] = json!([]);
    assert!(codes(&validate_export(&export)).contains(&"SETTINGS_NOT_OBJECT"));
}

#[test]
fn follows_shape_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["follows"] = json!({"mbid": {}});
    assert!(codes(&validate_export(&export)).contains(&"FOLLOWS_NOT_ARRAY"));

    let mut export = fixture.shell();
    export["follows"] = json!(["mbid"]);
    assert!(codes(&validate_export(&export)).contains(&"FOLLOW_NOT_OBJECT"));
}

#[test]
fn approvals_shape_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["approvals"] = json!({"mbid": {}});
    assert!(codes(&validate_export(&export)).contains(&"APPROVALS_NOT_ARRAY"));

    let mut export = fixture.shell();
    export["approvals"] = json!(["mbid"]);
    assert!(codes(&validate_export(&export)).contains(&"APPROVAL_NOT_OBJECT"));
}

#[test]
fn duplicate_recovery_hash_rejected() {
    let fixture = Fixture::shared();
    let first = fixture.user("u1");
    let mut second = fixture.user("u2");
    second["recovery_code"]["code_hash"] = first["recovery_code"]["code_hash"].clone();
    second["email"] = json!("u2@example.com");
    second["username"] = json!("u2");
    let mut export = fixture.shell();
    export["users"] = json!([first, second]);
    assert!(codes(&validate_export(&export)).contains(&"DUPLICATE_RECOVERY_HASH"));
}

#[test]
fn unknown_settings_section_warns_and_skips() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["future_section"] = json!({"api_key": {"$sealed": "!!!not-b64!!!"}});
    let report = validate_export(&export);
    assert!(report.valid());
    assert!(
        warning_codes(&report).contains(&"UNKNOWN_SETTINGS_SECTION"),
        "warnings: {:?}",
        report.warnings
    );
    assert!(
        !codes(&report).contains(&"SEALED_NOT_BASE64"),
        "ignored sections skip sealed-shape checks"
    );
}

#[test]
fn lastfm_bare_secret_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["settings"]["lastfm_settings"] = json!({"enabled": true, "api_key": "bare-key"});
    assert!(codes(&validate_export(&export)).contains(&"SECRET_NOT_SEALED"));
}

#[test]
fn duplicate_follow_and_approval_rejected() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([fixture.user("u1")]);
    let upper = support::MBID.to_uppercase();
    export["follows"] = json!([
        fixture.follow("u1", support::MBID),
        fixture.follow("u1", &upper)
    ]);
    export["approvals"] = json!([
        fixture.approval("u1", support::MBID),
        fixture.approval("u1", &upper)
    ]);
    let report = validate_export(&export);
    let found = codes(&report);
    assert!(found.contains(&"DUPLICATE_FOLLOW"), "{found:?}");
    assert!(found.contains(&"DUPLICATE_APPROVAL"), "{found:?}");
}
