"""Bounded explicit re-identification evaluation and candidate selection."""

from __future__ import annotations

import time
import uuid
from pathlib import Path

from core.exceptions import ExternalServiceError
from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
)
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.identification import IdentificationAttempt, IdentificationEvidenceRecord
from services.native.album_candidate_service import AlbumCandidateService
from services.native.album_evidence_engine import MATCHER_VERSION, AlbumEvidenceEngine
from services.native.album_identification_service import (
    MAX_NEW_FINGERPRINTS_PER_ATTEMPT,
    _candidate_key,
    _to_grouping_track,
)
from services.native.background_workload_gate import BackgroundWorkloadGate
from services.native.conditional_fingerprint_service import (
    FINGERPRINTER_VERSION,
    ConditionalFingerprintService,
)
from services.native.identification_revisions import album_input_revisions
from services.native.library_operation_service import LibraryOperationService


class ExplicitReidentificationWorker:
    def __init__(
        self,
        store: NativeLibraryStore,
        candidates: AlbumCandidateService,
        evidence: AlbumEvidenceEngine,
        fingerprints: ConditionalFingerprintService | None = None,
        workload_gate: BackgroundWorkloadGate | None = None,
    ) -> None:
        self._store = store
        self._candidates = candidates
        self._evidence = evidence
        self._fingerprints = fingerprints
        self._workload_gate = workload_gate

    async def run_claimed(
        self,
        job: dict,
        worker_id: str,
        *,
        now: float | None = None,
    ) -> dict:
        timestamp = time.time() if now is None else now
        work = await self._store.claim_operation_work(
            str(job["id"]), worker_id, now=timestamp
        )
        if work is None:
            return await self._store.finish_operation_job(
                str(job["id"]),
                worker_id,
                state="failed",
                terminal_code="MISSING_WORK",
                now=timestamp,
            )
        context = await self._store.get_album_identification_context(
            str(work["local_album_id"])
        )
        if context is None:
            return await self._store.finish_operation_job(
                str(job["id"]),
                worker_id,
                state="failed",
                terminal_code="SUBJECT_NOT_AVAILABLE",
                now=timestamp,
            )
        revisions = album_input_revisions(context["tracks"])
        if ":".join(revisions) != str(work["expected_input_revision"]):
            return await self._store.finish_operation_job(
                str(job["id"]),
                worker_id,
                state="failed",
                terminal_code="STALE_INPUT",
                now=timestamp,
            )

        async def checkpoint() -> bool:
            if self._workload_gate is not None and self._workload_gate.scan_active:
                return False
            current = await self._store.get_operation_job(str(job["id"]))
            return (
                current is not None
                and current["control_request"] == "none"
                and (self._workload_gate is None or not self._workload_gate.scan_active)
            )

        async def halt() -> dict:
            if self._workload_gate is not None and self._workload_gate.scan_active:
                return await self._store.defer_explicit_operation_for_scan(
                    str(job["id"]), worker_id, now=timestamp
                )
            paused = await self._store.checkpoint_operation_control(
                str(job["id"]), worker_id, now=timestamp
            )
            return paused or job

        degradation = init_degradation_context()
        try:
            tracks = [_to_grouping_track(row) for row in context["tracks"]]
            cached_release_groups: list[str] = []
            for track, row in zip(tracks, context["tracks"], strict=True):
                cached = await self._store.get_fingerprint_outcome(
                    track.local_track_id,
                    str(row["stat_revision"]),
                    FINGERPRINTER_VERSION,
                )
                if cached is not None:
                    cached_release_groups.extend(cached.release_group_ids)
                    if cached.state == "matched" and cached.recording_mbid:
                        track.recording_mbid = cached.recording_mbid
            candidates = await self._candidates.recall(
                tracks,
                cached_fingerprint_release_groups=list(
                    dict.fromkeys(cached_release_groups)
                ),
                explicit=True,
                checkpoint=checkpoint,
            )
            if not await checkpoint():
                return await halt()
            decision = self._evidence.decide(tracks, candidates)
            if self._fingerprints is not None and decision.outcome in (
                "ambiguous",
                "insufficient_evidence",
            ):
                new_release_groups: list[str] = []
                requested = 0
                for track, row in zip(tracks, context["tracks"], strict=True):
                    if requested >= MAX_NEW_FINGERPRINTS_PER_ATTEMPT:
                        break
                    supported_recordings = {
                        item.recording_mbid
                        for candidate in decision.candidates
                        for item in candidate.track_evidence
                        if item.local_track_id == track.local_track_id
                        and item.classification == "supported"
                        and item.recording_mbid
                    }
                    needed = len(supported_recordings) != 1
                    outcome = await self._fingerprints.fingerprint_if_needed(
                        local_track_id=track.local_track_id,
                        path=Path(str(row["file_path"])),
                        stat_revision=str(row["stat_revision"]),
                        needed=needed,
                        now=timestamp,
                        checkpoint=checkpoint,
                    )
                    if not needed:
                        continue
                    requested += 1
                    if not await checkpoint():
                        return await halt()
                    if outcome is not None and outcome.state == "failed":
                        raise ExternalServiceError(
                            "Fingerprint evidence is temporarily unavailable."
                        )
                    if outcome is not None and outcome.recording_mbid:
                        track.recording_mbid = outcome.recording_mbid
                        new_release_groups.extend(outcome.release_group_ids)
                if new_release_groups:
                    candidates = await self._candidates.recall(
                        tracks,
                        cached_fingerprint_release_groups=list(
                            dict.fromkeys([*cached_release_groups, *new_release_groups])
                        ),
                        explicit=True,
                        checkpoint=checkpoint,
                    )
                    if not await checkpoint():
                        return await halt()
                    decision = self._evidence.decide(tracks, candidates)
            attempt_id = str(uuid.uuid4())
            records = [
                IdentificationEvidenceRecord(
                    id=str(uuid.uuid4()),
                    attempt_id=attempt_id,
                    candidate_key=_candidate_key(candidate),
                    evidence=candidate,
                    created_at=timestamp,
                )
                for candidate in decision.candidates
            ]
            degraded = degradation.degraded_summary()
            attempt = IdentificationAttempt(
                id=attempt_id,
                local_album_id=str(work["local_album_id"]),
                trigger="explicit_reidentification",
                requested_by_user_id=job["requested_by_user_id"],
                input_tag_revision=revisions[0],
                input_file_revision=revisions[1],
                input_policy_revision=revisions[2],
                matcher_version=MATCHER_VERSION,
                state=decision.outcome,
                terminal_reason_code=(
                    "PROVIDER_TEMPORARILY_UNAVAILABLE"
                    if degraded and not records
                    else decision.reason_code
                ),
                candidate_count=len(records),
                degradation_flags=[
                    f"{source}:{status}" for source, status in sorted(degraded.items())
                ],
                started_at=timestamp,
                completed_at=timestamp,
            )
            return await self._store.finish_reidentification_evaluation(
                str(job["id"]),
                int(work["ordinal"]),
                worker_id=worker_id,
                expected_work_revision=int(work["row_revision"]),
                expected_album_revision=int(context["album"]["row_revision"]),
                attempt=attempt,
                evidence=records,
                now=timestamp,
            )
        except ExternalServiceError:
            attempt = IdentificationAttempt(
                id=str(uuid.uuid4()),
                local_album_id=str(work["local_album_id"]),
                trigger="explicit_reidentification",
                requested_by_user_id=job["requested_by_user_id"],
                input_tag_revision=revisions[0],
                input_file_revision=revisions[1],
                input_policy_revision=revisions[2],
                matcher_version=MATCHER_VERSION,
                state="provider_deferred",
                terminal_reason_code="PROVIDER_TEMPORARILY_UNAVAILABLE",
                started_at=timestamp,
                completed_at=timestamp,
            )
            return await self._store.finish_reidentification_evaluation(
                str(job["id"]),
                int(work["ordinal"]),
                worker_id=worker_id,
                expected_work_revision=int(work["row_revision"]),
                expected_album_revision=int(context["album"]["row_revision"]),
                attempt=attempt,
                evidence=[],
                now=timestamp,
            )
        finally:
            clear_degradation_context()

    async def select_candidate(
        self,
        job_id: str,
        *,
        expected_job_revision: int,
        candidate_key: str,
        confirmation: bool,
        actor_user_id: str,
        now: float | None = None,
    ) -> dict:
        return await self._store.accept_reidentification_candidate(
            job_id,
            expected_job_revision=expected_job_revision,
            candidate_key=candidate_key,
            confirmation=confirmation,
            actor_user_id=actor_user_id,
            now=time.time() if now is None else now,
        )

    @staticmethod
    def response(row: dict):
        return LibraryOperationService._response(row)
