//! Built-in presets: the catalog constants, the Picard-style and
//! complete organizer profiles, the fresh-tenant settings, preset
//! migration on load, and the per-group preset diff.

use super::*;

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

/// Picard-style organizer preset profile.
pub fn picard_style_organizer_profile() -> LibraryManagementProfile {
    let mut fields: Vec<ManagedField> = MANAGED_FIELD_NAMES
        .iter()
        .filter(|field| **field != "acoustid_id" && **field != "acoustid_fingerprint")
        .map(|field| ManagedField {
            field: (*field).to_owned(),
            mode: FieldMode::Replace,
            clear_when_canonical_missing: false,
        })
        .collect();
    fields.push(ManagedField {
        field: "acoustid_id".to_owned(),
        mode: FieldMode::FillMissing,
        clear_when_canonical_missing: false,
    });
    fields.push(ManagedField {
        field: "acoustid_fingerprint".to_owned(),
        mode: FieldMode::FillMissing,
        clear_when_canonical_missing: false,
    });
    let mut profile = LibraryManagementProfile {
        id: PICARD_ORGANIZER_PROFILE_ID.to_owned(),
        name: "Picard-style Organizer".to_owned(),
        description: "Canonical MusicBrainz tags and artwork with same-root organization, sidecars, and custom-tag preservation.".to_owned(),
        preset_origin: Some("picard_style_organizer".to_owned()),
        preset_version: Some(PICARD_ORGANIZER_PRESET_VERSION),
        metadata: MetadataManagementSettings {
            fields,
            ..MetadataManagementSettings::default()
        },
        organization: OrganizationManagementSettings {
            naming_script_id: PICARD_NAMING_SCRIPT_ID.to_owned(),
            multi_disc_naming_script_id: Some(PICARD_MULTI_DISC_NAMING_SCRIPT_ID.to_owned()),
            ..OrganizationManagementSettings::default()
        },
        ..LibraryManagementProfile::default()
    };
    profile.revision = profile_revision(&profile);
    profile
}

/// Complete library organizer preset profile (the Picard preset with
/// wider genres, artwork, lyrics, and ReplayGain).
pub fn complete_library_organizer_profile() -> LibraryManagementProfile {
    let mut profile = picard_style_organizer_profile();
    profile.id = COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID.to_owned();
    profile.name = "Complete Library Organizer".to_owned();
    profile.description =
        "Best-effort metadata, genres, artwork, lyrics, ReplayGain, and same-root organization."
            .to_owned();
    profile.preset_origin = Some("complete_library_organizer".to_owned());
    profile.preset_version = Some(1);
    profile.genres.sources = vec![
        GenreSource::Musicbrainz,
        GenreSource::Listenbrainz,
        GenreSource::Lastfm,
    ];
    profile.artwork.image_types = vec![
        ArtworkImageType::Front,
        ArtworkImageType::Back,
        ArtworkImageType::Booklet,
        ArtworkImageType::Medium,
        ArtworkImageType::Tray,
        ArtworkImageType::Obi,
        ArtworkImageType::Spine,
        ArtworkImageType::Track,
        ArtworkImageType::Other,
    ];
    profile.artwork.local_file_patterns = ["*.jpg", "*.jpeg", "*.png", "*.webp", "*.gif", "*.pdf"]
        .iter()
        .map(ToString::to_string)
        .collect();
    profile.artwork.external_front_only = false;
    profile.enrichment.lyrics.enabled = true;
    profile.enrichment.replaygain.enabled = true;
    profile.enrichment.replaygain.mode = ReplayGainMode::Replace;
    profile.revision = profile_revision(&profile);
    profile
}

/// Preset profile for a tracked origin, if known.
pub fn preset_profile_for_origin(origin: &str) -> Option<LibraryManagementProfile> {
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
pub fn current_picard_preset_scripts(settings: &LibraryManagement) -> (NamingScript, NamingScript) {
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
        let mut script = NamingScript {
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

/// Fresh-tenant initial settings (v2 `build_initial_library_management_settings`):
/// the Picard-style (default) and complete organizer presets with their
/// two naming scripts, plus a path-only profile seeded from the naming
/// template configured before Library Management existed. Nothing is
/// assigned to a root, so seeding never starts work.
pub fn initial_settings(legacy_naming_template: &str) -> LibraryManagement {
    let mut settings = LibraryManagement {
        schema_version: MANAGEMENT_SCHEMA_VERSION,
        preset_catalog_version: PRESET_CATALOG_VERSION,
        ..LibraryManagement::default()
    };
    let (standard, multi_disc) = current_picard_preset_scripts(&settings);
    let legacy_source = if legacy_naming_template.trim().is_empty() {
        DEFAULT_NAMING_TEMPLATE
    } else {
        legacy_naming_template
    };
    let mut legacy_script = NamingScript {
        id: LEGACY_NAMING_SCRIPT_ID.to_owned(),
        name: "Existing DroppedNeedle naming".to_owned(),
        source: legacy_source.to_owned(),
        revision: String::new(),
        preset_origin: Some("legacy_naming_template".to_owned()),
        preset_version: Some(1),
    };
    legacy_script.revision = naming_script_revision(&legacy_script);
    settings.naming_scripts = vec![standard, multi_disc, legacy_script];
    let picard = picard_style_organizer_profile();
    settings.default_profile_id = picard.id.clone();
    settings.profiles = vec![
        picard,
        complete_library_organizer_profile(),
        legacy_naming_profile(),
    ];
    settings
}

/// The path-only profile seeded from the pre-Library-Management naming
/// template: tags, genres and artwork off, sources kept in place.
fn legacy_naming_profile() -> LibraryManagementProfile {
    let mut profile = LibraryManagementProfile {
        id: LEGACY_NAMING_PROFILE_ID.to_owned(),
        name: "Existing naming template".to_owned(),
        description: "Path-only seed copied from the naming template that was configured \
                      before Library Management."
            .to_owned(),
        preset_origin: Some("legacy_naming_template".to_owned()),
        preset_version: Some(1),
        ..LibraryManagementProfile::default()
    };
    profile.metadata.enabled = false;
    profile.genres.enabled = false;
    profile.artwork.embedded_enabled = false;
    profile.artwork.external_enabled = false;
    profile.organization.naming_script_id = LEGACY_NAMING_SCRIPT_ID.to_owned();
    profile.organization.move_sidecars = false;
    profile.organization.sidecar_patterns = Vec::new();
    profile.organization.source_cleanup = SourceCleanupMode::Keep;
    profile.organization.remove_empty_directories = false;
    profile.revision = profile_revision(&profile);
    profile
}

/// Sidecar-pattern history: legacy (`cover`+cue/log/lrc/m3u/pls),
/// current 27-pattern default, and the skipped-aquarium lineage.
pub fn sidecar_default_history() -> Vec<Vec<String>> {
    let legacy: Vec<String> = LEGACY_DEFAULT_SIDECAR_PATTERNS
        .iter()
        .map(ToString::to_string)
        .collect();
    let current: Vec<String> = OrganizationManagementSettings::default().sidecar_patterns;
    vec![legacy, current]
}

/// Migrate stored settings to the current preset catalog: refresh
/// unedited preset profiles/scripts, bump the catalog version, and heal
/// legacy sidecar patterns. Runs on read (persisted when it changes
/// anything) and before every write.
pub fn migrate_presets(settings: &mut LibraryManagement) {
    if settings.preset_catalog_version >= PRESET_CATALOG_VERSION {
        return;
    }
    let history = sidecar_default_history();
    for profile in &mut settings.profiles {
        if history.contains(&profile.organization.sidecar_patterns) {
            profile.organization.sidecar_patterns =
                OrganizationManagementSettings::default().sidecar_patterns;
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

/// Group-level preset drift: which top-level profile groups differ from
/// the tracked preset, plus the preset profile for rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = LibraryManagementPresetDiff)]
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
    pub preset_profile: Option<LibraryManagementProfile>,
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
pub fn preset_diff(profile: &LibraryManagementProfile) -> PresetDiff {
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
