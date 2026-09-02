"""Local probe aggregation and conservative mixed-quality regressions."""

from types import SimpleNamespace

from models.acquisition_quality import (
    AudioQualityEvidence as EV,
    CodecFamily as F,
    EvidenceCertainty as C,
    EvidenceProvenance as P,
    QualityRecipeEntry,
)
from services.native.acquisition.local_probe import _merge
from services.native.acquisition.quality import build_snapshot, evaluate_worst


def _evidence(**overrides):
    values = dict(
        extension="flac",
        codec_family=F.LOSSLESS,
        certainty=C.EXACT,
        provenance=P.LOCAL_PROBE,
        audio_file_count=1,
    )
    values.update(overrides)
    return EV(**values)


def _recipe_snapshot(*entries):
    return build_snapshot(
        SimpleNamespace(
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
            unknown_quality_behavior="review",
            source_selection_mode="source_first",
            quality_recipe=list(entries),
        )
    )


def test_merge_uses_worst_certainty_and_preserves_complementary_axes():
    merged = _merge(
        [
            _evidence(bit_depth=24, sample_rate_hz=None),
            _evidence(
                bit_depth=None,
                sample_rate_hz=96000,
                certainty=C.UNKNOWN,
                provenance=P.FORMAT_ONLY,
            ),
        ]
    )

    assert merged.certainty is C.UNKNOWN
    assert merged.bit_depth == 24
    assert merged.sample_rate_hz == 96000
    assert merged.mixed_format is False
    assert merged.mixed_quality is True


def test_merge_flags_same_format_lossy_bitrate_variation():
    merged = _merge(
        [
            _evidence(
                extension="mp3",
                codec_family=F.LOSSY,
                bitrate_kbps=192,
            ),
            _evidence(
                extension="mp3",
                codec_family=F.LOSSY,
                bitrate_kbps=320,
            ),
        ]
    )

    assert merged.mixed_format is False
    assert merged.mixed_quality is True
    assert merged.bitrate_kbps == 192


def test_evaluate_worst_same_format_flac_axes_becomes_unknown_review():
    snapshot = _recipe_snapshot(
        QualityRecipeEntry(format="flac", quality="cd"),
    )
    decision = evaluate_worst(
        snapshot,
        [
            _evidence(bit_depth=16, sample_rate_hz=44100),
            _evidence(bit_depth=16, sample_rate_hz=48000),
        ],
    )

    assert decision.disposition == "needs_review"
    assert decision.evidence.mixed_format is False
    assert decision.evidence.mixed_quality is True


def test_evaluate_worst_same_format_mp3_axes_becomes_unknown_review():
    snapshot = _recipe_snapshot(
        QualityRecipeEntry(format="mp3", quality="192_255"),
    )
    decision = evaluate_worst(
        snapshot,
        [
            _evidence(
                extension="mp3",
                codec_family=F.LOSSY,
                bitrate_kbps=200,
            ),
            _evidence(
                extension="mp3",
                codec_family=F.LOSSY,
                bitrate_kbps=224,
            ),
        ],
    )

    assert decision.disposition == "needs_review"
    assert decision.evidence.mixed_format is False
    assert decision.evidence.mixed_quality is True
