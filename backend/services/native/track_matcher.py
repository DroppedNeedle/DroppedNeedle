"""Per-track matcher.

Single-track scoring with no group/coherence phase: scores each candidate file
against the target track and wraps the best as a one-file ``ScoredCandidate`` so
the orchestrator's track branch and the Review tab consume it identically to an
album candidate. Reuses the album scorer's ``_file_confidence`` and the shared
quarantine + quality-tier filters (codec/tier gate + absolute highest-tier
preference).
"""

from infrastructure.persistence.download_store import DownloadStore
from models.download import ScoredCandidate, TargetTrack
from repositories.protocols.download_client import DownloadSearchResult
from services.native.album_preflight_scorer import _file_confidence
from services.native.quality_tiers import (
    DEFAULT_QUALITY_MAX,
    DEFAULT_QUALITY_MIN,
    file_tier,
    in_range,
    is_audio,
    is_flac_or_mp3,
    tier_rank,
)


class TrackMatcher:
    def __init__(
        self,
        download_store: DownloadStore,
        *,
        quality_min: str = DEFAULT_QUALITY_MIN,
        quality_max: str = DEFAULT_QUALITY_MAX,
        flac_mp3_only: bool = True,
    ):
        self._store = download_store
        self._quality_min = quality_min
        self._quality_max = quality_max
        self._flac_mp3_only = flac_mp3_only

    async def match(
        self,
        target: TargetTrack,
        results: list[DownloadSearchResult],
        *,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
    ) -> ScoredCandidate | None:
        ranked = await self.rank(
            target, results,
            auto_accept_threshold=auto_accept_threshold,
            manual_threshold=manual_threshold,
        )
        return ranked[0] if ranked else None

    async def rank(
        self,
        target: TargetTrack,
        results: list[DownloadSearchResult],
        *,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
        limit: int = 20,
    ) -> list[ScoredCandidate]:
        """Rank candidate files for a single track, best first, one per peer so the
        orchestrator can fail over to a different source. Each is wrapped as a
        one-file ``ScoredCandidate`` (consumed identically to an album candidate)."""
        quarantined = await self._store.load_quarantine_set()
        filtered = [r for r in results if (r.username, r.filename) not in quarantined]
        # drop the art/cue/log sidecars a folder search returns alongside the tracks
        filtered = [r for r in filtered if is_audio(r)]
        if self._flac_mp3_only:
            filtered = [r for r in filtered if is_flac_or_mp3(r)]
        filtered = [
            r
            for r in filtered
            if in_range(file_tier(r), self._quality_min, self._quality_max)
        ]
        if not filtered:
            return []

        scored: list[tuple[int, float, DownloadSearchResult]] = []
        for file in filtered:
            score = _file_confidence(
                target.track_title, target.artist_name, target.duration_seconds, file
            )
            scored.append((tier_rank(file_tier(file)), score, file))
        # prefer the higher tier, then the better match
        scored.sort(key=lambda t: (t[0], t[1]), reverse=True)

        candidates: list[ScoredCandidate] = []
        seen_peers: set[str] = set()
        for _rank, score, file in scored:
            if file.username in seen_peers:
                continue  # one candidate per peer - failover skips same-peer anyway
            seen_peers.add(file.username)
            if score >= auto_accept_threshold:
                tier = "auto"
            elif score >= manual_threshold:
                tier = "manual"
            else:
                tier = "rejected"
            candidates.append(
                ScoredCandidate(
                    username=file.username,
                    parent_directory=file.parent_directory,
                    files=[file],
                    coherence=score,
                    file_confidence=score,
                    final_score=score,
                    tier=tier,
                )
            )
            if len(candidates) >= limit:
                break
        return candidates
