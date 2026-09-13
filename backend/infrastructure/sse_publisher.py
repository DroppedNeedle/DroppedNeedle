"""SSEPublisher - in-memory multi-channel pub/sub event bus (AUD-4).

Generalises the single-channel fan-out in ``services/cache_status_service.py``
to N named channels. Used by scan progress (Phase 4) and download progress
(Phase 7). **Must be a singleton** - one instance per app lifecycle, registered
via the house ``@singleton`` provider; per-request instances would each get
their own bus and never receive published events.

Single-process invariant: correct only under ``uvicorn --workers 1`` (the
current Dockerfile CMD). Multi-worker fan-out is post-v1.
"""

import asyncio
import logging
from collections import defaultdict
from collections.abc import AsyncIterator

logger = logging.getLogger(__name__)

_QUEUE_MAXSIZE = 200
_KEEPALIVE_INTERVAL = 30.0

# Sentinel yielded by subscribe() when no event arrives within the keepalive
# interval. The SSE route turns it into a `: keepalive\n\n` comment (AUD-4) so an
# idle stream is not dropped by reverse proxies. Real events always carry a
# non-empty ``event`` name, so an empty name unambiguously marks the heartbeat.
KEEPALIVE = {"event": "", "data": None}


class SSEPublisher:
    """Publish ``(channel, event, data)``; subscribers get snapshot-then-deltas.

    Per-channel ``_latest`` retains the most recent payload of each event type,
    so a new subscriber (or one that reconnects) immediately sees current state:
    terminal events (``complete``/``failed``) are never missed by a late joiner.
    """

    def __init__(self) -> None:
        self._subscribers: dict[str, list[asyncio.Queue]] = defaultdict(list)
        self._latest: dict[str, dict[str, dict]] = defaultdict(dict)
        self._lock = asyncio.Lock()

    async def publish(self, channel: str, event: str, data: dict) -> None:
        async with self._lock:
            self._latest[channel][event] = data
            queues = self._subscribers.get(channel)
            if not queues:
                return
            message = {"event": event, "data": data}
            # Fan out under the lock so a concurrent subscribe/disconnect can't
            # mutate the list mid-iteration; put_nowait never awaits. Dead queues
            # (consumer gone but not yet torn down) are evicted, mirroring
            # CacheStatusService.broadcast_progress (AUD-4).
            dead = [q for q in queues if not self.offer_newest(q, message)]
            for q in dead:
                queues.remove(q)
            if not queues:
                self._subscribers.pop(channel, None)

    @staticmethod
    def offer_newest(queue: asyncio.Queue, message: object) -> bool:
        """Deliver the newest message, draining buffered items on overflow so a
        slow consumer always converges to current state (bus terminal events
        also persist in ``_latest`` for late joiners). Returns ``False`` if the
        queue is dead (cannot accept even after draining), so the caller
        evicts it. Payload-agnostic: the bus passes dicts, the mux passes
        tagged tuples."""
        try:
            queue.put_nowait(message)
        except asyncio.QueueFull:
            try:
                while not queue.empty():
                    queue.get_nowait()
                queue.put_nowait(message)
            except Exception:  # noqa: BLE001 - dead consumer; signal eviction
                return False
        return True

    async def subscribe(
        self, channel: str, keepalive_interval: float = _KEEPALIVE_INTERVAL
    ) -> AsyncIterator[dict]:
        queue: asyncio.Queue = asyncio.Queue(maxsize=_QUEUE_MAXSIZE)
        async with self._lock:
            self._subscribers[channel].append(queue)
            snapshot = list(self._latest.get(channel, {}).items())
        try:
            for event, data in snapshot:
                yield {"event": event, "data": data}
            while True:
                try:
                    yield await asyncio.wait_for(queue.get(), timeout=keepalive_interval)
                except asyncio.TimeoutError:
                    yield KEEPALIVE
        finally:
            async with self._lock:
                subscribers = self._subscribers.get(channel)
                if subscribers and queue in subscribers:
                    subscribers.remove(queue)
                if subscribers == []:
                    self._subscribers.pop(channel, None)

    async def subscriber_count(self, channel: str) -> int:
        async with self._lock:
            return len(self._subscribers.get(channel, []))
