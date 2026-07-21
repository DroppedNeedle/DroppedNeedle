"""Durable target identification queue policy and controls."""

from __future__ import annotations

import time
import uuid

from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_work import IdentificationJob

PRIORITY_NEW_OR_CHANGED = 20
PRIORITY_REVIEW_RETRY = 30
PRIORITY_HISTORICAL_BACKLOG = 40
PRIORITY_SUPPORTING_MAINTENANCE = 50
LEASE_SECONDS = 60.0
MAX_BACKOFF_SECONDS = 6 * 60 * 60


class IdentificationQueueService:
    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    async def enqueue_album(
        self,
        album_id: str,
        *,
        input_revision: str,
        kind: str = "automatic",
        historical: bool = False,
        requested_by_user_id: str | None = None,
        now: float | None = None,
        expected_policy_revision: str | None = None,
    ) -> str:
        job = self._album_job(
            album_id,
            input_revision=input_revision,
            kind=kind,
            historical=historical,
            requested_by_user_id=requested_by_user_id,
            now=now,
        )
        return await self._store.enqueue_identification_job(
            job, expected_policy_revision=expected_policy_revision
        )

    async def enqueue_album_with_disposition(
        self,
        album_id: str,
        *,
        input_revision: str,
        kind: str = "automatic",
        historical: bool = False,
        requested_by_user_id: str | None = None,
        now: float | None = None,
    ) -> tuple[str, bool]:
        job = self._album_job(
            album_id,
            input_revision=input_revision,
            kind=kind,
            historical=historical,
            requested_by_user_id=requested_by_user_id,
            now=now,
        )
        return await self._store.enqueue_identification_job_result(job)

    async def enqueue_albums_with_disposition(
        self,
        albums: list[tuple[str, str, str]],
        *,
        now: float | None = None,
        scan_run_id: str | None = None,
        grouping_context: tuple[str, str] | None = None,
        queue_cursor: str | None = None,
        background: bool = False,
    ) -> list[tuple[str, bool]]:
        jobs = [
            self._album_job(
                album_id,
                input_revision=input_revision,
                kind=kind,
                historical=False,
                requested_by_user_id=None,
                now=now,
            )
            for album_id, input_revision, kind in albums
        ]
        return await self._store.enqueue_identification_job_results(
            jobs,
            scan_run_id=scan_run_id,
            grouping_context=grouping_context,
            queue_cursor=queue_cursor,
            background=background,
        )

    @staticmethod
    def _album_job(
        album_id: str,
        *,
        input_revision: str,
        kind: str,
        historical: bool,
        requested_by_user_id: str | None,
        now: float | None,
    ) -> IdentificationJob:
        timestamp = time.time() if now is None else now
        if kind == "review_retry":
            priority = PRIORITY_REVIEW_RETRY
        elif kind == "post_processing":
            priority = PRIORITY_SUPPORTING_MAINTENANCE
        elif historical:
            priority = PRIORITY_HISTORICAL_BACKLOG
        else:
            priority = PRIORITY_NEW_OR_CHANGED
        job = IdentificationJob(
            id=str(uuid.uuid4()),
            local_album_id=album_id,
            kind=kind,
            priority=priority,
            dedupe_key=f"{kind}:{album_id}:{input_revision}",
            input_revision=input_revision,
            requested_by_user_id=requested_by_user_id,
            created_at=timestamp,
        )
        return job

    async def claim(self, worker_id: str, *, now: float | None = None) -> dict | None:
        return await self._store.claim_identification_job(
            worker_id,
            now=time.time() if now is None else now,
            lease_seconds=LEASE_SECONDS,
        )

    async def defer(
        self,
        job: dict,
        worker_id: str,
        failure_code: str,
        *,
        now: float | None = None,
    ) -> int:
        timestamp = time.time() if now is None else now
        attempts = max(1, int(job.get("attempt_count", 1)))
        backoff = min(MAX_BACKOFF_SECONDS, 30 * (2 ** min(attempts - 1, 10)))
        return await self._store.defer_identification_job(
            str(job["id"]),
            worker_id=worker_id,
            expected_job_revision=int(job["row_revision"]),
            failure_code=failure_code,
            not_before=timestamp + backoff,
            now=timestamp,
        )

    async def pause(
        self,
        requested_by_user_id: str | None,
        *,
        expected_revision: int | None = None,
        now: float | None = None,
    ) -> int:
        return await self._store.pause_identification_queue(
            requested_by_user_id=requested_by_user_id,
            requested_at=time.time() if now is None else now,
            expected_revision=expected_revision,
        )

    async def checkpoint_pause(
        self,
        job: dict,
        worker_id: str,
        checkpoint: dict,
        *,
        now: float | None = None,
    ) -> int:
        return await self._store.checkpoint_identification_pause(
            str(job["id"]),
            worker_id=worker_id,
            expected_job_revision=int(job["row_revision"]),
            checkpoint=checkpoint,
            now=time.time() if now is None else now,
        )

    async def resume(
        self, *, expected_revision: int | None = None, now: float | None = None
    ) -> int:
        return await self._store.resume_identification_queue(
            resumed_at=time.time() if now is None else now,
            expected_revision=expected_revision,
        )

    async def is_paused(self) -> bool:
        return (await self._store.get_identification_control())["state"] == "paused"

    async def recover(self, *, now: float | None = None) -> int:
        return await self._store.recover_expired_identification_leases(
            now=time.time() if now is None else now
        )

    async def activity_snapshot(self) -> dict:
        return await self._store.get_identification_activity_snapshot()

    async def stream_revisions(self) -> dict[str, int]:
        return {
            kind: await self._store.get_stream_revision(kind)
            for kind in ("scan", "identification", "operation")
        }
