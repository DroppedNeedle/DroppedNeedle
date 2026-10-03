//! Library management settings: profile CRUD, revisions, impact
//! classification, activation health, and profile sharing.
//!
//! Ports v2's profile service over the `library_management` section.
//! The wire DTOs mirror the section field for field, so conversions
//! bridge through JSON values; parity tests pin the shapes equal. All
//! content revisions (settings, profile, script, naming-policy) hash
//! byte-compatibly with v2 (ASCII-escaped canonical JSON), so migrated
//! activations and CAS tokens keep working. Golden vectors minted from
//! v2 pin this in the settings briefs.
//!
//! Script validation rides behind the [`ScriptCompiler`] port: the
//! shipped structural compiler checks the documented rules (naming
//! shape, known variables/targets, append gates); the full expression
//! compiler is library-engine follow-up work behind the same port.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use super::error::SettingsError;
use super::library_policy::stable_hash;
use super::models::{
    ArtistStandardizationDto, ArtworkImageTypeDto, ArtworkProviderDto, FieldModeDto,
    GenreSourceDto, Id3TextEncodingDto, Id3VersionDto, LibraryManagementProfileDto,
    LibraryManagementRootAssignmentDto, LibraryManagementSettingsDto, ManagedFieldDto,
    MetadataManagementSettingsDto, Mp3ApePolicyDto, MultiDiscNamingModeDto, NamingScriptDto,
    OrganizationManagementSettingsDto, RawAacTagPolicyDto, ReplayGainModeDto, SourceCleanupModeDto,
    TaggingScriptDto, WavTagPolicyDto,
};
use droppedneedle::ids::IdGenerator;
use droppedneedle::runtime_config::sections::DEFAULT_NAMING_TEMPLATE;

/// Management settings schema version.
pub const MANAGEMENT_SCHEMA_VERSION: i64 = 1;
/// Preset catalog version.
pub const PRESET_CATALOG_VERSION: i64 = 1;
/// Picard organizer preset version.
pub const PICARD_ORGANIZER_PRESET_VERSION: i64 = 4;
/// Picard organizer profile id.
pub const PICARD_ORGANIZER_PROFILE_ID: &str = "c2741223-da7c-5231-bcf5-7cead27b07d9";
/// Complete organizer profile id.
pub const COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID: &str = "4c012b0e-509b-5f23-a759-65552c84db85";
/// Legacy naming profile id.
pub const LEGACY_NAMING_PROFILE_ID: &str = "b9b1f9b4-752d-54ff-bc67-24f75cb67847";
/// Legacy naming script id.
pub const LEGACY_NAMING_SCRIPT_ID: &str = "ec7b9d83-00f7-5a78-93c7-20d117bef008";
/// Picard single-disc naming script id.
pub const PICARD_NAMING_SCRIPT_ID: &str = "69202666-cb88-52b0-bac2-0afc62b1e909";
/// Picard multi-disc naming script id.
pub const PICARD_MULTI_DISC_NAMING_SCRIPT_ID: &str = "5b2bd6e2-4179-53bf-94aa-cfec47de8ab0";

/// Picard single-disc naming source.
pub const PICARD_STANDARD_NAMING_SOURCE: &str = "{albumartist}/{album}{conditional(is_empty(year), \"\", concat(\" (\", year, \")\"))}{conditional(is_empty(album_disambiguation), \"\", concat(\" (\", album_disambiguation, \")\"))}/{track:02d} - {title}.{ext}";
/// Picard multi-disc naming source.
pub const PICARD_MULTI_DISC_NAMING_SOURCE: &str = "{albumartist}/{album}{conditional(is_empty(year), \"\", concat(\" (\", year, \")\"))}{conditional(is_empty(album_disambiguation), \"\", concat(\" (\", album_disambiguation, \")\"))}/{default(medium_format, \"Disc\")} {medium_number:02d}/{track:02d} - {title}.{ext}";

/// Managed tag-field names.
pub const MANAGED_FIELD_NAMES: &[&str] = &[
    "title",
    "artist",
    "artists",
    "album",
    "albumartist",
    "album_artist",
    "albumartists",
    "album_artists",
    "track_number",
    "track",
    "disc_number",
    "disc",
    "total_tracks",
    "total_discs",
    "year",
    "date",
    "genre",
    "comment",
    "composer",
    "lyricist",
    "conductor",
    "performer",
    "arranger",
    "remixer",
    "producer",
    "publisher",
    "copyright",
    "encoded_by",
    "isrc",
    "barcode",
    "catalog_number",
    "label",
    "media",
    "musicbrainz_recording_id",
    "musicbrainz_release_id",
    "musicbrainz_release_group_id",
    "musicbrainz_artist_id",
    "musicbrainz_album_artist_id",
    "musicbrainz_work_id",
    "acoustid_id",
    "acoustid_fingerprint",
];

/// Mergeable managed tag-field names.
pub const MERGEABLE_MANAGED_FIELD_NAMES: &[&str] = &[
    "artist",
    "artists",
    "albumartist",
    "album_artist",
    "albumartists",
    "album_artists",
    "genre",
    "comment",
    "composer",
    "lyricist",
    "conductor",
    "performer",
    "arranger",
    "remixer",
    "producer",
];

/// Legacy default sidecar patterns (pre-#401).
pub const LEGACY_DEFAULT_SIDECAR_PATTERNS: &[&str] = &[
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "cover.webp",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "front.jpg",
    "front.png",
    "*.cue",
    "*.log",
    "*.lrc",
    "*.m3u",
    "*.m3u8",
    "*.pls",
];

/// Bridge a DTO to its section type through JSON. Shapes are pinned
/// equal by parity tests; a failure here is a programmer error (500),
// never a caller fault.
pub fn dto_to_section<T: serde::Serialize, S: serde::de::DeserializeOwned>(
    dto: &T,
    ids: &dyn IdGenerator,
) -> Result<S, SettingsError> {
    let value = serde_json::to_value(dto)
        .map_err(|cause| SettingsError::internal(&cause.to_string(), ids))?;
    serde_json::from_value(value).map_err(|cause| SettingsError::internal(&cause.to_string(), ids))
}

/// Bridge a section type to its DTO through JSON.
pub fn section_to_dto<S: serde::Serialize, T: serde::de::DeserializeOwned>(
    section: &S,
    ids: &dyn IdGenerator,
) -> Result<T, SettingsError> {
    dto_to_section(section, ids)
}

/// Strip the legacy-compat shims before hashing (v2
/// `_remove_default_*`): default lyrics preservation, default
/// multi-disc naming (`inherit` + null script), and the default
/// identity section (automatic acceptance off).
fn strip_legacy_shims(profile: &mut serde_json::Value) {
    if let Some(enrichment) = profile.get_mut("enrichment")
        && let Some(lyrics) = enrichment.get_mut("lyrics")
        && lyrics.get("preserve_existing") == Some(&serde_json::Value::Bool(false))
        && let Some(map) = lyrics.as_object_mut()
    {
        map.remove("preserve_existing");
    }
    if let Some(overrides) = profile.get_mut("organization") {
        let is_default_multi =
            overrides.get("multi_disc_naming_script_id") == Some(&serde_json::Value::Null);
        if is_default_multi && let Some(map) = overrides.as_object_mut() {
            map.remove("multi_disc_naming_script_id");
        }
    }
    if let Some(identity) = profile.get_mut("identity")
        && identity.get("automatic_edition_acceptance_enabled")
            == Some(&serde_json::Value::Bool(false))
        && let Some(map) = identity.as_object_mut()
    {
        map.remove("automatic_edition_acceptance_enabled");
    }
}

/// Profile content revision: the full profile minus `revision` and the
/// legacy shims, hashed.
pub fn profile_revision(profile: &LibraryManagementProfileDto) -> String {
    let mut payload = serde_json::to_value(profile).unwrap_or(serde_json::Value::Null);
    if let Some(map) = payload.as_object_mut() {
        map.remove("revision");
    }
    strip_legacy_shims(&mut payload);
    stable_hash(&payload)
}

/// Settings content revision: the full settings with per-profile legacy
/// shims stripped, hashed. Includes profile/script revisions and
/// assignment activation pins.
pub fn settings_revision(settings: &LibraryManagementSettingsDto) -> String {
    let mut payload = serde_json::to_value(settings).unwrap_or(serde_json::Value::Null);
    if let Some(profiles) = payload.get_mut("profiles").and_then(|v| v.as_array_mut()) {
        for profile in profiles {
            strip_legacy_shims(profile);
        }
    }
    stable_hash(&payload)
}

/// Naming-script content revision (minus `revision`).
pub fn naming_script_revision(script: &NamingScriptDto) -> String {
    let mut payload = serde_json::to_value(script).unwrap_or(serde_json::Value::Null);
    if let Some(map) = payload.as_object_mut() {
        map.remove("revision");
    }
    stable_hash(&payload)
}

/// Tagging-script content revision (minus `revision`).
pub fn tagging_script_revision(script: &TaggingScriptDto) -> String {
    let mut payload = serde_json::to_value(script).unwrap_or(serde_json::Value::Null);
    if let Some(map) = payload.as_object_mut() {
        map.remove("revision");
    }
    stable_hash(&payload)
}

/// Naming-policy revision for a pinned profile: the standard script
/// revision alone, or the hashed standard+multi-disc pair.
pub fn naming_policy_revision(
    standard: &NamingScriptDto,
    multi_disc: Option<&NamingScriptDto>,
) -> String {
    match multi_disc {
        None => standard.revision.clone(),
        Some(multi) => {
            let payload = serde_json::json!({
                "policy_version": 1,
                "standard": {"id": standard.id, "revision": standard.revision},
                "multi_disc": {"id": multi.id, "revision": multi.revision},
            });
            stable_hash(&payload)
        }
    }
}

/// Picard-style organizer preset profile.
pub fn picard_style_organizer_profile() -> LibraryManagementProfileDto {
    let mut fields: Vec<ManagedFieldDto> = MANAGED_FIELD_NAMES
        .iter()
        .filter(|field| **field != "acoustid_id" && **field != "acoustid_fingerprint")
        .map(|field| ManagedFieldDto {
            field: (*field).to_owned(),
            mode: FieldModeDto::Replace,
            clear_when_canonical_missing: false,
        })
        .collect();
    fields.push(ManagedFieldDto {
        field: "acoustid_id".to_owned(),
        mode: FieldModeDto::FillMissing,
        clear_when_canonical_missing: false,
    });
    fields.push(ManagedFieldDto {
        field: "acoustid_fingerprint".to_owned(),
        mode: FieldModeDto::FillMissing,
        clear_when_canonical_missing: false,
    });
    let mut profile = LibraryManagementProfileDto {
        id: PICARD_ORGANIZER_PROFILE_ID.to_owned(),
        name: "Picard-style Organizer".to_owned(),
        description: "Canonical MusicBrainz tags and artwork with same-root organization, sidecars, and custom-tag preservation.".to_owned(),
        preset_origin: Some("picard_style_organizer".to_owned()),
        preset_version: Some(PICARD_ORGANIZER_PRESET_VERSION),
        metadata: MetadataManagementSettingsDto {
            fields,
            ..MetadataManagementSettingsDto::default()
        },
        organization: OrganizationManagementSettingsDto {
            naming_script_id: PICARD_NAMING_SCRIPT_ID.to_owned(),
            multi_disc_naming_script_id: Some(PICARD_MULTI_DISC_NAMING_SCRIPT_ID.to_owned()),
            ..OrganizationManagementSettingsDto::default()
        },
        ..LibraryManagementProfileDto::default()
    };
    profile.revision = profile_revision(&profile);
    profile
}

/// Complete library organizer preset profile (the Picard preset with
/// wider genres, artwork, lyrics, and ReplayGain).
pub fn complete_library_organizer_profile() -> LibraryManagementProfileDto {
    let mut profile = picard_style_organizer_profile();
    profile.id = COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID.to_owned();
    profile.name = "Complete Library Organizer".to_owned();
    profile.description =
        "Best-effort metadata, genres, artwork, lyrics, ReplayGain, and same-root organization."
            .to_owned();
    profile.preset_origin = Some("complete_library_organizer".to_owned());
    profile.preset_version = Some(1);
    profile.genres.sources = vec![
        GenreSourceDto::Musicbrainz,
        GenreSourceDto::Listenbrainz,
        GenreSourceDto::Lastfm,
    ];
    profile.artwork.image_types = vec![
        ArtworkImageTypeDto::Front,
        ArtworkImageTypeDto::Back,
        ArtworkImageTypeDto::Booklet,
        ArtworkImageTypeDto::Medium,
        ArtworkImageTypeDto::Tray,
        ArtworkImageTypeDto::Obi,
        ArtworkImageTypeDto::Spine,
        ArtworkImageTypeDto::Track,
        ArtworkImageTypeDto::Other,
    ];
    profile.artwork.local_file_patterns = ["*.jpg", "*.jpeg", "*.png", "*.webp", "*.gif", "*.pdf"]
        .iter()
        .map(ToString::to_string)
        .collect();
    profile.artwork.external_front_only = false;
    profile.enrichment.lyrics.enabled = true;
    profile.enrichment.replaygain.enabled = true;
    profile.enrichment.replaygain.mode = ReplayGainModeDto::Replace;
    profile.revision = profile_revision(&profile);
    profile
}

/// Preset profile for a tracked origin, if known.
pub fn preset_profile_for_origin(origin: &str) -> Option<LibraryManagementProfileDto> {
    match origin {
        "picard_style_organizer" => Some(picard_style_organizer_profile()),
        "complete_library_organizer" => Some(complete_library_organizer_profile()),
        _ => None,
    }
}

/// Unique preset name against the used set (`Name`, `Name 2`, ...).
pub fn unique_preset_name(base: &str, used: &BTreeSet<String>) -> String {
    if !used.contains(&base.to_lowercase()) {
        return base.to_owned();
    }
    let mut counter = 2;
    loop {
        let candidate = format!("{base} {counter}");
        if !used.contains(&candidate.to_lowercase()) {
            return candidate;
        }
        counter += 1;
    }
}

/// Current Picard preset scripts (names de-duplicated against stored scripts).
pub fn current_picard_preset_scripts(
    settings: &LibraryManagementSettingsDto,
) -> (NamingScriptDto, NamingScriptDto) {
    let preset_ids = [PICARD_NAMING_SCRIPT_ID, PICARD_MULTI_DISC_NAMING_SCRIPT_ID];
    let mut used: BTreeSet<String> = settings
        .naming_scripts
        .iter()
        .filter(|script| !preset_ids.contains(&script.id.as_str()))
        .map(|script| script.name.to_lowercase())
        .collect();
    let mut scripts = Vec::new();
    for (script_id, base_name, source) in [
        (
            PICARD_NAMING_SCRIPT_ID,
            "Picard-style: single disc",
            PICARD_STANDARD_NAMING_SOURCE,
        ),
        (
            PICARD_MULTI_DISC_NAMING_SCRIPT_ID,
            "Picard-style: multiple discs",
            PICARD_MULTI_DISC_NAMING_SOURCE,
        ),
    ] {
        let name = unique_preset_name(base_name, &used);
        used.insert(name.to_lowercase());
        let mut script = NamingScriptDto {
            id: script_id.to_owned(),
            name,
            source: source.to_owned(),
            revision: String::new(),
            preset_origin: Some("picard_style_organizer".to_owned()),
            preset_version: Some(PICARD_ORGANIZER_PRESET_VERSION),
        };
        script.revision = naming_script_revision(&script);
        scripts.push(script);
    }
    (scripts.remove(0), scripts.remove(0))
}

/// Fresh-tenant initial settings: the Picard preset profile (default),
/// the two preset scripts, and the legacy resistor barriers.
pub fn initial_settings() -> LibraryManagementSettingsDto {
    let mut settings = LibraryManagementSettingsDto {
        schema_version: MANAGEMENT_SCHEMA_VERSION,
        preset_catalog_version: PRESET_CATALOG_VERSION,
        ..LibraryManagementSettingsDto::default()
    };
    let (standard, multi_disc) = current_picard_preset_scripts(&settings);
    settings.naming_scripts = vec![standard, multi_disc];
    let preset = picard_style_organizer_profile();
    settings.default_profile_id = preset.id.clone();
    settings.profiles = vec![preset];
    settings
}

/// Carry the activation pins and migration history forward on save:
///
/// - dropped `revision` inputs recompute (never trusted from the client);
/// - legacy default sidecar patterns migrate silently to current defaults;
/// - default assignments on removed roots retarget to the default profile;
/// - assignments losing their profile fall back to the suggested profile
///   (the saved default) before failing;
/// - activation pins carry: matching pins stay, non-matching pins (or
///   pins whose naming policy drifted) clear and surface as invalidations.
pub fn migration_carry(
    current: &LibraryManagementSettingsDto,
    incoming: &mut LibraryManagementSettingsDto,
    suggested_profile_id: &str,
) -> Result<Vec<String>, SettingsError> {
    use std::collections::HashMap;
    let previous_assignments: HashMap<&str, &LibraryManagementRootAssignmentDto> = current
        .root_assignments
        .iter()
        .map(|assignment| (assignment.root_id.as_str(), assignment))
        .collect();
    let legacy_patterns: Vec<String> = LEGACY_DEFAULT_SIDECAR_PATTERNS
        .iter()
        .map(ToString::to_string)
        .collect();

    for profile in &mut incoming.profiles {
        if profile.organization.sidecar_patterns == legacy_patterns {
            profile.organization.sidecar_patterns = profile.organization.sidecar_patterns.clone();
            // Replaced with the DTO default below; the legacy list is
            // silent-migrated to current defaults.
            profile.organization.sidecar_patterns =
                OrganizationManagementSettingsDto::default().sidecar_patterns;
        }
        profile.revision = profile_revision(profile);
    }
    for script in &mut incoming.naming_scripts {
        script.revision = naming_script_revision(script);
    }
    for script in &mut incoming.tagging_scripts {
        script.revision = tagging_script_revision(script);
    }

    let mut invalidated = Vec::new();
    for assignment in &mut incoming.root_assignments {
        match previous_assignments.get(assignment.root_id.as_str()) {
            None => {
                clear_activation(assignment);
            }
            Some(previous) => {
                if previous.profile_id != assignment.profile_id
                    || previous.overrides.as_ref().map(canonical_overrides)
                        != assignment.overrides.as_ref().map(canonical_overrides)
                    || previous.automatic_acquisitions != assignment.automatic_acquisitions
                    || previous.automatic_drop_imports != assignment.automatic_drop_imports
                    || previous.automatic_scan_discovered != assignment.automatic_scan_discovered
                    || previous.automatic_custom_editions != assignment.automatic_custom_editions
                    || previous.enabled != assignment.enabled
                {
                    clear_activation(assignment);
                    if assignment.activation_profile_revision.is_some()
                        || previous.activation_profile_revision.is_some()
                    {
                        invalidated.push(assignment.root_id.clone());
                    }
                } else {
                    assignment.activation_profile_revision =
                        previous.activation_profile_revision.clone();
                    assignment.activation_naming_policy_revision =
                        previous.activation_naming_policy_revision.clone();
                    assignment.activation_policy_revision =
                        previous.activation_policy_revision.clone();
                    assignment.activation_settings_revision =
                        previous.activation_settings_revision.clone();
                    assignment.activation_preview_token = previous.activation_preview_token.clone();
                    assignment.activation_preview_hash = previous.activation_preview_hash.clone();
                    assignment.activation_confirmed_at = previous.activation_confirmed_at;
                    // A pin whose profile or naming policy drifted under
                    // the new settings clears and invalidates.
                    let pinned_profile = assignment.profile_id.as_deref().unwrap_or_default();
                    let stored_profile = incoming
                        .profiles
                        .iter()
                        .find(|profile| profile.id == pinned_profile);
                    let drifted = match (stored_profile, &assignment.activation_profile_revision) {
                        (Some(stored), Some(pinned)) => stored.revision != *pinned,
                        (None, _) => true,
                        (_, None) => false,
                    };
                    if drifted {
                        clear_activation(assignment);
                        invalidated.push(assignment.root_id.clone());
                    }
                }
            }
        }
        if assignment.profile_id.is_none() && !suggested_profile_id.is_empty() {
            assignment.profile_id = Some(suggested_profile_id.to_owned());
        }
    }
    invalidated.sort();
    invalidated.dedup();
    Ok(invalidated)
}

/// Canonical overrides JSON for pin-carry comparison.
fn canonical_overrides(overrides: &super::models::LibraryManagementRootOverridesDto) -> String {
    super::library_policy::canonical_json(
        &serde_json::to_value(overrides).unwrap_or(serde_json::Value::Null),
    )
}

/// Clear an assignment's activation pins.
fn clear_activation(assignment: &mut LibraryManagementRootAssignmentDto) {
    assignment.activation_profile_revision = None;
    assignment.activation_naming_policy_revision = None;
    assignment.activation_policy_revision = None;
    assignment.activation_settings_revision = None;
    assignment.activation_preview_token = None;
    assignment.activation_preview_hash = None;
    assignment.activation_confirmed_at = None;
}

/// Normalize, validate, and populate deterministic derived revisions.
/// Script sources compile through the injected compiler. Errors are
/// caller faults (400) with the v2 messages.
pub fn normalize(
    settings: &mut LibraryManagementSettingsDto,
    compiler: &dyn ScriptCompiler,
) -> Result<(), SettingsError> {
    if settings.schema_version != MANAGEMENT_SCHEMA_VERSION {
        return Err(invalid("Unsupported Library Management settings version."));
    }
    if settings.preset_catalog_version < 0
        || settings.preset_catalog_version > PRESET_CATALOG_VERSION
    {
        return Err(invalid(
            "Unsupported Library Management preset catalog version.",
        ));
    }
    if settings.undo_retention_days < 1 || settings.undo_retention_days > 3650 {
        return Err(invalid("Undo retention must be between 1 and 3650 days."));
    }
    if settings.preview_retention_hours < 1 || settings.preview_retention_hours > 168 {
        return Err(invalid(
            "Preview retention must be between 1 and 168 hours.",
        ));
    }
    settings.recycle_bin_path = settings.recycle_bin_path.trim().to_owned();
    if settings.recycle_bin_path.contains('\x00')
        || settings.recycle_bin_path.len() > 4096
        || (!settings.recycle_bin_path.is_empty()
            && !std::path::Path::new(&settings.recycle_bin_path).is_absolute())
    {
        return Err(invalid("The recycle bin must be an absolute path."));
    }
    if settings.external_refresh.retry_attempts < 0 || settings.external_refresh.retry_attempts > 20
    {
        return Err(invalid(
            "External refresh retries must be between 0 and 20.",
        ));
    }
    if settings.external_refresh.retry_delay_seconds < 1
        || settings.external_refresh.retry_delay_seconds > 3600
    {
        return Err(invalid(
            "External refresh retry delay must be between 1 and 3600 seconds.",
        ));
    }

    let mut script_ids: BTreeSet<String> = BTreeSet::new();
    let mut script_names: BTreeSet<String> = BTreeSet::new();
    for script in &mut settings.naming_scripts {
        validate_uuid(&script.id, "Naming script ID")?;
        if !script_ids.insert(script.id.clone()) {
            return Err(invalid("Every naming script needs a unique ID."));
        }
        script.name = validate_name(&script.name, "Naming script")?;
        if !script_names.insert(script.name.to_lowercase()) {
            return Err(invalid("Every naming script needs a unique name."));
        }
        script.source = validate_script_source(&script.source, &script.name)?;
        validate_naming_language(&script.source, &script.name, compiler)?;
        script.revision = naming_script_revision(script);
    }
    let mut tagging_ids: BTreeSet<String> = BTreeSet::new();
    let mut tagging_names: BTreeSet<String> = BTreeSet::new();
    for script in &mut settings.tagging_scripts {
        validate_uuid(&script.id, "Tagging script ID")?;
        if !tagging_ids.insert(script.id.clone()) {
            return Err(invalid("Every tagging script needs a unique ID."));
        }
        script.name = validate_name(&script.name, "Tagging script")?;
        if !tagging_names.insert(script.name.to_lowercase()) {
            return Err(invalid("Every tagging script needs a unique name."));
        }
        script.source = validate_script_source(&script.source, &script.name)?;
        validate_tagging_language(&script.source, &script.name, compiler)?;
        script.revision = tagging_script_revision(script);
    }

    let mut profile_ids: BTreeSet<String> = BTreeSet::new();
    let mut profile_names: BTreeSet<String> = BTreeSet::new();
    for profile in &mut settings.profiles {
        validate_uuid(&profile.id, "Profile ID")?;
        if !profile_ids.insert(profile.id.clone()) {
            return Err(invalid(
                "Every Library Management profile needs a unique ID.",
            ));
        }
        profile.name = validate_name(&profile.name, "Library Management profile")?;
        if !profile_names.insert(profile.name.to_lowercase()) {
            return Err(invalid(
                "Every Library Management profile needs a unique name.",
            ));
        }
        validate_profile_blocks(profile, &script_ids, &tagging_ids)?;
        profile.revision = profile_revision(profile);
    }

    if settings.default_profile_id.is_empty() || !profile_ids.contains(&settings.default_profile_id)
    {
        return Err(invalid(
            "The default Library Management profile does not exist.",
        ));
    }

    let mut assignment_roots: BTreeSet<String> = BTreeSet::new();
    for assignment in &mut settings.root_assignments {
        if assignment.root_id.trim().is_empty()
            || !assignment_roots.insert(assignment.root_id.clone())
        {
            return Err(invalid(
                "Every root can have only one management assignment.",
            ));
        }
        if !assignment.automatic_scan_discovered {
            assignment.automatic_custom_editions = false;
        }
        if let Some(profile_id) = &assignment.profile_id
            && !profile_ids.contains(profile_id)
        {
            return Err(invalid("A root assignment references an unknown profile."));
        }
        if let Some(overrides) = &assignment.overrides {
            if let Some(script_id) = &overrides.naming_script_id
                && !script_ids.contains(script_id)
            {
                return Err(invalid(
                    "A root override references an unknown naming script.",
                ));
            }
            if overrides.multi_disc_naming_mode == MultiDiscNamingModeDto::Script {
                match &overrides.multi_disc_naming_script_id {
                    Some(script_id) if script_ids.contains(script_id) => {}
                    _ => {
                        return Err(invalid(
                            "A root multi-disc script override references an unknown naming script.",
                        ));
                    }
                }
            } else if overrides.multi_disc_naming_script_id.is_some() {
                return Err(invalid(
                    "A root multi-disc script ID is allowed only in selected-script mode.",
                ));
            }
        }
    }

    settings.naming_scripts.sort_by(|a, b| a.id.cmp(&b.id));
    settings.tagging_scripts.sort_by(|a, b| a.id.cmp(&b.id));
    settings.profiles.sort_by(|a, b| a.id.cmp(&b.id));
    settings
        .root_assignments
        .sort_by(|a, b| a.root_id.cmp(&b.root_id));
    Ok(())
}

fn invalid(message: &str) -> SettingsError {
    SettingsError::InvalidInput {
        message: message.to_owned(),
    }
}

fn validate_uuid(value: &str, label: &str) -> Result<(), SettingsError> {
    if uuid::Uuid::parse_str(value).is_err() {
        return Err(invalid(&format!("{label} must be a valid UUID.")));
    }
    Ok(())
}

/// Collapse whitespace; names are 1-120 chars.
fn validate_name(value: &str, label: &str) -> Result<String, SettingsError> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(invalid(&format!("{label} needs a name.")));
    }
    if normalized.len() > 120 {
        return Err(invalid(&format!("{label} name is too long.")));
    }
    Ok(normalized)
}

/// Strip trailing whitespace; sources are non-empty, NUL-free, capped.
fn validate_script_source(source: &str, label: &str) -> Result<String, SettingsError> {
    let normalized = source.trim_end().to_owned();
    if normalized.is_empty() {
        return Err(invalid(&format!("{label} needs source text.")));
    }
    if normalized.contains('\x00') || normalized.len() > 32_768 {
        return Err(invalid(&format!("{label} source is invalid or too long.")));
    }
    Ok(normalized)
}

fn normalize_unique_strings(
    values: Vec<String>,
    label: &str,
) -> Result<Vec<String>, SettingsError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in values {
        let value = raw.trim().to_owned();
        if value.is_empty() {
            return Err(invalid(&format!("{label} cannot contain an empty value.")));
        }
        if !seen.insert(value.to_lowercase()) {
            return Err(invalid(&format!("{label} cannot contain duplicates.")));
        }
        out.push(value);
    }
    Ok(out)
}

fn validate_sidecar_pattern(pattern: &str) -> Result<String, SettingsError> {
    let value = pattern.trim().to_owned();
    if value.is_empty() || value.contains('\\') || value.starts_with('/') {
        return Err(invalid(&format!(
            "Sidecar pattern must stay inside the album directory: {pattern}"
        )));
    }
    if value
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid(&format!(
            "Sidecar pattern must stay inside the album directory: {pattern}"
        )));
    }
    Ok(value)
}

fn validate_artwork_pattern(pattern: &str) -> Result<String, SettingsError> {
    let value = pattern.trim().to_owned();
    if value.is_empty()
        || matches!(value.as_str(), "*" | "**" | "**/*")
        || value.contains('\\')
        || value.starts_with('/')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid(&format!(
            "Artwork pattern must stay inside the album directory: {pattern}"
        )));
    }
    Ok(value)
}

fn validate_profile_blocks(
    profile: &mut LibraryManagementProfileDto,
    script_ids: &BTreeSet<String>,
    tagging_ids: &BTreeSet<String>,
) -> Result<(), SettingsError> {
    let mut field_names = BTreeSet::new();
    for field in &mut profile.metadata.fields {
        if field.mode == FieldModeDto::Preserve {
            field.mode = FieldModeDto::Disabled;
        }
        if field.mode != FieldModeDto::Replace {
            field.clear_when_canonical_missing = false;
        }
        if !MANAGED_FIELD_NAMES.contains(&field.field.as_str()) {
            return Err(invalid(&format!("Unknown managed field: {}", field.field)));
        }
        if !field_names.insert(field.field.clone()) {
            return Err(invalid(&format!(
                "Managed field is configured twice: {}",
                field.field
            )));
        }
        if field.mode == FieldModeDto::Merge
            && !MERGEABLE_MANAGED_FIELD_NAMES.contains(&field.field.as_str())
        {
            return Err(invalid(&format!(
                "Managed field does not support merge: {}",
                field.field
            )));
        }
    }
    profile.metadata.preserve_fields = normalize_unique_strings(
        std::mem::take(&mut profile.metadata.preserve_fields),
        "Preserved fields",
    )?;
    if profile.metadata.preserve_fields.len() > 100 {
        return Err(invalid("A profile has too many preserved fields."));
    }
    if profile
        .metadata
        .preserve_fields
        .iter()
        .any(|value| value.contains('\x00') || value.len() > 255)
    {
        return Err(invalid("A preserved field name is invalid."));
    }
    profile.metadata.artist_credits.preferred_locales = normalize_unique_strings(
        std::mem::take(&mut profile.metadata.artist_credits.preferred_locales),
        "Preferred locales",
    )?;
    if profile.metadata.artist_credits.standardization == ArtistStandardizationDto::Variations {
        profile.metadata.artist_credits.standardization = ArtistStandardizationDto::Credited;
    }
    if profile
        .metadata
        .format_compatibility
        .constrained_genres_primary_only
    {
        profile.genres.write_primary_only_for_constrained_formats = true;
        profile
            .metadata
            .format_compatibility
            .constrained_genres_primary_only = false;
    }
    profile.notification.refresh_droppedneedle = true;
    if profile
        .metadata
        .format_compatibility
        .id3v23_join_delimiter
        .len()
        > 8
    {
        return Err(invalid("The ID3v2.3 join delimiter is too long."));
    }
    if profile.metadata.format_compatibility.id3_version == Id3VersionDto::V23
        && profile.metadata.format_compatibility.id3_text_encoding == Id3TextEncodingDto::Utf8
    {
        return Err(invalid("ID3v2.3 requires UTF-16 text encoding."));
    }
    if profile
        .metadata
        .tagging_script_ids
        .iter()
        .any(|value| !tagging_ids.contains(value))
    {
        return Err(invalid("A profile references an unknown tagging script."));
    }
    if profile
        .metadata
        .tagging_script_ids
        .iter()
        .collect::<BTreeSet<_>>()
        .len()
        != profile.metadata.tagging_script_ids.len()
    {
        return Err(invalid("A profile cannot attach a tagging script twice."));
    }

    let genres = &mut profile.genres;
    if genres.maximum_count < 1 || genres.maximum_count > 100 {
        return Err(invalid("Maximum genre count must be between 1 and 100."));
    }
    if genres.musicbrainz_minimum_count < 0 || genres.musicbrainz_minimum_count > 1_000_000 {
        return Err(invalid("MusicBrainz genre threshold is invalid."));
    }
    if genres.listenbrainz_minimum_count < 0 || genres.listenbrainz_minimum_count > 1_000_000 {
        return Err(invalid("ListenBrainz genre threshold is invalid."));
    }
    if genres.lastfm_minimum_weight < 0 || genres.lastfm_minimum_weight > 100 {
        return Err(invalid("Last.fm genre weight must be between 0 and 100."));
    }
    if genres.maximum_ancestry_depth < 0 || genres.maximum_ancestry_depth > 32 {
        return Err(invalid("Genre ancestry depth must be between 0 and 32."));
    }
    if genres.sources.iter().collect::<BTreeSet<_>>().len() != genres.sources.len() {
        return Err(invalid("Genre sources cannot contain duplicates."));
    }
    genres.allowlist =
        normalize_unique_strings(std::mem::take(&mut genres.allowlist), "Genre allowlist")?;
    genres.denylist =
        normalize_unique_strings(std::mem::take(&mut genres.denylist), "Genre denylist")?;
    genres.preferred_casing = normalize_unique_strings(
        std::mem::take(&mut genres.preferred_casing),
        "Preferred genre casing",
    )?;
    if genres.allowlist.len() > 500
        || genres.denylist.len() > 500
        || genres.aliases.len() > 500
        || genres.preferred_casing.len() > 500
    {
        return Err(invalid("A genre rule list is too large."));
    }
    let mut alias_sources = BTreeSet::new();
    for alias in &mut genres.aliases {
        alias.source = alias.source.trim().to_owned();
        alias.target = alias.target.trim().to_owned();
        if alias.source.is_empty() || alias.target.is_empty() {
            return Err(invalid("Genre aliases need a source and target."));
        }
        if !alias_sources.insert(alias.source.to_lowercase()) {
            return Err(invalid("Genre alias sources must be unique."));
        }
    }

    let artwork = &mut profile.artwork;
    artwork
        .providers
        .retain(|provider| *provider != ArtworkProviderDto::Audiodb);
    if artwork.providers.iter().collect::<BTreeSet<_>>().len() != artwork.providers.len() {
        return Err(invalid("Artwork providers cannot contain duplicates."));
    }
    if artwork.image_types.iter().collect::<BTreeSet<_>>().len() != artwork.image_types.len() {
        return Err(invalid("Artwork image types cannot contain duplicates."));
    }
    if artwork.local_file_patterns.len() > 100 {
        return Err(invalid("A profile has too many local artwork patterns."));
    }
    artwork.local_file_patterns = std::mem::take(&mut artwork.local_file_patterns)
        .into_iter()
        .map(|pattern| validate_artwork_pattern(&pattern))
        .collect::<Result<Vec<_>, _>>()?;
    if artwork
        .local_file_patterns
        .iter()
        .map(|value| value.to_lowercase())
        .collect::<BTreeSet<_>>()
        .len()
        != artwork.local_file_patterns.len()
    {
        return Err(invalid("Local artwork patterns cannot contain duplicates."));
    }
    if [
        artwork.minimum_width,
        artwork.minimum_height,
        artwork.embedded_maximum_size,
        artwork.external_maximum_size,
    ]
    .iter()
    .any(|value| *value < 0 || *value > 20_000)
    {
        return Err(invalid(
            "Artwork dimensions must be between 0 and 20000 pixels.",
        ));
    }
    if let Some(script_id) = &artwork.external_naming_script_id
        && !script_ids.contains(script_id)
    {
        return Err(invalid("Artwork references an unknown naming script."));
    }
    let lyrics = &profile.enrichment.lyrics;
    if lyrics.enabled && !(lyrics.write_plain || lyrics.write_synced) {
        return Err(invalid("Enabled lyrics need at least one output format."));
    }

    let organization = &mut profile.organization;
    if !script_ids.contains(&organization.naming_script_id) {
        return Err(invalid("A profile references an unknown naming script."));
    }
    if let Some(script_id) = &organization.multi_disc_naming_script_id
        && !script_ids.contains(script_id)
    {
        return Err(invalid(
            "A profile references an unknown multi-disc naming script.",
        ));
    }
    if organization.sidecar_patterns.len() > 100 {
        return Err(invalid("A profile has too many sidecar patterns."));
    }
    organization.sidecar_patterns = std::mem::take(&mut organization.sidecar_patterns)
        .into_iter()
        .map(|pattern| validate_sidecar_pattern(&pattern))
        .collect::<Result<Vec<_>, _>>()?;
    if organization
        .sidecar_patterns
        .iter()
        .collect::<BTreeSet<_>>()
        .len()
        != organization.sidecar_patterns.len()
    {
        return Err(invalid("Sidecar patterns cannot contain duplicates."));
    }
    let compatibility = &organization.compatibility;
    if compatibility.separator_replacement.chars().count() != 1
        || matches!(
            compatibility.separator_replacement.as_str(),
            "/" | "\\" | "\x00"
        )
    {
        return Err(invalid(
            "Path separator replacement must be one safe character.",
        ));
    }
    if compatibility.maximum_component_length < 1 || compatibility.maximum_component_length > 255 {
        return Err(invalid(
            "Maximum path component length must be between 1 and 255.",
        ));
    }
    if compatibility.maximum_path_length < 64 || compatibility.maximum_path_length > 32_767 {
        return Err(invalid("Maximum path length must be between 64 and 32767."));
    }
    if !profile.file_behavior.reject_symlinks {
        return Err(invalid("Library Management cannot follow symlinks."));
    }
    Ok(())
}

// --- script language port ------------------------------------------------------

/// Naming variable universe (managed fields plus path-only variables).
pub fn naming_variables() -> BTreeSet<&'static str> {
    let mut variables: BTreeSet<&'static str> = MANAGED_FIELD_NAMES.iter().copied().collect();
    variables.extend(
        [
            "genre",
            "genres",
            "primary_genre",
            "artist_display",
            "artists",
            "artist_sorts",
            "album_artist_display",
            "album_artists",
            "album_artist_sorts",
            "albumartist",
            "initial",
            "year",
            "track",
            "disc",
            "ext",
            "extension",
            "medium",
            "album_disambiguation",
            "medium_format",
            "medium_number",
            "musicbrainz_id",
            "artist_mbid",
            "codec",
            "quality",
            "bitrate",
            "sample_rate",
            "bit_depth",
            "artwork_type",
            "artwork_comment",
            "artwork_extension",
            "artwork_format",
        ]
        .iter()
        .copied(),
    );
    variables
}

/// One compiled naming segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamingSegment {
    /// Literal text, when a literal segment.
    pub literal: Option<String>,
    /// Legacy variable name, when a `{variable}` segment.
    pub legacy_variable: Option<String>,
    /// Numeric format spec, when present.
    pub format_spec: Option<String>,
    /// Variables referenced by an expression segment.
    pub expression_variables: Vec<ExprVariable>,
    /// Source line (1-based).
    pub line: i64,
    /// Source column (1-based).
    pub column: i64,
}

/// One variable reference inside an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprVariable {
    /// Variable name.
    pub name: String,
    /// Source line (1-based).
    pub line: i64,
    /// Source column (1-based).
    pub column: i64,
}

/// One compiled tagging statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggingStatement {
    /// Operation (`set`, `append`, `delete`, `if`, ...).
    pub operation: String,
    /// Target field, when the statement has one.
    pub target: Option<String>,
    /// Variables referenced by the statement expression.
    pub expression_variables: Vec<ExprVariable>,
    /// Source line (1-based).
    pub line: i64,
    /// Source column (1-based).
    pub column: i64,
}

/// Script compile failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    /// Human explanation.
    pub message: String,
    /// Source line (1-based).
    pub line: i64,
    /// Source column (1-based).
    pub column: i64,
}

/// Script-language compiler port. The shipped structural compiler
/// segments naming templates and tagging programs exactly and scans
/// expressions for identifiers; the full expression compiler (function
/// names, arity, evaluation) is library-engine follow-up work behind
/// this same port.
pub trait ScriptCompiler: Send + Sync {
    /// Compile a naming template into segments.
    fn compile_naming(
        &self,
        source: &str,
        script_name: &str,
    ) -> Result<Vec<NamingSegment>, ScriptError>;
    /// Compile a tagging program into statements.
    fn compile_tagging(
        &self,
        source: &str,
        script_name: &str,
    ) -> Result<Vec<TaggingStatement>, ScriptError>;
}

fn script_error(message: String, script_name: &str, line: i64, column: i64) -> SettingsError {
    SettingsError::InvalidInput {
        message: format!("{script_name} line {line}, column {column}: {message}"),
    }
}

fn validate_naming_language(
    source: &str,
    script_name: &str,
    compiler: &dyn ScriptCompiler,
) -> Result<(), SettingsError> {
    if source.contains('\n') || source.contains('\r') {
        return Err(script_error(
            "Naming scripts must be a single path template.".to_owned(),
            script_name,
            1,
            1,
        ));
    }
    let segments = compiler
        .compile_naming(source, script_name)
        .map_err(|cause| script_error(cause.message, script_name, cause.line, cause.column))?;
    let path_shape: String = segments
        .iter()
        .map(|segment| {
            segment
                .literal
                .clone()
                .unwrap_or_else(|| "value".to_owned())
        })
        .collect();
    if path_shape.starts_with('/')
        || path_shape
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(script_error(
            "Naming script must stay in a non-empty relative path.".to_owned(),
            script_name,
            1,
            1,
        ));
    }
    let variables = naming_variables();
    for segment in &segments {
        if let Some(legacy) = &segment.legacy_variable {
            if !variables.contains(legacy.as_str()) {
                return Err(script_error(
                    format!("Unknown naming variable: {legacy}."),
                    script_name,
                    segment.line,
                    segment.column,
                ));
            }
            if segment.format_spec.is_some()
                && !matches!(
                    legacy.as_str(),
                    "track"
                        | "disc"
                        | "track_number"
                        | "disc_number"
                        | "total_tracks"
                        | "total_discs"
                        | "medium_number"
                )
            {
                return Err(script_error(
                    format!("Variable {legacy} does not support a numeric format."),
                    script_name,
                    segment.line,
                    segment.column,
                ));
            }
        }
        for variable in &segment.expression_variables {
            if !variables.contains(variable.name.as_str()) {
                return Err(script_error(
                    format!("Unknown naming variable: {}.", variable.name),
                    script_name,
                    variable.line,
                    variable.column,
                ));
            }
        }
    }
    Ok(())
}

fn validate_tagging_language(
    source: &str,
    script_name: &str,
    compiler: &dyn ScriptCompiler,
) -> Result<(), SettingsError> {
    let allowed: BTreeSet<&str> = MANAGED_FIELD_NAMES
        .iter()
        .copied()
        .chain(["genre"])
        .collect();
    let ordered: BTreeSet<&str> = MERGEABLE_MANAGED_FIELD_NAMES
        .iter()
        .copied()
        .chain(["genre"])
        .collect();
    let read_only: BTreeSet<&str> = [
        "artist_display",
        "album_artist_display",
        "artists",
        "album_artists",
        "genres",
        "year",
        "primary_genre",
    ]
    .iter()
    .copied()
    .collect();
    let statements = compiler
        .compile_tagging(source, script_name)
        .map_err(|cause| script_error(cause.message, script_name, cause.line, cause.column))?;
    for statement in &statements {
        if let Some(target) = &statement.target {
            let custom = if target.len() > 7
                && target.is_char_boundary(7)
                && target[..7].eq_ignore_ascii_case("custom.")
            {
                Some(target[7..].trim())
            } else {
                None
            };
            let custom_valid = custom.is_some_and(|name| {
                !name.is_empty() && !name.contains('\x00') && name.len() <= 255
            });
            if !allowed.contains(target.as_str()) && !custom_valid {
                return Err(script_error(
                    format!("Unknown or invalid tagging target: {target}."),
                    script_name,
                    statement.line,
                    statement.column,
                ));
            }
            if statement.operation == "append"
                && !ordered.contains(target.as_str())
                && custom.is_none()
            {
                return Err(script_error(
                    format!("Field {target} does not accept append."),
                    script_name,
                    statement.line,
                    statement.column,
                ));
            }
        }
        for variable in &statement.expression_variables {
            if !(allowed.contains(variable.name.as_str())
                || read_only.contains(variable.name.as_str())
                || variable.name.len() > 7
                    && variable.name.is_char_boundary(7)
                    && variable.name[..7].eq_ignore_ascii_case("custom."))
            {
                return Err(script_error(
                    format!("Unknown tagging variable: {}.", variable.name),
                    script_name,
                    variable.line,
                    variable.column,
                ));
            }
        }
    }
    Ok(())
}

/// Structural script compiler: exact segmentation, identifier scanning
/// for expressions. Catches unknown variables/targets, bad numeric
/// formats, multi-line naming, bad path shapes, and append-to-scalar;
/// malformed expressions (bad function names, arity, syntax) need the
/// full compiler behind the same port.
#[derive(Debug, Default)]
pub struct StructuralCompiler;

impl StructuralCompiler {
    /// Build the structural compiler.
    pub fn new() -> Self {
        Self
    }
}

/// Scan identifiers out of an expression body: string literals skipped,
/// dotted names kept whole, function-call callees skipped.
fn scan_expression_variables(body: &str, line: i64) -> Vec<ExprVariable> {
    let mut out = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '"' || ch == '\'' {
            let quote = ch;
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' {
                    index += 2;
                    continue;
                }
                if chars[index] == quote {
                    index += 1;
                    break;
                }
                index += 1;
            }
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = index;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric()
                    || chars[index] == '_'
                    || chars[index] == '.')
            {
                index += 1;
            }
            let mut name: String = chars[start..index].iter().collect();
            while name.ends_with('.') {
                name.pop();
            }
            // A name followed by `(` is a function call, not a variable.
            let mut lookahead = index;
            while lookahead < chars.len() && chars[lookahead].is_whitespace() {
                lookahead += 1;
            }
            if lookahead < chars.len() && chars[lookahead] == '(' {
                continue;
            }
            if !name.is_empty() {
                out.push(ExprVariable {
                    name,
                    line,
                    column: start as i64 + 1,
                });
            }
            continue;
        }
        index += 1;
    }
    out
}

impl ScriptCompiler for StructuralCompiler {
    fn compile_naming(
        &self,
        source: &str,
        script_name: &str,
    ) -> Result<Vec<NamingSegment>, ScriptError> {
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut index = 0;
        let chars: Vec<char> = source.chars().collect();
        let fail = |message: &str, column: usize| ScriptError {
            message: format!("{script_name}: {message}"),
            line: 1,
            column: column as i64,
        };
        // v2's legacy placeholder: {name} or {name:spec}.
        let legacy = |body: &str| -> Option<(String, Option<String>)> {
            let (name, spec) = match body.split_once(':') {
                Some((name, spec)) => (name, Some(spec.to_owned())),
                None => (body, None),
            };
            if name.is_empty()
                || !name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')
            {
                return None;
            }
            if let Some(spec) = &spec
                && (spec.is_empty()
                    || !spec
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
            {
                return None;
            }
            Some((name.to_owned(), spec))
        };
        while index < chars.len() {
            if chars[index] == '{' {
                // Find the matching close brace (nesting-aware).
                let mut depth = 0;
                let mut end = None;
                for (offset, ch) in chars[index..].iter().enumerate() {
                    if *ch == '{' {
                        depth += 1;
                    } else if *ch == '}' {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(index + offset);
                            break;
                        }
                    }
                }
                let Some(end) = end else {
                    return Err(fail("Unmatched '{' in naming template.", index + 1));
                };
                if !literal.is_empty() {
                    segments.push(NamingSegment {
                        literal: Some(std::mem::take(&mut literal)),
                        legacy_variable: None,
                        format_spec: None,
                        expression_variables: Vec::new(),
                        line: 1,
                        column: 1,
                    });
                }
                let body: String = chars[index + 1..end].iter().collect();
                match legacy(&body) {
                    Some((name, spec)) => segments.push(NamingSegment {
                        literal: None,
                        legacy_variable: Some(name),
                        format_spec: spec,
                        expression_variables: Vec::new(),
                        line: 1,
                        column: index as i64 + 1,
                    }),
                    None => segments.push(NamingSegment {
                        literal: None,
                        legacy_variable: None,
                        format_spec: None,
                        expression_variables: scan_expression_variables(&body, 1),
                        line: 1,
                        column: index as i64 + 1,
                    }),
                }
                index = end + 1;
            } else if chars[index] == '}' {
                return Err(fail("Unmatched '}' in naming template.", index + 1));
            } else {
                literal.push(chars[index]);
                index += 1;
            }
        }
        if !literal.is_empty() {
            segments.push(NamingSegment {
                literal: Some(literal),
                legacy_variable: None,
                format_spec: None,
                expression_variables: Vec::new(),
                line: 1,
                column: 1,
            });
        }
        Ok(segments)
    }

    fn compile_tagging(
        &self,
        source: &str,
        script_name: &str,
    ) -> Result<Vec<TaggingStatement>, ScriptError> {
        let mut statements = Vec::new();
        let mut depth = 0;
        for (number, raw_line) in source.lines().enumerate() {
            let line_number = number as i64 + 1;
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fail = |message: &str| ScriptError {
                message: format!("{script_name}: {message}"),
                line: line_number,
                column: 1,
            };
            let mut words = line.splitn(2, char::is_whitespace);
            let keyword = words.next().unwrap_or("").to_lowercase();
            let rest = words.next().unwrap_or("").trim();
            match keyword.as_str() {
                "if" => {
                    depth += 1;
                    statements.push(TaggingStatement {
                        operation: "if".to_owned(),
                        target: None,
                        expression_variables: scan_expression_variables(rest, line_number),
                        line: line_number,
                        column: 1,
                    });
                }
                "else" => {
                    if depth == 0 {
                        return Err(fail("else without if."));
                    }
                    statements.push(TaggingStatement {
                        operation: "else".to_owned(),
                        target: None,
                        expression_variables: Vec::new(),
                        line: line_number,
                        column: 1,
                    });
                }
                "end" => {
                    if depth == 0 {
                        return Err(fail("end without if."));
                    }
                    depth -= 1;
                    statements.push(TaggingStatement {
                        operation: "end".to_owned(),
                        target: None,
                        expression_variables: Vec::new(),
                        line: line_number,
                        column: 1,
                    });
                }
                "set" | "append" | "delete" => {
                    let (target, expression) = match rest.split_once('=') {
                        Some((target, expression)) => (target.trim().to_owned(), expression.trim()),
                        None => {
                            if keyword == "delete" && !rest.is_empty() {
                                (rest.to_owned(), "")
                            } else {
                                return Err(fail(&format!(
                                    "{keyword} needs a target and a value."
                                )));
                            }
                        }
                    };
                    if target.is_empty() {
                        return Err(fail(&format!("{keyword} needs a target.")));
                    }
                    statements.push(TaggingStatement {
                        operation: keyword,
                        target: Some(target),
                        expression_variables: scan_expression_variables(expression, line_number),
                        line: line_number,
                        column: 1,
                    });
                }
                _ => {
                    return Err(fail(&format!("Unknown tagging statement: {keyword}.")));
                }
            }
        }
        if depth != 0 {
            return Err(ScriptError {
                message: format!("{script_name}: Unclosed if block."),
                line: 1,
                column: 1,
            });
        }
        Ok(statements)
    }
}

// --- preset migration ----------------------------------------------------------

/// Sidecar-pattern history: legacy (`cover`+cue/log/lrc/m3u/pls),
/// current 27-pattern default, and the skipped-aquarium lineage.
pub fn sidecar_default_history() -> Vec<Vec<String>> {
    let legacy: Vec<String> = LEGACY_DEFAULT_SIDECAR_PATTERNS
        .iter()
        .map(ToString::to_string)
        .collect();
    let current: Vec<String> = OrganizationManagementSettingsDto::default().sidecar_patterns;
    vec![legacy, current]
}

/// Migrate stored settings to the current preset catalog: refresh
/// unedited preset profiles/scripts, bump the catalog version, and heal
/// legacy sidecar patterns. Runs on read (persisted when it changes
/// anything) and before every write.
pub fn migrate_presets(settings: &mut LibraryManagementSettingsDto) {
    if settings.preset_catalog_version >= PRESET_CATALOG_VERSION {
        return;
    }
    let history = sidecar_default_history();
    for profile in &mut settings.profiles {
        if history.contains(&profile.organization.sidecar_patterns) {
            profile.organization.sidecar_patterns =
                OrganizationManagementSettingsDto::default().sidecar_patterns;
        }
    }
    // Refresh the Picard preset scripts when the stored copies are
    // byte-identical to a known preset generation (unedited).
    let (standard, multi_disc) = current_picard_preset_scripts(settings);
    for (fresh, id) in [
        (&standard, PICARD_NAMING_SCRIPT_ID),
        (&multi_disc, PICARD_MULTI_DISC_NAMING_SCRIPT_ID),
    ] {
        if let Some(stored) = settings
            .naming_scripts
            .iter_mut()
            .find(|script| script.id == *id)
            && stored.preset_origin.as_deref() == Some("picard_style_organizer")
            && stored.source != fresh.source
            && is_known_picard_source(&stored.source)
        {
            stored.name = fresh.name.clone();
            stored.source = fresh.source.clone();
            stored.preset_version = fresh.preset_version;
            stored.revision = naming_script_revision(stored);
        }
    }
    settings.preset_catalog_version = PRESET_CATALOG_VERSION;
}

/// Whether a naming source is a known Picard preset generation (the
/// standard, multi-disc, v3, or default templates).
fn is_known_picard_source(source: &str) -> bool {
    const KNOWN: &[&str] = &[
        PICARD_STANDARD_NAMING_SOURCE,
        PICARD_MULTI_DISC_NAMING_SOURCE,
        DEFAULT_NAMING_TEMPLATE,
    ];
    KNOWN.contains(&source)
}

// --- effective profile (pin) -----------------------------------------------------

/// Pin a profile for one root: overlay the assignment overrides onto a
/// detached copy. Unknown override script ids fail closed (the dry run
/// cannot run against a script that does not exist).
pub fn pin_profile(
    settings: &LibraryManagementSettingsDto,
    assignment: &LibraryManagementRootAssignmentDto,
) -> Result<LibraryManagementProfileDto, SettingsError> {
    let profile_id = assignment.profile_id.as_deref().unwrap_or_default();
    let mut profile = settings
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .cloned()
        .ok_or_else(|| invalid(&format!("Assigned profile does not exist: {profile_id}")))?;
    let Some(overrides) = &assignment.overrides else {
        return Ok(profile);
    };
    if let Some(enabled) = overrides.metadata_enabled {
        profile.metadata.enabled = enabled;
    }
    if let Some(enabled) = overrides.genres_enabled {
        profile.genres.enabled = enabled;
    }
    if let Some(enabled) = overrides.embedded_artwork_enabled {
        profile.artwork.embedded_enabled = enabled;
    }
    if let Some(enabled) = overrides.external_artwork_enabled {
        profile.artwork.external_enabled = enabled;
    }
    if let Some(enabled) = overrides.rename_enabled {
        profile.organization.rename_enabled = enabled;
    }
    if let Some(enabled) = overrides.move_enabled {
        profile.organization.move_enabled = enabled;
    }
    if let Some(move_sidecars) = overrides.move_sidecars {
        profile.organization.move_sidecars = move_sidecars;
    }
    if let Some(cleanup) = overrides.source_cleanup {
        profile.organization.source_cleanup = cleanup;
    }
    if let Some(preserve) = overrides.preserve_timestamps {
        profile.file_behavior.preserve_timestamps = preserve;
    }
    if let Some(script_id) = &overrides.naming_script_id {
        if !settings
            .naming_scripts
            .iter()
            .any(|script| &script.id == script_id)
        {
            return Err(invalid(&format!(
                "Override naming script does not exist: {script_id}"
            )));
        }
        profile.organization.naming_script_id = script_id.clone();
    }
    match overrides.multi_disc_naming_mode {
        MultiDiscNamingModeDto::Inherit => {}
        MultiDiscNamingModeDto::Standard => {
            profile.organization.multi_disc_naming_script_id = None;
        }
        MultiDiscNamingModeDto::Script => {
            let Some(script_id) = &overrides.multi_disc_naming_script_id else {
                return Err(invalid(
                    "A root multi-disc script override references an unknown naming script.",
                ));
            };
            if !settings
                .naming_scripts
                .iter()
                .any(|script| &script.id == script_id)
            {
                return Err(invalid(
                    "A root multi-disc script override references an unknown naming script.",
                ));
            }
            profile.organization.multi_disc_naming_script_id = Some(script_id.clone());
        }
    }
    if let Some(enabled) = overrides.automatic_edition_acceptance_enabled {
        profile.identity.automatic_edition_acceptance_enabled = enabled;
    }
    Ok(profile)
}

/// Resolve the naming scripts a pinned profile needs: (standard,
/// multi-disc-or-None). Unknown ids fail closed.
pub fn pin_naming_scripts(
    settings: &LibraryManagementSettingsDto,
    effective: &LibraryManagementProfileDto,
) -> Result<(NamingScriptDto, Option<NamingScriptDto>), SettingsError> {
    let standard = settings
        .naming_scripts
        .iter()
        .find(|script| script.id == effective.organization.naming_script_id)
        .cloned()
        .ok_or_else(|| {
            invalid(&format!(
                "Naming script does not exist: {}",
                effective.organization.naming_script_id
            ))
        })?;
    let multi_disc = match &effective.organization.multi_disc_naming_script_id {
        None => None,
        Some(script_id) => Some(
            settings
                .naming_scripts
                .iter()
                .find(|script| &script.id == script_id)
                .cloned()
                .ok_or_else(|| {
                    invalid(&format!(
                        "Multi-disc naming script does not exist: {script_id}"
                    ))
                })?,
        ),
    };
    Ok((standard, multi_disc))
}

/// Whether an assignment's saved activation is current: the effective
/// profile revision, naming policy, and library policy all match the
/// pins, and the dry-run proof (preview token, hash, confirmation) is
/// present.
pub fn activation_is_current(
    settings: &LibraryManagementSettingsDto,
    assignment: &LibraryManagementRootAssignmentDto,
    library_policy_revision: &str,
) -> bool {
    let (Some(pinned_profile), Some(pinned_naming), Some(pinned_policy)) = (
        assignment.activation_profile_revision.as_deref(),
        assignment.activation_naming_policy_revision.as_deref(),
        assignment.activation_policy_revision.as_deref(),
    ) else {
        return false;
    };
    if pinned_policy != library_policy_revision {
        return false;
    }
    if assignment
        .activation_preview_token
        .as_deref()
        .unwrap_or_default()
        .is_empty()
        || assignment
            .activation_preview_hash
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        || assignment.activation_confirmed_at.is_none()
    {
        return false;
    }
    let Ok(effective) = pin_profile(settings, assignment) else {
        return false;
    };
    let mut detached = effective;
    detached.revision = profile_revision(&detached);
    if detached.revision != pinned_profile {
        return false;
    }
    let Ok((standard, multi_disc)) = pin_naming_scripts(settings, &detached) else {
        return false;
    };
    naming_policy_revision(&standard, multi_disc.as_ref()) == pinned_naming
}

/// Whether a default-only migration is the sole activation drift: the
/// stored profile carries the current project defaults and the
/// activation is current for that same effective profile with a
/// historical default list substituted back in. Read-only: pins
/// converge on the next confirmed dry run.
pub fn migration_carry_applies(
    settings: &LibraryManagementSettingsDto,
    assignment: &LibraryManagementRootAssignmentDto,
    library_policy_revision: &str,
) -> bool {
    let Ok(effective) = pin_profile(settings, assignment) else {
        return false;
    };
    if effective.organization.sidecar_patterns
        != OrganizationManagementSettingsDto::default().sidecar_patterns
    {
        return false;
    }
    for historical in sidecar_default_history() {
        let mut pre_migration = effective.clone();
        pre_migration.organization.sidecar_patterns = historical;
        pre_migration.revision = profile_revision(&pre_migration);
        // The historical substitution must reproduce the pinned profile
        // revision exactly; the naming/policy pins are unchanged.
        if assignment.activation_profile_revision.as_deref()
            == Some(pre_migration.revision.as_str())
            && activation_pins_match_except_profile(settings, assignment, library_policy_revision)
        {
            return true;
        }
    }
    false
}

fn activation_pins_match_except_profile(
    settings: &LibraryManagementSettingsDto,
    assignment: &LibraryManagementRootAssignmentDto,
    library_policy_revision: &str,
) -> bool {
    let (Some(pinned_naming), Some(pinned_policy)) = (
        assignment.activation_naming_policy_revision.as_deref(),
        assignment.activation_policy_revision.as_deref(),
    ) else {
        return false;
    };
    if pinned_policy != library_policy_revision {
        return false;
    }
    if assignment
        .activation_preview_token
        .as_deref()
        .unwrap_or_default()
        .is_empty()
        || assignment
            .activation_preview_hash
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        || assignment.activation_confirmed_at.is_none()
    {
        return false;
    }
    let Ok(effective) = pin_profile(settings, assignment) else {
        return false;
    };
    let Ok((standard, multi_disc)) = pin_naming_scripts(settings, &effective) else {
        return false;
    };
    naming_policy_revision(&standard, multi_disc.as_ref()) == pinned_naming
}

// --- profile sharing (export/import bundles) -------------------------------------
//
// Deterministic, inert profile bundles. The portable document carries the
// profile with identity fields stripped and script references rewritten to
// `naming-N`/`tagging-N` keys; the share code is the same document
// zlib-compressed (level 9) and base64url-encoded behind the `DNLP1:`
// prefix. Both forms verify against the `sha256:` checksum over the
// canonical payload. Byte-compatible with v2: either side reads the
// other's exports.

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
    pub profile: LibraryManagementProfileDto,
    /// Imported naming scripts.
    pub naming_scripts: Vec<NamingScriptDto>,
    /// Imported tagging scripts.
    pub tagging_scripts: Vec<TaggingScriptDto>,
}

/// One import warning: machine code plus human title and message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileImportWarningDto {
    /// Stable warning code.
    pub code: String,
    /// `warning` or `danger`.
    pub severity: String,
    /// Short title.
    pub title: String,
    /// What the profile will do.
    pub message: String,
}

/// Bundle canonical JSON: sorted keys, compact separators, raw UTF-8 —
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
    profile: &LibraryManagementProfileDto,
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
    profile: &LibraryManagementProfileDto,
    naming_scripts: &[NamingScriptDto],
    tagging_scripts: &[TaggingScriptDto],
    ids: &dyn IdGenerator,
) -> Result<EncodedProfileBundle, SettingsError> {
    let naming_by_id: BTreeMap<&str, &NamingScriptDto> = naming_scripts
        .iter()
        .map(|script| (script.id.as_str(), script))
        .collect();
    let tagging_by_id: BTreeMap<&str, &TaggingScriptDto> = tagging_scripts
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
    let profile: LibraryManagementProfileDto = serde_json::from_value(value)
        .map_err(|_| invalid("The shared profile contains invalid settings."))?;
    if profile
        .metadata
        .fields
        .iter()
        .any(|field| field.mode == FieldModeDto::Preserve)
    {
        return Err(invalid(
            "Legacy Preserve metadata modes cannot be imported.",
        ));
    }
    if profile.metadata.artist_credits.standardization == ArtistStandardizationDto::Variations {
        return Err(invalid(
            "Legacy artist-variation settings cannot be imported.",
        ));
    }
    if profile
        .artwork
        .providers
        .contains(&ArtworkProviderDto::Audiodb)
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
    let mut detached = LibraryManagementSettingsDto {
        profiles: vec![profile],
        naming_scripts: parsed
            .payload
            .naming_scripts
            .iter()
            .zip(naming_assigned)
            .map(|(script, id)| NamingScriptDto {
                id,
                name: script.name.clone(),
                source: script.source.clone(),
                ..NamingScriptDto::default()
            })
            .collect(),
        tagging_scripts: parsed
            .payload
            .tagging_scripts
            .iter()
            .zip(tagging_assigned)
            .map(|(script, id)| TaggingScriptDto {
                id,
                name: script.name.clone(),
                source: script.source.clone(),
                ..TaggingScriptDto::default()
            })
            .collect(),
        ..LibraryManagementSettingsDto::default()
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
    settings: &LibraryManagementSettingsDto,
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
    let mut detached = LibraryManagementSettingsDto {
        profiles: vec![renamed.profile.clone()],
        naming_scripts: renamed.naming_scripts.clone(),
        tagging_scripts: renamed.tagging_scripts.clone(),
        ..LibraryManagementSettingsDto::default()
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
pub fn profile_aspects(profile: &LibraryManagementProfileDto) -> Vec<String> {
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
) -> ProfileImportWarningDto {
    ProfileImportWarningDto {
        code: code.to_owned(),
        severity: severity.to_owned(),
        title: title.to_owned(),
        message,
    }
}

/// Warnings for a profile under review: destructive tag, container,
/// source, artwork, enrichment, and server-refresh behavior, each
/// with a stable code and severity.
pub fn profile_import_warnings(
    profile: &LibraryManagementProfileDto,
) -> Vec<ProfileImportWarningDto> {
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
            .any(|field| field.mode == FieldModeDto::Replace && field.clear_when_canonical_missing)
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
    if profile.metadata.enabled && compatibility.mp3_apev2_policy == Mp3ApePolicyDto::Remove {
        warnings.push(import_warning(
            "remove_mp3_apev2",
            "danger",
            "Removes APEv2 tags from MP3 files",
            "The complete MP3 APEv2 tag container is deleted.".to_owned(),
        ));
    }
    if profile.metadata.enabled
        && compatibility.raw_aac_tag_policy == RawAacTagPolicyDto::RemoveApev2
    {
        warnings.push(import_warning(
            "remove_raw_aac_apev2",
            "danger",
            "Removes APEv2 tags from raw AAC files",
            "The APEv2 tag container and any artwork stored there are deleted.".to_owned(),
        ));
    } else if profile.metadata.enabled
        && compatibility.raw_aac_tag_policy == RawAacTagPolicyDto::DoNotWrite
    {
        warnings.push(import_warning(
            "skip_raw_aac_tags",
            "warning",
            "Does not write tags to raw AAC files",
            "Managed metadata changes are skipped for raw AAC files.".to_owned(),
        ));
    }
    if profile.metadata.enabled && compatibility.wav_tag_policy != WavTagPolicyDto::PreserveExisting
    {
        let wav_format = if compatibility.wav_tag_policy == WavTagPolicyDto::Id3 {
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
        && profile.organization.source_cleanup == SourceCleanupModeDto::RemoveAfterConfirmedMove
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
        && profile.enrichment.replaygain.mode == ReplayGainModeDto::Replace
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

// --- impact classification -------------------------------------------------------

/// Whether an assignment engages automatic work (custom editions excluded:
// gaining custom-edition automation is always destructive on its own).
pub fn active_automatic(assignment: Option<&LibraryManagementRootAssignmentDto>) -> bool {
    assignment.is_some_and(|assignment| {
        assignment.enabled
            && (assignment.automatic_acquisitions
                || assignment.automatic_drop_imports
                || assignment.automatic_scan_discovered)
    })
}

/// Scope payload for impact diffing: the effective profile minus
/// identity/notification, plus the naming/tagging script revisions it
/// pins. Missing scripts resolve to None (compare-equal), never fail.
pub fn scope_payload(
    settings: &LibraryManagementSettingsDto,
    assignment: &LibraryManagementRootAssignmentDto,
) -> serde_json::Value {
    let mut payload = match pin_profile(settings, assignment) {
        Ok(effective) => serde_json::to_value(&effective).unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    };
    if let Some(map) = payload.as_object_mut() {
        for field in [
            "id",
            "name",
            "description",
            "preset_origin",
            "preset_version",
            "revision",
            "notification",
        ] {
            map.remove(field);
        }
    }
    let naming: BTreeMap<&str, &NamingScriptDto> = settings
        .naming_scripts
        .iter()
        .map(|script| (script.id.as_str(), script))
        .collect();
    let tagging: BTreeMap<&str, &TaggingScriptDto> = settings
        .tagging_scripts
        .iter()
        .map(|script| (script.id.as_str(), script))
        .collect();
    let (naming_id, multi_id, external_id, tagging_ids) = match pin_profile(settings, assignment) {
        Ok(effective) => (
            Some(effective.organization.naming_script_id.clone()),
            effective.organization.multi_disc_naming_script_id.clone(),
            effective.artwork.external_naming_script_id.clone(),
            effective.metadata.tagging_script_ids.clone(),
        ),
        Err(_) => (None, None, None, Vec::new()),
    };
    if let Some(map) = payload.as_object_mut() {
        map.insert(
            "_naming_script_revision".to_owned(),
            naming_id
                .and_then(|id| naming.get(id.as_str()))
                .map(|script| serde_json::Value::String(script.revision.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "_multi_disc_naming_script_revision".to_owned(),
            multi_id
                .and_then(|id| naming.get(id.as_str()))
                .map(|script| serde_json::Value::String(script.revision.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "_naming_policy_revision".to_owned(),
            match pin_profile(settings, assignment)
                .ok()
                .and_then(|effective| pin_naming_scripts(settings, &effective).ok())
            {
                Some((standard, multi_disc)) => serde_json::Value::String(naming_policy_revision(
                    &standard,
                    multi_disc.as_ref(),
                )),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "_external_artwork_script_revision".to_owned(),
            external_id
                .and_then(|id| naming.get(id.as_str()))
                .map(|script| serde_json::Value::String(script.revision.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "_tagging_script_revisions".to_owned(),
            serde_json::Value::Array(
                tagging_ids
                    .iter()
                    .map(|id| {
                        tagging
                            .get(id.as_str())
                            .map(|script| serde_json::Value::String(script.revision.clone()))
                            .unwrap_or(serde_json::Value::Null)
                    })
                    .collect(),
            ),
        );
    }
    payload
}

fn field_mode_rank(mode: &str) -> i64 {
    match mode {
        "disabled" | "preserve" => 0,
        "fill_missing" => 1,
        "merge" => 2,
        "replace" => 3,
        _ => 3,
    }
}

fn genre_mode_rank(mode: &str) -> i64 {
    match mode {
        "fill_missing" => 0,
        "merge" => 1,
        "replace" => 2,
        _ => 2,
    }
}

fn ordered_subset(candidate: &[serde_json::Value], original: &[serde_json::Value]) -> bool {
    let kept: Vec<&serde_json::Value> = original
        .iter()
        .filter(|value| candidate.contains(value))
        .collect();
    candidate.len() == kept.len() && candidate.iter().zip(kept.iter()).all(|(a, b)| a == *b)
}

/// Artwork `preserve_existing_types` as a string set (image-type ids).
fn preserve_existing_types(profile: &serde_json::Value) -> BTreeSet<&str> {
    profile
        .get("artwork")
        .and_then(|artwork| artwork.get("preserve_existing_types"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        })
        .unwrap_or_default()
}

fn reset_safe_boolean(
    candidate: &mut serde_json::Value,
    old: &serde_json::Value,
    path: &[&str],
    safe_from: bool,
    safe_to: bool,
) {
    let mut old_node = old;
    let mut candidate_node = candidate;
    for key in &path[..path.len() - 1] {
        match (old_node.get(key), candidate_node.get_mut(key)) {
            (Some(next_old), Some(next_candidate)) => {
                old_node = next_old;
                candidate_node = next_candidate;
            }
            _ => return,
        }
    }
    let key = path[path.len() - 1];
    let old_value = old_node.get(key).and_then(serde_json::Value::as_bool);
    let candidate_value = candidate_node.get(key).and_then(serde_json::Value::as_bool);
    if old_value == Some(safe_from) && candidate_value == Some(safe_to) {
        candidate_node[key] = serde_json::Value::Bool(safe_from);
    }
}

/// Whether a profile change only narrows write scope: reset every
/// safe-direction boolean/list/threshold in a candidate copy, then the
/// change is restrictive exactly when nothing else differs.
pub fn is_restrictive_profile_change(
    old_profile: &LibraryManagementProfileDto,
    new_profile: &LibraryManagementProfileDto,
) -> bool {
    let old = serde_json::to_value(old_profile).unwrap_or(serde_json::Value::Null);
    let new = serde_json::to_value(new_profile).unwrap_or(serde_json::Value::Null);
    if old == new {
        return false;
    }
    let mut candidate = new.clone();
    for path in [
        ["metadata", "enabled"].as_slice(),
        &["metadata", "relationships", "enabled"],
        &["genres", "enabled"],
        &["artwork", "embedded_enabled"],
        &["artwork", "external_enabled"],
        &["organization", "rename_enabled"],
        &["organization", "move_enabled"],
        &["organization", "move_sidecars"],
        &["organization", "remove_empty_directories"],
        &["enrichment", "lyrics", "enabled"],
        &["enrichment", "replaygain", "enabled"],
        &["identity", "automatic_edition_acceptance_enabled"],
    ] {
        reset_safe_boolean(&mut candidate, &old, path, true, false);
    }
    for path in [
        ["metadata", "preserve_embedded_art_during_scrub"].as_slice(),
        &["genres", "listenbrainz_curated_only"],
        &["genres", "lastfm_whitelist_only"],
        &["genres", "write_primary_only_for_constrained_formats"],
        &["artwork", "approved_only"],
        &["artwork", "embedded_front_only"],
        &["artwork", "external_front_only"],
        &["artwork", "never_replace_with_smaller"],
        &["file_behavior", "preserve_timestamps"],
        &["file_behavior", "preserve_permissions"],
        &["file_behavior", "strict_capability_gate"],
        &["file_behavior", "validate_written_metadata"],
        &["file_behavior", "validate_technical_audio"],
    ] {
        reset_safe_boolean(&mut candidate, &old, path, false, true);
    }
    reset_safe_boolean(
        &mut candidate,
        &old,
        &["metadata", "scrub_unmanaged_tags"],
        true,
        false,
    );
    reset_safe_boolean(
        &mut candidate,
        &old,
        &["artwork", "overwrite_external_files"],
        true,
        false,
    );

    let old_fields: BTreeMap<&str, &serde_json::Value> = old
        .get("metadata")
        .and_then(|metadata| metadata.get("fields"))
        .and_then(serde_json::Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| {
                    field
                        .get("field")
                        .and_then(serde_json::Value::as_str)
                        .map(|name| (name, field))
                })
                .collect()
        })
        .unwrap_or_default();
    let new_fields: BTreeMap<&str, &serde_json::Value> = new
        .get("metadata")
        .and_then(|metadata| metadata.get("fields"))
        .and_then(serde_json::Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| {
                    field
                        .get("field")
                        .and_then(serde_json::Value::as_str)
                        .map(|name| (name, field))
                })
                .collect()
        })
        .unwrap_or_default();
    let fields_narrow = new_fields
        .keys()
        .all(|field| old_fields.contains_key(field))
        && new_fields.iter().all(|(field, value)| {
            let old_field = old_fields[field];
            let mode_ok = field_mode_rank(
                value
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ) <= field_mode_rank(
                old_field
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            );
            let clear_ok = !value
                .get("clear_when_canonical_missing")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                || old_field
                    .get("clear_when_canonical_missing")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
            mode_ok && clear_ok
        });
    if fields_narrow
        && let Some(fields) = candidate
            .get_mut("metadata")
            .and_then(|metadata| metadata.get_mut("fields"))
        && let Some(old_raw) = old
            .get("metadata")
            .and_then(|metadata| metadata.get("fields"))
    {
        *fields = old_raw.clone();
    }

    let old_preserved: BTreeSet<&str> = old
        .get("metadata")
        .and_then(|metadata| metadata.get("preserve_fields"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        })
        .unwrap_or_default();
    let new_preserved: BTreeSet<&str> = new
        .get("metadata")
        .and_then(|metadata| metadata.get("preserve_fields"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        })
        .unwrap_or_default();
    if new_preserved.is_superset(&old_preserved)
        && let Some(preserved) = candidate
            .get_mut("metadata")
            .and_then(|metadata| metadata.get_mut("preserve_fields"))
        && let Some(old_raw) = old
            .get("metadata")
            .and_then(|metadata| metadata.get("preserve_fields"))
    {
        *preserved = old_raw.clone();
    }

    let old_relationships = old
        .get("metadata")
        .and_then(|metadata| metadata.get("relationships"))
        .and_then(|relationships| relationships.get("types"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let new_relationships = new
        .get("metadata")
        .and_then(|metadata| metadata.get("relationships"))
        .and_then(|relationships| relationships.get("types"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if ordered_subset(&new_relationships, &old_relationships)
        && let Some(types) = candidate
            .get_mut("metadata")
            .and_then(|metadata| metadata.get_mut("relationships"))
            .and_then(|relationships| relationships.get_mut("types"))
    {
        *types = serde_json::Value::Array(old_relationships);
    }

    let genre_mode_ok = genre_mode_rank(
        new.get("genres")
            .and_then(|genres| genres.get("mode"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    ) <= genre_mode_rank(
        old.get("genres")
            .and_then(|genres| genres.get("mode"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    );
    if genre_mode_ok
        && let Some(mode) = candidate
            .get_mut("genres")
            .and_then(|genres| genres.get_mut("mode"))
        && let Some(old_raw) = old.get("genres").and_then(|genres| genres.get("mode"))
    {
        *mode = old_raw.clone();
    }
    let old_sources = old
        .get("genres")
        .and_then(|genres| genres.get("sources"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let new_sources = new
        .get("genres")
        .and_then(|genres| genres.get("sources"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if ordered_subset(&new_sources, &old_sources)
        && let Some(sources) = candidate
            .get_mut("genres")
            .and_then(|genres| genres.get_mut("sources"))
    {
        *sources = serde_json::Value::Array(old_sources);
    }
    let count_ok = new
        .get("genres")
        .and_then(|genres| genres.get("maximum_count"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(i64::MAX)
        <= old
            .get("genres")
            .and_then(|genres| genres.get("maximum_count"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(i64::MIN);
    if count_ok
        && let Some(count) = candidate
            .get_mut("genres")
            .and_then(|genres| genres.get_mut("maximum_count"))
        && let Some(old_raw) = old
            .get("genres")
            .and_then(|genres| genres.get("maximum_count"))
    {
        *count = old_raw.clone();
    }
    for threshold in [
        "musicbrainz_minimum_count",
        "listenbrainz_minimum_count",
        "lastfm_minimum_weight",
    ] {
        let raises = new
            .get("genres")
            .and_then(|genres| genres.get(threshold))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(i64::MIN)
            >= old
                .get("genres")
                .and_then(|genres| genres.get(threshold))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(i64::MAX);
        if raises
            && let Some(node) = candidate
                .get_mut("genres")
                .and_then(|genres| genres.get_mut(threshold))
            && let Some(old_raw) = old.get("genres").and_then(|genres| genres.get(threshold))
        {
            *node = old_raw.clone();
        }
    }

    for field in ["providers", "image_types"] {
        let old_list = old
            .get("artwork")
            .and_then(|artwork| artwork.get(field))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let new_list = new
            .get("artwork")
            .and_then(|artwork| artwork.get(field))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if ordered_subset(&new_list, &old_list)
            && let Some(node) = candidate
                .get_mut("artwork")
                .and_then(|artwork| artwork.get_mut(field))
        {
            *node = serde_json::Value::Array(old_list);
        }
    }
    let old_preserve_types = preserve_existing_types(&old);
    let new_preserve_types = preserve_existing_types(&new);
    if new_preserve_types.is_superset(&old_preserve_types)
        && let Some(node) = candidate
            .get_mut("artwork")
            .and_then(|artwork| artwork.get_mut("preserve_existing_types"))
        && let Some(old_raw) = old
            .get("artwork")
            .and_then(|artwork| artwork.get("preserve_existing_types"))
    {
        *node = old_raw.clone();
    }
    for field in ["minimum_width", "minimum_height"] {
        let raises = new
            .get("artwork")
            .and_then(|artwork| artwork.get(field))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(i64::MIN)
            >= old
                .get("artwork")
                .and_then(|artwork| artwork.get(field))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(i64::MAX);
        if raises
            && let Some(node) = candidate
                .get_mut("artwork")
                .and_then(|artwork| artwork.get_mut(field))
            && let Some(old_raw) = old.get("artwork").and_then(|artwork| artwork.get(field))
        {
            *node = old_raw.clone();
        }
    }

    let old_sidecars = old
        .get("organization")
        .and_then(|organization| organization.get("sidecar_patterns"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let new_sidecars = new
        .get("organization")
        .and_then(|organization| organization.get("sidecar_patterns"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if ordered_subset(&new_sidecars, &old_sidecars)
        && let Some(node) = candidate
            .get_mut("organization")
            .and_then(|organization| organization.get_mut("sidecar_patterns"))
    {
        *node = serde_json::Value::Array(old_sidecars);
    }
    let cleanup_narrows = old
        .get("organization")
        .and_then(|organization| organization.get("source_cleanup"))
        .and_then(serde_json::Value::as_str)
        == Some("remove_after_confirmed_move")
        && new
            .get("organization")
            .and_then(|organization| organization.get("source_cleanup"))
            .and_then(serde_json::Value::as_str)
            == Some("keep");
    if cleanup_narrows
        && let Some(node) = candidate
            .get_mut("organization")
            .and_then(|organization| organization.get_mut("source_cleanup"))
        && let Some(old_raw) = old
            .get("organization")
            .and_then(|organization| organization.get("source_cleanup"))
    {
        *node = old_raw.clone();
    }

    candidate == old
}

/// Impact verdict for a candidate: classification, preview requirement,
/// affected roots, and human reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeImpact {
    /// Current stored revision.
    pub current_settings_revision: String,
    /// Normalized candidate revision.
    pub proposed_settings_revision: String,
    /// The caller's revision was already stale.
    pub stale: bool,
    /// `no_change`, `harmless`, `restrictive`, or `destructive`.
    pub classification: String,
    /// A current dry run must be confirmed before enabling.
    pub preview_required: bool,
    /// Affected root ids.
    pub affected_root_ids: Vec<String>,
    /// Human reasons.
    pub reasons: Vec<String>,
}

/// Classify a candidate against the current settings.
pub fn classify(
    current: &LibraryManagementSettingsDto,
    candidate: &LibraryManagementSettingsDto,
    expected_settings_revision: Option<&str>,
) -> ChangeImpact {
    let current_revision = settings_revision(current);
    let proposed_revision = settings_revision(candidate);
    let stale = expected_settings_revision.is_some_and(|expected| expected != current_revision);
    if current_revision == proposed_revision {
        return ChangeImpact {
            current_settings_revision: current_revision,
            proposed_settings_revision: proposed_revision,
            stale,
            classification: "no_change".to_owned(),
            preview_required: false,
            affected_root_ids: Vec::new(),
            reasons: Vec::new(),
        };
    }
    let current_assignments: BTreeMap<&str, &LibraryManagementRootAssignmentDto> = current
        .root_assignments
        .iter()
        .map(|assignment| (assignment.root_id.as_str(), assignment))
        .collect();
    let candidate_assignments: BTreeMap<&str, &LibraryManagementRootAssignmentDto> = candidate
        .root_assignments
        .iter()
        .map(|assignment| (assignment.root_id.as_str(), assignment))
        .collect();
    let mut roots: BTreeSet<&str> = BTreeSet::new();
    roots.extend(current_assignments.keys().copied());
    roots.extend(candidate_assignments.keys().copied());

    let mut destructive = Vec::new();
    let mut restrictive = Vec::new();
    let mut harmless = Vec::new();
    let mut affected = BTreeSet::new();
    for root_id in roots {
        let old_assignment = current_assignments.get(root_id).copied();
        let new_assignment = candidate_assignments.get(root_id).copied();
        let old_active = active_automatic(old_assignment);
        let new_active = active_automatic(new_assignment);
        if !old_active && new_active {
            destructive.push(format!(
                "Automatic Library Management is enabled for root {root_id}."
            ));
            affected.insert(root_id.to_owned());
            continue;
        }
        if old_active && !new_active {
            restrictive.push(format!(
                "Automatic Library Management is reduced for root {root_id}."
            ));
            affected.insert(root_id.to_owned());
            continue;
        }
        let (Some(old_assignment), Some(new_assignment)) = (old_assignment, new_assignment) else {
            continue;
        };
        if !old_active {
            continue;
        }
        let added_trigger = (new_assignment.automatic_acquisitions
            && !old_assignment.automatic_acquisitions)
            || (new_assignment.automatic_drop_imports && !old_assignment.automatic_drop_imports)
            || (new_assignment.automatic_scan_discovered
                && !old_assignment.automatic_scan_discovered);
        let removed_trigger = (old_assignment.automatic_acquisitions
            && !new_assignment.automatic_acquisitions)
            || (old_assignment.automatic_drop_imports && !new_assignment.automatic_drop_imports)
            || (old_assignment.automatic_scan_discovered
                && !new_assignment.automatic_scan_discovered);
        let old_profile = pin_profile(current, old_assignment);
        let new_profile = pin_profile(candidate, new_assignment);
        let old_payload = scope_payload(current, old_assignment);
        let new_payload = scope_payload(candidate, new_assignment);
        if added_trigger {
            harmless.push(format!(
                "An automatic trigger is enabled for root {root_id}; the authorized write profile is unchanged."
            ));
            affected.insert(root_id.to_owned());
        }
        let old_custom = old_assignment.enabled
            && old_assignment.automatic_scan_discovered
            && old_assignment.automatic_custom_editions;
        let new_custom = new_assignment.enabled
            && new_assignment.automatic_scan_discovered
            && new_assignment.automatic_custom_editions;
        if new_custom && !old_custom {
            destructive.push(format!(
                "Automatic Custom edition management is enabled for root {root_id}."
            ));
            affected.insert(root_id.to_owned());
        }
        if old_payload != new_payload {
            affected.insert(root_id.to_owned());
            match (old_profile, new_profile) {
                (Ok(old), Ok(new)) if is_restrictive_profile_change(&old, &new) => {
                    restrictive.push(format!(
                        "The effective profile is restricted for root {root_id}."
                    ));
                }
                _ => {
                    destructive.push(format!(
                        "The effective profile changes write scope for root {root_id}."
                    ));
                }
            }
        } else if removed_trigger {
            restrictive.push(format!(
                "An automatic trigger is disabled for root {root_id}."
            ));
            affected.insert(root_id.to_owned());
        }
    }

    let (classification, reasons) = if destructive.is_empty() {
        if restrictive.is_empty() {
            (
                "harmless",
                if harmless.is_empty() {
                    vec!["No enabled automatic root gains file-writing scope.".to_owned()]
                } else {
                    harmless
                },
            )
        } else {
            restrictive.extend(harmless);
            ("restrictive", restrictive)
        }
    } else {
        destructive.extend(restrictive);
        ("destructive", destructive)
    };
    ChangeImpact {
        current_settings_revision: current_revision,
        proposed_settings_revision: proposed_revision,
        stale,
        classification: classification.to_owned(),
        preview_required: classification == "destructive",
        affected_root_ids: affected.into_iter().collect(),
        reasons,
    }
}

// --- activation health -------------------------------------------------------------

/// Dry-run activation health: stale roots (saved activation no longer
/// matches) and blocked roots (no dry run could help).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActivationHealth {
    /// Active roots whose activation no longer matches.
    pub stale_root_ids: Vec<String>,
    /// Active roots no dry run could help.
    pub blocked_root_ids: Vec<String>,
    /// Policy error, set only with a non-empty `blocked_root_ids`.
    pub blocked_reason: Option<String>,
}

/// Validate every root assignment against the library policy: the
/// policy resolves, the recycle bin overlaps no root, every assignment
/// names a known root, and every active root is an available writable
/// directory. Returns the resolved policy revision.
pub fn validate_root_assignments(
    settings: &LibraryManagementSettingsDto,
    library: &super::models::LibrarySettingsDto,
) -> Result<String, SettingsError> {
    let resolved = super::library_policy::resolve(library)?;
    if !settings.recycle_bin_path.trim().is_empty() {
        let recycle = std::path::PathBuf::from(settings.recycle_bin_path.trim());
        for root in &resolved.settings.library_roots {
            let library_root = std::path::PathBuf::from(&root.path);
            if recycle == library_root
                || recycle.starts_with(&library_root)
                || library_root.starts_with(&recycle)
            {
                return Err(invalid(
                    "The Library Management recycle bin cannot overlap a library root.",
                ));
            }
        }
    }
    let roots: BTreeMap<&str, &super::models::LibraryRootDto> = resolved
        .settings
        .library_roots
        .iter()
        .map(|root| (root.id.as_str(), root))
        .collect();
    for assignment in &settings.root_assignments {
        let Some(root) = roots.get(assignment.root_id.as_str()) else {
            return Err(invalid(
                "A Library Management assignment references an unknown root.",
            ));
        };
        if !active_automatic(Some(assignment)) {
            continue;
        }
        let path = std::path::Path::new(&root.path);
        if !path.exists() || !path.is_dir() {
            return Err(invalid(&format!(
                "Library root {} is not currently available.",
                root.label
            )));
        }
        // Writable check: the directory must accept a probe file. The
        // probe is created and removed atomically; failure means
        // read-only (or a permissions error either way).
        let probe = path.join(".droppedneedle-write-probe");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
            }
            Err(_) => {
                return Err(invalid(&format!(
                    "Library root {} is not currently writable.",
                    root.label
                )));
            }
        }
    }
    Ok(resolved.policy_revision)
}

/// Compute activation health for the active automatic roots. One broken
/// assignment must not hide the others: roots whose effective profile
/// cannot resolve land in blocked; roots that resolve but no longer
/// match land in stale (unless a default-only migration carries them).
/// A policy that cannot resolve at all blocks every active root with
/// the policy error as the reason.
pub fn activation_health(
    settings: &LibraryManagementSettingsDto,
    library: &super::models::LibrarySettingsDto,
) -> ActivationHealth {
    let mut health = ActivationHealth::default();
    let policy_revision = match validate_root_assignments(settings, library) {
        Ok(revision) => revision,
        Err(error) => {
            let blocked: Vec<String> = settings
                .root_assignments
                .iter()
                .filter(|assignment| active_automatic(Some(assignment)))
                .map(|assignment| assignment.root_id.clone())
                .collect();
            if !blocked.is_empty() {
                health.blocked_root_ids = blocked;
                health.blocked_reason = Some(match error {
                    SettingsError::InvalidInput { message } => message,
                    other => format!("{other:?}"),
                });
            }
            return health;
        }
    };
    for assignment in &settings.root_assignments {
        if !active_automatic(Some(assignment)) {
            continue;
        }
        let pinned = pin_profile(settings, assignment)
            .ok()
            .and_then(|effective| {
                pin_naming_scripts(settings, &effective)
                    .ok()
                    .map(|_| effective)
            });
        if pinned.is_none() {
            health.blocked_root_ids.push(assignment.root_id.clone());
            continue;
        }
        if activation_is_current(settings, assignment, &policy_revision)
            || migration_carry_applies(settings, assignment, &policy_revision)
        {
            continue;
        }
        health.stale_root_ids.push(assignment.root_id.clone());
    }
    health
}

// --- preset diff -----------------------------------------------------------------------

/// Group-level preset drift: which top-level profile groups differ from
/// the tracked preset, plus the preset profile for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetDiff {
    /// Profile id.
    pub profile_id: String,
    /// Preset origin, when tracked.
    pub preset_origin: Option<String>,
    /// Preset version, when tracked.
    pub preset_version: Option<i64>,
    /// Whether any group differs.
    pub differs: bool,
    /// Differing groups.
    pub changed_groups: Vec<String>,
    /// Groups a preset version upgrade would touch.
    pub version_upgrade_groups: Vec<String>,
    /// The preset profile, when tracked.
    pub preset_profile: Option<LibraryManagementProfileDto>,
}

/// Groups compared by the preset diff, in order.
pub const PRESET_DIFF_GROUPS: &[&str] = &[
    "metadata",
    "genres",
    "artwork",
    "organization",
    "file_behavior",
    "enrichment",
    "identity",
    "notification",
];

/// Diff one profile against its tracked preset.
pub fn preset_diff(profile: &LibraryManagementProfileDto) -> PresetDiff {
    let origin = profile.preset_origin.clone();
    let version = profile.preset_version;
    let preset = origin.as_deref().and_then(preset_profile_for_origin);
    let Some(preset) = preset else {
        return PresetDiff {
            profile_id: profile.id.clone(),
            preset_origin: origin,
            preset_version: version,
            differs: false,
            changed_groups: Vec::new(),
            version_upgrade_groups: Vec::new(),
            preset_profile: None,
        };
    };
    let current_value = serde_json::to_value(profile).unwrap_or(serde_json::Value::Null);
    let preset_value = serde_json::to_value(&preset).unwrap_or(serde_json::Value::Null);
    let mut changed = Vec::new();
    for group in PRESET_DIFF_GROUPS {
        if current_value.get(group) != preset_value.get(group) {
            changed.push((*group).to_owned());
        }
    }
    // A version upgrade touches the groups whose preset content differs
    // between the tracked version and the current catalog: the catalog
    // carries one generation, so any tracked-but-drifted group is an
    // upgrade touch.
    let version_upgrade_groups = if version.is_some_and(|v| v < PICARD_ORGANIZER_PRESET_VERSION)
        && origin.as_deref() == Some("picard_style_organizer")
    {
        changed.clone()
    } else {
        Vec::new()
    };
    PresetDiff {
        profile_id: profile.id.clone(),
        preset_origin: origin,
        preset_version: version,
        differs: !changed.is_empty(),
        changed_groups: changed,
        version_upgrade_groups,
        preset_profile: Some(preset),
    }
}

/// Identity fields shared by both script kinds.
pub trait ScriptRef {
    /// Script id.
    fn script_id(&self) -> &str;
    /// Script name.
    fn script_name(&self) -> &str;
    /// Script source.
    fn script_source(&self) -> &str;
}

impl ScriptRef for NamingScriptDto {
    fn script_id(&self) -> &str {
        &self.id
    }
    fn script_name(&self) -> &str {
        &self.name
    }
    fn script_source(&self) -> &str {
        &self.source
    }
}

impl ScriptRef for TaggingScriptDto {
    fn script_id(&self) -> &str {
        &self.id
    }
    fn script_name(&self) -> &str {
        &self.name
    }
    fn script_source(&self) -> &str {
        &self.source
    }
}
