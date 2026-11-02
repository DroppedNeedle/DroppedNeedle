//! Standalone export validator: error/warning taxonomy only, no DB access.
//!
//! Errors refuse the import; warnings ride along in the report. An optional
//! `--v2-root` cross-check is CLI wiring and lives outside this module.
//!
//! [`read_export`] gives the typed document the plan reads. The header, the
//! top-level keys, the settings and the attachment list are checked on the raw
//! JSON. Every other section decodes record by record: a record that does not
//! decode gets a look at its JSON only to say what is wrong with it, and
//! serde's own text is never shown, because it can quote a value, and a bare
//! secret could be that value.
//!
//! Dangling references fail closed: a follow or approval whose `user_id`
//! has no `users[].id` is a `DANGLING_USER_REF` error, so the whole file is
//! refused and the operator repairs v2 and re-exports. Unknown settings
//! sections are ignored with a warning, never unsealed.

use std::collections::{HashMap, HashSet};

use serde::Deserialize as _;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::envelope::{ENVELOPE_NONCE_LEN, ENVELOPE_SALT_LEN, SEALED_KEY, is_sealed};
use crate::allowlist::{
    ADVANCED_KEPT, INTERNAL_KEPT, REMOVED_FIELDS, REMOVED_PREFIXES, SECRETS, is_removed_field,
    is_removed_section, is_replaced,
};
use crate::export::envelope::HashScheme;
use crate::export::envelope::{
    AppPasswordRecord, ApprovalRecord, BUNDLE_ROLE, ConnectionRecord, EXPORT_FORMAT,
    EventCityRecord, EventSeenRecord, ExportDoc, FORMAT_VERSION, FollowRecord, LeftBehind,
    MIN_FORMAT_VERSION, OPTIONAL_KEYS, ProviderRecord, REQUIRED_KEYS_V2, RESERVED_SECTIONS,
    UserRecord, required_keys,
};
use crate::export::seal::{KDF_ALGO, M_COST_KIB, P_COST, SCHEME, T_COST};
use droppedneedle_auth::times::parse_iso;
use droppedneedle_kernel::config::deployment::DROPPED_ENV_VARS;
use droppedneedle_kernel::runtime_config::DROPPED_SECTION_KEYS;

/// Vestigial v2 `Settings` fields that must never cross the cutover.
const VESTIGIAL_FIELDS: &[&str] = &[
    "cache_ttl_default",
    "cache_ttl_artist",
    "cache_ttl_album",
    "cache_ttl_covers",
    "cache_cleanup_interval",
];

/// Take every removed setting out of one exported section, returning the
/// path of each one taken (`download_policy.quality_recipe[0].target_bitrate_kbps`).
pub(crate) fn strip_removed(section: &str, value: &mut Value) -> Vec<String> {
    let mut taken = Vec::new();
    for path in REMOVED_FIELDS {
        let mut segments = path.split('.');
        let Some(head) = segments.next() else {
            continue;
        };
        let rest: Vec<&str> = segments.collect();
        if head == section {
            strip_path(value, &rest, section.to_owned(), &mut taken);
        } else if head.strip_suffix("[]") == Some(section)
            && let Some(items) = value.as_array_mut()
        {
            for (index, item) in items.iter_mut().enumerate() {
                strip_path(item, &rest, format!("{section}[{index}]"), &mut taken);
            }
        }
    }
    if let Some(object) = value.as_object_mut() {
        let family: Vec<String> = object
            .keys()
            .filter(|field| {
                REMOVED_PREFIXES
                    .iter()
                    .any(|(owner, prefix)| *owner == section && field.starts_with(prefix))
            })
            .cloned()
            .collect();
        for field in family {
            object.remove(&field);
            taken.push(format!("{section}.{field}"));
        }
    }
    taken
}

fn strip_path(value: &mut Value, segments: &[&str], at: String, taken: &mut Vec<String>) {
    let Some((first, rest)) = segments.split_first() else {
        return;
    };
    if let Some(list) = first.strip_suffix("[]") {
        let Some(items) = value.get_mut(list).and_then(Value::as_array_mut) else {
            return;
        };
        for (index, item) in items.iter_mut().enumerate() {
            strip_path(item, rest, format!("{at}.{list}[{index}]"), taken);
        }
        return;
    }
    if rest.is_empty() {
        if let Some(object) = value.as_object_mut()
            && object.remove(*first).is_some()
        {
            taken.push(format!("{at}.{first}"));
        }
        return;
    }
    if let Some(child) = value.get_mut(*first) {
        strip_path(child, rest, format!("{at}.{first}"), taken);
    }
}

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
    read_export(root).0
}

/// Validate one parsed export document and read it as the typed document,
/// which is there only when the file may be imported.
#[must_use]
pub fn read_export(root: &Value) -> (ValidationReport, Option<ExportDoc>) {
    let mut report = ValidationReport::default();
    let Some(object) = root.as_object() else {
        report.errors.push(ValidationIssue::new(
            "EXPORT_NOT_OBJECT",
            String::new(),
            "export root must be a JSON object".to_owned(),
        ));
        return (report, None);
    };

    validate_envelope_header(object, &mut report);
    validate_top_level_keys(object, &mut report);

    let user_ids = validate_users(object.get("users"), &mut report);
    validate_settings(object.get("settings"), &mut report);
    validate_follows(object.get("follows"), &user_ids, &mut report);
    validate_approvals(object.get("approvals"), &user_ids, &mut report);
    validate_event_cities(object.get("event_cities"), &user_ids, &mut report);
    validate_event_seen(object.get("event_seen"), &user_ids, &mut report);
    let carries_v2_sections = object.get("format_version").and_then(Value::as_u64) >= Some(2);
    if carries_v2_sections {
        validate_connections(object.get("user_connections"), &user_ids, &mut report);
        validate_attachments(object.get("attachments"), &mut report);
        validate_left_behind(object.get("left_behind"), &mut report);
    }
    if !report.valid() {
        return (report, None);
    }

    let mut document = object.clone();
    if !carries_v2_sections {
        // A format 1 reader never had them, whatever a file holds there.
        for key in REQUIRED_KEYS_V2 {
            document.remove(*key);
        }
    }
    match serde_json::from_value(Value::Object(document)) {
        Ok(doc) => (report, Some(doc)),
        Err(_) => {
            report.errors.push(ValidationIssue::new(
                "BAD_DOCUMENT",
                String::new(),
                "a top-level field of the export has the wrong type".to_owned(),
            ));
            (report, None)
        }
    }
}

/// How one section's list is refused: its codes and the record message.
struct ListShape {
    name: &'static str,
    not_array: &'static str,
    not_object: &'static str,
    object_message: &'static str,
}

/// Decode a section's records, each with its JSON path. A record that does
/// not decode is refused by name: `explain` says why when it can, from the
/// record's JSON, and otherwise a field has the wrong type.
fn decode_list<T: DeserializeOwned>(
    section: Option<&Value>,
    shape: &ListShape,
    report: &mut ValidationReport,
    mut explain: impl FnMut(&Map<String, Value>, &str, &mut ValidationReport),
) -> Vec<(String, T)> {
    let Some(list) = section else {
        return Vec::new();
    };
    let Some(items) = list.as_array() else {
        report.errors.push(ValidationIssue::new(
            shape.not_array,
            shape.name.to_owned(),
            format!("{} must be a JSON array", shape.name),
        ));
        return Vec::new();
    };
    let mut records = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let path = format!("{}[{index}]", shape.name);
        let Some(record) = item.as_object() else {
            report.errors.push(ValidationIssue::new(
                shape.not_object,
                path,
                shape.object_message.to_owned(),
            ));
            continue;
        };
        match serde_json::from_value::<T>(item.clone()) {
            Ok(decoded) => records.push((path, decoded)),
            Err(_) => {
                let before = report.errors.len();
                explain(record, &path, report);
                if report.errors.len() == before {
                    report.errors.push(ValidationIssue::new(
                        "BAD_RECORD",
                        path,
                        "a field in this record has the wrong type".to_owned(),
                    ));
                }
            }
        }
    }
    records
}

/// A text field of a record that did not decode, empty when it is not text.
fn raw_text<'a>(record: &'a Map<String, Value>, field: &str) -> &'a str {
    record
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Per-user service links: an owner the file carries, a service name, a
/// sealed document, and one link per user and service.
fn validate_connections(
    connections: Option<&Value>,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    let records: Vec<(String, ConnectionRecord)> = decode_list(
        connections,
        &ListShape {
            name: "user_connections",
            not_array: "CONNECTIONS_NOT_ARRAY",
            not_object: "CONNECTION_NOT_OBJECT",
            object_message: "connection record must be a JSON object",
        },
        report,
        |record, path, report| {
            check_connection_owner(raw_text(record, "user_id"), path, user_ids, report);
            if raw_text(record, "service").is_empty() {
                missing_service(path, report);
            }
            match record.get("data") {
                Some(data) if is_sealed(data) => {
                    check_sealed_blob(data, &format!("{path}.data"), report);
                }
                _ => report.errors.push(ValidationIssue::new(
                    "SECRET_NOT_SEALED",
                    format!("{path}.data"),
                    "connection data must be a sealed object".to_owned(),
                )),
            }
        },
    );
    let mut seen = HashSet::new();
    for (path, record) in &records {
        check_connection_owner(&record.user_id, path, user_ids, report);
        if record.service.is_empty() {
            missing_service(path, report);
        } else if !seen.insert((record.user_id.as_str(), record.service.as_str())) {
            report.errors.push(ValidationIssue::new(
                "DUPLICATE_CONNECTION",
                path.clone(),
                "the same user and service appear more than once".to_owned(),
            ));
        }
        check_sealed_text(Some(&record.data.sealed), &format!("{path}.data"), report);
    }
}

fn check_connection_owner(
    user_id: &str,
    path: &str,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    if !user_ids.contains(user_id) {
        report.errors.push(ValidationIssue::new(
            "DANGLING_USER_REF",
            format!("{path}.user_id"),
            format!("user_id {user_id:?} has no matching users[].id"),
        ));
    }
}

fn missing_service(path: &str, report: &mut ValidationReport) {
    report.errors.push(ValidationIssue::new(
        "CONNECTION_MISSING_SERVICE",
        format!("{path}.service"),
        "connection record needs a service name".to_owned(),
    ));
}

/// Files beside the export: a plain file name (never a path), a known
/// role, a SHA-256 and a size. Their content is checked by the importer,
/// which can read them.
fn validate_attachments(attachments: Option<&Value>, report: &mut ValidationReport) {
    let Some(list) = attachments else { return };
    let Some(items) = list.as_array() else {
        report.errors.push(ValidationIssue::new(
            "ATTACHMENTS_NOT_ARRAY",
            "attachments".to_owned(),
            "attachments must be a JSON array".to_owned(),
        ));
        return;
    };
    let mut bundles = 0;
    for (index, item) in items.iter().enumerate() {
        let path = format!("attachments[{index}]");
        let role = item.get("role").and_then(Value::as_str).unwrap_or_default();
        if role == BUNDLE_ROLE {
            bundles += 1;
        } else {
            report.errors.push(ValidationIssue::new(
                "UNKNOWN_ATTACHMENT",
                format!("{path}.role"),
                format!("attachment role {role:?} is not known"),
            ));
        }
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        let plain_name = !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains(['/', '\\'])
            && !name.contains('\0');
        if !plain_name {
            report.errors.push(ValidationIssue::new(
                "BAD_ATTACHMENT_NAME",
                format!("{path}.name"),
                "attachment name must be a plain file name".to_owned(),
            ));
        }
        let sha = item
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let sha_ok = sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !sha_ok || item.get("bytes").and_then(Value::as_u64).is_none() {
            report.errors.push(ValidationIssue::new(
                "BAD_ATTACHMENT_HASH",
                path,
                "attachment needs a hex sha256 and a byte size".to_owned(),
            ));
        }
    }
    if bundles > 1 {
        report.errors.push(ValidationIssue::new(
            "UNKNOWN_ATTACHMENT",
            "attachments".to_owned(),
            "more than one bundle attached".to_owned(),
        ));
    }
}

/// Check every attached file beside the export in `folder`: present, and
/// the very file the export names (same SHA-256 and size). For the CLI's
/// `validate`; the importer runs the same check before it writes.
#[must_use]
pub fn check_attachments(root: &Value, folder: &std::path::Path) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let Some(items) = root.get("attachments").and_then(Value::as_array) else {
        return issues;
    };
    for (index, item) in items.iter().enumerate() {
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        let path = folder.join(name);
        let expected = (
            item.get("sha256")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase(),
            item.get("bytes").and_then(Value::as_u64),
        );
        match crate::export::bundle::file_sha256(&path) {
            Ok((sha, bytes)) if sha == expected.0 && Some(bytes) == expected.1 => {}
            Ok(_) => issues.push(ValidationIssue::new(
                "BUNDLE_MISMATCH",
                format!("attachments[{index}]"),
                format!(
                    "{} is not the file this export was made with",
                    path.display()
                ),
            )),
            Err(error) => issues.push(ValidationIssue::new(
                "BUNDLE_MISSING",
                format!("attachments[{index}]"),
                format!("cannot read {}: {error}", path.display()),
            )),
        }
    }
    issues
}

/// The left-behind list is informational; only its shape is checked.
fn validate_left_behind(left_behind: Option<&Value>, report: &mut ValidationReport) {
    let Some(list) = left_behind else { return };
    let well_formed = list.as_array().is_some_and(|items| {
        items
            .iter()
            .all(|item| LeftBehind::deserialize(item).is_ok())
    });
    if !well_formed {
        report.errors.push(ValidationIssue::new(
            "BAD_LEFT_BEHIND",
            "left_behind".to_owned(),
            "left_behind must list {table, rows, reason, kind} objects".to_owned(),
        ));
    }
}

fn validate_envelope_header(object: &Map<String, Value>, report: &mut ValidationReport) {
    match object.get("format").and_then(Value::as_str) {
        Some(EXPORT_FORMAT) => {}
        _ => report.errors.push(ValidationIssue::new(
            "BAD_FORMAT",
            "format".to_owned(),
            format!("format must equal {EXPORT_FORMAT:?}"),
        )),
    }
    match object.get("format_version").and_then(Value::as_u64) {
        Some(version)
            if (u64::from(MIN_FORMAT_VERSION)..=u64::from(FORMAT_VERSION)).contains(&version) => {}
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
            format!("format_version must be between {MIN_FORMAT_VERSION} and {FORMAT_VERSION}"),
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

fn validate_top_level_keys(object: &Map<String, Value>, report: &mut ValidationReport) {
    let version = object
        .get("format_version")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(MIN_FORMAT_VERSION));
    let required = required_keys(version);
    for key in &required {
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
        if required.contains(&key.as_str())
            || OPTIONAL_KEYS.contains(&key.as_str())
            || key == crate::export::seal::DIGEST_KEY
        {
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
    let records: Vec<(String, UserRecord)> = decode_list(
        users,
        &ListShape {
            name: "users",
            not_array: "USERS_NOT_ARRAY",
            not_object: "USER_NOT_OBJECT",
            object_message: "user record must be a JSON object",
        },
        report,
        |record, path, report| {
            // Still a user the file names, so its rows are not called dangling.
            let id = raw_text(record, "id");
            if id.is_empty() {
                missing_user_id(path, report);
            } else {
                ids.insert(id.to_owned());
            }
            explain_providers(record.get("providers"), path, report);
            explain_app_passwords(record.get("app_passwords"), path, report);
        },
    );
    let mut seen_ids: HashSet<&str> = HashSet::new();
    let mut seen_emails: HashMap<&str, usize> = HashMap::new();
    let mut seen_usernames: HashMap<&str, usize> = HashMap::new();
    let mut seen_bindings: HashSet<(&str, &str)> = HashSet::new();
    for (index, (path, user)) in records.iter().enumerate() {
        if user.id.is_empty() {
            missing_user_id(path, report);
        } else {
            if !seen_ids.insert(&user.id) {
                report.errors.push(ValidationIssue::new(
                    "DUPLICATE_USER_ID",
                    format!("{path}.id"),
                    format!("duplicate users[].id {:?}", user.id),
                ));
            }
            ids.insert(user.id.clone());
        }
        check_unique_field(
            user.email.as_deref(),
            "email",
            index,
            path,
            &mut seen_emails,
            report,
        );
        check_unique_field(
            user.username.as_deref(),
            "username",
            index,
            path,
            &mut seen_usernames,
            report,
        );
        validate_providers(&user.providers, path, &mut seen_bindings, report);
        validate_app_passwords(&user.app_passwords, path, report);
    }
    ids
}

fn missing_user_id(path: &str, report: &mut ValidationReport) {
    report.errors.push(ValidationIssue::new(
        "USER_MISSING_ID",
        format!("{path}.id"),
        "user record must carry a non-empty id".to_owned(),
    ));
}

fn check_unique_field<'a>(
    value: Option<&'a str>,
    field: &str,
    index: usize,
    path: &str,
    seen: &mut HashMap<&'a str, usize>,
    report: &mut ValidationReport,
) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return;
    };
    if let Some(first) = seen.insert(value, index) {
        report.errors.push(ValidationIssue::new(
            "USER_FIELD_COLLISION",
            format!("{path}.{field}"),
            format!("{field} {value:?} already used by users[{first}]"),
        ));
    }
}

/// Say what is wrong with the provider list of a user that did not decode.
fn explain_providers(providers: Option<&Value>, path: &str, report: &mut ValidationReport) {
    let Some(list) = providers.filter(|list| !list.is_null()) else {
        return;
    };
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
        if raw_text(record, "provider").is_empty() || raw_text(record, "provider_uid").is_empty() {
            missing_provider_key(item_path, report);
        }
    }
}

/// Say what is wrong with the app password list of a user that did not decode.
fn explain_app_passwords(passwords: Option<&Value>, path: &str, report: &mut ValidationReport) {
    let Some(list) = passwords.filter(|list| !list.is_null()) else {
        return;
    };
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
    }
}

fn missing_provider_key(path: String, report: &mut ValidationReport) {
    report.errors.push(ValidationIssue::new(
        "PROVIDER_MISSING_KEY",
        path,
        "provider record needs a non-empty provider and provider_uid".to_owned(),
    ));
}

fn validate_providers<'a>(
    providers: &'a [ProviderRecord],
    path: &str,
    seen: &mut HashSet<(&'a str, &'a str)>,
    report: &mut ValidationReport,
) {
    for (index, record) in providers.iter().enumerate() {
        let item_path = format!("{path}.providers[{index}]");
        let (name, uid) = (record.provider.as_str(), record.provider_uid.as_str());
        if name.is_empty() || uid.is_empty() {
            missing_provider_key(item_path, report);
            continue;
        }
        if !seen.insert((name, uid)) {
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

fn validate_local_provider(record: &ProviderRecord, path: &str, report: &mut ValidationReport) {
    if record.hash_scheme != HashScheme::Bcrypt {
        report.errors.push(ValidationIssue::new(
            "BAD_LOCAL_HASH",
            format!("{path}.hash_scheme"),
            "local providers must declare hash_scheme bcrypt".to_owned(),
        ));
    }
    let parsed: Option<Value> = record
        .provider_data
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok());
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

fn validate_app_passwords(
    passwords: &[AppPasswordRecord],
    path: &str,
    report: &mut ValidationReport,
) {
    for (index, record) in passwords.iter().enumerate() {
        let item_path = format!("{path}.app_passwords[{index}]");
        check_sealed_text(
            Some(&record.secret.sealed),
            &format!("{item_path}.secret"),
            report,
        );
        if record.revoked {
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
        if is_removed_section(name) {
            continue;
        }
        if !is_replaced(name) {
            report.warnings.push(ValidationIssue::new(
                "UNKNOWN_SETTINGS_SECTION",
                path,
                format!("unknown settings section {name:?} is ignored"),
            ));
            continue;
        }
        match name.as_str() {
            "advanced_settings" => validate_allowlist(name, section, &path, ADVANCED_KEPT, report),
            "_internal" => validate_allowlist(name, section, &path, INTERNAL_KEPT, report),
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
    name: &str,
    section: &Value,
    path: &str,
    kept: &[&str],
    report: &mut ValidationReport,
) {
    let Some(object) = section.as_object() else {
        return;
    };
    for field in object.keys() {
        let allowed = kept.contains(&field.as_str()) || is_removed_field(name, field);
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
                "lastfm secrets must arrive sealed".to_owned(),
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
    // Every Last.fm field but its switch must arrive sealed: validate_lastfm.
    for (owner, leaf, _) in SECRETS {
        if *owner != section_name || section_name == "lastfm_settings" {
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
    check_sealed_text(sealed.get(SEALED_KEY).and_then(Value::as_str), path, report);
}

fn check_sealed_text(encoded: Option<&str>, path: &str, report: &mut ValidationReport) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
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
    let records: Vec<(String, FollowRecord)> = decode_list(
        follows,
        &ListShape {
            name: "follows",
            not_array: "FOLLOWS_NOT_ARRAY",
            not_object: "FOLLOW_NOT_OBJECT",
            object_message: "follow record must be a JSON object",
        },
        report,
        explain_member(user_ids),
    );
    let mut seen = HashSet::new();
    for (path, record) in &records {
        check_member(&record.user_id, &record.artist_mbid, path, user_ids, report);
        check_duplicate(
            &record.user_id,
            &record.artist_mbid,
            path,
            "DUPLICATE_FOLLOW",
            &mut seen,
            report,
        );
    }
}

/// One row per user and artist (MBID compared case-insensitively, as the
/// table key is): a repeat would abort the import on the primary key.
fn check_duplicate(
    user_id: &str,
    mbid: &str,
    path: &str,
    code: &'static str,
    seen: &mut HashSet<(String, String)>,
    report: &mut ValidationReport,
) {
    if !seen.insert((user_id.to_owned(), mbid.trim().to_lowercase())) {
        report.errors.push(ValidationIssue::new(
            code,
            path.to_owned(),
            "the same user and artist appear more than once".to_owned(),
        ));
    }
}

fn validate_approvals(
    approvals: Option<&Value>,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    let records: Vec<(String, ApprovalRecord)> = decode_list(
        approvals,
        &ListShape {
            name: "approvals",
            not_array: "APPROVALS_NOT_ARRAY",
            not_object: "APPROVAL_NOT_OBJECT",
            object_message: "approval record must be a JSON object",
        },
        report,
        explain_member(user_ids),
    );
    let mut seen = HashSet::new();
    for (path, record) in &records {
        check_member(&record.user_id, &record.artist_mbid, path, user_ids, report);
        check_duplicate(
            &record.user_id,
            &record.artist_mbid,
            path,
            "DUPLICATE_APPROVAL",
            &mut seen,
            report,
        );
        if let Some(reviewer) = record.reviewed_by_id.as_deref()
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

/// Say what is wrong with a follow or approval that did not decode.
fn explain_member(
    user_ids: &HashSet<String>,
) -> impl FnMut(&Map<String, Value>, &str, &mut ValidationReport) + '_ {
    move |record, path, report| {
        check_member(
            raw_text(record, "user_id"),
            raw_text(record, "artist_mbid"),
            path,
            user_ids,
            report,
        );
    }
}

fn check_member(
    user_id: &str,
    mbid: &str,
    path: &str,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    check_user_ref(user_id, path, user_ids, report);
    if !is_mbid(mbid) {
        report.errors.push(ValidationIssue::new(
            "BAD_MBID",
            format!("{path}.artist_mbid"),
            "artist_mbid must be 36 hex-or-dash characters".to_owned(),
        ));
    }
}

/// The record names an exported user.
fn check_user_ref(
    user_id: &str,
    path: &str,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    if user_id.is_empty() {
        report.errors.push(ValidationIssue::new(
            "MISSING_USER_ID",
            format!("{path}.user_id"),
            "record must carry a non-empty user_id".to_owned(),
        ));
    } else if !user_ids.contains(user_id) {
        report.errors.push(ValidationIssue::new(
            "DANGLING_USER_REF",
            format!("{path}.user_id"),
            format!("user_id {user_id:?} has no matching users[].id"),
        ));
    }
}

/// A finite number within `range`, else an error at `path.field`.
fn check_number(
    value: Option<f64>,
    path: &str,
    field: &str,
    range: std::ops::RangeInclusive<f64>,
    report: &mut ValidationReport,
) {
    if !value.is_some_and(|number| range.contains(&number)) {
        report.errors.push(ValidationIssue::new(
            "BAD_NUMBER",
            format!("{path}.{field}"),
            format!(
                "{field} must be a number from {} to {}",
                range.start(),
                range.end()
            ),
        ));
    }
}

fn check_city(
    user_id: &str,
    city_name: &str,
    numbers: [Option<f64>; 3],
    path: &str,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    check_user_ref(user_id, path, user_ids, report);
    if city_name.trim().is_empty() {
        report.errors.push(ValidationIssue::new(
            "MISSING_CITY_NAME",
            format!("{path}.city_name"),
            "city_name must be a non-empty string".to_owned(),
        ));
    }
    let [latitude, longitude, radius] = numbers;
    check_number(latitude, path, "latitude", -90.0..=90.0, report);
    check_number(longitude, path, "longitude", -180.0..=180.0, report);
    check_number(radius, path, "radius_km", 0.0..=f64::MAX, report);
}

/// Concerts cities: a real user, a name, coordinates in range, and one row
/// per user and coordinate pair (the table key).
fn validate_event_cities(
    section: Option<&Value>,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    let records: Vec<(String, EventCityRecord)> = decode_list(
        section,
        &ListShape {
            name: "event_cities",
            not_array: "SECTION_NOT_ARRAY",
            not_object: "RECORD_NOT_OBJECT",
            object_message: "record must be a JSON object",
        },
        report,
        |record, path, report| {
            let number = |field: &str| record.get(field).and_then(Value::as_f64);
            check_city(
                raw_text(record, "user_id"),
                raw_text(record, "city_name"),
                [number("latitude"), number("longitude"), number("radius_km")],
                path,
                user_ids,
                report,
            );
        },
    );
    let mut seen = HashSet::new();
    for (path, record) in &records {
        check_city(
            &record.user_id,
            &record.city_name,
            [
                Some(record.latitude),
                Some(record.longitude),
                Some(record.radius_km),
            ],
            path,
            user_ids,
            report,
        );
        let in_range = (-90.0..=90.0).contains(&record.latitude)
            && (-180.0..=180.0).contains(&record.longitude);
        if in_range
            && !seen.insert((
                record.user_id.as_str(),
                (record.latitude + 0.0).to_bits(),
                (record.longitude + 0.0).to_bits(),
            ))
        {
            report.errors.push(ValidationIssue::new(
                "DUPLICATE_EVENT_CITY",
                path.clone(),
                "the same user and coordinates appear more than once".to_owned(),
            ));
        }
    }
}

/// Concerts seen markers: a real user, a timestamp, one row per user.
fn validate_event_seen(
    section: Option<&Value>,
    user_ids: &HashSet<String>,
    report: &mut ValidationReport,
) {
    let records: Vec<(String, EventSeenRecord)> = decode_list(
        section,
        &ListShape {
            name: "event_seen",
            not_array: "SECTION_NOT_ARRAY",
            not_object: "RECORD_NOT_OBJECT",
            object_message: "record must be a JSON object",
        },
        report,
        |record, path, report| {
            check_user_ref(raw_text(record, "user_id"), path, user_ids, report);
            check_number(
                record.get("seen_at").and_then(Value::as_f64),
                path,
                "seen_at",
                0.0..=f64::MAX,
                report,
            );
        },
    );
    let mut seen = HashSet::new();
    for (path, record) in &records {
        check_user_ref(&record.user_id, path, user_ids, report);
        check_number(
            Some(record.seen_at),
            path,
            "seen_at",
            0.0..=f64::MAX,
            report,
        );
        if !seen.insert(record.user_id.as_str()) {
            report.errors.push(ValidationIssue::new(
                "DUPLICATE_EVENT_SEEN",
                path.clone(),
                "the same user appears more than once".to_owned(),
            ));
        }
    }
}

/// MBID shape: 36 characters of hex or dash after stripping.
fn is_mbid(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.len() == 36
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}
