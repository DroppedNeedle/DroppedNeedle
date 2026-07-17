from __future__ import annotations

import asyncio
import logging
import time
from pathlib import Path
from typing import TYPE_CHECKING

import msgspec

from infrastructure.validators import validate_mbid, validate_audiodb_image_url
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.http.disconnect import DisconnectCallable, check_disconnected
from core.exceptions import ClientDisconnectedError

if TYPE_CHECKING:
    from services.audiodb_image_service import AudioDBImageService
    from services.audiodb_browse_queue import AudioDBBrowseQueue
    from repositories.protocols.library import LibraryRepositoryProtocol
    from repositories.musicbrainz_repository import MusicBrainzRepository
    from repositories.jellyfin_repository import JellyfinRepository

logger = logging.getLogger(__name__)


class _ReleaseGroupMetadataResponse(msgspec.Struct):
    release: str | None = None


def _decode_json_response(response, decode_type: type[_ReleaseGroupMetadataResponse]) -> _ReleaseGroupMetadataResponse:
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray, memoryview)):
        return msgspec.json.decode(content, type=decode_type)
    return msgspec.convert(response.json(), type=decode_type)


def _log_task_error(task: asyncio.Task) -> None:
    if not task.cancelled() and task.exception():
        logger.error(f"Background cache write failed: {task.exception()}")


COVER_ART_ARCHIVE_BASE = "https://coverartarchive.org"

VALID_IMAGE_CONTENT_TYPES = frozenset([
    "image/jpeg", "image/jpg", "image/png", "image/gif",
    "image/webp", "image/avif", "image/svg+xml",
])
LOCAL_SOURCE_TIMEOUT_SECONDS = 1.0
RELEASE_AUDIODB_STATE_TTL_SECONDS = 300.0


def _is_valid_image_content_type(content_type: str) -> bool:
    if not content_type:
        return False
    base_type = content_type.split(";")[0].strip().lower()
    return base_type in VALID_IMAGE_CONTENT_TYPES


class AlbumCoverFetcher:
    def __init__(
        self,
        http_get_fn,
        write_cache_fn,
        library_repo: 'LibraryRepositoryProtocol' | None = None,
        mb_repo: 'MusicBrainzRepository' | None = None,
        jellyfin_repo: 'JellyfinRepository' | None = None,
        audiodb_service: 'AudioDBImageService' | None = None,
        audiodb_browse_queue: 'AudioDBBrowseQueue' | None = None,
    ):
        self._http_get = http_get_fn
        self._write_disk_cache = write_cache_fn
        self._library_repo = library_repo
        self._mb_repo = mb_repo
        self._jellyfin_repo = jellyfin_repo
        self._audiodb_service = audiodb_service
        self._audiodb_browse_queue = audiodb_browse_queue
        self._release_audiodb_warming: dict[str, tuple[str, float]] = {}
        self._release_audiodb_resolvers: dict[str, asyncio.Task] = {}

    async def fetch_release_group_cover(
        self,
        release_group_id: str,
        size: str | None,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
        is_disconnected: DisconnectCallable | None = None,
        include_best_release: bool = True,
    ) -> tuple[bytes, str, str] | None:
        size_int = int(size) if size and size.isdigit() else 500
        await check_disconnected(is_disconnected)
        result = await self._fetch_from_audiodb(release_group_id, file_path, priority=priority)
        if result:
            return result
        result = None
        try:
            await check_disconnected(is_disconnected)
            result = await asyncio.wait_for(
                self._fetch_release_group_local_sources(release_group_id, file_path, size_int, priority=priority),
                timeout=LOCAL_SOURCE_TIMEOUT_SECONDS,
            )
        except TimeoutError:
            pass
        if result:
            return result
        size_suffix = f"-{size}" if size else ""
        front_url = f"{COVER_ART_ARCHIVE_BASE}/release-group/{release_group_id}/front{size_suffix}"
        await check_disconnected(is_disconnected)
        try:
            response = await self._http_get(
                front_url,
                priority,
                source="coverart",
            )
            if response.status_code == 200:
                content_type = response.headers.get("content-type", "")
                if not _is_valid_image_content_type(content_type):
                    logger.warning(f"Non-image content-type from CoverArtArchive: {content_type}")
                else:
                    content = response.content
                    task = asyncio.create_task(
                        self._write_disk_cache(
                            file_path,
                            content,
                            content_type,
                            {"source": "cover-art-archive"},
                        )
                    )
                    task.add_done_callback(_log_task_error)
                    return (content, content_type, "cover-art-archive")
        except ClientDisconnectedError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.debug(f"Failed to fetch cover via release group: {e}")
        # The best-release fallback costs two more serial archive.org calls (release-group
        # metadata -> representative release front). The app's hot path defers it to the
        # background (include_best_release=False); compat/prewarm callers run it inline.
        if not include_best_release:
            return None
        await check_disconnected(is_disconnected)
        return await self._get_cover_from_best_release(release_group_id, size, file_path, priority=priority, is_disconnected=is_disconnected)

    async def _fetch_release_group_local_sources(
        self,
        release_group_id: str,
        file_path: Path,
        size: int,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[bytes, str, str] | None:
        result = await self._fetch_from_library(release_group_id, file_path, size=size, priority=priority)
        if result:
            return result
        return await self._fetch_from_jellyfin(release_group_id, file_path, priority=priority)

    async def _fetch_from_audiodb(
        self,
        release_group_id: str,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[bytes, str, str] | None:
        if self._audiodb_service is None or not self._audiodb_service.is_enabled():
            return None
        try:
            # Live lookups share a rate-limited key, so a miss queues metadata while
            # local and CAA fallbacks continue loading the grid.
            cached_images = await self._audiodb_service.get_cached_album_images(release_group_id)
            if cached_images is None:
                if self._audiodb_browse_queue is not None:
                    await self._audiodb_browse_queue.enqueue("album", release_group_id)
                return None
            if cached_images.is_negative or not cached_images.album_thumb_url:
                return None
            if not validate_audiodb_image_url(cached_images.album_thumb_url):
                logger.warning("[IMG:AudioDB] Rejected unsafe URL for album %s", release_group_id[:8])
                return None
            response = await self._http_get(
                cached_images.album_thumb_url,
                priority,
                source="audiodb",
            )
            if response.status_code != 200:
                return None
            content_type = response.headers.get("content-type", "")
            if not _is_valid_image_content_type(content_type):
                logger.warning(f"[IMG:AudioDB] Non-image content-type ({content_type}) for {release_group_id[:8]}")
                return None
            content = response.content
            task = asyncio.create_task(
                self._write_disk_cache(file_path, content, content_type, {"source": "audiodb"})
            )
            task.add_done_callback(_log_task_error)
            return (content, content_type, "audiodb")
        except ClientDisconnectedError:
            raise
        except Exception:  # noqa: BLE001
            return None

    def is_audiodb_album_warming(self, release_group_id: str) -> bool:
        return bool(
            self._audiodb_browse_queue
            and self._audiodb_browse_queue.is_pending("album", release_group_id)
        )

    async def fetch_cached_audiodb_cover(
        self,
        release_group_id: str,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[bytes, str, str] | None:
        return await self._fetch_from_audiodb(
            release_group_id, file_path, priority=priority
        )

    def is_audiodb_release_warming(self, release_id: str) -> bool:
        if release_id in self._release_audiodb_resolvers:
            return True
        return self._get_warming_release_group(release_id) is not None

    def _get_warming_release_group(self, release_id: str) -> str | None:
        self._prune_expired_release_states()
        state = self._release_audiodb_warming.get(release_id)
        if state is None:
            return None
        release_group_id, _ = state
        return release_group_id

    def _prune_expired_release_states(self) -> None:
        now = time.monotonic()
        expired = [
            release_id
            for release_id, (_, expires_at) in self._release_audiodb_warming.items()
            if expires_at <= now
        ]
        for release_id in expired:
            self._release_audiodb_warming.pop(release_id, None)

    def _spawn_release_audiodb_resolve(self, release_id: str) -> None:
        self._prune_expired_release_states()
        if self._mb_repo is None or release_id in self._release_audiodb_resolvers:
            return

        async def _resolve() -> None:
            try:
                release_group_id = await self._mb_repo.get_release_group_id_from_release(
                    release_id,
                    priority=RequestPriority.BACKGROUND_SYNC,
                )
                if not release_group_id:
                    return
                self._release_audiodb_warming[release_id] = (
                    release_group_id,
                    time.monotonic() + RELEASE_AUDIODB_STATE_TTL_SECONDS,
                )
                if self._audiodb_browse_queue is not None:
                    await self._audiodb_browse_queue.enqueue(
                        "album", release_group_id
                    )
            finally:
                self._release_audiodb_resolvers.pop(release_id, None)

        task = asyncio.create_task(_resolve())
        self._release_audiodb_resolvers[release_id] = task
        task.add_done_callback(_log_task_error)

    async def fetch_release_audiodb_cover(
        self,
        release_id: str,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
        is_disconnected: DisconnectCallable | None = None,
    ) -> tuple[bytes, str, str] | None:
        if self._audiodb_service is None or not self._audiodb_service.is_enabled():
            return None
        await check_disconnected(is_disconnected)
        release_group_id = self._get_warming_release_group(release_id)
        if release_group_id is None:
            self._spawn_release_audiodb_resolve(release_id)
            return None
        result = await self._fetch_from_audiodb(
            release_group_id, file_path, priority=priority
        )
        if result is not None or not self.is_audiodb_album_warming(release_group_id):
            self._release_audiodb_warming.pop(release_id, None)
        return result

    async def _get_cover_from_best_release(
        self,
        release_group_id: str,
        size: str | None,
        cache_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
        is_disconnected: DisconnectCallable | None = None,
    ) -> tuple[bytes, str, str] | None:
        try:
            metadata_url = f"{COVER_ART_ARCHIVE_BASE}/release-group/{release_group_id}"
            response = await self._http_get(
                metadata_url,
                priority,
                source="coverart",
                headers={"Accept": "application/json"},
            )
            if response.status_code != 200:
                return None
            data = _decode_json_response(response, _ReleaseGroupMetadataResponse)
            release_url = data.release or ""
            if not release_url:
                return None
            release_id = release_url.split("/")[-1]
            try:
                release_id = validate_mbid(release_id, "release")
            except ValueError as e:
                logger.warning(f"Invalid release MBID extracted from metadata: {e}")
                return None
            await check_disconnected(is_disconnected)
            size_suffix = f"-{size}" if size else ""
            release_front_url = f"{COVER_ART_ARCHIVE_BASE}/release/{release_id}/front{size_suffix}"
            response = await self._http_get(
                release_front_url,
                priority,
                source="coverart",
            )
            if response.status_code == 200:
                content_type = response.headers.get("content-type", "")
                if not _is_valid_image_content_type(content_type):
                    logger.warning(f"Non-image content-type from release: {content_type}")
                    return None
                content = response.content
                task = asyncio.create_task(
                    self._write_disk_cache(
                        cache_path,
                        content,
                        content_type,
                        {"source": "cover-art-archive"},
                    )
                )
                task.add_done_callback(_log_task_error)
                return (content, content_type, "cover-art-archive")
        except ClientDisconnectedError:
            raise
        except Exception:  # noqa: BLE001
            return None
        return None

    async def _fetch_from_library(
        self,
        release_group_id: str,
        file_path: Path,
        size: int | None = 500,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[bytes, str, str] | None:
        if not self._library_repo:
            return None
        if not self._library_repo.is_configured():
            return None
        try:
            image_url = await self._library_repo.get_album_image_url(release_group_id, size=size)
            if not image_url:
                return None
            response = await self._http_get(
                image_url,
                priority,
                source="library",
            )
            if response.status_code != 200:
                return None
            content_type = response.headers.get("content-type", "")
            if not _is_valid_image_content_type(content_type):
                logger.warning(f"Non-image content-type from Lidarr album: {content_type}")
                return None
            content = response.content
            task = asyncio.create_task(self._write_disk_cache(file_path, content, content_type, {"source": "library"}))
            task.add_done_callback(_log_task_error)
            return (content, content_type, "library")
        except Exception:  # noqa: BLE001
            return None

    async def _fetch_from_jellyfin(
        self,
        musicbrainz_id: str,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[bytes, str, str] | None:
        if not self._jellyfin_repo or not self._jellyfin_repo.is_configured():
            return None
        try:
            album = await self._jellyfin_repo.get_album_by_mbid(musicbrainz_id)
            if not album:
                return None
            image_url = self._jellyfin_repo.get_image_url(album.id, album.image_tag)
            if not image_url:
                return None
            response = await self._http_get(
                image_url,
                priority,
                source="jellyfin",
                headers=self._jellyfin_repo.get_auth_headers(),
            )
            if response.status_code != 200:
                return None
            content_type = response.headers.get("content-type", "")
            if not _is_valid_image_content_type(content_type):
                logger.warning(f"Non-image content-type from Jellyfin album: {content_type}")
                return None
            content = response.content
            task = asyncio.create_task(
                self._write_disk_cache(file_path, content, content_type, {"source": "jellyfin"})
            )
            task.add_done_callback(_log_task_error)
            return (content, content_type, "jellyfin")
        except Exception:  # noqa: BLE001
            return None

    async def fetch_release_cover(
        self,
        release_id: str,
        size: str | None,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
        is_disconnected: DisconnectCallable | None = None,
    ) -> tuple[bytes, str, str] | None:
        result = await self.fetch_release_audiodb_cover(
            release_id,
            file_path,
            priority=priority,
            is_disconnected=is_disconnected,
        )
        if result:
            return result
        release_group_id = self._get_warming_release_group(release_id)
        try:
            await check_disconnected(is_disconnected)
            result = await asyncio.wait_for(
                self._fetch_release_local_sources(
                    release_id,
                    file_path,
                    size,
                    release_group_id,
                    priority=priority,
                    resolve_release_group=False,
                ),
                timeout=LOCAL_SOURCE_TIMEOUT_SECONDS,
            )
        except TimeoutError:
            pass
        if result:
            return result
        return await self._fetch_release_from_caa(
            release_id,
            size,
            file_path,
            priority=priority,
            is_disconnected=is_disconnected,
        )

    async def _fetch_release_from_caa(
        self,
        release_id: str,
        size: str | None,
        file_path: Path,
        priority: RequestPriority,
        is_disconnected: DisconnectCallable | None,
    ) -> tuple[bytes, str, str] | None:
        size_suffix = f"-{size}" if size else ""
        url = f"{COVER_ART_ARCHIVE_BASE}/release/{release_id}/front{size_suffix}"
        await check_disconnected(is_disconnected)
        try:
            response = await self._http_get(url, priority, source="coverart")
            if response.status_code == 200:
                content_type = response.headers.get("content-type", "")
                if not _is_valid_image_content_type(content_type):
                    logger.warning(f"Non-image content-type from release cover: {content_type}")
                    return None
                content = response.content
                task = asyncio.create_task(
                    self._write_disk_cache(
                        file_path,
                        content,
                        content_type,
                        {"source": "cover-art-archive"},
                    )
                )
                task.add_done_callback(_log_task_error)
                return (content, content_type, "cover-art-archive")
        except ClientDisconnectedError:
            raise
        except Exception:  # noqa: BLE001
            pass
        return None

    async def _fetch_release_local_sources(
        self,
        release_id: str,
        file_path: Path,
        size: str | None,
        release_group_id: str | None = None,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
        resolve_release_group: bool = True,
    ) -> tuple[bytes, str, str] | None:
        size_int = int(size) if size and size.isdigit() else 500
        if release_group_id is None and self._mb_repo and resolve_release_group:
            release_group_id = await self._mb_repo.get_release_group_id_from_release(
                release_id,
                priority=priority,
            )

        if release_group_id:
            result = await self._fetch_from_library(release_group_id, file_path, size=size_int, priority=priority)
            if result:
                return result
            result = await self._fetch_from_jellyfin(release_group_id, file_path, priority=priority)
            if result:
                return result

        return await self._fetch_from_jellyfin(release_id, file_path, priority=priority)
