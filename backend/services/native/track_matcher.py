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
from models.acquisition_quality import (
    AcquisitionQualitySnapshot,
    CodecFamily,
    EvidenceCertainty,
)
from models.download import ScoredCandidate, TargetTrack
from models.download_identity import soulseek_identity
from repositories.protocols.download_client import DownloadSearchResult
from services.native.album_preflight_scorer import _file_confidence, _file_evidence
from services.native.title_match import artist_evidence
from services.native.acquisition import quality as acq_quality
from services.native.quality_tiers import (
    in_range,
    file_tier,
    folder_hires_key,
    is_audio,
    is_flac_or_mp3,
    tier_rank,
)


class TrackMatcher:
    """Quality flows exclusively through the per-call ``AcquisitionQualitySnapshot``
    (spec Snapshot rule); non-quality spec gates ride ``spec_extras``."""

    def __init__(self, download_store: DownloadStore):
        self._store = download_store

    async def match(
        self,
        target: TargetTrack,
        results: list[DownloadSearchResult],
        *,
        snapshot: AcquisitionQualitySnapshot,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
    ) -> ScoredCandidate | None:
        ranked = await self.rank(
            target,
            results,
            snapshot=snapshot,
            auto_accept_threshold=auto_accept_threshold,
            manual_threshold=manual_threshold,
        )
        return ranked[0] if ranked else None

    async def rank(
        self,
        target: TargetTrack,
        results: list[DownloadSearchResult],
        *,
        snapshot: AcquisitionQualitySnapshot,
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
            r
            for r in results
            if ("soulseek", soulseek_identity(r.username, r.filename))
            not in quarantined
        ]
        # Drop art/cue/log sidecars a folder search returns alongside tracks.
        filtered = [r for r in filtered if is_audio(r)]
        if snapshot.flac_mp3_only:
            filtered = [r for r in filtered if is_flac_or_mp3(r)]
        # Recipe entries replace the contiguous v1 tier range. Keep soft
        # policy misses visible for an explicit manual pick, but remove hard
        # importability/cap violations before ranking.
        if acq_quality.is_recipe_snapshot(snapshot):
            recipe_filtered = []
            for result in filtered:
                decision = acq_quality.evaluate(snapshot, _file_evidence(result))
                if not acq_quality.is_hard_quality_rejection(decision):
                    recipe_filtered.append(result)
            filtered = recipe_filtered
        else:
            order = snapshot.quality_preference_order

            def _inside_range(candidate_result) -> bool:
                projection = file_tier(candidate_result)
                return in_range(projection, order[-1], order[0])

            filtered = [r for r in filtered if _inside_range(r)]
        if held_tier is not None:
            filtered = [
                r for r in filtered if tier_rank(file_tier(r)) > tier_rank(held_tier)
            ]
        if not filtered:
            return []

        scored = []
        for file in filtered:
            # strict_title: a track title is directly comparable to a filename,
            # so the containment metric applies.
            score = _file_confidence(
                target.track_title,
                target.artist_name,
                target.duration_seconds,
                file,
                strict_title=True,
            )
            if score >= auto_accept_threshold and artist_evidence(
                target.artist_name, file.filename
            ):
                acceptance = "auto"
            elif score >= manual_threshold:
                acceptance = "manual"
            else:
                acceptance = "rejected"
            quality_decision = (
                acq_quality.evaluate(snapshot, _file_evidence(file))
                if acq_quality.is_recipe_snapshot(snapshot)
                else None
            )
            if (
                quality_decision is not None
                and not quality_decision.eligible
                and acceptance == "auto"
            ):
                acceptance = "manual"
            if quality_decision is not None:
                step = (
                    quality_decision.preference_step
                    if quality_decision.preference_step is not None
                    else len(snapshot.quality_recipe) + 1
                )
                refinement = acq_quality.recipe_refinement_key(
                    snapshot, quality_decision.evidence
                )
                rank_key = (
                    {"rejected": 0, "manual": 1, "auto": 2}[acceptance],
                    -step,
                    *(-value for value in refinement),
                    int(file.has_free_slot),
                    int(file.queue_length is not None),
                    -(
                        file.queue_length
                        if file.queue_length is not None
                        else 2**31 - 1
                    ),
                    file.upload_speed,
                    -file.size,
                    score,
                )
            else:
                bit_depth, sample_rate = folder_hires_key([file])
                queue_known = file.queue_length is not None
                rank_key = (
                    {"rejected": 0, "manual": 1, "auto": 2}[acceptance],
                    tier_rank(file_tier(file)),
                    bit_depth,
                    sample_rate,
                    int(file.has_free_slot),
                    int(queue_known),
                    -(
                        file.queue_length
                        if file.queue_length is not None
                        else 2**31 - 1
                    ),
                    file.upload_speed,
                    -file.size,
                    score,
                )
            scored.append((rank_key, score, acceptance, file, quality_decision))
        scored.sort(key=lambda item: item[0], reverse=True)

        candidates: list[ScoredCandidate] = []
        seen_peers: set[str] = set()
        for _rank, score, acceptance, file, quality_decision in scored:
            if file.username in seen_peers:
                continue  # one candidate per peer - failover skips same-peer anyway
            seen_peers.add(file.username)
            # Auto-acceptance must be EARNED by identity evidence (D2, 2026-07-05
            # wrong-single incident): a wrong-artist file with an exact title +
            # plausible duration clears 0.70 on score alone, so a candidate whose
            # full remote path never names the requested artist caps at 'manual'
            # (one human click in Review) instead of downloading silently.
            candidates.append(
                ScoredCandidate(
                    username=file.username,
                    parent_directory=file.parent_directory,
                    files=[file],
                    coherence=score,
                    file_confidence=score,
                    final_score=score,
                    tier=acceptance,
                    quality_evidence=(
                        quality_decision.evidence
                        if quality_decision is not None
                        else None
                    ),
                    quality_decision=quality_decision,
                )
            )
            if len(candidates) >= limit:
                break
        return candidates
