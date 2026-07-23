"""Typed configuration contracts for opt-in Library Management.

These settings describe policy only. They do not grant activation and no constructor
or migration in this module starts work or mutates a library file.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import uuid
from typing import Literal

import msgspec

from api.v1.schemas.settings import DEFAULT_NAMING_TEMPLATE
from core.management_script_language import (
    compile_naming_template,
    compile_tagging_program,
    walk_expressions,
    walk_statements,
)
from core.exceptions import ScriptValidationError
from infrastructure.msgspec_fastapi import AppStruct

LIBRARY_MANAGEMENT_SCHEMA_VERSION = 1
PICARD_ORGANIZER_PRESET_VERSION = 1

PICARD_ORGANIZER_PROFILE_ID = "c2741223-da7c-5231-bcf5-7cead27b07d9"
PICARD_ORGANIZER_NAMING_SCRIPT_ID = "f66f6409-ba0c-5b9a-9258-8fb91eefcb0b"
LEGACY_NAMING_PROFILE_ID = "b9b1f9b4-752d-54ff-bc67-24f75cb67847"
LEGACY_NAMING_SCRIPT_ID = "ec7b9d83-00f7-5a78-93c7-20d117bef008"

MAX_SCRIPT_SOURCE_LENGTH = 32_768
MAX_SIDE_CAR_PATTERNS = 100
MAX_ARTWORK_PATTERNS = 100
MAX_GENRE_RULES = 500
MAX_PRESERVE_FIELDS = 100

FieldMode = Literal["disabled", "replace", "fill_missing", "merge", "preserve"]
GenreMode = Literal["replace", "merge", "fill_missing"]
GenreSource = Literal["musicbrainz", "listenbrainz", "lastfm", "existing_local"]
ArtworkProvider = Literal[
    "cover_art_archive_release",
    "cover_art_archive_release_group",
    "local_files",
    "embedded",
    "audiodb",
]
ArtworkImageType = Literal[
    "front",
    "back",
    "booklet",
    "medium",
    "tray",
    "obi",
    "spine",
    "track",
    "other",
]
ArtworkOutputFormat = Literal["original", "jpeg", "png", "webp"]
ArtworkDownloadSize = Literal["full", "1200", "500", "250"]
ArtistStandardization = Literal["credited", "variations", "canonical"]
RelationshipType = Literal[
    "composer",
    "lyricist",
    "conductor",
    "performer",
    "arranger",
    "remixer",
    "producer",
    "other",
]
SourceCleanupMode = Literal["keep", "remove_after_confirmed_move"]
ID3Version = Literal["2.4", "2.3"]
MP3ApePolicy = Literal["preserve", "remove"]
RawAacTagPolicy = Literal["save_apev2", "do_not_write", "remove_apev2"]
WavTagPolicy = Literal["id3", "riff_info", "preserve_existing"]
ManagementImpactClassification = Literal[
    "no_change", "harmless", "restrictive", "destructive"
]

MANAGED_FIELD_NAMES: tuple[str, ...] = (
    "album",
    "title",
    "album_sort",
    "title_sort",
    "artist",
    "album_artist",
    "artist_sort",
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
    "media",
    "compilation",
    "label",
    "catalog_number",
    "barcode",
    "asin",
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
    "acoustid_id",
    "acoustid_fingerprint",
)

LYRICS_MANAGED_FIELD_NAMES: tuple[str, ...] = (
    "lyrics_plain",
    "lyrics_synced",
)
REPLAYGAIN_MANAGED_FIELD_NAMES: tuple[str, ...] = (
    "replaygain_track_gain",
    "replaygain_album_gain",
    "replaygain_track_peak",
    "replaygain_album_peak",
)
ENRICHMENT_MANAGED_FIELD_NAMES: tuple[str, ...] = (
    *LYRICS_MANAGED_FIELD_NAMES,
    *REPLAYGAIN_MANAGED_FIELD_NAMES,
)
AUDIO_MANAGED_FIELD_NAMES: tuple[str, ...] = (
    *MANAGED_FIELD_NAMES,
    "genre",
    *ENRICHMENT_MANAGED_FIELD_NAMES,
)

MERGEABLE_MANAGED_FIELD_NAMES: frozenset[str] = frozenset(
    {
        "artist",
        "album_artist",
        "artist_sort",
        "album_artist_sort",
        "release_type",
        "label",
        "catalog_number",
        "isrc",
        "musicbrainz_artist_id",
        "musicbrainz_album_artist_id",
        "musicbrainz_work_id",
        "composer",
        "lyricist",
        "conductor",
        "performer",
        "arranger",
        "remixer",
        "producer",
    }
)


class ManagedFieldSettings(AppStruct):
    field: str
    mode: FieldMode = "replace"
    clear_when_canonical_missing: bool = False


class ArtistCreditSettings(AppStruct):
    standardization: ArtistStandardization = "credited"
    translate_names: bool = False
    preferred_locales: list[str] = msgspec.field(default_factory=list)


class RelationshipCreditSettings(AppStruct):
    enabled: bool = True
    types: list[RelationshipType] = msgspec.field(
        default_factory=lambda: [
            "composer",
            "lyricist",
            "conductor",
            "performer",
            "arranger",
            "remixer",
            "producer",
        ]
    )


class FormatCompatibilitySettings(AppStruct):
    id3_version: ID3Version = "2.4"
    id3v23_join_delimiter: str = "; "
    id3_text_encoding: Literal["utf8", "utf16"] = "utf8"
    remove_id3_from_flac: bool = False
    mp3_apev2_policy: MP3ApePolicy = "preserve"
    raw_aac_tag_policy: RawAacTagPolicy = "save_apev2"
    wav_tag_policy: WavTagPolicy = "id3"
    constrained_genres_primary_only: bool = False


class MetadataManagementSettings(AppStruct):
    enabled: bool = True
    fields: list[ManagedFieldSettings] = msgspec.field(default_factory=list)
    artist_credits: ArtistCreditSettings = msgspec.field(
        default_factory=ArtistCreditSettings
    )
    relationships: RelationshipCreditSettings = msgspec.field(
        default_factory=RelationshipCreditSettings
    )
    tagging_script_ids: list[str] = msgspec.field(default_factory=list)
    preserve_fields: list[str] = msgspec.field(default_factory=list)
    scrub_unmanaged_tags: bool = False
    preserve_embedded_art_during_scrub: bool = True
    format_compatibility: FormatCompatibilitySettings = msgspec.field(
        default_factory=FormatCompatibilitySettings
    )


class GenreAliasSettings(AppStruct):
    source: str
    target: str


class GenreManagementSettings(AppStruct):
    enabled: bool = True
    mode: GenreMode = "replace"
    sources: list[GenreSource] = msgspec.field(
        default_factory=lambda: ["musicbrainz", "listenbrainz"]
    )
    maximum_count: int = 5
    musicbrainz_minimum_count: int = 1
    listenbrainz_minimum_count: int = 1
    lastfm_minimum_weight: int = 10
    listenbrainz_curated_only: bool = True
    lastfm_whitelist_only: bool = True
    canonicalize: bool = True
    maximum_ancestry_depth: int = 4
    allowlist: list[str] = msgspec.field(default_factory=list)
    denylist: list[str] = msgspec.field(default_factory=list)
    aliases: list[GenreAliasSettings] = msgspec.field(default_factory=list)
    preferred_casing: list[str] = msgspec.field(default_factory=list)
    write_primary_only_for_constrained_formats: bool = False


class ArtworkManagementSettings(AppStruct):
    embedded_enabled: bool = True
    external_enabled: bool = True
    providers: list[ArtworkProvider] = msgspec.field(
        default_factory=lambda: [
            "cover_art_archive_release",
            "cover_art_archive_release_group",
            "local_files",
            "embedded",
        ]
    )
    approved_only: bool = True
    download_size: ArtworkDownloadSize = "full"
    local_file_patterns: list[str] = msgspec.field(
        default_factory=lambda: [
            "cover.jpg",
            "cover.jpeg",
            "cover.png",
            "cover.webp",
            "folder.jpg",
            "folder.png",
            "front.jpg",
            "front.png",
        ]
    )
    image_types: list[ArtworkImageType] = msgspec.field(
        default_factory=lambda: ["front"]
    )
    minimum_width: int = 0
    minimum_height: int = 0
    embedded_maximum_size: int = 1200
    embedded_format: ArtworkOutputFormat = "jpeg"
    external_maximum_size: int = 0
    external_format: ArtworkOutputFormat = "original"
    embedded_front_only: bool = True
    external_front_only: bool = True
    never_replace_with_smaller: bool = True
    preserve_existing_types: list[ArtworkImageType] = msgspec.field(
        default_factory=list
    )
    external_naming_script_id: str | None = None
    overwrite_external_files: bool = False


class PathCompatibilitySettings(AppStruct):
    windows_compatible: bool = True
    replace_non_ascii: bool = False
    replace_spaces_with_underscores: bool = False
    separator_replacement: str = "_"
    maximum_component_length: int = 240
    maximum_path_length: int = 4096
    unicode_normalization: Literal["NFC", "NFKC"] = "NFC"
    extension_case: Literal["preserve", "lower", "upper"] = "preserve"
    windows_legacy_path_limit: bool = False


class OrganizationManagementSettings(AppStruct):
    rename_enabled: bool = True
    move_enabled: bool = True
    naming_script_id: str = PICARD_ORGANIZER_NAMING_SCRIPT_ID
    compatibility: PathCompatibilitySettings = msgspec.field(
        default_factory=PathCompatibilitySettings
    )
    move_sidecars: bool = True
    sidecar_patterns: list[str] = msgspec.field(
        default_factory=lambda: [
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
        ]
    )
    source_cleanup: SourceCleanupMode = "remove_after_confirmed_move"
    remove_empty_directories: bool = True


class FileBehaviorSettings(AppStruct):
    preserve_timestamps: bool = True
    preserve_permissions: bool = True
    strict_capability_gate: bool = True
    reject_symlinks: bool = True
    validate_written_metadata: bool = True
    validate_technical_audio: bool = True


class LyricsManagementSettings(AppStruct):
    enabled: bool = False
    provider: Literal["lrclib"] = "lrclib"
    write_plain: bool = True
    write_synced: bool = False
    required: bool = False


class ReplayGainManagementSettings(AppStruct):
    enabled: bool = False
    mode: Literal["preserve", "fill_missing", "replace"] = "preserve"
    album_aware: bool = True
    required: bool = False


class EnrichmentManagementSettings(AppStruct):
    lyrics: LyricsManagementSettings = msgspec.field(
        default_factory=LyricsManagementSettings
    )
    replaygain: ReplayGainManagementSettings = msgspec.field(
        default_factory=ReplayGainManagementSettings
    )


class ProfileNotificationSettings(AppStruct):
    refresh_droppedneedle: bool = True
    refresh_external_servers: bool = False


class LibraryManagementProfile(AppStruct):
    id: str
    name: str
    description: str = ""
    preset_origin: str | None = None
    preset_version: int | None = None
    revision: str = ""
    metadata: MetadataManagementSettings = msgspec.field(
        default_factory=MetadataManagementSettings
    )
    genres: GenreManagementSettings = msgspec.field(
        default_factory=GenreManagementSettings
    )
    artwork: ArtworkManagementSettings = msgspec.field(
        default_factory=ArtworkManagementSettings
    )
    organization: OrganizationManagementSettings = msgspec.field(
        default_factory=OrganizationManagementSettings
    )
    file_behavior: FileBehaviorSettings = msgspec.field(
        default_factory=FileBehaviorSettings
    )
    enrichment: EnrichmentManagementSettings = msgspec.field(
        default_factory=EnrichmentManagementSettings
    )
    notification: ProfileNotificationSettings = msgspec.field(
        default_factory=ProfileNotificationSettings
    )


class NamingScriptSettings(AppStruct):
    id: str
    name: str
    source: str
    revision: str = ""
    preset_origin: str | None = None
    preset_version: int | None = None


class TaggingScriptSettings(AppStruct):
    id: str
    name: str
    source: str
    revision: str = ""
    preset_origin: str | None = None
    preset_version: int | None = None


class LibraryManagementRootOverrides(AppStruct):
    metadata_enabled: bool | None = None
    genres_enabled: bool | None = None
    embedded_artwork_enabled: bool | None = None
    external_artwork_enabled: bool | None = None
    rename_enabled: bool | None = None
    move_enabled: bool | None = None
    move_sidecars: bool | None = None
    source_cleanup: SourceCleanupMode | None = None
    preserve_timestamps: bool | None = None
    naming_script_id: str | None = None


class LibraryManagementRootAssignment(AppStruct):
    root_id: str
    profile_id: str | None = None
    overrides: LibraryManagementRootOverrides | None = None
    enabled: bool = False
    automatic_acquisitions: bool = False
    automatic_drop_imports: bool = False
    automatic_scan_discovered: bool = False
    activation_profile_revision: str | None = None
    activation_policy_revision: str | None = None
    activation_settings_revision: str | None = None
    activation_preview_token: str | None = None
    activation_preview_hash: str | None = None
    activation_confirmed_at: float | None = None


class ExternalRefreshSettings(AppStruct):
    enabled: bool = False
    plex_enabled: bool = False
    jellyfin_enabled: bool = False
    navidrome_enabled: bool = False
    retry_attempts: int = 3
    retry_delay_seconds: int = 30


class LibraryManagementSettings(AppStruct):
    schema_version: int = LIBRARY_MANAGEMENT_SCHEMA_VERSION
    profiles: list[LibraryManagementProfile] = msgspec.field(default_factory=list)
    default_profile_id: str = ""
    root_assignments: list[LibraryManagementRootAssignment] = msgspec.field(
        default_factory=list
    )
    naming_scripts: list[NamingScriptSettings] = msgspec.field(default_factory=list)
    tagging_scripts: list[TaggingScriptSettings] = msgspec.field(default_factory=list)
    undo_retention_days: int = 90
    preview_retention_hours: int = 24
    recycle_bin_path: str = ""
    external_refresh: ExternalRefreshSettings = msgspec.field(
        default_factory=ExternalRefreshSettings
    )


class LibraryManagementSettingsResponse(LibraryManagementSettings):
    settings_revision: str = ""


class LibraryManagementSettingsUpdateRequest(AppStruct):
    settings: LibraryManagementSettings
    expected_settings_revision: str


class LibraryManagementChangeImpact(AppStruct):
    current_settings_revision: str
    proposed_settings_revision: str
    stale: bool = False
    classification: ManagementImpactClassification = "no_change"
    preview_required: bool = False
    affected_root_ids: list[str] = msgspec.field(default_factory=list)
    reasons: list[str] = msgspec.field(default_factory=list)


class LibraryManagementPresetDiff(AppStruct):
    profile_id: str
    preset_origin: str | None = None
    preset_version: int | None = None
    differs: bool = False
    changed_groups: list[str] = msgspec.field(default_factory=list)
    preset_profile: LibraryManagementProfile | None = None


def _stable_hash(value: object) -> str:
    encoded = json.dumps(
        msgspec.to_builtins(value),
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()
    return hashlib.sha256(encoded).hexdigest()


def _revision_without(value: object, field: str) -> str:
    payload = msgspec.to_builtins(value)
    if not isinstance(payload, dict):
        raise TypeError("Revision input must be a struct object.")
    payload.pop(field, None)
    encoded = json.dumps(
        payload, ensure_ascii=True, separators=(",", ":"), sort_keys=True
    ).encode()
    return hashlib.sha256(encoded).hexdigest()


def profile_revision(profile: LibraryManagementProfile) -> str:
    return _revision_without(profile, "revision")


def naming_script_revision(script: NamingScriptSettings) -> str:
    return _revision_without(script, "revision")


def tagging_script_revision(script: TaggingScriptSettings) -> str:
    return _revision_without(script, "revision")


def settings_revision(settings: LibraryManagementSettings) -> str:
    return _stable_hash(settings)


def _validate_uuid(value: str, label: str) -> None:
    try:
        uuid.UUID(value)
    except (ValueError, AttributeError) as exc:
        raise ValueError(f"{label} must be a UUID.") from exc


def _validate_name(value: str, label: str) -> str:
    normalized = " ".join(value.split())
    if not normalized:
        raise ValueError(f"{label} needs a name.")
    if len(normalized) > 120:
        raise ValueError(f"{label} name is too long.")
    return normalized


def _validate_script_source(source: str, label: str) -> str:
    normalized = source.rstrip()
    if not normalized:
        raise ValueError(f"{label} needs source text.")
    if "\x00" in normalized or len(normalized) > MAX_SCRIPT_SOURCE_LENGTH:
        raise ValueError(f"{label} source is invalid or too long.")
    return normalized


def _script_error(
    message: str,
    script_name: str,
    line: int,
    column: int,
) -> None:
    raise ScriptValidationError(
        message,
        script_name=script_name,
        line=line,
        column=column,
    )


def _validate_naming_language(source: str, script_name: str) -> None:
    variables = {
        *MANAGED_FIELD_NAMES,
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
        "year",
        "track",
        "disc",
        "ext",
        "extension",
        "medium",
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
    }
    if "\n" in source or "\r" in source:
        _script_error(
            "Naming scripts must be a single path template.", script_name, 1, 1
        )
    segments = compile_naming_template(source, script_name=script_name)
    path_shape = "".join(
        segment.literal if segment.literal is not None else "value"
        for segment in segments
    )
    if path_shape.startswith("/") or any(
        part in {"", ".", ".."} for part in path_shape.split("/")
    ):
        _script_error(
            "Naming script must stay in a non-empty relative path.",
            script_name,
            1,
            1,
        )
    for segment in segments:
        if segment.legacy_variable is not None:
            if segment.legacy_variable not in variables:
                _script_error(
                    f"Unknown naming variable: {segment.legacy_variable}.",
                    script_name,
                    segment.line,
                    segment.column,
                )
            if segment.format_spec and segment.legacy_variable not in {
                "track",
                "disc",
                "track_number",
                "disc_number",
                "total_tracks",
                "total_discs",
            }:
                _script_error(
                    f"Variable {segment.legacy_variable} does not support a numeric format.",
                    script_name,
                    segment.line,
                    segment.column,
                )
        if segment.expression is not None:
            for expression in walk_expressions(segment.expression):
                if expression.kind == "variable" and expression.value not in variables:
                    _script_error(
                        f"Unknown naming variable: {expression.value}.",
                        script_name,
                        expression.line,
                        expression.column,
                    )


def _validate_tagging_language(source: str, script_name: str) -> None:
    allowed_fields = {*MANAGED_FIELD_NAMES, "genre"}
    ordered_fields = {*MERGEABLE_MANAGED_FIELD_NAMES, "genre"}
    read_only = {
        "artist_display",
        "album_artist_display",
        "artists",
        "album_artists",
        "genres",
        "year",
        "primary_genre",
    }
    program = compile_tagging_program(source, script_name=script_name)
    for statement in walk_statements(program):
        target = statement.target
        if target is not None:
            custom_name = (
                target[7:].strip() if target.casefold().startswith("custom.") else None
            )
            if target not in allowed_fields and (
                not custom_name or "\x00" in custom_name or len(custom_name) > 255
            ):
                _script_error(
                    f"Unknown or invalid tagging target: {target}.",
                    script_name,
                    statement.line,
                    statement.column,
                )
            if (
                statement.operation == "append"
                and target not in ordered_fields
                and custom_name is None
            ):
                _script_error(
                    f"Field {target} does not accept append.",
                    script_name,
                    statement.line,
                    statement.column,
                )
        if statement.expression is None:
            continue
        for expression in walk_expressions(statement.expression):
            if expression.kind != "variable":
                continue
            variable = str(expression.value)
            if (
                variable not in allowed_fields
                and variable not in read_only
                and not variable.casefold().startswith("custom.")
            ):
                _script_error(
                    f"Unknown tagging variable: {variable}.",
                    script_name,
                    expression.line,
                    expression.column,
                )


def _validate_sidecar_pattern(pattern: str) -> str:
    value = pattern.strip()
    parsed = PurePosixPath(value)
    if (
        not value
        or "\\" in value
        or parsed.is_absolute()
        or any(part in ("", ".", "..") for part in parsed.parts)
    ):
        raise ValueError(
            f"Sidecar pattern must stay inside the album directory: {pattern}"
        )
    return value


def _validate_artwork_pattern(pattern: str) -> str:
    value = pattern.strip()
    parsed = PurePosixPath(value)
    if (
        not value
        or value in {"*", "**", "**/*"}
        or "\\" in value
        or parsed.is_absolute()
        or any(part in ("", ".", "..") for part in parsed.parts)
    ):
        raise ValueError(
            f"Artwork pattern must stay inside the album directory: {pattern}"
        )
    return value


def _normalize_unique_strings(values: list[str], label: str) -> list[str]:
    result: list[str] = []
    seen: set[str] = set()
    for raw in values:
        value = raw.strip()
        folded = value.casefold()
        if not value:
            raise ValueError(f"{label} cannot contain an empty value.")
        if folded in seen:
            raise ValueError(f"{label} cannot contain duplicates.")
        seen.add(folded)
        result.append(value)
    return result


def normalize_library_management_settings(
    settings: LibraryManagementSettings,
) -> LibraryManagementSettings:
    """Normalize, validate, and populate deterministic derived revisions."""

    if settings.schema_version != LIBRARY_MANAGEMENT_SCHEMA_VERSION:
        raise ValueError("Unsupported Library Management settings version.")
    if not 1 <= settings.undo_retention_days <= 3650:
        raise ValueError("Undo retention must be between 1 and 3650 days.")
    if not 1 <= settings.preview_retention_hours <= 168:
        raise ValueError("Preview retention must be between 1 and 168 hours.")
    settings.recycle_bin_path = settings.recycle_bin_path.strip()
    if (
        "\x00" in settings.recycle_bin_path
        or len(settings.recycle_bin_path) > 4096
        or (
            settings.recycle_bin_path
            and not Path(settings.recycle_bin_path).is_absolute()
        )
    ):
        raise ValueError("The recycle bin must be an absolute path.")
    if not 0 <= settings.external_refresh.retry_attempts <= 20:
        raise ValueError("External refresh retries must be between 0 and 20.")
    if not 1 <= settings.external_refresh.retry_delay_seconds <= 3600:
        raise ValueError(
            "External refresh retry delay must be between 1 and 3600 seconds."
        )

    script_ids: set[str] = set()
    script_names: set[str] = set()
    for script in settings.naming_scripts:
        _validate_uuid(script.id, "Naming script ID")
        if script.id in script_ids:
            raise ValueError("Every naming script needs a unique ID.")
        script_ids.add(script.id)
        script.name = _validate_name(script.name, "Naming script")
        folded = script.name.casefold()
        if folded in script_names:
            raise ValueError("Every naming script needs a unique name.")
        script_names.add(folded)
        script.source = _validate_script_source(script.source, script.name)
        _validate_naming_language(script.source, script.name)
        script.revision = naming_script_revision(script)

    tagging_ids: set[str] = set()
    tagging_names: set[str] = set()
    for script in settings.tagging_scripts:
        _validate_uuid(script.id, "Tagging script ID")
        if script.id in tagging_ids:
            raise ValueError("Every tagging script needs a unique ID.")
        tagging_ids.add(script.id)
        script.name = _validate_name(script.name, "Tagging script")
        folded = script.name.casefold()
        if folded in tagging_names:
            raise ValueError("Every tagging script needs a unique name.")
        tagging_names.add(folded)
        script.source = _validate_script_source(script.source, script.name)
        _validate_tagging_language(script.source, script.name)
        script.revision = tagging_script_revision(script)

    profile_ids: set[str] = set()
    profile_names: set[str] = set()
    allowed_fields = set(MANAGED_FIELD_NAMES)
    for profile in settings.profiles:
        _validate_uuid(profile.id, "Profile ID")
        if profile.id in profile_ids:
            raise ValueError("Every Library Management profile needs a unique ID.")
        profile_ids.add(profile.id)
        profile.name = _validate_name(profile.name, "Library Management profile")
        folded = profile.name.casefold()
        if folded in profile_names:
            raise ValueError("Every Library Management profile needs a unique name.")
        profile_names.add(folded)

        field_names: set[str] = set()
        for field in profile.metadata.fields:
            if field.field not in allowed_fields:
                raise ValueError(f"Unknown managed field: {field.field}")
            if field.field in field_names:
                raise ValueError(f"Managed field is configured twice: {field.field}")
            if (
                field.mode == "merge"
                and field.field not in MERGEABLE_MANAGED_FIELD_NAMES
            ):
                raise ValueError(f"Managed field does not support merge: {field.field}")
            field_names.add(field.field)
        profile.metadata.preserve_fields = _normalize_unique_strings(
            profile.metadata.preserve_fields, "Preserved fields"
        )
        if len(profile.metadata.preserve_fields) > MAX_PRESERVE_FIELDS:
            raise ValueError("A profile has too many preserved fields.")
        if any(
            "\x00" in value or len(value) > 255
            for value in profile.metadata.preserve_fields
        ):
            raise ValueError("A preserved field name is invalid.")
        profile.metadata.artist_credits.preferred_locales = _normalize_unique_strings(
            profile.metadata.artist_credits.preferred_locales, "Preferred locales"
        )
        if len(profile.metadata.format_compatibility.id3v23_join_delimiter) > 8:
            raise ValueError("The ID3v2.3 join delimiter is too long.")
        if (
            profile.metadata.format_compatibility.id3_version == "2.3"
            and profile.metadata.format_compatibility.id3_text_encoding == "utf8"
        ):
            raise ValueError("ID3v2.3 requires UTF-16 text encoding.")
        if any(
            value not in tagging_ids for value in profile.metadata.tagging_script_ids
        ):
            raise ValueError("A profile references an unknown tagging script.")
        if len(set(profile.metadata.tagging_script_ids)) != len(
            profile.metadata.tagging_script_ids
        ):
            raise ValueError("A profile cannot attach a tagging script twice.")

        genres = profile.genres
        if not 1 <= genres.maximum_count <= 100:
            raise ValueError("Maximum genre count must be between 1 and 100.")
        if not 0 <= genres.musicbrainz_minimum_count <= 1_000_000:
            raise ValueError("MusicBrainz genre threshold is invalid.")
        if not 0 <= genres.listenbrainz_minimum_count <= 1_000_000:
            raise ValueError("ListenBrainz genre threshold is invalid.")
        if not 0 <= genres.lastfm_minimum_weight <= 100:
            raise ValueError("Last.fm genre weight must be between 0 and 100.")
        if not 0 <= genres.maximum_ancestry_depth <= 32:
            raise ValueError("Genre ancestry depth must be between 0 and 32.")
        if len(set(genres.sources)) != len(genres.sources):
            raise ValueError("Genre sources cannot contain duplicates.")
        genres.allowlist = _normalize_unique_strings(
            genres.allowlist, "Genre allowlist"
        )
        genres.denylist = _normalize_unique_strings(genres.denylist, "Genre denylist")
        genres.preferred_casing = _normalize_unique_strings(
            genres.preferred_casing, "Preferred genre casing"
        )
        if any(
            len(values) > MAX_GENRE_RULES
            for values in (
                genres.allowlist,
                genres.denylist,
                genres.aliases,
                genres.preferred_casing,
            )
        ):
            raise ValueError("A genre rule list is too large.")
        alias_sources: set[str] = set()
        for alias in genres.aliases:
            alias.source = alias.source.strip()
            alias.target = alias.target.strip()
            if not alias.source or not alias.target:
                raise ValueError("Genre aliases need a source and target.")
            folded_alias = alias.source.casefold()
            if folded_alias in alias_sources:
                raise ValueError("Genre alias sources must be unique.")
            alias_sources.add(folded_alias)

        artwork = profile.artwork
        if len(set(artwork.providers)) != len(artwork.providers):
            raise ValueError("Artwork providers cannot contain duplicates.")
        if len(set(artwork.image_types)) != len(artwork.image_types):
            raise ValueError("Artwork image types cannot contain duplicates.")
        if len(artwork.local_file_patterns) > MAX_ARTWORK_PATTERNS:
            raise ValueError("A profile has too many local artwork patterns.")
        artwork.local_file_patterns = [
            _validate_artwork_pattern(value) for value in artwork.local_file_patterns
        ]
        if len({value.casefold() for value in artwork.local_file_patterns}) != len(
            artwork.local_file_patterns
        ):
            raise ValueError("Local artwork patterns cannot contain duplicates.")
        if any(
            value < 0 or value > 20_000
            for value in (
                artwork.minimum_width,
                artwork.minimum_height,
                artwork.embedded_maximum_size,
                artwork.external_maximum_size,
            )
        ):
            raise ValueError("Artwork dimensions must be between 0 and 20000 pixels.")
        if artwork.external_naming_script_id is not None and (
            artwork.external_naming_script_id not in script_ids
        ):
            raise ValueError("Artwork references an unknown naming script.")
        lyrics = profile.enrichment.lyrics
        if lyrics.enabled and not (lyrics.write_plain or lyrics.write_synced):
            raise ValueError("Enabled lyrics need at least one output format.")

        organization = profile.organization
        if organization.naming_script_id not in script_ids:
            raise ValueError("A profile references an unknown naming script.")
        if len(organization.sidecar_patterns) > MAX_SIDE_CAR_PATTERNS:
            raise ValueError("A profile has too many sidecar patterns.")
        organization.sidecar_patterns = [
            _validate_sidecar_pattern(value) for value in organization.sidecar_patterns
        ]
        if len(set(organization.sidecar_patterns)) != len(
            organization.sidecar_patterns
        ):
            raise ValueError("Sidecar patterns cannot contain duplicates.")
        compatibility = organization.compatibility
        if (
            len(compatibility.separator_replacement) != 1
            or compatibility.separator_replacement in "/\\\x00"
        ):
            raise ValueError("Path separator replacement must be one safe character.")
        if not 1 <= compatibility.maximum_component_length <= 255:
            raise ValueError("Maximum path component length must be between 1 and 255.")
        if not 64 <= compatibility.maximum_path_length <= 32_767:
            raise ValueError("Maximum path length must be between 64 and 32767.")
        if not profile.file_behavior.reject_symlinks:
            raise ValueError("Library Management cannot follow symlinks.")
        profile.revision = profile_revision(profile)

    if (
        not settings.default_profile_id
        or settings.default_profile_id not in profile_ids
    ):
        raise ValueError("The default Library Management profile does not exist.")

    assignment_roots: set[str] = set()
    for assignment in settings.root_assignments:
        if not assignment.root_id.strip() or assignment.root_id in assignment_roots:
            raise ValueError("Every root can have only one management assignment.")
        assignment_roots.add(assignment.root_id)
        if (
            assignment.profile_id is not None
            and assignment.profile_id not in profile_ids
        ):
            raise ValueError("A root assignment references an unknown profile.")
        if (
            assignment.overrides is not None
            and assignment.overrides.naming_script_id is not None
            and assignment.overrides.naming_script_id not in script_ids
        ):
            raise ValueError("A root override references an unknown naming script.")

    settings.naming_scripts.sort(key=lambda value: value.id)
    settings.tagging_scripts.sort(key=lambda value: value.id)
    settings.profiles.sort(key=lambda value: value.id)
    settings.root_assignments.sort(key=lambda value: value.root_id)
    return settings


def picard_style_organizer_profile() -> LibraryManagementProfile:
    fields = [
        ManagedFieldSettings(field=field)
        for field in MANAGED_FIELD_NAMES
        if field not in {"acoustid_id", "acoustid_fingerprint"}
    ]
    fields.extend(
        [
            ManagedFieldSettings(field="acoustid_id", mode="fill_missing"),
            ManagedFieldSettings(field="acoustid_fingerprint", mode="fill_missing"),
        ]
    )
    return LibraryManagementProfile(
        id=PICARD_ORGANIZER_PROFILE_ID,
        name="Picard-style Organizer",
        description=(
            "Canonical MusicBrainz tags and artwork with same-root organization, "
            "sidecars, and custom-tag preservation."
        ),
        preset_origin="picard_style_organizer",
        preset_version=PICARD_ORGANIZER_PRESET_VERSION,
        metadata=MetadataManagementSettings(fields=fields),
    )


def build_initial_library_management_settings(
    legacy_naming_template: str = DEFAULT_NAMING_TEMPLATE,
) -> LibraryManagementSettings:
    """Create available presets without assigning or enabling any library root."""

    picard_script = NamingScriptSettings(
        id=PICARD_ORGANIZER_NAMING_SCRIPT_ID,
        name="Picard-style folders",
        source=DEFAULT_NAMING_TEMPLATE,
        preset_origin="picard_style_organizer",
        preset_version=PICARD_ORGANIZER_PRESET_VERSION,
    )
    legacy_script = NamingScriptSettings(
        id=LEGACY_NAMING_SCRIPT_ID,
        name="Existing DroppedNeedle naming",
        source=legacy_naming_template or DEFAULT_NAMING_TEMPLATE,
        preset_origin="legacy_naming_template",
        preset_version=1,
    )
    legacy_profile = LibraryManagementProfile(
        id=LEGACY_NAMING_PROFILE_ID,
        name="Existing naming template",
        description=(
            "Path-only seed copied from the naming template that was configured "
            "before Library Management."
        ),
        preset_origin="legacy_naming_template",
        preset_version=1,
        metadata=MetadataManagementSettings(enabled=False),
        genres=GenreManagementSettings(enabled=False),
        artwork=ArtworkManagementSettings(
            embedded_enabled=False,
            external_enabled=False,
        ),
        organization=OrganizationManagementSettings(
            naming_script_id=LEGACY_NAMING_SCRIPT_ID,
            move_sidecars=False,
            sidecar_patterns=[],
            source_cleanup="keep",
            remove_empty_directories=False,
        ),
    )
    return normalize_library_management_settings(
        LibraryManagementSettings(
            profiles=[picard_style_organizer_profile(), legacy_profile],
            default_profile_id=PICARD_ORGANIZER_PROFILE_ID,
            root_assignments=[],
            naming_scripts=[picard_script, legacy_script],
        )
    )


def to_library_management_response(
    settings: LibraryManagementSettings,
) -> LibraryManagementSettingsResponse:
    normalized = normalize_library_management_settings(settings)
    payload = msgspec.to_builtins(normalized)
    payload["settings_revision"] = settings_revision(normalized)
    return msgspec.convert(
        payload,
        type=LibraryManagementSettingsResponse,
    )
