"""Acquisition quality evidence, immutable policy snapshots, and decisions.

Source-neutral domain structs shared by every acquisition surface (Soulseek,
Usenet, Free Music, local verification, task/review projections). Pure data +
closed enums ONLY: classification/comparison logic lives in
``services/native/acquisition/quality.py``; nothing here performs I/O or reads
settings. Persisted across our HTTP boundary, so everything subclasses
``AppStruct``.

Semantics governed by ``.dev-notes/Plans/Acquisition.md`` (owner-signed);
field names mirror the frontend hand-mirrored types in
``frontend/src/lib/types.ts`` - change both together.
"""

import enum

import msgspec

from infrastructure.msgspec_fastapi import AppStruct

# Schema version of the persisted snapshot shape. Legacy snapshots intentionally
# stay version 1; recipe-bearing snapshots use version 2.
SNAPSHOT_SCHEMA_VERSION = 1
RECIPE_SNAPSHOT_SCHEMA_VERSION = 2

# The closed v2 recipe only admits these two containers. Keep the broader set
# here so the evaluator can distinguish a known audio format from an absent or
# opaque extension without importing an I/O-heavy metadata module.
RECIPE_FORMATS: tuple[str, ...] = ("flac", "mp3")
KNOWN_AUDIO_EXTENSIONS: frozenset[str] = frozenset(
    {
        "flac",
        "mp3",
        "m4a",
        "m4b",
        "mp4",
        "ogg",
        "oga",
        "opus",
        "wav",
        "aac",
        "wma",
        "alac",
        "ape",
        "wv",
        "dsf",
        "dff",
        "mka",
    }
)

# The recipe is deliberately closed over formats/quality identifiers. Adding a
# codec later is an explicit schema change rather than accepting arbitrary codec
# strings from the settings API.
MP3_RECIPE_QUALITIES: tuple[str, ...] = (
    "below_192",
    "192_255",
    "256_319",
    "320_plus",
    "custom",
)
FLAC_RECIPE_QUALITIES: tuple[str, ...] = (
    "cd",
    "24_48",
    "24_96",
    "24_192",
    "hi_res",
    "custom",
)
RECIPE_QUALITIES: frozenset[str] = frozenset(
    (*MP3_RECIPE_QUALITIES, *FLAC_RECIPE_QUALITIES)
)
_MP3_STANDARD_BOUNDS: dict[str, tuple[int | None, int, int | None]] = {
    "below_192": (16, 128, 191),
    "192_255": (192, 192, 255),
    "256_319": (256, 256, 319),
    "320_plus": (320, 320, None),
}


def _strict_int(value: object, field: str, *, low: int, high: int) -> int:
    if type(value) is not int or not low <= value <= high:
        raise ValueError(f"{field} must be an integer between {low} and {high}")
    return value


def _validate_recipe_entry_fields(
    format: object,
    quality: object,
    min_bitrate_kbps: object,
    target_bitrate_kbps: object,
    max_bitrate_kbps: object,
    bit_depth: object,
    sample_rate_hz: object,
) -> tuple[int | None, int | None, int | None, int | None, int | None]:
    if format not in RECIPE_FORMATS:
        raise ValueError(f"unsupported quality recipe format: {format!r}")
    if not isinstance(quality, str):
        raise ValueError("quality recipe quality must be a string")
    allowed = MP3_RECIPE_QUALITIES if format == "mp3" else FLAC_RECIPE_QUALITIES
    if quality not in allowed:
        raise ValueError(f"unsupported {format} quality recipe value: {quality!r}")

    if format == "mp3":
        if bit_depth is not None or sample_rate_hz is not None:
            raise ValueError("MP3 recipe entries cannot define FLAC resolution")
        if quality == "custom":
            minimum = _strict_int(
                min_bitrate_kbps, "min_bitrate_kbps", low=16, high=2048
            )
            target = _strict_int(
                target_bitrate_kbps, "target_bitrate_kbps", low=16, high=2048
            )
            maximum = _strict_int(
                max_bitrate_kbps, "max_bitrate_kbps", low=16, high=2048
            )
            if not minimum <= target <= maximum:
                raise ValueError(
                    "custom MP3 recipe requires min_bitrate_kbps <= "
                    "target_bitrate_kbps <= max_bitrate_kbps"
                )
            return minimum, target, maximum, None, None
        bounds = _MP3_STANDARD_BOUNDS[quality]
        expected_minimum, expected_target, expected_maximum = bounds
        if (min_bitrate_kbps, target_bitrate_kbps, max_bitrate_kbps) not in {
            (None, None, None),
            (expected_minimum, expected_target, expected_maximum),
        }:
            raise ValueError(
                "standard MP3 recipe fields must use their canonical bounds"
            )
        return expected_minimum, expected_target, expected_maximum, None, None

    if any(
        value is not None
        for value in (min_bitrate_kbps, target_bitrate_kbps, max_bitrate_kbps)
    ):
        raise ValueError("FLAC recipe entries cannot define a bitrate")
    if quality == "custom":
        depth = _strict_int(bit_depth, "bit_depth", low=1, high=64)
        rate = _strict_int(sample_rate_hz, "sample_rate_hz", low=8000, high=768000)
        return None, None, None, depth, rate
    if bit_depth is not None or sample_rate_hz is not None:
        raise ValueError("standard FLAC recipe entries cannot define exact resolution")
    return None, None, None, None, None


class QualityRecipeEntry(AppStruct, forbid_unknown_fields=True):
    """One ordered, closed format-quality recipe entry.

    Standard entries omit numeric details on the wire and are canonicalised to
    their fixed MP3 bounds. Custom entries carry only the fields relevant to
    their format. Unknown keys are rejected at every decode boundary so a
    future setting cannot silently become a different recipe.
    """

    format: str
    quality: str
    min_bitrate_kbps: int | None = None
    target_bitrate_kbps: int | None = None
    max_bitrate_kbps: int | None = None
    bit_depth: int | None = None
    sample_rate_hz: int | None = None

    def __post_init__(self) -> None:
        (
            self.min_bitrate_kbps,
            self.target_bitrate_kbps,
            self.max_bitrate_kbps,
            self.bit_depth,
            self.sample_rate_hz,
        ) = _validate_recipe_entry_fields(
            self.format,
            self.quality,
            self.min_bitrate_kbps,
            self.target_bitrate_kbps,
            self.max_bitrate_kbps,
            self.bit_depth,
            self.sample_rate_hz,
        )


def _intervals_overlap(
    left_min: int, left_max: int | None, right_min: int, right_max: int | None
) -> bool:
    return (left_max is None or right_min <= left_max) and (
        right_max is None or left_min <= right_max
    )


def validate_quality_recipe(
    entries: list[QualityRecipeEntry], *, require_nonempty: bool = True
) -> list[QualityRecipeEntry]:
    """Validate recipe uniqueness/overlap and return canonical entry copies."""
    if not isinstance(entries, list):
        raise ValueError("quality_recipe must be a list")
    if require_nonempty and not entries:
        raise ValueError("quality_recipe must contain at least one entry")
    canonical: list[QualityRecipeEntry] = []
    for entry in entries:
        if not isinstance(entry, QualityRecipeEntry):
            raise ValueError("quality_recipe entries must be objects")
        canonical.append(
            QualityRecipeEntry(
                format=entry.format,
                quality=entry.quality,
                min_bitrate_kbps=entry.min_bitrate_kbps,
                target_bitrate_kbps=entry.target_bitrate_kbps,
                max_bitrate_kbps=entry.max_bitrate_kbps,
                bit_depth=entry.bit_depth,
                sample_rate_hz=entry.sample_rate_hz,
            )
        )
    seen_standard: set[tuple[str, str]] = set()
    mp3_ranges: list[tuple[int, int | None, int]] = []
    flac_custom_pairs: set[tuple[int, int]] = set()
    for index, entry in enumerate(canonical):
        if entry.quality != "custom":
            key = (entry.format, entry.quality)
            if key in seen_standard:
                raise ValueError(
                    f"quality_recipe contains duplicate {entry.format}/{entry.quality}"
                )
            seen_standard.add(key)
        if entry.format == "mp3":
            minimum = entry.min_bitrate_kbps
            maximum = entry.max_bitrate_kbps
            assert minimum is not None
            if entry.quality != "custom":
                for existing_min, existing_max, _ in mp3_ranges:
                    if _intervals_overlap(minimum, maximum, existing_min, existing_max):
                        raise ValueError("MP3 quality recipe ranges overlap")
            else:
                for existing_min, existing_max, _ in mp3_ranges:
                    if _intervals_overlap(minimum, maximum, existing_min, existing_max):
                        raise ValueError(
                            "custom MP3 quality recipe overlaps another MP3 range"
                        )
            mp3_ranges.append((minimum, maximum, index))
        elif entry.quality == "custom":
            pair = (entry.bit_depth, entry.sample_rate_hz)
            assert pair[0] is not None and pair[1] is not None
            if pair in flac_custom_pairs:
                raise ValueError(
                    "quality_recipe contains duplicate custom FLAC resolution"
                )
            flac_custom_pairs.add(pair)
    return canonical


class CodecFamily(enum.Enum):
    """The proven (or unknown) codec family of one unit of audio evidence."""

    LOSSY = "lossy"
    LOSSLESS = "lossless"
    UNKNOWN = "unknown"


class EvidenceCertainty(enum.Enum):
    """How much the evidence chain can be trusted. Ordering matters:
    ``exact > partial > inferred > unknown``."""

    EXACT = "exact"
    PARTIAL = "partial"
    INFERRED = "inferred"
    UNKNOWN = "unknown"


class EvidenceProvenance(enum.Enum):
    """WHERE the evidence came from. Drives the trust ceiling applied upstream."""

    LOCAL_PROBE = "local_probe"
    SOURCE_METADATA = "source_metadata"
    ARCHIVE_FORMAT = "archive_format"
    RELEASE_TITLE = "release_title"
    CATEGORY = "category"
    FORMAT_ONLY = "format_only"
    NONE = "none"


class UnknownQualityBehavior(enum.Enum):
    """Global rule for family-unknown evidence (codec cannot be determined)."""

    REJECT = "reject"
    REVIEW = "review"
    ALLOW_AS_FALLBACK = "allow_as_fallback"


class SourceSelectionMode(enum.Enum):
    """How the orchestrator walks sources: configured order first (default),
    or earliest global preference step across concurrently searched sources."""

    SOURCE_FIRST = "source_first"
    QUALITY_FIRST = "quality_first"


class SnapshotOrigin(enum.Enum):
    """Why this snapshot exists: captured live at creation, written once by the
    startup migration for pre-existing rows, or pinned by an intentional manual
    override flow."""

    GLOBAL_POLICY = "global_policy"
    LEGACY_MIGRATION = "legacy_migration"
    MANUAL_OVERRIDE = "manual_override"


class QualityReason(str, enum.Enum):
    """Stable reason codes (spec "Every decision has structured reasons").
    EXACTLY these seventeen; frontend maps each to consistent copy."""

    PREFERRED_TIER = "preferred_tier"
    FALLBACK_TIER = "fallback_tier"
    PREFERRED_LOSSY_BITRATE = "preferred_lossy_bitrate"
    LOSSY_BITRATE_BELOW_MINIMUM = "lossy_bitrate_below_minimum"
    LOSSY_BITRATE_ABOVE_MAXIMUM = "lossy_bitrate_above_maximum"
    LOSSY_BITRATE_UNKNOWN = "lossy_bitrate_unknown"
    PREFERRED_LOSSLESS_RESOLUTION = "preferred_lossless_resolution"
    LOSSLESS_RESOLUTION_ABOVE_MAXIMUM = "lossless_resolution_above_maximum"
    LOSSLESS_RESOLUTION_PARTIAL = "lossless_resolution_partial"
    LOSSLESS_RESOLUTION_UNKNOWN = "lossless_resolution_unknown"
    QUALITY_INFERRED_FROM_TITLE = "quality_inferred_from_title"
    QUALITY_INFERRED_FROM_CATEGORY = "quality_inferred_from_category"
    FORMAT_NOT_IMPORTABLE = "format_not_importable"
    FORMAT_OUTSIDE_POLICY = "format_outside_policy"
    OUTSIDE_GLOBAL_PREFERENCE = "outside_global_preference"
    POST_DOWNLOAD_QUALITY_MISMATCH = "post_download_quality_mismatch"
    MANUAL_QUALITY_OVERRIDE = "manual_quality_override"


# Known-unimportable audio containers (DSD families; anything else unsupported is
# caught downstream by the importer's own suffix gate). A declared DSD release must
# not spend bandwidth even when a noisy title claims "lossless".
NOT_IMPORTABLE_EXTENSIONS: frozenset[str] = frozenset({"dsd", "dsf", "dff"})
# Fixed 6-step lossless detail ladder INSIDE the canonical ``lossless`` tier
# (never new canonical tiers). Steps ascend in technical detail:
#   0 "cd"      : at most 16-bit / 48 kHz
#   1 "24_48"   : up to 24-bit / 48 kHz
#   2 "24_96"   : up to 24-bit / 96 kHz
#   3 "24_192"  : up to 24-bit / 192 kHz
#   4 "hi_res"  : above 24-bit or above 192 kHz
#   5 "partial" : resolution partial/unknown
LOSSLESS_DETAIL_STEPS: tuple[str, ...] = (
    "cd",
    "24_48",
    "24_96",
    "24_192",
    "hi_res",
    "partial",
)

# Legacy comparator floor used ONLY as a ranking stand-in for an ABSENT axis
# (a known axis may prove a cap rejection but cannot prove a positive step).
_LEGACY_FLOOR_BIT_DEPTH = 16
_LEGACY_FLOOR_SAMPLE_RATE = 44100


def detail_comparator_axes(
    bit_depth: int | None, sample_rate_hz: int | None
) -> tuple[int, int]:
    """``(depth, rate)`` comparison axes with the legacy floor filled in for an
    ABSENT axis (preserves migrated Soulseek FLAC ordering; labelled partial).
    """
    return (
        bit_depth if bit_depth is not None else _LEGACY_FLOOR_BIT_DEPTH,
        sample_rate_hz if sample_rate_hz is not None else _LEGACY_FLOOR_SAMPLE_RATE,
    )


def lossless_detail_step(bit_depth: int | None, sample_rate_hz: int | None) -> int:
    """Map trusted depth/rate onto the fixed ladder. Both axes present -> steps
    0-4 by ascending order match; either axis absent -> the ``partial`` step.
    Requires Hz-normalised integers (khz text handling belongs to parsing code).
    """
    if bit_depth is None or sample_rate_hz is None:
        return 5
    if bit_depth <= _LEGACY_FLOOR_BIT_DEPTH and sample_rate_hz <= 48000:
        return 0
    if bit_depth <= 24 and sample_rate_hz <= 48000:
        return 1
    if bit_depth <= 24 and sample_rate_hz <= 96000:
        return 2
    if bit_depth <= 24 and sample_rate_hz <= 192000:
        return 3
    return 4


class AudioQualityEvidence(AppStruct):
    """Measured/inferred quality facts about ONE downloaded-or-proposed unit of
    audio: a candidate folder (aggregate over its audio files), a track file, or
    a probed download. Aggregation policy: a folder is as good as its WORST
    audio file; ``mixed_*`` flags record heterogeneity instead of averaging."""

    # Canonical effective extension/container, lowercase, no dot ("flac", "mp3",
    # "m4a"). Empty string when nothing was determinable.
    extension: str = ""
    codec_family: CodecFamily = CodecFamily.UNKNOWN
    # kbps when meaningful for the family (None for lossless bitrates - FLAC
    # bitrate varies with compression and is NOT a fidelity axis).
    bitrate_kbps: int | None = None
    bit_depth: int | None = None
    sample_rate_hz: int | None = None
    total_bytes: int | None = None
    audio_file_count: int = 0
    mixed_format: bool = False
    mixed_quality: bool = False
    certainty: EvidenceCertainty = EvidenceCertainty.UNKNOWN
    provenance: EvidenceProvenance = EvidenceProvenance.NONE


class AcquisitionQualitySnapshot(AppStruct, forbid_unknown_fields=True):
    """Immutable quality policy pinned to one task or search."""

    schema_version: int = SNAPSHOT_SCHEMA_VERSION
    # Stable sha256 over the canonical JSON of policy inputs; excludes hash,
    # origin, and derived summary.
    snapshot_hash: str = ""
    # Legacy v1 canonical tier order. Recipe-bearing snapshots use
    # ``quality_recipe`` as the source of truth and leave this field empty.
    quality_preference_order: list[str] = []
    quality_recipe: list[QualityRecipeEntry] = []
    lossy_target_kbps: int | None = None
    lossy_min_bitrate_kbps: int | None = None
    lossy_max_bitrate_kbps: int | None = None
    lossless_preference: str = "highest"
    lossless_max_bit_depth: int | None = None
    lossless_max_sample_rate_hz: int | None = None
    flac_mp3_only: bool = True
    unknown_quality_behavior: str = UnknownQualityBehavior.ALLOW_AS_FALLBACK.value
    source_selection_mode: str = SourceSelectionMode.SOURCE_FIRST.value
    summary: str = ""
    origin: str = SnapshotOrigin.GLOBAL_POLICY.value


class QualityDecision(AppStruct):
    """Evaluator verdict for one evidence item under one snapshot."""

    eligible: bool = False
    disposition: str = ""  # ""|reject|review|fallback|eligible
    tier: str | None = None
    preference_step: int | None = None
    quality_recipe_index: int | None = None
    quality_recipe_entry: QualityRecipeEntry | None = None
    lossless_detail_step: int | None = None
    evidence: AudioQualityEvidence = msgspec.field(default_factory=AudioQualityEvidence)
    reasons: list[QualityReason] = msgspec.field(default_factory=list)
    summary: str = ""
