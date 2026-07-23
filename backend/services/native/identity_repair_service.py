"""Snapshot-based existing-identity audit and explicit safe-detach Apply."""

from __future__ import annotations

import json
import time
import uuid
from collections.abc import Awaitable, Callable

from api.v1.schemas.library_operations import (
    IdentityPreparationCreateRequest,
    IdentityPreparationEstimateResponse,
    OperationListResponse,
    OperationResponse,
    RepairCreateRequest,
    RepairEstimateResponse,
    RepairFindingListResponse,
    RepairFindingResponse,
)
from core.exceptions import ExternalServiceError, ResourceNotFoundError, ValidationError
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.identification import (
    AlbumCandidate,
    CandidateTrack,
    IdentificationAttempt,
    IdentificationEvidenceRecord,
)
from models.library_work import OperationJob, RepairFinding
from repositories.protocols.identification import IdentificationProviderProtocol
from repositories.protocols.musicbrainz_management import (
    CanonicalMusicBrainzRepositoryProtocol,
    MbManagementRelease,
)
from services.native.album_evidence_engine import MATCHER_VERSION, AlbumEvidenceEngine
from services.native.album_identification_service import (
    _candidate_key,
    _to_grouping_track,
)
from services.native.conditional_fingerprint_service import FINGERPRINTER_VERSION
from services.native.identification_revisions import album_input_revisions
from services.native.library_operation_service import (
    LEASE_SECONDS,
    LibraryOperationService,
)

MANAGEMENT_READINESS_PURPOSE = "management_readiness"
MANAGEMENT_MAPPING_VERSION = "management-exact-release-v1"


class IdentityRepairService:
    def __init__(
        self,
        store: NativeLibraryStore,
        provider: IdentificationProviderProtocol | None = None,
        evidence: AlbumEvidenceEngine | None = None,
        canonical_provider: CanonicalMusicBrainzRepositoryProtocol | None = None,
    ) -> None:
        self._store = store
        self._provider = provider
        self._evidence = evidence or AlbumEvidenceEngine()
        self._canonical_provider = canonical_provider
        self._operations = LibraryOperationService(store)

    async def create(
        self,
        request: RepairCreateRequest,
        actor_user_id: str,
        *,
        now: float | None = None,
    ) -> OperationResponse:
        timestamp = time.time() if now is None else now
        job = OperationJob(
            id=str(uuid.uuid4()),
            kind="repair",
            requested_by_user_id=actor_user_id,
            input_catalog_revision=await self._store.get_catalog_revision(),
            idempotency_key=request.idempotency_key,
            created_at=timestamp,
        )
        row = await self._store.create_repair_operation(
            job,
            scope={
                "root_ids": request.root_ids,
                "legacy_only": request.source_matcher_version is None,
            },
            source_matcher_version=request.source_matcher_version,
            target_matcher_version=request.target_matcher_version,
        )
        return self._operations._response(row)

    async def create_management_preparation(
        self,
        request: IdentityPreparationCreateRequest,
        actor_user_id: str,
        *,
        now: float | None = None,
    ) -> OperationResponse:
        timestamp = time.time() if now is None else now
        job = OperationJob(
            id=str(uuid.uuid4()),
            kind="repair",
            requested_by_user_id=actor_user_id,
            input_catalog_revision=await self._store.get_catalog_revision(),
            idempotency_key=request.idempotency_key,
            created_at=timestamp,
        )
        row = await self._store.create_repair_operation(
            job,
            scope={
                "root_ids": request.root_ids,
                "legacy_only": False,
                "purpose": MANAGEMENT_READINESS_PURPOSE,
            },
            source_matcher_version=None,
            target_matcher_version=MANAGEMENT_MAPPING_VERSION,
        )
        return self._operations._response(row)

    async def estimate_management_preparation(
        self, root_ids: list[str]
    ) -> IdentityPreparationEstimateResponse:
        unique_root_ids = list(dict.fromkeys(root_ids))
        result = await self._store.estimate_management_identity_preparation(
            unique_root_ids
        )
        return IdentityPreparationEstimateResponse(
            album_count=result["album_count"],
            ready_album_count=result["ready_album_count"],
            mapping_required_count=result["mapping_required_count"],
            exact_release_required_count=result["exact_release_required_count"],
            selected_root_count=len(unique_root_ids),
            queued_preparation_count=result["queued_preparation_count"],
        )

    async def estimate(self, root_ids: list[str]) -> RepairEstimateResponse:
        unique_root_ids = list(dict.fromkeys(root_ids))
        result = await self._store.estimate_repair_operation(unique_root_ids)
        return RepairEstimateResponse(
            identity_count=result["identity_count"],
            selected_root_count=len(unique_root_ids),
            queued_repair_count=result["queued_repair_count"],
        )

    async def history(
        self,
        *,
        purpose: str,
        limit: int = 50,
        cursor: str | None = None,
    ) -> OperationListResponse:
        if limit < 1 or limit > 50:
            raise ValidationError(
                "Operation history page size must be between 1 and 50."
            )
        before_created_at: float | None = None
        before_id: str | None = None
        if cursor is not None:
            try:
                created, before_id = cursor.split(":", 1)
                before_created_at = float(created)
            except (TypeError, ValueError) as error:
                raise ValidationError(
                    "The operation history cursor is invalid."
                ) from error
        rows = await self._store.list_repair_operation_jobs(
            purpose=purpose,
            limit=limit + 1,
            before_created_at=before_created_at,
            before_id=before_id,
        )
        page = rows[:limit]
        return OperationListResponse(
            items=[await self._operations.get(str(row["id"])) for row in page],
            next_cursor=(
                f"{page[-1]['created_at']}:{page[-1]['id']}"
                if len(rows) > limit and page
                else None
            ),
        )

    async def get_for_purpose(self, job_id: str, purpose: str) -> OperationResponse:
        snapshot = await self._store.get_operation_snapshot(job_id)
        if snapshot is None or snapshot["snapshot"] is None:
            raise ResourceNotFoundError("Identity operation not found.")
        scope = json.loads(str(snapshot["snapshot"]["scope_json"]))
        actual = str(scope.get("purpose", "existing_matches"))
        if actual != purpose:
            raise ResourceNotFoundError("Identity operation not found.")
        return await self._operations.get(job_id)

    async def run_claimed_audit(
        self,
        job: dict,
        worker_id: str,
        *,
        now: float | None = None,
        checkpoint: Callable[[], Awaitable[None]] | None = None,
    ) -> OperationResponse:
        snapshot = await self._store.get_operation_snapshot(str(job["id"]))
        scope = (
            snapshot["snapshot"].get("scope_json")
            if snapshot is not None and snapshot["snapshot"] is not None
            else None
        )
        purpose = (
            str(json.loads(str(scope)).get("purpose", "existing_matches"))
            if scope
            else "existing_matches"
        )
        while True:
            timestamp = time.time() if now is None else now
            controlled = await self._store.checkpoint_operation_control(
                str(job["id"]), worker_id, now=timestamp
            )
            if controlled is not None and controlled["state"] != "running":
                return self._operations._response(controlled)
            work = await self._store.claim_operation_work(
                str(job["id"]), worker_id, now=timestamp
            )
            if work is None:
                await self._store.mark_repair_ready(
                    str(job["id"]), worker_id, now=timestamp
                )
                return await self._operations.get(str(job["id"]))
            context = await self._store.get_album_identification_context(
                str(work["local_album_id"])
            )
            renewed = await self._store.heartbeat_operation_job(
                str(job["id"]),
                worker_id,
                now=timestamp,
                lease_seconds=LEASE_SECONDS,
            )
            if not renewed:
                raise ResourceNotFoundError("The identity check lease changed.")
            if purpose == MANAGEMENT_READINESS_PURPOSE:
                finding, attempt, evidence = await self._classify_management_readiness(
                    str(job["id"]), work, context, timestamp
                )
            else:
                finding, attempt, evidence = await self._classify(
                    str(job["id"]), work, context
                )
            await self._store.save_repair_finding_for_work(
                str(job["id"]),
                int(work["ordinal"]),
                worker_id=worker_id,
                expected_work_revision=int(work["row_revision"]),
                finding=finding,
                attempt=attempt,
                evidence=evidence,
                now=timestamp,
            )
            if checkpoint is not None:
                await checkpoint()

    async def _classify_management_readiness(
        self,
        job_id: str,
        work: dict,
        context: dict | None,
        timestamp: float,
    ) -> tuple[
        RepairFinding,
        IdentificationAttempt | None,
        list[IdentificationEvidenceRecord],
    ]:
        album_id = str(work["local_album_id"])
        if context is None:
            return (
                self._finding(job_id, work, "stale", "IDENTITY_CHANGED", False),
                None,
                [],
            )
        identity = context["identity"]
        if (
            identity is None
            or not identity["release_group_mbid"]
            or not identity["release_mbid"]
        ):
            return (
                self._finding(
                    job_id,
                    work,
                    "exact_release_required",
                    "EXACT_EDITION_NOT_ACCEPTED",
                    False,
                    identity_revision=(
                        int(identity["row_revision"]) if identity is not None else None
                    ),
                ),
                None,
                [],
            )
        tracks = context["tracks"]
        release_track_ids = [
            str(row["release_track_mbid"])
            for row in tracks
            if row["release_track_mbid"]
        ]
        complete = (
            bool(tracks)
            and all(
                row["recording_mbid"]
                and row["release_track_mbid"]
                and row["identity_release_mbid"] == identity["release_mbid"]
                and row["medium_position"] is not None
                and row["release_track_position"] is not None
                for row in tracks
            )
            and len(set(release_track_ids)) == len(tracks)
        )
        if complete:
            return (
                self._finding(
                    job_id,
                    work,
                    "ready",
                    "EXACT_RELEASE_MAPPINGS_PRESENT",
                    False,
                    identity_revision=int(identity["row_revision"]),
                ),
                None,
                [],
            )
        if self._canonical_provider is None:
            return (
                self._finding(
                    job_id,
                    work,
                    "unverifiable",
                    "PROVIDER_DEFERRED",
                    False,
                    identity_revision=int(identity["row_revision"]),
                ),
                None,
                [],
            )
        try:
            release = await self._canonical_provider.get_canonical_release(
                str(identity["release_mbid"]),
                includes=("artist-credits", "recordings", "release-groups"),
                priority=RequestPriority.BACKGROUND_SYNC,
            )
        except ExternalServiceError:
            return (
                self._finding(
                    job_id,
                    work,
                    "unverifiable",
                    "PROVIDER_DEFERRED",
                    False,
                    identity_revision=int(identity["row_revision"]),
                ),
                None,
                [],
            )
        if release is None:
            return (
                self._finding(
                    job_id,
                    work,
                    "needs_review",
                    "SELECTED_RELEASE_UNAVAILABLE",
                    False,
                    identity_revision=int(identity["row_revision"]),
                ),
                None,
                [],
            )
        if (
            release.id != identity["release_mbid"]
            or release.release_group.id != identity["release_group_mbid"]
        ):
            return (
                self._finding(
                    job_id,
                    work,
                    "needs_review",
                    "SELECTED_RELEASE_CONFLICT",
                    False,
                    identity_revision=int(identity["row_revision"]),
                ),
                None,
                [],
            )
        local_tracks = [_to_grouping_track(row) for row in tracks]
        for local, row in zip(local_tracks, tracks, strict=True):
            local.recording_mbid = (
                row["recording_mbid"] or row["embedded_recording_mbid"]
            )
            local.release_mbid = str(identity["release_mbid"])
            local.release_group_mbid = str(identity["release_group_mbid"])
        evaluated = self._evidence.evaluate_candidate(
            local_tracks, self._management_candidate(release)
        )
        proposed = [
            item
            for item in evaluated.track_evidence
            if item.classification == "supported"
        ]
        release_tracks = [item.release_track_mbid for item in proposed]
        safe = (
            bool(tracks)
            and bool(proposed)
            and all(
                (
                    item.recording_mbid
                    and item.release_track_mbid
                    and item.candidate_disc_number is not None
                    and item.candidate_track_position is not None
                )
                for item in proposed
            )
        )
        safe = bool(
            safe
            and evaluated.reason_code == "SUPPORTED"
            and len(proposed) == len(tracks)
            and len(set(release_tracks)) == len(release_tracks)
        )
        if safe:
            by_id = {item.local_track_id: item for item in proposed}
            for row in tracks:
                item = by_id[str(row["id"])]
                if any(
                    (
                        row["recording_mbid"]
                        and row["recording_mbid"] != item.recording_mbid,
                        row["identity_release_mbid"]
                        and row["identity_release_mbid"] != release.id,
                        row["release_track_mbid"]
                        and row["release_track_mbid"] != item.release_track_mbid,
                        row["embedded_release_group_mbid"]
                        and row["embedded_release_group_mbid"]
                        != release.release_group.id,
                        row["embedded_release_mbid"]
                        and row["embedded_release_mbid"] != release.id,
                        row["embedded_recording_mbid"]
                        and row["embedded_recording_mbid"] != item.recording_mbid,
                        row["embedded_release_track_mbid"]
                        and row["embedded_release_track_mbid"]
                        != item.release_track_mbid,
                    )
                ):
                    safe = False
                    evaluated.reason_code = "CONFLICTING_TRACK_EVIDENCE"
                    break
        revisions = album_input_revisions(tracks)
        attempt_id = str(uuid.uuid4())
        evidence_id = str(uuid.uuid4())
        attempt = IdentificationAttempt(
            id=attempt_id,
            local_album_id=album_id,
            trigger="management_identity_preparation",
            input_tag_revision=revisions[0],
            input_file_revision=revisions[1],
            input_policy_revision=revisions[2],
            matcher_version=MANAGEMENT_MAPPING_VERSION,
            state="identified" if safe else "contradictory",
            terminal_reason_code=(
                "EXACT_RELEASE_MAPPING_SUPPORTED" if safe else evaluated.reason_code
            ),
            selected_candidate_key=_candidate_key(evaluated) if safe else None,
            candidate_count=1,
            started_at=timestamp,
            completed_at=timestamp,
        )
        record = IdentificationEvidenceRecord(
            id=evidence_id,
            attempt_id=attempt_id,
            candidate_key=_candidate_key(evaluated),
            evidence=evaluated,
            created_at=timestamp,
        )
        return (
            self._finding(
                job_id,
                work,
                "mapping_ready" if safe else "needs_review",
                ("EXACT_RELEASE_MAPPING_SUPPORTED" if safe else evaluated.reason_code),
                safe,
                evidence_id=evidence_id,
                identity_revision=int(identity["row_revision"]),
            ),
            attempt,
            [record],
        )

    @staticmethod
    def _management_candidate(release: MbManagementRelease) -> AlbumCandidate:
        absolute = 0
        tracks: list[CandidateTrack] = []
        for medium in release.media:
            for track in medium.tracks:
                absolute += 1
                duration = track.length or track.recording.length
                tracks.append(
                    CandidateTrack(
                        title=track.title or track.recording.title,
                        position=track.position,
                        disc_number=medium.position,
                        absolute_position=absolute,
                        duration_seconds=(duration / 1000.0 if duration else None),
                        recording_mbid=track.recording.id or None,
                        release_track_mbid=track.id or None,
                    )
                )
        album_artist = "".join(
            f"{credit.name or credit.artist.name}{credit.joinphrase}"
            for credit in release.artist_credit
        ).strip()
        first_artist = (
            release.artist_credit[0].artist if release.artist_credit else None
        )
        return AlbumCandidate(
            release_group_mbid=release.release_group.id,
            release_mbid=release.id,
            album_title=release.title or release.release_group.title,
            album_artist_name=album_artist,
            artist_mbid=first_artist.id if first_artist is not None else None,
            tracks=tracks,
            release_type=(
                release.release_group.primary_type.casefold()
                if release.release_group.primary_type
                else None
            ),
            secondary_types=[
                value.casefold() for value in release.release_group.secondary_types
            ],
            release_date=release.date or release.release_group.first_release_date,
            source_kinds=["accepted_exact_release"],
        )

    async def begin_apply(
        self,
        job_id: str,
        *,
        expected_row_revision: int,
        confirmation: bool,
        now: float | None = None,
    ) -> OperationResponse:
        if not confirmation:
            raise ValidationError(
                "Confirm the repair report before applying safe detachments."
            )
        snapshot = await self._store.get_operation_snapshot(job_id)
        if snapshot is None or snapshot["snapshot"] is None:
            raise ResourceNotFoundError("Repair job not found.")
        scope = json.loads(str(snapshot["snapshot"]["scope_json"]))
        if scope.get("purpose") == MANAGEMENT_READINESS_PURPOSE:
            raise ResourceNotFoundError("Repair job not found.")
        row = await self._store.start_repair_apply(
            job_id,
            expected_row_revision=expected_row_revision,
            now=time.time() if now is None else now,
        )
        return self._operations._response(row)

    async def begin_management_preparation_apply(
        self,
        job_id: str,
        *,
        expected_row_revision: int,
        confirmation: bool,
        now: float | None = None,
    ) -> OperationResponse:
        if not confirmation:
            raise ValidationError(
                "Confirm the exact-release mapping report before accepting it."
            )
        snapshot = await self._store.get_operation_snapshot(job_id)
        if snapshot is None or snapshot["snapshot"] is None:
            raise ResourceNotFoundError("Identity preparation job not found.")
        scope = json.loads(str(snapshot["snapshot"]["scope_json"]))
        if scope.get("purpose") != MANAGEMENT_READINESS_PURPOSE:
            raise ResourceNotFoundError("Identity preparation job not found.")
        row = await self._store.start_repair_apply(
            job_id,
            expected_row_revision=expected_row_revision,
            now=time.time() if now is None else now,
        )
        return self._operations._response(row)

    async def discard_management_preparation(
        self,
        job_id: str,
        *,
        expected_row_revision: int,
        now: float | None = None,
    ) -> OperationResponse:
        row = await self._store.discard_management_identity_preparation(
            job_id,
            expected_row_revision=expected_row_revision,
            now=time.time() if now is None else now,
        )
        return self._operations._response(row)

    async def run_claimed_apply(
        self,
        job: dict,
        worker_id: str,
        actor_user_id: str,
        *,
        now: float | None = None,
        checkpoint: Callable[[], Awaitable[None]] | None = None,
    ) -> OperationResponse:
        timestamp = time.time() if now is None else now
        while True:
            controlled = await self._store.checkpoint_operation_control(
                str(job["id"]), worker_id, now=timestamp
            )
            if controlled is not None and controlled["state"] != "running":
                return self._operations._response(controlled)
            work = await self._store.claim_operation_work(
                str(job["id"]), worker_id, now=timestamp
            )
            if work is None:
                done = await self._store.finish_operation_job(
                    str(job["id"]),
                    worker_id,
                    state="succeeded",
                    terminal_code="APPLY_COMPLETED",
                    now=timestamp,
                )
                return self._operations._response(done)
            await self._store.apply_repair_work(
                str(job["id"]),
                int(work["ordinal"]),
                worker_id=worker_id,
                expected_work_revision=int(work["row_revision"]),
                actor_user_id=actor_user_id,
                now=timestamp,
            )
            if checkpoint is not None:
                await checkpoint()

    async def findings(
        self,
        job_id: str,
        *,
        limit: int = 100,
        cursor: str | None = None,
        finding_category: str | None = None,
    ) -> RepairFindingListResponse:
        if limit < 1 or limit > 200:
            raise ValidationError("Repair finding page size must be between 1 and 200.")
        snapshot = await self._store.get_operation_snapshot(job_id)
        if snapshot is None or snapshot["snapshot"] is None:
            raise ResourceNotFoundError("Repair job not found.")
        scope = json.loads(str(snapshot["snapshot"]["scope_json"]))
        if scope.get("purpose") == MANAGEMENT_READINESS_PURPOSE:
            categories = {
                "ready": ["ready"],
                "mapping_ready": ["mapping_ready"],
                "exact_release_required": ["exact_release_required"],
                "needs_review": ["needs_review"],
                "unverifiable": ["unverifiable", "stale"],
            }
        else:
            categories = {
                "valid": ["valid"],
                "safe_detach": ["safe_detach"],
                "needs_review": ["needs_review"],
                "unverifiable": ["unverifiable", "stale"],
                "manual_identity": ["manual_identity"],
            }
        if finding_category is not None and finding_category not in categories:
            raise ValidationError("The repair finding category is invalid.")
        cursor_updated_at: float | None = None
        cursor_id: str | None = None
        if cursor is not None:
            try:
                updated, cursor_id = cursor.split(":", 1)
                cursor_updated_at = float(updated)
            except (TypeError, ValueError) as error:
                raise ValidationError(
                    "The repair finding cursor is invalid."
                ) from error
        result = await self._store.list_repair_findings(
            job_id,
            limit=limit,
            finding_codes=categories.get(finding_category),
            cursor_updated_at=cursor_updated_at,
            cursor_id=cursor_id,
        )
        rows = result["rows"]
        next_cursor = None
        if result["has_more"] and rows:
            next_cursor = f"{rows[-1]['updated_at']}:{rows[-1]['id']}"
        return RepairFindingListResponse(
            items=[
                RepairFindingResponse(
                    id=str(row["id"]),
                    local_album_id=str(row["local_album_id"]),
                    evidence_id=row["evidence_id"],
                    review_id=row["review_id"],
                    finding_code=str(row["finding_code"]),
                    reason_code=str(row["reason_code"]),
                    confidence=str(row["confidence"]),
                    apply_eligible=bool(row["apply_eligible"]),
                    state=str(row["state"]),
                    apply_result=row["apply_result"],
                    updated_at=float(row["updated_at"]),
                    row_revision=int(row["row_revision"]),
                )
                for row in rows
            ],
            next_cursor=next_cursor,
            has_more=bool(result["has_more"]),
        )

    async def _classify(
        self, job_id: str, work: dict, context: dict | None
    ) -> tuple[
        RepairFinding,
        IdentificationAttempt | None,
        list[IdentificationEvidenceRecord],
    ]:
        album_id = str(work["local_album_id"])
        if context is None or context["identity"] is None:
            return (
                self._finding(job_id, work, "stale", "IDENTITY_CHANGED", False),
                None,
                [],
            )
        identity = context["identity"]
        if identity["decision_source"] == "manual":
            return (
                self._finding(
                    job_id,
                    work,
                    "manual_identity",
                    "MANUAL_IDENTITY_REPORT_ONLY",
                    False,
                    identity_revision=int(identity["row_revision"]),
                ),
                None,
                [],
            )
        stored = await self._store.get_selected_album_evidence(album_id)
        if stored is None:
            stored = await self._store.get_latest_album_candidate_evidence(
                album_id,
                f"{identity['release_group_mbid']}:{identity['release_mbid'] or ''}",
            )
        attempt: IdentificationAttempt | None = None
        records: list[IdentificationEvidenceRecord] = []
        provider_deferred = False
        if stored is None and self._provider is not None:
            try:
                candidate = await self._provider.get_album_candidate(
                    str(identity["release_group_mbid"]),
                    len(context["tracks"]),
                    RequestPriority.BACKGROUND_SYNC,
                )
            except ExternalServiceError:
                candidate = None
                provider_deferred = True
            if candidate is not None:
                tracks = [_to_grouping_track(row) for row in context["tracks"]]
                for track, row in zip(tracks, context["tracks"], strict=True):
                    cached = await self._store.get_fingerprint_outcome(
                        track.local_track_id,
                        str(row["stat_revision"]),
                        FINGERPRINTER_VERSION,
                    )
                    if (
                        cached is not None
                        and cached.state == "matched"
                        and cached.recording_mbid
                    ):
                        track.recording_mbid = cached.recording_mbid
                evaluated = self._evidence.evaluate_candidate(tracks, candidate)
                attempt_id = str(uuid.uuid4())
                evidence_id = str(uuid.uuid4())
                revisions = album_input_revisions(context["tracks"])
                attempt = IdentificationAttempt(
                    id=attempt_id,
                    local_album_id=album_id,
                    trigger="repair_audit",
                    input_tag_revision=revisions[0],
                    input_file_revision=revisions[1],
                    input_policy_revision=revisions[2],
                    matcher_version=MATCHER_VERSION,
                    state=(
                        "identified"
                        if evaluated.reason_code == "SUPPORTED"
                        else "contradictory"
                    ),
                    terminal_reason_code=evaluated.reason_code,
                    selected_candidate_key=_candidate_key(evaluated),
                    candidate_count=1,
                    started_at=float(work["updated_at"]),
                    completed_at=float(work["updated_at"]),
                )
                stored = IdentificationEvidenceRecord(
                    id=evidence_id,
                    attempt_id=attempt_id,
                    candidate_key=_candidate_key(evaluated),
                    evidence=evaluated,
                    created_at=float(work["updated_at"]),
                )
                records = [stored]
        if stored is None:
            return (
                self._finding(
                    job_id,
                    work,
                    "unverifiable",
                    "PROVIDER_DEFERRED"
                    if provider_deferred
                    else "EVIDENCE_UNAVAILABLE",
                    False,
                    identity_revision=int(identity["row_revision"]),
                ),
                None,
                [],
            )
        supported = sum(
            item.classification == "supported"
            for item in stored.evidence.track_evidence
        )
        contradictory = sum(
            item.classification == "contradictory"
            for item in stored.evidence.track_evidence
        )
        complete = not stored.evidence.unmatched_expected_tracks
        safe = complete and (supported == 0 or contradictory > 0)
        if safe:
            finding_code = "safe_detach"
            reason = "ZERO_SUPPORT" if supported == 0 else "HARD_CONTRADICTION"
        elif stored.evidence.reason_code in {
            "ACCEPTED",
            "SUPPORTED",
            "SUPPORTED_EMBEDDED_IDS",
        }:
            finding_code = "valid"
            reason = "CURRENT_IDENTITY_PASSES"
        else:
            finding_code = "needs_review"
            reason = "NON_TERMINAL_SAFETY_CONCERN"
        return (
            self._finding(
                job_id,
                work,
                finding_code,
                reason,
                safe,
                evidence_id=stored.id,
                identity_revision=int(identity["row_revision"]),
            ),
            attempt,
            records,
        )

    @staticmethod
    def _finding(
        job_id: str,
        work: dict,
        finding_code: str,
        reason_code: str,
        apply_eligible: bool,
        *,
        evidence_id: str | None = None,
        identity_revision: int | None = None,
    ) -> RepairFinding:
        return RepairFinding(
            id=str(
                uuid.uuid5(
                    uuid.NAMESPACE_URL,
                    f"{job_id}:{work['local_album_id']}:{finding_code}",
                )
            ),
            local_album_id=str(work["local_album_id"]),
            expected_album_revision=int(work["expected_subject_revision"]),
            expected_identity_revision=identity_revision,
            finding_code=finding_code,
            reason_code=reason_code,
            confidence="complete" if apply_eligible else "bounded",
            apply_eligible=apply_eligible,
            evidence_id=evidence_id,
        )
