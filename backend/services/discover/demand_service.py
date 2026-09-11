"""Demand-only optional discovery work, shared across foreground entry and ticks."""
import asyncio
import logging
import time

import msgspec

from core.config import get_settings
from core.task_registry import TaskRegistry
from infrastructure.observability.optional_work import (
    OptionalWorkBudget, OptionalWorkDeferred, optional_work_budget, optional_dispatch_guard,
)
from infrastructure.observability.provider_counters import ProviderWorkload, provider_workload
from repositories.musicbrainz_base import capture_mb_source_context, is_mb_source_current

logger = logging.getLogger(__name__)
_active_users: set[str] = set()


def _log_error(task: asyncio.Task) -> None:
    if task.cancelled():
        return
    error = task.exception()
    if error is not None and not isinstance(error, OptionalWorkDeferred):
        logger.error("Discovery demand job failed", exc_info=error)


class DiscoveryDemandService:
    def __init__(self, store, get_discover_service, get_home_service, get_queue_manager,
                 get_artist_service, get_auth_store, workload_gate=None) -> None:
        self._store = store
        self._discover = get_discover_service
        self._home = get_home_service
        self._queue = get_queue_manager
        self._artist = get_artist_service
        self._auth = get_auth_store
        self._workload_gate = workload_gate

    def trigger_user(self, user_id: str) -> None:
        registry = TaskRegistry.get_instance()
        name = f"discovery-demand-{user_id}"
        if registry.is_running(name):
            return
        task = asyncio.create_task(self.run_due_tick(user_id))
        registry.register(name, task)
        task.add_done_callback(_log_error)

    async def run_due_tick(self, user_id: str | None = None) -> None:
        if not get_settings().discover_warmer_enabled:
            return
        source = capture_mb_source_context()
        source_key = msgspec.json.encode((source.source_mode, source.source_id, source.generation)).decode()
        rows = await self._store.get_due_activity(source_key, time.time(), user_id=user_id)
        if not rows:
            return
        user_id = rows[0]["user_id"]
        if user_id in _active_users:
            return
        _active_users.add(user_id)
        try:
            await self._run_user(user_id, rows, source)
        finally:
            _active_users.discard(user_id)

    async def _run_user(self, user_id: str, rows: list[dict], source) -> None:
        if await self._auth().get_user_by_id(user_id) is None:
            await self._store.delete_user(user_id)
            return
        lease = self._store.user_lease(user_id)
        def current() -> bool:
            return lease.active and is_mb_source_current(source) and get_settings().discover_warmer_enabled
        budget = OptionalWorkBudget(guard=current)
        with optional_work_budget(budget):
            for row in rows:
                if row["user_id"] != user_id or not current():
                    continue
                feature = row["feature"]
                if feature == "queue" and not self._queue().scheduled_enabled():
                    await self._store.finish_activity(row, time.time(), success=False, retry_seconds=90)
                    continue
                name = f"discovery-demand-{feature}-{user_id}"
                registry = TaskRegistry.get_instance()
                if registry.is_running(name):
                    continue
                success = False
                retry = 90.0
                task = asyncio.create_task(self._run_feature(row))
                registry.register(name, task)
                task.add_done_callback(_log_error)
                try:
                    published = await asyncio.wait_for(task, timeout=300)
                    success = published and current()
                    retry = 6 * 3600 if success else 90
                except OptionalWorkDeferred:
                    pass
                except asyncio.CancelledError:
                    raise
                except Exception:  # noqa: BLE001 - failing feature reschedules; the demand loop continues
                    retry = 900
                    logger.exception("Optional discovery feature failed: %s", feature)
                await self._store.finish_activity(row, time.time(), success=success, retry_seconds=retry)
                if budget.remaining <= 0:
                    break

    async def _run_feature(self, row: dict) -> bool:
        user_id, feature = row["user_id"], row["feature"]
        if await self._auth().get_user_by_id(user_id) is None:
            await self._store.delete_user(user_id)
            raise OptionalWorkDeferred()
        workload = {"home": ProviderWorkload.HOME, "discover": ProviderWorkload.HOME,
                    "queue": ProviderWorkload.QUEUE, "artist": ProviderWorkload.ARTIST}[feature]
        with provider_workload(workload):
            if feature == "home":
                if self._workload_gate is None:
                    return await self._home().warm_cache(user_id)
                return await self._workload_gate.run_warmer_unit(
                    lambda: self._home().warm_cache(user_id)
                )
            elif feature == "discover":
                if self._workload_gate is None:
                    return await self._discover().warm_cache(user_id)
                return await self._workload_gate.run_warmer_unit(
                    lambda: self._discover().warm_cache(user_id)
                )
            elif feature == "queue":
                with optional_dispatch_guard(lambda: self._queue().scheduled_enabled()):
                    manager = self._queue()
                    result = await manager.start_build(user_id, scheduled=True)
                    if result.action == "disabled":
                        raise OptionalWorkDeferred()
                    return await manager.wait_for_build(user_id)
            else:
                return await self._artist().warm_requested_section(row["artist_mbid"], row["section"], row["provider"], user_id)
