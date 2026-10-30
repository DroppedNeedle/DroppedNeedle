//! Profile sharing: export and import bundles.
//!
//! Bundles are deterministic and inert. The portable document carries the
//! profile without its identity and with script references rewritten to
//! `naming-N`/`tagging-N` keys; the share code is the same document
//! zlib-compressed (level 9) and base64url-encoded behind the `DNLP2:`
//! prefix. Both forms verify against the `sha256:` checksum over the
//! canonical payload. A bundle naming an option this version does not
//! have is refused, and so is one shared from v2 (a `DNLP1:` code or a
//! version 1 document), whose profile shape v3 no longer reads.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;
use uuid::Uuid;

use super::invalid;
use super::normalize::normalize;
use droppedneedle_kernel::runtime_config::sections::{
    ArtworkManagementSettings, EnrichmentManagementSettings, FieldMode, GenreManagementSettings,
    IdentityManagementSettings, LibraryManagement, LibraryManagementProfile,
    MetadataManagementSettings, NamingScript, OrganizationManagementSettings,
    ProfileNotificationSettings, ReplayGainMode, TaggingScript,
};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;

/// Bundle document format tag.
pub const PROFILE_BUNDLE_FORMAT: &str = "droppedneedle-library-profile";
/// Bundle document version.
pub const PROFILE_BUNDLE_VERSION: i64 = 2;
/// Export MIME type.
pub const PROFILE_BUNDLE_MIME_TYPE: &str = "application/vnd.droppedneedle.profile+json";
/// Share-code prefix.
pub const PROFILE_SHARE_CODE_PREFIX: &str = "DNLP2:";
/// Largest accepted bundle document (1 MiB).
pub const MAX_PROFILE_BUNDLE_BYTES: usize = 1_048_576;
/// Largest accepted share code (1.5M chars).
pub const MAX_PROFILE_SHARE_CODE_CHARS: usize = 1_500_000;
/// UUID namespace for deterministic preview ids.
pub const PROFILE_PREVIEW_NAMESPACE: &str = "54f7ffdf-3c94-58d4-8c08-7a21304ea02d";
/// Longest profile or script name (v2 `MAX_MANAGEMENT_NAME_LENGTH`).
pub const MAX_MANAGEMENT_NAME_LENGTH: usize = 120;

/// The document version and share-code prefix v2 wrote.
const V2_BUNDLE_VERSION: i64 = 1;
const V2_SHARE_CODE_PREFIX: &str = "DNLP1:";
const V2_BUNDLE_REFUSED: &str =
    "This profile was shared from DroppedNeedle v2, which v3 cannot read. Share it again from v3.";

/// A script carried by a bundle: portable key plus name and source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableScript {
    /// Portable reference key (`naming-N` / `tagging-N`).
    pub key: String,
    /// Display name.
    pub name: String,
    /// Script source.
    pub source: String,
}

/// A profile as a bundle carries it: the profile section's settings
/// without its identity (id, preset origin and version, revision), with
/// script ids rewritten to portable keys. Any other field is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PortableProfile {
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Metadata block; `tagging_script_ids` holds portable keys.
    pub metadata: MetadataManagementSettings,
    /// Genre block.
    pub genres: GenreManagementSettings,
    /// Artwork block.
    pub artwork: ArtworkManagementSettings,
    /// Organization block; its naming script ids hold portable keys.
    pub organization: OrganizationManagementSettings,
    /// Enrichment block.
    pub enrichment: EnrichmentManagementSettings,
    /// Identity policy.
    pub identity: IdentityManagementSettings,
    /// Notifications.
    pub notification: ProfileNotificationSettings,
}

impl From<&LibraryManagementProfile> for PortableProfile {
    fn from(profile: &LibraryManagementProfile) -> Self {
        // Destructured in full so a new profile field has to be placed
        // here, either carried or left out as identity.
        let LibraryManagementProfile {
            id: _,
            name,
            description,
            preset_origin: _,
            preset_version: _,
            revision: _,
            metadata,
            genres,
            artwork,
            organization,
            enrichment,
            identity,
            notification,
        } = profile.clone();
        Self {
            name,
            description,
            metadata,
            genres,
            artwork,
            organization,
            enrichment,
            identity,
            notification,
        }
    }
}

impl Default for PortableProfile {
    fn default() -> Self {
        Self::from(&LibraryManagementProfile::default())
    }
}

impl PortableProfile {
    /// The live profile under a fresh id, with no preset or revision.
    fn into_profile(self, id: &str) -> LibraryManagementProfile {
        LibraryManagementProfile {
            id: id.to_owned(),
            name: self.name,
            description: self.description,
            preset_origin: None,
            preset_version: None,
            revision: String::new(),
            metadata: self.metadata,
            genres: self.genres,
            artwork: self.artwork,
            organization: self.organization,
            enrichment: self.enrichment,
            identity: self.identity,
            notification: self.notification,
        }
    }
}

/// Bundle payload: portable profile plus its script dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortablePayload {
    /// Profile with identity stripped and script ids rewritten to keys.
    pub profile: PortableProfile,
    /// Naming scripts in reference order.
    #[serde(default)]
    pub naming_scripts: Vec<PortableScript>,
    /// Tagging scripts in reference order.
    #[serde(default)]
    pub tagging_scripts: Vec<PortableScript>,
}

/// Versioned bundle document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableDocument {
    /// Format tag.
    pub format: String,
    /// Document version.
    pub version: i64,
    /// Carried profile and scripts.
    pub payload: PortablePayload,
    /// `sha256:` checksum over the canonical payload.
    pub checksum: String,
}

/// An exported bundle: pretty document, share code, and hash.
#[derive(Debug, Clone)]
pub struct EncodedProfileBundle {
    /// Pretty-printed document text.
    pub document: String,
    /// Compressed share code.
    pub share_code: String,
    /// Canonical payload hash.
    pub bundle_hash: String,
}

/// A parsed bundle: payload plus its verified hash.
#[derive(Debug, Clone)]
pub struct ParsedProfileBundle {
    /// Verified payload.
    pub payload: PortablePayload,
    /// Canonical payload hash.
    pub bundle_hash: String,
}

/// A materialized bundle: profile and scripts with real ids, normalized.
#[derive(Debug, Clone)]
pub struct MaterializedProfileBundle {
    /// Imported profile.
    pub profile: LibraryManagementProfile,
    /// Imported naming scripts.
    pub naming_scripts: Vec<NamingScript>,
    /// Imported tagging scripts.
    pub tagging_scripts: Vec<TaggingScript>,
}

/// One import warning: machine code plus human title and message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = LibraryManagementProfileImportWarning)]
pub struct ProfileImportWarning {
    /// Stable warning code.
    pub code: String,
    /// `warning` or `danger`.
    pub severity: String,
    /// Short title.
    pub title: String,
    /// What the profile will do.
    pub message: String,
}

/// Bundle canonical JSON: sorted keys, compact separators, raw UTF-8,
/// byte-identical to Python's `json.dumps(value, ensure_ascii=False,
/// separators=(",", ":"), sort_keys=True)` for the shapes hashed here.
fn bundle_canonical_json(value: &Value) -> String {
    // serde_json Map is a BTreeMap: keys already in byte order, which
    // matches code-point order for UTF-8. Compact separators and raw
    // UTF-8 output match Python's ensure_ascii=False encoding.
    serde_json::to_string(value).unwrap_or_default()
}

/// SHA-256 hex of the bundle canonical payload encoding. Import hashes
/// the payload as written, so a bundle from a release that lacked a
/// field added since still verifies.
fn bundle_hash(payload: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bundle_canonical_json(payload).as_bytes());
    format!("{:x}", hasher.finalize())
}

fn ordered_unique(values: Vec<Option<String>>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for value in values.into_iter().flatten() {
        if seen.insert(value.clone()) {
            ordered.push(value);
        }
    }
    ordered
}

/// Rewrite a profile into portable form: identity left out and script
/// ids rewritten to portable keys.
fn portable_profile(
    profile: &LibraryManagementProfile,
    naming_keys: &BTreeMap<String, String>,
    tagging_keys: &BTreeMap<String, String>,
) -> Result<PortableProfile, SettingsError> {
    let key_for = |keys: &BTreeMap<String, String>, id: &str| {
        keys.get(id)
            .cloned()
            .ok_or_else(|| invalid("The profile references a script that is no longer available."))
    };
    let mut portable = PortableProfile::from(profile);
    portable.metadata.tagging_script_ids = profile
        .metadata
        .tagging_script_ids
        .iter()
        .map(|id| key_for(tagging_keys, id))
        .collect::<Result<_, _>>()?;
    portable.organization.naming_script_id =
        key_for(naming_keys, &profile.organization.naming_script_id)?;
    portable.organization.multi_disc_naming_script_id = profile
        .organization
        .multi_disc_naming_script_id
        .as_deref()
        .map(|id| key_for(naming_keys, id))
        .transpose()?;
    Ok(portable)
}

/// Export a profile as a share bundle: pretty document, share code,
/// and hash. The profile keeps its effective settings but loses its
/// identity; referenced scripts travel with it under portable keys.
pub fn export_profile_bundle(
    profile: &LibraryManagementProfile,
    naming_scripts: &[NamingScript],
    tagging_scripts: &[TaggingScript],
) -> Result<EncodedProfileBundle, SettingsError> {
    let naming_by_id: BTreeMap<&str, &NamingScript> = naming_scripts
        .iter()
        .map(|script| (script.id.as_str(), script))
        .collect();
    let tagging_by_id: BTreeMap<&str, &TaggingScript> = tagging_scripts
        .iter()
        .map(|script| (script.id.as_str(), script))
        .collect();
    let naming_ids = ordered_unique(vec![
        Some(profile.organization.naming_script_id.clone()),
        profile.organization.multi_disc_naming_script_id.clone(),
    ]);
    let tagging_ids = ordered_unique(
        profile
            .metadata
            .tagging_script_ids
            .iter()
            .cloned()
            .map(Some)
            .collect(),
    );
    let mut selected_naming = Vec::with_capacity(naming_ids.len());
    for id in &naming_ids {
        let Some(script) = naming_by_id.get(id.as_str()) else {
            return Err(invalid(
                "The profile references a script that is no longer available.",
            ));
        };
        selected_naming.push(*script);
    }
    let mut selected_tagging = Vec::with_capacity(tagging_ids.len());
    for id in &tagging_ids {
        let Some(script) = tagging_by_id.get(id.as_str()) else {
            return Err(invalid(
                "The profile references a script that is no longer available.",
            ));
        };
        selected_tagging.push(*script);
    }
    let naming_keys: BTreeMap<String, String> = selected_naming
        .iter()
        .enumerate()
        .map(|(index, script)| (script.id.clone(), format!("naming-{}", index + 1)))
        .collect();
    let tagging_keys: BTreeMap<String, String> = selected_tagging
        .iter()
        .enumerate()
        .map(|(index, script)| (script.id.clone(), format!("tagging-{}", index + 1)))
        .collect();
    let payload = PortablePayload {
        profile: portable_profile(profile, &naming_keys, &tagging_keys)?,
        naming_scripts: selected_naming
            .iter()
            .map(|script| PortableScript {
                key: naming_keys[script.id.as_str()].clone(),
                name: script.name.clone(),
                source: script.source.clone(),
            })
            .collect(),
        tagging_scripts: selected_tagging
            .iter()
            .map(|script| PortableScript {
                key: tagging_keys[script.id.as_str()].clone(),
                name: script.name.clone(),
                source: script.source.clone(),
            })
            .collect(),
    };
    let payload_value =
        serde_json::to_value(&payload).map_err(|cause| SettingsError::internal(&cause))?;
    let digest = bundle_hash(&payload_value);
    let document_value = serde_json::json!({
        "format": PROFILE_BUNDLE_FORMAT,
        "version": PROFILE_BUNDLE_VERSION,
        "payload": payload_value,
        "checksum": format!("sha256:{digest}"),
    });
    let mut document = serde_json::to_string_pretty(&document_value)
        .map_err(|cause| SettingsError::internal(&cause))?;
    document.push('\n');
    if document.len() > MAX_PROFILE_BUNDLE_BYTES {
        return Err(invalid("This profile is too large to share."));
    }
    let canonical = bundle_canonical_json(&document_value);
    let share_code = encode_share_code(canonical.as_bytes())?;
    if share_code.len() > MAX_PROFILE_SHARE_CODE_CHARS {
        return Err(invalid(
            "This profile is too large to encode as a share code.",
        ));
    }
    Ok(EncodedProfileBundle {
        document,
        share_code,
        bundle_hash: digest,
    })
}

/// Compress a canonical document into a share code (zlib level 9,
/// base64url without padding, behind the share-code prefix).
fn encode_share_code(document: &[u8]) -> Result<String, SettingsError> {
    use base64::Engine;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(document)
        .map_err(|cause| SettingsError::internal(&cause))?;
    let compressed = encoder
        .finish()
        .map_err(|cause| SettingsError::internal(&cause))?;
    Ok(format!(
        "{PROFILE_SHARE_CODE_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&compressed)
    ))
}

/// Decode a share code back into the canonical document bytes.
/// Refuses oversized, malformed, corrupt, trailing-data, and
/// incomplete payloads with the v2 messages.
fn decode_share_code(content: &str) -> Result<Vec<u8>, SettingsError> {
    use base64::Engine;
    use flate2::{Decompress, FlushDecompress, Status};
    let encoded = &content[PROFILE_SHARE_CODE_PREFIX.len()..];
    if encoded.is_empty() || content.len() > MAX_PROFILE_SHARE_CODE_CHARS {
        return Err(invalid("The profile share code is empty or too large."));
    }
    let compressed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid("The profile share code is malformed."))?;
    let mut decompress = Decompress::new(true);
    let mut decoded = vec![0u8; MAX_PROFILE_BUNDLE_BYTES + 1];
    let mut total_in = 0usize;
    let mut total_out = 0usize;
    let mut status = Status::Ok;
    while status == Status::Ok {
        let before_in = decompress.total_in();
        let before_out = decompress.total_out();
        status = decompress
            .decompress(
                &compressed[total_in..],
                &mut decoded[total_out..],
                FlushDecompress::Finish,
            )
            .map_err(|_| invalid("The profile share code is corrupted."))?;
        total_in = decompress.total_in() as usize;
        total_out = decompress.total_out() as usize;
        let _ = (before_in, before_out);
        if total_out > MAX_PROFILE_BUNDLE_BYTES {
            return Err(invalid("The decoded profile bundle is too large."));
        }
        if status == Status::BufError {
            break;
        }
    }
    if total_out > MAX_PROFILE_BUNDLE_BYTES {
        return Err(invalid("The decoded profile bundle is too large."));
    }
    if total_in != compressed.len() || status != Status::StreamEnd {
        return Err(invalid(
            "The profile share code contains trailing or incomplete data.",
        ));
    }
    decoded.truncate(total_out);
    Ok(decoded)
}

/// Parse a bundle document or share code: v2 refusal, structural decode,
/// format and version gate, unknown-field rejection, then checksum
/// verify.
pub fn parse_profile_bundle(content: &str) -> Result<ParsedProfileBundle, SettingsError> {
    let stripped = content.trim();
    if stripped.starts_with(V2_SHARE_CODE_PREFIX) {
        return Err(invalid(V2_BUNDLE_REFUSED));
    }
    let document_bytes: Vec<u8> = if stripped.starts_with(PROFILE_SHARE_CODE_PREFIX) {
        decode_share_code(stripped)?
    } else {
        if stripped.is_empty() {
            return Err(invalid("Paste a profile code or choose a .dnprofile file."));
        }
        if stripped.len() > MAX_PROFILE_BUNDLE_BYTES {
            return Err(invalid("The profile bundle is too large."));
        }
        stripped.as_bytes().to_vec()
    };
    let not_versioned_json = || invalid("The profile bundle is not valid versioned JSON.");
    let raw: Value = serde_json::from_slice(&document_bytes).map_err(|_| not_versioned_json())?;
    if !raw.is_object() {
        return Err(not_versioned_json());
    }
    if raw.get("format").and_then(Value::as_str) != Some(PROFILE_BUNDLE_FORMAT) {
        return Err(invalid(
            "This file is not a DroppedNeedle Library Management profile.",
        ));
    }
    match raw.get("version").and_then(Value::as_i64) {
        Some(PROFILE_BUNDLE_VERSION) => {}
        Some(V2_BUNDLE_VERSION) => return Err(invalid(V2_BUNDLE_REFUSED)),
        Some(version) => {
            return Err(invalid(&format!(
                "Unsupported profile bundle version: {version}."
            )));
        }
        None => return Err(not_versioned_json()),
    }
    let raw_payload = raw.get("payload").cloned().unwrap_or(Value::Null);
    let raw_profile = raw_payload.get("profile").cloned().unwrap_or(Value::Null);
    let decoded = serde_json::from_value::<PortableDocument>(raw);
    let known = match &decoded {
        Ok(document) => serde_json::to_value(&document.payload.profile),
        Err(_) => serde_json::to_value(PortableProfile::default()),
    }
    .map_err(|cause| SettingsError::internal(&cause))?;
    if let Some(field) = first_unknown_field(&raw_profile, &known, &mut Vec::new()) {
        return Err(invalid(&format!(
            "Unknown or unsupported profile field: {field}."
        )));
    }
    let document = decoded.map_err(|_| not_versioned_json())?;
    let digest = bundle_hash(&raw_payload);
    if document.checksum != format!("sha256:{digest}") {
        return Err(invalid(
            "The profile bundle checksum does not match its contents.",
        ));
    }
    Ok(ParsedProfileBundle {
        payload: document.payload,
        bundle_hash: digest,
    })
}

/// The first field, as a dotted path, that the bundle's profile has and
/// the typed profile does not: an option this version does not have.
/// Array items are checked against their array's path.
fn first_unknown_field(given: &Value, known: &Value, path: &mut Vec<String>) -> Option<String> {
    match (given, known) {
        (Value::Object(given), Value::Object(known)) => given.iter().find_map(|(key, value)| {
            path.push(key.clone());
            let found = match known.get(key) {
                None => Some(path.join(".")),
                Some(known) => first_unknown_field(value, known, path),
            };
            path.pop();
            found
        }),
        (Value::Array(given), Value::Array(known)) => given
            .iter()
            .zip(known)
            .find_map(|(value, known)| first_unknown_field(value, known, path)),
        _ => None,
    }
}

/// Check a bundle's script collection: keys valid and unique, names
/// unique, and exactly the referenced set (no missing, no spare).
fn validate_dependency_collection(
    scripts: &[PortableScript],
    referenced: &[String],
    label: &str,
) -> Result<(), SettingsError> {
    if scripts
        .iter()
        .any(|script| script.key.is_empty() || script.key.len() > 64)
    {
        return Err(invalid(&format!("A {label} key is invalid.")));
    }
    let keys: BTreeSet<&str> = scripts.iter().map(|script| script.key.as_str()).collect();
    if keys.len() != scripts.len() {
        return Err(invalid(&format!(
            "The profile bundle contains duplicate {label} keys."
        )));
    }
    let names: BTreeSet<String> = scripts
        .iter()
        .map(|script| script.name.to_lowercase())
        .collect();
    if names.len() != scripts.len() {
        return Err(invalid(&format!(
            "The profile bundle contains duplicate {label} names."
        )));
    }
    let referenced: BTreeSet<&str> = referenced.iter().map(String::as_str).collect();
    if keys != referenced {
        return Err(invalid(&format!(
            "The profile bundle has missing or unreferenced {label} dependencies."
        )));
    }
    Ok(())
}

/// Turn a parsed payload into a live profile: portable keys become the
/// assigned script ids, identity is stamped fresh.
fn materialized_profile(
    parsed: &ParsedProfileBundle,
    profile_id: &str,
    naming_ids: &BTreeMap<String, String>,
    tagging_ids: &BTreeMap<String, String>,
) -> Result<LibraryManagementProfile, SettingsError> {
    let portable = &parsed.payload.profile;
    let naming_key = &portable.organization.naming_script_id;
    let multi_disc_key = &portable.organization.multi_disc_naming_script_id;
    let tagging_keys = &portable.metadata.tagging_script_ids;
    let mut referenced_naming = vec![Some(naming_key.clone())];
    referenced_naming.push(multi_disc_key.clone());
    validate_dependency_collection(
        &parsed.payload.naming_scripts,
        &ordered_unique(referenced_naming),
        "naming script",
    )?;
    validate_dependency_collection(
        &parsed.payload.tagging_scripts,
        &ordered_unique(tagging_keys.iter().cloned().map(Some).collect()),
        "tagging script",
    )?;
    let assigned = |ids: &BTreeMap<String, String>, key: &str| {
        ids.get(key)
            .cloned()
            .ok_or_else(|| invalid("The shared profile references a missing script."))
    };
    let assigned_tagging = tagging_keys
        .iter()
        .map(|key| assigned(tagging_ids, key))
        .collect::<Result<_, _>>()?;
    let assigned_naming = assigned(naming_ids, naming_key)?;
    let assigned_multi_disc = multi_disc_key
        .as_deref()
        .map(|key| assigned(naming_ids, key))
        .transpose()?;
    let mut profile = portable.clone().into_profile(profile_id);
    profile.metadata.tagging_script_ids = assigned_tagging;
    profile.organization.naming_script_id = assigned_naming;
    profile.organization.multi_disc_naming_script_id = assigned_multi_disc;
    Ok(profile)
}

/// Materialize a parsed bundle into a live profile and scripts with
/// the given ids, then normalize the detached set. Refuses legacy
/// Preserve field modes.
pub fn materialize_profile_bundle(
    parsed: &ParsedProfileBundle,
    profile_id: &str,
    naming_ids: &BTreeMap<String, String>,
    tagging_ids: &BTreeMap<String, String>,
) -> Result<MaterializedProfileBundle, SettingsError> {
    let profile = materialized_profile(parsed, profile_id, naming_ids, tagging_ids)?;
    if profile
        .metadata
        .fields
        .iter()
        .any(|field| field.mode == FieldMode::Preserve)
    {
        return Err(invalid(
            "Legacy Preserve metadata modes cannot be imported.",
        ));
    }
    let scripts = |portable: &[PortableScript], assigned: &BTreeMap<String, String>| {
        portable
            .iter()
            .map(|script| {
                assigned
                    .get(&script.key)
                    .cloned()
                    .ok_or_else(|| invalid("The shared profile references a missing script."))
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let naming_assigned = scripts(&parsed.payload.naming_scripts, naming_ids)?;
    let tagging_assigned = scripts(&parsed.payload.tagging_scripts, tagging_ids)?;
    let mut detached = LibraryManagement {
        profiles: vec![profile],
        naming_scripts: parsed
            .payload
            .naming_scripts
            .iter()
            .zip(naming_assigned)
            .map(|(script, id)| NamingScript {
                id,
                name: script.name.clone(),
                source: script.source.clone(),
                ..NamingScript::default()
            })
            .collect(),
        tagging_scripts: parsed
            .payload
            .tagging_scripts
            .iter()
            .zip(tagging_assigned)
            .map(|(script, id)| TaggingScript {
                id,
                name: script.name.clone(),
                source: script.source.clone(),
                ..TaggingScript::default()
            })
            .collect(),
        ..LibraryManagement::default()
    };
    detached.default_profile_id = detached
        .profiles
        .first()
        .map(|profile| profile.id.clone())
        .unwrap_or_default();
    normalize(&mut detached)?;
    let mut materialized = detached.profiles.into_iter();
    Ok(MaterializedProfileBundle {
        profile: materialized
            .next()
            .ok_or_else(|| invalid("The shared profile contains invalid settings."))?,
        naming_scripts: detached.naming_scripts,
        tagging_scripts: detached.tagging_scripts,
    })
}

/// UUIDv5 (SHA-1 namespace hash, RFC 4122): hand-rolled over the
/// sha1 crate so previews stay deterministic without extra deps.
fn uuid_v5(namespace: &Uuid, name: &[u8]) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(namespace.as_bytes());
    hasher.update(name);
    let mut bytes: [u8; 16] = hasher.finalize()[..16].try_into().unwrap_or([0u8; 16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

/// Materialize a bundle for preview with deterministic ids: UUID5 over
/// the preview namespace of `bundle_hash:kind:key`, so reviewing the
/// same bundle twice shows the same shape.
pub fn preview_materialized_profile(
    parsed: &ParsedProfileBundle,
) -> Result<MaterializedProfileBundle, SettingsError> {
    let namespace = Uuid::parse_str(PROFILE_PREVIEW_NAMESPACE).unwrap_or(Uuid::NAMESPACE_URL);
    let stable_id = |kind: &str, key: &str| {
        uuid_v5(
            &namespace,
            format!("{}:{kind}:{key}", parsed.bundle_hash).as_bytes(),
        )
    };
    let naming_ids: BTreeMap<String, String> = parsed
        .payload
        .naming_scripts
        .iter()
        .map(|script| (script.key.clone(), stable_id("naming", &script.key)))
        .collect();
    let tagging_ids: BTreeMap<String, String> = parsed
        .payload
        .tagging_scripts
        .iter()
        .map(|script| (script.key.clone(), stable_id("tagging", &script.key)))
        .collect();
    materialize_profile_bundle(
        parsed,
        &stable_id("profile", "profile"),
        &naming_ids,
        &tagging_ids,
    )
}

/// Pick a free import name: the base when free, otherwise `base
/// (imported)`, `base (imported 2)`, and so on, truncated to the name
/// limit. Comparison is case-insensitive.
pub fn unique_import_name(base: &str, used_names: &BTreeSet<String>) -> String {
    if !used_names.contains(&base.to_lowercase()) {
        return base.to_owned();
    }
    let mut number: Option<u32> = None;
    loop {
        let suffix = match number {
            None => " (imported)".to_owned(),
            Some(n) => format!(" (imported {n})"),
        };
        let keep = MAX_MANAGEMENT_NAME_LENGTH.saturating_sub(suffix.len());
        let prefix: String = base.chars().take(keep).collect();
        let candidate = format!("{}{suffix}", prefix.trim_end());
        if !used_names.contains(&candidate.to_lowercase()) {
            return candidate;
        }
        number = Some(number.map_or(2, |n| n + 1));
    }
}

/// Resolve a materialized import against live settings: free names for
/// the profile and scripts, then a detached normalize so the preview
/// shows exactly what the import would save.
pub fn resolve_import_names(
    materialized: &MaterializedProfileBundle,
    settings: &LibraryManagement,
) -> Result<MaterializedProfileBundle, SettingsError> {
    let mut renamed = materialized.clone();
    let used_profiles: BTreeSet<String> = settings
        .profiles
        .iter()
        .map(|profile| profile.name.to_lowercase())
        .collect();
    renamed.profile.name = unique_import_name(&renamed.profile.name, &used_profiles);
    let mut used_naming: BTreeSet<String> = settings
        .naming_scripts
        .iter()
        .map(|script| script.name.to_lowercase())
        .collect();
    for script in &mut renamed.naming_scripts {
        script.name = unique_import_name(&script.name, &used_naming);
        used_naming.insert(script.name.to_lowercase());
    }
    let mut used_tagging: BTreeSet<String> = settings
        .tagging_scripts
        .iter()
        .map(|script| script.name.to_lowercase())
        .collect();
    for script in &mut renamed.tagging_scripts {
        script.name = unique_import_name(&script.name, &used_tagging);
        used_tagging.insert(script.name.to_lowercase());
    }
    let mut detached = LibraryManagement {
        profiles: vec![renamed.profile.clone()],
        naming_scripts: renamed.naming_scripts.clone(),
        tagging_scripts: renamed.tagging_scripts.clone(),
        ..LibraryManagement::default()
    };
    detached.default_profile_id = detached
        .profiles
        .first()
        .map(|profile| profile.id.clone())
        .unwrap_or_default();
    normalize(&mut detached)?;
    let mut profiles = detached.profiles.into_iter();
    Ok(MaterializedProfileBundle {
        profile: profiles
            .next()
            .ok_or_else(|| invalid("The shared profile contains invalid settings."))?,
        naming_scripts: detached.naming_scripts,
        tagging_scripts: detached.tagging_scripts,
    })
}

/// Export filename for a shared profile: NFKD-folded, slugified,
/// plus `.dnprofile`.
pub fn profile_bundle_filename(name: &str) -> String {
    let ascii: String = name.nfkd().filter(|c| c.is_ascii()).collect();
    let mut slug = String::new();
    let mut dash = false;
    for c in ascii.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash {
            slug.push('-');
            dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    format!(
        "{}.dnprofile",
        if slug.is_empty() {
            "library-profile"
        } else {
            slug
        }
    )
}

/// Human aspects a profile touches, in display order: tags, genres,
/// artwork, lyrics, ReplayGain, rename, move.
pub fn profile_aspects(profile: &LibraryManagementProfile) -> Vec<String> {
    let mut aspects = Vec::new();
    if profile.metadata.enabled {
        aspects.push("Metadata tags".to_owned());
    }
    if profile.genres.enabled {
        aspects.push("Genres".to_owned());
    }
    if profile.artwork.embedded_enabled || profile.artwork.external_enabled {
        aspects.push("Artwork".to_owned());
    }
    if profile.enrichment.lyrics.enabled {
        aspects.push("Lyrics".to_owned());
    }
    if profile.enrichment.replaygain.enabled {
        aspects.push("ReplayGain".to_owned());
    }
    if profile.organization.rename_enabled {
        aspects.push("Rename files".to_owned());
    }
    if profile.organization.move_enabled {
        aspects.push("Move files".to_owned());
    }
    aspects
}

fn import_warning(
    code: &str,
    severity: &str,
    title: &str,
    message: String,
) -> ProfileImportWarning {
    ProfileImportWarning {
        code: code.to_owned(),
        severity: severity.to_owned(),
        title: title.to_owned(),
        message,
    }
}

/// Warnings for a profile under review: destructive tag, enrichment, and
/// server-refresh behavior, each with a stable code and severity.
pub fn profile_import_warnings(profile: &LibraryManagementProfile) -> Vec<ProfileImportWarning> {
    let mut warnings = Vec::new();
    if profile.metadata.enabled && profile.metadata.scrub_unmanaged_tags {
        warnings.push(import_warning(
            "scrub_unmanaged_tags",
            "danger",
            "Removes unmanaged tags",
            "Tags outside the managed and preserved field lists will be removed.".to_owned(),
        ));
    }
    if profile.metadata.enabled
        && profile
            .metadata
            .fields
            .iter()
            .any(|field| field.mode == FieldMode::Replace && field.clear_when_canonical_missing)
    {
        warnings.push(import_warning(
            "clear_missing_metadata",
            "danger",
            "Clears missing canonical values",
            "Selected Replace fields may be cleared when MusicBrainz has no value.".to_owned(),
        ));
    }
    let mut replacement_enrichment = Vec::new();
    if profile.enrichment.lyrics.enabled && !profile.enrichment.lyrics.preserve_existing {
        replacement_enrichment.push("lyrics");
    }
    if profile.enrichment.replaygain.enabled
        && profile.enrichment.replaygain.mode == ReplayGainMode::Replace
    {
        replacement_enrichment.push("ReplayGain");
    }
    if !replacement_enrichment.is_empty() {
        warnings.push(import_warning(
            "replace_enrichment",
            "warning",
            "Replaces enrichment values",
            format!(
                "The profile replaces existing {} values when new values are available.",
                replacement_enrichment.join(" and ")
            ),
        ));
    }
    if profile.notification.refresh_external_servers {
        warnings.push(import_warning(
            "refresh_external_servers",
            "warning",
            "Refreshes external media servers",
            "Configured external media servers are refreshed after publication.".to_owned(),
        ));
    }
    warnings
}
