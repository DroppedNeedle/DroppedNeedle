"""Artist metadata + image pre-caching phase."""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from repositories.protocols import LibraryRepositoryProtocol, CoverArtRepositoryProtocol
from repositories.coverart_disk_cache import get_cache_filename
from services.cache_status_service import CacheStatusService
from infrastructure.cache.cache_keys import ARTIST_INFO_PREFIX

logger = logging.getLogger(__name__)


class ArtistPhase:
    def __init__(
        self,
        library_repo: LibraryRepositoryProtocol,
        cover_repo: CoverArtRepositoryProtocol,
        preferences_service: Any,
        genre_index: Any,
        sync_state_store: Any,
    ):
        self._library_repo = library_repo
        self._cover_repo = cover_repo
        self._preferences_service = preferences_service
        self._genre_index = genre_index
        self._sync_state_store = sync_state_store

    async def precache_artist_images(
        self,
        artists: list[dict],
        status_service: CacheStatusService,
        library_artist_mbids: set[str] = None,
        library_album_mbids: dict[str, Any] = None,
        offset: int = 0,
        generation: int = 0,
    ) -> None:
        from core.dependencies import get_artist_service
        from infrastructure.validators import is_valid_mbid
        artist_service = get_artist_service()

        seen_mbids: set[str] = set()
        unique_artists: list[dict] = []
        for a in artists:
            mbid = a.get('mbid')
            if not is_valid_mbid(mbid):
                unique_artists.append(a)
            elif mbid.lower() not in seen_mbids:
                seen_mbids.add(mbid.lower())
                unique_artists.append(a)
        artists = unique_artists

        async def cache_artist(artist: dict, index: int) -> str:
            mbid = artist.get('mbid')
            try:
                artist_name = artist.get('name', 'Unknown')
                if not is_valid_mbid(mbid):
                    await status_service.update_progress(index + 1, artist_name, processed_artists=offset + index + 1, generation=generation)
                    return mbid
                artist_cache_key = f"{ARTIST_INFO_PREFIX}{mbid}"
                cached_artist = await artist_service._cache.get(artist_cache_key)
                if not cached_artist:
                    try:
                        await artist_service.get_artist_info(mbid, library_artist_mbids, library_album_mbids)
                    except Exception:  # noqa: BLE001
                        logger.debug(f"Failed to cache artist metadata for {artist_name}")
                else:
                    logger.debug(f"Artist metadata for {artist_name} already cached, skipping fetch")
                cache_filename_250 = get_cache_filename(f"artist_{mbid}_250", "img")
                file_path_250 = self._cover_repo.cache_dir / f"{cache_filename_250}.bin"
                cache_filename_500 = get_cache_filename(f"artist_{mbid}_500", "img")
                file_path_500 = self._cover_repo.cache_dir / f"{cache_filename_500}.bin"
                if file_path_250.exists() and file_path_500.exists():
                    logger.debug(f"Artist images for {artist_name} already cached, skipping")
                    await status_service.update_progress(index + 1, artist_name, processed_artists=offset + index + 1, generation=generation)
                    return mbid
                await status_service.update_progress(index + 1, f"Fetching images for {artist_name}", processed_artists=offset + index + 1, generation=generation)
                if not file_path_250.exists():
                    await self._cover_repo.get_artist_image(mbid, size=250)
                if not file_path_500.exists():
                    await self._cover_repo.get_artist_image(mbid, size=500)
                await status_service.update_progress(index + 1, artist_name, processed_artists=offset + index + 1, generation=generation)
                return mbid
            except Exception as e:  # noqa: BLE001
                logger.warning(f"Failed to cache artist {artist.get('name')} (mbid: {mbid}): {e}", exc_info=True)
                await status_service.update_progress(index + 1, f"Failed: {artist.get('name', 'Unknown')}", processed_artists=offset + index + 1, generation=generation)
                return mbid

        advanced_settings = self._preferences_service.get_advanced_settings()
        batch_size = advanced_settings.batch_artist_images
        for i in range(0, len(artists), batch_size):
            if status_service.is_cancelled():
                break
            batch = artists[i:i + batch_size]
            tasks = [cache_artist(artist, i + idx) for idx, artist in enumerate(batch)]
            results = await asyncio.gather(*tasks, return_exceptions=True)
            processed_mbids = []
            for idx, result in enumerate(results):
                if isinstance(result, Exception):
                    artist_name = batch[idx].get('name', 'Unknown')
                    logger.error(f"Batch error caching artist {artist_name}: {result}")
                    processed_mbids.append(batch[idx].get('mbid'))
                elif result:
                    processed_mbids.append(result)
            if processed_mbids:
                await self._sync_state_store.mark_items_processed_batch('artist', processed_mbids)
            await status_service.persist_progress(generation=generation)
            await asyncio.sleep(advanced_settings.delay_artist)
        await status_service.persist_progress(force=True, generation=generation)
        await self._cache_artist_genres(artists)

    async def _cache_artist_genres(self, artists: list[dict]) -> None:
        from core.dependencies import get_artist_service
        artist_service = get_artist_service()
        artist_genres: dict[str, list[str]] = {}
        for artist in artists:
            mbid = artist.get('mbid')
            if not mbid:
                continue
            cache_key = f"{ARTIST_INFO_PREFIX}{mbid}"
            cached_info = await artist_service._cache.get(cache_key)
            if cached_info and hasattr(cached_info, 'tags') and cached_info.tags:
                artist_genres[mbid] = cached_info.tags[:10]
        if artist_genres:
            await self._genre_index.save_artist_genres(artist_genres)
