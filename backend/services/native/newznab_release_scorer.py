"""``NewznabReleaseScorer`` - release-level scoring for Usenet (D10).

Mirrors ``AlbumPreflightScorer``'s tier bands (auto ≥0.70 / manual 0.50-0.70 /
rejected) but scores a whole NZB, not per-file. Usenet titles are noisy/obfuscated,
so **category is the primary quality signal** (3040⇒lossless) and identity leans on
**indexer-match** (the indexer returned this release for our query) so an obfuscated
release with a good category + healthy grabs can still reach **auto** (Q4) - the
real backstop being the import tag-match, which imports 0 tracks for a wrong album
and triggers blocklist + failover. Reads ``download_policy`` for the quality range;
filters quarantined releases by the **title+size** identity (not the per-indexer guid).
"""

import logging
import re
from collections import Counter

from rapidfuzz import fuzz

from infrastructure.persistence.download_store import DownloadStore
from models.download import ScoredCandidate, TargetAlbum
from models.download_identity import usenet_identity
from repositories.protocols.indexer import UsenetRelease
from services.native.acquisition import pipeline
from services.native.acquisition.context import build_context
from services.native.acquisition.decision import (
    Candidate,
    Reject,
    RejectCode,
    SpecPolicy,
)
from models.acquisition_quality import (
    AcquisitionQualitySnapshot,
    AudioQualityEvidence,
    CodecFamily,
    EvidenceCertainty,
    EvidenceProvenance,
    QualityReason,
)
from services.native.acquisition import quality as acq_quality
from services.native.quality_tiers import tier_rank
from services.native.title_match import fold

logger = logging.getLogger(__name__)

# Audio category ids (the "Other" id varies - 3050 standard, 3999 nZEDb - so it's not
# special-cased here; an unknown audio cat maps to "unknown" quality).
_CAT_LOSSLESS = 3040
_CAT_MP3 = 3010
_CAT_VIDEO = 3020  # music videos - not a music codec; reject for album/track music
_CAT_AUDIO_PARENT = 3000

_LOSSLESS_RE = re.compile(
    r"\b(flac|alac|ape|wavpack|wav|24[\s\-]?bit|24[\s\-]?44|24[\s\-]?96|dsd|lossless)\b",
    re.IGNORECASE,
)
# Hi-res markers (H1): Usenet captures no per-file bit-depth/sample-rate, so read them from
# the (noisy) title to prefer a 24-bit/96k+ release over a 16/44 one of EQUAL score. DSD and
# 24/192 outrank 24/96. A pure tie-breaker - never lifts a worse-scored or non-matching release.
_HIRES_192_RE = re.compile(
    r"\b(24[\s\-]?192|192[\s\-]?khz|dsd|dsf|sacd)\b", re.IGNORECASE
)
_HIRES_RE = re.compile(
    r"\b(24[\s\-]?bit|24[\s\-]?96|24[\s\-]?88|96[\s\-]?khz|88[\.\s\-]?2[\s\-]?khz|hi[\s\-]?res)\b",
    re.IGNORECASE,
)
_MP3_320_RE = re.compile(r"\b(320|v0|cbr[\s\-]?320)\b", re.IGNORECASE)
_MP3_256_RE = re.compile(r"\b(256|v2)\b", re.IGNORECASE)
_MP3_192_RE = re.compile(r"\b(192|v5)\b", re.IGNORECASE)
_MP3_GENERIC_RE = re.compile(r"\b(mp3|cbr|vbr)\b", re.IGNORECASE)

_QUALITY_SCORE = {
    "lossless": 1.0,
    "mp3_320": 0.8,
    "mp3_256": 0.65,
    "mp3_192": 0.5,
    "low": 0.3,
    "unknown": 0.5,
}

# Size-plausibility MIN (Lidarr's AcceptableSize, by runtime). A release far below the
# album-at-nominal-bitrate is a single track / fragment. Nominal bitrates are the DECLARED
# tier's typical rate (FLAC ~700kbps). The fraction is generous so VBR rips, heavy FLAC
# compression and short EPs are never dropped - only egregiously-small releases. (No MAX:
# like Lidarr, lossless size varies too much - 16-bit vs 24/192 - to cap; oversized
# boxsets are rejected by NAME below, not size.)
_TIER_NOMINAL_KBPS = {
    "lossless": 700,
    "mp3_320": 320,
    "mp3_256": 256,
    "mp3_192": 192,
    "low": 128,
    "unknown": 320,
}
_SIZE_MIN_FRAC = 0.35
_AVG_TRACK_SECONDS = 240.0  # fallback when MB gave no album duration

# (The edition / wrong-product reject moved to the shared ``wrong_edition`` spec - M3 -
# so the Soulseek path gets the identical hard reject.)


def _hires_rank(title: str) -> int:
    """Title-derived hi-res rank for the sort tie-breaker: 2 = 24/192 / DSD / SACD,
    1 = 24-bit / 96k+, 0 = none. Best-effort off a noisy title (Usenet has no per-file
    bit-depth), so it only ever breaks an exact score tie."""
    text = title or ""
    if _HIRES_192_RE.search(text):
        return 2
    if _HIRES_RE.search(text):
        return 1
    return 0


def _release_evidence(
    release: UsenetRelease,
    tier: str,
    snapshot: AcquisitionQualitySnapshot | None = None,
) -> AudioQualityEvidence:
    """Project release metadata into the shared quality evaluator.

    v1 keeps the historical tier projection. A v2 recipe never invents a
    bitrate or FLAC resolution when the title/category does not provide one.
    """
    title = release.title or ""
    categories = set(release.category_ids)
    provenance = (
        EvidenceProvenance.CATEGORY
        if _CAT_LOSSLESS in categories or _CAT_MP3 in categories
        else EvidenceProvenance.RELEASE_TITLE
    )
    if re.search(r"\b(dsd|dsf|dff|sacd)\b", title, re.IGNORECASE):
        return AudioQualityEvidence(
            extension="dsf",
            codec_family=CodecFamily.UNKNOWN,
            total_bytes=release.size_bytes,
            audio_file_count=1,
            certainty=EvidenceCertainty.PARTIAL,
            provenance=provenance,
        )
    if snapshot is None or not acq_quality.is_recipe_snapshot(snapshot):
        bitrate = None
        family = CodecFamily.UNKNOWN
        certainty = EvidenceCertainty.INFERRED
        ext = ""
        if tier == "lossless":
            family = CodecFamily.LOSSLESS
            ext = "flac"
        elif tier == "mp3_320":
            family = CodecFamily.LOSSY
            bitrate = 320
            ext = "mp3"
            certainty = (
                EvidenceCertainty.PARTIAL
                if _MP3_320_RE.search(title) or _MP3_GENERIC_RE.search(title)
                else EvidenceCertainty.INFERRED
            )
        elif tier == "mp3_256":
            family, bitrate, ext = CodecFamily.LOSSY, 256, "mp3"
        elif tier == "mp3_192":
            family, bitrate, ext = CodecFamily.LOSSY, 192, "mp3"
        return AudioQualityEvidence(
            extension=ext,
            codec_family=family,
            bitrate_kbps=bitrate,
            total_bytes=release.size_bytes,
            audio_file_count=1,
            certainty=certainty,
            provenance=provenance,
        )

    if _CAT_MP3 in categories or _MP3_GENERIC_RE.search(title):
        bitrate = (
            320
            if _MP3_320_RE.search(title)
            else 256
            if _MP3_256_RE.search(title)
            else 192
            if _MP3_192_RE.search(title)
            else None
        )
        return AudioQualityEvidence(
            extension="mp3",
            codec_family=CodecFamily.LOSSY,
            bitrate_kbps=bitrate,
            total_bytes=release.size_bytes,
            audio_file_count=1,
            certainty=(
                EvidenceCertainty.PARTIAL
                if bitrate is not None
                else EvidenceCertainty.INFERRED
            ),
            provenance=provenance,
        )

    if _CAT_LOSSLESS in categories or _LOSSLESS_RE.search(title):
        bit_depth = (
            24 if _HIRES_RE.search(title) or _HIRES_192_RE.search(title) else None
        )
        sample_rate = None
        if _HIRES_192_RE.search(title):
            sample_rate = 192000
        elif re.search(r"(?:24[\s\-]?96|96[\s\-]?khz)", title, re.IGNORECASE):
            sample_rate = 96000
        elif re.search(r"(?:24[\s\-]?48|48[\s\-]?khz)", title, re.IGNORECASE):
            sample_rate = 48000
        elif re.search(
            r"(?:24[\s\-]?44(?:\.1)?|44(?:\.1)?[\s\-]?khz)", title, re.IGNORECASE
        ):
            sample_rate = 44100
        return AudioQualityEvidence(
            extension="flac",
            codec_family=CodecFamily.LOSSLESS,
            bit_depth=bit_depth,
            sample_rate_hz=sample_rate,
            total_bytes=release.size_bytes,
            audio_file_count=1,
            certainty=(
                EvidenceCertainty.PARTIAL
                if bit_depth is not None and sample_rate is not None
                else EvidenceCertainty.INFERRED
            ),
            provenance=provenance,
        )
    return AudioQualityEvidence(
        extension="",
        codec_family=CodecFamily.UNKNOWN,
        total_bytes=release.size_bytes,
        audio_file_count=1,
        certainty=EvidenceCertainty.INFERRED,
        provenance=provenance,
    )


class NewznabReleaseScorer:
    """Quality flows exclusively through the per-call ``AcquisitionQualitySnapshot``;
    identity/health weighting and distrust heuristics are unchanged. The ONE
    sanctioned ``unknown``-tier semantics change (spec): the family-unknown rule now
    reads ``snapshot.unknown_quality_behavior`` instead of an unconditional pass."""

    def __init__(self, download_store: DownloadStore) -> None:
        self._store = download_store

    async def rank(
        self,
        target: TargetAlbum,
        releases: list[UsenetRelease],
        *,
        snapshot: AcquisitionQualitySnapshot,
        spec_extras: "SpecPolicy | None" = None,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
        track_count: int | None = None,
        held_tier: str | None = None,
    ) -> list[ScoredCandidate]:
        context = await build_context(self._store, held_tier=held_tier)
        import msgspec as _ms

        order = snapshot.quality_preference_order
        recipe_mode = acq_quality.is_recipe_snapshot(snapshot)
        policy = SpecPolicy(
            quality_min="low" if recipe_mode else order[-1] if order else "low",
            quality_max="lossless"
            if recipe_mode
            else order[0]
            if order
            else "lossless",
        )
        if spec_extras is not None:
            policy = _ms.structs.replace(
                policy,
                max_size_mb=spec_extras.max_size_mb,
                ignored_terms=tuple(spec_extras.ignored_terms),
                required_terms=tuple(spec_extras.required_terms),
                usenet_retention_days=spec_extras.usenet_retention_days,
            )
        tracks = track_count if track_count is not None else target.track_count
        scored: list[ScoredCandidate] = []
        dropped_video = dropped_size = 0
        pipeline_drops: Counter[RejectCode] = Counter()

        for release in releases:
            # Music-video category isn't a music codec tier. Stays inline (a category gate,
            # not a release-name spec); the in_range half is now the shared quality_range spec.
            if _CAT_VIDEO in set(release.category_ids):
                dropped_video += 1
                continue
            declared = self._declared_tier(release)
            tier = self._release_tier(release, tracks)
            # Shared spec pipeline - the SAME rules as the Soulseek path: blocklist by the
            # title+size identity, password, wrong-edition (live/boxset/comp) + wrong-album
            # (a different album by the same artist scores near-identical under
            # token_set_ratio), sample, ignored/required terms, quality-range (an 'unknown'
            # tier passes; the import tag-match is the real quality truth, D10), max-size,
            # retention + min-age (Usenet age gates, off by default), free-space.
            decision = pipeline.run(
                Candidate(
                    source="usenet",
                    identity=usenet_identity(release.title, release.size_bytes),
                    match_text=release.title,
                    tier=tier,
                    size_bytes=release.size_bytes,
                    usenet_date=release.usenet_date,
                    password=release.password,
                ),
                target,
                context,
                policy,
            )
            if isinstance(decision, Reject):
                pipeline_drops[decision.code] += 1
                continue
            # Size-plausibility MIN (min_size, Usenet-only) stays inline: it needs the DECLARED
            # tier's nominal bitrate + the album runtime, not just the candidate's bytes.
            if self._size_implausible(
                release, declared, tracks, target.duration_seconds
            ):
                dropped_size += 1
                continue

            identity = self._identity_score(target, release)
            quality = _QUALITY_SCORE.get(tier, 0.5)
            health = min(1.0, (release.grabs or 0) / 50.0)
            final = 0.40 * identity + 0.45 * quality + 0.15 * health
            band = (
                "auto"
                if final >= auto_accept_threshold
                else "manual"
                if final >= manual_threshold
                else "rejected"
            )
            decision_ev = _release_evidence(release, tier, snapshot)
            release_decision = acq_quality.evaluate(snapshot, decision_ev)
            if (
                recipe_mode
                and not release_decision.eligible
                and acq_quality.is_hard_quality_rejection(release_decision)
            ):
                pipeline_drops[RejectCode.QUALITY_REJECTED] += 1
                continue
            if recipe_mode and not release_decision.eligible and band == "auto":
                band = "manual"
            scored.append(
                ScoredCandidate(
                    source="usenet",
                    usenet_release=release,
                    coherence=identity,
                    file_confidence=quality,
                    final_score=round(final, 4),
                    tier=band,
                    quality_evidence=decision_ev,
                    quality_decision=release_decision,
                )
            )

        band_rank = {"auto": 2, "manual": 1, "rejected": 0}
        worst_step = len(order) + 2

        def _sort_key(cand):
            decision_ = cand.quality_decision
            evidence_ = cand.quality_evidence
            step = decision_.preference_step if decision_ else None
            if recipe_mode:
                step = step if step is not None else len(snapshot.quality_recipe) + 1
                refinement = acq_quality.recipe_refinement_key(
                    snapshot, evidence_ or AudioQualityEvidence()
                )
                return (
                    band_rank.get(cand.tier, 0),
                    -step,
                    *(-value for value in refinement),
                    cand.final_score,
                    _hires_rank(
                        cand.usenet_release.title if cand.usenet_release else ""
                    ),
                )
            certainty = acq_quality.CERTAINTY_RANK[
                evidence_.certainty if evidence_ else EvidenceCertainty.PARTIAL
            ]
            return (
                band_rank.get(cand.tier, 0),
                -(step if step is not None else worst_step),
                -certainty,
                cand.final_score,
                _hires_rank(cand.usenet_release.title if cand.usenet_release else ""),
            )

        scored.sort(key=_sort_key, reverse=True)
        if dropped_video or dropped_size or pipeline_drops:
            logger.info(
                "newznab.scored",
                extra={
                    "releases": len(releases),
                    "scored": len(scored),
                    # inline category/size gates, plus one key per shared-spec reject code.
                    "dropped_video": dropped_video,
                    "dropped_size": dropped_size,
                    **{
                        f"dropped_{code.value}": n for code, n in pipeline_drops.items()
                    },
                },
            )
        return scored[:50]

    def _size_implausible(
        self,
        release: UsenetRelease,
        declared_tier: str,
        track_count: int | None,
        duration_seconds: float | None,
    ) -> bool:
        """True when the release is far too SMALL to hold the album at its declared tier
        (a single track / fragment). Indeterminate inputs never reject."""
        if not release.size_bytes:
            return False
        seconds = duration_seconds or (
            track_count * _AVG_TRACK_SECONDS if track_count else None
        )
        if not seconds:
            return False
        nominal = _TIER_NOMINAL_KBPS.get(declared_tier, 320)
        expected = nominal * 1000 / 8 * seconds
        return release.size_bytes < _SIZE_MIN_FRAC * expected

    def _declared_tier(self, release: UsenetRelease) -> str:
        """Quality tier from category (most reliable) then title regex, WITHOUT the size
        downgrade - the declared codec, for size-plausibility bounds."""
        cats = set(release.category_ids)
        title = release.title or ""
        if _CAT_LOSSLESS in cats or _LOSSLESS_RE.search(title):
            return "lossless"
        if _CAT_MP3 in cats:
            return self._mp3_subtier(title)
        if _MP3_320_RE.search(title):
            return "mp3_320"
        if _MP3_256_RE.search(title):
            return "mp3_256"
        if _MP3_192_RE.search(title):
            return "mp3_192"
        if _MP3_GENERIC_RE.search(title):
            return "mp3_320"
        return "unknown"

    def release_tier(
        self, release: UsenetRelease, track_count: int | None = None
    ) -> str:
        """Public accessor for the scoring tier, so the orchestrator's Phase-2 re-gate
        can re-judge a stored Usenet candidate the same way it was scored."""
        return self._release_tier(release, track_count)

    def _release_tier(self, release: UsenetRelease, track_count: int | None) -> str:
        """Quality tier for SCORING: the declared tier, but a 'lossless' release far too
        small to be real lossless degrades to 'unknown' (don't trust the label)."""
        tier = self._declared_tier(release)
        if tier == "lossless" and track_count and release.size_bytes:
            if release.size_bytes / track_count / (1024 * 1024) < 8:
                tier = "unknown"
        return tier

    @staticmethod
    def _mp3_subtier(title: str) -> str:
        if _MP3_320_RE.search(title):
            return "mp3_320"
        if _MP3_256_RE.search(title):
            return "mp3_256"
        if _MP3_192_RE.search(title):
            return "mp3_192"
        return "mp3_320"  # an MP3-category release with no bitrate hint: assume 320

    @staticmethod
    def _identity_score(target: TargetAlbum, release: UsenetRelease) -> float:
        """Indexer-match base (this release came back for our artist+album query) plus a
        title-fuzzy bonus, and a small year-match nudge to disambiguate editions (e.g. a
        2011 remaster vs the original). Obfuscated titles degrade to the base, so identity
        stays a weak-positive signal even on garbage names (Q4)."""
        base = 0.5
        # Fold accents/case on BOTH sides so "Motley Crue" matches "Mötley Crüe" (rapidfuzz
        # applies no normalisation by default) - otherwise accented artists score near base.
        query = fold(f"{target.artist_name} {target.album_title}")
        ratio = fuzz.token_set_ratio(query, fold(release.title or ""))
        score = base + 0.4 * (ratio / 100.0)
        if target.year and str(target.year) in (release.title or ""):
            score += (
                0.08  # tie-breaker between editions, not enough to lift a non-match
            )
        return min(1.0, score)
