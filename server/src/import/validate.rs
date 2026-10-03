//! Standalone export validator: error/warning taxonomy only, no DB access.
//!
//! Rule source is `.dev-notes/Plans/RustPort/stage0-export.md` §8. Errors
//! refuse the import; warnings ride along in the report. An optional
//! `--v2-root` cross-check is CLI wiring and lives outside this module.
//!
//! Dangling references fail closed: a follow or approval whose `user_id`
//! has no `users[].id` is a `DANGLING_USER_REF` error, so the whole file is
//! refused and the operator repairs v2 and re-exports. The spec's §2.4
//! drop-and-count branch survives in the merge planner as defense in depth,
//! but it cannot fire through `run_import` because validation runs first.
//! Two rules extend §8: `DUPLICATE_RECOVERY_HASH` (the `code_hash` column
//! is UNIQUE, so a mid-import collision would only fail later and ruder)
//! and the `UNKNOWN_SETTINGS_SECTION` warning (unknown sections are
//! ignored, never unsealed).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::envelope::{ENVELOPE_NONCE_LEN, ENVELOPE_SALT_LEN, SEALED_KEY, is_sealed};
use crate::auth::times::parse_iso;
use crate::export::envelope::{EXPORT_FORMAT, FORMAT_VERSION, REQUIRED_KEYS, RESERVED_SECTIONS};
use crate::export::seal::{KDF_ALGO, M_COST_KIB, P_COST, SCHEME, T_COST};
use crate::runtime_config::DROPPED_SECTION_KEYS;
use crate::runtime_config::deployment::DROPPED_ENV_VARS;

/// Vestigial v2 `Settings` fields (§3.3) that must never cross the cutover.
const VESTIGIAL_FIELDS: &[&str] = &[
    "cache_ttl_default",
    "cache_ttl_artist",
    "cache_ttl_album",
    "cache_ttl_covers",
    "cache_cleanup_interval",
];

/// Explicit `advanced_settings` allowlist (§3.2). The `cache_ttl_*` and
/// `frontend_ttl_*` families are prefix rules handled separately.
const ADVANCED_SETTINGS_KEPT: &[&str] = &[
    "http_timeout",
    "http_connect_timeout",
    "http_max_connections",
    "batch_artist_images",
    "batch_albums",
    "delay_artist",
    "delay_albums",
    "memory_cache_max_entries",
    "memory_cache_cleanup_interval",
    "cover_memory_cache_max_entries",
    "cover_memory_cache_max_size_mb",
    "disk_cache_cleanup_interval",
    "recent_metadata_max_size_mb",
    "recent_covers_max_size_mb",
    "persistent_metadata_ttl_hours",
    "discover_queue_size",
    "discover_queue_ttl",
    "discover_queue_auto_generate",
    "discover_queue_polling_interval",
    "discover_queue_seed_artists",
    "discover_queue_wildcard_slots",
    "discover_picks_count",
    "discover_picks_genre_affinity_weight",
    "audiodb_enabled",
    "audiodb_name_search_fallback",
    "audiodb_api_key",
    "cache_ttl_audiodb_found",
    "cache_ttl_audiodb_not_found",
    "cache_ttl_audiodb_library",
    "prefer_local_cover_art",
    "direct_remote_images_enabled",
    "genre_section_ttl",
    "request_concurrency",
    "request_history_retention_days",
    "ignored_releases_retention_days",
    "orphan_cover_demote_interval_hours",
    "store_prune_interval_hours",
    "sync_stall_timeout_minutes",
    "sync_max_timeout_hours",
];

/// `_internal` allowlist (§3.2).
const INTERNAL_KEPT: &[&str] = &[
    "plex_client_id",
    "droppedneedle_device_id",
    "brainzmash_consent_admin",
];

/// Known secret leaves: (section, dotted path inside the section). A
/// non-empty bare string at one of these is an error; empties mean
/// absence and pass. Paths ending in `[]` apply to every array element
/// (`indexers[].api_key`).
const SECRET_LEAVES: &[(&str, &str)] = &[
    ("download_client", "api_key"),
    ("download_clients", "sabnzbd.api_key"),
    ("indexers", "[].api_key"),
    ("prowlarr", "api_key"),
    ("lidarr_import", "api_key"),
    ("jellyfin_settings", "api_key"),
    ("navidrome_settings", "password"),
    ("plex_settings", "plex_token"),
    ("listenbrainz_settings", "user_token"),
    ("youtube_settings", "api_key"),
    ("spotify_settings", "client_secret"),
    ("events", "ticketmaster_api_key"),
    ("events", "skiddle_api_key"),
    ("wrapped_settings", "api_key"),
    ("oidc_settings", "client_secret"),
    ("library_settings", "acoustid_api_key"),
    ("advanced_settings", "audiodb_api_key"),
];

/// One validator finding. `path` is a dotted JSON path (`users[2].email`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// Stable machine-readable code (`DROPPED_SECTION_PRESENT`).
    pub code: &'static str,
    /// Human sentence describing the finding.
    pub message: String,
    /// JSON path of the offending value, empty for file-level findings.
    pub path: String,
}

impl ValidationIssue {
    fn new(code: &'static str, path: String, message: String) -> Self {
        Self {
            code,
            message,
            path,
        }
    }
}

/// Validator output: errors refuse the import, warnings do not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// Findings that refuse the import.
    pub errors: Vec<ValidationIssue>,
    /// Findings the import proceeds with.
    pub warnings: Vec<ValidationIssue>,
}

impl ValidationReport {
    /// True when the file may be imported.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate one parsed export document. Pure and standalone: no DB, no
/// passphrase, no filesystem.
#[must_use]
pub fn validate_export(root: &Value) -> ValidationReport {
    let mut report = ValidationReport::default();
    let Some(object) = root.as_object() else {
        report.errors.push(ValidationIssue::new(
            "EXPORT_NOT_OBJECT",
            String::new(),
            "export root must be a JSON object".to_owned(),
        ));
        return report;
    };

    validate_envelope_header(object, &mut report);
    validate_top_level_keys(object, &mut report);

    let user_ids = validate_users(object.get("users"), &mut report);
    validate_settings(object.get("settings"), &mut report);
    validate_follows(object.get("follows"), &user_ids, &mut report);
    validate_approvals(object.get("approvals"), &user_ids, &mut report);
    report
}

fn validate_envelope_header(
    object: &serde_json::Map<String, Value>,
    report: &mut ValidationReport,
) {
    match object.get("format").and_then(Value::as_str) {
        Some(EXPORT_FORMAT) => {}
        _ => report.errors.push(ValidationIssue::new(
            "BAD_FORMAT",
            "format".to_owned(),
            format!("format must equal {EXPORT_FORMAT:?}"),
        )),
    }
    match object.get("format_version").and_then(Value::as_u64) {
        Some(version) if version == u64::from(FORMAT_VERSION) => {}
        Some(version) if version > u64::from(FORMAT_VERSION) => {
            report.errors.push(ValidationIssue::new(
                "UNSUPPORTED_FORMAT_VERSION",
                "format_version".to_owned(),
                format!(
                    "format_version {version} is newer than supported {}",
                    FORMAT_VERSION
                ),
            ));
        }
        _ => report.errors.push(ValidationIssue::new(
            "BAD_FORMAT_VERSION",
            "format_version".to_owned(),
            format!("format_version must equal {}", FORMAT_VERSION),
        )),
    }
    match object.get("exported_at").and_then(Value::as_str) {
        Some(stamp) if is_utc_stamp(stamp) => {}
        _ => report.errors.push(ValidationIssue::new(
            "BAD_EXPORTED_AT",
            "exported_at".to_owned(),
            "exported_at must be RFC 3339 UTC".to_owned(),
        )),
    }
    let envelope = object.get("secret_envelope").and_then(Value::as_object);
    let scheme = envelope
        .and_then(|block| block.get("scheme"))
        .and_then(Value::as_str);
    if scheme != Some(SCHEME) {
        report.errors.push(ValidationIssue::new(
            "UNKNOWN_ENVELOPE_SCHEME",
            "secret_envelope.scheme".to_owned(),
            format!("secret_envelope.scheme must equal {SCHEME:?}"),
        ));
    }
    let kdf = envelope
        .and_then(|block| block.get("kdf"))
        .and_then(Value::as_object);
    let pinned = kdf.is_some_and(|block| {
        block.get("algo").and_then(Value::as_str) == Some(KDF_ALGO)
            && block.get("m").and_then(Value::as_u64) == Some(u64::from(M_COST_KIB))
            && block.get("t").and_then(Value::as_u64) == Some(u64::from(T_COST))
            && block.get("p").and_then(Value::as_u64) == Some(u64::from(P_COST))
    });
    if !pinned {
        report.errors.push(ValidationIssue::new(
            "BAD_ENVELOPE_PARAMS",
            "secret_envelope.kdf".to_owned(),
            format!(
                "secret_envelope.kdf must be pinned {KDF_ALGO} m={M_COST_KIB} t={T_COST} p={P_COST}"
            ),
        ));
    }
    check_b64_len(
        kdf.and_then(|block| block.get("salt_b64"))
            .and_then(Value::as_str),
        ENVELOPE_SALT_LEN,
        "secret_envelope.kdf.salt_b64",
        "BAD_ENVELOPE_SALT",
        "kdf salt",
        report,
    );
    check_b64_len(
        envelope
            .and_then(|block| block.get("nonce_b64"))
            .and_then(Value::as_str),
        ENVELOPE_NONCE_LEN,
        "secret_envelope.nonce_b64",
        "BAD_ENVELOPE_NONCE",
        "envelope nonce",
        report,
    );
}

fn check_b64_len(
    encoded: Option<&str>,
    expected: usize,
    path: &str,
    code: &'static str,
    label: &str,
    report: &mut ValidationReport,
) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let ok = encoded.is_some_and(|text| {
        STANDARD
            .decode(text.trim())
            .is_ok_and(|bytes| bytes.len() == expected)
    });
    if !ok {
        report.errors.push(ValidationIssue::new(
            code,
            path.to_owned(),
            format!("{label} must be base64 of exactly {expected} bytes"),
        ));
    }
}

/// True when the stamp parses and carries a UTC designator.
fn is_utc_stamp(stamp: &str) -> bool {
    if parse_iso(stamp).is_none() {
        return false;
    }
    let trimmed = stamp.trim();
    if trimmed.ends_with('Z') || trimmed.ends_with('z') {
        return true;
    }
    trimmed.ends_with("+00:00") || trimmed.ends_with("+0000") || trimmed.ends_with("+00")
}

fn validate_top_level_keys(object: &serde_json::Map<String, Value>, report: &mut ValidationReport) {
    for key in REQUIRED_KEYS {
        if !object.contains_key(*key) {
            report.errors.push(ValidationIssue::new(
                "MISSING_KEY",
                (*key).to_owned(),
                format!("required top-level key {key:?} is missing"),
            ));
        }
    }
    if !object.contains_key("v2_commit") {
        report.warnings.push(ValidationIssue::new(
            "MISSING_V2_COMMIT",
            String::new(),
            "v2_commit is absent; provenance will be incomplete".to_owned(),
        ));
    }
    for key in object.keys() {
        if REQUIRED_KEYS.contains(&key.as_str()) || key == "v2_commit" {
            continue;
        }
        if RESERVED_SECTIONS.contains(&key.as_str()) {
            report.warnings.push(ValidationIssue::new(
                "IGNORED_RESERVED_SECTION",
                key.clone(),
                format!("reserved section {key:?} is ignored by v1 importers"),
            ));
            continue;
        }
        if is_dropped_name(key) {
            report.errors.push(ValidationIssue::new(
                "DROPPED_SECTION_PRESENT",
                key.clone(),
                format!("dropped name {key:?} must not appear in the export"),
            ));
            continue;
        }
        report.warnings.push(ValidationIssue::new(
            "UNKNOWN_TOP_LEVEL_KEY",
            key.clone(),
            format!("unknown top-level key {key:?} is ignored"),
        ));
    }
}

/// True when the key is a dropped section, vestigial field, or dropped
/// env-tier name. `instance_id` is excluded: the export's top-level id is
/// the blessed owner even though the env var is dropped.
fn is_dropped_name(key: &str) -> bool {
    if key.eq_ignore_ascii_case("instance_id") {
        return false;
    }
    if DROPPED_SECTION_KEYS
        .iter()
        .any(|dropped| dropped.eq_ignore_ascii_case(key))
    {
        return true;
    }
    if VESTIGIAL_FIELDS
        .iter()
        .any(|field| field.eq_ignore_ascii_case(key))
    {
        return true;
    }
    DROPPED_ENV_VARS
        .iter()
        .any(|var| var.env_name.eq_ignore_ascii_case(key))
}

fn validate_users(users: Option<&Value>, report: &mut ValidationReport) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Some(list) = users else { return ids };
    let Some(items) = list.as_array() else {
        report.errors.push(ValidationIssue::new(
            "USERS_NOT_ARRAY",
            "users".to_owned(),
            "users must be a JSON array".to_owned(),
        ));
        return ids;
    };
    let mut seen_ids: HashSet<&str> = HashSet::new();
    let mut seen_emails: HashMap<&str, usize> = HashMap::new();
    let mut seen_usernames: HashMap<&str, usize> = HashMap::new();
    let mut seen_bindings: HashSet<(String, String)> = HashSet::new();
    let mut seen_hashes: HashSet<&str> = HashSet::new();
    for (index, user) in items.iter().enumerate() {
        let path = format!("users[{index}]");
        let Some(record) = user.as_object() else {
            report.errors.push(ValidationIssue::new(
                "USER_NOT_OBJECT",
                path,
                "user record must be a JSON object".to_owned(),
            ));
            continue;
        };
        let id = record.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            report.errors.push(ValidationIssue::new(
                "USER_MISSING_ID",
                format!("{path}.id"),
                "user record must carry a non-empty id".to_owned(),
            ));
        } else {
            if !seen_ids.insert(id) {
                report.errors.push(ValidationIssue::new(
                    "DUPLICATE_USER_ID",
                    format!("{path}.id"),
                    format!("duplicate users[].id {id:?}"),
                ));
            }
            ids.insert(id.to_owned());
        }
        check_unique_field(record, "email", index, &path, &mut seen_emails, report);
        check_unique_field(
            record,
            "username",
            index,
            &path,
            &mut seen_usernames,
            report,
        );
        validate_providers(record.get("providers"), &path, &mut seen_bindings, report);
        validate_app_passwords(record.get("app_passwords"), &path, report);
        if let Some(hash) = record
            .get("recovery_code")
            .and_then(Value::as_object)
            .and_then(|recovery| recovery.get("code_hash"))
            .and_then(Value::as_str)
            && !hash.is_empty()
            && !seen_hashes.insert(hash)
        {
            report.errors.push(ValidationIssue::new(
                "DUPLICATE_RECOVERY_HASH",
                format!("{path}.recovery_code.code_hash"),
                "recovery code_hash is already used by another user".to_owned(),
            ));
        }
    }
    ids
}

fn check_unique_field<'a>(
    record: &'a serde_json::Map<String, Value>,
    field: &str,
    index: usize,
    path: &str,
    seen: &mut HashMap<&'a str, usize>,
    report: &mut ValidationReport,
) {
    let Some(value) = record.get(field).and_then(Value::as_str) else {
        return;
    };
    if value.is_empty() {
        return;
    }
    if let Some(first) = seen.insert(value, index) {
        report.errors.push(ValidationIssue::new(
            "USER_FIELD_COLLISION",
            format!("{path}.{field}"),
            format!("{field} {value:?} already used by users[{first}]"),
        ));
    }
}

fn validate_providers(
    providers: Option<&Value>,
    path: &str,
    seen: &mut HashSet<(String, String)>,
    report: &mut ValidationReport,
) {
    let Some(list) = providers else { return };
    let Some(items) = list.as_array() else {
        report.errors.push(ValidationIssue::new(
            "PROVIDERS_NOT_ARRAY",
            format!("{path}.providers"),
            "providers must be a JSON array".to_owned(),
        ));
        return;
    };
    for (index, provider) in items.iter().enumerate() {
        let item_path = format!("{path}.providers[{index}]");
        let Some(record) = provider.as_object() else {
            report.errors.push(ValidationIssue::new(
                "PROVIDER_NOT_OBJECT",
                item_path,
                "provider record must be a JSON object".to_owned(),
            ));
            continue;
        };
        let name = record
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let uid = record
            .get("provider_uid")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if name.is_empty() || uid.is_empty() {
            report.errors.push(ValidationIssue::new(
                "PROVIDER_MISSING_KEY",
                item_path,
                "provider record needs a non-empty provider and provider_uid".to_owned(),
            ));
            continue;
        }
        if !seen.insert((name.to_owned(), uid.to_owned())) {
            report.errors.push(ValidationIssue::new(
                "DUPLICATE_PROVIDER",
                item_path.clone(),
                format!("duplicate provider binding ({name:?}, {uid:?})"),
            ));
        }
        if name == "local" {
            validate_local_provider(record, &item_path, report);
        }
    }
}

fn validate_local_provider(
    record: &serde_json::Map<String, Value>,
    path: &str,
    report: &mut ValidationReport,
) {
    let scheme = record.get("hash_scheme").and_then(Value::as_str);
    if scheme != Some("bcrypt") {
        report.errors.push(ValidationIssue::new(
            "BAD_LOCAL_HASH",
            format!("{path}.hash_scheme"),
            "local providers must declare hash_scheme bcrypt".to_owned(),
        ));
    }
    let data = record.get("provider_data").and_then(Value::as_str);
    let parsed: Option<Value> = data.and_then(|text| serde_json::from_str(text).ok());
    let hash = parsed
        .as_ref()
        .and_then(|json| json.get("password_hash"))
        .and_then(Value::as_str);
    let well_formed = hash.is_some_and(|value| {
        value.starts_with("$2a$") || value.starts_with("$2b$") || value.starts_with("$2y$")
    });
    if !well_formed {
        report.errors.push(ValidationIssue::new(
            "BAD_LOCAL_HASH",
            format!("{path}.provider_data"),
            "local provider_data must be JSON with a bcrypt password_hash".to_owned(),
        ));
    }
}

fn validate_app_passwords(passwords: Option<&Value>, path: &str, report: &mut ValidationReport) {
    let Some(list) = passwords else { return };
    let Some(items) = list.as_array() else {
        report.errors.push(ValidationIssue::new(
            "APP_PASSWORDS_NOT_ARRAY",
            format!("{path}.app_passwords"),
            "app_passwords must be a JSON array".to_owned(),
        ));
        return;
    };
    for (index, entry) in items.iter().enumerate() {
        let item_path = format!("{path}.app_passwords[{index}]");
        let Some(record) = entry.as_object() else {
            report.errors.push(ValidationIssue::new(
                "APP_PASSWORD_NOT_OBJECT",
                item_path,
                "app password record must be a JSON object".to_owned(),
            ));
            continue;
        };
        match record.get("secret") {
            Some(secret) if is_sealed(secret) => {
                check_sealed_blob(secret, &format!("{item_path}.secret"), report);
            }
            _ => report.errors.push(ValidationIssue::new(
                "SECRET_NOT_SEALED",
                format!("{item_path}.secret"),
                "app password secret must be a sealed object".to_owned(),
            )),
        }
        if record.get("revoked").and_then(Value::as_bool) == Some(true) {
            report.warnings.push(ValidationIssue::new(
                "REVOKED_APP_PASSWORD_KEPT",
                item_path,
                "revoked app password is kept revoked by design".to_owned(),
            ));
        }
    }
}

fn validate_settings(settings: Option<&Value>, report: &mut ValidationReport) {
    let Some(block) = settings else { return };
    let Some(sections) = block.as_object() else {
        report.errors.push(ValidationIssue::new(
            "SETTINGS_NOT_OBJECT",
            "settings".to_owned(),
            "settings must be a JSON object".to_owned(),
        ));
        return;
    };
    for (name, section) in sections {
        let path = format!("settings.{name}");
        if is_dropped_name(name) {
            report.errors.push(ValidationIssue::new(
                "DROPPED_SECTION_PRESENT",
                path.clone(),
                format!("dropped section {name:?} must not appear in the export"),
            ));
            continue;
        }
        if !super::pipeline::REPLACE_UNIVERSE.contains(&name.as_str()) {
            report.warnings.push(ValidationIssue::new(
                "UNKNOWN_SETTINGS_SECTION",
                path,
                format!("unknown settings section {name:?} is ignored"),
            ));
            continue;
        }
        match name.as_str() {
            "advanced_settings" => validate_allowlist(
                section,
                &path,
                ADVANCED_SETTINGS_KEPT,
                &["cache_ttl_", "frontend_ttl_"],
                report,
            ),
            "_internal" => validate_allowlist(section, &path, INTERNAL_KEPT, &[], report),
            "indexers" => {
                if !section.is_array() {
                    report.errors.push(ValidationIssue::new(
                        "INDEXERS_NOT_ARRAY",
                        path.clone(),
                        "indexers must be a JSON array so priority order survives".to_owned(),
                    ));
                }
            }
            "lastfm_settings" => validate_lastfm(section, &path, report),
            _ => {}
        }
        check_secret_leaves(name, section, &path, report);
        check_sealed_blobs(section, &path, report);
    }
}

fn validate_allowlist(
    section: &Value,
    path: &str,
    kept: &[&str],
    prefixes: &[&str],
    report: &mut ValidationReport,
) {
    let Some(object) = section.as_object() else {
        return;
    };
    for field in object.keys() {
        let allowed = kept.contains(&field.as_str())
            || prefixes.iter().any(|prefix| field.starts_with(prefix));
        if !allowed {
            report.errors.push(ValidationIssue::new(
                "SETTINGS_FIELD_NOT_ALLOWED",
                format!("{path}.{field}"),
                format!("field {field:?} is outside the export allowlist"),
            ));
        }
    }
}

fn validate_lastfm(section: &Value, path: &str, report: &mut ValidationReport) {
    let Some(object) = section.as_object() else {
        return;
    };
    for (field, value) in object {
        if field == "enabled" {
            continue;
        }
        if let Some(text) = value.as_str()
            && !text.is_empty()
        {
            report.errors.push(ValidationIssue::new(
                "SECRET_NOT_SEALED",
                format!("{path}.{field}"),
                "lastfm secrets must arrive sealed (they are dropped after unlock)".to_owned(),
            ));
        }
    }
}

fn check_secret_leaves(
    section_name: &str,
    section: &Value,
    path: &str,
    report: &mut ValidationReport,
) {
    for (owner, leaf) in SECRET_LEAVES {
        if *owner != section_name {
            continue;
        }
        for (leaf_path, value) in resolve_leaf(section, leaf) {
            if let Some(text) = value.as_str()
                && !text.is_empty()
            {
                report.errors.push(ValidationIssue::new(
                    "SECRET_NOT_SEALED",
                    format!("{path}.{leaf_path}"),
                    "secret field must be a sealed object, not a bare string".to_owned(),
                ));
            }
        }
    }
}

/// Resolve a dotted leaf path against a section, fanning out over arrays
/// for the `[]` step. Returns path/value pairs for reporting.
fn resolve_leaf<'a>(section: &'a Value, leaf: &str) -> Vec<(String, &'a Value)> {
    let mut current = vec![(String::new(), section)];
    for step in leaf.split('.') {
        let mut next = Vec::new();
        for (prefix, node) in current {
            if step == "[]" {
                if let Some(items) = node.as_array() {
                    for (index, item) in items.iter().enumerate() {
                        next.push((format!("{prefix}[{index}]"), item));
                    }
                }
            } else if let Some(child) = node.get(step) {
                let child_path = if prefix.is_empty() {
                    step.to_owned()
                } else {
                    format!("{prefix}.{step}")
                };
                next.push((child_path, child));
            }
        }
        current = next;
    }
    current
}

fn check_sealed_blobs(node: &Value, path: &str, report: &mut ValidationReport) {
    if is_sealed(node) {
        check_sealed_blob(node, path, report);
        return;
    }
    match node {
        Value::Object(object) => {
            for (key, child) in object {
                check_sealed_blobs(child, &format!("{path}.{key}"), report);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                check_sealed_blobs(child, &format!("{path}[{index}]"), report);
            }
        }
        _ => {}
    }
}

fn check_sealed_blob(sealed: &Value, path: &str, report: &mut ValidationReport) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let encoded = sealed.get(SEALED_KEY).and_then(Value::as_str);
    let decoded_len = encoded
        .and_then(|text| STANDARD.decode(text).ok())
        .map(|blob| blob.len());
    match decoded_len {
        Some(len) if len >= super::envelope::MIN_SEALED_LEN => {}
        Some(_) => report.errors.push(ValidationIssue::new(
            "SEALED_TOO_SHORT",
            path.to_owned(),
            "sealed blob is shorter than nonce plus tag".to_owned(),
        )),
        None => report.errors.push(ValidationIssue::new(
            "SEALED_NOT_BASE64",
            path.to_owned(),
            "sealed blob is not valid base64".to_owned(),
        )),
    }
}

fn validate_follows(
    follows: Option<&Value>,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    let Some(list) = follows else { return };
    let Some(items) = list.as_array() else {
        report.errors.push(ValidationIssue::new(
            "FOLLOWS_NOT_ARRAY",
            "follows".to_owned(),
            "follows must be a JSON array".to_owned(),
        ));
        return;
    };
    for (index, follow) in items.iter().enumerate() {
        let path = format!("follows[{index}]");
        let Some(record) = follow.as_object() else {
            report.errors.push(ValidationIssue::new(
                "FOLLOW_NOT_OBJECT",
                path,
                "follow record must be a JSON object".to_owned(),
            ));
            continue;
        };
        validate_member_ref(record, &path, user_ids, report);
    }
}

fn validate_approvals(
    approvals: Option<&Value>,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    let Some(list) = approvals else { return };
    let Some(items) = list.as_array() else {
        report.errors.push(ValidationIssue::new(
            "APPROVALS_NOT_ARRAY",
            "approvals".to_owned(),
            "approvals must be a JSON array".to_owned(),
        ));
        return;
    };
    for (index, approval) in items.iter().enumerate() {
        let path = format!("approvals[{index}]");
        let Some(record) = approval.as_object() else {
            report.errors.push(ValidationIssue::new(
                "APPROVAL_NOT_OBJECT",
                path,
                "approval record must be a JSON object".to_owned(),
            ));
            continue;
        };
        validate_member_ref(record, &path, user_ids, report);
        if let Some(reviewer) = record.get("reviewed_by_id").and_then(Value::as_str)
            && !reviewer.is_empty()
            && !user_ids.contains(reviewer)
        {
            report.warnings.push(ValidationIssue::new(
                "DANGLING_REVIEWER",
                format!("{path}.reviewed_by_id"),
                "reviewer is not an exported user; the field will be nulled".to_owned(),
            ));
        }
    }
}

fn validate_member_ref(
    record: &serde_json::Map<String, Value>,
    path: &str,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    match record.get("user_id").and_then(Value::as_str) {
        Some(user_id) if !user_id.is_empty() => {
            if !user_ids.contains(user_id) {
                report.errors.push(ValidationIssue::new(
                    "DANGLING_USER_REF",
                    format!("{path}.user_id"),
                    format!("user_id {user_id:?} has no matching users[].id"),
                ));
            }
        }
        _ => report.errors.push(ValidationIssue::new(
            "MISSING_USER_ID",
            format!("{path}.user_id"),
            "record must carry a non-empty user_id".to_owned(),
        )),
    }
    let mbid = record
        .get("artist_mbid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_mbid(mbid) {
        report.errors.push(ValidationIssue::new(
            "BAD_MBID",
            format!("{path}.artist_mbid"),
            "artist_mbid must be 36 hex-or-dash characters".to_owned(),
        ));
    }
}

/// Spec §8 MBID shape: 36 characters of hex or dash after stripping.
fn is_mbid(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.len() == 36
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}
