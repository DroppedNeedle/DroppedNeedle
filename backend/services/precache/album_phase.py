"""Album metadata + cover pre-caching phase."""

from __future__ import annotations

import asyncio
import logging
import time
from typing import Any

from repositories.protocols import CoverArtRepositoryProtocol
from repositories.coverart_disk_cache import get_cache_filename
from services.cache_status_service import CacheStatusService
from infrastructure.cache.cache_keys import ALBUM_INFO_PREFIX
from infrastructure.queue.priority_queue import RequestPriority

logger = logging.getLogger(__name__)


class AlbumPhase:
    def __init__(
        self,
        cover_repo: CoverArtRepositoryProtocol,
        preferences_service: Any,
        sync_state_store: Any,
    ):
        self._cover_repo = cover_repo
        self._preferences_service = preferences_service
        self._sync_state_store = sync_state_store

    async def precache_album_data(
        self,
        release_group_ids: list[str],
        library_mbids: set[str],
        status_service: CacheStatusService,
        library_album_mbids: dict[str, Any] = None,
        offset: int = 0,
        generation: int = 0,
    ) -> None:
        from core.dependencies import get_album_service
        album_service = get_album_service()

        async def cache_rg(rgid: str, index: int) -> tuple[str, bool, bool]:
            try:
                if not rgid or rgid.startswith('unknown_'):
                    return (rgid, False, False)
                metadata_fetched = False
                cover_fetched = False
                cache_key = f"{ALBUM_INFO_PREFIX}{rgid}"
                cached_info = await album_service._cache.get(cache_key)
                if not cached_info:
                    await status_service.update_progress(index + 1, f"Fetching metadata for {rgid[:8]}...", processed_albums=offset + index + 1, generation=generation)
                    await album_service.get_album_info(
                        rgid,
                        library_mbids=library_mbids,
                        priority=RequestPriority.BACKGROUND_SYNC,
                    )
                    metadata_fetched = True
                else:
                    await status_service.update_progress(index + 1, f"Cached: {rgid[:8]}...", processed_albums=offset + index + 1, generation=generation)
                if rgid.lower() in library_mbids:
                    cache_filename = get_cache_filename(f"rg_{rgid}", "500")
                    file_path = self._cover_repo.cache_dir / f"{cache_filename}.bin"
                    if not file_path.exists():
                        try:
                            await self._cover_repo.get_release_group_cover(
                                rgid,
                                size="500",
                                priority=RequestPriority.BACKGROUND_SYNC,
                            )
                            cover_fetched = True
                        except Exception as e:  # noqa: BLE001
                            logger.debug(f"Failed to cache cover for {rgid}: {e}")
                return (rgid, metadata_fetched, cover_fetched)
            except Exception as e:  # noqa: BLE001
                logger.debug(f"Failed to pre-cache release-group {rgid}: {e}")
                return (rgid, False, False)

        advanced_settings = self._preferences_service.get_advanced_settings()
        batch_size = advanced_settings.batch_albums
        min_batch = max(1, advanced_settings.batch_albums - 2)
        max_batch = min(20, advanced_settings.batch_albums + 12)
        metadata_fetched = 0
        covers_fetched = 0
        consecutive_slow_batches = 0
        i = 0
        while i < len(release_group_ids):
            if status_service.is_cancelled():
                break
            batch_start = time.time()
            batch = release_group_ids[i:i + batch_size]
            tasks = [cache_rg(rg, i + idx) for idx, rg in enumerate(batch)]
            results = await asyncio.gather(*tasks, return_exceptions=True)
            processed_mbids = []
            for idx, result in enumerate(results):
                if isinstance(result, tuple) and len(result) == 3:
                    rgid, meta, cover = result
                    if meta:
                        metadata_fetched += 1
                    if cover:
                        covers_fetched += 1
                    if rgid:
                        processed_mbids.append(rgid)
                elif isinstance(result, Exception):
                    rgid = batch[idx] if idx < len(batch) else 'Unknown'
                    logger.error(f"Batch error caching album {rgid[:8] if isinstance(rgid, str) else rgid}: {result}")
                    if isinstance(rgid, str):
                        processed_mbids.append(rgid)
            if processed_mbids:
                await self._sync_state_store.mark_items_processed_batch('album', processed_mbids)
            await status_service.persist_progress(generation=generation)
            batch_duration = time.time() - batch_start
            avg_time_per_item = batch_duration / len(batch) if batch else 1.0
            if avg_time_per_item > 1.5:
                consecutive_slow_batches += 1
                if consecutive_slow_batches >= 3:
                    batch_size = max(batch_size - 2, min_batch)
                    logger.warning(f"Sustained slowness detected, reducing batch size to {batch_size}")
                elif batch_size > min_batch:
                    batch_size = max(batch_size - 1, min_batch)
                    logger.debug(f"Decreasing batch size to {batch_size} (slow: {avg_time_per_item:.2f}s/item)")
            else:
                consecutive_slow_batches = 0
                if avg_time_per_item < 0.8 and batch_size < max_batch:
                    batch_size = min(batch_size + 1, max_batch)
                    logger.debug(f"Increasing batch size to {batch_size} (fast: {avg_time_per_item:.2f}s/item)")
            next_i = i + len(batch)
            i = next_i
            await asyncio.sleep(advanced_settings.delay_albums)
        await status_service.persist_progress(force=True, generation=generation)
