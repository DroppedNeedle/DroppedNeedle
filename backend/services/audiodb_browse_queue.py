import asyncio
import logging
import time
from typing import TYPE_CHECKING

import msgspec

if TYPE_CHECKING:
    from services.audiodb_image_service import AudioDBImageService
    from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

_BROWSE_QUEUE_MAX_SIZE = 500
_BROWSE_QUEUE_INTER_ITEM_DELAY = 3.0
_BROWSE_QUEUE_DEDUP_TTL = 3600.0
_BROWSE_QUEUE_LOG_INTERVAL = 100


class BrowseQueueItem(msgspec.Struct):
    entity_type: str
    mbid: str
    name: str | None = None
    artist_name: str | None = None


class AudioDBBrowseQueue:
    def __init__(self) -> None:
        self._queue: asyncio.Queue[BrowseQueueItem] = asyncio.Queue(
            maxsize=_BROWSE_QUEUE_MAX_SIZE,
        )
        self._recent: dict[str, float] = {}
        self._pending: set[tuple[str, str]] = set()
        self._consumer_task: asyncio.Task | None = None

    async def enqueue(
        self,
        entity_type: str,
        mbid: str,
        name: str | None = None,
        artist_name: str | None = None,
    ) -> None:
        now = time.monotonic()
        self._evict_expired(now)

        if mbid in self._recent:
            logger.debug("audiodb.browse_queue action=dedup mbid=%s", mbid[:8])
            return

        if self._queue.full():
            logger.debug("audiodb.browse_queue action=full mbid=%s", mbid[:8])
            return

        item = BrowseQueueItem(
            entity_type=entity_type,
            mbid=mbid,
            name=name,
            artist_name=artist_name,
        )
        self._queue.put_nowait(item)
        self._pending.add((entity_type, mbid))
        self._recent[mbid] = now

    def is_pending(self, entity_type: str, mbid: str) -> bool:
        return (entity_type, mbid) in self._pending

    def _evict_expired(self, now: float) -> None:
        cutoff = now - _BROWSE_QUEUE_DEDUP_TTL
        expired = [k for k, ts in self._recent.items() if ts < cutoff]
        for k in expired:
            del self._recent[k]

    def start_consumer(
        self,
        audiodb_image_service: 'AudioDBImageService',
        preferences_service: 'PreferencesService',
    ) -> asyncio.Task:
        self._consumer_task = asyncio.create_task(
            self._process_queue(audiodb_image_service, preferences_service)
        )
        from core.task_registry import TaskRegistry
        TaskRegistry.get_instance().register("audiodb-browse-consumer", self._consumer_task)
        return self._consumer_task

    async def _process_queue(
        self,
        svc: 'AudioDBImageService',
        preferences_service: 'PreferencesService',
    ) -> None:
        processed = 0
        try:
            while True:
                item = await self._queue.get()
                try:
                    settings = preferences_service.get_advanced_settings()
                    if not settings.audiodb_enabled:
                        continue

                    if item.entity_type == "artist":
                        await svc.fetch_and_cache_artist_images(
                            item.mbid, item.name, is_monitored=False,
                        )
                    elif item.entity_type == "album":
                        await svc.fetch_and_cache_album_images(
                            item.mbid, artist_name=item.artist_name,
                            album_name=item.name, is_monitored=False,
                        )

                    processed += 1
                except Exception as e:
                    logger.error(
                        "audiodb.browse_queue action=item_error entity_type=%s mbid=%s error=%s",
                        item.entity_type,
                        item.mbid[:8],
                        e,
                        exc_info=True,
                    )
                finally:
                    self._pending.discard((item.entity_type, item.mbid))
                    self._queue.task_done()

                await asyncio.sleep(_BROWSE_QUEUE_INTER_ITEM_DELAY)
        except asyncio.CancelledError:
            pass
