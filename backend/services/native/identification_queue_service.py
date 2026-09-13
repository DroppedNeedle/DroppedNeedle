"""Durable target identification queue policy and controls."""

from __future__ import annotations

import time
import uuid
from collections.abc import Callable

from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_work import IdentificationJob

PRIORITY_NEW_OR_CHANGED = 20
PRIORITY_REVIEW_RETRY = 30
PRIORITY_HISTORICAL_BACKLOG = 40
PRIORITY_SUPPORTING_MAINTENANCE = 50
LEASE_SECONDS = 60.0
# F-IDENT-04 policy (owner-signed): the cap is DERIVED so it always equals the
# largest delay the doubling formula can actually schedule under the retained
# ten-attempt terminal bound. Applied sequence: 30, 60, 120, 240, 480, 960,
# 1,920, 3,840, 7,680 seconds - cumulative 15,330 seconds before attempt ten
# terminalizes. This is a bounded-retry window, never a provider-health
# timeout; provider recovery is gated separately by resurrection eligibility.
MAX_DEFERRAL_ATTEMPTS = 10
MAX_BACKOFF_SECONDS = 30 * 2 ** min(MAX_DEFERRAL_ATTEMPTS - 2, 10)
# F-056: attempt caps are monotonic modulo the store's bounded two-wipe
# provider-reset budget; recovery releases only reset-stale or far-future rows.
SUBJECT_NOT_AVAILABLE_GRACE_SECONDS = 24 * 60 * 60


class IdentificationQueueService:
    def __init__(
        self,
        store: NativeLibraryStore,
        *,
        provider_available: Callable[[], bool] | None = None,
    ) -> None:
        self._store = store
        self._provider_available = provider_available

    def _resurrect_attention(self) -> bool:
        if self._provider_available is None:
            return True
        return self._provider_available()

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
            job,
            expected_policy_revision=expected_policy_revision,
            resurrect_attention=self._resurrect_attention(),
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
        return await self._store.enqueue_identification_job_result(
            job, resurrect_attention=self._resurrect_attention()
        )

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
            resurrect_attention=self._resurrect_attention(),
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
        retry_after_seconds: float | None = None,
    ) -> int:
        timestamp = time.time() if now is None else now
        attempts = max(1, int(job.get("attempt_count", 1)))
        if attempts >= MAX_DEFERRAL_ATTEMPTS:
            return await self._store.terminal_fail_identification_job(
                str(job["id"]),
                worker_id=worker_id,
                expected_job_revision=int(job["row_revision"]),
                failure_code="MAX_DEFERRALS_EXCEEDED",
                attention_cause=failure_code,
                now=timestamp,
            )
        backoff = min(MAX_BACKOFF_SECONDS, 30 * (2 ** min(attempts - 1, 10)))
        # Use max(existing safe policy 30s+ backoff, exception deadline) per F-PERF-01
        if retry_after_seconds is not None:
            try:
                candidate = float(retry_after_seconds)
                import math

                if math.isfinite(candidate) and candidate > 0:
                    backoff = max(backoff, candidate)
            except (TypeError, ValueError):
                pass
        return await self._store.defer_identification_job(
            str(job["id"]),
            worker_id=worker_id,
            expected_job_revision=int(job["row_revision"]),
            failure_code=failure_code,
            not_before=timestamp + backoff,
            now=timestamp,
        )

    async def release(
        self,
        job: dict,
        worker_id: str,
        *,
        now: float | None = None,
    ) -> int:
        """R-04: release a cancelled run's claim with NO backoff.

        Cancel-path primitive - never defer() here (30s floor +
        MAX_DEFERRALS_EXCEEDED counting). NOT idempotent: call exactly once
        per cancel; StaleRevisionError means the finish commit already landed
        (commit wins) and the caller swallows it.
        """
        return await self._store.release_identification_claim(
            str(job["id"]),
            worker_id=worker_id,
            expected_job_revision=int(job["row_revision"]),
            now=time.time() if now is None else now,
        )

    async def fail(
        self,
        job: dict,
        worker_id: str,
        failure_code: str,
        *,
        now: float | None = None,
    ) -> int:
        """Terminally fail a running job, keeping the row for auditability."""
        return await self._store.terminal_fail_identification_job(
            str(job["id"]),
            worker_id=worker_id,
            expected_job_revision=int(job["row_revision"]),
            failure_code=failure_code,
            attention_cause=failure_code,
            now=time.time() if now is None else now,
        )

    async def reset_provider_deferrals(self, *, now: float | None = None) -> int:
        """F-056: recovery-edge release with preserved failure history; the
        staleness window is one full backoff quantum."""
        return await self._store.reset_provider_identification_deferrals(
            now=time.time() if now is None else now,
            staleness_seconds=MAX_BACKOFF_SECONDS,
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
        expected_job_revision_override: int | None = None,
    ) -> int:
        """F-057: lease heartbeats bump row_revision mid-run, so pausing uses
        the freshest known revision when the caller supplies one."""
        return await self._store.checkpoint_identification_pause(
            str(job["id"]),
            worker_id=worker_id,
            expected_job_revision=(
                expected_job_revision_override
                if expected_job_revision_override is not None
                else int(job["row_revision"])
            ),
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
        timestamp = time.time() if now is None else now
        recovered = await self._store.recover_expired_identification_leases(
            now=timestamp
        )
        await self._store.gc_stale_identification_jobs(
            now=timestamp, grace_seconds=SUBJECT_NOT_AVAILABLE_GRACE_SECONDS
        )
        return recovered

    async def activity_snapshot(self) -> dict:
        return await self._store.get_identification_activity_snapshot(
            now=time.time()
        )

    async def stream_revisions(self) -> dict[str, int]:
        return await self._store.get_library_revisions()
