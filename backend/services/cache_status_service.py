import asyncio
import logging
import threading
import time
from typing import Optional, TYPE_CHECKING

import msgspec

if TYPE_CHECKING:
    from infrastructure.persistence import SyncStateStore

logger = logging.getLogger(__name__)


class CacheSyncProgress(msgspec.Struct):
    is_syncing: bool
    phase: Optional[str]
    total_items: int
    processed_items: int
    current_item: Optional[str]
    started_at: Optional[float]
    error_message: Optional[str] = None
    total_artists: int = 0
    processed_artists: int = 0
    total_albums: int = 0
    processed_albums: int = 0

    @property
    def progress_percent(self) -> int:
        if self.total_items == 0:
            return 0
        return min(100, int((self.processed_items / self.total_items) * 100))


class CacheStatusService:

    _instance: Optional['CacheStatusService'] = None
    _creation_lock = threading.Lock()

    def __new__(cls, sync_state_store: Optional['SyncStateStore'] = None):
        with cls._creation_lock:
            if cls._instance is None:
                cls._instance = super().__new__(cls)
                cls._instance._initialize(sync_state_store)
            elif sync_state_store is not None and cls._instance._sync_state_store is None:
                cls._instance._sync_state_store = sync_state_store
        return cls._instance

    def _initialize(self, sync_state_store: Optional['SyncStateStore'] = None):
        self._sync_state_store = sync_state_store
        self._sync_generation: int = 0
        self._progress = CacheSyncProgress(
            is_syncing=False,
            phase=None,
            total_items=0,
            processed_items=0,
            current_item=None,
            started_at=None,
            error_message=None,
            total_artists=0,
            processed_artists=0,
            total_albums=0,
            processed_albums=0
        )
        self._cancel_event = asyncio.Event()
        self._current_task: Optional[asyncio.Task] = None
        self._state_lock = asyncio.Lock()
        self._sse_subscribers: set[asyncio.Queue] = set()
        self._sse_lock = threading.Lock()
        self._last_persist_time: float = 0.0
        self._last_broadcast_time: float = 0.0
        self._persist_item_counter: int = 0
        self._last_progress_at: float = time.time()

    def set_sync_state_store(self, sync_state_store: 'SyncStateStore'):
        self._sync_state_store = sync_state_store

    def subscribe_sse(self) -> asyncio.Queue:
        queue: asyncio.Queue = asyncio.Queue(maxsize=100)
        with self._sse_lock:
            self._sse_subscribers.add(queue)
        return queue

    def unsubscribe_sse(self, queue: asyncio.Queue) -> None:
        with self._sse_lock:
            self._sse_subscribers.discard(queue)

    def progress_payload(self) -> dict:
        """Current progress as the SSE payload dict (shared by the mux stream)."""
        progress = self.get_progress()
        return {
            'is_syncing': progress.is_syncing,
            'phase': progress.phase,
            'total_items': progress.total_items,
            'processed_items': progress.processed_items,
            'progress_percent': progress.progress_percent,
            'current_item': progress.current_item,
            'started_at': progress.started_at,
            'error_message': progress.error_message,
            'total_artists': progress.total_artists,
            'processed_artists': progress.processed_artists,
            'total_albums': progress.total_albums,
            'processed_albums': progress.processed_albums
        }

    async def broadcast_progress(self) -> None:
        data = msgspec.json.encode(self.progress_payload()).decode("utf-8")
        with self._sse_lock:
            dead_queues = []
            for queue in self._sse_subscribers:
                try:
                    queue.put_nowait(data)
                except asyncio.QueueFull:
                    try:
                        while not queue.empty():
                            queue.get_nowait()
                        queue.put_nowait(data)
                    except Exception:  # noqa: BLE001
                        dead_queues.append(queue)
            for q in dead_queues:
                self._sse_subscribers.discard(q)

    async def start_sync(self, phase: str, total_items: int, total_artists: int = 0, total_albums: int = 0) -> int:
        async with self._state_lock:
            self._sync_generation += 1
            generation = self._sync_generation
            self._cancel_event.clear()
            self._last_persist_time = 0.0
            self._last_broadcast_time = 0.0
            self._persist_item_counter = 0
            self._last_progress_at = time.time()
            started_at = time.time()
            self._progress = CacheSyncProgress(
                is_syncing=True,
                phase=phase,
                total_items=total_items,
                processed_items=0,
                current_item=None,
                started_at=started_at,
                error_message=None,
                total_artists=total_artists,
                processed_artists=0,
                total_albums=total_albums,
                processed_albums=0
            )

            if self._sync_state_store:
                try:
                    await self._sync_state_store.save_sync_state(
                        status='running',
                        phase=phase,
                        total_artists=total_artists,
                        total_albums=total_albums,
                        started_at=started_at
                    )
                except Exception as e:  # noqa: BLE001
                    logger.warning(f"Failed to persist sync state: {e}")

        await self.broadcast_progress()
        return generation

    _BROADCAST_THROTTLE_SECONDS = 0.3

    async def update_progress(
        self,
        processed: int,
        current_item: Optional[str] = None,
        processed_artists: Optional[int] = None,
        processed_albums: Optional[int] = None,
        generation: int = 0,
    ):
        async with self._state_lock:
            if generation and generation != self._sync_generation:
                return
            if processed >= self._progress.processed_items:
                self._progress.processed_items = processed
                self._progress.current_item = current_item
                if processed_artists is not None:
                    self._progress.processed_artists = processed_artists
                if processed_albums is not None:
                    self._progress.processed_albums = processed_albums
                self._last_progress_at = time.time()

        now = time.time()
        is_final = processed >= self._progress.total_items
        if is_final or (now - self._last_broadcast_time) >= self._BROADCAST_THROTTLE_SECONDS:
            self._last_broadcast_time = now
            await self.broadcast_progress()

    async def update_phase(self, phase: str, total_items: int, generation: int = 0):
        async with self._state_lock:
            if generation and generation != self._sync_generation:
                return
            self._progress.phase = phase
            self._progress.total_items = total_items
            self._progress.processed_items = 0
            self._progress.current_item = None
            self._last_progress_at = time.time()

            if self._sync_state_store and self._progress.is_syncing:
                try:
                    await self._sync_state_store.save_sync_state(
                        status='running',
                        phase=phase,
                        total_artists=self._progress.total_artists,
                        processed_artists=self._progress.processed_artists,
                        total_albums=self._progress.total_albums if phase == 'albums' else total_items,
                        processed_albums=self._progress.processed_albums,
                        started_at=self._progress.started_at
                    )
                except Exception as e:  # noqa: BLE001
                    logger.warning(f"Failed to persist phase update: {e}")

        await self.broadcast_progress()

    async def skip_phase(self, phase: str, generation: int = 0):
        """Broadcast a phase with 0 items so the frontend sees it as skipped."""
        async with self._state_lock:
            if generation and generation != self._sync_generation:
                return
            self._progress.phase = phase
            self._progress.total_items = 0
            self._progress.processed_items = 0
            self._progress.current_item = None
        await self.broadcast_progress()
        await asyncio.sleep(0.5)

    def get_last_progress_at(self) -> float:
        return self._last_progress_at

    _PERSIST_INTERVAL_SECONDS = 5.0
    _PERSIST_ITEM_INTERVAL = 10

    async def persist_progress(self, force: bool = False, generation: int = 0):
        if not self._progress.is_syncing:
            return
        if self.is_cancelled():
            return
        if generation and generation != self._sync_generation:
            return

        self._persist_item_counter += 1
        now = time.time()
        elapsed = now - self._last_persist_time

        if not force and elapsed < self._PERSIST_INTERVAL_SECONDS and self._persist_item_counter < self._PERSIST_ITEM_INTERVAL:
            return

        self._persist_item_counter = 0
        self._last_persist_time = now

        if self._sync_state_store:
            try:
                await self._sync_state_store.save_sync_state(
                    status='running',
                    phase=self._progress.phase,
                    total_artists=self._progress.total_artists,
                    processed_artists=self._progress.processed_artists,
                    total_albums=self._progress.total_albums,
                    processed_albums=self._progress.processed_albums,
                    current_item=self._progress.current_item,
                    started_at=self._progress.started_at
                )
            except Exception as e:  # noqa: BLE001
                logger.warning(f"Failed to persist progress: {e}")

    async def complete_sync(self, error_message: Optional[str] = None, generation: int = 0):
        async with self._state_lock:
            if not self._progress.is_syncing:
                return
            if generation and generation != self._sync_generation:
                return
            is_success = error_message is None
            status = 'completed' if is_success else 'failed'

            if self._sync_state_store:
                try:
                    await self._sync_state_store.save_sync_state(
                        status=status,
                        phase=self._progress.phase,
                        total_artists=self._progress.total_artists,
                        processed_artists=self._progress.processed_artists,
                        total_albums=self._progress.total_albums,
                        processed_albums=self._progress.processed_albums,
                        error_message=error_message,
                        started_at=self._progress.started_at
                    )
                    if is_success:
                        await self._sync_state_store.clear_sync_state()
                        await self._sync_state_store.clear_processed_items()
                except Exception as e:  # noqa: BLE001
                    logger.warning(f"Failed to persist completion: {e}")

            self._progress = CacheSyncProgress(
                is_syncing=False,
                phase=None,
                total_items=0,
                processed_items=0,
                current_item=None,
                started_at=None,
                error_message=error_message,
                total_artists=0,
                processed_artists=0,
                total_albums=0,
                processed_albums=0
            )

        await self.broadcast_progress()

    def get_progress(self) -> CacheSyncProgress:
        return self._progress

    def is_syncing(self) -> bool:
        return self._progress.is_syncing

    async def cancel_current_sync(self):
        async with self._state_lock:
            logger.warning(f"Cancelling sync: phase={self._progress.phase}, progress={self._progress.processed_items}/{self._progress.total_items}")
            self._cancel_event.set()

            if self._sync_state_store and self._progress.is_syncing:
                try:
                    await self._sync_state_store.save_sync_state(
                        status='cancelled',
                        phase=self._progress.phase,
                        total_artists=self._progress.total_artists,
                        processed_artists=self._progress.processed_artists,
                        total_albums=self._progress.total_albums,
                        processed_albums=self._progress.processed_albums,
                        started_at=self._progress.started_at
                    )
                except Exception as e:  # noqa: BLE001
                    logger.warning(f"Failed to persist cancellation: {e}")

            self._progress = CacheSyncProgress(
                is_syncing=False,
                phase=None,
                total_items=0,
                processed_items=0,
                current_item=None,
                started_at=None,
                error_message=None,
                total_artists=0,
                processed_artists=0,
                total_albums=0,
                processed_albums=0
            )

        await self.broadcast_progress()

    def is_cancelled(self) -> bool:
        return self._cancel_event.is_set()

    def set_current_task(self, task: Optional[asyncio.Task]):
        self._current_task = task

    async def wait_for_completion(self):
        task = self._current_task
        if task and not task.done():
            try:
                await asyncio.wait_for(task, timeout=30.0)
            except asyncio.TimeoutError:
                logger.warning("Sync task did not complete within 30s timeout, forcing cancellation")
                if not task.done():
                    task.cancel()
            except Exception as e:  # noqa: BLE001
                logger.error(f"Error waiting for sync completion: {e}")

    def can_start_sync(self) -> bool:
        return not self._progress.is_syncing

    async def restore_from_persistence(self) -> Optional[dict]:
        if not self._sync_state_store:
            return None

        try:
            state = await self._sync_state_store.get_sync_state()
            if state and state.get('status') == 'running':

                phase = state.get('phase')
                if phase == 'albums':
                    total_items = state.get('total_albums')
                    processed_items = state.get('processed_albums')
                elif phase == 'audiodb_prewarm':
                    total_items = 0
                    processed_items = 0
                else:
                    total_items = state.get('total_artists')
                    processed_items = state.get('processed_artists')

                self._progress = CacheSyncProgress(
                    is_syncing=True,
                    phase=phase,
                    total_items=total_items,
                    processed_items=processed_items,
                    current_item=state.get('current_item'),
                    started_at=state.get('started_at'),
                    error_message=None,
                    total_artists=state.get('total_artists', 0),
                    processed_artists=state.get('processed_artists', 0),
                    total_albums=state.get('total_albums', 0),
                    processed_albums=state.get('processed_albums', 0)
                )
                return state
            return None
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to restore from persistence: {e}")
            return None
