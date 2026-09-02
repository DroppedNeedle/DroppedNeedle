"""Pure acquisition-quality classification/comparison (no I/O).

Every scorer, the orchestrator's stored-snapshot recheck, Free Music ranking,
the local probe's mismatch comparison, and task/review projections consume THIS
module; nothing here reads settings files, stores, or the network. Runtime has
NO import of the API ``DownloadPolicySettings`` schema (breaking an import
cycle api.schemas <- services): ``build_snapshot`` duck-types its ``policy``
argument against ``DownloadPolicySettings``' quality fields, type-hinted under
``TYPE_CHECKING``.

Semantics governed by ``.dev-notes/Plans/Acquisition.md``:
- identity/importability/quota gates stay OUT of here (they precede quality);
- a mixed album takes the worst policy verdict of any audio file;
- proven-lossy-without-bitrate keeps the legacy canonical ``low`` projection
  (plus ``lossy_bitrate_unknown``) and is NEVER promoted through the
  family-unknown rule;
- lossless caps reject only PROVABLE violations (a known axis exceeding the
  cap); one absent axis ranks via the legacy comparator floor and stays
  labelled partial.
"""

import hashlib
import json
import msgspec
import re
from typing import TYPE_CHECKING

from models.acquisition_quality import (
    FLAC_RECIPE_QUALITIES,
    KNOWN_AUDIO_EXTENSIONS,
    LOSSLESS_DETAIL_STEPS,
    MP3_RECIPE_QUALITIES,
    NOT_IMPORTABLE_EXTENSIONS,
    RECIPE_FORMATS,
    RECIPE_SNAPSHOT_SCHEMA_VERSION,
    SNAPSHOT_SCHEMA_VERSION,
    AudioQualityEvidence,
    AcquisitionQualitySnapshot,
    CodecFamily,
    EvidenceCertainty,
    EvidenceProvenance,
    QualityDecision,
    QualityReason,
    QualityRecipeEntry,
    SnapshotOrigin,
    SourceSelectionMode,
    UnknownQualityBehavior,
    detail_comparator_axes,
    lossless_detail_step,
    validate_quality_recipe,
)

if TYPE_CHECKING:  # pragma: no cover - typing only, never imported at runtime
    from api.v1.schemas.settings import DownloadPolicySettings

# Canonical five tiers, best -> worst (mirrors quality_tiers.TIER_KEYS; kept
# local so this module depends on models only, never on production ranking).
_TIER_KEYS_BEST_FIRST: tuple[str, ...] = (
    "lossless",
    "mp3_320",
    "mp3_256",
    "mp3_192",
    "low",
)

CERTAINTY_RANK: dict[EvidenceCertainty, int] = {
    EvidenceCertainty.EXACT: 3,
    EvidenceCertainty.PARTIAL: 2,
    EvidenceCertainty.INFERRED: 1,
    EvidenceCertainty.UNKNOWN: 0,
}

# Provenance trust ladder, lowest -> highest (drives evaluate_worst's merge).
_PROVENANCE_ORDER: tuple[EvidenceProvenance, ...] = (
    EvidenceProvenance.NONE,
    EvidenceProvenance.FORMAT_ONLY,
    EvidenceProvenance.CATEGORY,
    EvidenceProvenance.RELEASE_TITLE,
    EvidenceProvenance.ARCHIVE_FORMAT,
    EvidenceProvenance.SOURCE_METADATA,
    EvidenceProvenance.LOCAL_PROBE,
)

_LOSSY_BANDS: tuple[tuple[int, str], ...] = (
    (320, "mp3_320"),
    (256, "mp3_256"),
    (192, "mp3_192"),
)


def is_recipe_snapshot(snapshot: AcquisitionQualitySnapshot) -> bool:
    """Whether ``snapshot`` carries the v2 source-of-truth recipe."""
    return snapshot.schema_version == RECIPE_SNAPSHOT_SCHEMA_VERSION and bool(
        snapshot.quality_recipe
    )


def _recipe_entry_label(entry: QualityRecipeEntry) -> str:
    if entry.format == "flac":
        if entry.quality == "custom":
            return (
                f"FLAC {entry.bit_depth}-bit/{round(entry.sample_rate_hz / 1000)} kHz"
            )
        return f"FLAC {entry.quality.replace('_', '/')}"
    if entry.quality == "custom":
        return (
            f"MP3 {entry.min_bitrate_kbps}-{entry.target_bitrate_kbps}-"
            f"{entry.max_bitrate_kbps} kbps"
        )
    return f"MP3 {entry.quality.replace('_', '-')} kbps"


def _recipe_interval_contains(entry: QualityRecipeEntry, bitrate: int) -> bool:
    return (
        entry.min_bitrate_kbps is not None
        and bitrate >= entry.min_bitrate_kbps
        and (entry.max_bitrate_kbps is None or bitrate <= entry.max_bitrate_kbps)
    )


def recipe_entry_for_evidence(
    snapshot: AcquisitionQualitySnapshot, evidence: AudioQualityEvidence
) -> tuple[int, QualityRecipeEntry] | None:
    """Return the first v2 entry matching complete evidence.

    The recipe is an ordered ladder, not a type-priority list: a custom entry
    only wins over a standard entry when it appears first and both match the
    same evidence (notably a custom FLAC resolution inside a standard bucket).
    Partial/inferred facts without a complete quality axis deliberately do not
    fabricate a recipe match.
    """
    if (
        not is_recipe_snapshot(snapshot)
        or evidence.certainty is not EvidenceCertainty.EXACT
    ):
        return None
    family = evidence.codec_family
    for index, entry in enumerate(snapshot.quality_recipe):
        if family is CodecFamily.LOSSY and evidence.extension.lower() == "mp3":
            if evidence.bitrate_kbps is None or entry.format != "mp3":
                continue
            if _recipe_interval_contains(entry, evidence.bitrate_kbps):
                return index, entry
            continue
        if (
            family is CodecFamily.LOSSLESS
            and evidence.extension.lower() == "flac"
            and evidence.bit_depth is not None
            and evidence.sample_rate_hz is not None
            and entry.format == "flac"
        ):
            if entry.quality == "custom" and (
                entry.bit_depth,
                entry.sample_rate_hz,
            ) == (evidence.bit_depth, evidence.sample_rate_hz):
                return index, entry
            detail = LOSSLESS_DETAIL_STEPS[
                lossless_detail_step(evidence.bit_depth, evidence.sample_rate_hz)
            ]
            if entry.quality == detail:
                return index, entry
    return None


def recipe_refinement_key(
    snapshot: AcquisitionQualitySnapshot, evidence: AudioQualityEvidence
) -> tuple:
    """Lower-is-better same-entry refinement key for v2 candidates."""
    match = recipe_entry_for_evidence(snapshot, evidence)
    if match is None:
        return (1, 10**9, 10**9, -CERTAINTY_RANK[evidence.certainty])
    _, entry = match
    if entry.format == "mp3":
        return (
            0,
            abs(evidence.bitrate_kbps - (entry.target_bitrate_kbps or 0)),
            -(evidence.bitrate_kbps or 0),
            -CERTAINTY_RANK[evidence.certainty],
        )
    detail_key = lossless_rank_key(snapshot, evidence)
    return (0, *detail_key, -CERTAINTY_RANK[evidence.certainty])


def derive_default_order(quality_min: str, quality_max: str) -> list[str]:
    """Existing-install migration shape: every accepted tier HIGHEST->LOWEST."""
    lo = _TIER_KEYS_BEST_FIRST.index(quality_max)
    hi = _TIER_KEYS_BEST_FIRST.index(quality_min)
    return list(_TIER_KEYS_BEST_FIRST[lo : hi + 1])


def normalize_order(order: list[str], quality_min: str, quality_max: str) -> list[str]:
    """Validate a submitted order: exactly the accepted contiguous range, once
    each, endpoints matching ``quality_max`` first and ``quality_min`` last
    (inclusion stays the min/max range; ordering cannot add/remove tiers).
    Raises ValueError - the caller surfaces a 400; never silently clamps."""
    expected = derive_default_order(quality_min, quality_max)
    if sorted(order) != sorted(expected) or len(order) != len(expected):
        raise ValueError(
            "quality_preference_order must contain exactly the accepted tiers "
            f"{expected} (min={quality_min}, max={quality_max}), got {order}"
        )
    # Order[0] is the MOST-PREFERRED tier - any permutation of the accepted
    # set is valid (e.g. '192-first' presets start at the lowest band).
    return list(order)


def _tier_label(key: str) -> str:
    return {
        "lossless": "lossless",
        "mp3_320": "lossy 320 kbps",
        "mp3_256": "lossy 256-319 kbps",
        "mp3_192": "lossy 192-255 kbps",
        "low": "lossy below 192 kbps",
    }[key]


def recipe_entry_legacy_tiers(entry: QualityRecipeEntry) -> set[str]:
    """Return canonical v1 tiers touched by one recipe entry for rollback."""
    if entry.format == "flac":
        return {"lossless"}
    if entry.quality == "below_192":
        return {"low"}
    if entry.quality == "192_255":
        return {"mp3_192"}
    if entry.quality == "256_319":
        return {"mp3_256"}
    if entry.quality == "320_plus":
        return {"mp3_320"}
    minimum = entry.min_bitrate_kbps or 16
    maximum = entry.max_bitrate_kbps
    upper = 2048 if maximum is None else maximum
    touched: set[str] = set()
    if minimum <= 191 and upper >= 16:
        touched.add("low")
    if minimum <= 255 and upper >= 192:
        touched.add("mp3_192")
    if minimum <= 319 and upper >= 256:
        touched.add("mp3_256")
    if upper >= 320:
        touched.add("mp3_320")
    return touched or {"low"}


def legacy_range_from_recipe(entries: list[QualityRecipeEntry]) -> tuple[str, str]:
    """Closest contiguous v1 range covering all v2 recipe entries."""
    if not entries:
        raise ValueError("cannot project an empty quality recipe")
    tiers = set().union(*(recipe_entry_legacy_tiers(entry) for entry in entries))
    ordered = list(reversed(_TIER_KEYS_BEST_FIRST))
    ranks = [ordered.index(tier) for tier in tiers]
    return ordered[min(ranks)], ordered[max(ranks)]


def legacy_recipe_order(entries: list[QualityRecipeEntry]) -> list[str]:
    """Project every tier in the derived contiguous range exactly once.

    Explicit recipe entries establish the strongest available ordering
    constraints. Missing tiers are inserted at their canonical positions
    between those constraints, so a custom 320-to-192 span becomes
    ``320, 256, 192`` instead of silently dropping 256.
    """
    if not entries:
        raise ValueError("cannot project an empty quality recipe")
    touched = set().union(*(recipe_entry_legacy_tiers(entry) for entry in entries))
    positions = [_TIER_KEYS_BEST_FIRST.index(tier) for tier in touched]
    first = min(positions)
    last = max(positions)
    contiguous = list(_TIER_KEYS_BEST_FIRST[first : last + 1])

    order: list[str] = []
    seen: set[str] = set()
    for entry in entries:
        entry_tiers = recipe_entry_legacy_tiers(entry)
        for tier in _TIER_KEYS_BEST_FIRST:
            if tier in entry_tiers and tier not in seen:
                order.append(tier)
                seen.add(tier)
    for tier in contiguous:
        if tier in seen:
            continue
        position = _TIER_KEYS_BEST_FIRST.index(tier)
        insertion = next(
            (
                index
                for index, existing in enumerate(order)
                if _TIER_KEYS_BEST_FIRST.index(existing) > position
            ),
            len(order),
        )
        order.insert(insertion, tier)
        seen.add(tier)
    return order


def compose_summary(snapshot: AcquisitionQualitySnapshot) -> str:
    """Compose the saved product contract shown to users."""
    if is_recipe_snapshot(snapshot):
        order = snapshot.quality_recipe
        sentence = f"Try {_recipe_entry_label(order[0])}"
        for entry in order[1:]:
            sentence += f", then {_recipe_entry_label(entry)}"
        sentence += "."
    else:
        order = snapshot.quality_preference_order
        if not order:
            return "No quality preference configured."
        sentence = f"Try {_tier_label(order[0])}"
        for tier in order[1:]:
            sentence += f", then {_tier_label(tier)}"
        sentence += "."
        pref = snapshot.lossless_preference
        if pref != "highest" and "lossless" in order and pref in LOSSLESS_DETAIL_STEPS:
            detail = {
                "cd": "CD-quality (16-bit/48 kHz)",
                "24_48": "up-to-24-bit/48 kHz",
                "24_96": "up-to-24-bit/96 kHz",
                "24_192": "up-to-24-bit/192 kHz",
            }.get(pref)
            if detail:
                sentence += f" Lossless prefers {detail} copies."
    cap_bits: list[str] = []
    if snapshot.lossless_max_bit_depth is not None:
        cap_bits.append(f"{snapshot.lossless_max_bit_depth}-bit maximum bit depth")
    if snapshot.lossless_max_sample_rate_hz is not None:
        cap_bits.append(
            f"{round(snapshot.lossless_max_sample_rate_hz / 1000)} kHz maximum sample rate"
        )
    if cap_bits:
        sentence += " Never acquire above " + " or ".join(cap_bits) + "."
    unknown = snapshot.unknown_quality_behavior
    if unknown == UnknownQualityBehavior.REVIEW.value:
        sentence += " Never acquire unknown-quality audio automatically."
    elif unknown == UnknownQualityBehavior.REJECT.value:
        sentence += " Unknown-quality copies are excluded."
    else:
        sentence += " Unknown-quality copies are a last resort."
    return sentence


class SnapshotValidationError(ValueError):
    """A persisted quality snapshot is absent, malformed, or tampered with."""


# Keep this order identical to the pre-recipe AcquisitionQualitySnapshot JSON
# shape. V1 rows are read and written through this explicit projection rather
# than msgspec's struct field order, so adding v2 fields cannot alter old blobs.
_SNAPSHOT_BASE_FIELDS: tuple[str, ...] = (
    "schema_version",
    "snapshot_hash",
    "quality_preference_order",
    "lossy_target_kbps",
    "lossy_min_bitrate_kbps",
    "lossy_max_bitrate_kbps",
    "lossless_preference",
    "lossless_max_bit_depth",
    "lossless_max_sample_rate_hz",
    "flac_mp3_only",
    "unknown_quality_behavior",
    "source_selection_mode",
    "summary",
    "origin",
)


_VALID_LOSSLESS_PREFERENCES = frozenset({"cd", "24_48", "24_96", "24_192", "highest"})
_VALID_UNKNOWN_BEHAVIORS = frozenset(value.value for value in UnknownQualityBehavior)
_VALID_SOURCE_MODES = frozenset(value.value for value in SourceSelectionMode)
_VALID_SNAPSHOT_ORIGINS = frozenset(value.value for value in SnapshotOrigin)


def _validate_snapshot_fields(
    snapshot: AcquisitionQualitySnapshot, *, require_hash: bool = False
) -> None:
    if type(snapshot.schema_version) is not int:
        raise SnapshotValidationError(
            "quality snapshot schema_version must be an integer"
        )
    if require_hash and (
        not isinstance(snapshot.snapshot_hash, str)
        or not snapshot.snapshot_hash
        or not re.fullmatch(r"[0-9a-f]{64}", snapshot.snapshot_hash)
    ):
        raise SnapshotValidationError("quality snapshot hash is malformed")
    if not isinstance(snapshot.quality_preference_order, list):
        raise SnapshotValidationError("quality snapshot preference order is malformed")
    if not isinstance(snapshot.quality_recipe, list):
        raise SnapshotValidationError("quality snapshot recipe is malformed")
    if any(
        type(tier) is not str or tier not in _TIER_KEYS_BEST_FIRST
        for tier in snapshot.quality_preference_order
    ) or len(set(snapshot.quality_preference_order)) != len(
        snapshot.quality_preference_order
    ):
        raise SnapshotValidationError("quality snapshot preference order is malformed")
    for name in (
        "lossy_target_kbps",
        "lossy_min_bitrate_kbps",
        "lossy_max_bitrate_kbps",
    ):
        value = getattr(snapshot, name)
        if value is not None and (type(value) is not int or not 16 <= value <= 2048):
            raise SnapshotValidationError(f"quality snapshot {name} is malformed")
    if (
        snapshot.lossy_min_bitrate_kbps is not None
        and snapshot.lossy_max_bitrate_kbps is not None
        and snapshot.lossy_min_bitrate_kbps > snapshot.lossy_max_bitrate_kbps
    ):
        raise SnapshotValidationError("quality snapshot lossy bounds are malformed")
    if snapshot.lossy_target_kbps is not None and (
        (
            snapshot.lossy_min_bitrate_kbps is not None
            and snapshot.lossy_target_kbps < snapshot.lossy_min_bitrate_kbps
        )
        or (
            snapshot.lossy_max_bitrate_kbps is not None
            and snapshot.lossy_target_kbps > snapshot.lossy_max_bitrate_kbps
        )
    ):
        raise SnapshotValidationError("quality snapshot lossy target is malformed")
    if not isinstance(snapshot.lossless_preference, str) or (
        snapshot.lossless_preference not in _VALID_LOSSLESS_PREFERENCES
    ):
        raise SnapshotValidationError(
            "quality snapshot lossless preference is malformed"
        )
    for name, low, high in (
        ("lossless_max_bit_depth", 1, 64),
        ("lossless_max_sample_rate_hz", 8000, 768000),
    ):
        value = getattr(snapshot, name)
        if value is not None and (type(value) is not int or not low <= value <= high):
            raise SnapshotValidationError(f"quality snapshot {name} is malformed")
    if type(snapshot.flac_mp3_only) is not bool:
        raise SnapshotValidationError("quality snapshot flac_mp3_only is malformed")
    if not isinstance(snapshot.unknown_quality_behavior, str) or (
        snapshot.unknown_quality_behavior not in _VALID_UNKNOWN_BEHAVIORS
    ):
        raise SnapshotValidationError("quality snapshot unknown rule is malformed")
    if not isinstance(snapshot.source_selection_mode, str) or (
        snapshot.source_selection_mode not in _VALID_SOURCE_MODES
    ):
        raise SnapshotValidationError("quality snapshot source mode is malformed")
    if (
        not isinstance(snapshot.summary, str)
        or not isinstance(snapshot.origin, str)
        or snapshot.origin not in _VALID_SNAPSHOT_ORIGINS
    ):
        raise SnapshotValidationError("quality snapshot display fields are malformed")


def _snapshot_payload(snapshot: AcquisitionQualitySnapshot) -> dict:
    _validate_snapshot_fields(snapshot)
    if snapshot.schema_version not in (
        SNAPSHOT_SCHEMA_VERSION,
        RECIPE_SNAPSHOT_SCHEMA_VERSION,
    ):
        raise SnapshotValidationError(
            f"unsupported quality snapshot schema {snapshot.schema_version!r}"
        )
    if snapshot.schema_version == SNAPSHOT_SCHEMA_VERSION:
        if snapshot.quality_recipe:
            raise SnapshotValidationError(
                "v1 quality snapshots cannot contain quality_recipe"
            )
    else:
        if not snapshot.quality_recipe:
            raise SnapshotValidationError(
                "v2 quality snapshots require a non-empty quality_recipe"
            )
        if snapshot.quality_preference_order:
            raise SnapshotValidationError(
                "v2 quality snapshots cannot contain a legacy preference order"
            )
        if not snapshot.flac_mp3_only:
            raise SnapshotValidationError("v2 quality snapshots require flac_mp3_only")
        validate_quality_recipe(list(snapshot.quality_recipe))

    payload = {
        field: msgspec.to_builtins(getattr(snapshot, field))
        for field in _SNAPSHOT_BASE_FIELDS
    }
    if snapshot.schema_version == RECIPE_SNAPSHOT_SCHEMA_VERSION:
        # Place this directly after the legacy order, matching the model's
        # intentional v2 extension while leaving every v1 byte untouched.
        payload = {
            **{field: payload[field] for field in _SNAPSHOT_BASE_FIELDS[:3]},
            "quality_recipe": [
                msgspec.to_builtins(entry) for entry in snapshot.quality_recipe
            ],
            **{field: payload[field] for field in _SNAPSHOT_BASE_FIELDS[3:]},
        }
    return payload


def snapshot_policy_hash(snapshot: AcquisitionQualitySnapshot) -> str:
    """Stable hash over normalized policy inputs.

    The v1 payload is intentionally byte-for-byte compatible with the previous
    implementation. Recipe snapshots add the canonical ordered entries and use
    schema version 2.
    """
    payload = _snapshot_payload(snapshot)
    hash_payload = {
        key: payload[key]
        for key in payload
        if key not in {"snapshot_hash", "summary", "origin"}
    }
    canonical = json.dumps(hash_payload, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def validate_snapshot(
    snapshot: AcquisitionQualitySnapshot,
) -> AcquisitionQualitySnapshot:
    """Validate schema, recipe shape, and the immutable policy hash."""
    if not isinstance(snapshot, AcquisitionQualitySnapshot):
        raise SnapshotValidationError("quality snapshot has the wrong type")
    _validate_snapshot_fields(snapshot, require_hash=True)
    try:
        _snapshot_payload(snapshot)
        expected_hash = snapshot_policy_hash(snapshot)
    except (SnapshotValidationError, TypeError, ValueError) as exc:
        if isinstance(exc, SnapshotValidationError):
            raise
        raise SnapshotValidationError("quality snapshot validation failed") from exc
    if not snapshot.snapshot_hash or snapshot.snapshot_hash != expected_hash:
        raise SnapshotValidationError("quality snapshot hash mismatch")
    return snapshot


def encode_snapshot(snapshot: AcquisitionQualitySnapshot) -> str:
    """Encode a validated snapshot using the stable v1/v2 persistence shape."""
    validate_snapshot(snapshot)
    return json.dumps(_snapshot_payload(snapshot))


def decode_snapshot(
    raw: str | bytes | bytearray | memoryview,
) -> AcquisitionQualitySnapshot:
    """Decode and validate one persisted snapshot; never falls back to live policy."""
    if raw is None:
        raise SnapshotValidationError("quality snapshot is absent")
    try:
        data = raw.encode("utf-8") if isinstance(raw, str) else bytes(raw)
        value = msgspec.json.decode(data, type=dict[str, object])
        if not isinstance(value, dict):
            raise SnapshotValidationError("quality snapshot must be a JSON object")
        schema = value.get("schema_version")
        expected = set(_SNAPSHOT_BASE_FIELDS)
        if schema == RECIPE_SNAPSHOT_SCHEMA_VERSION:
            expected.add("quality_recipe")
        elif schema != SNAPSHOT_SCHEMA_VERSION:
            raise SnapshotValidationError(
                f"unsupported quality snapshot schema {schema!r}"
            )
        if set(value) != expected:
            raise SnapshotValidationError("quality snapshot has an invalid field set")
        snapshot = msgspec.convert(value, type=AcquisitionQualitySnapshot, strict=True)
        return validate_snapshot(snapshot)
    except SnapshotValidationError:
        raise
    except (TypeError, ValueError, msgspec.MsgspecError) as exc:
        raise SnapshotValidationError("quality snapshot decode failed") from exc


def build_snapshot(policy: "DownloadPolicySettings") -> AcquisitionQualitySnapshot:
    """Normalize live policy into an immutable v1 or recipe-bearing v2 snapshot."""
    submitted = getattr(policy, "quality_preference_order", None)
    raw_recipe = getattr(policy, "quality_recipe", None) or []
    recipe = []
    # A legacy policy that permits codecs outside the closed v2 set remains
    # v1-compatible; never silently project it by dropping those codecs.
    if raw_recipe and getattr(policy, "flac_mp3_only", True):
        recipe = validate_quality_recipe(
            [
                entry
                if isinstance(entry, QualityRecipeEntry)
                else msgspec.convert(entry, type=QualityRecipeEntry, strict=True)
                for entry in list(raw_recipe)
            ]
        )
    order = (
        list(submitted)
        if submitted
        else derive_default_order(policy.quality_min, policy.quality_max)
    )
    snapshot = AcquisitionQualitySnapshot(
        schema_version=(
            RECIPE_SNAPSHOT_SCHEMA_VERSION if recipe else SNAPSHOT_SCHEMA_VERSION
        ),
        quality_preference_order=[] if recipe else order,
        quality_recipe=recipe,
        lossy_target_kbps=policy.preferred_lossy_bitrate_kbps,
        lossy_min_bitrate_kbps=policy.lossy_min_bitrate_kbps,
        lossy_max_bitrate_kbps=policy.lossy_max_bitrate_kbps,
        lossless_preference=policy.lossless_preference,
        lossless_max_bit_depth=policy.lossless_max_bit_depth,
        lossless_max_sample_rate_hz=policy.lossless_max_sample_rate_hz,
        flac_mp3_only=policy.flac_mp3_only,
        unknown_quality_behavior=policy.unknown_quality_behavior,
        source_selection_mode=policy.source_selection_mode,
    )
    snapshot.snapshot_hash = snapshot_policy_hash(snapshot)
    snapshot.summary = compose_summary(snapshot)
    return snapshot


def migration_snapshot(policy: "DownloadPolicySettings") -> AcquisitionQualitySnapshot:
    """Same normalization, tagged ``legacy_migration`` for the startup backfill."""
    snapshot = build_snapshot(policy)
    snapshot.origin = SnapshotOrigin.LEGACY_MIGRATION.value
    return snapshot


def project_canonical_tier(evidence: AudioQualityEvidence) -> str | None:
    """Canonical five-tier projection. Returns None ONLY for family-unknown
    evidence (which never becomes a fake tier). Proven lossy WITHOUT a bitrate
    keeps the legacy ``low`` projection (spec: never promoted through the
    family-unknown rule)."""
    family = evidence.codec_family
    if family is CodecFamily.LOSSLESS:
        return "lossless"
    if family is CodecFamily.LOSSY:
        rate = evidence.bitrate_kbps
        if rate is None:
            return "low"
        for floor, key in _LOSSY_BANDS:
            if rate >= floor:
                return key
        return "low"
    return None


def preference_step_from_tier(
    snapshot: AcquisitionQualitySnapshot, tier: str
) -> int | None:
    """0-based index in the snapshot order; None when outside policy."""
    try:
        return snapshot.quality_preference_order.index(tier)
    except ValueError:
        return None


def lossless_target_index(snapshot: AcquisitionQualitySnapshot) -> int | None:
    """Ladder index of the configured target resolution; None under ``highest``
    (which keeps today's descending hi-res-first order untouched)."""
    pref = snapshot.lossless_preference
    if pref == "highest":
        return None
    try:
        return LOSSLESS_DETAIL_STEPS.index(pref)
    except ValueError:
        return None


def lossless_rank_key(
    snapshot: AcquisitionQualitySnapshot, evidence: AudioQualityEvidence
) -> tuple:
    """Within-``lossless`` comparator under TARGET modes: candidates on the
    configured detail step rank first, then successively more-detailed known
    steps, then lesser-known steps, then partial/unknown - i.e. the fixed
    ladder walked upward from the target. Under ``highest`` (None target) this
    returns the legacy descending ``(min-depth, min-rate)`` axes unchanged."""
    target_idx = lossless_target_index(snapshot)
    step = lossless_detail_step(evidence.bit_depth, evidence.sample_rate_hz)
    partial_step = len(LOSSLESS_DETAIL_STEPS) - 1
    if target_idx is None:
        return (0,) + tuple(
            -axis
            for axis in detail_comparator_axes(
                evidence.bit_depth, evidence.sample_rate_hz
            )
        )
    if step >= partial_step:
        # partial/unknown resolution sorts dead last among lossless candidates
        return (1, partial_step, 0)
    upward = [i for i in range(target_idx, partial_step)] + [
        i for i in range(0, target_idx)
    ]
    return (0, upward.index(step), 0)


def lossy_target_distance(
    snapshot: AcquisitionQualitySnapshot, evidence: AudioQualityEvidence
) -> int | None:
    """|distance to the preferred lossy bitrate| for ONE file (callers fold the
    per-file max across a folder). None when not provably lossy, bitrate
    unknown, or no target configured."""
    if (
        evidence.codec_family is not CodecFamily.LOSSY
        or evidence.bitrate_kbps is None
        or snapshot.lossy_target_kbps is None
    ):
        return None
    return abs(evidence.bitrate_kbps - snapshot.lossy_target_kbps)


# Worst-first ranks used to fold folder decisions.
_NOT_IMPORTABLE_RANK = 100
_REJECTED_RANK = 90
_NEEDS_REVIEW_RANK = 80


def _certainty_floor(certainties):
    return min(certainties, key=lambda c: CERTAINTY_RANK[c])


def _mixed_quality(
    evidences: list[AudioQualityEvidence], projected_families: set[object]
) -> bool:
    """Return whether one folder contains incompatible quality axes.

    A canonical tier alone cannot distinguish, for example, two FLAC resolutions
    that both map to ``lossless`` or two MP3 bitrates inside one recipe band.
    Missing and present axes are intentionally different values: combining them
    must not manufacture a complete exact claim.
    """
    if len(projected_families) > 1 or any(e.mixed_quality for e in evidences):
        return True
    codec_families = {e.codec_family for e in evidences}
    if len(codec_families) > 1:
        return True
    if codec_families == {CodecFamily.LOSSY}:
        axes = {(e.bitrate_kbps,) for e in evidences}
    elif codec_families == {CodecFamily.LOSSLESS}:
        axes = {(e.bit_depth, e.sample_rate_hz) for e in evidences}
    else:
        axes = {(e.bitrate_kbps, e.bit_depth, e.sample_rate_hz) for e in evidences}
    return len(axes) > 1


def _recipe_unknown_decision(
    snapshot: AcquisitionQualitySnapshot,
    evidence: AudioQualityEvidence,
    *,
    tier: str | None = None,
    mixed: bool = False,
) -> QualityDecision:
    reason = [QualityReason.OUTSIDE_GLOBAL_PREFERENCE]
    if mixed:
        summary = "Mixed format/quality copy has no single preferred recipe entry."
    else:
        summary = "Quality could not be mapped to an explicit recipe entry."
    rule = snapshot.unknown_quality_behavior
    if rule == UnknownQualityBehavior.REJECT.value:
        return QualityDecision(
            eligible=False,
            disposition="unknown_rejected",
            tier=tier,
            evidence=evidence,
            reasons=reason,
            summary=summary + " The server policy excludes it.",
        )
    if rule == UnknownQualityBehavior.REVIEW.value:
        return QualityDecision(
            eligible=False,
            disposition="needs_review",
            tier=tier,
            evidence=evidence,
            reasons=[] if mixed else reason,
            summary=summary + " Review required.",
        )
    return QualityDecision(
        eligible=True,
        disposition="fallback",
        tier=tier,
        preference_step=len(snapshot.quality_recipe),
        evidence=evidence,
        reasons=reason,
        summary=summary + " Held as a last-resort fallback.",
    )


def _evaluate_recipe(
    snapshot: AcquisitionQualitySnapshot, evidence: AudioQualityEvidence
) -> QualityDecision:
    ext = (evidence.extension or "").lower().lstrip(".")
    if ext in NOT_IMPORTABLE_EXTENSIONS:
        return QualityDecision(
            eligible=False,
            disposition="not_importable",
            evidence=evidence,
            reasons=[QualityReason.FORMAT_NOT_IMPORTABLE],
            summary=f"{ext.upper()} is not an importable audio format.",
        )
    # Closed recipes reject known alternate containers before any weak family
    # evidence can route them through the configured unknown fallback. A fully
    # exact but opaque extension is equally unsafe; an absent/opaque partial
    # extension still follows the explicit unknown-quality rule.
    if ext not in {"", *RECIPE_FORMATS} and (
        ext in KNOWN_AUDIO_EXTENSIONS or evidence.certainty is EvidenceCertainty.EXACT
    ):
        return QualityDecision(
            eligible=False,
            disposition="not_importable",
            evidence=evidence,
            reasons=[QualityReason.FORMAT_OUTSIDE_POLICY],
            summary=f"{ext.upper()} is not included in the closed FLAC/MP3 recipe.",
        )
    tier = project_canonical_tier(evidence)
    # A recipe entry is an automatic claim about quality. Only exact evidence
    # may make that claim; partial/inferred/unknown evidence follows the
    # configured reject/review/last-resort rule instead.
    if evidence.certainty is not EvidenceCertainty.EXACT:
        return _recipe_unknown_decision(snapshot, evidence, tier=tier)
    match = recipe_entry_for_evidence(snapshot, evidence)
    if match is None:
        # A complete, proven format/quality with no explicit entry is a hard
        # recipe rejection. Partial axes and unknown family retain the existing
        # reject/review/last-resort policy rule instead.
        complete_quality = (
            evidence.codec_family is CodecFamily.LOSSY
            and ext == "mp3"
            and evidence.bitrate_kbps is not None
        ) or (
            evidence.codec_family is CodecFamily.LOSSLESS
            and ext == "flac"
            and evidence.bit_depth is not None
            and evidence.sample_rate_hz is not None
        )
        if not complete_quality:
            return _recipe_unknown_decision(snapshot, evidence, tier=tier)
        return QualityDecision(
            eligible=False,
            disposition="outside_policy",
            tier=tier,
            evidence=evidence,
            reasons=[QualityReason.FORMAT_OUTSIDE_POLICY],
            summary="Known quality is not included in the acquisition recipe.",
        )

    index, entry = match
    reasons: list[QualityReason] = []
    detail = (
        lossless_detail_step(evidence.bit_depth, evidence.sample_rate_hz)
        if entry.format == "flac"
        else None
    )
    if entry.format == "flac":
        depth_known = evidence.bit_depth is not None
        rate_known = evidence.sample_rate_hz is not None
        # A match requires both axes, but retain the existing cap semantics if
        # this helper is called with a hand-built non-canonical snapshot.
        depth_violation = (
            snapshot.lossless_max_bit_depth is not None
            and depth_known
            and evidence.bit_depth > snapshot.lossless_max_bit_depth
        )
        rate_violation = (
            snapshot.lossless_max_sample_rate_hz is not None
            and rate_known
            and evidence.sample_rate_hz > snapshot.lossless_max_sample_rate_hz
        )
        if depth_violation or rate_violation:
            bits: list[str] = []
            if depth_violation:
                bits.append(f"{evidence.bit_depth}-bit")
            if rate_violation:
                bits.append(f"{evidence.sample_rate_hz} Hz")
            return QualityDecision(
                eligible=False,
                disposition="outside_policy",
                tier=tier,
                preference_step=index,
                quality_recipe_index=index,
                quality_recipe_entry=entry,
                lossless_detail_step=detail,
                evidence=evidence,
                reasons=[QualityReason.LOSSLESS_RESOLUTION_ABOVE_MAXIMUM],
                summary="FLAC copy exceeds the server limit ("
                + " / ".join(bits)
                + ").",
            )
    else:
        bitrate = evidence.bitrate_kbps
        assert bitrate is not None
        if (
            snapshot.lossy_min_bitrate_kbps is not None
            and bitrate < snapshot.lossy_min_bitrate_kbps
        ):
            return QualityDecision(
                eligible=False,
                disposition="outside_policy",
                tier=tier,
                preference_step=index,
                quality_recipe_index=index,
                quality_recipe_entry=entry,
                evidence=evidence,
                reasons=[QualityReason.LOSSY_BITRATE_BELOW_MINIMUM],
                summary=(
                    f"{bitrate} kbps is below the server minimum "
                    f"({snapshot.lossy_min_bitrate_kbps} kbps)."
                ),
            )
        if (
            snapshot.lossy_max_bitrate_kbps is not None
            and bitrate > snapshot.lossy_max_bitrate_kbps
        ):
            return QualityDecision(
                eligible=False,
                disposition="outside_policy",
                tier=tier,
                preference_step=index,
                quality_recipe_index=index,
                quality_recipe_entry=entry,
                evidence=evidence,
                reasons=[QualityReason.LOSSY_BITRATE_ABOVE_MAXIMUM],
                summary=(
                    f"{bitrate} kbps is above the server maximum "
                    f"({snapshot.lossy_max_bitrate_kbps} kbps)."
                ),
            )
    if evidence.provenance is EvidenceProvenance.RELEASE_TITLE:
        reasons.append(QualityReason.QUALITY_INFERRED_FROM_TITLE)
    elif evidence.provenance is EvidenceProvenance.CATEGORY:
        reasons.append(QualityReason.QUALITY_INFERRED_FROM_CATEGORY)
    reasons.insert(
        0,
        QualityReason.PREFERRED_TIER if index == 0 else QualityReason.FALLBACK_TIER,
    )
    label = _recipe_entry_label(entry)
    if entry.format == "mp3" and evidence.bitrate_kbps is not None:
        label += f" ({evidence.bitrate_kbps} kbps)"
    elif entry.format == "flac":
        label += (
            f" ({evidence.bit_depth}-bit/{round(evidence.sample_rate_hz / 1000)} kHz)"
        )
    return QualityDecision(
        eligible=True,
        disposition="preferred" if index == 0 else "fallback",
        tier=tier,
        preference_step=index,
        quality_recipe_index=index,
        quality_recipe_entry=entry,
        lossless_detail_step=detail,
        evidence=evidence,
        reasons=reasons,
        summary=label + ("" if index == 0 else f" - fallback {index}") + ".",
    )


def evaluate(
    snapshot: AcquisitionQualitySnapshot, evidence: AudioQualityEvidence
) -> QualityDecision:
    """Full eligibility + preference evaluation of ONE unit of evidence."""
    if is_recipe_snapshot(snapshot):
        return _evaluate_recipe(snapshot, evidence)
    reasons: list[QualityReason] = []

    ext = (evidence.extension or "").lower()
    if ext in NOT_IMPORTABLE_EXTENSIONS:
        reasons.insert(0, QualityReason.FORMAT_NOT_IMPORTABLE)
        return QualityDecision(
            eligible=False,
            disposition="not_importable",
            evidence=evidence,
            reasons=reasons,
            summary=f"{ext.upper()} is not an importable audio format.",
        )

    family = evidence.codec_family
    if family is CodecFamily.UNKNOWN:
        rule = snapshot.unknown_quality_behavior
        if rule == UnknownQualityBehavior.REJECT.value:
            return QualityDecision(
                eligible=False,
                disposition="unknown_rejected",
                evidence=evidence,
                reasons=[QualityReason.OUTSIDE_GLOBAL_PREFERENCE],
                summary=(
                    "Quality could not be determined; the server policy excludes it."
                ),
            )
        if rule == UnknownQualityBehavior.REVIEW.value:
            return QualityDecision(
                eligible=False,
                disposition="needs_review",
                evidence=evidence,
                summary=(
                    "Only unknown-resolution copies were found - review required."
                ),
            )
        # allow_as_fallback: strictly AFTER every known eligible step.
        return QualityDecision(
            eligible=True,
            disposition="fallback",
            preference_step=len(snapshot.quality_preference_order),
            evidence=evidence,
            reasons=[QualityReason.OUTSIDE_GLOBAL_PREFERENCE],
            summary="Unknown-quality copy held as a last-resort fallback.",
        )

    tier = project_canonical_tier(evidence)
    # Proven lossy without a reported bitrate ALWAYS carries this display
    # code - even when its legacy ``low`` projection lands outside the range.
    if family is CodecFamily.LOSSY and evidence.bitrate_kbps is None:
        reasons.append(QualityReason.LOSSY_BITRATE_UNKNOWN)

    step = preference_step_from_tier(snapshot, tier)
    if step is None:
        return QualityDecision(
            eligible=False,
            disposition="outside_policy",
            tier=tier,
            evidence=evidence,
            reasons=reasons or [QualityReason.OUTSIDE_GLOBAL_PREFERENCE],
            summary=f"{_tier_label(tier).capitalize()} is outside the accepted quality.",
        )

    detail = None
    if tier == "lossless":
        detail = lossless_detail_step(evidence.bit_depth, evidence.sample_rate_hz)
        depth_known = evidence.bit_depth is not None
        rate_known = evidence.sample_rate_hz is not None
        if depth_known != rate_known:
            reasons.append(QualityReason.LOSSLESS_RESOLUTION_PARTIAL)
        elif not depth_known and not rate_known:
            reasons.append(QualityReason.LOSSLESS_RESOLUTION_UNKNOWN)
        # Caps reject only PROVABLE violations (a KNOWN axis exceeding the cap).
        depth_violation = (
            snapshot.lossless_max_bit_depth is not None
            and depth_known
            and evidence.bit_depth > snapshot.lossless_max_bit_depth
        )
        rate_violation = (
            snapshot.lossless_max_sample_rate_hz is not None
            and rate_known
            and evidence.sample_rate_hz > snapshot.lossless_max_sample_rate_hz
        )
        if depth_violation or rate_violation:
            reasons.insert(0, QualityReason.LOSSLESS_RESOLUTION_ABOVE_MAXIMUM)
            bits: list[str] = []
            if depth_violation:
                bits.append(f"{evidence.bit_depth}-bit")
            if rate_violation:
                bits.append(f"{evidence.sample_rate_hz} Hz")
            return QualityDecision(
                eligible=False,
                disposition="outside_policy",
                tier=tier,
                lossless_detail_step=detail,
                evidence=evidence,
                reasons=reasons,
                summary=(
                    "Hi-res copy exceeds the server limit (" + " / ".join(bits) + ")."
                ),
            )
        target_idx = lossless_target_index(snapshot)
        claimed_exact = detail < len(LOSSLESS_DETAIL_STEPS) - 1 and all(
            r
            not in (
                QualityReason.LOSSLESS_RESOLUTION_PARTIAL,
                QualityReason.LOSSLESS_RESOLUTION_UNKNOWN,
            )
            for r in reasons
        )
        if target_idx is not None and claimed_exact and detail == target_idx:
            reasons.append(QualityReason.PREFERRED_LOSSLESS_RESOLUTION)

    if tier != "lossless" and evidence.bitrate_kbps is not None:
        if (
            snapshot.lossy_min_bitrate_kbps is not None
            and evidence.bitrate_kbps < snapshot.lossy_min_bitrate_kbps
        ):
            reasons.insert(0, QualityReason.LOSSY_BITRATE_BELOW_MINIMUM)
            return QualityDecision(
                eligible=False,
                disposition="outside_policy",
                tier=tier,
                evidence=evidence,
                reasons=reasons,
                summary=(
                    f"{evidence.bitrate_kbps} kbps is below the server minimum "
                    f"({snapshot.lossy_min_bitrate_kbps} kbps)."
                ),
            )
        if (
            snapshot.lossy_max_bitrate_kbps is not None
            and evidence.bitrate_kbps > snapshot.lossy_max_bitrate_kbps
        ):
            reasons.insert(0, QualityReason.LOSSY_BITRATE_ABOVE_MAXIMUM)
            return QualityDecision(
                eligible=False,
                disposition="outside_policy",
                tier=tier,
                evidence=evidence,
                reasons=reasons,
                summary=(
                    f"{evidence.bitrate_kbps} kbps is above the server maximum "
                    f"({snapshot.lossy_max_bitrate_kbps} kbps)."
                ),
            )
        if (
            snapshot.lossy_target_kbps is not None
            and evidence.bitrate_kbps == snapshot.lossy_target_kbps
        ):
            reasons.append(QualityReason.PREFERRED_LOSSY_BITRATE)

    if evidence.provenance is EvidenceProvenance.RELEASE_TITLE:
        reasons.append(QualityReason.QUALITY_INFERRED_FROM_TITLE)
    elif evidence.provenance is EvidenceProvenance.CATEGORY:
        reasons.append(QualityReason.QUALITY_INFERRED_FROM_CATEGORY)

    if step == 0:
        reasons.insert(0, QualityReason.PREFERRED_TIER)
    else:
        reasons.insert(0, QualityReason.FALLBACK_TIER)

    label_bits: list[str] = [_tier_label(tier)]
    if tier != "lossless" and evidence.bitrate_kbps is not None:
        label_bits.append(f"{evidence.bitrate_kbps} kbps")
    if tier == "lossless" and evidence.bit_depth is not None:
        rate = evidence.sample_rate_hz
        label_bits.append(
            f"{evidence.bit_depth}-bit/{round(rate / 1000)} kHz"
            if rate
            else f"{evidence.bit_depth}-bit"
        )
    return QualityDecision(
        eligible=True,
        disposition="preferred" if step == 0 else "fallback",
        tier=tier,
        preference_step=step,
        lossless_detail_step=detail,
        evidence=evidence,
        reasons=reasons,
        summary=", ".join(label_bits)
        + ("" if step == 0 else f" - fallback {step}")
        + ".",
    )


def _decision_rank(decision: QualityDecision) -> int:
    if decision.disposition == "not_importable":
        return _NOT_IMPORTABLE_RANK
    if decision.eligible:
        step = decision.preference_step
        return 10 + (step if step is not None else 9999)
    if decision.disposition == "needs_review":
        return _NEEDS_REVIEW_RANK
    return _REJECTED_RANK


def is_hard_quality_rejection(decision: QualityDecision) -> bool:
    return decision.disposition == "not_importable" or any(
        reason
        in {
            QualityReason.FORMAT_NOT_IMPORTABLE,
            QualityReason.LOSSLESS_RESOLUTION_ABOVE_MAXIMUM,
            QualityReason.LOSSY_BITRATE_BELOW_MINIMUM,
            QualityReason.LOSSY_BITRATE_ABOVE_MAXIMUM,
        }
        for reason in decision.reasons
    )


def evaluate_worst(
    snapshot: AcquisitionQualitySnapshot, evidences: list[AudioQualityEvidence]
) -> QualityDecision:
    """Fold per-file decisions into ONE folder verdict.

    V1 retains its historical worst-file decision. V2 additionally treats a
    folder spanning multiple recipe entries or heterogeneous formats/quality
    as one unknown mapping and applies the configured reject/review/fallback
    rule to the merged evidence, unless a hard safety rejection wins first.
    """
    if not evidences:
        raise ValueError("evaluate_worst requires at least one evidence item")
    decisions = [evaluate(snapshot, ev) for ev in evidences]
    worst = max(decisions, key=_decision_rank)

    extensions = {e.extension.lower() for e in evidences if e.extension}
    families = {project_canonical_tier(e) or e.codec_family.value for e in evidences}
    depths = [e.bit_depth for e in evidences if e.bit_depth is not None]
    rates = [e.sample_rate_hz for e in evidences if e.sample_rate_hz is not None]
    merged_evidence = AudioQualityEvidence(
        extension=worst.evidence.extension,
        codec_family=worst.evidence.codec_family,
        bitrate_kbps=None
        if any(
            e.codec_family is CodecFamily.LOSSY and e.bitrate_kbps is None
            for e in evidences
        )
        else worst.evidence.bitrate_kbps,
        bit_depth=min(depths) if depths else None,
        sample_rate_hz=min(rates) if rates else None,
        total_bytes=sum((e.total_bytes or 0) for e in evidences) or None,
        audio_file_count=sum(e.audio_file_count or 1 for e in evidences),
        mixed_format=len(extensions) > 1 or any(e.mixed_format for e in evidences),
        mixed_quality=_mixed_quality(evidences, families),
        certainty=_certainty_floor([e.certainty for e in evidences]),
        provenance=max((e.provenance for e in evidences), key=_PROVENANCE_ORDER.index),
    )

    if is_recipe_snapshot(snapshot):
        matched_entries = {
            decision.quality_recipe_index
            for decision in decisions
            if decision.quality_recipe_index is not None
        }
        mixed_recipe = (
            len(matched_entries) > 1
            or merged_evidence.mixed_format
            or merged_evidence.mixed_quality
        )
        if mixed_recipe and not any(is_hard_quality_rejection(d) for d in decisions):
            return _recipe_unknown_decision(
                snapshot,
                merged_evidence,
                tier=project_canonical_tier(merged_evidence),
                mixed=True,
            )

    return QualityDecision(
        eligible=worst.eligible,
        disposition=worst.disposition,
        tier=worst.tier,
        preference_step=worst.preference_step,
        quality_recipe_index=worst.quality_recipe_index,
        quality_recipe_entry=worst.quality_recipe_entry,
        lossless_detail_step=worst.lossless_detail_step,
        evidence=merged_evidence,
        reasons=list(worst.reasons),
        summary=worst.summary,
    )


def preference_step(
    snapshot: AcquisitionQualitySnapshot, evidence: AudioQualityEvidence
) -> int | None:
    """Cheap stable-step lookup mirroring :func:`evaluate`. None = outside
    policy / needs-review / rejected; family-unknown fallback sits at
    ``len(order)`` - strictly after every known step."""
    return evaluate(snapshot, evidence).preference_step


def evidence_from_archive_format(format_string: str) -> AudioQualityEvidence:
    """Archive.org ``format`` strings are pre-fetch evidence (partial at best).

    Live shapes (verified against archive advancedsearch results): ``Flac``,
    ``24bit Flac`` (proves 24-bit-class depth, violating any 16-bit cap even
    with an unknown sample rate), ``MP3`` / ``VBR MP3`` (proven lossy FAMILY,
    unknown pre-fetch bitrate - keeps the legacy low projection, never promoted
    through the family-unknown rule), ``Ogg Vorbis`` (lossy, unknown bitrate).
    """
    lowered = (format_string or "").strip().lower()
    if not lowered:
        return AudioQualityEvidence()
    depth = 24 if ("24bit" in lowered or "24-bit" in lowered) else None
    if "flac" in lowered:
        return AudioQualityEvidence(
            extension="flac",
            codec_family=CodecFamily.LOSSLESS,
            bit_depth=depth,
            certainty=EvidenceCertainty.PARTIAL,
            provenance=EvidenceProvenance.ARCHIVE_FORMAT,
        )
    if "mp3" in lowered:
        return AudioQualityEvidence(
            extension="mp3",
            codec_family=CodecFamily.LOSSY,
            certainty=EvidenceCertainty.PARTIAL,
            provenance=EvidenceProvenance.ARCHIVE_FORMAT,
        )
    if "vorbis" in lowered or lowered.startswith("ogg"):
        return AudioQualityEvidence(
            extension="ogg",
            codec_family=CodecFamily.LOSSY,
            certainty=EvidenceCertainty.PARTIAL,
            provenance=EvidenceProvenance.ARCHIVE_FORMAT,
        )
    if re.match(r"^24[\s\-]?bit", lowered):
        # Bare "24bit" claim on another container: depth-proven, family unclear.
        return AudioQualityEvidence(
            codec_family=CodecFamily.UNKNOWN,
            bit_depth=depth,
            certainty=EvidenceCertainty.UNKNOWN,
            provenance=EvidenceProvenance.ARCHIVE_FORMAT,
        )
    return AudioQualityEvidence(
        extension=lowered.split()[0].strip("."),
        codec_family=CodecFamily.UNKNOWN,
        certainty=EvidenceCertainty.UNKNOWN,
        provenance=EvidenceProvenance.ARCHIVE_FORMAT,
    )


__all__ = [
    "CERTAINTY_RANK",
    "FLAC_RECIPE_QUALITIES",
    "KNOWN_AUDIO_EXTENSIONS",
    "LOSSLESS_DETAIL_STEPS",
    "MP3_RECIPE_QUALITIES",
    "NOT_IMPORTABLE_EXTENSIONS",
    "RECIPE_SNAPSHOT_SCHEMA_VERSION",
    "SNAPSHOT_SCHEMA_VERSION",
    "AcquisitionQualitySnapshot",
    "AudioQualityEvidence",
    "CodecFamily",
    "EvidenceCertainty",
    "EvidenceProvenance",
    "QualityDecision",
    "QualityReason",
    "QualityRecipeEntry",
    "SnapshotOrigin",
    "SnapshotValidationError",
    "UnknownQualityBehavior",
    "build_snapshot",
    "compose_summary",
    "decode_snapshot",
    "derive_default_order",
    "detail_comparator_axes",
    "encode_snapshot",
    "legacy_range_from_recipe",
    "legacy_recipe_order",
    "recipe_entry_legacy_tiers",
    "evidence_from_archive_format",
    "evaluate",
    "evaluate_worst",
    "is_hard_quality_rejection",
    "lossless_detail_step",
    "lossless_rank_key",
    "lossless_target_index",
    "lossy_target_distance",
    "migration_snapshot",
    "normalize_order",
    "preference_step",
    "preference_step_from_tier",
    "project_canonical_tier",
    "recipe_entry_for_evidence",
    "recipe_refinement_key",
    "snapshot_policy_hash",
    "validate_snapshot",
]
