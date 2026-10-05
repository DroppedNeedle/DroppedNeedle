//! Profile sharing: export and import bundles.
//!
//! Bundles are deterministic and inert. The portable document carries the
//! profile with identity fields stripped and script references rewritten to
//! `naming-N`/`tagging-N` keys; the share code is the same document
//! zlib-compressed (level 9) and base64url-encoded behind the `DNLP1:`
//! prefix. Both forms verify against the `sha256:` checksum over the
//! canonical payload. Byte-compatible with v2: either side reads the
//! other's exports.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;
use uuid::Uuid;

use super::invalid;
use super::normalize::normalize;
use super::script::ScriptCompiler;
use crate::ids::IdGenerator;
use crate::runtime_config::sections::{
    ArtistStandardization, ArtworkProvider, FieldMode, LibraryManagement, LibraryManagementProfile,
    Mp3ApePolicy, NamingScript, RawAacTagPolicy, ReplayGainMode, SourceCleanupMode, TaggingScript,
    WavTagPolicy,
};
use crate::settings::error::SettingsError;

/// Bundle document format tag.
pub const PROFILE_BUNDLE_FORMAT: &str = "droppedneedle-library-profile";
/// Bundle document version.
pub const PROFILE_BUNDLE_VERSION: i64 = 1;
/// Export MIME type.
pub const PROFILE_BUNDLE_MIME_TYPE: &str = "application/vnd.droppedneedle.profile+json";
/// Share-code prefix.
pub const PROFILE_SHARE_CODE_PREFIX: &str = "DNLP1:";
/// Largest accepted bundle document (1 MiB).
pub const MAX_PROFILE_BUNDLE_BYTES: usize = 1_048_576;
/// Largest accepted share code (1.5M chars).
pub const MAX_PROFILE_SHARE_CODE_CHARS: usize = 1_500_000;
/// UUID namespace for deterministic preview ids.
pub const PROFILE_PREVIEW_NAMESPACE: &str = "54f7ffdf-3c94-58d4-8c08-7a21304ea02d";
/// Longest profile or script name (v2 `MAX_MANAGEMENT_NAME_LENGTH`).
pub const MAX_MANAGEMENT_NAME_LENGTH: usize = 120;

/// Bundle identity fields: stripped on export, refused on import.
const BUNDLE_IDENTITY_FIELDS: &[&str] = &["id", "preset_origin", "preset_version", "revision"];

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

/// Bundle payload: portable profile plus its script dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortablePayload {
    /// Profile with identity stripped and script ids rewritten to keys.
    pub profile: Value,
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

/// SHA-256 hex of the bundle canonical payload encoding.
fn bundle_hash(payload: &PortablePayload) -> String {
    let value = serde_json::to_value(payload).unwrap_or(Value::Null);
    let mut hasher = Sha256::new();
    hasher.update(bundle_canonical_json(&value).as_bytes());
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

/// Rewrite a profile into portable form: identity fields stripped,
/// script ids rewritten to portable keys, compatibility-only fields
/// dropped (they always take local defaults on import).
fn portable_profile(
    profile: &LibraryManagementProfile,
    naming_keys: &BTreeMap<String, String>,
    tagging_keys: &BTreeMap<String, String>,
) -> Result<Value, SettingsError> {
    let mut value = serde_json::to_value(profile)
        .map_err(|_| invalid("The profile could not be prepared for sharing."))?;
    let Value::Object(ref mut map) = value else {
        return Err(invalid("Profile sharing requires an object profile."));
    };
    for field in BUNDLE_IDENTITY_FIELDS {
        map.remove(*field);
    }
    let metadata = map
        .get_mut("metadata")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid("The profile could not be prepared for sharing."))?;
    let tagging_ids = metadata
        .get("tagging_script_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("The profile could not be prepared for sharing."))?;
    let mut portable_tagging = Vec::with_capacity(tagging_ids.len());
    for id in tagging_ids {
        let key = id
            .as_str()
            .and_then(|id| tagging_keys.get(id))
            .ok_or_else(|| {
                invalid("The profile references a script that is no longer available.")
            })?;
        portable_tagging.push(Value::String(key.clone()));
    }
    metadata.insert(
        "tagging_script_ids".to_owned(),
        Value::Array(portable_tagging),
    );
    if let Some(compatibility) = metadata
        .get_mut("format_compatibility")
        .and_then(Value::as_object_mut)
    {
        compatibility.remove("constrained_genres_primary_only");
    }
    let organization = map
        .get_mut("organization")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid("The profile could not be prepared for sharing."))?;
    let naming_key = organization
        .get("naming_script_id")
        .and_then(Value::as_str)
        .and_then(|id| naming_keys.get(id))
        .ok_or_else(|| invalid("The profile references a script that is no longer available."))?
        .clone();
    organization.insert("naming_script_id".to_owned(), Value::String(naming_key));
    if organization
        .get("multi_disc_naming_script_id")
        .is_some_and(|id| !id.is_null())
    {
        let key = organization
            .get("multi_disc_naming_script_id")
            .and_then(Value::as_str)
            .and_then(|id| naming_keys.get(id))
            .ok_or_else(|| invalid("The profile references a script that is no longer available."))?
            .clone();
        organization.insert("multi_disc_naming_script_id".to_owned(), Value::String(key));
    }
    let artwork = map
        .get_mut("artwork")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid("The profile could not be prepared for sharing."))?;
    if artwork
        .get("external_naming_script_id")
        .is_some_and(|id| !id.is_null())
    {
        let key = artwork
            .get("external_naming_script_id")
            .and_then(Value::as_str)
            .and_then(|id| naming_keys.get(id))
            .ok_or_else(|| invalid("The profile references a script that is no longer available."))?
            .clone();
        artwork.insert("external_naming_script_id".to_owned(), Value::String(key));
    }
    if let Some(notification) = map.get_mut("notification").and_then(Value::as_object_mut) {
        notification.remove("refresh_droppedneedle");
    }
    Ok(value)
}

/// Export a profile as a share bundle: pretty document, share code,
/// and hash. The profile keeps its effective settings but loses its
/// identity; referenced scripts travel with it under portable keys.
pub fn export_profile_bundle(
    profile: &LibraryManagementProfile,
    naming_scripts: &[NamingScript],
    tagging_scripts: &[TaggingScript],
    ids: &dyn IdGenerator,
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
        profile.artwork.external_naming_script_id.clone(),
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
    let digest = bundle_hash(&payload);
    let document_value = serde_json::json!({
        "format": PROFILE_BUNDLE_FORMAT,
        "version": PROFILE_BUNDLE_VERSION,
        "payload": payload,
        "checksum": format!("sha256:{digest}"),
    });
    let mut document = serde_json::to_string_pretty(&document_value)
        .map_err(|cause| SettingsError::internal(&cause, ids))?;
    document.push('\n');
    if document.len() > MAX_PROFILE_BUNDLE_BYTES {
        return Err(invalid("This profile is too large to share."));
    }
    let canonical = bundle_canonical_json(&document_value);
    let share_code = encode_share_code(canonical.as_bytes(), ids)?;
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
/// base64url without padding, behind the `DNLP1:` prefix).
fn encode_share_code(document: &[u8], ids: &dyn IdGenerator) -> Result<String, SettingsError> {
    use base64::Engine;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(document)
        .map_err(|cause| SettingsError::internal(&cause, ids))?;
    let compressed = encoder
        .finish()
        .map_err(|cause| SettingsError::internal(&cause, ids))?;
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

/// Parse a bundle document or share code: structural decode, format
/// and version gate, unknown-field rejection, then checksum verify.
pub fn parse_profile_bundle(content: &str) -> Result<ParsedProfileBundle, SettingsError> {
    let stripped = content.trim();
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
    let raw: Value = serde_json::from_slice(&document_bytes)
        .map_err(|_| invalid("The profile bundle is not valid versioned JSON."))?;
    let document: PortableDocument = serde_json::from_value(raw)
        .map_err(|_| invalid("The profile bundle is not valid versioned JSON."))?;
    if document.format != PROFILE_BUNDLE_FORMAT {
        return Err(invalid(
            "This file is not a DroppedNeedle Library Management profile.",
        ));
    }
    if document.version != PROFILE_BUNDLE_VERSION {
        return Err(invalid(&format!(
            "Unsupported profile bundle version: {}.",
            document.version
        )));
    }
    reject_unknown_profile_fields(&document.payload.profile, &[])?;
    let digest = bundle_hash(&document.payload);
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

/// Allowed object keys at a profile path. Mirrors the v2 schema walk:
/// identity fields are refused at the root (export strips them) and
/// the compatibility-only fields are refused where they would sit
/// (import always takes local defaults for those).
fn allowed_profile_keys(path: &[&str]) -> Option<&'static [&'static str]> {
    let keys: &'static [&'static str] = match path {
        [] => &[
            "name",
            "description",
            "metadata",
            "genres",
            "artwork",
            "organization",
            "file_behavior",
            "enrichment",
            "identity",
            "notification",
        ],
        ["metadata"] => &[
            "enabled",
            "fields",
            "artist_credits",
            "relationships",
            "tagging_script_ids",
            "preserve_fields",
            "scrub_unmanaged_tags",
            "preserve_embedded_art_during_scrub",
            "format_compatibility",
        ],
        ["metadata", "fields"] => &["field", "mode", "clear_when_canonical_missing"],
        ["metadata", "artist_credits"] => {
            &["standardization", "translate_names", "preferred_locales"]
        }
        ["metadata", "relationships"] => &["enabled", "types"],
        ["metadata", "format_compatibility"] => &[
            "id3_version",
            "id3v23_join_delimiter",
            "id3_text_encoding",
            "remove_id3_from_flac",
            "mp3_apev2_policy",
            "raw_aac_tag_policy",
            "wav_tag_policy",
        ],
        ["genres"] => &[
            "enabled",
            "mode",
            "sources",
            "maximum_count",
            "musicbrainz_minimum_count",
            "listenbrainz_minimum_count",
            "lastfm_minimum_weight",
            "listenbrainz_curated_only",
            "lastfm_whitelist_only",
            "canonicalize",
            "maximum_ancestry_depth",
            "allowlist",
            "denylist",
            "aliases",
            "preferred_casing",
            "write_primary_only_for_constrained_formats",
        ],
        ["genres", "aliases"] => &["source", "target"],
        ["artwork"] => &[
            "embedded_enabled",
            "external_enabled",
            "providers",
            "approved_only",
            "download_size",
            "local_file_patterns",
            "image_types",
            "minimum_width",
            "minimum_height",
            "embedded_maximum_size",
            "embedded_format",
            "external_maximum_size",
            "external_format",
            "embedded_front_only",
            "external_front_only",
            "never_replace_with_smaller",
            "preserve_existing_types",
            "external_naming_script_id",
            "overwrite_external_files",
        ],
        ["organization"] => &[
            "rename_enabled",
            "move_enabled",
            "naming_script_id",
            "multi_disc_naming_script_id",
            "compatibility",
            "move_sidecars",
            "sidecar_patterns",
            "source_cleanup",
            "remove_empty_directories",
        ],
        ["organization", "compatibility"] => &[
            "windows_compatible",
            "replace_non_ascii",
            "replace_spaces_with_underscores",
            "separator_replacement",
            "maximum_component_length",
            "maximum_path_length",
            "unicode_normalization",
            "extension_case",
            "windows_legacy_path_limit",
        ],
        ["file_behavior"] => &[
            "preserve_timestamps",
            "preserve_permissions",
            "strict_capability_gate",
            "reject_symlinks",
            "validate_written_metadata",
            "validate_technical_audio",
        ],
        ["enrichment"] => &["lyrics", "replaygain"],
        ["enrichment", "lyrics"] => &[
            "enabled",
            "provider",
            "write_plain",
            "write_synced",
            "preserve_existing",
            "required",
        ],
        ["enrichment", "replaygain"] => &["enabled", "mode", "album_aware", "required"],
        ["identity"] => &["automatic_edition_acceptance_enabled"],
        ["notification"] => &["refresh_external_servers"],
        _ => return None,
    };
    Some(keys)
}

/// Reject unknown or unsupported profile fields, naming the first
/// offender by dotted path. Arrays check their items against the
/// array's own path (matching v2's schema walk).
fn reject_unknown_profile_fields(value: &Value, path: &[&str]) -> Result<(), SettingsError> {
    match value {
        Value::Object(map) => {
            if let Some(allowed) = allowed_profile_keys(path) {
                let mut unknown: Vec<&str> = map
                    .keys()
                    .filter(|key| !allowed.contains(&key.as_str()))
                    .map(String::as_str)
                    .collect();
                if !unknown.is_empty() {
                    unknown.sort();
                    let mut location: Vec<&str> = path.to_vec();
                    location.push(unknown[0]);
                    return Err(invalid(&format!(
                        "Unknown or unsupported profile field: {}.",
                        location.join(".")
                    )));
                }
            }
            for (key, item) in map {
                let mut child = path.to_vec();
                child.push(key.as_str());
                // Borrow-safe: recurse with a scratch join.
                reject_unknown_profile_fields_owned(item, child)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                if item.is_object() {
                    reject_unknown_profile_fields(item, path)?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn reject_unknown_profile_fields_owned(
    value: &Value,
    path: Vec<&str>,
) -> Result<(), SettingsError> {
    // Only descend where the schema has object shapes; leaves and
    // unknown branches stop here (the parent already gated the key).
    match path.as_slice() {
        []
        | ["metadata"]
        | ["metadata", "fields"]
        | ["metadata", "artist_credits"]
        | ["metadata", "relationships"]
        | ["metadata", "format_compatibility"]
        | ["genres"]
        | ["genres", "aliases"]
        | ["artwork"]
        | ["organization"]
        | ["organization", "compatibility"]
        | ["file_behavior"]
        | ["enrichment"]
        | ["enrichment", "lyrics"]
        | ["enrichment", "replaygain"]
        | ["identity"]
        | ["notification"] => reject_unknown_profile_fields(value, &path),
        _ => Ok(()),
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

/// Rewrite a parsed payload into a live profile value: portable keys
/// become the assigned script ids, identity is stamped fresh.
fn materialized_profile_value(
    parsed: &ParsedProfileBundle,
    profile_id: &str,
    naming_ids: &BTreeMap<String, String>,
    tagging_ids: &BTreeMap<String, String>,
) -> Result<Value, SettingsError> {
    let mut value = parsed.payload.profile.clone();
    let Value::Object(ref mut map) = value else {
        return Err(invalid("The shared profile is not an object."));
    };
    let metadata_tagging: Vec<String> = map
        .get("metadata")
        .and_then(|metadata| metadata.get("tagging_script_ids"))
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("The shared profile is missing script assignments."))?
        .iter()
        .map(|key| {
            key.as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("The shared tagging-script assignments are invalid."))
        })
        .collect::<Result<_, _>>()?;
    let naming_key = map
        .get("organization")
        .and_then(|organization| organization.get("naming_script_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("The shared naming-script assignment is invalid."))?
        .to_owned();
    let optional_key = |block: &str, field: &str| -> Result<Option<String>, SettingsError> {
        match map.get(block).and_then(|block| block.get(field)) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(key)) => Ok(Some(key.clone())),
            Some(_) => Err(invalid("A shared naming-script assignment is invalid.")),
        }
    };
    let multi_disc_key = optional_key("organization", "multi_disc_naming_script_id")?;
    let external_key = optional_key("artwork", "external_naming_script_id")?;
    if map
        .get("metadata")
        .is_none_or(|metadata| !metadata.is_object())
        || map
            .get("organization")
            .is_none_or(|organization| !organization.is_object())
    {
        return Err(invalid("The shared profile is missing script assignments."));
    }
    let mut referenced_naming = vec![naming_key.clone()];
    referenced_naming.extend(multi_disc_key.clone());
    referenced_naming.extend(external_key.clone());
    validate_dependency_collection(
        &parsed.payload.naming_scripts,
        &ordered_unique(referenced_naming.into_iter().map(Some).collect()),
        "naming script",
    )?;
    validate_dependency_collection(
        &parsed.payload.tagging_scripts,
        &ordered_unique(metadata_tagging.clone().into_iter().map(Some).collect()),
        "tagging script",
    )?;
    let assigned_tagging: Vec<Value> = metadata_tagging
        .iter()
        .map(|key| {
            tagging_ids
                .get(key)
                .map(|id| Value::String(id.clone()))
                .ok_or_else(|| invalid("The shared profile references a missing script."))
        })
        .collect::<Result<_, _>>()?;
    let assigned_naming = naming_ids
        .get(&naming_key)
        .ok_or_else(|| invalid("The shared profile references a missing script."))?;
    let assigned_multi_disc = multi_disc_key
        .map(|key| {
            naming_ids
                .get(&key)
                .cloned()
                .ok_or_else(|| invalid("The shared profile references a missing script."))
        })
        .transpose()?;
    let assigned_external = external_key
        .map(|key| {
            naming_ids
                .get(&key)
                .cloned()
                .ok_or_else(|| invalid("The shared profile references a missing script."))
        })
        .transpose()?;
    if let Some(metadata) = map.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.insert(
            "tagging_script_ids".to_owned(),
            Value::Array(assigned_tagging),
        );
    }
    if let Some(organization) = map.get_mut("organization").and_then(Value::as_object_mut) {
        organization.insert(
            "naming_script_id".to_owned(),
            Value::String(assigned_naming.clone()),
        );
        organization.insert(
            "multi_disc_naming_script_id".to_owned(),
            assigned_multi_disc
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
    }
    if map
        .get("artwork")
        .is_none_or(|artwork| !artwork.is_object())
    {
        map.insert("artwork".to_owned(), Value::Object(Default::default()));
    }
    if let Some(artwork) = map.get_mut("artwork").and_then(Value::as_object_mut) {
        artwork.insert(
            "external_naming_script_id".to_owned(),
            assigned_external.map(Value::String).unwrap_or(Value::Null),
        );
    }
    map.insert("id".to_owned(), Value::String(profile_id.to_owned()));
    map.insert("preset_origin".to_owned(), Value::Null);
    map.insert("preset_version".to_owned(), Value::Null);
    map.insert("revision".to_owned(), Value::String(String::new()));
    Ok(value)
}

/// Materialize a parsed bundle into a live profile and scripts with
/// the given ids, then normalize the detached set. Refuses legacy
/// settings that no longer import (Preserve field modes, artist
/// variations, TheAudioDB artwork).
pub fn materialize_profile_bundle(
    parsed: &ParsedProfileBundle,
    profile_id: &str,
    naming_ids: &BTreeMap<String, String>,
    tagging_ids: &BTreeMap<String, String>,
    compiler: &dyn ScriptCompiler,
) -> Result<MaterializedProfileBundle, SettingsError> {
    let value = materialized_profile_value(parsed, profile_id, naming_ids, tagging_ids)?;
    let profile: LibraryManagementProfile = serde_json::from_value(value)
        .map_err(|_| invalid("The shared profile contains invalid settings."))?;
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
    if profile.metadata.artist_credits.standardization == ArtistStandardization::Variations {
        return Err(invalid(
            "Legacy artist-variation settings cannot be imported.",
        ));
    }
    if profile
        .artwork
        .providers
        .contains(&ArtworkProvider::Audiodb)
    {
        return Err(invalid("TheAudioDB artwork settings cannot be imported."));
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
    normalize(&mut detached, compiler)?;
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
    compiler: &dyn ScriptCompiler,
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
        compiler,
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
    compiler: &dyn ScriptCompiler,
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
    normalize(&mut detached, compiler)?;
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

/// Warnings for a profile under review: destructive tag, container,
/// source, artwork, enrichment, and server-refresh behavior, each
/// with a stable code and severity.
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
    let compatibility = &profile.metadata.format_compatibility;
    if profile.metadata.enabled && compatibility.remove_id3_from_flac {
        warnings.push(import_warning(
            "remove_flac_id3",
            "danger",
            "Removes ID3 tags from FLAC files",
            "Any stray ID3 tag container is deleted when a FLAC file is written.".to_owned(),
        ));
    }
    if profile.metadata.enabled && compatibility.mp3_apev2_policy == Mp3ApePolicy::Remove {
        warnings.push(import_warning(
            "remove_mp3_apev2",
            "danger",
            "Removes APEv2 tags from MP3 files",
            "The complete MP3 APEv2 tag container is deleted.".to_owned(),
        ));
    }
    if profile.metadata.enabled && compatibility.raw_aac_tag_policy == RawAacTagPolicy::RemoveApev2
    {
        warnings.push(import_warning(
            "remove_raw_aac_apev2",
            "danger",
            "Removes APEv2 tags from raw AAC files",
            "The APEv2 tag container and any artwork stored there are deleted.".to_owned(),
        ));
    } else if profile.metadata.enabled
        && compatibility.raw_aac_tag_policy == RawAacTagPolicy::DoNotWrite
    {
        warnings.push(import_warning(
            "skip_raw_aac_tags",
            "warning",
            "Does not write tags to raw AAC files",
            "Managed metadata changes are skipped for raw AAC files.".to_owned(),
        ));
    }
    if profile.metadata.enabled && compatibility.wav_tag_policy != WavTagPolicy::PreserveExisting {
        let wav_format = if compatibility.wav_tag_policy == WavTagPolicy::Id3 {
            "ID3"
        } else {
            "RIFF INFO"
        };
        warnings.push(import_warning(
            "convert_wav_tags",
            "warning",
            "May convert WAV tags",
            format!(
                "WAV metadata is written as {wav_format}, which may replace the current tag representation."
            ),
        ));
    }
    if profile.organization.move_enabled
        && profile.organization.source_cleanup == SourceCleanupMode::RemoveAfterConfirmedMove
    {
        warnings.push(import_warning(
            "remove_sources",
            "danger",
            "Removes verified move sources",
            "Source files are removed after their managed moves are confirmed.".to_owned(),
        ));
    }
    if profile.artwork.external_enabled && profile.artwork.overwrite_external_files {
        warnings.push(import_warning(
            "overwrite_external_artwork",
            "danger",
            "Overwrites external artwork",
            "Existing external artwork files may be replaced.".to_owned(),
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
