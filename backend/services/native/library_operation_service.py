"""Shared durable control and bounded worker for administrator operations."""

from __future__ import annotations

import time
from collections.abc import Awaitable, Callable
import json

from api.v1.schemas.library_operations import (
    OperationListResponse,
    OperationResponse,
    OperationWorkResult,
    RepairReportSummary,
    ReviewCandidateDetail,
)
from core.exceptions import ResourceNotFoundError, ValidationError
from infrastructure.persistence.native_library_store import NativeLibraryStore

LEASE_SECONDS = 60.0
AUTOMATIC_SAFE_EVIDENCE_REASONS = frozenset(
    {"SUPPORTED", "ACCEPTED", "SUPPORTED_EMBEDDED_IDS"}
)


class LibraryOperationService:
    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    async def get(self, job_id: str) -> OperationResponse:
        row = await self._store.get_operation_job(job_id)
        if row is None:
            raise ResourceNotFoundError("Library operation not found.")
        raw_results = await self._store.list_operation_work_results(job_id)
        results = [
            OperationWorkResult(
                ordinal=int(result["ordinal"]),
                local_album_id=result["local_album_id"],
                local_track_id=result["local_track_id"],
                action=str(result["action"]),
                state=str(result["state"]),
                failure_code=result["failure_code"],
                result=json.loads(str(result["result_json"]))
                if result["result_json"]
                else {},
            )
            for result in raw_results[:100]
        ]
        repair_summary = None
        reidentification_candidates: list[ReviewCandidateDetail] = []
        if row["kind"] == "explicit_reidentification" and results:
            attempt_id = results[0].result.get("attempt_id")
            if isinstance(attempt_id, str):
                evidence = await self._store.get_attempt_evidence(attempt_id)
                reidentification_candidates = sorted(
                    [
                        ReviewCandidateDetail(
                            candidate_key=item.candidate_key,
                            evidence_revision=item.id,
                            evidence=item.evidence,
                            automatic_safe=item.evidence.reason_code
                            in AUTOMATIC_SAFE_EVIDENCE_REASONS,
                        )
                        for item in evidence
                    ],
                    key=lambda item: (
                        not item.automatic_safe,
                        -item.evidence.score,
                        item.candidate_key,
                    ),
                )
        if row["kind"] == "repair":
            operation = await self._store.get_operation_snapshot(job_id)
            raw_summary = (
                json.loads(str(operation["snapshot"]["result_json"]))
                if operation is not None
                and operation["snapshot"] is not None
                and operation["snapshot"]["result_json"]
                else None
            )
            if raw_summary is not None:
                repair_summary = RepairReportSummary(**raw_summary)
        return self._response(
            row,
            results=results,
            results_truncated=len(raw_results) > 100,
            repair_summary=repair_summary,
            reidentification_candidates=reidentification_candidates,
        )

    async def history(
        self,
        *,
        kind: str | None = None,
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
        rows = await self._store.list_operation_jobs(
            kind=kind,
            limit=limit + 1,
            before_created_at=before_created_at,
            before_id=before_id,
        )
        items = rows[:limit]
        return OperationListResponse(
            items=[self._response(row) for row in items],
            next_cursor=(
                f"{items[-1]['created_at']}:{items[-1]['id']}"
                if len(rows) > limit and items
                else None
            ),
        )

    async def control(
        self,
        job_id: str,
        control: str,
        expected_row_revision: int,
        *,
        now: float | None = None,
    ) -> OperationResponse:
        row = await self._store.request_operation_control(
            job_id,
            control=control,
            expected_row_revision=expected_row_revision,
            now=time.time() if now is None else now,
        )
        return self._response(row)

    async def recover(self, *, now: float | None = None) -> int:
        return await self._store.recover_expired_operation_leases(
            now=time.time() if now is None else now
        )

    async def run_bulk_claimed(
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
                return self._response(controlled)
            staged = await self._store.stage_bulk_review_operation_batch(
                str(job["id"]), worker_id, now=timestamp
            )
            if staged["complete"]:
                break
        while True:
            controlled = await self._store.checkpoint_operation_control(
                str(job["id"]), worker_id, now=timestamp
            )
            if controlled is not None and controlled["state"] != "running":
                return self._response(controlled)
            work = await self._store.claim_operation_work(
                str(job["id"]), worker_id, now=timestamp
            )
            if work is None:
                done = await self._store.finish_operation_job(
                    str(job["id"]),
                    worker_id,
                    state="succeeded",
                    terminal_code="COMPLETED",
                    now=timestamp,
                )
                return self._response(done)
            await self._store.apply_bulk_review_work(
                str(job["id"]),
                int(work["ordinal"]),
                worker_id=worker_id,
                expected_work_revision=int(work["row_revision"]),
                actor_user_id=actor_user_id,
                now=timestamp,
            )
            if checkpoint is not None:
                await checkpoint()

    async def claim(self, worker_id: str, *, now: float | None = None) -> dict | None:
        timestamp = time.time() if now is None else now
        return await self._store.claim_operation_job(
            worker_id, now=timestamp, lease_seconds=LEASE_SECONDS
        )

    @staticmethod
    def _response(
        row: dict,
        *,
        results: list[OperationWorkResult] | None = None,
        results_truncated: bool = False,
        repair_summary: RepairReportSummary | None = None,
        reidentification_candidates: list[ReviewCandidateDetail] | None = None,
    ) -> OperationResponse:
        return OperationResponse(
            id=str(row["id"]),
            kind=str(row["kind"]),
            state=str(row["state"]),
            expected_work_count=int(row["expected_work_count"]),
            completed_count=int(row["completed_count"]),
            succeeded_count=int(row["succeeded_count"]),
            failed_count=int(row["failed_count"]),
            skipped_count=int(row["skipped_count"]),
            control_request=str(row["control_request"]),
            terminal_code=row["terminal_code"],
            row_revision=int(row["row_revision"]),
            event_revision=int(row["event_revision"]),
            created_at=float(row["created_at"]),
            updated_at=float(row["updated_at"]),
            results=results or [],
            results_truncated=results_truncated,
            repair_summary=repair_summary,
            reidentification_candidates=reidentification_candidates or [],
        )
