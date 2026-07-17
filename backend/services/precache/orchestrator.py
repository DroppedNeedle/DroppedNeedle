"""Pre-cache orchestrator: delegates to phase sub-services."""

from __future__ import annotations

import logging
import asyncio
import time
from typing import Any, TYPE_CHECKING

from repositories.protocols import LibraryRepositoryProtocol, CoverArtRepositoryProtocol
from repositories.coverart_disk_cache import get_cache_filename
from services.cache_status_service import CacheStatusService
from core.exceptions import ExternalServiceError
from infrastructure.cache.cache_keys import ALBUM_INFO_PREFIX
from infrastructure.validators import is_unknown_mbid, is_valid_mbid

from .artist_phase import ArtistPhase
from .album_phase import AlbumPhase
from .audiodb_phase import AudioDBPhase

if TYPE_CHECKING:
    from infrastructure.persistence import SyncStateStore, GenreIndex, LibraryDB
    from services.audiodb_image_service import AudioDBImageService

logger = logging.getLogger(__name__)


class LibraryPrecacheService:
    def __init__(
        self,
        library_repo: LibraryRepositoryProtocol,
        cover_repo: CoverArtRepositoryProtocol,
        preferences_service: Any,
        sync_state_store: "SyncStateStore",
        genre_index: "GenreIndex",
        library_db: "LibraryDB",
        artist_discovery_service: Any = None,
        audiodb_image_service: 'AudioDBImageService | None' = None,
    ):
        self._library_repo = library_repo
        self._cover_repo = cover_repo
        self._preferences_service = preferences_service
        self._sync_state_store = sync_state_store
        self._artist_discovery_service = artist_discovery_service
        self._audiodb_image_service = audiodb_image_service

        self._artist_phase = ArtistPhase(library_repo, cover_repo, preferences_service, genre_index, sync_state_store)
        self._album_phase = AlbumPhase(cover_repo, preferences_service, sync_state_store)
        self._audiodb_phase = AudioDBPhase(cover_repo, preferences_service, audiodb_image_service)

    # Delegation for backward compat (tests access private methods)
    async def _check_audiodb_cache_needs(self, artists, albums):
        return await self._audiodb_phase.check_cache_needs(artists, albums)

    async def _precache_audiodb_data(self, artists, albums, status_service):
        return await self._audiodb_phase.precache_audiodb_data(artists, albums, status_service)

    async def _download_audiodb_bytes(self, url, entity_type, mbid):
        return await self._audiodb_phase.download_bytes(url, entity_type, mbid)

    def _sort_by_cover_priority(self, items, item_type):
        return self._audiodb_phase.sort_by_cover_priority(items, item_type)

    async def precache_library_resources(self, artists: list[dict], albums: list[Any], resume: bool = False) -> None:
        status_service = CacheStatusService(self._sync_state_store)
        task = None

        advanced_settings = self._preferences_service.get_advanced_settings()
        stall_timeout_s = advanced_settings.sync_stall_timeout_minutes * 60
        max_timeout_s = advanced_settings.sync_max_timeout_hours * 3600

        try:
            task = asyncio.create_task(self._do_precache(artists, albums, status_service, resume))
            from core.task_registry import TaskRegistry
            TaskRegistry.get_instance().register("precache-library", task)

            watchdog = asyncio.create_task(
                self._watchdog(task, status_service, stall_timeout_s, max_timeout_s)
            )

            done, _ = await asyncio.wait(
                {task, watchdog}, return_when=asyncio.FIRST_COMPLETED
            )

            # Always prioritise the main task result; if it completed
            # successfully we don't care about a simultaneous watchdog error.
            if task in done:
                watchdog.cancel()
                try:
                    await watchdog
                except asyncio.CancelledError:
                    pass
                if task.exception():
                    raise task.exception()
            elif watchdog in done:
                exc = watchdog.exception() if watchdog.done() and not watchdog.cancelled() else None
                if exc:
                    if not task.done():
                        task.cancel()
                        try:
                            await asyncio.wait_for(asyncio.shield(task), timeout=15)
                        except asyncio.CancelledError:
                            if asyncio.current_task().cancelling() > 0:
                                raise  # outer task cancelled; propagate
                            # inner task exited cleanly after cancel
                        except (asyncio.TimeoutError, Exception):  # noqa: BLE001
                            logger.warning("Precache task did not exit within 15s of cancel")
                    await status_service.complete_sync(str(exc))
                    raise ExternalServiceError(str(exc))

        except asyncio.CancelledError:
            logger.warning("Pre-cache was cancelled")
            await status_service.complete_sync()
            raise
        except ExternalServiceError:
            raise
        except Exception as e:
            logger.error(f"Pre-cache failed: {e}")
            await status_service.complete_sync(str(e))
            raise

    async def _watchdog(
        self,
        task: asyncio.Task,
        status_service: CacheStatusService,
        stall_timeout_s: float,
        max_timeout_s: float,
    ) -> None:
        start = time.time()
        while not task.done():
            await asyncio.sleep(30)
            elapsed = time.time() - start
            if elapsed > max_timeout_s:
                msg = f"Sync exceeded maximum timeout ({max_timeout_s / 3600:.1f}h)"
                logger.error(msg)
                raise ExternalServiceError(msg)
            stall_duration = time.time() - status_service.get_last_progress_at()
            if stall_duration > stall_timeout_s:
                msg = (
                    f"Sync stalled: no progress for {stall_duration / 60:.0f} minutes "
                    f"during {status_service.get_progress().phase or 'unknown'} phase"
                )
                logger.error(msg)
                raise ExternalServiceError(msg)

    async def _do_precache(self, artists: list[dict], albums: list[Any], status_service: CacheStatusService, resume: bool = False) -> None:
        from core.dependencies import get_album_service
        generation = 0
        try:
            processed_artists: set[str] = set()
            processed_albums: set[str] = set()
            skip_artists = False

            if resume:
                processed_artists = await self._sync_state_store.get_processed_items('artist')
                processed_albums = await self._sync_state_store.get_processed_items('album')

                state = await self._sync_state_store.get_sync_state()
                if state and state.get('phase') in ('albums', 'audiodb_prewarm'):
                    skip_artists = True
                else:
                    pass

            total_artists = len(artists)
            total_albums = len(albums)

            album_service = get_album_service()
            library_artist_mbids = await self._library_repo.get_artist_mbids()
            library_album_mbids = await self._library_repo.get_library_mbids(include_release_ids=True)

            if not skip_artists:
                remaining_artists = [a for a in artists if a.get('mbid') not in processed_artists]
                if remaining_artists:
                    generation = await status_service.start_sync('artists', len(remaining_artists), total_artists=total_artists, total_albums=total_albums)
                    await self._artist_phase.precache_artist_images(remaining_artists, status_service, library_artist_mbids, library_album_mbids, len(processed_artists), generation=generation)
                else:
                    generation = await status_service.start_sync('artists', 0, total_artists=total_artists, total_albums=total_albums)
                    await status_service.skip_phase('artists', generation=generation)
            else:
                generation = await status_service.start_sync('albums', 0, total_artists=total_artists, total_albums=total_albums)
            if status_service.is_cancelled():
                return

            if self._artist_discovery_service and not skip_artists:
                artist_mbids = list(dict.fromkeys(
                    a.get('mbid') for a in artists
                    if is_valid_mbid(a.get('mbid'))
                ))
                if artist_mbids:
                    await status_service.update_phase('discovery', len(artist_mbids), generation=generation)
                    mbid_to_name = {
                        a.get('mbid'): a.get('name', a.get('mbid', '')[:8])
                        for a in artists if a.get('mbid')
                    }
                    try:
                        advanced_settings = self._preferences_service.get_advanced_settings()
                        precache_delay = advanced_settings.artist_discovery_precache_delay
                        await self._artist_discovery_service.precache_artist_discovery(
                            artist_mbids, delay=precache_delay,
                            status_service=status_service, mbid_to_name=mbid_to_name,
                            generation=generation,
                        )
                    except Exception as e:  # noqa: BLE001
                        logger.warning(f"Discovery precache failed (non-fatal): {e}")
                else:
                    await status_service.skip_phase('discovery', generation=generation)
            elif not skip_artists:
                await status_service.skip_phase('discovery', generation=generation)

            if status_service.is_cancelled():
                return

            library_album_mbids_set: set[str] = set()
            for a in albums:
                mbid = getattr(a, 'musicbrainz_id', None) if hasattr(a, 'musicbrainz_id') else a.get('mbid') if isinstance(a, dict) else None
                if is_valid_mbid(mbid):
                    library_album_mbids_set.add(mbid.lower())
            deduped_release_groups = list(library_album_mbids_set)
            if status_service.is_cancelled():
                return
            items_needing_metadata = []
            cache_checks = []
            for rgid in deduped_release_groups:
                if rgid in processed_albums:
                    continue
                cache_key = f"{ALBUM_INFO_PREFIX}{rgid}"
                cache_checks.append((rgid, album_service._cache.get(cache_key)))
            cache_results = await asyncio.gather(*[check for _, check in cache_checks])
            for (rgid, _), cached_info in zip(cache_checks, cache_results):
                if not cached_info:
                    items_needing_metadata.append(rgid)
            items_needing_covers = []
            cover_paths = []
            for rgid in deduped_release_groups:
                if rgid in processed_albums:
                    continue
                if rgid.lower() in library_album_mbids_set:
                    cache_filename = get_cache_filename(f"rg_{rgid}", "500")
                    file_path = self._cover_repo.cache_dir / f"{cache_filename}.bin"
                    cover_paths.append((rgid, file_path))
            for rgid, file_path in cover_paths:
                if not file_path.exists():
                    items_needing_covers.append(rgid)
            items_to_process = list(set(items_needing_metadata + items_needing_covers))
            if items_to_process:
                await status_service.update_phase('albums', len(items_to_process), generation=generation)
                await self._album_phase.precache_album_data(items_to_process, library_album_mbids_set, status_service, library_album_mbids, len(processed_albums), generation=generation)
            else:
                await status_service.skip_phase('albums', generation=generation)

            if status_service.is_cancelled():
                return

            try:
                await self._audiodb_phase.precache_audiodb_data(artists, albums, status_service, generation=generation)
            except Exception as e:  # noqa: BLE001
                logger.warning(f"AudioDB pre-warming failed (non-fatal): {e}")

            if not status_service.is_cancelled():
                await status_service.complete_sync(generation=generation)
        except Exception as e:
            logger.error(f"Error during pre-cache: {e}")
            raise
        finally:
            if status_service.is_syncing():
                await status_service.complete_sync(generation=generation)
