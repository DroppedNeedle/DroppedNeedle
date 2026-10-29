//! Library Management: profiles, naming and tagging scripts, root
//! assignments, and external refresh (secret-free on purpose).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::Section;

// --- library_management (secret-free on purpose) ------------------------

/// Settings schema version (v2 `LIBRARY_MANAGEMENT_SCHEMA_VERSION`).
pub const LIBRARY_MANAGEMENT_SCHEMA_VERSION: i64 = 1;
/// Default organization naming script (v2 Picard organizer id).
pub const PICARD_ORGANIZER_NAMING_SCRIPT_ID: &str = "69202666-cb88-52b0-bac2-0afc62b1e909";

/// Default sidecar patterns (v2 `DEFAULT_SIDECAR_PATTERNS`).
pub const DEFAULT_SIDECAR_PATTERNS: [&str; 27] = [
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "cover.webp",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "front.jpg",
    "front.png",
    "back.jpg",
    "back.jpeg",
    "back.png",
    "back.webp",
    "booklet*.jpg",
    "booklet*.jpeg",
    "booklet*.png",
    "booklet*.webp",
    "medium*.jpg",
    "medium*.jpeg",
    "medium*.png",
    "medium*.webp",
    "*.cue",
    "*.log",
    "*.lrc",
    "*.m3u",
    "*.m3u8",
    "*.pls",
];

/// Tag-field write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldMode {
    /// Do not touch.
    Disabled,
    /// Overwrite.
    #[default]
    Replace,
    /// Fill when empty.
    FillMissing,
    /// Merge values.
    Merge,
    /// Keep existing.
    Preserve,
}

/// Genre write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenreMode {
    /// Overwrite.
    #[default]
    Replace,
    /// Merge values.
    Merge,
    /// Fill when empty.
    FillMissing,
}

/// Genre source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenreSource {
    /// MusicBrainz.
    Musicbrainz,
    /// ListenBrainz.
    Listenbrainz,
    /// Last.fm.
    Lastfm,
    /// Keep local genres.
    ExistingLocal,
}

/// Artwork output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkOutputFormat {
    /// Keep original.
    #[default]
    Original,
    /// JPEG.
    Jpeg,
    /// PNG.
    Png,
    /// WebP.
    Webp,
}

/// Artist credit standardization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtistStandardization {
    /// As credited. `variations`, no longer offered, reads as this.
    #[default]
    #[serde(alias = "variations")]
    Credited,
    /// Canonical name.
    Canonical,
}

/// Credited relationship type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipType {
    /// Composer.
    Composer,
    /// Lyricist.
    Lyricist,
    /// Conductor.
    Conductor,
    /// Performer.
    Performer,
    /// Arranger.
    Arranger,
    /// Remixer.
    Remixer,
    /// Producer.
    Producer,
    /// Other.
    Other,
}

/// Unicode normalization for paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum UnicodeNormalization {
    /// NFC.
    #[default]
    NFC,
    /// NFKC.
    NFKC,
}

/// Extension case policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionCase {
    /// Keep as-is.
    #[default]
    Preserve,
    /// Lowercase.
    Lower,
    /// Uppercase.
    Upper,
}

/// ReplayGain write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplayGainMode {
    /// Leave tags alone.
    #[default]
    Preserve,
    /// Fill missing tags.
    FillMissing,
    /// Overwrite tags.
    Replace,
}

/// Multi-disc naming mode for a root override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MultiDiscNamingMode {
    /// Inherit the profile.
    #[default]
    Inherit,
    /// Standard naming.
    Standard,
    /// Naming script.
    Script,
}

/// One managed tag field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ManagedField {
    /// Field name.
    pub field: String,
    /// Write mode.
    pub mode: FieldMode,
    /// Clear when the canonical value is missing.
    pub clear_when_canonical_missing: bool,
}

impl Default for ManagedField {
    fn default() -> Self {
        Self {
            field: String::new(),
            mode: FieldMode::Replace,
            clear_when_canonical_missing: false,
        }
    }
}

/// Artist-credit handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ArtistCreditSettings {
    /// Standardization level.
    pub standardization: ArtistStandardization,
    /// Translate names.
    pub translate_names: bool,
    /// Preferred locales.
    pub preferred_locales: Vec<String>,
}

impl Default for ArtistCreditSettings {
    fn default() -> Self {
        Self {
            standardization: ArtistStandardization::Credited,
            translate_names: false,
            preferred_locales: Vec::new(),
        }
    }
}

/// Relationship-credit handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct RelationshipCreditSettings {
    /// Master switch.
    pub enabled: bool,
    /// Credited relationship types.
    pub types: Vec<RelationshipType>,
}

impl Default for RelationshipCreditSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            types: vec![
                RelationshipType::Composer,
                RelationshipType::Lyricist,
                RelationshipType::Conductor,
                RelationshipType::Performer,
                RelationshipType::Arranger,
                RelationshipType::Remixer,
                RelationshipType::Producer,
            ],
        }
    }
}

/// Metadata management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MetadataManagementSettings {
    /// Master switch.
    pub enabled: bool,
    /// Managed fields.
    pub fields: Vec<ManagedField>,
    /// Artist credits.
    pub artist_credits: ArtistCreditSettings,
    /// Relationship credits.
    pub relationships: RelationshipCreditSettings,
    /// Tagging script ids.
    pub tagging_script_ids: Vec<String>,
    /// Fields never touched.
    pub preserve_fields: Vec<String>,
    /// Scrub unmanaged tags.
    pub scrub_unmanaged_tags: bool,
    /// Keep embedded art during a scrub.
    pub preserve_embedded_art_during_scrub: bool,
}

/// v2 defaults: metadata management is on and embedded art survives a
/// scrub unless the profile says otherwise.
impl Default for MetadataManagementSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            fields: Vec::new(),
            artist_credits: ArtistCreditSettings::default(),
            relationships: RelationshipCreditSettings::default(),
            tagging_script_ids: Vec::new(),
            preserve_fields: Vec::new(),
            scrub_unmanaged_tags: false,
            preserve_embedded_art_during_scrub: true,
        }
    }
}

/// One genre alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct GenreAlias {
    /// Source label.
    pub source: String,
    /// Target label.
    pub target: String,
}

/// Genre management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct GenreManagementSettings {
    /// Master switch.
    pub enabled: bool,
    /// Write mode.
    pub mode: GenreMode,
    /// Genre sources.
    pub sources: Vec<GenreSource>,
    /// Max genres written.
    pub maximum_count: i64,
    /// MusicBrainz vote floor.
    pub musicbrainz_minimum_count: i64,
    /// ListenBrainz vote floor.
    pub listenbrainz_minimum_count: i64,
    /// Last.fm weight floor.
    pub lastfm_minimum_weight: i64,
    /// ListenBrainz curated tags only.
    pub listenbrainz_curated_only: bool,
    /// Last.fm whitelisted tags only.
    pub lastfm_whitelist_only: bool,
    /// Canonicalize labels.
    pub canonicalize: bool,
    /// Allowed labels.
    pub allowlist: Vec<String>,
    /// Blocked labels.
    pub denylist: Vec<String>,
    /// Aliases.
    pub aliases: Vec<GenreAlias>,
    /// Preferred casing.
    pub preferred_casing: Vec<String>,
    /// Primary genre only for constrained formats.
    pub write_primary_only_for_constrained_formats: bool,
}

impl Default for GenreManagementSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: GenreMode::Replace,
            sources: vec![GenreSource::Musicbrainz, GenreSource::Listenbrainz],
            maximum_count: 5,
            musicbrainz_minimum_count: 1,
            listenbrainz_minimum_count: 1,
            lastfm_minimum_weight: 10,
            listenbrainz_curated_only: true,
            lastfm_whitelist_only: true,
            canonicalize: true,
            allowlist: Vec::new(),
            denylist: Vec::new(),
            aliases: Vec::new(),
            preferred_casing: Vec::new(),
            write_primary_only_for_constrained_formats: false,
        }
    }
}

/// Artwork management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ArtworkManagementSettings {
    /// Embed art in files.
    pub embedded_enabled: bool,
    /// Write external art files.
    pub external_enabled: bool,
    /// Local filename patterns.
    pub local_file_patterns: Vec<String>,
    /// Embedded size cap (0 uncapped).
    pub embedded_maximum_size: i64,
    /// Embedded output format.
    pub embedded_format: ArtworkOutputFormat,
    /// External size cap (0 uncapped).
    pub external_maximum_size: i64,
    /// External output format.
    pub external_format: ArtworkOutputFormat,
    /// Never replace art with smaller art.
    pub never_replace_with_smaller: bool,
}

impl Default for ArtworkManagementSettings {
    fn default() -> Self {
        Self {
            embedded_enabled: true,
            external_enabled: true,
            local_file_patterns: [
                "cover.jpg",
                "cover.jpeg",
                "cover.png",
                "cover.webp",
                "folder.jpg",
                "folder.png",
                "front.jpg",
                "front.png",
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
            embedded_maximum_size: 1200,
            embedded_format: ArtworkOutputFormat::Jpeg,
            external_maximum_size: 0,
            external_format: ArtworkOutputFormat::Original,
            never_replace_with_smaller: true,
        }
    }
}

/// Path-compatibility handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PathCompatibilitySettings {
    /// Windows-safe names.
    pub windows_compatible: bool,
    /// Replace non-ASCII characters.
    pub replace_non_ascii: bool,
    /// Replace spaces with underscores.
    pub replace_spaces_with_underscores: bool,
    /// Path-separator replacement.
    pub separator_replacement: String,
    /// Max path-component length.
    pub maximum_component_length: i64,
    /// Max path length.
    pub maximum_path_length: i64,
    /// Unicode normalization.
    pub unicode_normalization: UnicodeNormalization,
    /// Extension case.
    pub extension_case: ExtensionCase,
    /// Honor the legacy Windows path limit.
    pub windows_legacy_path_limit: bool,
}

impl Default for PathCompatibilitySettings {
    fn default() -> Self {
        Self {
            windows_compatible: true,
            replace_non_ascii: false,
            replace_spaces_with_underscores: false,
            separator_replacement: "_".to_owned(),
            maximum_component_length: 240,
            maximum_path_length: 4096,
            unicode_normalization: UnicodeNormalization::NFC,
            extension_case: ExtensionCase::Preserve,
            windows_legacy_path_limit: false,
        }
    }
}

/// Organization management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OrganizationManagementSettings {
    /// Rename files.
    pub rename_enabled: bool,
    /// Move files.
    pub move_enabled: bool,
    /// Naming script id.
    pub naming_script_id: String,
    /// Multi-disc naming script id.
    pub multi_disc_naming_script_id: Option<String>,
    /// Path compatibility.
    pub compatibility: PathCompatibilitySettings,
    /// Move sidecar files along.
    pub move_sidecars: bool,
    /// Sidecar filename patterns.
    pub sidecar_patterns: Vec<String>,
}

impl Default for OrganizationManagementSettings {
    fn default() -> Self {
        Self {
            rename_enabled: true,
            move_enabled: true,
            naming_script_id: PICARD_ORGANIZER_NAMING_SCRIPT_ID.to_owned(),
            multi_disc_naming_script_id: None,
            compatibility: PathCompatibilitySettings::default(),
            move_sidecars: true,
            sidecar_patterns: DEFAULT_SIDECAR_PATTERNS
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

/// Lyrics enrichment block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LyricsManagementSettings {
    /// Master switch.
    pub enabled: bool,
    /// Write plain lyrics.
    pub write_plain: bool,
    /// Write synced lyrics.
    pub write_synced: bool,
    /// Keep existing lyrics.
    pub preserve_existing: bool,
    /// Lyrics required for completion.
    pub required: bool,
}

impl Default for LyricsManagementSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            write_plain: true,
            write_synced: true,
            preserve_existing: false,
            required: false,
        }
    }
}

/// ReplayGain enrichment block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ReplayGainManagementSettings {
    /// Master switch.
    pub enabled: bool,
    /// Write mode.
    pub mode: ReplayGainMode,
    /// Album-aware gain.
    pub album_aware: bool,
    /// Gain required for completion.
    pub required: bool,
}

impl Default for ReplayGainManagementSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ReplayGainMode::Preserve,
            album_aware: true,
            required: false,
        }
    }
}

/// Enrichment management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct EnrichmentManagementSettings {
    /// Lyrics.
    pub lyrics: LyricsManagementSettings,
    /// ReplayGain.
    pub replaygain: ReplayGainManagementSettings,
}

/// Catalog-identity policy (no file writes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct IdentityManagementSettings {
    /// Automatic edition acceptance.
    pub automatic_edition_acceptance_enabled: bool,
}

/// Post-publish notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct ProfileNotificationSettings {
    /// Refresh external servers.
    pub refresh_external_servers: bool,
}

/// One named management profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryManagementProfile {
    /// Profile id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Preset origin.
    pub preset_origin: Option<String>,
    /// Preset version.
    pub preset_version: Option<i64>,
    /// Content revision.
    pub revision: String,
    /// Metadata block.
    pub metadata: MetadataManagementSettings,
    /// Genre block.
    pub genres: GenreManagementSettings,
    /// Artwork block.
    pub artwork: ArtworkManagementSettings,
    /// Organization block.
    pub organization: OrganizationManagementSettings,
    /// Enrichment block.
    pub enrichment: EnrichmentManagementSettings,
    /// Identity policy.
    pub identity: IdentityManagementSettings,
    /// Notifications.
    pub notification: ProfileNotificationSettings,
}

impl Default for LibraryManagementProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            preset_origin: None,
            preset_version: None,
            revision: String::new(),
            metadata: MetadataManagementSettings {
                enabled: true,
                preserve_embedded_art_during_scrub: true,
                ..MetadataManagementSettings::default()
            },
            genres: GenreManagementSettings::default(),
            artwork: ArtworkManagementSettings::default(),
            organization: OrganizationManagementSettings::default(),
            enrichment: EnrichmentManagementSettings::default(),
            identity: IdentityManagementSettings::default(),
            notification: ProfileNotificationSettings::default(),
        }
    }
}

/// One naming script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct NamingScript {
    /// Script id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Script source.
    pub source: String,
    /// Content revision.
    pub revision: String,
    /// Preset origin.
    pub preset_origin: Option<String>,
    /// Preset version.
    pub preset_version: Option<i64>,
}

/// One tagging script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct TaggingScript {
    /// Script id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Script source.
    pub source: String,
    /// Content revision.
    pub revision: String,
    /// Preset origin.
    pub preset_origin: Option<String>,
    /// Preset version.
    pub preset_version: Option<i64>,
}

/// Per-root profile overrides (`None` inherits the profile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct LibraryManagementRootOverrides {
    /// Metadata switch.
    pub metadata_enabled: Option<bool>,
    /// Genre switch.
    pub genres_enabled: Option<bool>,
    /// Embedded-artwork switch.
    pub embedded_artwork_enabled: Option<bool>,
    /// External-artwork switch.
    pub external_artwork_enabled: Option<bool>,
    /// Rename switch.
    pub rename_enabled: Option<bool>,
    /// Move switch.
    pub move_enabled: Option<bool>,
    /// Sidecar-move switch.
    pub move_sidecars: Option<bool>,
    /// Naming script id.
    pub naming_script_id: Option<String>,
    /// Multi-disc naming mode.
    pub multi_disc_naming_mode: MultiDiscNamingMode,
    /// Multi-disc naming script id.
    pub multi_disc_naming_script_id: Option<String>,
}

/// One root-to-profile assignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct LibraryManagementRootAssignment {
    /// Library root id.
    pub root_id: String,
    /// Assigned profile id.
    pub profile_id: Option<String>,
    /// Per-root overrides.
    pub overrides: Option<LibraryManagementRootOverrides>,
    /// Assignment switch.
    pub enabled: bool,
    /// Automatic acquisitions.
    pub automatic_acquisitions: bool,
    /// Automatic drop imports.
    pub automatic_drop_imports: bool,
    /// Automatic scan-discovered organization.
    pub automatic_scan_discovered: bool,
    /// Automatic custom editions.
    pub automatic_custom_editions: bool,
    /// Activation pins (set by the activation flow).
    pub activation_profile_revision: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_naming_policy_revision: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_policy_revision: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_settings_revision: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_preview_token: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_preview_hash: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_confirmed_at: Option<f64>,
    /// Who confirmed the dry run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_confirmed_by: Option<String>,
}

/// External-server refresh after publishing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ExternalRefreshSettings {
    /// Master switch.
    pub enabled: bool,
    /// Refresh Plex.
    pub plex_enabled: bool,
    /// Refresh Jellyfin.
    pub jellyfin_enabled: bool,
    /// Refresh Navidrome.
    pub navidrome_enabled: bool,
}

/// Library management settings (secret-free on purpose).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = LibraryManagementSettings)]
#[serde(default, remote = "Self")]
pub struct LibraryManagement {
    /// Settings schema version.
    pub schema_version: i64,
    /// Preset catalog version.
    pub preset_catalog_version: i64,
    /// Named profiles.
    pub profiles: Vec<LibraryManagementProfile>,
    /// Default profile id.
    pub default_profile_id: String,
    /// Root assignments.
    pub root_assignments: Vec<LibraryManagementRootAssignment>,
    /// Naming scripts.
    pub naming_scripts: Vec<NamingScript>,
    /// Tagging scripts.
    pub tagging_scripts: Vec<TaggingScript>,
    /// Undo retention in days.
    pub undo_retention_days: i64,
    /// Preview retention in hours.
    pub preview_retention_hours: i64,
    /// External refresh.
    pub external_refresh: ExternalRefreshSettings,
    /// The document these settings were decoded from, never written: an
    /// activation pinned against options since removed hashed them too,
    /// and a read needs their stored values to carry it.
    #[serde(skip)]
    pub as_read: AsRead,
}

/// A decoded document, kept beside the settings read from it. It takes no
/// part in comparing settings: two reads of one value are equal.
#[derive(Clone, Default)]
pub struct AsRead(pub Option<std::sync::Arc<serde_json::Value>>);

impl PartialEq for AsRead {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for AsRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AsRead")
    }
}

impl Serialize for LibraryManagement {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Self::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for LibraryManagement {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = serde_json::Value::deserialize(deserializer)?;
        let mut settings = Self::deserialize(&document).map_err(serde::de::Error::custom)?;
        settings.as_read = AsRead(Some(std::sync::Arc::new(document)));
        Ok(settings)
    }
}

impl Default for LibraryManagement {
    fn default() -> Self {
        Self {
            schema_version: LIBRARY_MANAGEMENT_SCHEMA_VERSION,
            preset_catalog_version: 0,
            profiles: Vec::new(),
            default_profile_id: String::new(),
            root_assignments: Vec::new(),
            naming_scripts: Vec::new(),
            tagging_scripts: Vec::new(),
            undo_retention_days: 90,
            preview_retention_hours: 24,
            external_refresh: ExternalRefreshSettings::default(),
            as_read: AsRead::default(),
        }
    }
}

impl Section for LibraryManagement {
    const KEY: &'static str = "library_management";
}
