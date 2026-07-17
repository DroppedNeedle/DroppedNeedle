import logging
from typing import Any

import msgspec

from infrastructure.cache.disk_cache import DiskMetadataCache
from infrastructure.cache.memory_cache import CacheInterface
from repositories.audiodb_models import (
    AudioDBArtistImages,
    AudioDBAlbumImages,
)
from repositories.audiodb_repository import AudioDBRepository
from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

MEMORY_TTL_SECONDS = 300


class AudioDBImageService:

    def __init__(
        self,
        audiodb_repo: AudioDBRepository,
        disk_cache: DiskMetadataCache,
        preferences_service: PreferencesService,
        memory_cache: CacheInterface | None = None,
    ):
        self._repo = audiodb_repo
        self._disk_cache = disk_cache
        self._preferences_service = preferences_service
        self._memory_cache = memory_cache

    def _get_settings(self):
        return self._preferences_service.get_advanced_settings()

    def is_enabled(self) -> bool:
        return self._get_settings().audiodb_enabled

    def _mem_key(self, entity_type: str, mbid: str) -> str:
        return f"audiodb_{entity_type}:{mbid}"

    async def _mem_get(self, entity_type: str, mbid: str) -> Any | None:
        if self._memory_cache is None:
            return None
        return await self._memory_cache.get(self._mem_key(entity_type, mbid))

    async def _mem_set(self, entity_type: str, mbid: str, value: Any) -> None:
        if self._memory_cache is None:
            return
        await self._memory_cache.set(self._mem_key(entity_type, mbid), value, ttl_seconds=MEMORY_TTL_SECONDS)

    @staticmethod
    def _resolve_ttl(is_monitored: bool, ttl_library: int, ttl_found: int) -> int:
        return ttl_library if is_monitored else ttl_found

    async def get_cached_artist_images(self, mbid: str) -> AudioDBArtistImages | None:
        if not self._get_settings().audiodb_enabled:
            return None
        if not mbid or not mbid.strip():
            return None

        mem_hit = await self._mem_get("artist", mbid)
        if isinstance(mem_hit, AudioDBArtistImages):
            return mem_hit

        raw = await self._disk_cache.get_audiodb_artist(mbid)
        if raw is None:
            return None

        try:
            images = msgspec.convert(raw, type=AudioDBArtistImages)
        except (msgspec.ValidationError, msgspec.DecodeError, TypeError, KeyError):
            logger.warning("audiodb.cache action=corrupt entity_type=artist mbid=%s lookup_source=mbid", mbid)
            await self._disk_cache.delete_entity("audiodb_artist", mbid)
            return None

        await self._mem_set("artist", mbid, images)
        return images

    async def get_cached_album_images(self, mbid: str) -> AudioDBAlbumImages | None:
        if not self._get_settings().audiodb_enabled:
            return None
        if not mbid or not mbid.strip():
            return None

        mem_hit = await self._mem_get("album", mbid)
        if isinstance(mem_hit, AudioDBAlbumImages):
            return mem_hit

        raw = await self._disk_cache.get_audiodb_album(mbid)
        if raw is None:
            return None

        try:
            images = msgspec.convert(raw, type=AudioDBAlbumImages)
        except (msgspec.ValidationError, msgspec.DecodeError, TypeError, KeyError):
            logger.warning("audiodb.cache action=corrupt entity_type=album mbid=%s lookup_source=mbid", mbid)
            await self._disk_cache.delete_entity("audiodb_album", mbid)
            return None

        await self._mem_set("album", mbid, images)
        return images

    async def fetch_and_cache_artist_images(
        self,
        mbid: str,
        name: str | None = None,
        is_monitored: bool = False,
    ) -> AudioDBArtistImages | None:
        settings = self._get_settings()
        if not settings.audiodb_enabled:
            return None
        if not mbid or not mbid.strip():
            return None

        cached = await self.get_cached_artist_images(mbid)
        if cached is not None:
            if not cached.is_negative:
                return cached
            if cached.lookup_source == "name":
                return cached
            should_fallback = is_monitored or settings.audiodb_name_search_fallback
            if not name or not name.strip() or not should_fallback:
                return cached

        ttl_found = settings.cache_ttl_audiodb_found
        ttl_not_found = settings.cache_ttl_audiodb_not_found
        ttl_library = settings.cache_ttl_audiodb_library

        cached_negative_mbid = cached is not None and cached.is_negative and cached.lookup_source == "mbid"

        if not cached_negative_mbid:
            try:
                resp = await self._repo.get_artist_by_mbid(mbid)
            except Exception:  # noqa: BLE001
                logger.warning("audiodb.cache action=fetch_error entity_type=artist mbid=%s lookup_source=mbid", mbid, exc_info=True)
                return None

            if resp is not None:
                images = AudioDBArtistImages.from_response(resp, lookup_source="mbid")
                ttl = self._resolve_ttl(is_monitored, ttl_library, ttl_found)
                await self._disk_cache.set_audiodb_artist(
                    mbid, images, is_monitored=is_monitored, ttl_seconds=ttl,
                )
                await self._mem_set("artist", mbid, images)
                return images

            negative = AudioDBArtistImages.negative(lookup_source="mbid")
            await self._disk_cache.set_audiodb_artist(
                mbid, negative, is_monitored=False, ttl_seconds=ttl_not_found,
            )
            await self._mem_set("artist", mbid, negative)
        else:
            negative = cached

        if name and name.strip() and (is_monitored or settings.audiodb_name_search_fallback):
            try:
                name_resp = await self._repo.search_artist_by_name(name.strip())
            except Exception:  # noqa: BLE001
                logger.warning("audiodb.cache action=fetch_error entity_type=artist mbid=%s lookup_source=name name=%s", mbid, name, exc_info=True)
                return negative

            if name_resp is not None:
                images = AudioDBArtistImages.from_response(name_resp, lookup_source="name")
                ttl = self._resolve_ttl(is_monitored, ttl_library, ttl_found)
                await self._disk_cache.set_audiodb_artist(
                    mbid, images, is_monitored=is_monitored, ttl_seconds=ttl,
                )
                await self._mem_set("artist", mbid, images)
                return images

            negative_name = AudioDBArtistImages.negative(lookup_source="name")
            await self._disk_cache.set_audiodb_artist(
                mbid, negative_name, is_monitored=False, ttl_seconds=ttl_not_found,
            )
            await self._mem_set("artist", mbid, negative_name)
            return negative_name

        return negative

    async def fetch_and_cache_album_images(
        self,
        mbid: str,
        artist_name: str | None = None,
        album_name: str | None = None,
        is_monitored: bool = False,
    ) -> AudioDBAlbumImages | None:
        settings = self._get_settings()
        if not settings.audiodb_enabled:
            return None
        if not mbid or not mbid.strip():
            return None

        cached = await self.get_cached_album_images(mbid)
        if cached is not None:
            if not cached.is_negative:
                return cached
            if cached.lookup_source == "name":
                return cached
            can_name_search = (
                artist_name and artist_name.strip()
                and album_name and album_name.strip()
                and (is_monitored or settings.audiodb_name_search_fallback)
            )
            if not can_name_search:
                return cached

        ttl_found = settings.cache_ttl_audiodb_found
        ttl_not_found = settings.cache_ttl_audiodb_not_found
        ttl_library = settings.cache_ttl_audiodb_library

        cached_negative_mbid = cached is not None and cached.is_negative and cached.lookup_source == "mbid"

        if not cached_negative_mbid:
            try:
                resp = await self._repo.get_album_by_mbid(mbid)
            except Exception:  # noqa: BLE001
                logger.warning("audiodb.cache action=fetch_error entity_type=album mbid=%s lookup_source=mbid", mbid, exc_info=True)
                return None

            if resp is not None:
                images = AudioDBAlbumImages.from_response(resp, lookup_source="mbid")
                ttl = self._resolve_ttl(is_monitored, ttl_library, ttl_found)
                await self._disk_cache.set_audiodb_album(
                    mbid, images, is_monitored=is_monitored, ttl_seconds=ttl,
                )
                await self._mem_set("album", mbid, images)
                return images

            negative = AudioDBAlbumImages.negative(lookup_source="mbid")
            await self._disk_cache.set_audiodb_album(
                mbid, negative, is_monitored=False, ttl_seconds=ttl_not_found,
            )
            await self._mem_set("album", mbid, negative)
        else:
            negative = cached

        can_name_search = (
            artist_name and artist_name.strip()
            and album_name and album_name.strip()
            and (is_monitored or settings.audiodb_name_search_fallback)
        )
        if can_name_search:
            try:
                name_resp = await self._repo.search_album_by_name(
                    artist_name.strip(), album_name.strip()
                )
            except Exception:  # noqa: BLE001
                logger.warning(
                    "audiodb.cache action=fetch_error entity_type=album mbid=%s lookup_source=name artist=%s album=%s",
                    mbid, artist_name, album_name,
                    exc_info=True,
                )
                return negative

            if name_resp is not None:
                images = AudioDBAlbumImages.from_response(name_resp, lookup_source="name")
                ttl = self._resolve_ttl(is_monitored, ttl_library, ttl_found)
                await self._disk_cache.set_audiodb_album(
                    mbid, images, is_monitored=is_monitored, ttl_seconds=ttl,
                )
                await self._mem_set("album", mbid, images)
                return images

            negative_name = AudioDBAlbumImages.negative(lookup_source="name")
            await self._disk_cache.set_audiodb_album(
                mbid, negative_name, is_monitored=False, ttl_seconds=ttl_not_found,
            )
            await self._mem_set("album", mbid, negative_name)
            return negative_name

        return negative
