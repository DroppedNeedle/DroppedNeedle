from __future__ import annotations

import asyncio
import logging
import re
from pathlib import Path
from typing import TYPE_CHECKING, TypeVar
from urllib.parse import quote

import httpx
import msgspec

from core.exceptions import ExternalServiceError, RateLimitedError
from infrastructure.cache.cache_keys import ARTIST_WIKIDATA_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.resilience.retry import CircuitOpenError
from infrastructure.validators import validate_audiodb_image_url
from infrastructure.http.disconnect import DisconnectCallable, check_disconnected

if TYPE_CHECKING:
    from services.audiodb_image_service import AudioDBImageService
    from services.audiodb_browse_queue import AudioDBBrowseQueue
    from repositories.musicbrainz_repository import MusicBrainzRepository
    from repositories.protocols.library import LibraryRepositoryProtocol
    from repositories.jellyfin_repository import JellyfinRepository

logger = logging.getLogger(__name__)
LOCAL_SOURCE_TIMEOUT_SECONDS = 1.0
T = TypeVar("T")
DEFAULT_EXTERNAL_USER_AGENT = "DroppedNeedle/1.0 (contact@droppedneedle.com; https://www.droppedneedle.com)"


class TransientImageFetchError(Exception):
    pass


TRANSIENT_FETCH_EXCEPTIONS = (
    CircuitOpenError,
    httpx.TimeoutException,
    httpx.NetworkError,
    ExternalServiceError,
    RateLimitedError,
)


class _WikidataValue(msgspec.Struct):
    value: str | None = None


class _WikidataSnak(msgspec.Struct):
    datavalue: _WikidataValue | None = None


class _WikidataClaim(msgspec.Struct):
    mainsnak: _WikidataSnak | None = None


class _WikidataClaimsResponse(msgspec.Struct):
    claims: dict[str, list[_WikidataClaim]] = {}


class _CommonsImageInfo(msgspec.Struct):
    url: str | None = None
    thumburl: str | None = None


class _CommonsPage(msgspec.Struct):
    imageinfo: list[_CommonsImageInfo] = []


class _CommonsQuery(msgspec.Struct):
    pages: dict[str, _CommonsPage] = {}


class _CommonsQueryResponse(msgspec.Struct):
    query: _CommonsQuery | None = None


def _decode_json_response(response: httpx.Response, decode_type: type[T]) -> T:
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray, memoryview)):
        return msgspec.json.decode(content, type=decode_type)
    return msgspec.convert(response.json(), type=decode_type)


def _log_task_error(task: asyncio.Task) -> None:
    if not task.cancelled() and task.exception():
        logger.error(f"Background cache write failed: {task.exception()}")


def _is_valid_image_content_type(content_type: str) -> bool:
    if not content_type:
        return False
    base_type = content_type.split(";")[0].strip().lower()
    return base_type in frozenset([
        "image/jpeg", "image/jpg", "image/png", "image/gif",
        "image/webp", "image/avif", "image/svg+xml",
    ])


class ArtistImageFetcher:
    def __init__(
        self,
        http_get_fn,
        write_cache_fn,
        cache: CacheInterface,
        mb_repo: 'MusicBrainzRepository' | None = None,
        library_repo: 'LibraryRepositoryProtocol' | None = None,
        jellyfin_repo: 'JellyfinRepository' | None = None,
        audiodb_service: 'AudioDBImageService' | None = None,
        audiodb_browse_queue: 'AudioDBBrowseQueue' | None = None,
        user_agent: str | None = None,
    ):
        self._http_get = http_get_fn
        self._write_disk_cache = write_cache_fn
        self._cache = cache
        self._mb_repo = mb_repo
        self._library_repo = library_repo
        self._jellyfin_repo = jellyfin_repo
        self._audiodb_service = audiodb_service
        self._audiodb_browse_queue = audiodb_browse_queue
        resolved_user_agent = user_agent
        if not resolved_user_agent or resolved_user_agent.lower().startswith("python-httpx"):
            resolved_user_agent = DEFAULT_EXTERNAL_USER_AGENT
        self._external_headers = {"User-Agent": resolved_user_agent}

    async def fetch_artist_image(
        self,
        artist_id: str,
        size: int | None,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
        is_disconnected: DisconnectCallable | None = None,
        include_wikidata: bool = True,
    ) -> tuple[bytes, str, str] | None:
        result = None
        had_transient_failure = False
        last_transient_error: Exception | None = None
        try:
            await check_disconnected(is_disconnected)
            result = await self._fetch_from_audiodb(artist_id, file_path, priority=priority)
        except TRANSIENT_FETCH_EXCEPTIONS as exc:
            had_transient_failure = True
            last_transient_error = exc
            result = None
        if result:
            return result
        try:
            await check_disconnected(is_disconnected)
            local_result, local_transient = await asyncio.wait_for(
                self._fetch_local_sources(artist_id, size, file_path, priority=priority),
                timeout=LOCAL_SOURCE_TIMEOUT_SECONDS,
            )
            if local_transient:
                had_transient_failure = True
            result = local_result
        except TimeoutError:
            had_transient_failure = True
            last_transient_error = TimeoutError(
                f"Timed out local source lookup for {artist_id}"
            )
        if result:
            return result
        # The Wikidata branch is the expensive one: MusicBrainz relations at a hard 1 req/s,
        # then Wikidata + Commons. The app's hot path defers it to the background
        # (include_wikidata=False) so a cold artist grid never serialises on MusicBrainz;
        # compat/prewarm callers run it inline.
        if include_wikidata:
            try:
                await check_disconnected(is_disconnected)
                result = await self._fetch_from_wikidata(artist_id, size, file_path, priority=priority)
            except TRANSIENT_FETCH_EXCEPTIONS as exc:
                had_transient_failure = True
                last_transient_error = exc
                result = None
            if result:
                return result
        if had_transient_failure:
            raise TransientImageFetchError(
                f"Transient failure while fetching artist image for {artist_id}"
            ) from last_transient_error
        return None

    async def _fetch_local_sources(
        self,
        artist_id: str,
        size: int | None,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[tuple[bytes, str, str] | None, bool]:
        had_transient_failure = False

        try:
            result = await self._fetch_from_library(artist_id, size, file_path, priority=priority)
        except TRANSIENT_FETCH_EXCEPTIONS:
            had_transient_failure = True
            result = None

        if result:
            return result, had_transient_failure

        try:
            result = await self._fetch_from_jellyfin(artist_id, file_path, priority=priority)
        except TRANSIENT_FETCH_EXCEPTIONS:
            had_transient_failure = True
            result = None

        return result, had_transient_failure

    async def _fetch_from_audiodb(
        self,
        artist_id: str,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[bytes, str, str] | None:
        if self._audiodb_service is None:
            return None
        try:
            # cache-only on the hot path: a live AudioDB lookup is throttled to the
            # shared free key and would serialise the grid. warm misses in the
            # background; fall through to local/Wikidata now.
            images = await self._audiodb_service.get_cached_artist_images(artist_id)
            if images is None:
                if self._audiodb_browse_queue is not None:
                    await self._audiodb_browse_queue.enqueue("artist", artist_id)
                return None
            if images.is_negative or not images.thumb_url:
                return None
            if not validate_audiodb_image_url(images.thumb_url):
                logger.warning("[IMG:AudioDB] Rejected unsafe URL for artist %s", artist_id[:8])
                return None
            response = await self._http_get(
                images.thumb_url,
                priority,
                source="audiodb",
                headers=self._external_headers,
            )
            if response.status_code != 200:
                return None
            content_type = response.headers.get("content-type", "")
            if not _is_valid_image_content_type(content_type):
                logger.warning(f"[IMG:AudioDB] Non-image content-type ({content_type}) for {artist_id[:8]}")
                return None
            content = response.content
            task = asyncio.create_task(
                self._write_disk_cache(file_path, content, content_type, {"source": "audiodb"})
            )
            task.add_done_callback(_log_task_error)
            return (content, content_type, "audiodb")
        except TRANSIENT_FETCH_EXCEPTIONS:
            raise
        except Exception:  # noqa: BLE001
            return None

    async def _fetch_from_library(
        self,
        artist_id: str,
        size: int | None,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[bytes, str, str] | None:
        if not self._library_repo:
            return None
        if not self._library_repo.is_configured():
            return None
        try:
            image_url = await self._library_repo.get_artist_image_url(artist_id, size=size or 250)
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
                logger.warning(f"[IMG:Lidarr] Non-image content-type ({content_type}) for {artist_id[:8]}")
                return None
            content = response.content
            task = asyncio.create_task(self._write_disk_cache(file_path, content, content_type, {"source": "library"}))
            task.add_done_callback(_log_task_error)
            return (content, content_type, "library")
        except TRANSIENT_FETCH_EXCEPTIONS:
            raise
        except Exception:  # noqa: BLE001
            return None

    async def _fetch_from_jellyfin(
        self,
        artist_id: str,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[bytes, str, str] | None:
        if not self._jellyfin_repo or not self._jellyfin_repo.is_configured():
            return None
        try:
            artist = await self._jellyfin_repo.get_artist_by_mbid(artist_id)
            if not artist:
                return None
            image_url = self._jellyfin_repo.get_image_url(artist.id, artist.image_tag)
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
                logger.warning(f"[IMG:Jellyfin] Non-image content-type ({content_type}) for {artist_id[:8]}")
                return None
            content = response.content
            task = asyncio.create_task(
                self._write_disk_cache(file_path, content, content_type, {"source": "jellyfin"})
            )
            task.add_done_callback(_log_task_error)
            return (content, content_type, "jellyfin")
        except TRANSIENT_FETCH_EXCEPTIONS:
            raise
        except Exception:  # noqa: BLE001
            return None

    async def _fetch_from_wikidata(
        self,
        artist_id: str,
        size: int | None,
        file_path: Path,
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
    ) -> tuple[bytes, str, str] | None:
        cache_key = f"{ARTIST_WIKIDATA_PREFIX}{artist_id}"
        wikidata_url = await self._cache.get(cache_key)
        if wikidata_url is None:
            wikidata_url = await self._lookup_wikidata_url(artist_id)
            if wikidata_url:
                await self._cache.set(cache_key, wikidata_url, ttl_seconds=86400)
        if not wikidata_url:
            return None
        try:
            match = re.search(r'/(?:wiki|entity)/(Q\d+)', wikidata_url)
            wikidata_id = match.group(1) if match else None
            if not wikidata_id:
                return None
            api_url = (
                f"https://www.wikidata.org/w/api.php"
                f"?action=wbgetclaims&entity={wikidata_id}&property=P18&format=json"
            )
            response = await self._http_get(
                api_url,
                priority,
                source="wikidata",
                headers=self._external_headers,
            )
            if response.status_code != 200:
                return None
            data = _decode_json_response(response, _WikidataClaimsResponse)
            image_claims = data.claims.get("P18", [])
            if not image_claims:
                return None
            first_claim = image_claims[0]
            filename = (
                first_claim.mainsnak.datavalue.value
                if first_claim.mainsnak and first_claim.mainsnak.datavalue
                else None
            )
            if not filename:
                return None
            commons_api = (
                f"https://commons.wikimedia.org/w/api.php"
                f"?action=query&titles=File:{quote(filename)}"
                f"&prop=imageinfo&iiprop=url&format=json"
            )
            if size:
                commons_api += f"&iiurlwidth={size}"
            commons_response = await self._http_get(
                commons_api,
                priority,
                source="wikimedia",
                headers=self._external_headers,
            )
            if commons_response.status_code != 200:
                return None
            commons_data = _decode_json_response(commons_response, _CommonsQueryResponse)
            pages = commons_data.query.pages if commons_data.query else {}
            image_url = None
            for page in pages.values():
                imageinfo = page.imageinfo
                if imageinfo:
                    if size and imageinfo[0].thumburl:
                        image_url = imageinfo[0].thumburl
                    else:
                        image_url = imageinfo[0].url
                    break
            if not image_url:
                return None
            response = await self._http_get(
                image_url,
                priority,
                source="wikimedia",
                headers=self._external_headers,
            )
            if response.status_code == 200:
                content_type = response.headers.get("content-type", "")
                if not _is_valid_image_content_type(content_type):
                    logger.warning(f"[IMG:Wikidata] Non-image content-type ({content_type})")
                    return None
                content = response.content
                task = asyncio.create_task(
                    self._write_disk_cache(
                        file_path,
                        content,
                        content_type,
                        {"wikidata_id": wikidata_id, "source": "wikidata"},
                    )
                )
                task.add_done_callback(_log_task_error)
                return (content, content_type, "wikidata")
        except TRANSIENT_FETCH_EXCEPTIONS:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Error fetching artist image for {artist_id}: {e}")
        return None

    async def _lookup_wikidata_url(self, artist_id: str) -> str | None:
        if not self._mb_repo:
            return None
        try:
            artist_data = await self._mb_repo.get_artist_relations(artist_id)
            if not artist_data:
                return None
            url_relations = artist_data.get("relations", [])
            if url_relations:
                for url_rel in url_relations:
                    if isinstance(url_rel, dict):
                        typ = url_rel.get("type") or url_rel.get("link_type")
                        url_obj = url_rel.get("url", {})
                        target = url_obj.get("resource", "") if isinstance(url_obj, dict) else ""
                        if typ == "wikidata" and target:
                            return target
            external_links = artist_data.get("external_links") or artist_data.get("external_links_list")
            if external_links:
                for ext in external_links:
                    try:
                        ext_type = getattr(ext, "type", None) if not isinstance(ext, dict) else ext.get("type")
                        ext_url = getattr(ext, "url", None) if not isinstance(ext, dict) else ext.get("url")
                    except (AttributeError, TypeError):
                        ext_type = None
                        ext_url = None
                    if ext_type == "wikidata" and ext_url:
                        return ext_url
            return None
        except TRANSIENT_FETCH_EXCEPTIONS:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"[IMG:Wikidata] Failed to fetch artist metadata for {artist_id}: {e}")
            return None
