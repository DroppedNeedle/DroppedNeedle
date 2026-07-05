"""Per-track matcher.

Single-track scoring with no group/coherence phase: scores each candidate file
against the target track and wraps the best as a one-file ``ScoredCandidate`` so
the orchestrator's track branch and the Review tab consume it identically to an
album candidate. Reuses the album scorer's ``_file_confidence`` and the shared
quarantine + quality-tier filters (codec/tier gate + absolute highest-tier
preference). ``tier='auto'`` additionally requires the requested artist to be
named somewhere in the candidate's remote path (``title_match.artist_evidence``,
D2) - score alone cannot silently accept a wrong-artist file. Also serves
1-track album tasks (singles), which the Soulseek strategy routes here instead
of the folder scorer (2026-07-05 wrong-single incident).
"""

from infrastructure.persistence.download_store import DownloadStore
from models.download import ScoredCandidate, TargetTrack
from models.download_identity import soulseek_identity
from repositories.protocols.download_client import DownloadSearchResult
from services.native.album_preflight_scorer import _file_confidence
from services.native.title_match import artist_evidence
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
        held_tier: str | None = None,
    ) -> list[ScoredCandidate]:
        """Rank candidate files for a single track, best first, one per peer so the
        orchestrator can fail over to a different source. Each is wrapped as a
        one-file ``ScoredCandidate`` (consumed identically to an album candidate).
        ``held_tier`` (an origin='upgrade' run) keeps only files that STRICTLY beat
        the recording's best held copy (D12; the track path has no spec pipeline)."""
        quarantined = await self._store.load_quarantine_set()
        filtered = [
            r for r in results
            if ("soulseek", soulseek_identity(r.username, r.filename)) not in quarantined
        ]
        # drop the art/cue/log sidecars a folder search returns alongside the tracks
        filtered = [r for r in filtered if is_audio(r)]
        if self._flac_mp3_only:
            filtered = [r for r in filtered if is_flac_or_mp3(r)]
        filtered = [
            r
            for r in filtered
            if in_range(file_tier(r), self._quality_min, self._quality_max)
        ]
        if held_tier is not None:
            filtered = [
                r for r in filtered if tier_rank(file_tier(r)) > tier_rank(held_tier)
            ]
        if not filtered:
            return []

        scored: list[tuple[int, float, DownloadSearchResult]] = []
        for file in filtered:
            # strict_title: a track title is directly comparable to a filename, so the
            # containment metric applies (unlike the album path's title-vs-album noise).
            score = _file_confidence(
                target.track_title, target.artist_name, target.duration_seconds, file,
                strict_title=True,
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
            # Auto-acceptance must be EARNED by identity evidence (D2, 2026-07-05
            # wrong-single incident): a wrong-artist file with an exact title +
            # plausible duration clears 0.70 on score alone, so a candidate whose
            # full remote path never names the requested artist caps at 'manual'
            # (one human click in Review) instead of downloading silently.
            if score >= auto_accept_threshold and artist_evidence(
                target.artist_name, file.filename
            ):
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
