"""Single durable entry point for every inactive target scan trigger."""

from __future__ import annotations

import time
import uuid
import logging
from collections.abc import Callable
from pathlib import Path

from core.exceptions import StaleRevisionError, ValidationError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_work import (
    ScanControlResult,
    ScanRequest,
    ScanRequestResult,
    ScanRun,
    ScanRunSnapshot,
    ScanScope,
)
from services.native.library_indexer import LibraryIndexer
from services.native.library_inventory_scanner import LibraryInventoryScanner
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.library_reconciler import LibraryReconciler
from services.native.library_scan_events import LibraryScanEventPublisher
from services.native.background_workload_gate import BackgroundWorkloadGate

PolicyResolverGetter = Callable[[], LibraryPolicyResolver]
logger = logging.getLogger(__name__)


class LibraryScanCoordinator:
    def __init__(
        self,
        store: NativeLibraryStore,
        inventory: LibraryInventoryScanner,
        indexer: LibraryIndexer,
        reconciler: LibraryReconciler,
        resolver_getter: PolicyResolverGetter,
        events: LibraryScanEventPublisher | None = None,
        *,
        clock: Callable[[], float] = time.time,
        workload_gate: BackgroundWorkloadGate | None = None,
    ) -> None:
        self._store = store
        self._inventory = inventory
        self._indexer = indexer
        self._reconciler = reconciler
        self._resolver_getter = resolver_getter
        self._events = events
        self._clock = clock
        self._workload_gate = workload_gate
        self._last_progress_log: dict[str, float] = {}
        self._pending_control_run_ids: set[str] = set()

    def _log_progress(self, run: ScanRun, event: str, *, force: bool = False) -> None:
        now = self._clock()
        if not force and now - self._last_progress_log.get(run.id, 0.0) < 30.0:
            return
        self._last_progress_log[run.id] = now
        counters = run.counters
        total = int(counters.get("total_count", 0))
        inspected = int(counters.get("inspected_count", 0))
        percentage = 100.0 if run.state == "completed" else (
            inspected * 100.0 / total if total else 0.0
        )
        elapsed = max(0.0, now - (run.started_at or now))
        throughput = inspected / elapsed if elapsed else 0.0
        logger.info(
            "library_scan event=%s state=%s phase=%s discovered=%d processed=%d "
            "total=%d percentage=%.1f new=%d changed=%d unchanged=%d excluded=%d "
            "failed=%d throughput=%.1f elapsed=%.1f",
            event,
            run.state,
            run.phase,
            int(counters.get("discovered_count", 0)),
            inspected,
            total,
            percentage,
            int(counters.get("new_count", 0)),
            int(counters.get("changed_count", 0)),
            int(counters.get("unchanged_count", 0)),
            int(counters.get("excluded_count", 0)),
            int(counters.get("errored_count", 0)),
            throughput,
            elapsed,
        )

    async def request_run(self, request: ScanRequest) -> ScanRequestResult:
        if not request.scopes:
            raise ValidationError("Select at least one library scope.")
        if any(
            scope.policy_revision != request.policy_revision for scope in request.scopes
        ):
            raise StaleRevisionError("The selected library policy has changed.")
        result = await self._store.request_scan_run(
            request, run_id=str(uuid.uuid4()), requested_at=self._clock()
        )
        if self._events is not None and result.disposition != "conflict":
            run = (await self._store.get_scan_run(result.run_id))[0]
            await self._events.publish(run, event="scan.requested")
        return result

    async def snapshot(self, run_id: str) -> ScanRunSnapshot:
        run, scopes, counters = await self._store.get_scan_run(run_id)
        return ScanRunSnapshot(run=run, scopes=scopes, counters=counters)

    async def current(self) -> list[ScanRun]:
        return await self._store.list_current_scan_runs()

    async def estimate(self, scopes: list[ScanScope]) -> tuple[int, float]:
        return await self._store.estimate_scan_scope(scopes), self._clock()

    async def history(self, *, limit: int = 50) -> list[ScanRun]:
        return await self._store.list_scan_history(limit=limit)

    async def latest_filesystem_terminal(self) -> ScanRun | None:
        return await self._store.get_latest_filesystem_scan_terminal()

    async def history_page(
        self, *, limit: int = 50, cursor: str | None = None
    ) -> tuple[list[ScanRun], str | None]:
        before_terminal_at: float | None = None
        before_id: str | None = None
        if cursor is not None:
            try:
                terminal, before_id = cursor.split(":", 1)
                before_terminal_at = float(terminal)
            except (TypeError, ValueError) as exc:
                raise ValidationError("The scan history cursor is invalid.") from exc
        rows = await self._store.list_scan_history(
            limit=limit + 1,
            before_terminal_at=before_terminal_at,
            before_id=before_id,
        )
        items = rows[:limit]
        next_cursor = None
        if len(rows) > limit and items:
            last = items[-1]
            next_cursor = f"{last.terminal_at}:{last.id}"
        return items, next_cursor

    async def control(
        self, run_id: str, control: str, expected_revision: int
    ) -> ScanControlResult:
        run, stream_revision = await self._store.request_scan_control(
            run_id,
            control=control,
            expected_revision=expected_revision,
            now=self._clock(),
        )
        if run.state in {"pausing", "stopping"}:
            self._pending_control_run_ids.add(run.id)
        else:
            self._pending_control_run_ids.discard(run.id)
        if self._events is not None:
            await self._events.publish(run, event="scan.transition")
        if run.terminal_at is not None:
            await self._store.flush_scan_invalidation(terminal=True)
        self._log_progress(run, f"control_{control}", force=True)
        return ScanControlResult(
            run_id=run.id,
            state=run.state,
            row_revision=run.row_revision,
            event_revision=run.event_revision,
            stream_revision=stream_revision,
        )

    async def recover(self) -> list[ScanRun]:
        runs = await self._store.recover_scan_runs(now=self._clock())
        self._pending_control_run_ids.clear()
        for run in runs:
            self._log_progress(run, "recovery", force=True)
        return runs

    async def _settle_pending_control(self, run_id: str) -> ScanRun:
        while True:
            run, _, _ = await self._store.get_scan_run(run_id)
            if run.state not in {"pausing", "stopping"}:
                self._pending_control_run_ids.discard(run.id)
                return run
            new_state = "paused" if run.state == "pausing" else "cancelled"
            try:
                settled = await self._store.transition_scan_run(
                    run.id,
                    expected_state=run.state,
                    expected_revision=run.row_revision,
                    new_state=new_state,
                    now=self._clock(),
                )
            except StaleRevisionError:
                continue
            if self._events is not None:
                await self._events.publish(settled, event="scan.transition")
            await self._store.flush_scan_invalidation(
                terminal=settled.state == "cancelled"
            )
            self._log_progress(
                settled, "pause" if settled.state == "paused" else "stop", force=True
            )
            self._pending_control_run_ids.discard(settled.id)
            return settled

    async def checkpoint(self, run_id: str, frozen_policy_revision: str) -> bool:
        current_policy_revision = self._resolver_getter().policy_revision
        if (
            current_policy_revision == frozen_policy_revision
            and run_id not in self._pending_control_run_ids
        ):
            return True
        run, _, _ = await self._store.get_scan_run(run_id)
        if current_policy_revision != frozen_policy_revision:
            if run.state == "paused":
                await self._store.transition_scan_run(
                    run.id,
                    expected_state="paused",
                    expected_revision=run.row_revision,
                    new_state="superseded_policy_changed",
                    now=self._clock(),
                    terminal_code="SUPERSEDED_POLICY_CHANGED",
                )
            elif run.state in {"discovering", "indexing", "reconciling", "pausing"}:
                await self._store.transition_scan_run(
                    run.id,
                    expected_state=run.state,
                    expected_revision=run.row_revision,
                    new_state="superseded_policy_changed",
                    now=self._clock(),
                    terminal_code="SUPERSEDED_POLICY_CHANGED",
                )
            await self._store.flush_scan_invalidation(terminal=True)
            self._pending_control_run_ids.discard(run.id)
            return False
        if run.state == "pausing":
            await self._settle_pending_control(run.id)
            return False
        if run.state == "stopping":
            await self._settle_pending_control(run.id)
            return False
        self._pending_control_run_ids.discard(run.id)
        return run.state in {"discovering", "indexing", "reconciling"}

    async def run_once(self, root_paths: dict[str, Path]) -> ScanRun | None:
        await self._store.cleanup_terminal_scan_inventory(limit=5_000)
        run = await self._store.get_resumable_scan_run()
        newly_claimed = run is None
        if run is None:
            run = await self._store.claim_next_scan_run(now=self._clock())
        if run is None:
            return None
        if newly_claimed and self._events is not None:
            await self._events.publish(run, event="scan.transition")
        self._log_progress(run, "start", force=True)
        if self._workload_gate is not None:
            self._workload_gate.set_scan_active(True)
        try:
            return await self._continue_run(run, root_paths)
        except Exception:  # noqa: BLE001 - a crashed worker must leave a terminal durable run
            current, _, _ = await self._store.get_scan_run(run.id)
            if current.state in {"pausing", "stopping"}:
                return await self._settle_pending_control(current.id)
            if current.state in {"discovering", "indexing", "reconciling"}:
                failed = await self._store.transition_scan_run(
                    current.id,
                    expected_state=current.state,
                    expected_revision=current.row_revision,
                    new_state="failed",
                    now=self._clock(),
                    terminal_code="UNEXPECTED_WORKER_FAILURE",
                )
                if self._events is not None:
                    await self._events.publish(failed, event="scan.transition")
                await self._store.flush_scan_invalidation(terminal=True)
            raise
        finally:
            self._pending_control_run_ids.discard(run.id)
            if self._workload_gate is not None:
                self._workload_gate.set_scan_active(False)

    async def _continue_run(self, run: ScanRun, root_paths: dict[str, Path]) -> ScanRun:
        run, scopes, _ = await self._store.get_scan_run(run.id)
        frozen_policy_revision = scopes[0].policy_revision
        if run.state == "discovering":
            self._log_progress(run, "phase_discovery_start", force=True)
            await self._store.prepare_scan_discovery_resume(run.id)
            await self._store.cleanup_stale_scan_inventory(run.id)
            run, scopes, _ = await self._store.get_scan_run(run.id)
            run = await self._inventory.discover(
                run,
                scopes,
                root_paths,
                self._resolver_getter(),
                self.checkpoint,
            )
            if self._events is not None:
                await self._events.publish(run, event="scan.progress", counter=True)
            run, scopes, _ = await self._store.get_scan_run(run.id)
            if run.state != "discovering":
                return await self._settle_pending_control(run.id)
            run = await self._store.finalize_scan_discovery(
                run.id, updated_at=self._clock()
            )
            self._log_progress(run, "phase_discovery_end", force=True)
            run = await self._store.transition_scan_run(
                run.id,
                expected_state="discovering",
                expected_revision=run.row_revision,
                new_state="indexing",
                now=self._clock(),
            )
            if self._events is not None:
                await self._events.publish(run, event="scan.transition")

        if run.state == "indexing":
            self._log_progress(run, "phase_indexing_start", force=True)
            if not await self.checkpoint(run.id, frozen_policy_revision):
                return (await self._store.get_scan_run(run.id))[0]

            async def record_index_progress(updated_run: ScanRun) -> None:
                nonlocal run
                run = updated_run
                if self._events is not None:
                    await self._events.publish(run, event="scan.progress", counter=True)
                self._log_progress(run, "progress")

            index_counts = await self._indexer.index(
                run,
                frozen_policy_revision,
                self.checkpoint,
                progress=record_index_progress,
            )
            if index_counts["identification_enqueued"] and self._events is not None:
                run = (await self._store.get_scan_run(run.id))[0]
                await self._events.publish(run, event="scan.progress", counter=True)
            run, scopes, _ = await self._store.get_scan_run(run.id)
            if run.state != "indexing":
                return await self._settle_pending_control(run.id)
            if not await self.checkpoint(run.id, frozen_policy_revision):
                return (await self._store.get_scan_run(run.id))[0]
            await self._store.flush_scan_invalidation()
            self._log_progress(run, "phase_indexing_end", force=True)
            run = await self._store.transition_scan_run(
                run.id,
                expected_state="indexing",
                expected_revision=run.row_revision,
                new_state="reconciling",
                now=self._clock(),
            )
            if self._events is not None:
                await self._events.publish(run, event="scan.transition")

        self._log_progress(run, "phase_reconciliation_start", force=True)
        reconcile_counts = await self._reconciler.reconcile(
            run.id, scopes, self.checkpoint
        )
        if any(reconcile_counts.values()) and self._events is not None:
            run = (await self._store.get_scan_run(run.id))[0]
            await self._events.publish(run, event="scan.progress", counter=True)
        run, _, _ = await self._store.get_scan_run(run.id)
        if run.state != "reconciling":
            return await self._settle_pending_control(run.id)
        if not await self.checkpoint(run.id, frozen_policy_revision):
            return (await self._store.get_scan_run(run.id))[0]
        self._log_progress(run, "phase_reconciliation_end", force=True)
        run = await self._store.transition_scan_run(
            run.id,
            expected_state="reconciling",
            expected_revision=run.row_revision,
            new_state="completed",
            now=self._clock(),
        )
        if self._events is not None:
            await self._events.publish(run, event="scan.transition")
        await self._store.flush_scan_invalidation(terminal=True)
        self._log_progress(run, "completion", force=True)
        self._last_progress_log.pop(run.id, None)
        return run
