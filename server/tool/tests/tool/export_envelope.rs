//! Envelope tests: the export envelope parses only
//! `droppedneedle-export` at `format_version` 1, requires every top-level
//! key, and ignores reserved sections with a warning instead of failing.

use droppedneedle_tool::export::{EnvelopeWarning, ExportError, parse_export};
use serde_json::{Value, json};

fn minimal_envelope() -> Value {
    json!({
        "format": "droppedneedle-export",
        "format_version": 1,
        "exported_at": "2026-09-28T12:00:00Z",
        "instance_id": "instance-1",
        "secret_envelope": {
            "scheme": "argon2id+xchacha20poly1305",
            "kdf": {"algo": "argon2id", "m": 65536, "t": 3, "p": 1, "salt_b64": "AA"},
            "nonce_b64": "AA"
        },
        "users": [],
        "settings": {},
        "follows": [],
        "approvals": []
    })
}

fn parse(value: &Value) -> Result<droppedneedle_tool::export::ParsedExport, ExportError> {
    parse_export(&serde_json::to_string(value).unwrap())
}

/// Each bad envelope is refused with its own code.
#[test]
fn parse_refuses_each_bad_envelope() {
    let mut cases: Vec<(String, Value, &str)> = Vec::new();

    let mut unknown_format = minimal_envelope();
    unknown_format["format"] = json!("droppedneedle-backup");
    assert_eq!(
        parse(&unknown_format),
        Err(ExportError::UnsupportedFormat),
        "unknown format"
    );
    assert_eq!(ExportError::UnsupportedFormat.code(), "UNSUPPORTED_FORMAT");
    cases.push((
        "unknown format".to_owned(),
        unknown_format,
        "UNSUPPORTED_FORMAT",
    ));

    for version in [json!(0), json!(3), json!(99), json!("1"), json!(null)] {
        let mut envelope = minimal_envelope();
        envelope["format_version"] = version.clone();
        cases.push((
            format!("format_version {version}"),
            envelope,
            "UNSUPPORTED_FORMAT_VERSION",
        ));
    }

    // `format` and `format_version` fail their value checks when absent, so
    // they report the format/version codes rather than the key code.
    for (key, code) in [
        ("format", "UNSUPPORTED_FORMAT"),
        ("format_version", "UNSUPPORTED_FORMAT_VERSION"),
        ("exported_at", "MISSING_REQUIRED_KEY"),
        ("instance_id", "MISSING_REQUIRED_KEY"),
        ("secret_envelope", "MISSING_REQUIRED_KEY"),
        ("users", "MISSING_REQUIRED_KEY"),
        ("settings", "MISSING_REQUIRED_KEY"),
        ("follows", "MISSING_REQUIRED_KEY"),
        ("approvals", "MISSING_REQUIRED_KEY"),
    ] {
        let mut envelope = minimal_envelope();
        envelope.as_object_mut().unwrap().remove(key);
        cases.push((format!("missing key {key}"), envelope, code));
    }
    let mut no_users = minimal_envelope();
    no_users.as_object_mut().unwrap().remove("users");
    assert_eq!(
        parse(&no_users),
        Err(ExportError::MissingRequiredKey {
            key: "users".to_owned()
        })
    );

    for (section, bad) in [
        ("users", json!({"not": "a list"})),
        ("settings", json!(["not", "an", "object"])),
        ("follows", json!({"not": "a list"})),
        ("approvals", json!({"not": "a list"})),
        ("v2_commit", json!(12345)),
    ] {
        let mut envelope = minimal_envelope();
        envelope[section] = bad;
        cases.push((
            format!("wrongly typed {section}"),
            envelope,
            "INVALID_ENVELOPE",
        ));
    }

    for (label, envelope, code) in &cases {
        let error = parse(envelope).unwrap_err();
        assert_eq!(error.code(), *code, "case: {label}");
    }

    let error = parse_export("{not json").unwrap_err();
    assert_eq!(error.code(), "INVALID_ENVELOPE", "case: malformed json");
}

/// A valid envelope parses; reserved sections and unknown keys only warn.
#[test]
fn parse_accepts_and_warns() {
    let parsed = parse(&minimal_envelope()).unwrap();
    assert!(parsed.warnings.is_empty(), "case: valid envelope");
    assert_eq!(parsed.doc.instance_id, "instance-1");
    assert_eq!(parsed.doc.format_version, 1);

    let mut cases: Vec<(String, Value, Vec<EnvelopeWarning>)> = Vec::new();
    for section in [
        "playlists",
        "favorites",
        "quotas",
        "user_prefs",
        "wanted_watches",
    ] {
        let mut envelope = minimal_envelope();
        envelope[section] = json!([{"whatever": true}]);
        cases.push((
            format!("reserved section {section}"),
            envelope,
            vec![EnvelopeWarning::ignored_reserved_section(section)],
        ));
    }
    let mut envelope = minimal_envelope();
    envelope["future_section"] = json!({"x": 1});
    cases.push((
        "unknown top-level key".to_owned(),
        envelope,
        vec![EnvelopeWarning::unknown_top_level_key("future_section")],
    ));

    for (label, envelope, warnings) in &cases {
        let parsed = parse(envelope).unwrap();
        assert_eq!(&parsed.warnings, warnings, "case: {label}");
    }
}
