"""``TorrentReleaseScorer`` - release-level scoring for the torrent source.

Mirrors ``NewznabReleaseScorer`` (same tier bands, same shared spec pipeline, the
same title/category quality reads - imported from it, not copied) with the torrent
differences: **seeders replace grabs as the health signal** and an explicit
zero-seeder release is dropped outright. Missing counts remain available for
manual review but can never auto-accept. Identity is the same normalised
title+size key, namespaced ``source="torrent"`` in quarantine.
"""

import logging
from collections import Counter

from rapidfuzz import fuzz

from infrastructure.persistence.download_store import DownloadStore
from models.download import ScoredCandidate, TargetAlbum
from models.download_identity import usenet_identity
from repositories.protocols.indexer import TorrentRelease
from services.native.acquisition import pipeline
from services.native.acquisition.context import build_context
from services.native.acquisition.decision import (
    Candidate,
    Reject,
    RejectCode,
    SpecPolicy,
)
from services.native.newznab_release_scorer import (
    _AVG_TRACK_SECONDS,
    _CAT_LOSSLESS,
    _CAT_MP3,
    _CAT_VIDEO,
    _LOSSLESS_RE,
    _MP3_192_RE,
    _MP3_256_RE,
    _MP3_320_RE,
    _MP3_GENERIC_RE,
    _QUALITY_SCORE,
    _SIZE_MIN_FRAC,
    _TIER_NOMINAL_KBPS,
    _hires_rank,
)
from services.native.quality_tiers import DEFAULT_QUALITY_MAX, DEFAULT_QUALITY_MIN
from services.native.title_match import fold

logger = logging.getLogger(__name__)

# Health saturates at this many seeders (a 20-seed album is as healthy as a 200-seed
# one for a single grab; scale below that still rewards better-seeded releases).
_SEEDERS_SATURATION = 20.0


class TorrentReleaseScorer:
    def __init__(
        self,
        download_store: DownloadStore,
        *,
        quality_min: str = DEFAULT_QUALITY_MIN,
        quality_max: str = DEFAULT_QUALITY_MAX,
        flac_mp3_only: bool = True,
        policy: SpecPolicy | None = None,
    ) -> None:
        self._store = download_store
        self._flac_mp3_only = flac_mp3_only
        self._policy = policy or SpecPolicy(quality_min=quality_min, quality_max=quality_max)

    async def rank(
        self,
        target: TargetAlbum,
        releases: list[TorrentRelease],
        *,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
        track_count: int | None = None,
        held_tier: str | None = None,
    ) -> list[ScoredCandidate]:
        context = await build_context(self._store, held_tier=held_tier)
        policy = self._policy
        tracks = track_count if track_count is not None else target.track_count
        scored: list[ScoredCandidate] = []
        dropped_video = dropped_size = dropped_dead = 0
        pipeline_drops: Counter[RejectCode] = Counter()

        for release in releases:
            # Explicit zero means dead. A missing count is uncertain rather than dead:
            # retain it for manual review, but never allow it into the auto band.
            if release.seeders == 0:
                dropped_dead += 1
                continue
            if _CAT_VIDEO in set(release.category_ids):
                dropped_video += 1
                continue
            declared = self._declared_tier(release)
            tier = self._release_tier(release, tracks)
            # Shared spec pipeline - the SAME rules as the Soulseek/Usenet paths (blocklist
            # by title+size identity in the "torrent" namespace, wrong-edition/wrong-album,
            # ignored/required terms, quality range, max-size, free-space). usenet_date is
            # None: the Usenet retention/min-age gates don't apply to torrents.
            decision = pipeline.run(
                Candidate(
                    source="torrent",
                    identity=usenet_identity(release.title, release.size_bytes),
                    match_text=release.title,
                    tier=tier,
                    size_bytes=release.size_bytes,
                ),
                target, context, policy,
            )
            if isinstance(decision, Reject):
                pipeline_drops[decision.code] += 1
                continue
            if self._size_implausible(release, declared, tracks, target.duration_seconds):
                dropped_size += 1
                continue

            identity = self._identity_score(target, release)
            quality = _QUALITY_SCORE.get(tier, 0.5)
            health = min(1.0, (release.seeders or 0) / _SEEDERS_SATURATION)
            final = 0.40 * identity + 0.45 * quality + 0.15 * health
            band = (
                "auto" if final >= auto_accept_threshold
                else "manual" if final >= manual_threshold
                else "rejected"
            )
            if release.seeders is None and band == "auto":
                band = "manual"
            scored.append(
                ScoredCandidate(
                    source="torrent",
                    torrent_release=release,
                    coherence=identity,
                    file_confidence=quality,
                    final_score=round(final, 4),
                    tier=band,
                )
            )

        scored.sort(
            key=lambda c: (
                c.final_score,
                _hires_rank(c.torrent_release.title if c.torrent_release else ""),
            ),
            reverse=True,
        )
        if dropped_video or dropped_size or dropped_dead or pipeline_drops:
            logger.info(
                "torrent.scored",
                extra={
                    "releases": len(releases),
                    "scored": len(scored),
                    "dropped_video": dropped_video,
                    "dropped_size": dropped_size,
                    "dropped_dead": dropped_dead,
                    **{f"dropped_{code.value}": n for code, n in pipeline_drops.items()},
                },
            )
        return scored[:50]

    def _size_implausible(
        self, release: TorrentRelease, declared_tier: str, track_count: int | None,
        duration_seconds: float | None,
    ) -> bool:
        if not release.size_bytes:
            return False
        seconds = duration_seconds or (track_count * _AVG_TRACK_SECONDS if track_count else None)
        if not seconds:
            return False
        nominal = _TIER_NOMINAL_KBPS.get(declared_tier, 320)
        expected = nominal * 1000 / 8 * seconds
        return release.size_bytes < _SIZE_MIN_FRAC * expected

    def _declared_tier(self, release: TorrentRelease) -> str:
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

    def release_tier(self, release: TorrentRelease, track_count: int | None = None) -> str:
        """Public accessor for the scoring tier (the orchestrator's re-gate mirror of
        ``NewznabReleaseScorer.release_tier``)."""
        return self._release_tier(release, track_count)

    def _release_tier(self, release: TorrentRelease, track_count: int | None) -> str:
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
        return "mp3_320"

    @staticmethod
    def _identity_score(target: TargetAlbum, release: TorrentRelease) -> float:
        base = 0.5
        query = fold(f"{target.artist_name} {target.album_title}")
        ratio = fuzz.token_set_ratio(query, fold(release.title or ""))
        score = base + 0.4 * (ratio / 100.0)
        if target.year and str(target.year) in (release.title or ""):
            score += 0.08
        return min(1.0, score)
