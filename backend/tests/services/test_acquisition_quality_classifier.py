"""Unit tests for the shared acquisition-quality classifier and evidence model.

These pin the NEW semantics from .dev-notes/Plans/Acquisition.md: snapshot
normalization (incl. the existing-install derivation), preference steps,
lossy bounds/target ordering inputs, lossless caps rejecting only provable
violations, unknown-evidence rules, mixed-folder worst-step folding, Archive
format evidence, and hash stability.
"""

import msgspec
import pytest
from types import SimpleNamespace as NS

from models.acquisition_quality import (
    AudioQualityEvidence as EV,
    AcquisitionQualitySnapshot,
    CodecFamily as F,
    EvidenceCertainty as C,
    EvidenceProvenance as P,
    QualityDecision,
    QualityReason as QR,
    QualityRecipeEntry,
    lossless_detail_step,
)
from services.native.acquisition.quality import (
    build_snapshot,
    compose_summary,
    derive_default_order,
    detail_comparator_axes,
    evaluate,
    evaluate_worst,
    evidence_from_archive_format,
    lossless_rank_key,
    migration_snapshot,
    normalize_order,
    preference_step,
    snapshot_policy_hash,
)


def policy(**overrides):
    base = dict(
        quality_min="mp3_320",
        quality_max="lossless",
        quality_preference_order=[],
        preferred_lossy_bitrate_kbps=None,
        lossy_min_bitrate_kbps=None,
        lossy_max_bitrate_kbps=None,
        lossless_preference="highest",
        lossless_max_bit_depth=None,
        lossless_max_sample_rate_hz=None,
        flac_mp3_only=True,
        unknown_quality_behavior="allow_as_fallback",
        source_selection_mode="source_first",
    )
    base.update(overrides)
    return NS(**base)


BALANCED = dict(
    quality_min="mp3_192",
    quality_preference_order=["lossless", "mp3_320", "mp3_256", "mp3_192"],
    preferred_lossy_bitrate_kbps=320,
    lossless_preference="cd",
    lossless_max_bit_depth=16,
    lossless_max_sample_rate_hz=48000,
    unknown_quality_behavior="review",
)


# ---------------------------------------------------------------- snapshot


def test_existing_install_migration_derives_highest_first_order():
    snap = build_snapshot(policy())  # legacy default range [mp3_320..lossless]
    assert snap.quality_preference_order == ["lossless", "mp3_320"]
    assert snap.origin == "global_policy"
    assert len(snap.snapshot_hash) == 64
    assert "lossless" in compose_summary(snap)


def test_derive_default_order_contiguous_and_descending():
    assert derive_default_order("low", "lossless") == [
        "lossless",
        "mp3_320",
        "mp3_256",
        "mp3_192",
        "low",
    ]
    assert derive_default_order("mp3_192", "mp3_320") == [
        "mp3_320",
        "mp3_256",
        "mp3_192",
    ]


def test_normalize_order_rejects_missing_extra_and_wrong_endpoints():
    ok = ["lossless", "mp3_320", "mp3_256", "mp3_192"]
    assert normalize_order(ok, "mp3_192", "lossless") == ok
    with pytest.raises(ValueError):
        normalize_order(["lossless"], "mp3_192", "lossless")
    with pytest.raises(ValueError):  # duplicate-ish wrong set
        normalize_order(
            ["lossless", "mp3_320", "mp3_256", "low"], "mp3_192", "lossless"
        )
    # A non-default permutation IS valid (order[0] = most preferred), e.g.
    # the Efficient-192+ preset list.
    assert normalize_order(
        ["mp3_192", "mp3_256", "mp3_320", "lossless"], "mp3_192", "lossless"
    )


def test_snapshot_hash_stable_and_summary_independent():
    a = build_snapshot(policy(**BALANCED))
    b = build_snapshot(policy(**BALANCED))
    assert a.snapshot_hash == b.snapshot_hash
    changed = build_snapshot(
        policy(**{**BALANCED, "preferred_lossy_bitrate_kbps": 256})
    )
    assert changed.snapshot_hash != a.snapshot_hash


def test_migration_snapshot_tags_origin():
    snap = migration_snapshot(policy())
    assert snap.origin == "legacy_migration"


# ---------------------------------------------------------------- ladders


@pytest.mark.parametrize(
    ("depth", "rate", "step"),
    [
        (16, 44100, 0),
        (16, 48000, 0),
        (20, 48000, 1),  # 20-bit maps to the up-to-24-bit step
        (24, 44100, 1),
        (24, 88200, 2),  # 88.2 kHz folds into the 96 step
        (24, 96000, 2),
        (24, 176400, 3),  # 176.4 folds into 192
        (24, 192000, 3),
        (24, 225792, 4),  # beyond 24/192 class
        (32, 44100, 4),
        (16, None, 5),
        (None, 96000, 5),
    ],
)
def test_lossless_detail_step_boundaries(depth, rate, step):
    assert lossless_detail_step(depth, rate) == step


def test_detail_comparator_axes_fill_legacy_floor():
    assert detail_comparator_axes(None, None) == (16, 44100)
    assert detail_comparator_axes(24, None) == (24, 44100)
    assert detail_comparator_axes(None, 48000) == (16, 48000)


def test_legacy_highest_preserves_hires_first():
    snap = build_snapshot(policy())
    cd = EV(codec_family=F.LOSSLESS, bit_depth=16, sample_rate_hz=44100)
    hires = EV(codec_family=F.LOSSLESS, bit_depth=24, sample_rate_hz=96000)
    d96 = EV(codec_family=F.LOSSLESS, bit_depth=24, sample_rate_hz=176400)
    keys = [lossless_rank_key(snap, e) for e in (cd, hires, d96)]
    assert keys.index(sorted(keys)[0]) == 2  # 24/176 sorts first under 'highest'


def test_target_mode_walks_ladder_upward_from_target():
    snap = build_snapshot(policy(**{**BALANCED, "quality_min": "low"}))
    ordered = [
        EV(codec_family=F.LOSSLESS, bit_depth=d, sample_rate_hz=r)
        for d, r in [(24, 192000), (24, 96000), (16, 44100), (24, 48000)]
    ]
    keys = [lossless_rank_key(snap, e) for e in ordered]
    best = ordered[keys.index(min(keys))]
    assert (best.bit_depth, best.sample_rate_hz) == (16, 44100)  # CD target first
    partial = EV(codec_family=F.LOSSLESS)  # resolution unknown: last among lossless
    assert lossless_rank_key(snap, partial) > max(keys)


# ---------------------------------------------------------------- evaluate


def test_preferred_cd_lossless_is_step_zero_with_resolution_code():
    snap = build_snapshot(policy(**BALANCED))
    d = evaluate(
        snap,
        EV(
            extension="flac",
            codec_family=F.LOSSLESS,
            bit_depth=16,
            sample_rate_hz=44100,
        ),
    )
    assert d.eligible and d.preference_step == 0 and d.tier == "lossless"
    names = [r.name for r in d.reasons]
    assert "PREFERRED_TIER" in names and "PREFERRED_LOSSLESS_RESOLUTION" in names


def test_hires_violates_provable_cap_even_when_one_axis_unknown():
    snap = build_snapshot(policy(**BALANCED))
    d = evaluate(snap, EV(extension="flac", codec_family=F.LOSSLESS, bit_depth=24))
    assert not d.eligible
    assert d.disposition == "outside_policy"
    assert any(r is QR.LOSSLESS_RESOLUTION_ABOVE_MAXIMUM for r in d.reasons)
    # Known-axis compliance + missing axis cannot prove violation -> eligible partial.
    d2 = evaluate(
        snap, EV(extension="flac", codec_family=F.LOSSLESS, sample_rate_hz=44100)
    )
    assert d2.eligible
    assert any(r is QR.LOSSLESS_RESOLUTION_PARTIAL for r in d2.reasons)


def test_unknown_family_rules_dispatch():
    review = evaluate(build_snapshot(policy(unknown_quality_behavior="review")), EV())
    assert (
        review.disposition == "needs_review"
        and review.tier is None
        and not review.eligible
    )
    rej = evaluate(build_snapshot(policy(unknown_quality_behavior="reject")), EV())
    assert rej.disposition == "unknown_rejected" and not rej.eligible
    fb = evaluate(build_snapshot(policy()), EV())  # allow_as_fallback default
    assert fb.eligible and fb.preference_step == 2  # after every known step


def test_family_unknown_fallback_ranks_strictly_after_known_steps():
    snap = build_snapshot(policy(unknown_quality_behavior="allow_as_fallback"))
    known_best = preference_step(snap, EV(extension="flac", codec_family=F.LOSSLESS))
    known_worst = preference_step(
        snap, EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=320)
    )
    unknown = preference_step(snap, EV(codec_family=F.UNKNOWN))
    assert unknown == len(snap.quality_preference_order)
    assert unknown > max(m for m in (known_best, known_worst) if m is not None)


def test_missing_bitrate_lossy_keeps_low_projection_and_never_promotes():
    snap = build_snapshot(policy())  # range [mp3_320..lossless] excludes 'low'
    d = evaluate(snap, EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=None))
    assert d.tier == "low" and not d.eligible
    assert any(r is QR.LOSSY_BITRATE_UNKNOWN for r in d.reasons)
    # A range containing 'low' ranks it LAST with the code present, still no family-unknown promotion.
    wide = build_snapshot(policy(quality_min="low"))
    d2 = evaluate(wide, EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=None))
    assert d2.eligible and d2.preference_step == len(wide.quality_preference_order) - 1
    codes = [r.name for r in d2.reasons]
    assert "LOSSY_BITRATE_UNKNOWN" in codes and "FALLBACK_TIER" in codes
    assert (
        "OUTSIDE_GLOBAL_PREFERENCE" not in codes or d2.disposition != "outside_policy"
    )


def test_lossy_bounds_reject_outside_band():
    # Bounds apply once the projected tier lies INSIDE the accepted range.
    bounded = build_snapshot(
        policy(
            quality_min="low",
            lossy_min_bitrate_kbps=192,
            lossy_max_bitrate_kbps=320,
        )
    )
    below = evaluate(
        bounded, EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=160)
    )
    above = evaluate(
        bounded, EV(extension="ogg", codec_family=F.LOSSY, bitrate_kbps=500)
    )
    assert not below.eligible and any(
        r is QR.LOSSY_BITRATE_BELOW_MINIMUM for r in below.reasons
    )
    assert not above.eligible and any(
        r is QR.LOSSY_BITRATE_ABOVE_MAXIMUM for r in above.reasons
    )


def test_lossy_target_exact_hit_records_code():
    snap = build_snapshot(policy(preferred_lossy_bitrate_kbps=192, quality_min="low"))
    d = evaluate(snap, EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=192))
    assert any(r is QR.PREFERRED_LOSSY_BITRATE for r in d.reasons)


def test_not_importable_dsd_short_circuits():
    d = evaluate(build_snapshot(policy()), EV(extension="dsf", codec_family=F.UNKNOWN))
    assert d.disposition == "not_importable" and not d.eligible
    assert d.reasons and d.reasons[0] is QR.FORMAT_NOT_IMPORTABLE


def test_inferred_provenance_codes_attached():
    title = evaluate(
        build_snapshot(policy()),
        EV(extension="flac", codec_family=F.LOSSLESS, provenance=P.RELEASE_TITLE),
    )
    cat = evaluate(
        build_snapshot(policy()),
        EV(
            extension="mp3",
            codec_family=F.LOSSY,
            bitrate_kbps=320,
            provenance=P.CATEGORY,
        ),
    )
    assert any(r is QR.QUALITY_INFERRED_FROM_TITLE for r in title.reasons)
    assert any(r is QR.QUALITY_INFERRED_FROM_CATEGORY for r in cat.reasons)


# ---------------------------------------------------------------- folding


def test_evaluate_worst_takes_worst_policy_step():
    snap = build_snapshot(policy())  # [lossless, mp3_320]
    folded = evaluate_worst(
        snap,
        [
            EV(
                extension="flac",
                codec_family=F.LOSSLESS,
                bit_depth=16,
                sample_rate_hz=44100,
            ),
            EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=320),
        ],
    )
    assert folded.eligible and folded.tier == "mp3_320" and folded.preference_step == 1
    assert folded.evidence.audio_file_count == 2


def test_evaluate_worst_flags_mixed_folder_and_floors_certainty():
    snap = build_snapshot(policy())
    a = EV(
        extension="flac",
        codec_family=F.LOSSLESS,
        bit_depth=24,
        sample_rate_hz=96000,
        certainty=C.PARTIAL,
        audio_file_count=3,
        total_bytes=100,
    )
    b = EV(
        extension="mp3",
        codec_family=F.LOSSY,
        bitrate_kbps=320,
        certainty=C.EXACT,
        audio_file_count=1,
        total_bytes=50,
    )
    folded = evaluate_worst(snap, [a, b])
    assert folded.evidence.certainty is C.PARTIAL
    assert folded.evidence.total_bytes == 150
    # Mixing lossless with lossy spans two canonical tiers on BOTH flags.
    assert folded.evidence.mixed_format and folded.evidence.mixed_quality
    assert folded.evidence.bit_depth == 24 and folded.evidence.sample_rate_hz == 96000


def test_evaluate_worst_prefers_review_over_eligible_and_not_importable_over_all():
    review_snap = build_snapshot(policy(unknown_quality_behavior="review"))
    with_review = evaluate_worst(review_snap, [EV(codec_family=F.UNKNOWN)])
    assert with_review.disposition == "needs_review"
    mixed = evaluate_worst(
        review_snap,
        [
            EV(codec_family=F.UNKNOWN),
            EV(
                extension="flac",
                codec_family=F.LOSSLESS,
                bit_depth=16,
                sample_rate_hz=44100,
            ),
        ],
    )
    assert mixed.disposition == "needs_review"  # one unknown file parks the folder
    hard = evaluate_worst(
        review_snap,
        [EV(extension="dff", codec_family=F.UNKNOWN), EV(codec_family=F.UNKNOWN)],
    )
    assert hard.disposition == "not_importable"


# ---------------------------------------------------------------- archive


@pytest.mark.parametrize(
    ("fmt", "family", "depth"),
    [
        ("Flac", "lossless", None),
        ("24bit Flac", "lossless", 24),
        ("MP3", "lossy", None),
        ("VBR MP3", "lossy", None),
        ("Ogg Vorbis", "lossy", None),
        ("TXT", "unknown", None),
    ],
)
def test_archive_format_strings_to_evidence(fmt, family, depth):
    ev = evidence_from_archive_format(fmt)
    assert ev.codec_family.value == family
    assert ev.bit_depth == depth
    assert ev.provenance is P.ARCHIVE_FORMAT
    if family == "lossless":
        assert ev.certainty is C.PARTIAL


def test_archive_24bit_flac_proves_cap_violation_without_sample_rate():
    snap = build_snapshot(policy(**BALANCED))  # CD cap
    d = evaluate(snap, evidence_from_archive_format("24bit Flac"))
    assert not d.eligible
    assert any(r is QR.LOSSLESS_RESOLUTION_ABOVE_MAXIMUM for r in d.reasons)


# ---------------------------------------------------------------- structs


def test_decision_roundtrips_through_house_json_codec():
    snap = build_snapshot(policy(**BALANCED))
    d = evaluate(
        snap,
        EV(
            extension="flac",
            codec_family=F.LOSSLESS,
            bit_depth=16,
            sample_rate_hz=44100,
        ),
    )
    blob = msgspec.json.encode(d)
    back = msgspec.convert(msgspec.json.decode(blob), type=QualityDecision)
    assert back == d


def test_policy_hash_changes_only_on_policy_inputs():
    s1 = build_snapshot(policy(**BALANCED))
    s2 = build_snapshot(policy(**BALANCED))
    s2.summary += " (edited)"
    s2.origin = "manual_override"
    assert snapshot_policy_hash(s1) == snapshot_policy_hash(s2)
    s3 = build_snapshot(
        policy(**{**BALANCED, "source_selection_mode": "quality_first"})
    )
    assert snapshot_policy_hash(s3) != snapshot_policy_hash(s1)


def test_efficient_preset_orders_band_then_target_distance_then_lossless():
    """Plan Verification scenario: Efficient-192+ must rank a
    192/240/256/320/CD-FLAC field exactly 192 -> 240 -> 256 -> 320 -> CD.
    Composed comparator = (preference_step, -target_distance, certainty):
    240 sits in the SAME canonical band as 192 but loses on |distance|."""
    snap = build_snapshot(
        policy(
            quality_min="low",
            quality_preference_order=["mp3_192", "mp3_256", "mp3_320", "lossless"],
            preferred_lossy_bitrate_kbps=192,
            lossless_preference="cd",
        )
    )
    from services.native.acquisition.quality import lossy_target_distance

    field = {
        "cd_flac": EV(codec_family=F.LOSSLESS, bit_depth=16, sample_rate_hz=44100),
        "k192": EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=192),
        "v240": EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=240),
        "k256": EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=256),
        "k320": EV(extension="mp3", codec_family=F.LOSSY, bitrate_kbps=320),
    }

    def key(e):
        step = preference_step(snap, e)
        dist = lossy_target_distance(snap, e)
        # ascending sort: lower step then CLOSER to target wins
        return (step if step is not None else 99, dist if dist is not None else -1)

    ranked = sorted(field, key=lambda k: key(field[k]))
    assert ranked == ["k192", "v240", "k256", "k320", "cd_flac"], ranked


def test_snapshot_struct_version_pinned():
    snap = AcquisitionQualitySnapshot()
    assert snap.schema_version == 1


def test_recipe_entry_requires_exact_evidence_for_automatic_claim():
    snap = build_snapshot(
        policy(
            quality_recipe=[
                QualityRecipeEntry(format="flac", quality="cd"),
                QualityRecipeEntry(format="mp3", quality="320_plus"),
            ],
            unknown_quality_behavior="review",
        )
    )
    partial = evaluate(
        snap,
        EV(
            extension="mp3",
            codec_family=F.LOSSY,
            bitrate_kbps=320,
            certainty=C.PARTIAL,
            provenance=P.SOURCE_METADATA,
        ),
    )
    inferred = evaluate(
        snap,
        EV(
            extension="mp3",
            codec_family=F.LOSSY,
            bitrate_kbps=320,
            certainty=C.INFERRED,
            provenance=P.CATEGORY,
        ),
    )
    assert partial.disposition == "needs_review"
    assert inferred.disposition == "needs_review"
    assert partial.quality_recipe_index is None
    assert inferred.quality_recipe_entry is None

    exact = evaluate(
        snap,
        EV(
            extension="mp3",
            codec_family=F.LOSSY,
            bitrate_kbps=320,
            certainty=C.EXACT,
            provenance=P.SOURCE_METADATA,
        ),
    )
    assert exact.eligible is True
    assert exact.quality_recipe_index == 1
    assert exact.quality_recipe_entry == snap.quality_recipe[1]


def test_recipe_unknown_allow_is_last_resort_without_entry():
    snap = build_snapshot(
        policy(
            quality_recipe=[QualityRecipeEntry(format="mp3", quality="320_plus")],
            unknown_quality_behavior="allow_as_fallback",
        )
    )
    decision = evaluate(
        snap,
        EV(
            extension="mp3",
            codec_family=F.LOSSY,
            bitrate_kbps=320,
            certainty=C.INFERRED,
            provenance=P.RELEASE_TITLE,
        ),
    )
    assert decision.eligible is True
    assert decision.disposition == "fallback"
    assert decision.preference_step == len(snap.quality_recipe)
    assert decision.quality_recipe_index is None
