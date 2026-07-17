from __future__ import annotations

import asyncio
import logging
import time
from enum import Enum
from typing import Any, TYPE_CHECKING

import msgspec

from api.v1.schemas.discover import (
    DiscoverQueueEnrichment,
    DiscoverQueueItemFull,
    DiscoverQueueResponse,
    DiscoverQueueStatusResponse,
    QueueGenerateResponse,
)
from infrastructure.serialization import clone_with_updates
from services.discover_service import DiscoverService
from services.preferences_service import PreferencesService
from services.discover.snapshot_codec import decode_discover_queue

if TYPE_CHECKING:
    from infrastructure.persistence.discovery_snapshot_store import (
        DiscoverySnapshotStore,
    )
    from repositories.coverart_repository import CoverArtRepository

logger = logging.getLogger(__name__)


class QueueBuildStatus(str, Enum):
    IDLE = "idle"
    BUILDING = "building"
    READY = "ready"
    ERROR = "error"


class SourceQueueState:
    __slots__ = ("status", "queue", "error", "built_at", "persisted_stale", "task")

    def __init__(self) -> None:
        self.status: QueueBuildStatus = QueueBuildStatus.IDLE
        self.queue: DiscoverQueueResponse | None = None
        self.error: str | None = None
        self.built_at: float = 0.0
        self.persisted_stale: bool = False
        self.task: asyncio.Task[None] | None = None


_COVER_PREWARM_CONCURRENCY = 4
_COVER_PREWARM_DELAY = 0.5
_QUEUE_SNAPSHOT_PREFIX = "discover_queue:"


class PersistedQueue(msgspec.Struct):
    queue: DiscoverQueueResponse
    built_at: float


class DiscoverQueueManager:
    def __init__(
        self,
        discover_service: DiscoverService,
        preferences_service: PreferencesService,
        cover_repo: CoverArtRepository | None = None,
        snapshot_store: "DiscoverySnapshotStore | None" = None,
    ) -> None:
        self._discover = discover_service
        self._preferences = preferences_service
        self._cover_repo = cover_repo
        self._snapshot_store = snapshot_store
        # Keyed per user; the queue follows the user's primary source internally.
        self._states: dict[str, SourceQueueState] = {}
        self._lock = asyncio.Lock()
        self._loaded_users: set[str] = set()

    @staticmethod
    def _snapshot_key(user_id: str) -> str:
        return f"{_QUEUE_SNAPSHOT_PREFIX}{user_id}"

    async def ensure_loaded(self, user_id: str) -> None:
        if self._snapshot_store is None or user_id in self._loaded_users:
            return
        async with self._lock:
            if user_id in self._loaded_users:
                return
            self._loaded_users.add(user_id)
            saved = await self._snapshot_store.get_with_stale(
                self._snapshot_key(user_id)
            )
            if saved is None:
                return
            payload, stale = saved
            try:
                queue, built_at = decode_discover_queue(payload)
            except (
                msgspec.DecodeError,
                msgspec.ValidationError,
                TypeError,
                ValueError,
            ):
                logger.warning("Ignoring an invalid Discover queue snapshot")
                return
            state = self._get_state(user_id)
            state.queue = queue
            state.built_at = built_at
            state.persisted_stale = stale
            state.status = QueueBuildStatus.READY

    def _get_state(self, user_id: str) -> SourceQueueState:
        if user_id not in self._states:
            self._states[user_id] = SourceQueueState()
        return self._states[user_id]

    def _get_ttl(self) -> int:
        adv = self._preferences.get_advanced_settings()
        return adv.discover_queue_ttl

    def _is_stale(self, state: SourceQueueState) -> bool:
        if state.status != QueueBuildStatus.READY or state.queue is None:
            return True
        return state.persisted_stale or (time.time() - state.built_at) > self._get_ttl()

    def get_status(self, user_id: str) -> DiscoverQueueStatusResponse:
        state = self._get_state(user_id)
        if state.status == QueueBuildStatus.READY and state.queue:
            return DiscoverQueueStatusResponse(
                status=state.status.value,
                queue_id=state.queue.queue_id,
                item_count=len(state.queue.items),
                built_at=state.built_at,
                stale=self._is_stale(state),
            )
        if state.status == QueueBuildStatus.ERROR:
            return DiscoverQueueStatusResponse(
                status=state.status.value,
                error=state.error,
            )
        return DiscoverQueueStatusResponse(status=state.status.value)

    @staticmethod
    def _build_generate_response(action: str, status: DiscoverQueueStatusResponse) -> QueueGenerateResponse:
        return QueueGenerateResponse(
            action=action,
            status=status.status,
            queue_id=status.queue_id,
            item_count=status.item_count,
            built_at=status.built_at,
            stale=status.stale,
            error=status.error,
        )

    def get_queue(self, user_id: str) -> DiscoverQueueResponse | None:
        state = self._get_state(user_id)
        if state.status == QueueBuildStatus.READY and state.queue and not self._is_stale(state):
            return state.queue
        return None

    async def start_build(self, user_id: str, *, force: bool = False) -> QueueGenerateResponse:
        await self.ensure_loaded(user_id)
        async with self._lock:
            state = self._get_state(user_id)

            if state.status == QueueBuildStatus.BUILDING:
                return self._build_generate_response("already_building", self.get_status(user_id))

            if not force and state.status == QueueBuildStatus.READY and not self._is_stale(state):
                return self._build_generate_response("already_ready", self.get_status(user_id))

            if state.task and not state.task.done():
                state.task.cancel()

            state.status = QueueBuildStatus.BUILDING
            state.error = None
            state.task = asyncio.create_task(self._do_build(user_id))
            from core.task_registry import TaskRegistry
            try:
                TaskRegistry.get_instance().register(f"discover-build-{user_id}", state.task)
            except RuntimeError:
                pass

        return self._build_generate_response("started", self.get_status(user_id))

    async def wait_for_build(self, user_id: str) -> None:
        state = self._get_state(user_id)
        task = state.task
        if task is not None and not task.done():
            await task

    async def build_hydrated_queue(
        self, user_id: str, count: int | None = None
    ) -> DiscoverQueueResponse:
        queue = await self._discover.build_queue(user_id, count=count)
        return await self._hydrate_queue_items(queue)

    async def _hydrate_queue_items(
        self, queue: DiscoverQueueResponse
    ) -> DiscoverQueueResponse:
        if not queue.items:
            return queue

        concurrency = min(4, len(queue.items))
        semaphore = asyncio.Semaphore(concurrency)

        async def hydrate_item(item: Any) -> Any:
            if getattr(item, "enrichment", None) is not None:
                return item
            try:
                async with semaphore:
                    enrichment = await self._discover.enrich_queue_item(item.release_group_mbid)
            except Exception as exc:  # noqa: BLE001
                logger.warning(
                    "Queue item enrichment failed (release_group_mbid=%s): %s",
                    item.release_group_mbid,
                    exc,
                )
                enrichment = DiscoverQueueEnrichment()

            item_data = msgspec.to_builtins(item)
            item_data["enrichment"] = enrichment
            return DiscoverQueueItemFull(**item_data)

        hydrated_items = await asyncio.gather(*(hydrate_item(item) for item in queue.items))
        return clone_with_updates(queue, {"items": hydrated_items})

    async def _do_build(self, user_id: str) -> None:
        state = self._get_state(user_id)
        try:
            queue = await self.build_hydrated_queue(user_id)
            state.queue = queue
            state.built_at = time.time()
            state.persisted_stale = False
            state.status = QueueBuildStatus.READY
            if self._snapshot_store:
                try:
                    saved = PersistedQueue(queue=queue, built_at=state.built_at)
                    await self._snapshot_store.save(
                        self._snapshot_key(user_id),
                        user_id,
                        msgspec.json.encode(saved),
                        state.built_at,
                    )
                except Exception as exc:  # noqa: BLE001 - queue remains usable in memory
                    logger.warning("Could not persist Discover queue snapshot: %s", exc)
            task = asyncio.create_task(self._prewarm_covers(queue))
            task.add_done_callback(_log_queue_task_error)
            from core.task_registry import TaskRegistry
            try:
                TaskRegistry.get_instance().register(f"discover-cover-prewarm-{user_id}", task)
            except RuntimeError:
                pass
        except asyncio.CancelledError:
            if state.status == QueueBuildStatus.BUILDING:
                state.status = QueueBuildStatus.IDLE
            raise
        except Exception as e:  # noqa: BLE001
            logger.error("Background queue build failed: %s", e)
            state.status = QueueBuildStatus.ERROR
            state.error = str(e)

    async def _prewarm_covers(self, queue: DiscoverQueueResponse) -> None:
        if not self._cover_repo or not queue.items:
            return

        from infrastructure.queue.priority_queue import RequestPriority

        mbids = [
            item.release_group_mbid
            for item in queue.items
            if getattr(item, "release_group_mbid", None)
        ]
        if not mbids:
            return

        semaphore = asyncio.Semaphore(_COVER_PREWARM_CONCURRENCY)

        async def warm_one(mbid: str) -> bool:
            async with semaphore:
                try:
                    result = await self._cover_repo.get_release_group_cover(
                        mbid, size="500", priority=RequestPriority.BACKGROUND_SYNC
                    )
                    return result is not None
                except Exception as exc:  # noqa: BLE001
                    logger.debug("Discover queue cover pre-warm failed for %s: %s", mbid[:8], exc)
                    return False

        await asyncio.gather(*(warm_one(m) for m in mbids), return_exceptions=True)

    async def consume_queue(self, user_id: str) -> DiscoverQueueResponse | None:
        await self.ensure_loaded(user_id)
        state = self._get_state(user_id)
        if state.status != QueueBuildStatus.READY or state.queue is None:
            return None
        if self._is_stale(state):
            state.queue = None
            state.status = QueueBuildStatus.IDLE
            state.built_at = 0.0
            state.persisted_stale = False
            if self._snapshot_store:
                await self._snapshot_store.delete(self._snapshot_key(user_id))
            return None
        queue = state.queue
        state.queue = None
        state.status = QueueBuildStatus.IDLE
        state.built_at = 0.0
        state.persisted_stale = False
        if self._snapshot_store:
            await self._snapshot_store.delete(self._snapshot_key(user_id))
        return queue

    def invalidate(self, user_id: str | None = None) -> None:
        if user_id is None:
            # Invalidate every user's queues (shutdown / global cache clear).
            for key in list(self._states.keys()):
                self.invalidate(key)
            return
        state = self._get_state(user_id)
        if state.task and not state.task.done():
            state.task.cancel()
        self._states[user_id] = SourceQueueState()
        self._loaded_users.add(user_id)


def _log_queue_task_error(task: "asyncio.Task[Any]") -> None:
    if task.cancelled():
        return
    exc = task.exception()
    if exc is not None:
        logger.error("Discover queue background task failed: %s", exc, exc_info=exc)
