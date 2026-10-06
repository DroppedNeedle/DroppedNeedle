//! The versioned v2-to-v3 export envelope.
//!
//! Version 1 carries accounts, settings, follows and auto-download
//! approvals. Version 2 adds per-user connections (sealed like every other
//! secret), an attached SQLite bundle with the rest of the carried user
//! data (see [`crate::export::sections`]), and a list of what stayed behind
//! in v2 with row counts. The bundle's SHA-256 sits in the document, so the
//! content digest covers it too. Readers accept both versions. Each user's
//! concerts cities and seen marker travel as optional sections at either
//! version.
//!
//! [`parse_export`] enforces the envelope contract: known format and
//! version, every required key present, reserved sections ignored with a
//! warning. Deeper semantic checks (section shapes, secret positions,
//! dangling references) belong to the import validator, which builds on
//! these parsed types.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::export::error::ExportError;

/// The only accepted `format` marker.
pub const EXPORT_FORMAT: &str = "droppedneedle-export";
/// The `format_version` the exporter writes, and the newest one readers
/// accept.
pub const FORMAT_VERSION: u32 = 2;
/// The oldest `format_version` readers still accept.
pub const MIN_FORMAT_VERSION: u32 = 1;

/// Top-level keys every file must carry.
pub const REQUIRED_KEYS: &[&str] = &[
    "format",
    "format_version",
    "exported_at",
    "instance_id",
    "secret_envelope",
    "users",
    "settings",
    "follows",
    "approvals",
];

/// Optional top-level keys a file of any version may carry. The concerts
/// sections came after the first v1 files, so a file without them is still
/// whole (its digest covers only the keys it has).
pub const OPTIONAL_KEYS: &[&str] = &["v2_commit", "event_cities", "event_seen"];

/// Top-level keys a version 2 file carries on top of [`REQUIRED_KEYS`].
pub const REQUIRED_KEYS_V2: &[&str] = &["user_connections", "attachments", "left_behind"];

/// Top-level names kept free for JSON sections. Readers ignore them with a
/// warning instead of failing. The data they name travels in the bundle.
pub const RESERVED_SECTIONS: &[&str] = &[
    "playlists",
    "favorites",
    "quotas",
    "user_prefs",
    "wanted_watches",
];

/// The `role` of the attached SQLite bundle.
pub const BUNDLE_ROLE: &str = "bundle";

/// Required top-level keys for one format version.
#[must_use]
pub fn required_keys(version: u64) -> Vec<&'static str> {
    let mut keys = REQUIRED_KEYS.to_vec();
    if version >= 2 {
        keys.extend_from_slice(REQUIRED_KEYS_V2);
    }
    keys
}

/// Warning codes emitted while parsing.
pub const IGNORED_RESERVED_SECTION: &str = "ignored_reserved_section";
/// Warning codes emitted while parsing.
pub const UNKNOWN_TOP_LEVEL_KEY: &str = "unknown_top_level_key";

/// One non-fatal parsing note. The import report carries these; warnings
/// never block parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeWarning {
    /// Machine-readable code.
    pub code: String,
    /// Human detail naming the key.
    pub detail: String,
}

impl EnvelopeWarning {
    /// A reserved section was skipped by design.
    #[must_use]
    pub fn ignored_reserved_section(section: &str) -> Self {
        Self {
            code: IGNORED_RESERVED_SECTION.to_owned(),
            detail: format!("reserved section '{section}' ignored"),
        }
    }

    /// An unknown top-level key was skipped for forward compatibility.
    #[must_use]
    pub fn unknown_top_level_key(key: &str) -> Self {
        Self {
            code: UNKNOWN_TOP_LEVEL_KEY.to_owned(),
            detail: format!("unknown top-level key '{key}' ignored"),
        }
    }
}

/// Argon2id parameters sealing the envelope secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Key-derivation algorithm, always `argon2id` in v1.
    pub algo: String,
    /// Memory cost in KiB.
    pub m: u32,
    /// Time cost (iterations).
    pub t: u32,
    /// Parallelism.
    pub p: u32,
    /// Base64 salt.
    pub salt_b64: String,
}

/// The secret envelope: how to re-derive the sealing key. Never carries the
/// passphrase or any secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretEnvelope {
    /// Sealing scheme, always `argon2id+xchacha20poly1305` in v1.
    pub scheme: String,
    /// Key-derivation parameters.
    pub kdf: KdfParams,
    /// Base64 envelope nonce (KDF context, never a cipher nonce).
    pub nonce_b64: String,
}

/// How v3 should verify this provider's credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashScheme {
    /// A v2 bcrypt hash: verify, then rehash into the native scheme on login.
    Bcrypt,
    /// Anything else: carried verbatim, never interpreted.
    Opaque,
}

/// Derive the v3 verification scheme for one v2 provider row. `bcrypt` only
/// when the row is a local login whose stored JSON holds a bcrypt hash;
/// every other provider stays opaque.
#[must_use]
pub fn derive_hash_scheme(provider: &str, provider_data: Option<&str>) -> HashScheme {
    if provider != "local" {
        return HashScheme::Opaque;
    }
    let Some(data) = provider_data else {
        return HashScheme::Opaque;
    };
    let parsed: Value = match serde_json::from_str(data) {
        Ok(value) => value,
        Err(_) => return HashScheme::Opaque,
    };
    let hash = parsed
        .get("password_hash")
        .and_then(Value::as_str)
        .unwrap_or("");
    if hash.starts_with("$2a$") || hash.starts_with("$2b$") || hash.starts_with("$2y$") {
        HashScheme::Bcrypt
    } else {
        HashScheme::Opaque
    }
}

/// One v2 auth provider binding. The v2 surrogate id stays behind; identity
/// is the `(provider, provider_uid)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRecord {
    /// Provider name (`local`, `plex`, ...).
    pub provider: String,
    /// Provider-side user id.
    pub provider_uid: String,
    /// Verbatim v2 blob (bcrypt JSON for local logins).
    pub provider_data: Option<String>,
    /// Exporter-derived verification scheme.
    pub hash_scheme: HashScheme,
    /// Verbatim creation timestamp.
    pub created_at: String,
}

/// One sealed app-password blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedValue {
    /// Base64 nonce-plus-ciphertext.
    #[serde(rename = "$sealed")]
    pub sealed: String,
}

/// One app password, active or revoked. The v2 id and the derived SHA-256
/// stay behind; revocation state survives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPasswordRecord {
    /// Display name.
    pub name: String,
    /// Sealed plaintext secret.
    pub secret: SealedValue,
    /// Verbatim creation timestamp.
    pub created_at: String,
    /// Verbatim last-use timestamp.
    pub last_used_at: Option<String>,
    /// Verbatim last client label.
    pub last_client: Option<String>,
    /// True for revoked rows, which are still exported.
    pub revoked: bool,
}

/// One password-recovery code. Hashes travel verbatim and are stored
/// verbatim at import, never re-encrypted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCode {
    /// Hash of the single-use code.
    pub code_hash: String,
    /// Verbatim creation timestamp.
    pub created_at: String,
    /// Verbatim expiry timestamp.
    pub expires_at: String,
}

/// One user account with its providers, app passwords, and recovery code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRecord {
    /// Opaque v2 id, verbatim.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Email address, if set.
    pub email: Option<String>,
    /// Avatar URL, if set.
    pub avatar_url: Option<String>,
    /// Role (`user`, `trusted`, `admin`).
    pub role: String,
    /// Lowercased login name, if set.
    pub username: Option<String>,
    /// Preferred login-name casing, if set.
    pub username_display: Option<String>,
    /// Verbatim creation timestamp.
    pub created_at: String,
    /// Verbatim last-login timestamp.
    pub last_login_at: Option<String>,
    /// Provider bindings.
    pub providers: Vec<ProviderRecord>,
    /// App passwords, revoked rows included.
    pub app_passwords: Vec<AppPasswordRecord>,
    /// Recovery code, if one exists.
    pub recovery_code: Option<RecoveryCode>,
}

/// One followed artist. The derived lowercase MBID stays behind; the
/// importer lowercases `artist_mbid` to form the semantic key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FollowRecord {
    /// Owning user id.
    pub user_id: String,
    /// Artist MBID in original case.
    pub artist_mbid: String,
    /// Display name.
    pub artist_name: String,
    /// Auto-download switch.
    pub auto_download: bool,
    /// Follow time as a Unix timestamp.
    pub followed_at: f64,
    /// Last-change time as a Unix timestamp.
    pub updated_at: f64,
}

/// One auto-download approval. A reviewer id with no exported user is nulled
/// at import; the approval itself survives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// Requesting user id.
    pub user_id: String,
    /// Artist MBID in original case.
    pub artist_mbid: String,
    /// Display name.
    pub artist_name: String,
    /// Review state.
    pub state: String,
    /// Request time as a Unix timestamp.
    pub requested_at: f64,
    /// Reviewer user id, if reviewed.
    pub reviewed_by_id: Option<String>,
    /// Reviewer display name, if reviewed.
    pub reviewed_by_name: Option<String>,
    /// Review time, if reviewed.
    pub reviewed_at: Option<f64>,
    /// Bulk-approval batch, if any.
    pub batch_id: Option<String>,
    /// Origin label, if any.
    pub source: Option<String>,
}

/// One saved concerts city.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventCityRecord {
    /// Owning user id.
    pub user_id: String,
    /// Display name.
    pub city_name: String,
    /// Country code, when known.
    pub country_code: Option<String>,
    /// Latitude in degrees.
    pub latitude: f64,
    /// Longitude in degrees.
    pub longitude: f64,
    /// Match radius in km.
    pub radius_km: f64,
    /// Picker order.
    pub position: i64,
}

/// When one user last opened the concerts page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventSeenRecord {
    /// Owning user id.
    pub user_id: String,
    /// Seen time as a Unix timestamp.
    pub seen_at: f64,
}

/// One per-user service link (Last.fm, ListenBrainz, Spotify, Navidrome,
/// Jellyfin, Plex). v2 stored the link as encrypted JSON; the exporter
/// opens it with the v2 key and seals the plaintext JSON whole. The
/// importer reshapes it into the v3 record for that service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRecord {
    /// Owning user id.
    pub user_id: String,
    /// Service tag, verbatim from v2.
    pub service: String,
    /// v2 enabled switch.
    pub enabled: bool,
    /// Verbatim creation timestamp.
    pub created_at: String,
    /// Verbatim last-change timestamp.
    pub updated_at: String,
    /// The sealed plaintext JSON document.
    pub data: SealedValue,
}

/// One file shipped beside the export JSON, named relative to the export
/// file's directory. The digest covers its hash, so a swapped or changed
/// file fails the import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// What the file is ([`BUNDLE_ROLE`] for the SQLite bundle).
    pub role: String,
    /// File name, no directory part.
    pub name: String,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
    /// File size in bytes.
    pub bytes: u64,
    /// Rows per bundle section, for operators and cross-checks.
    #[serde(default)]
    pub sections: std::collections::BTreeMap<String, u64>,
}

/// v2 data that did not travel, with a count and the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeftBehind {
    /// v2 table (or file set) name.
    pub table: String,
    /// Rows (or files) left behind.
    pub rows: u64,
    /// Why, in plain words.
    pub reason: String,
}

/// The full parsed export document. Settings stay schemaless JSON: v2
/// section keys verbatim, secret positions replaced by sealed objects. The
/// importer maps them onto the clean-slate v3 sections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportDoc {
    /// Format marker.
    pub format: String,
    /// Format version (1 or 2).
    pub format_version: u32,
    /// Export time, RFC 3339 UTC.
    pub exported_at: String,
    /// Best-effort v2 provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v2_commit: Option<String>,
    /// Verbatim v2 instance id.
    pub instance_id: String,
    /// Secret-envelope parameters.
    pub secret_envelope: SecretEnvelope,
    /// User accounts.
    pub users: Vec<UserRecord>,
    /// Settings sections by v2 section key.
    pub settings: Map<String, Value>,
    /// Followed artists.
    pub follows: Vec<FollowRecord>,
    /// Auto-download approvals.
    pub approvals: Vec<ApprovalRecord>,
    /// Saved concerts cities; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_cities: Vec<EventCityRecord>,
    /// Concerts seen markers; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_seen: Vec<EventSeenRecord>,
    /// Per-user service links (version 2).
    #[serde(default)]
    pub user_connections: Vec<ConnectionRecord>,
    /// Files shipped beside the JSON (version 2).
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// What stayed behind in v2 (version 2).
    #[serde(default)]
    pub left_behind: Vec<LeftBehind>,
    /// HMAC over every other top-level key (see `export::seal`). Empty
    /// only while the exporter assembles the document.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_hmac: String,
}

impl ExportDoc {
    /// Render the document as pretty JSON for the export file.
    pub fn to_json_string(&self) -> Result<String, ExportError> {
        serde_json::to_string_pretty(self).map_err(|error| ExportError::InvalidEnvelope {
            reason: format!("cannot serialize export document: {error}"),
        })
    }
}

/// A parsed envelope plus its non-fatal warnings.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedExport {
    /// The typed document.
    pub doc: ExportDoc,
    /// Reserved-section and unknown-key notes.
    pub warnings: Vec<EnvelopeWarning>,
}

/// Parse and envelope-check one export file.
///
/// Order matters: the format marker first (nothing else is
/// interpretable without it), then the version (fail closed outside the
/// supported range), then required keys, then typed section shapes. Reserved sections and
/// unknown top-level keys collect warnings and parse on.
///
/// Typed convenience for tests and tooling, not the import path: the
/// importer uses `ExportFile::parse` plus the validator, which is
/// authoritative and differs in strictness on record details (a missing
/// `display_name` fails here but passes there).
pub fn parse_export(text: &str) -> Result<ParsedExport, ExportError> {
    let root: Value = serde_json::from_str(text).map_err(|_| ExportError::InvalidEnvelope {
        reason: "file is not valid JSON".to_owned(),
    })?;
    let object = root
        .as_object()
        .ok_or_else(|| ExportError::InvalidEnvelope {
            reason: "top level must be a JSON object".to_owned(),
        })?;

    if object.get("format").and_then(Value::as_str) != Some(EXPORT_FORMAT) {
        return Err(ExportError::UnsupportedFormat);
    }
    let version = object
        .get("format_version")
        .and_then(Value::as_u64)
        .filter(|version| {
            (u64::from(MIN_FORMAT_VERSION)..=u64::from(FORMAT_VERSION)).contains(version)
        })
        .ok_or(ExportError::UnsupportedFormatVersion)?;
    let required = required_keys(version);
    for key in &required {
        if !object.contains_key(*key) {
            return Err(ExportError::MissingRequiredKey {
                key: (*key).to_owned(),
            });
        }
    }

    let mut warnings = Vec::new();
    for key in object.keys() {
        if RESERVED_SECTIONS.contains(&key.as_str()) {
            warnings.push(EnvelopeWarning::ignored_reserved_section(key));
        } else if !required.contains(&key.as_str())
            && !OPTIONAL_KEYS.contains(&key.as_str())
            && key != crate::export::seal::DIGEST_KEY
        {
            warnings.push(EnvelopeWarning::unknown_top_level_key(key));
        }
    }

    let doc: ExportDoc =
        serde_json::from_value(root).map_err(|error| ExportError::InvalidEnvelope {
            reason: format!("a section has the wrong shape: {error}"),
        })?;
    Ok(ParsedExport { doc, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_scheme_derivation() {
        let bcrypt = r#"{"password_hash": "$2b$12$abc"}"#;
        assert_eq!(
            derive_hash_scheme("local", Some(bcrypt)),
            HashScheme::Bcrypt
        );
        assert_eq!(
            derive_hash_scheme("local", Some(r#"{"password_hash": "$2y$10$abc"}"#)),
            HashScheme::Bcrypt
        );
        assert_eq!(
            derive_hash_scheme("local", Some(r#"{"password_hash": "md5:abc"}"#)),
            HashScheme::Opaque
        );
        assert_eq!(
            derive_hash_scheme("local", Some("nope")),
            HashScheme::Opaque
        );
        assert_eq!(derive_hash_scheme("local", None), HashScheme::Opaque);
        assert_eq!(derive_hash_scheme("plex", Some(bcrypt)), HashScheme::Opaque);
    }
}
