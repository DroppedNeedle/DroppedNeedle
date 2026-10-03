//! Envelope briefs: the stage-11 export envelope parses only
//! `droppedneedle-export` at `format_version` 1, requires every top-level
//! key, and ignores reserved sections with a warning instead of failing.

use droppedneedle::export::{EnvelopeWarning, ExportError, parse_export};
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

fn parse(value: &Value) -> Result<droppedneedle::export::ParsedExport, ExportError> {
    parse_export(&serde_json::to_string(value).unwrap())
}

#[test]
fn valid_envelope_parses_without_warnings() {
    let parsed = parse(&minimal_envelope()).unwrap();
    assert!(parsed.warnings.is_empty());
    assert_eq!(parsed.doc.instance_id, "instance-1");
    assert_eq!(parsed.doc.format_version, 1);
}

#[test]
fn rejects_unknown_format() {
    let mut envelope = minimal_envelope();
    envelope["format"] = json!("droppedneedle-backup");
    assert_eq!(parse(&envelope), Err(ExportError::UnsupportedFormat));
    assert_eq!(ExportError::UnsupportedFormat.code(), "UNSUPPORTED_FORMAT");
}

#[test]
fn rejects_unsupported_format_versions() {
    for version in [json!(0), json!(2), json!(99), json!("1"), json!(null)] {
        let mut envelope = minimal_envelope();
        envelope["format_version"] = version;
        let error = parse(&envelope).unwrap_err();
        assert_eq!(error.code(), "UNSUPPORTED_FORMAT_VERSION",);
    }
}

#[test]
fn rejects_each_missing_required_key() {
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
        let error = parse(&envelope).unwrap_err();
        assert_eq!(error.code(), code, "key: {key}");
    }
    let mut envelope = minimal_envelope();
    envelope.as_object_mut().unwrap().remove("users");
    assert_eq!(
        parse(&envelope),
        Err(ExportError::MissingRequiredKey {
            key: "users".to_owned()
        })
    );
}

#[test]
fn reserved_sections_are_ignored_with_warning() {
    for section in [
        "playlists",
        "favorites",
        "quotas",
        "user_prefs",
        "wanted_watches",
        "user_connections",
    ] {
        let mut envelope = minimal_envelope();
        envelope[section] = json!([{"whatever": true}]);
        let parsed = parse(&envelope).unwrap();
        assert_eq!(
            parsed.warnings,
            vec![EnvelopeWarning::ignored_reserved_section(section)],
            "section: {section}"
        );
    }
}

#[test]
fn unknown_top_level_keys_warn_and_parse() {
    let mut envelope = minimal_envelope();
    envelope["future_section"] = json!({"x": 1});
    let parsed = parse(&envelope).unwrap();
    assert_eq!(
        parsed.warnings,
        vec![EnvelopeWarning::unknown_top_level_key("future_section")]
    );
}

#[test]
fn rejects_malformed_json() {
    let error = parse_export("{not json").unwrap_err();
    assert_eq!(error.code(), "INVALID_ENVELOPE");
}

#[test]
fn rejects_wrongly_typed_sections() {
    for (section, bad) in [
        ("users", json!({"not": "a list"})),
        ("settings", json!(["not", "an", "object"])),
        ("follows", json!({"not": "a list"})),
        ("approvals", json!({"not": "a list"})),
        ("v2_commit", json!(12345)),
    ] {
        let mut envelope = minimal_envelope();
        envelope[section] = bad;
        let error = parse(&envelope).unwrap_err();
        assert_eq!(error.code(), "INVALID_ENVELOPE", "section: {section}");
    }
}
