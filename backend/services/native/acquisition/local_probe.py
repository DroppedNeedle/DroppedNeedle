"""Unconditional LOCAL quality verification before publication.

Provider metadata is selection evidence, not truth (Acquisition plan): every
automatically acquired or manually picked audio file is probed through the
shared codec-aware metadata path BEFORE FileProcessor / Drop Import publishes
it. The probe never modifies audio and never writes around the Library
Management publisher/import path - it only reads tags through
``infrastructure.audio.tagger`` (the single sanctioned audio-writer boundary;
mutagen is importable ONLY there).

Facts derived here carry certainty ``exact`` with provenance ``local_probe``;
classification/comparison stays in :mod:`services.native.acquisition.quality`.
"""

import asyncio
import logging
from pathlib import Path

from models.acquisition_quality import (
    AudioQualityEvidence,
    CodecFamily,
    EvidenceCertainty,
    EvidenceProvenance,
    QualityDecision,
)
from services.native.acquisition.quality import (
    CERTAINTY_RANK,
    evaluate,
    project_canonical_tier,
)

logger = logging.getLogger(__name__)

_LOSSLESS_FORMATS = {"flac", "wav", "ape", "wv"}
_MP4_FAMILY = {"m4a", "mp4", "mov", "m4b"}
_KNOWN_LOSSY = {"mp3", "ogg", "oga", "opus"}


def family_for_format(file_format: str, bit_depth: int | None) -> CodecFamily:
    """Codec-family classification honouring F-EDITION-04: an MP4-family file
    is lossless ONLY with trusted non-None bit-depth evidence (proven ALAC);
    lossy AAC keeps the synthetic depth suppressed upstream."""
    fmt = (file_format or "").lower()
    if fmt in _LOSSLESS_FORMATS:
        return CodecFamily.LOSSLESS
    if fmt in _MP4_FAMILY:
        return CodecFamily.LOSSLESS if bit_depth is not None else CodecFamily.LOSSY
    if fmt in _KNOWN_LOSSY:
        return CodecFamily.LOSSY
    return CodecFamily.UNKNOWN


def _evidence_from_info(path: Path, info) -> AudioQualityEvidence:
    """info: ``infrastructure.audio`` AudioInfo (duration/bitrate/sample_rate/
    channels/file_format/file_size_bytes/bit_depth)."""
    fmt = (info.file_format or path.suffix.lstrip(".")).lower()
    return AudioQualityEvidence(
        extension=fmt,
        codec_family=family_for_format(fmt, info.bit_depth),
        bitrate_kbps=info.bitrate
        if fmt not in _LOSSLESS_FORMATS | _MP4_FAMILY
        else None,
        bit_depth=info.bit_depth,
        sample_rate_hz=info.sample_rate,
        total_bytes=info.file_size_bytes,
        audio_file_count=1,
        certainty=EvidenceCertainty.EXACT,
        provenance=EvidenceProvenance.LOCAL_PROBE,
    )


def _merge(evidences: list[AudioQualityEvidence]) -> AudioQualityEvidence:
    """Folder aggregation without a snapshot: a folder reads as its WORST-file
    axes (minimum known depth/rate), flags record heterogeneity, bytes count."""
    families = {e.codec_family for e in evidences}
    depths = [e.bit_depth for e in evidences if e.bit_depth is not None]
    rates = [e.sample_rate_hz for e in evidences if e.sample_rate_hz is not None]
    extensions = {e.extension for e in evidences if e.extension}
    worst_family = (
        next(iter(families))
        if len(families) == 1
        else CodecFamily.LOSSY
        if CodecFamily.LOSSY in families
        else CodecFamily.UNKNOWN
    )
    lossy = [e for e in evidences if e.codec_family is CodecFamily.LOSSY]
    if len(families) > 1:
        mixed_quality = True
    elif families == {CodecFamily.LOSSY}:
        mixed_quality = len({e.bitrate_kbps for e in evidences}) > 1
    elif families == {CodecFamily.LOSSLESS}:
        mixed_quality = len({(e.bit_depth, e.sample_rate_hz) for e in evidences}) > 1
    else:
        mixed_quality = (
            len({(e.bitrate_kbps, e.bit_depth, e.sample_rate_hz) for e in evidences})
            > 1
        )
    mixed_quality = mixed_quality or any(e.mixed_quality for e in evidences)
    return AudioQualityEvidence(
        extension=sorted(extensions)[0] if len(extensions) == 1 else "",
        codec_family=(
            CodecFamily.LOSSLESS if families == {CodecFamily.LOSSLESS} else worst_family
        ),
        bitrate_kbps=min(
            (e.bitrate_kbps for e in lossy if e.bitrate_kbps is not None), default=None
        ),
        bit_depth=min(depths) if depths else None,
        sample_rate_hz=min(rates) if rates else None,
        total_bytes=sum(e.total_bytes or 0 for e in evidences) or None,
        audio_file_count=len(evidences),
        mixed_format=len(extensions) > 1,
        mixed_quality=mixed_quality,
        certainty=min((e.certainty for e in evidences), key=CERTAINTY_RANK.__getitem__),
        provenance=EvidenceProvenance.LOCAL_PROBE,
    )


def probe_files_sync(paths: list[Path], tagger) -> AudioQualityEvidence:
    """Read every audio file through the shared tagger and fold ONE evidence.
    A single unreadable file yields family-unknown for that file only; every
    file unreadable collapses to an unknown-folder verdict (never raises into
    the publication path)."""
    evidences: list[AudioQualityEvidence] = []
    for path in paths:
        try:
            _tag, info = tagger.read_tags(Path(path))
            evidences.append(_evidence_from_info(Path(path), info))
        except Exception as exc:  # noqa: BLE001 - probe must survive bad files
            logger.warning("local_probe.read_failed path=%s error=%s", path, exc)
            evidences.append(
                AudioQualityEvidence(
                    extension=Path(path).suffix.lstrip(".").lower(),
                    codec_family=CodecFamily.UNKNOWN,
                    certainty=EvidenceCertainty.UNKNOWN,
                    provenance=EvidenceProvenance.LOCAL_PROBE,
                )
            )
    if not evidences:
        return AudioQualityEvidence(
            codec_family=CodecFamily.UNKNOWN,
            certainty=EvidenceCertainty.UNKNOWN,
            provenance=EvidenceProvenance.LOCAL_PROBE,
        )
    if len(evidences) == 1:
        return evidences[0]
    return _merge(evidences)


async def probe_file(path: Path, tagger) -> AudioQualityEvidence:
    """Probe one downloaded audio file (thread-offloaded tag I/O)."""
    return await asyncio.to_thread(probe_files_sync, [Path(path)], tagger)


async def probe_files(paths: list[Path], tagger) -> AudioQualityEvidence:
    """Probe every attempt file and aggregate one folder/track evidence."""
    return await asyncio.to_thread(probe_files_sync, [Path(p) for p in paths], tagger)


def quality_mismatch(
    snapshot,
    decision: QualityDecision | None,
    probed: AudioQualityEvidence,
) -> bool:
    """Whether PROBED reality materially contradicts what selection promised.

    A canonical-tier comparison is meaningful only when the selection itself
    had exact evidence; partial source labels are re-evaluated from the probe
    under the pinned snapshot instead of treating a conservative projection as
    a promise.
    """
    projected = project_canonical_tier(probed)
    if (
        decision is not None
        and decision.tier is not None
        and decision.evidence.certainty is EvidenceCertainty.EXACT
        and projected != decision.tier
    ):
        return True
    probed_decision = evaluate(snapshot, probed)
    return not probed_decision.eligible


def expected_vs_actual_copy(
    decision: QualityDecision | None, probed: AudioQualityEvidence
) -> str:
    expected = (
        decision.summary
        if decision is not None and decision.summary
        else "the policy-accepted copy"
    )
    probed_bits: list[str] = []
    if probed.codec_family is CodecFamily.LOSSLESS and probed.bit_depth is not None:
        rate = probed.sample_rate_hz
        probed_bits.append(
            f"{probed.bit_depth}-bit/{round(rate / 1000)} kHz"
            if rate
            else f"{probed.bit_depth}-bit"
        )
    elif probed.bitrate_kbps is not None:
        probed_bits.append(f"{probed.bitrate_kbps} kbps")
    elif probed.extension:
        probed_bits.append(probed.extension.upper())
    actual = (
        f"{probed.codec_family.value} ({', '.join(probed_bits)})"
        if probed_bits
        else probed.codec_family.value
    )
    return f"Expected {expected.rstrip('.')} but downloaded files are {actual}."


__all__ = [
    "expected_vs_actual_copy",
    "family_for_format",
    "probe_file",
    "probe_files",
    "probe_files_sync",
    "quality_mismatch",
]
