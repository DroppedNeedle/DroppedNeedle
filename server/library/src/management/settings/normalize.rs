//! Normalize-and-validate for a candidate settings document, plus the
//! save-time carry of activation pins and migration history.

use std::collections::{BTreeMap, BTreeSet};

use super::activation::pin_profile;
use super::invalid;
use super::presets::{
    LEGACY_DEFAULT_SIDECAR_PATTERNS, MANAGED_FIELDS, MANAGEMENT_SCHEMA_VERSION,
    PRESET_CATALOG_VERSION, managed_field,
};
use super::revision::{naming_script_revision, profile_revision, tagging_script_revision};
use super::script::{validate_naming_language, validate_tagging_language};
use droppedneedle_kernel::runtime_config::sections::{
    ArtistStandardization, ArtworkProvider, FieldMode, Id3TextEncoding, Id3Version,
    LibraryManagement, LibraryManagementProfile, LibraryManagementRootAssignment,
    LibraryManagementRootOverrides, ManagedField, MultiDiscNamingMode, NamingScript,
    OrganizationManagementSettings, TaggingScript,
};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;

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
    current: &LibraryManagement,
    incoming: &mut LibraryManagement,
    suggested_profile_id: &str,
) -> Result<Vec<String>, SettingsError> {
    use std::collections::HashMap;
    complete_fields(incoming);
    let previous_assignments: HashMap<&str, &LibraryManagementRootAssignment> = current
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
            // The legacy default list migrates silently to the current one.
            profile.organization.sidecar_patterns =
                OrganizationManagementSettings::default().sidecar_patterns;
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
                    assignment.activation_confirmed_by = previous.activation_confirmed_by.clone();
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
fn canonical_overrides(overrides: &LibraryManagementRootOverrides) -> String {
    crate::policy::canonical_json(
        &serde_json::to_value(overrides).unwrap_or(serde_json::Value::Null),
    )
}

/// Clear an assignment's activation pins.
fn clear_activation(assignment: &mut LibraryManagementRootAssignment) {
    assignment.activation_profile_revision = None;
    assignment.activation_naming_policy_revision = None;
    assignment.activation_policy_revision = None;
    assignment.activation_settings_revision = None;
    assignment.activation_preview_token = None;
    assignment.activation_preview_hash = None;
    assignment.activation_confirmed_at = None;
    assignment.activation_confirmed_by = None;
}

/// Profile spellings that name another field.
const FIELD_ALIASES: &[(&str, &str)] = &[
    ("albumartist", "album_artist"),
    ("albumartists", "album_artists"),
    ("track", "track_number"),
    ("disc", "disc_number"),
    ("year", "date"),
];

/// Fields that were written under another field's entry before each had
/// its own: (field, the entry that wrote it).
const FORMER_PARENTS: &[(&str, &str)] = &[
    ("artist_sort", "artist"),
    ("album_artist_sort", "album_artist"),
    ("disc_subtitle", "disc_number"),
    ("original_date", "date"),
    ("release_status", "album"),
    ("release_country", "album"),
    ("release_type", "album"),
    ("compilation", "album"),
    ("asin", "album"),
    ("musicbrainz_release_track_id", "musicbrainz_recording_id"),
];

/// Whether a stored field entry names a field, an alias of one, or an
/// entry the completion drops (`genre`, `publisher`).
pub(super) fn is_field_name(name: &str) -> bool {
    matches!(name, "genre" | "publisher") || managed_field(canonical_field(name)).is_some()
}

fn canonical_field(name: &str) -> &str {
    FIELD_ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map_or(name, |(_, field)| field)
}

/// How much of the source a mode lets through. Entries for one field were
/// applied in turn, so the strongest one decided what was written.
fn strength(mode: FieldMode) -> u8 {
    match mode {
        FieldMode::Disabled | FieldMode::Preserve => 0,
        FieldMode::FillMissing => 1,
        FieldMode::Merge => 2,
        FieldMode::Replace => 3,
    }
}

/// Bring every profile to one entry per managed field, writing exactly
/// what it wrote before. A folder activation that matched its profile
/// before still matches it after. Returns whether anything changed.
pub fn complete_fields(settings: &mut LibraryManagement) -> bool {
    let matched_before: Vec<bool> = settings
        .root_assignments
        .iter()
        .map(|assignment| {
            assignment
                .activation_profile_revision
                .as_ref()
                .is_some_and(|pinned| {
                    pin_profile(settings, assignment)
                        .is_ok_and(|effective| profile_revision(&effective) == *pinned)
                })
        })
        .collect();
    let mut changed = false;
    for profile in &mut settings.profiles {
        if complete_profile(profile) {
            profile.revision = profile_revision(profile);
            changed = true;
        }
    }
    if !changed {
        return false;
    }
    let repinned: Vec<Option<String>> = settings
        .root_assignments
        .iter()
        .zip(&matched_before)
        .map(|(assignment, matched)| {
            matched
                .then(|| pin_profile(settings, assignment).ok())
                .flatten()
                .map(|effective| profile_revision(&effective))
        })
        .collect();
    for (assignment, revision) in settings.root_assignments.iter_mut().zip(repinned) {
        if revision.is_some() {
            assignment.activation_profile_revision = revision;
        }
    }
    true
}

/// The fields every profile lists once each field has its own entry,
/// frozen as they were then. A profile missing one of them was saved
/// before that; one missing only a field added later was not.
const FIELDS_WITH_OWN_ENTRIES: [&str; 50] = [
    "title",
    "title_sort",
    "album",
    "album_sort",
    "artist",
    "artists",
    "artist_sort",
    "album_artist",
    "album_artists",
    "album_artist_sort",
    "track_number",
    "total_tracks",
    "disc_number",
    "total_discs",
    "disc_subtitle",
    "date",
    "original_date",
    "release_status",
    "release_country",
    "release_type",
    "compilation",
    "asin",
    "media",
    "label",
    "catalog_number",
    "barcode",
    "isrc",
    "musicbrainz_recording_id",
    "musicbrainz_release_track_id",
    "musicbrainz_release_id",
    "musicbrainz_release_group_id",
    "musicbrainz_artist_id",
    "musicbrainz_album_artist_id",
    "work",
    "musicbrainz_work_id",
    "movement",
    "movement_number",
    "movement_count",
    "composer",
    "lyricist",
    "conductor",
    "performer",
    "arranger",
    "remixer",
    "producer",
    "comment",
    "copyright",
    "encoded_by",
    "acoustid_id",
    "acoustid_fingerprint",
];

/// Complete one profile's field list and preserved fields:
///
/// - alias spellings fold onto their field, with the strongest mode of
///   the entries (clear when any replace entry cleared);
/// - `genre` and `publisher` entries, which never wrote anything, go;
/// - a field once written under another entry takes that entry's mode;
/// - a missing "Other roles" field (engineer, mixer and the rest) is
///   added as replace when the profile's Relationships write Other, so
///   ticking Other keeps meaning what it says;
/// - every other missing field is added as off;
/// - a preserved parent also preserves the fields that just took its
///   mode, which it used to cover.
fn complete_profile(profile: &mut LibraryManagementProfile) -> bool {
    let before = (
        profile.metadata.fields.clone(),
        profile.metadata.preserve_fields.clone(),
    );
    // A profile saved before every field had its own entry. Its clear on
    // a field with no source never removed anything, and its merge on
    // comment wrote nothing; neither starts doing so now. Fields added
    // since then never make a profile read as that shape.
    let stored_shape = FIELDS_WITH_OWN_ENTRIES.iter().any(|name| {
        !profile
            .metadata
            .fields
            .iter()
            .any(|field| field.field == *name)
    });
    let other_roles = crate::management::enrich::other_roles(profile);
    let mut fields: Vec<ManagedField> = Vec::new();
    for entry in std::mem::take(&mut profile.metadata.fields) {
        if matches!(entry.field.as_str(), "genre" | "publisher") {
            tracing::info!(
                profile = %profile.name,
                field = %entry.field,
                "dropped a managed field entry that never wrote anything"
            );
            continue;
        }
        let name = canonical_field(&entry.field);
        let clears = entry.mode == FieldMode::Replace && entry.clear_when_canonical_missing;
        match fields.iter_mut().find(|field| field.field == name) {
            Some(folded) => {
                if strength(entry.mode) > strength(folded.mode) {
                    folded.mode = entry.mode;
                }
                folded.clear_when_canonical_missing |= clears;
            }
            None => fields.push(ManagedField {
                field: name.to_owned(),
                mode: entry.mode,
                clear_when_canonical_missing: clears,
            }),
        }
    }
    for field in &mut fields {
        let Some(spec) = managed_field(&field.field) else {
            continue;
        };
        if stored_shape && field.mode == FieldMode::Merge && !spec.mergeable {
            field.mode = FieldMode::FillMissing;
        }
        if field.mode != FieldMode::Replace
            || !spec.clearable
            || (stored_shape && spec.source.is_none())
        {
            field.clear_when_canonical_missing = false;
        }
    }
    let mut inherited: Vec<&str> = Vec::new();
    for (name, parent) in FORMER_PARENTS {
        if fields.iter().any(|field| field.field == *name) {
            continue;
        }
        inherited.push(name);
        let (mode, clear) = fields
            .iter()
            .find(|field| field.field == *parent)
            .map_or((FieldMode::Disabled, false), |field| {
                (field.mode, field.clear_when_canonical_missing)
            });
        fields.push(ManagedField {
            field: (*name).to_owned(),
            mode,
            clear_when_canonical_missing: clear,
        });
    }
    for spec in MANAGED_FIELDS {
        if !fields.iter().any(|field| field.field == spec.name) {
            let writes = other_roles && crate::management::enrich::other_role_field(spec.name);
            fields.push(ManagedField {
                field: spec.name.to_owned(),
                mode: if writes {
                    FieldMode::Replace
                } else {
                    FieldMode::Disabled
                },
                clear_when_canonical_missing: false,
            });
        }
    }

    let mut preserved: Vec<String> = Vec::new();
    for name in std::mem::take(&mut profile.metadata.preserve_fields) {
        let name = canonical_field(&name).to_owned();
        let children = FORMER_PARENTS
            .iter()
            .filter(|(child, parent)| *parent == name && inherited.contains(child))
            .map(|(child, _)| (*child).to_owned());
        for value in std::iter::once(name.clone()).chain(children) {
            if !preserved.contains(&value) {
                preserved.push(value);
            }
        }
    }
    profile.metadata.fields = fields;
    profile.metadata.preserve_fields = preserved;
    (profile.metadata.fields != before.0) || (profile.metadata.preserve_fields != before.1)
}

/// Which scripts a normalize compiles.
enum Compile<'a> {
    All,
    None,
    /// Only scripts that are new or whose source differs from these.
    ChangedFrom(&'a LibraryManagement),
}

impl Compile<'_> {
    fn naming(&self, script: &NamingScript) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::ChangedFrom(current) => !current
                .naming_scripts
                .iter()
                .any(|stored| stored.id == script.id && stored.source == script.source),
        }
    }

    fn tagging(&self, script: &TaggingScript) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::ChangedFrom(current) => !current
                .tagging_scripts
                .iter()
                .any(|stored| stored.id == script.id && stored.source == script.source),
        }
    }
}

/// Normalize, validate, and populate deterministic derived revisions, for
/// an import: every script is compiled. Errors are caller faults (400)
/// with the v2 messages.
pub fn normalize(settings: &mut LibraryManagement) -> Result<(), SettingsError> {
    normalize_with(settings, Compile::All)
}

/// Normalize settings read back from the store. Scripts are not compiled:
/// a stored script the compiler refuses is kept, and fails per item at
/// preview rather than failing the whole read.
pub(super) fn normalize_stored(settings: &mut LibraryManagement) -> Result<(), SettingsError> {
    normalize_with(settings, Compile::None)
}

/// Normalize a save against the current settings. Only new and edited
/// scripts are compiled: one stored earlier that the compiler refuses
/// does not block an unrelated save, and keeps failing per item.
pub(super) fn normalize_changes(
    settings: &mut LibraryManagement,
    current: &LibraryManagement,
) -> Result<(), SettingsError> {
    normalize_with(settings, Compile::ChangedFrom(current))
}

fn normalize_with(
    settings: &mut LibraryManagement,
    compile: Compile<'_>,
) -> Result<(), SettingsError> {
    complete_fields(settings);
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
        if compile.naming(script) {
            validate_naming_language(&script.source, &script.name)?;
        }
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
        if compile.tagging(script) {
            validate_tagging_language(&script.source, &script.name)?;
        }
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
            if overrides.multi_disc_naming_mode == MultiDiscNamingMode::Script {
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
    profile: &mut LibraryManagementProfile,
    script_ids: &BTreeSet<String>,
    tagging_ids: &BTreeSet<String>,
) -> Result<(), SettingsError> {
    let mut field_names = BTreeSet::new();
    for field in &mut profile.metadata.fields {
        if field.mode == FieldMode::Preserve {
            field.mode = FieldMode::Disabled;
        }
        if field.mode != FieldMode::Replace {
            field.clear_when_canonical_missing = false;
        }
        let Some(spec) = managed_field(&field.field) else {
            return Err(invalid(&format!("Unknown managed field: {}", field.field)));
        };
        if !field_names.insert(field.field.clone()) {
            return Err(invalid(&format!(
                "Managed field is configured twice: {}",
                field.field
            )));
        }
        if field.mode == FieldMode::Merge && !spec.mergeable {
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
    if profile.metadata.artist_credits.standardization == ArtistStandardization::Variations {
        profile.metadata.artist_credits.standardization = ArtistStandardization::Credited;
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
    if profile.metadata.format_compatibility.id3_version == Id3Version::V23
        && profile.metadata.format_compatibility.id3_text_encoding == Id3TextEncoding::Utf8
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
        .retain(|provider| *provider != ArtworkProvider::Audiodb);
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

/// Built-in presets keep their identity: a stored preset profile cannot
/// be deleted or change its origin, and a new profile cannot claim one.
pub fn validate_preset_provenance(
    current: &LibraryManagement,
    proposed: &LibraryManagement,
) -> Result<(), SettingsError> {
    let current_by_id: BTreeMap<&str, &LibraryManagementProfile> = current
        .profiles
        .iter()
        .map(|profile| (profile.id.as_str(), profile))
        .collect();
    let proposed_by_id: BTreeMap<&str, &LibraryManagementProfile> = proposed
        .profiles
        .iter()
        .map(|profile| (profile.id.as_str(), profile))
        .collect();
    for profile in &current.profiles {
        match proposed_by_id.get(profile.id.as_str()) {
            None if profile.preset_origin.is_some() => {
                return Err(invalid(
                    "Built-in Library Management presets cannot be deleted.",
                ));
            }
            Some(candidate) if candidate.preset_origin != profile.preset_origin => {
                return Err(invalid(
                    "Library Management preset identity cannot be changed.",
                ));
            }
            _ => {}
        }
    }
    if proposed.profiles.iter().any(|profile| {
        !current_by_id.contains_key(profile.id.as_str()) && profile.preset_origin.is_some()
    }) {
        return Err(invalid(
            "Library Management preset identity cannot be assigned to a custom profile.",
        ));
    }
    Ok(())
}
