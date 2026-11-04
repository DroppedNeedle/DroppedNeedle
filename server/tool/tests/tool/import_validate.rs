//! Validator tests: one table of the rules that refuse a file and one of
//! the warnings that accept it.
//!
//! Each row pins one error or warning of the standalone export
//! validator. No database, no passphrase, no filesystem.

use crate::import_support as support;

use serde_json::{Value, json};
use support::Fixture;

use droppedneedle_tool::import::validate_export;

fn codes(report: &droppedneedle_tool::import::ValidationReport) -> Vec<&str> {
    report.errors.iter().map(|issue| issue.code).collect()
}

fn warning_codes(report: &droppedneedle_tool::import::ValidationReport) -> Vec<&str> {
    report.warnings.iter().map(|issue| issue.code).collect()
}

/// The shell with `users` set.
fn with_users(fixture: &Fixture, users: Value) -> Value {
    let mut export = fixture.shell();
    export["users"] = users;
    export
}

/// Each rule refuses the file with its own code.
#[test]
fn validator_refuses_each_rule() {
    let fixture = Fixture::shared();
    let mut rows: Vec<(String, Value, &str)> = Vec::new();
    let mut row = |label: &str, export: Value, code: &'static str| {
        rows.push((label.to_owned(), export, code));
    };

    let mut export = fixture.shell();
    export["format"] = json!("something-else");
    row("bad format", export, "BAD_FORMAT");

    let mut export = fixture.shell();
    export["format_version"] = json!(3);
    row(
        "future format version",
        export,
        "UNSUPPORTED_FORMAT_VERSION",
    );

    for version in [json!(0), json!("1"), json!(null)] {
        let mut export = fixture.shell();
        export["format_version"] = version.clone();
        row(
            &format!("format version {version}"),
            export,
            "BAD_FORMAT_VERSION",
        );
    }
    let mut export = fixture.shell();
    export.as_object_mut().unwrap().remove("format_version");
    row("missing format version", export, "BAD_FORMAT_VERSION");

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
        row(&format!("missing key {key}"), export, "MISSING_KEY");
    }

    for root in [json!([1, 2]), json!("export"), json!(null)] {
        row(&format!("root {root}"), root, "EXPORT_NOT_OBJECT");
    }

    for stamp in [
        "not-a-date",
        "2026-09-28T12:00:00+02:00",
        "2026-13-01T00:00:00Z",
    ] {
        let mut export = fixture.shell();
        export["exported_at"] = json!(stamp);
        row(&format!("exported_at {stamp}"), export, "BAD_EXPORTED_AT");
    }

    let mut export = fixture.shell();
    export["secret_envelope"]["scheme"] = json!("rot13");
    row("unknown envelope scheme", export, "UNKNOWN_ENVELOPE_SCHEME");

    for (param, bad) in [
        ("m", json!(1024)),
        ("t", json!(1)),
        ("p", json!(4)),
        ("algo", json!("scrypt")),
    ] {
        let mut export = fixture.shell();
        export["secret_envelope"]["kdf"][param] = bad;
        row(
            &format!("unpinned kdf {param}"),
            export,
            "BAD_ENVELOPE_PARAMS",
        );
    }

    let mut short = fixture.shell();
    short["secret_envelope"]["kdf"]["salt_b64"] = json!("AAAA");
    short["secret_envelope"]["nonce_b64"] = json!("AAAA");
    row("short salt", short.clone(), "BAD_ENVELOPE_SALT");
    row("short nonce", short, "BAD_ENVELOPE_NONCE");

    // Users.
    row(
        "users not an array",
        with_users(fixture, json!({"u1": {}})),
        "USERS_NOT_ARRAY",
    );
    row(
        "user not an object",
        with_users(fixture, json!(["u1"])),
        "USER_NOT_OBJECT",
    );
    for id in [json!(null), json!("")] {
        let mut user = fixture.user("u1");
        user["id"] = id.clone();
        row(
            &format!("user id {id}"),
            with_users(fixture, json!([user])),
            "USER_MISSING_ID",
        );
    }
    let mut user = fixture.user("u1");
    user.as_object_mut().unwrap().remove("id");
    row(
        "user without an id",
        with_users(fixture, json!([user])),
        "USER_MISSING_ID",
    );

    row(
        "duplicate user id",
        with_users(fixture, json!([fixture.user("u1"), fixture.user("u1")])),
        "DUPLICATE_USER_ID",
    );

    let mut first = fixture.user("u1");
    let mut second = fixture.user("u2");
    second["email"] = first["email"].clone();
    row(
        "email collision",
        with_users(fixture, json!([first.clone(), second])),
        "USER_FIELD_COLLISION",
    );
    let mut third = fixture.user("u3");
    third["username"] = first["username"].clone();
    first["email"] = json!("unique@example.com");
    row(
        "username collision",
        with_users(fixture, json!([first, third])),
        "USER_FIELD_COLLISION",
    );

    let mut first = fixture.user("u1");
    let mut second = fixture.user("u2");
    second["providers"] = first["providers"].clone();
    second["email"] = json!("u2@example.com");
    second["username"] = json!("u2");
    first["email"] = json!("u1@example.com");
    first["username"] = json!("u1");
    row(
        "duplicate provider",
        with_users(fixture, json!([first, second])),
        "DUPLICATE_PROVIDER",
    );

    let mut user = fixture.user("u1");
    user["providers"] = json!({"local": {}});
    row(
        "providers not an array",
        with_users(fixture, json!([user])),
        "PROVIDERS_NOT_ARRAY",
    );
    let mut user = fixture.user("u1");
    user["providers"] = json!(["local"]);
    row(
        "provider not an object",
        with_users(fixture, json!([user])),
        "PROVIDER_NOT_OBJECT",
    );
    for field in ["provider", "provider_uid"] {
        let mut user = fixture.user("u1");
        user["providers"][0][field] = json!("");
        row(
            &format!("provider without {field}"),
            with_users(fixture, json!([user])),
            "PROVIDER_MISSING_KEY",
        );
    }

    let mut user = fixture.user("u1");
    user["providers"][0]["hash_scheme"] = json!("opaque");
    row(
        "local provider with an opaque hash",
        with_users(fixture, json!([user.clone()])),
        "BAD_LOCAL_HASH",
    );
    user["providers"][0]["hash_scheme"] = json!("bcrypt");
    user["providers"][0]["provider_data"] = json!(r#"{"password_hash": "md5:abc"}"#);
    row(
        "local provider with an md5 hash",
        with_users(fixture, json!([user])),
        "BAD_LOCAL_HASH",
    );

    let mut user = fixture.user("u1");
    user["app_passwords"] = json!({"phone": {}});
    row(
        "app passwords not an array",
        with_users(fixture, json!([user])),
        "APP_PASSWORDS_NOT_ARRAY",
    );
    let mut user = fixture.user("u1");
    user["app_passwords"] = json!(["phone"]);
    row(
        "app password not an object",
        with_users(fixture, json!([user])),
        "APP_PASSWORD_NOT_OBJECT",
    );
    let mut user = fixture.user("u1");
    user["app_passwords"][0]["secret"] = json!("plaintext-secret");
    row(
        "app password secret not sealed",
        with_users(fixture, json!([user])),
        "SECRET_NOT_SEALED",
    );

    // Settings.
    let mut export = fixture.shell();
    export["settings"] = json!([]);
    row("settings not an object", export, "SETTINGS_NOT_OBJECT");

    for section in [
        "library_sync_settings",
        "_legacy_lidarr",
        "local_files_settings",
    ] {
        let mut export = fixture.shell();
        export["settings"][section] = json!({});
        row(
            &format!("dropped section {section}"),
            export,
            "DROPPED_SECTION_PRESENT",
        );
    }
    let mut export = fixture.shell();
    export["settings"]["cache_ttl_default"] = json!({});
    row(
        "dropped env name in settings",
        export,
        "DROPPED_SECTION_PRESENT",
    );
    let mut export = fixture.shell();
    export["jellyfin_url"] = json!("http://mirror");
    row(
        "dropped top-level jellyfin_url",
        export,
        "DROPPED_SECTION_PRESENT",
    );

    let mut export = fixture.shell();
    export["settings"]["advanced_settings"] =
        json!({"http_timeout": 30, "artist_discovery_warm_interval": 5});
    row(
        "advanced setting not allowed",
        export,
        "SETTINGS_FIELD_NOT_ALLOWED",
    );
    let mut export = fixture.shell();
    export["settings"]["_internal"] = json!({
        "plex_client_id": "x",
        "audiodb_sweep_cursor": "y",
    });
    row(
        "internal key not allowed",
        export,
        "SETTINGS_FIELD_NOT_ALLOWED",
    );

    let mut export = fixture.shell();
    export["settings"]["jellyfin_settings"] = json!({"api_key": "bare-secret"});
    row("bare secret", export, "SECRET_NOT_SEALED");
    let mut export = fixture.shell();
    export["settings"]["lastfm_settings"] = json!({"enabled": true, "api_key": "bare-key"});
    row("bare Last.fm key", export, "SECRET_NOT_SEALED");

    let mut export = fixture.shell();
    export["settings"]["jellyfin_settings"] = json!({"api_key": {"$sealed": "!!!not-b64!!!"}});
    row("sealed blob not base64", export, "SEALED_NOT_BASE64");
    let mut export = fixture.shell();
    export["settings"]["jellyfin_settings"] = json!({"api_key": {"$sealed": "AAAA"}});
    row("sealed blob too short", export, "SEALED_TOO_SHORT");

    let mut export = fixture.shell();
    export["settings"]["indexers"] = json!({"name": "x"});
    row("indexers not an array", export, "INDEXERS_NOT_ARRAY");

    // Follows and approvals.
    for (section, map_code, item_code) in [
        ("follows", "FOLLOWS_NOT_ARRAY", "FOLLOW_NOT_OBJECT"),
        ("approvals", "APPROVALS_NOT_ARRAY", "APPROVAL_NOT_OBJECT"),
    ] {
        let mut export = fixture.shell();
        export[section] = json!({"mbid": {}});
        row(&format!("{section} not an array"), export, map_code);
        let mut export = fixture.shell();
        export[section] = json!(["mbid"]);
        row(&format!("{section} item not an object"), export, item_code);
    }

    let with_u1 = with_users(fixture, json!([fixture.user("u1")]));
    for mbid in ["", "not-an-mbid", "01234567-89ab-cdef-0123-456789abcde!"] {
        let mut bad = with_u1.clone();
        bad["follows"] = json!([fixture.follow("u1", mbid)]);
        row(&format!("follow mbid {mbid:?}"), bad, "BAD_MBID");
        let mut bad = with_u1.clone();
        bad["approvals"] = json!([fixture.approval("u1", mbid)]);
        row(&format!("approval mbid {mbid:?}"), bad, "BAD_MBID");
    }

    let mut follow = fixture.follow("u1", support::MBID);
    follow.as_object_mut().unwrap().remove("user_id");
    let mut export = fixture.shell();
    export["follows"] = json!([follow]);
    row("follow without a user id", export, "MISSING_USER_ID");
    let mut approval = fixture.approval("u1", support::MBID);
    approval.as_object_mut().unwrap().remove("user_id");
    let mut export = fixture.shell();
    export["approvals"] = json!([approval]);
    row("approval without a user id", export, "MISSING_USER_ID");

    let mut export = with_u1.clone();
    export["follows"] = json!([fixture.follow("ghost", support::MBID)]);
    row("follow of an unknown user", export, "DANGLING_USER_REF");
    let mut export = with_u1.clone();
    export["approvals"] = json!([fixture.approval("ghost", support::MBID)]);
    row("approval of an unknown user", export, "DANGLING_USER_REF");

    let upper = support::MBID.to_uppercase();
    let mut duplicates = with_u1;
    duplicates["follows"] = json!([
        fixture.follow("u1", support::MBID),
        fixture.follow("u1", &upper)
    ]);
    duplicates["approvals"] = json!([
        fixture.approval("u1", support::MBID),
        fixture.approval("u1", &upper)
    ]);
    row("duplicate follow", duplicates.clone(), "DUPLICATE_FOLLOW");
    row("duplicate approval", duplicates, "DUPLICATE_APPROVAL");

    for (label, export, code) in &rows {
        let report = validate_export(export);
        assert!(!report.valid(), "case: {label}");
        let found = codes(&report);
        assert!(found.contains(code), "case: {label}: {found:?}");
    }
}

/// What a file may carry and still import: the clean shell passes silent,
/// and each of these only warns.
#[test]
fn validator_warns_and_accepts() {
    let fixture = Fixture::shared();
    let report = validate_export(&fixture.shell());
    assert!(report.valid(), "case: clean shell: {:?}", report.errors);
    assert!(report.warnings.is_empty(), "case: clean shell");

    // A row with no warning code only has to stay valid.
    let mut rows: Vec<(String, Value, Option<&str>)> = Vec::new();

    let mut export = fixture.shell();
    export["future_thing"] = json!({});
    rows.push((
        "unknown top-level key".to_owned(),
        export,
        Some("UNKNOWN_TOP_LEVEL_KEY"),
    ));

    for section in [
        "playlists",
        "favorites",
        "quotas",
        "user_prefs",
        "wanted_watches",
    ] {
        let mut export = fixture.shell();
        export[section] = json!([]);
        rows.push((
            format!("reserved section {section}"),
            export,
            Some("IGNORED_RESERVED_SECTION"),
        ));
    }

    let mut export = fixture.shell();
    export.as_object_mut().unwrap().remove("v2_commit");
    rows.push((
        "missing v2 commit".to_owned(),
        export,
        Some("MISSING_V2_COMMIT"),
    ));

    let mut user = fixture.user("u1");
    user["app_passwords"][0]["revoked"] = json!(true);
    rows.push((
        "revoked app password".to_owned(),
        with_users(fixture, json!([user])),
        Some("REVOKED_APP_PASSWORD_KEPT"),
    ));

    let mut export = with_users(fixture, json!([fixture.user("u1")]));
    let mut approval = fixture.approval("u1", support::MBID);
    approval["reviewed_by_id"] = json!("ghost");
    export["approvals"] = json!([approval]);
    rows.push((
        "dangling reviewer".to_owned(),
        export,
        Some("DANGLING_REVIEWER"),
    ));

    let mut unknown_section = fixture.shell();
    unknown_section["settings"]["future_section"] =
        json!({"api_key": {"$sealed": "!!!not-b64!!!"}});
    rows.push((
        "unknown settings section".to_owned(),
        unknown_section.clone(),
        Some("UNKNOWN_SETTINGS_SECTION"),
    ));

    let mut export = fixture.shell();
    export["settings"]["advanced_settings"] =
        json!({"http_timeout": 30, "cache_ttl_something_new": 5});
    rows.push(("advanced cache_ttl key".to_owned(), export, None));

    let mut export = fixture.shell();
    export["settings"]["jellyfin_settings"] = json!({"api_key": fixture.seal("real-secret")});
    rows.push(("sealed secret".to_owned(), export, None));

    for (label, export, warning) in &rows {
        let report = validate_export(export);
        assert!(report.valid(), "case: {label}: {:?}", report.errors);
        if let Some(warning) = warning {
            assert!(
                warning_codes(&report).contains(warning),
                "case: {label}: {:?}",
                report.warnings
            );
        }
    }

    let report = validate_export(&unknown_section);
    assert!(
        !codes(&report).contains(&"SEALED_NOT_BASE64"),
        "ignored sections skip sealed-shape checks"
    );
}

/// A missing identity field and a field of the wrong type both refuse the
/// file and name the record, without quoting what the record held.
#[test]
fn a_record_that_does_not_decode_is_refused_by_name() {
    let fixture = Fixture::shared();
    let mut export = fixture.shell();
    export["users"] = json!([{"id": "u1"}, {"display_name": "No id"}, {"id": "u3"}]);
    export["users"][2]["app_passwords"] = json!([{"name": "x", "secret": "plain-secret-word"}]);
    export["follows"] = json!([{"user_id": "u1"}]);
    let report = validate_export(&export);
    let named = |code: &str, path: &str| {
        report
            .errors
            .iter()
            .any(|issue| issue.code == code && issue.path == path)
    };
    assert!(
        named("USER_MISSING_ID", "users[1].id"),
        "{:?}",
        report.errors
    );
    assert!(
        named("SECRET_NOT_SEALED", "users[2].app_passwords[0].secret"),
        "{:?}",
        report.errors
    );
    assert!(
        named("BAD_MBID", "follows[0].artist_mbid"),
        "{:?}",
        report.errors
    );

    let mut export = fixture.shell();
    export["users"] = json!([{"id": "u1", "email": 5}]);
    let report = validate_export(&export);
    assert!(
        report
            .errors
            .iter()
            .any(|issue| issue.code == "BAD_RECORD" && issue.path == "users[0]"),
        "{:?}",
        report.errors
    );
    assert!(!format!("{:?}", report.errors).contains("plain-secret-word"));
}
