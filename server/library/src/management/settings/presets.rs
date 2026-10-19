//! Built-in presets: the catalog constants, the Picard-style and
//! complete organizer profiles, the fresh-tenant settings, preset
//! migration on load, and the per-group preset diff.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::revision::{naming_script_revision, profile_revision};
use droppedneedle_kernel::runtime_config::secret_sections::DEFAULT_NAMING_SCRIPT;
use droppedneedle_kernel::runtime_config::sections::{
    ArtworkImageType, DEFAULT_NAMING_TEMPLATE, FieldMode, GenreSource, LibraryManagement,
    LibraryManagementProfile, ManagedField, MetadataManagementSettings, NamingScript,
    OrganizationManagementSettings, RelationshipCreditSettings, RelationshipType, ReplayGainMode,
    SourceCleanupMode,
};

/// Management settings schema version.
pub const MANAGEMENT_SCHEMA_VERSION: i64 = 1;
/// Preset catalog version.
pub const PRESET_CATALOG_VERSION: i64 = 1;
/// Picard organizer preset version.
pub const PICARD_ORGANIZER_PRESET_VERSION: i64 = 6;
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

/// One tag field a profile manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedFieldSpec {
    /// The profile's name for it.
    pub name: &'static str,
    /// What the profile editor calls it.
    pub label: &'static str,
    /// What MusicBrainz fact the value comes from; `None` when nothing
    /// gives one, so the file keeps its value unless a tagging script sets
    /// it or replace with clear removes it.
    pub source: Option<&'static str>,
    /// Whether merge is offered (multi-valued fields).
    pub mergeable: bool,
    /// Whether replace can clear the file's value when the source has
    /// none.
    pub clearable: bool,
}

const fn field(
    name: &'static str,
    label: &'static str,
    source: Option<&'static str>,
    mergeable: bool,
) -> ManagedFieldSpec {
    ManagedFieldSpec {
        name,
        label,
        source,
        mergeable,
        clearable: true,
    }
}

/// A field worked out from the file's audio (AcoustID). A value that
/// could not be worked out says nothing about the file, so clear and
/// merge are not offered.
const fn from_audio(
    name: &'static str,
    label: &'static str,
    source: &'static str,
) -> ManagedFieldSpec {
    ManagedFieldSpec {
        name,
        label,
        source: Some(source),
        mergeable: false,
        clearable: false,
    }
}

/// Every tag field a profile manages, in the editor's order, under v2's
/// names. Mergeable is v2's list plus the multi-valued fields added since.
pub const MANAGED_FIELDS: &[ManagedFieldSpec] = &[
    field("title", "Title", Some("Track title"), false),
    field("title_sort", "Title sort", None, false),
    field("album", "Album", Some("Release title"), false),
    field("album_sort", "Album sort", None, false),
    field("artist", "Artist", Some("Track artist credit"), true),
    field("artists", "Artists", Some("Track artist names"), true),
    field(
        "artist_sort",
        "Artist sort",
        Some("Track artist sort names"),
        true,
    ),
    field(
        "album_artist",
        "Album artist",
        Some("Release artist credit"),
        true,
    ),
    field(
        "album_artists",
        "Album artists",
        Some("Release artist names"),
        true,
    ),
    field(
        "album_artist_sort",
        "Album artist sort",
        Some("Release artist sort names"),
        true,
    ),
    field(
        "track_number",
        "Track number",
        Some("Track position"),
        false,
    ),
    field(
        "total_tracks",
        "Total tracks",
        Some("Tracks on the disc"),
        false,
    ),
    field("disc_number", "Disc number", Some("Disc position"), false),
    field(
        "total_discs",
        "Total discs",
        Some("Discs in the release"),
        false,
    ),
    field("disc_subtitle", "Disc subtitle", Some("Disc title"), false),
    field("date", "Date", Some("Release date"), false),
    field(
        "original_date",
        "Original date",
        Some("Release group's first release date"),
        false,
    ),
    field(
        "release_status",
        "Release status",
        Some("Release status"),
        false,
    ),
    field(
        "release_country",
        "Release country",
        Some("Release country"),
        false,
    ),
    field(
        "release_type",
        "Release type",
        Some("Release group types"),
        true,
    ),
    field(
        "compilation",
        "Compilation",
        Some("Release artist is Various Artists"),
        false,
    ),
    field("asin", "ASIN", Some("Release ASIN"), false),
    field("media", "Media", Some("Disc format"), false),
    field("label", "Label", Some("Release labels"), true),
    field(
        "catalog_number",
        "Catalog number",
        Some("Release catalog numbers"),
        true,
    ),
    field("barcode", "Barcode", Some("Release barcode"), false),
    field("isrc", "ISRC", Some("Recording ISRCs"), true),
    field(
        "musicbrainz_recording_id",
        "MusicBrainz recording ID",
        Some("Recording ID"),
        false,
    ),
    field(
        "musicbrainz_release_track_id",
        "MusicBrainz release track ID",
        Some("Release track ID"),
        false,
    ),
    field(
        "musicbrainz_release_id",
        "MusicBrainz release ID",
        Some("Release ID"),
        false,
    ),
    field(
        "musicbrainz_release_group_id",
        "MusicBrainz release group ID",
        Some("Release group ID"),
        false,
    ),
    field(
        "musicbrainz_artist_id",
        "MusicBrainz artist ID",
        Some("Track artist IDs"),
        true,
    ),
    field(
        "musicbrainz_album_artist_id",
        "MusicBrainz album artist ID",
        Some("Release artist IDs"),
        true,
    ),
    field("work", "Work", Some("Performed work's title"), false),
    field(
        "musicbrainz_work_id",
        "MusicBrainz work ID",
        Some("Performed work IDs"),
        true,
    ),
    field("movement", "Movement", None, false),
    field("movement_number", "Movement number", None, false),
    field("movement_count", "Movement count", None, false),
    field(
        "composer",
        "Composer",
        Some("Recording and work credits"),
        true,
    ),
    field(
        "lyricist",
        "Lyricist",
        Some("Recording and work credits"),
        true,
    ),
    field(
        "conductor",
        "Conductor",
        Some("Recording and work credits"),
        true,
    ),
    field(
        "performer",
        "Performer",
        Some("Recording and work credits"),
        true,
    ),
    field(
        "arranger",
        "Arranger",
        Some("Recording and work credits"),
        true,
    ),
    field(
        "remixer",
        "Remixer",
        Some("Recording and work credits"),
        true,
    ),
    field(
        "producer",
        "Producer",
        Some("Recording and work credits"),
        true,
    ),
    field(
        "engineer",
        "Engineer",
        Some("Recording and work credits"),
        true,
    ),
    field("mixer", "Mixer", Some("Recording and work credits"), true),
    field(
        "dj_mixer",
        "DJ-mixer",
        Some("Recording and work credits"),
        true,
    ),
    field("writer", "Writer", Some("Recording and work credits"), true),
    field(
        "director",
        "Director",
        Some("Recording and work credits"),
        true,
    ),
    field("comment", "Comment", None, false),
    field("copyright", "Copyright", None, false),
    field("encoded_by", "Encoded by", None, false),
    from_audio(
        "acoustid_id",
        "AcoustID",
        "AcoustID lookup of the file's audio",
    ),
    from_audio(
        "acoustid_fingerprint",
        "AcoustID fingerprint",
        "The file's audio (Chromaprint)",
    ),
];

/// The managed field by its profile name.
pub fn managed_field(name: &str) -> Option<&'static ManagedFieldSpec> {
    MANAGED_FIELDS.iter().find(|field| field.name == name)
}

/// Older spellings tagging scripts still accept beside the field names:
/// `albumartist`, `albumartists`, `track`, `disc` and `year` name their
/// field, `publisher` is the label's frame, and genre is written by the
/// Genres section.
pub const SCRIPT_FIELD_SPELLINGS: &[&str] = &[
    "albumartist",
    "albumartists",
    "track",
    "disc",
    "year",
    "publisher",
    "genre",
];

/// Older spellings tagging scripts may still append to.
pub const SCRIPT_APPENDABLE_SPELLINGS: &[&str] =
    &["albumartist", "albumartists", "comment", "genre"];

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
    // Every field, replace; AcoustID fills only what is missing.
    let fields: Vec<ManagedField> = MANAGED_FIELDS
        .iter()
        .map(|field| ManagedField {
            field: field.name.to_owned(),
            mode: if field.name.starts_with("acoustid_") {
                FieldMode::FillMissing
            } else {
                FieldMode::Replace
            },
            clear_when_canonical_missing: false,
        })
        .collect();
    let mut profile = LibraryManagementProfile {
        id: PICARD_ORGANIZER_PROFILE_ID.to_owned(),
        name: "Picard-style Organizer".to_owned(),
        description: "Canonical MusicBrainz tags and artwork with same-root organization, sidecars, and custom-tag preservation.".to_owned(),
        preset_origin: Some("picard_style_organizer".to_owned()),
        preset_version: Some(PICARD_ORGANIZER_PRESET_VERSION),
        metadata: MetadataManagementSettings {
            fields,
            relationships: RelationshipCreditSettings {
                types: RelationshipCreditSettings::default()
                    .types
                    .into_iter()
                    .chain([RelationshipType::Other])
                    .collect(),
                ..RelationshipCreditSettings::default()
            },
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
    profile.preset_version = Some(3);
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
/// two naming scripts, plus a path-only profile seeded from the library's
/// import naming script. Nothing is assigned to a root, so seeding never
/// starts work.
pub fn initial_settings(import_naming_script: &str) -> LibraryManagement {
    let mut settings = LibraryManagement {
        schema_version: MANAGEMENT_SCHEMA_VERSION,
        preset_catalog_version: PRESET_CATALOG_VERSION,
        ..LibraryManagement::default()
    };
    let (standard, multi_disc) = current_picard_preset_scripts(&settings);
    let legacy_source = if import_naming_script.trim().is_empty() {
        DEFAULT_NAMING_SCRIPT
    } else {
        import_naming_script
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
    profile.metadata.fields = MANAGED_FIELDS
        .iter()
        .map(|field| ManagedField {
            field: field.name.to_owned(),
            mode: FieldMode::Disabled,
            clear_when_canonical_missing: false,
        })
        .collect();
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

/// A pre-script naming template rewritten as a naming script, with what
/// the rewrite could not keep exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyConversion {
    /// The naming script.
    pub script: String,
    /// One plain sentence per thing to look at.
    pub warnings: Vec<String>,
}

/// Names the template knew whose value can be empty for an imported
/// file, so a folder made only of them can render empty.
const OFTEN_EMPTY_NAMES: &[&str] = &["year", "musicbrainz_id", "artist_mbid"];

/// The script value a template's `{name}` became; `None` for a name that
/// always rendered empty for an import.
fn legacy_value(name: &str) -> Option<&'static str> {
    Some(match name {
        "artist" => "artist",
        "album" => "album",
        "albumartist" => "albumartist",
        "initial" => "initial",
        "title" => "title",
        "ext" => "ext",
        "track" => "track",
        "disc" => "disc",
        "musicbrainz_id" => "musicbrainz_id",
        // The import read the original release year first.
        "year" => "slice(default(original_date, date), 0, 4)",
        // The release's first credited artist.
        "artist_mbid" => "first(musicbrainz_album_artist_id)",
        _ => return None,
    })
}

/// A string literal for a naming expression. Braces are escaped: a raw
/// `}` would end the expression.
fn script_string(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\u007b"),
            '}' => out.push_str("\\u007d"),
            other if other.is_control() => out.push('_'),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// One part of a converted folder or file name.
#[derive(Debug, Clone)]
enum Piece {
    /// Literal path text.
    Text(String),
    /// A `{...}` hole, as script source.
    Hole {
        source: String,
        /// The hole is a plain `{ext}`.
        ext: bool,
        /// The hole can render empty on an import.
        often_empty: bool,
        /// The template text it came from.
        label: String,
    },
}

fn pieces_source(pieces: &[Piece]) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(text) => {
                for ch in text.chars() {
                    match ch {
                        '{' | '}' => {
                            out.push('{');
                            out.push_str(&script_string(&ch.to_string()));
                            out.push('}');
                        }
                        other if other.is_control() => out.push('_'),
                        other => out.push(other),
                    }
                }
            }
            Piece::Hole { source, .. } => out.push_str(source),
        }
    }
    out
}

/// Rewrite a naming template from before naming scripts (v2's, and the
/// first v3 `naming_template`) as a naming script that places an imported
/// file where the template did. A blank template is v2's default.
///
/// The template's rules carry over: `{name}` and `{track:02d}` holes
/// (names it did not know render nothing), a bracket pair around an empty
/// value disappearing with it, empty folders dropped, a `.` or `..`
/// folder becoming `_`, and the file always ending in `.{ext}`. A folder
/// that renders empty at run time is the one thing a script refuses, so
/// each folder that can is named in the warnings.
pub fn legacy_template_to_script(template: &str) -> LegacyConversion {
    let template = if template.trim().is_empty() {
        DEFAULT_NAMING_TEMPLATE
    } else {
        template
    };
    let mut warnings = Vec::new();
    let segments = template_segments(template);
    let mut keep: Vec<(usize, usize)> = segments
        .iter()
        .map(|(part, _)| (0, part.chars().count()))
        .collect();
    let mut holes: Vec<Option<Piece>> = vec![None; segments.len()];
    for (index, (part, is_token)) in segments.iter().enumerate() {
        if !is_token {
            continue;
        }
        let inner = &part[1..part.len() - 1];
        let (name, format) = inner.split_once(':').unwrap_or((inner, ""));
        let value = legacy_value(name);
        if value.is_none() {
            push_once(
                &mut warnings,
                if name == "genre" {
                    "{genre} was always empty for imports, so it is left out.".to_owned()
                } else {
                    format!("{{{name}}} is not a name the template knew, so it is left out.")
                },
            );
        }
        let numeric = matches!(name, "track" | "disc");
        let never_empty = numeric || matches!(name, "ext" | "initial");
        let group = if never_empty {
            None
        } else {
            template_empty_group(&segments, index)
        };
        let source = match (value, group) {
            (None, _) => None,
            (Some(value), None) if numeric && !format.is_empty() => {
                Some(match number_spec(format, &mut warnings) {
                    Some(spec) => format!("{{{value}:{spec}}}"),
                    None => format!("{{{value}}}"),
                })
            }
            (Some(value), None) => Some(format!("{{{value}}}")),
            (Some(value), Some((before_start, after_end))) => {
                let before: String = segments[index - 1].0.chars().skip(before_start).collect();
                let after: String = segments[index + 1].0.chars().take(after_end).collect();
                Some(format!(
                    "{{conditional(is_empty({value}), \"\", concat({}, {value}, {}))}}",
                    script_string(&before),
                    script_string(&after),
                ))
            }
        };
        if let Some((before_start, after_end)) = group {
            keep[index - 1].1 = keep[index - 1].1.min(before_start);
            keep[index + 1].0 = keep[index + 1].0.max(after_end);
        }
        holes[index] = source.map(|source| Piece::Hole {
            source,
            ext: name == "ext" && format.is_empty(),
            often_empty: OFTEN_EMPTY_NAMES.contains(&name),
            label: part.clone(),
        });
    }

    // Folders, split on the template's own slashes; empty ones go.
    let mut components: Vec<Vec<Piece>> = vec![Vec::new()];
    for (index, (part, is_token)) in segments.iter().enumerate() {
        if *is_token {
            if let (Some(hole), Some(current)) = (holes[index].take(), components.last_mut()) {
                current.push(hole);
            }
            continue;
        }
        let (start, end) = keep[index];
        let text: String = part
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        for (n, chunk) in text.split('/').enumerate() {
            if n > 0 {
                components.push(Vec::new());
            }
            if !chunk.is_empty()
                && let Some(current) = components.last_mut()
            {
                current.push(Piece::Text(chunk.to_owned()));
            }
        }
    }
    components.retain(|pieces| !pieces.is_empty());
    for pieces in &mut components {
        let literal_only = pieces.iter().all(|piece| matches!(piece, Piece::Text(_)));
        if literal_only && pieces_source(pieces).trim_matches([' ', '.']).is_empty() {
            *pieces = vec![Piece::Text("_".to_owned())];
        }
    }
    for pieces in &components {
        let may_vanish = pieces.iter().all(|piece| match piece {
            Piece::Text(text) => text.trim().is_empty(),
            Piece::Hole { often_empty, .. } => *often_empty,
        });
        if may_vanish {
            let names: Vec<&str> = pieces
                .iter()
                .filter_map(|piece| match piece {
                    Piece::Hole { label, .. } => Some(label.as_str()),
                    Piece::Text(_) => None,
                })
                .collect();
            warnings.push(format!(
                "A folder made only of {} is empty for a release without it, and such an import stops with an error.",
                names.join(" and ")
            ));
        }
    }

    // The file always ends in `.{ext}`.
    if components.is_empty() {
        components.push(Vec::new());
    }
    if let Some(last) = components.last_mut() {
        if let Some(Piece::Text(text)) = last.last_mut() {
            let trimmed = text.trim_end_matches([' ', '.']).len();
            text.truncate(trimmed);
            if text.is_empty() {
                last.pop();
            }
        }
        let ends_with_ext = matches!(
            last.as_slice(),
            [.., Piece::Text(text), Piece::Hole { ext: true, .. }] if text.ends_with('.')
        );
        if !ends_with_ext {
            let literal_ext = match last.last() {
                Some(Piece::Text(text)) => text
                    .rsplit_once('.')
                    .map(|(_, ext)| ext.to_lowercase())
                    .filter(|ext| !ext.is_empty()),
                _ => None,
            };
            if last.is_empty() {
                last.push(Piece::Text("_".to_owned()));
            }
            let source = match literal_ext {
                Some(ext) => format!(
                    "{{conditional(equals(ext, {}), \"\", concat(\".\", ext))}}",
                    script_string(&ext)
                ),
                None => ".{ext}".to_owned(),
            };
            last.push(Piece::Hole {
                source,
                ext: false,
                often_empty: false,
                label: String::new(),
            });
        }
    }
    let script = components
        .iter()
        .map(|pieces| pieces_source(pieces))
        .collect::<Vec<_>>()
        .join("/");
    LegacyConversion { script, warnings }
}

fn push_once(warnings: &mut Vec<String>, warning: String) {
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

/// The template's `02d`-style number format as a script writes it;
/// `None` for one that rendered the plain number.
fn number_spec(format: &str, warnings: &mut Vec<String>) -> Option<String> {
    let spec = format.strip_suffix('d').unwrap_or(format);
    let mut width = spec.trim_start_matches('0').parse::<usize>().unwrap_or(0);
    if width > 64 {
        push_once(
            warnings,
            format!("A number width of {width} is cut to 64, the widest a script allows."),
        );
        width = 64;
    }
    if width == 0 {
        return None;
    }
    Some(if spec.starts_with('0') {
        format!("0{width}d")
    } else {
        format!("{width}d")
    })
}

/// The template's literal and `{name}` segments, read as the template
/// engine read them: braces holding nothing stay literal.
fn template_segments(template: &str) -> Vec<(String, bool)> {
    let chars: Vec<char> = template.chars().collect();
    let mut out = Vec::new();
    let mut literal_start = 0;
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '{' {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < chars.len() && chars[end] != '{' && chars[end] != '}' {
            end += 1;
        }
        if end >= chars.len() {
            break;
        }
        if chars[end] == '{' {
            index = end;
            continue;
        }
        if end == index + 1 {
            index = end + 1;
            continue;
        }
        out.push((chars[literal_start..index].iter().collect(), false));
        out.push((chars[index..=end].iter().collect(), true));
        index = end + 1;
        literal_start = index;
    }
    out.push((chars[literal_start..].iter().collect(), false));
    out
}

/// For a value between two literals, the bracket pair around it that the
/// template dropped with an empty value: (start of the opening bracket and
/// the spaces before it, end of the closing bracket), in characters.
/// Nested brackets kept theirs.
fn template_empty_group(segments: &[(String, bool)], token: usize) -> Option<(usize, usize)> {
    if token == 0 || token + 1 >= segments.len() {
        return None;
    }
    let (before, before_token) = &segments[token - 1];
    let (after, after_token) = &segments[token + 1];
    if *before_token || *after_token {
        return None;
    }
    let before: Vec<char> = before.chars().collect();
    let after: Vec<char> = after.chars().collect();
    let opening = before.iter().rposition(|ch| !ch.is_whitespace())?;
    if let Some(outer) = before[..opening].iter().rposition(|ch| !ch.is_whitespace())
        && matches!(before[outer], '(' | '[' | '{')
    {
        return None;
    }
    let closing_char = match before[opening] {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => return None,
    };
    let closing = after.iter().position(|ch| !ch.is_whitespace())?;
    if after[closing] != closing_char {
        return None;
    }
    if let Some(outer) = after[closing + 1..]
        .iter()
        .position(|ch| !ch.is_whitespace())
        && matches!(after[closing + 1 + outer], ')' | ']' | '}')
    {
        return None;
    }
    let mut start = opening;
    while start > 0 && before[start - 1].is_whitespace() {
        start -= 1;
    }
    Some((start, closing + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::naming;
    use crate::management::settings::validate_naming_language;
    use crate::matching::model::{CreditedArtist, ReleaseMedium};
    use crate::matching::{Release, ReleaseTrack};

    fn credit(id: &str, name: &str) -> CreditedArtist {
        CreditedArtist {
            id: id.to_owned(),
            name: name.to_owned(),
            sort_name: None,
            join: String::new(),
        }
    }

    /// The release and track each table input names: Portishead's "Sour
    /// Times" (disc 1, track 3, 1994) with one thing changed.
    fn input(label: &str) -> (Release, ReleaseTrack, &'static str) {
        let mut release = Release {
            release_group_id: "rg-id".to_owned(),
            title: "Dummy".to_owned(),
            artists: vec![credit("aa-id", "Portishead")],
            date: Some("1994-08-22".to_owned()),
            original_date: Some("1994-08-22".to_owned()),
            media: vec![ReleaseMedium {
                position: 1,
                track_count: 11,
                ..ReleaseMedium::default()
            }],
            ..Release::default()
        };
        let mut track = ReleaseTrack {
            title: "Sour Times".to_owned(),
            artists: vec![credit("ta-id", "Portishead")],
            disc: 1,
            position: 3,
            ..ReleaseTrack::default()
        };
        let mut format = "flac";
        match label {
            "no_year" => {
                release.date = None;
                release.original_date = None;
            }
            "original" => {
                release.date = Some("2009-03-01".to_owned());
                release.original_date = Some("1991-10-14".to_owned());
            }
            "unsafe" => track.title = "AC/DC: Live?".to_owned(),
            "con" => release.title = "CON".to_owned(),
            "dots" => release.title = "..".to_owned(),
            "long228" => track.title = "é".repeat(114),
            "disc2" => {
                track.disc = 2;
                track.position = 12;
            }
            "beatles" => {
                release.artists = vec![credit("aa-id", "The Beatles")];
                track.artists = vec![credit("ta-id", "The Beatles")];
            }
            "mp3" => format = "mp3",
            "etc" => track.title = "Etc.".to_owned(),
            _ => {}
        }
        (release, track, format)
    }

    const INPUTS: &[&str] = &[
        "plain", "no_year", "original", "unsafe", "con", "dots", "long228", "disc2", "beatles",
        "mp3", "etc",
    ];

    /// Every template against every input: the path the template engine
    /// gave, except where a comment names a known difference. D1: a folder
    /// that renders empty now stops the import. D2: a value ending in a
    /// dot before an added `.{ext}` keeps its dot, and a file name that is
    /// only dots stops the import. CUT: names are cut at 240 bytes, not 252.
    /// `{LONG}` is a 228-byte title.
    const TABLE: &[(&str, [Option<&str>; 11])] = &[
        (
            "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}",
            [
                Some("Portishead/Dummy (1994)/0103 Sour Times.flac"),
                Some("Portishead/Dummy/0103 Sour Times.flac"),
                Some("Portishead/Dummy (1991)/0103 Sour Times.flac"),
                Some("Portishead/Dummy (1994)/0103 AC_DC_ Live_.flac"),
                Some("Portishead/CON (1994)/0103 Sour Times.flac"),
                Some("Portishead/(1994)/0103 Sour Times.flac"),
                Some("Portishead/Dummy (1994)/0103 {LONG}.flac"),
                Some("Portishead/Dummy (1994)/0212 Sour Times.flac"),
                Some("The Beatles/Dummy (1994)/0103 Sour Times.flac"),
                Some("Portishead/Dummy (1994)/0103 Sour Times.mp3"),
                Some("Portishead/Dummy (1994)/0103 Etc..flac"),
            ],
        ),
        (
            "{albumartist}/{album}/{track} {title}",
            [
                Some("Portishead/Dummy/3 Sour Times.flac"),
                Some("Portishead/Dummy/3 Sour Times.flac"),
                Some("Portishead/Dummy/3 Sour Times.flac"),
                Some("Portishead/Dummy/3 AC_DC_ Live_.flac"),
                Some("Portishead/_CON/3 Sour Times.flac"),
                Some("Portishead/_/3 Sour Times.flac"),
                Some("Portishead/Dummy/3 {LONG}.flac"),
                Some("Portishead/Dummy/12 Sour Times.flac"),
                Some("The Beatles/Dummy/3 Sour Times.flac"),
                Some("Portishead/Dummy/3 Sour Times.mp3"),
                Some("Portishead/Dummy/3 Etc..flac"), // D2
            ],
        ),
        (
            "{initial}/{albumartist}/[{year}] {album}/{disc:02d}-{track:02d} {title}.{ext}",
            [
                Some("P/Portishead/[1994] Dummy/01-03 Sour Times.flac"),
                Some("P/Portishead/Dummy/01-03 Sour Times.flac"),
                Some("P/Portishead/[1991] Dummy/01-03 Sour Times.flac"),
                Some("P/Portishead/[1994] Dummy/01-03 AC_DC_ Live_.flac"),
                Some("P/Portishead/[1994] CON/01-03 Sour Times.flac"),
                Some("P/Portishead/[1994]/01-03 Sour Times.flac"),
                Some("P/Portishead/[1994] Dummy/01-03 {LONG}.flac"),
                Some("P/Portishead/[1994] Dummy/02-12 Sour Times.flac"),
                Some("B/The Beatles/[1994] Dummy/01-03 Sour Times.flac"),
                Some("P/Portishead/[1994] Dummy/01-03 Sour Times.mp3"),
                Some("P/Portishead/[1994] Dummy/01-03 Etc..flac"),
            ],
        ),
        (
            "{artist}/{album}",
            [
                Some("Portishead/Dummy.flac"),
                Some("Portishead/Dummy.flac"),
                Some("Portishead/Dummy.flac"),
                Some("Portishead/Dummy.flac"),
                Some("Portishead/_CON.flac"),
                None, // D2: no name before the extension
                Some("Portishead/Dummy.flac"),
                Some("Portishead/Dummy.flac"),
                Some("The Beatles/Dummy.flac"),
                Some("Portishead/Dummy.mp3"),
                Some("Portishead/Dummy.flac"),
            ],
        ),
        (
            "{genre}/{albumartist}/{album}/{title}.{ext}",
            [
                Some("Portishead/Dummy/Sour Times.flac"),
                Some("Portishead/Dummy/Sour Times.flac"),
                Some("Portishead/Dummy/Sour Times.flac"),
                Some("Portishead/Dummy/AC_DC_ Live_.flac"),
                Some("Portishead/_CON/Sour Times.flac"),
                Some("Portishead/_/Sour Times.flac"),
                Some("Portishead/Dummy/{LONG}.flac"),
                Some("Portishead/Dummy/Sour Times.flac"),
                Some("The Beatles/Dummy/Sour Times.flac"),
                Some("Portishead/Dummy/Sour Times.mp3"),
                Some("Portishead/Dummy/Etc..flac"),
            ],
        ),
        (
            "{musicbrainz_id}/{artist_mbid}/{title}.flac",
            [
                Some("rg-id/aa-id/Sour Times.flac"),
                Some("rg-id/aa-id/Sour Times.flac"),
                Some("rg-id/aa-id/Sour Times.flac"),
                Some("rg-id/aa-id/AC_DC_ Live_.flac"),
                Some("rg-id/aa-id/Sour Times.flac"),
                Some("rg-id/aa-id/Sour Times.flac"),
                Some("rg-id/aa-id/{LONG}.flac"),
                Some("rg-id/aa-id/Sour Times.flac"),
                Some("rg-id/aa-id/Sour Times.flac"),
                Some("rg-id/aa-id/Sour Times.flac.mp3"),
                Some("rg-id/aa-id/Etc..flac"),
            ],
        ),
        (
            "{albumartist}/{album} {} }/{year:04d} {title}",
            [
                Some("Portishead/Dummy {} }/1994 Sour Times.flac"),
                Some("Portishead/Dummy {} }/Sour Times.flac"),
                Some("Portishead/Dummy {} }/1991 Sour Times.flac"),
                Some("Portishead/Dummy {} }/1994 AC_DC_ Live_.flac"),
                Some("Portishead/CON {} }/1994 Sour Times.flac"),
                Some("Portishead/{} }/1994 Sour Times.flac"),
                Some("Portishead/Dummy {} }/1994 {LONG}.flac"),
                Some("Portishead/Dummy {} }/1994 Sour Times.flac"),
                Some("The Beatles/Dummy {} }/1994 Sour Times.flac"),
                Some("Portishead/Dummy {} }/1994 Sour Times.mp3"),
                Some("Portishead/Dummy {} }/1994 Etc..flac"), // D2
            ],
        ),
        (
            "./{albumartist}/{album}/",
            [
                Some("_/Portishead/Dummy.flac"),
                Some("_/Portishead/Dummy.flac"),
                Some("_/Portishead/Dummy.flac"),
                Some("_/Portishead/Dummy.flac"),
                Some("_/Portishead/_CON.flac"),
                None, // D2: no name before the extension
                Some("_/Portishead/Dummy.flac"),
                Some("_/Portishead/Dummy.flac"),
                Some("_/The Beatles/Dummy.flac"),
                Some("_/Portishead/Dummy.mp3"),
                Some("_/Portishead/Dummy.flac"),
            ],
        ),
        (
            "{albumartist}/{{year}}/{title}",
            [
                Some("Portishead/{1994}/Sour Times.flac"),
                None, // D1
                Some("Portishead/{1991}/Sour Times.flac"),
                Some("Portishead/{1994}/AC_DC_ Live_.flac"),
                Some("Portishead/{1994}/Sour Times.flac"),
                Some("Portishead/{1994}/Sour Times.flac"),
                Some("Portishead/{1994}/{LONG}.flac"),
                Some("Portishead/{1994}/Sour Times.flac"),
                Some("The Beatles/{1994}/Sour Times.flac"),
                Some("Portishead/{1994}/Sour Times.mp3"),
                Some("Portishead/{1994}/Etc..flac"), // D2
            ],
        ),
        (
            "{year}/{albumartist}/{title}.{ext}",
            [
                Some("1994/Portishead/Sour Times.flac"),
                None, // D1
                Some("1991/Portishead/Sour Times.flac"),
                Some("1994/Portishead/AC_DC_ Live_.flac"),
                Some("1994/Portishead/Sour Times.flac"),
                Some("1994/Portishead/Sour Times.flac"),
                Some("1994/Portishead/{LONG}.flac"),
                Some("1994/Portishead/Sour Times.flac"),
                Some("1994/The Beatles/Sour Times.flac"),
                Some("1994/Portishead/Sour Times.mp3"),
                Some("1994/Portishead/Etc..flac"),
            ],
        ),
        (
            "{albumartist}/{album} - {date}/{track:2} \"{title}\" \\ x.{ext}",
            [
                Some("Portishead/Dummy -/3 _Sour Times_ _ x.flac"),
                Some("Portishead/Dummy -/3 _Sour Times_ _ x.flac"),
                Some("Portishead/Dummy -/3 _Sour Times_ _ x.flac"),
                Some("Portishead/Dummy -/3 _AC_DC_ Live__ _ x.flac"),
                Some("Portishead/CON -/3 _Sour Times_ _ x.flac"),
                Some("Portishead/-/3 _Sour Times_ _ x.flac"),
                Some("Portishead/Dummy -/3 _{LONG}_ _.flac"), // CUT
                Some("Portishead/Dummy -/12 _Sour Times_ _ x.flac"),
                Some("The Beatles/Dummy -/3 _Sour Times_ _ x.flac"),
                Some("Portishead/Dummy -/3 _Sour Times_ _ x.mp3"),
                Some("Portishead/Dummy -/3 _Etc._ _ x.flac"),
            ],
        ),
    ];

    #[test]
    fn converted_templates_place_files_where_the_template_did() {
        let long = "é".repeat(114);
        for (template, expected) in TABLE {
            let converted = legacy_template_to_script(template);
            validate_naming_language(&converted.script, "converted").expect(template);
            for (label, expected) in INPUTS.iter().zip(expected) {
                let (release, track, format) = input(label);
                let rendered = naming::render(
                    &converted.script,
                    &naming::variables(&release, &track, format),
                    "/music",
                )
                .ok();
                let expected = expected.map(|path| path.replace("{LONG}", &long));
                assert_eq!(rendered, expected, "{template} with {label}");
            }
        }
    }

    #[test]
    fn the_default_script_is_v2s_default_template() {
        assert_eq!(
            legacy_template_to_script(DEFAULT_NAMING_TEMPLATE),
            LegacyConversion {
                script: DEFAULT_NAMING_SCRIPT.to_owned(),
                warnings: Vec::new(),
            }
        );
        assert_eq!(legacy_template_to_script(" ").script, DEFAULT_NAMING_SCRIPT);
    }

    #[test]
    fn folders_that_can_render_empty_are_warned_about() {
        let warnings = |template: &str| legacy_template_to_script(template).warnings;
        assert_eq!(warnings("{year}/{title}").len(), 1);
        assert_eq!(warnings("{albumartist}/[{year}]/{title}").len(), 1);
        assert!(warnings("{albumartist}/{album} ({year})/{title}").is_empty());
        assert_eq!(
            warnings("{genre}/{title}"),
            vec!["{genre} was always empty for imports, so it is left out.".to_owned()]
        );
    }
}
