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

/// Artwork provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkProvider {
    /// CAA release.
    CoverArtArchiveRelease,
    /// CAA release group.
    CoverArtArchiveReleaseGroup,
    /// Local files.
    LocalFiles,
    /// Embedded art.
    Embedded,
    /// AudioDB.
    Audiodb,
}

/// Artwork image type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkImageType {
    /// Front cover.
    Front,
    /// Back cover.
    Back,
    /// Booklet.
    Booklet,
    /// Medium.
    Medium,
    /// Tray.
    Tray,
    /// Obi.
    Obi,
    /// Spine.
    Spine,
    /// Track art.
    Track,
    /// Other.
    Other,
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

/// Artwork download size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkDownloadSize {
    /// Full size.
    #[default]
    Full,
    /// 1200px.
    #[serde(rename = "1200")]
    Size1200,
    /// 500px.
    #[serde(rename = "500")]
    Size500,
    /// 250px.
    #[serde(rename = "250")]
    Size250,
}

/// Artist credit standardization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtistStandardization {
    /// As credited.
    #[default]
    Credited,
    /// Accepted variations.
    Variations,
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

/// Source-tree cleanup after a confirmed move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceCleanupMode {
    /// Leave sources alone.
    Keep,
    /// Remove after a confirmed move.
    #[default]
    RemoveAfterConfirmedMove,
}

/// ID3 version for MP3 writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum Id3Version {
    /// ID3v2.4.
    #[serde(rename = "2.4")]
    #[default]
    V24,
    /// ID3v2.3.
    #[serde(rename = "2.3")]
    V23,
}

/// APEv2 policy for MP3 writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mp3ApePolicy {
    /// Preserve APEv2 tags.
    #[default]
    Preserve,
    /// Remove APEv2 tags.
    Remove,
}

/// Raw-AAC tag policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RawAacTagPolicy {
    /// Write APEv2.
    #[default]
    SaveApev2,
    /// Write nothing.
    DoNotWrite,
    /// Remove APEv2.
    RemoveApev2,
}

/// WAV tag policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WavTagPolicy {
    /// ID3 chunk.
    #[default]
    Id3,
    /// RIFF INFO chunk.
    RiffInfo,
    /// Leave existing tags alone.
    PreserveExisting,
}

/// ID3 text encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Id3TextEncoding {
    /// UTF-8.
    #[default]
    Utf8,
    /// UTF-16.
    Utf16,
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

/// Format-compatibility handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FormatCompatibilitySettings {
    /// ID3 version for MP3 writes.
    pub id3_version: Id3Version,
    /// ID3v2.3 multi-value join delimiter.
    pub id3v23_join_delimiter: String,
    /// ID3 text encoding.
    pub id3_text_encoding: Id3TextEncoding,
    /// Strip ID3 chunks from FLAC.
    pub remove_id3_from_flac: bool,
    /// APEv2 policy for MP3.
    pub mp3_apev2_policy: Mp3ApePolicy,
    /// Raw-AAC tag policy.
    pub raw_aac_tag_policy: RawAacTagPolicy,
    /// WAV tag policy.
    pub wav_tag_policy: WavTagPolicy,
    /// Primary genre only for constrained formats.
    pub constrained_genres_primary_only: bool,
}

impl Default for FormatCompatibilitySettings {
    fn default() -> Self {
        Self {
            id3_version: Id3Version::V24,
            id3v23_join_delimiter: "; ".to_owned(),
            id3_text_encoding: Id3TextEncoding::Utf8,
            remove_id3_from_flac: false,
            mp3_apev2_policy: Mp3ApePolicy::Preserve,
            raw_aac_tag_policy: RawAacTagPolicy::SaveApev2,
            wav_tag_policy: WavTagPolicy::Id3,
            constrained_genres_primary_only: false,
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
    /// Format compatibility.
    pub format_compatibility: FormatCompatibilitySettings,
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
            format_compatibility: FormatCompatibilitySettings::default(),
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
    /// Max genre-ancestry depth.
    pub maximum_ancestry_depth: i64,
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
            maximum_ancestry_depth: 4,
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
    /// Provider order.
    pub providers: Vec<ArtworkProvider>,
    /// Approved art only.
    pub approved_only: bool,
    /// Download size.
    pub download_size: ArtworkDownloadSize,
    /// Local filename patterns.
    pub local_file_patterns: Vec<String>,
    /// Image types to fetch.
    pub image_types: Vec<ArtworkImageType>,
    /// Minimum width (0 any).
    pub minimum_width: i64,
    /// Minimum height (0 any).
    pub minimum_height: i64,
    /// Embedded size cap (0 uncapped).
    pub embedded_maximum_size: i64,
    /// Embedded output format.
    pub embedded_format: ArtworkOutputFormat,
    /// External size cap (0 uncapped).
    pub external_maximum_size: i64,
    /// External output format.
    pub external_format: ArtworkOutputFormat,
    /// Embedded front only.
    pub embedded_front_only: bool,
    /// External front only.
    pub external_front_only: bool,
    /// Never replace art with smaller art.
    pub never_replace_with_smaller: bool,
    /// Existing types never replaced.
    pub preserve_existing_types: Vec<ArtworkImageType>,
    /// External naming script id.
    pub external_naming_script_id: Option<String>,
    /// Overwrite external files.
    pub overwrite_external_files: bool,
}

impl Default for ArtworkManagementSettings {
    fn default() -> Self {
        Self {
            embedded_enabled: true,
            external_enabled: true,
            providers: vec![
                ArtworkProvider::CoverArtArchiveRelease,
                ArtworkProvider::CoverArtArchiveReleaseGroup,
                ArtworkProvider::LocalFiles,
                ArtworkProvider::Embedded,
            ],
            approved_only: true,
            download_size: ArtworkDownloadSize::Full,
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
            image_types: vec![ArtworkImageType::Front],
            minimum_width: 0,
            minimum_height: 0,
            embedded_maximum_size: 1200,
            embedded_format: ArtworkOutputFormat::Jpeg,
            external_maximum_size: 0,
            external_format: ArtworkOutputFormat::Original,
            embedded_front_only: true,
            external_front_only: true,
            never_replace_with_smaller: true,
            preserve_existing_types: Vec::new(),
            external_naming_script_id: None,
            overwrite_external_files: false,
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
    /// Source cleanup after a confirmed move.
    pub source_cleanup: SourceCleanupMode,
    /// Remove newly empty directories.
    pub remove_empty_directories: bool,
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
            source_cleanup: SourceCleanupMode::RemoveAfterConfirmedMove,
            remove_empty_directories: true,
        }
    }
}

/// File-behavior gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FileBehaviorSettings {
    /// Preserve mtimes.
    pub preserve_timestamps: bool,
    /// Preserve permission bits.
    pub preserve_permissions: bool,
    /// Refuse writes the format cannot hold.
    pub strict_capability_gate: bool,
    /// Refuse symlinked media.
    pub reject_symlinks: bool,
    /// Re-read tags after writing.
    pub validate_written_metadata: bool,
    /// Re-probe audio after writing.
    pub validate_technical_audio: bool,
}

impl Default for FileBehaviorSettings {
    fn default() -> Self {
        Self {
            preserve_timestamps: true,
            preserve_permissions: true,
            strict_capability_gate: true,
            reject_symlinks: true,
            validate_written_metadata: true,
            validate_technical_audio: true,
        }
    }
}

/// Lyrics enrichment block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LyricsManagementSettings {
    /// Master switch.
    pub enabled: bool,
    /// Lyrics provider.
    pub provider: String,
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
            provider: "lrclib".to_owned(),
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ProfileNotificationSettings {
    /// Refresh DroppedNeedle views.
    pub refresh_droppedneedle: bool,
    /// Refresh external servers.
    pub refresh_external_servers: bool,
}

impl Default for ProfileNotificationSettings {
    fn default() -> Self {
        Self {
            refresh_droppedneedle: true,
            refresh_external_servers: false,
        }
    }
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
    /// File-behavior gates.
    pub file_behavior: FileBehaviorSettings,
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
            file_behavior: FileBehaviorSettings::default(),
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
    /// Source cleanup.
    pub source_cleanup: Option<SourceCleanupMode>,
    /// Timestamp preservation.
    pub preserve_timestamps: Option<bool>,
    /// Naming script id.
    pub naming_script_id: Option<String>,
    /// Multi-disc naming mode.
    pub multi_disc_naming_mode: MultiDiscNamingMode,
    /// Multi-disc naming script id.
    pub multi_disc_naming_script_id: Option<String>,
    /// Automatic edition acceptance.
    pub automatic_edition_acceptance_enabled: Option<bool>,
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
}

/// External-server refresh after publishing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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
    /// Retry attempts.
    pub retry_attempts: i64,
    /// Retry delay in seconds.
    pub retry_delay_seconds: i64,
}

impl Default for ExternalRefreshSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            plex_enabled: false,
            jellyfin_enabled: false,
            navidrome_enabled: false,
            retry_attempts: 3,
            retry_delay_seconds: 30,
        }
    }
}

/// Library management settings (secret-free on purpose).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(as = LibraryManagementSettings)]
#[serde(default)]
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
    /// Recycle-bin path ("" disables).
    pub recycle_bin_path: String,
    /// External refresh.
    pub external_refresh: ExternalRefreshSettings,
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
            recycle_bin_path: String::new(),
            external_refresh: ExternalRefreshSettings::default(),
        }
    }
}

impl Section for LibraryManagement {
    const KEY: &'static str = "library_management";
}
