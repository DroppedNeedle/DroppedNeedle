import asyncio
import hashlib
import logging
from collections import OrderedDict
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
from pathlib import Path
from typing import Optional, TYPE_CHECKING
from urllib.parse import urlparse

import aiofiles
import httpx
import msgspec

from core.exceptions import ExternalServiceError, RateLimitedError, ClientDisconnectedError
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.cache_keys import ARTIST_WIKIDATA_PREFIX
from infrastructure.resilience.retry import with_retry, CircuitBreaker, CircuitOpenError
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from infrastructure.validators import validate_mbid
from infrastructure.audio.tagger import AudioTagger
from infrastructure.queue.priority_queue import RequestPriority, get_priority_queue
from infrastructure.http.deduplication import RequestDeduplicator
from infrastructure.http.disconnect import DisconnectCallable
from repositories.coverart_artist import ArtistImageFetcher, TransientImageFetchError
from repositories.coverart_album import AlbumCoverFetcher
from repositories.coverart_disk_cache import CoverDiskCache
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult

if TYPE_CHECKING:
    from repositories.musicbrainz_repository import MusicBrainzRepository
    from repositories.protocols.library import LibraryRepositoryProtocol
    from repositories.jellyfin_repository import JellyfinRepository
    from services.audiodb_image_service import AudioDBImageService
    from services.audiodb_browse_queue import AudioDBBrowseQueue
    from infrastructure.persistence.library_db import LibraryDB

logger = logging.getLogger(__name__)

_SOURCE = "coverart"


def _record_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx is not None:
        ctx.record(IntegrationResult.error(source=_SOURCE, msg=msg))


def _sniff_image_content_type(data: bytes) -> Optional[str]:
    """Raster image MIME from magic bytes, or ``None`` for anything we won't serve.

    Embedded cover art carries a self-declared MIME that is often wrong, and a file
    could embed an SVG or junk blob. We sniff the bytes and only accept raster
    formats, so the last-resort embedded cover is never served mislabeled."""
    if len(data) < 12:
        return None
    if data[:3] == b"\xff\xd8\xff":
        return "image/jpeg"
    if data[:8] == b"\x89PNG\r\n\x1a\n":
        return "image/png"
    if data[:6] in (b"GIF87a", b"GIF89a"):
        return "image/gif"
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "image/webp"
    return None

COVER_ART_ARCHIVE_BASE = "https://coverartarchive.org"
COVER_NEGATIVE_TTL_SECONDS = 4 * 3600
COVER_MEMORY_MAX_ENTRIES = 128
COVER_MEMORY_MAX_BYTES = 16 * 1024 * 1024

def _default_cache_dir() -> Path:
      from core.config import get_settings
      return get_settings().cache_dir / "covers"

_coverart_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="coverart"
)

_library_cover_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="coverart_library",
)

_jellyfin_cover_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="coverart_jellyfin",
)

_wikidata_cover_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="coverart_wikidata",
)

_wikimedia_cover_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="coverart_wikimedia",
)

_generic_cover_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="coverart_generic",
)

# CAA imposes no rate limit and serves bytes via the Internet Archive CDN, so the
# IMAGE_FETCH semaphore (10) + circuit breaker + 429/Retry-After are the real
# governors. A generous bucket stays polite while removing the head-of-line stall
# that made cold cover grids load ~1 image/second.
_coverart_rate_limiter = TokenBucketRateLimiter(rate=10.0, capacity=20)

_deduplicator = RequestDeduplicator()


class _CoverMemoryEntry(msgspec.Struct):
    content: bytes
    content_type: str
    source: str
    content_sha1: str
    size_bytes: int


class _CoverMemoryLRU:
    def __init__(self, max_entries: int, max_bytes: int):
        self._max_entries = max(1, max_entries)
        self._max_bytes = max(1, max_bytes)
        self._entries: OrderedDict[str, _CoverMemoryEntry] = OrderedDict()
        self._total_bytes = 0
        self._lock = asyncio.Lock()

    async def get(self, key: str) -> Optional[_CoverMemoryEntry]:
        async with self._lock:
            entry = self._entries.get(key)
            if entry is None:
                return None
            self._entries.move_to_end(key)
            return entry

    async def get_hash(self, key: str) -> Optional[str]:
        entry = await self.get(key)
        if entry is None:
            return None
        return entry.content_sha1

    async def set(self, key: str, content: bytes, content_type: str, source: str) -> None:
        content_size = len(content)
        if content_size <= 0:
            return

        async with self._lock:
            existing = self._entries.pop(key, None)
            if existing is not None:
                self._total_bytes -= existing.size_bytes

            entry = _CoverMemoryEntry(
                content=content,
                content_type=content_type,
                source=source,
                content_sha1=hashlib.sha1(content).hexdigest(),
                size_bytes=content_size,
            )
            self._entries[key] = entry
            self._entries.move_to_end(key)
            self._total_bytes += content_size

            while self._entries and (
                len(self._entries) > self._max_entries or self._total_bytes > self._max_bytes
            ):
                _, evicted = self._entries.popitem(last=False)
                self._total_bytes -= evicted.size_bytes

    async def evict(self, key: str) -> None:
        async with self._lock:
            entry = self._entries.pop(key, None)
            if entry is not None:
                self._total_bytes -= entry.size_bytes


def _log_task_error(task: asyncio.Task) -> None:
    if not task.cancelled() and task.exception():
        logger.error(f"Background task failed: {task.exception()}")



class CoverArtRepository:
    def __init__(
        self,
        http_client: httpx.AsyncClient,
        cache: CacheInterface,
        mb_repo: Optional['MusicBrainzRepository'] = None,
        library_repo: Optional['LibraryRepositoryProtocol'] = None,
        jellyfin_repo: Optional['JellyfinRepository'] = None,
        audiodb_service: Optional['AudioDBImageService'] = None,
        audiodb_browse_queue: Optional['AudioDBBrowseQueue'] = None,
        cache_dir: Path = _default_cache_dir(),
        cover_cache_max_size_mb: Optional[int] = None,
        cover_memory_cache_max_entries: int = COVER_MEMORY_MAX_ENTRIES,
        cover_memory_cache_max_bytes: int = COVER_MEMORY_MAX_BYTES,
        cover_non_monitored_ttl_seconds: int = 604800,  # 7 days; non-monitored covers change rarely
        library_db: Optional['LibraryDB'] = None,
    ):
        self._client = http_client
        self._cache = cache
        self._mb_repo = mb_repo
        self._library_repo = library_repo
        self._jellyfin_repo = jellyfin_repo
        self._library_db = library_db
        self._tagger = AudioTagger()
        self.cache_dir = cache_dir
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        self._disk_cache = CoverDiskCache(
            cache_dir,
            max_size_mb=cover_cache_max_size_mb,
            non_monitored_ttl_seconds=cover_non_monitored_ttl_seconds,
        )
        self._cover_memory_cache = _CoverMemoryLRU(
            max_entries=cover_memory_cache_max_entries,
            max_bytes=cover_memory_cache_max_bytes,
        )
        self._artist_fetcher = ArtistImageFetcher(
            http_get_fn=self._http_get,
            write_cache_fn=self._disk_cache.write,
            cache=cache,
            mb_repo=mb_repo,
            library_repo=library_repo,
            jellyfin_repo=jellyfin_repo,
            audiodb_service=audiodb_service,
            audiodb_browse_queue=audiodb_browse_queue,
            user_agent=self._client.headers.get("User-Agent"),
        )
        self._album_fetcher = AlbumCoverFetcher(
            http_get_fn=self._http_get,
            write_cache_fn=self._disk_cache.write,
            library_repo=library_repo,
            mb_repo=mb_repo,
            jellyfin_repo=jellyfin_repo,
            audiodb_service=audiodb_service,
            audiodb_browse_queue=audiodb_browse_queue,
        )
        # Release groups / artists whose expensive fallback (CAA best-release / Wikidata) is
        # being resolved in the background (hot-path defer), so concurrent misses don't each
        # spawn a resolver.
        self._deferred_rg_inflight: set[str] = set()
        self._deferred_artist_inflight: set[str] = set()

        try:
            task = asyncio.create_task(self._disk_cache.enforce_size_limit(force=True))
            task.add_done_callback(_log_task_error)
        except RuntimeError:
            pass

    @property
    def disk_cache(self) -> CoverDiskCache:
        return self._disk_cache

    async def delete_covers_for_album(self, album_mbid: str) -> int:
        identifiers = [(f"rg_{album_mbid}", suffix) for suffix in ("500", "250", "1200", "orig")]
        count = await self._disk_cache.delete_by_identifiers(identifiers)
        for identifier, suffix in identifiers:
            await self._cover_memory_cache.evict(f"{identifier}:{suffix}")
        return count

    async def delete_covers_for_artist(self, artist_mbid: str) -> int:
        identifiers = [(f"artist_{artist_mbid}_{size}", "img") for size in ("250", "500")]
        identifiers.append((f"artist_{artist_mbid}", "img"))
        count = await self._disk_cache.delete_by_identifiers(identifiers)
        for identifier, suffix in identifiers:
            await self._cover_memory_cache.evict(f"{identifier}:{suffix}")
        return count

    @staticmethod
    def _memory_cache_key(identifier: str, suffix: str) -> str:
        return f"{identifier}:{suffix}"

    @staticmethod
    def _is_successful_image_payload(content: bytes, content_type: str) -> bool:
        return bool(content) and content_type.lower().startswith("image/")

    async def _memory_get(
        self,
        identifier: str,
        suffix: str,
    ) -> Optional[tuple[bytes, str, str]]:
        entry = await self._cover_memory_cache.get(self._memory_cache_key(identifier, suffix))
        if entry is None:
            return None
        return entry.content, entry.content_type, entry.source

    async def _memory_get_hash(self, identifier: str, suffix: str) -> Optional[str]:
        return await self._cover_memory_cache.get_hash(self._memory_cache_key(identifier, suffix))

    async def _memory_set_from_result(
        self,
        identifier: str,
        suffix: str,
        result: Optional[tuple[bytes, str, str]],
    ) -> None:
        if result is None:
            return

        content, content_type, source = result
        if not self._is_successful_image_payload(content, content_type):
            return

        await self._cover_memory_cache.set(
            self._memory_cache_key(identifier, suffix),
            content,
            content_type,
            source,
        )
    
    @staticmethod
    def _parse_retry_after_seconds(retry_after: Optional[str]) -> Optional[float]:
        if not retry_after:
            return None

        try:
            seconds = float(retry_after)
            return seconds if seconds > 0 else None
        except ValueError:
            pass

        try:
            parsed_dt = parsedate_to_datetime(retry_after)
            if parsed_dt.tzinfo is None:
                parsed_dt = parsed_dt.replace(tzinfo=timezone.utc)
            seconds = (parsed_dt - datetime.now(timezone.utc)).total_seconds()
            return seconds if seconds > 0 else None
        except (TypeError, ValueError):
            return None

    @staticmethod
    def _infer_source_from_url(url: str) -> str:
        netloc = urlparse(url).netloc.lower()
        if "coverartarchive.org" in netloc:
            return "coverart"
        if "wikidata.org" in netloc:
            return "wikidata"
        if "wikimedia.org" in netloc:
            return "wikimedia"
        return "generic"

    @staticmethod
    def _raise_retryable_status(response: httpx.Response, source: str, url: str) -> None:
        status_code = response.status_code

        if status_code == 429:
            retry_after = CoverArtRepository._parse_retry_after_seconds(
                response.headers.get("Retry-After")
            )
            raise RateLimitedError(
                f"{source} rate limited (429): {url}",
                retry_after_seconds=retry_after,
            )

        if 500 <= status_code <= 599:
            raise ExternalServiceError(f"{source} transient error ({status_code})", url)

    async def _perform_http_get(
        self,
        url: str,
        priority: RequestPriority,
        source: str,
        **kwargs,
    ) -> httpx.Response:
        priority_mgr = get_priority_queue()
        semaphore = await priority_mgr.acquire_slot(priority)
        async with semaphore:
            response = await self._client.get(url, **kwargs)
            self._raise_retryable_status(response, source, url)
            return response

    # Only ONE retry (max_attempts=2), not three: covers ride a short-budget client, and a
    # cover that fails twice isn't worth a third 6s attempt on the user's hot path - it degrades
    # to the placeholder and is warmed in the background. Three attempts x the old 10s client is
    # exactly how a hung archive.org fetch became a 25-30s request tail.
    @with_retry(
        max_attempts=2,
        circuit_breaker=_coverart_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError, RateLimitedError),
    )
    async def _http_get_coverart(self, url: str, priority: RequestPriority, **kwargs) -> httpx.Response:
        await _coverart_rate_limiter.acquire()
        return await self._perform_http_get(url, priority, "coverart", **kwargs)

    @with_retry(
        max_attempts=3,
        circuit_breaker=_library_cover_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
    )
    async def _http_get_library(self, url: str, priority: RequestPriority, **kwargs) -> httpx.Response:
        return await self._perform_http_get(url, priority, "library", **kwargs)

    @with_retry(
        max_attempts=3,
        circuit_breaker=_jellyfin_cover_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
    )
    async def _http_get_jellyfin(self, url: str, priority: RequestPriority, **kwargs) -> httpx.Response:
        return await self._perform_http_get(url, priority, "jellyfin", **kwargs)

    @with_retry(
        max_attempts=3,
        circuit_breaker=_wikidata_cover_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
    )
    async def _http_get_wikidata(self, url: str, priority: RequestPriority, **kwargs) -> httpx.Response:
        return await self._perform_http_get(url, priority, "wikidata", **kwargs)

    @with_retry(
        max_attempts=3,
        circuit_breaker=_wikimedia_cover_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
    )
    async def _http_get_wikimedia(self, url: str, priority: RequestPriority, **kwargs) -> httpx.Response:
        return await self._perform_http_get(url, priority, "wikimedia", **kwargs)

    @with_retry(
        max_attempts=3,
        circuit_breaker=_generic_cover_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
    )
    async def _http_get_generic(self, url: str, priority: RequestPriority, **kwargs) -> httpx.Response:
        return await self._perform_http_get(url, priority, "generic", **kwargs)

    async def _http_get(
        self,
        url: str,
        priority: RequestPriority,
        source: Optional[str] = None,
        **kwargs,
    ) -> httpx.Response:
        request_source = source or self._infer_source_from_url(url)
        if request_source == "coverart":
            return await self._http_get_coverart(url, priority, **kwargs)
        if request_source == "library":
            return await self._http_get_library(url, priority, **kwargs)
        if request_source == "jellyfin":
            return await self._http_get_jellyfin(url, priority, **kwargs)
        if request_source == "wikidata":
            return await self._http_get_wikidata(url, priority, **kwargs)
        if request_source == "wikimedia":
            return await self._http_get_wikimedia(url, priority, **kwargs)
        return await self._http_get_generic(url, priority, **kwargs)

    async def get_release_group_cover_etag(
        self,
        release_group_id: str,
        size: Optional[str] = "500",
    ) -> Optional[str]:
        try:
            release_group_id = validate_mbid(release_group_id, "release-group")
        except ValueError:
            return None

        identifier = f"rg_{release_group_id}"
        suffix = size or "orig"

        if content_hash := await self._memory_get_hash(identifier, suffix):
            return content_hash

        file_path = self._disk_cache.get_file_path(identifier, suffix)
        return await self._disk_cache.get_content_hash(file_path)

    async def get_release_cover_etag(
        self,
        release_id: str,
        size: Optional[str] = "500",
    ) -> Optional[str]:
        try:
            release_id = validate_mbid(release_id, "release")
        except ValueError:
            return None

        identifier = f"rel_{release_id}"
        suffix = size or "orig"

        if content_hash := await self._memory_get_hash(identifier, suffix):
            return content_hash

        file_path = self._disk_cache.get_file_path(identifier, suffix)
        return await self._disk_cache.get_content_hash(file_path)

    async def get_artist_image_etag(
        self,
        artist_id: str,
        size: Optional[int] = None,
    ) -> Optional[str]:
        try:
            artist_id = validate_mbid(artist_id, "artist")
        except ValueError:
            return None

        size_suffix = f"_{size}" if size else ""
        identifier = f"artist_{artist_id}{size_suffix}"

        if content_hash := await self._memory_get_hash(identifier, "img"):
            return content_hash

        file_path = self._disk_cache.get_file_path(identifier, "img")

        content_hash = await self._disk_cache.get_content_hash(file_path)
        if content_hash:
            return content_hash

        if size and size != 250:
            fallback_identifier = f"artist_{artist_id}_250"
            if content_hash := await self._memory_get_hash(fallback_identifier, "img"):
                return content_hash
            fallback_path = self._disk_cache.get_file_path(fallback_identifier, "img")
            return await self._disk_cache.get_content_hash(fallback_path)

        return None
    
    async def get_artist_image(self, artist_id: str, size: Optional[int] = None, priority: RequestPriority = RequestPriority.IMAGE_FETCH, is_disconnected: DisconnectCallable | None = None, defer_wikidata: bool = False) -> Optional[tuple[bytes, str, str]]:
        try:
            artist_id = validate_mbid(artist_id, "artist")
        except ValueError as e:
            logger.warning(f"Invalid artist MBID: {e}")
            return None

        size_suffix = f"_{size}" if size else ""
        identifier = f"artist_{artist_id}{size_suffix}"
        file_path = self._disk_cache.get_file_path(identifier, "img")

        if cached_memory := await self._memory_get(identifier, "img"):
            return cached_memory

        if cached := await self._disk_cache.read(file_path, ["source", "wikidata_id"]):
            source = "wikidata"
            if cached[2] and isinstance(cached[2], dict):
                source = cached[2].get("source") or source
            result = (cached[0], cached[1], source)
            await self._memory_set_from_result(identifier, "img", result)
            return result

        if size and size != 250:
            fallback_identifier = f"artist_{artist_id}_250"
            if cached_memory := await self._memory_get(fallback_identifier, "img"):
                return cached_memory

            fallback_path = self._disk_cache.get_file_path(fallback_identifier, "img")
            if cached := await self._disk_cache.read(fallback_path, ["source", "wikidata_id"]):
                source = "wikidata"
                if cached[2] and isinstance(cached[2], dict):
                    source = cached[2].get("source") or source
                result = (cached[0], cached[1], source)
                await self._memory_set_from_result(fallback_identifier, "img", result)
                return result

        if await self._disk_cache.is_negative(file_path):
            return None

        # The dedupe key MUST encode the defer mode. The fast (deferred) hot path and the full
        # background resolve run DIFFERENT fetches; if they shared a key, a hot request arriving
        # while a slow full Wikidata/MusicBrainz resolve is in flight would coalesce onto it as a
        # follower and block for the whole resolve (seen live: a 55s cover request) - exactly the
        # stall deferring exists to avoid. Separate keys keep hot requests fast (they coalesce
        # only with each other) while background/compat full fetches coalesce among themselves.
        dedupe_key = f"artist:img:{artist_id}:{size}:{'deferred' if defer_wikidata else 'full'}"
        try:
            result = await _deduplicator.dedupe(
                dedupe_key,
                lambda: self._artist_fetcher.fetch_artist_image(artist_id, size, file_path, priority=priority, is_disconnected=is_disconnected, include_wikidata=not defer_wikidata)
            )
        except ClientDisconnectedError:
            raise
        except (TransientImageFetchError, CircuitOpenError, httpx.HTTPError, ExternalServiceError, RateLimitedError) as e:
            # Transient failure: fail soft WITHOUT caching a negative (the artist may well have
            # an image; it was just a blip) and without deferring - the next request retries.
            _record_degradation(f"Artist image fetch failed for {artist_id[:8]}: {e}")
            return None

        if result is None and defer_wikidata:
            # Hot path: AudioDB cache + local missed. Resolve the Wikidata chain (MusicBrainz
            # 1/s) in the background so the artist grid never serialises on it - placeholder
            # now, fills in on the next visit. No negative here or the resolve is short-circuited.
            self._spawn_deferred_artist_resolve(artist_id, size)
            return None
        if result is None:
            await self._disk_cache.write_negative(file_path, ttl_seconds=COVER_NEGATIVE_TTL_SECONDS)
        else:
            await self._memory_set_from_result(identifier, "img", result)
        return result

    def _spawn_deferred_artist_resolve(self, artist_id: str, size: Optional[int]) -> None:
        """Background full resolve of an artist image (the Wikidata/MusicBrainz chain the hot
        path skipped), deduped so concurrent misses spawn only one resolver. BACKGROUND_SYNC
        yields to live users; the result is banked to the disk cache for the next request."""
        key = f"{artist_id}:{size}"
        if key in self._deferred_artist_inflight:
            return
        self._deferred_artist_inflight.add(key)

        async def _resolve() -> None:
            try:
                await self.get_artist_image(
                    artist_id, size, priority=RequestPriority.BACKGROUND_SYNC,
                )
            finally:
                self._deferred_artist_inflight.discard(key)

        task = asyncio.create_task(_resolve())
        task.add_done_callback(_log_task_error)

    async def get_release_group_cover(
        self,
        release_group_id: str,
        size: Optional[str] = "500",
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
        is_disconnected: DisconnectCallable | None = None,
        defer_best_release: bool = False,
    ) -> Optional[tuple[bytes, str, str]]:
        try:
            release_group_id = validate_mbid(release_group_id, "release-group")
        except ValueError as e:
            logger.warning(f"Invalid release-group MBID: {e}")
            return None

        identifier = f"rg_{release_group_id}"
        suffix = size or 'orig'
        file_path = self._disk_cache.get_file_path(identifier, suffix)

        if cached_memory := await self._memory_get(identifier, suffix):
            if cached_memory[2] in {"audiodb", "legacy-cache"}:
                return cached_memory
            preferred = await self._prefer_audiodb_album_cover(
                release_group_id,
                identifier,
                suffix,
                file_path,
                cached_memory,
                priority,
            )
            return preferred

        if cached := await self._disk_cache.read(file_path, ["source"]):
            # Pre-metadata cache files have unknown provenance. Preserve them rather
            # than assuming they came from CAA and hiding restored/local artwork.
            source = "legacy-cache"
            if cached[2] and isinstance(cached[2], dict):
                source = cached[2].get("source") or source
            result = (cached[0], cached[1], source)
            await self._memory_set_from_result(identifier, suffix, result)
            if source in {"audiodb", "legacy-cache"}:
                return result
            return await self._prefer_audiodb_album_cover(
                release_group_id,
                identifier,
                suffix,
                file_path,
                result,
                priority,
            )

        if await self._disk_cache.is_negative(file_path):
            return await self._prefer_audiodb_album_cover(
                release_group_id,
                identifier,
                suffix,
                file_path,
                None,
                priority,
            )

        # Key encodes the defer mode so a fast (deferred) hot request never coalesces onto an
        # in-flight slow full best-release resolve and blocks on it. See get_artist_image.
        dedupe_key = f"cover:rg:{release_group_id}:{size}:{'deferred' if defer_best_release else 'full'}"
        result = await _deduplicator.dedupe(
            dedupe_key,
            lambda: self._album_fetcher.fetch_release_group_cover(
                release_group_id, size, file_path, priority=priority,
                is_disconnected=is_disconnected,
                include_best_release=not defer_best_release,
            )
        )
        if result is None and self._album_fetcher.is_audiodb_album_warming(
            release_group_id
        ):
            return None
        if result is None and defer_best_release:
            # Hot path: the cheap sources missed. Finish the expensive best-release fallback
            # (+ embedded art + negative marker) in the background so this request returns the
            # placeholder now and the cover fills in on the next visit - never write a negative
            # here or the background resolve would be short-circuited by it.
            self._spawn_deferred_rg_resolve(release_group_id, size)
            return None
        if result is None:
            # Last resort: every external source missed - serve art embedded in a
            # local library file for this album, if any has some. Beats the
            # turntable placeholder for albums the internet has no cover for.
            result = await self._embedded_album_cover(release_group_id, file_path)
        if result is None:
            await self._disk_cache.write_negative(file_path, ttl_seconds=COVER_NEGATIVE_TTL_SECONDS)
        else:
            await self._memory_set_from_result(identifier, suffix, result)
        return result

    async def _prefer_audiodb_album_cover(
        self,
        release_group_id: str,
        identifier: str,
        suffix: str,
        file_path: Path,
        fallback: Optional[tuple[bytes, str, str]],
        priority: RequestPriority,
    ) -> Optional[tuple[bytes, str, str]]:
        preferred = await self._album_fetcher.fetch_cached_audiodb_cover(
            release_group_id, file_path, priority=priority
        )
        if preferred is not None:
            await self._memory_set_from_result(identifier, suffix, preferred)
            return preferred
        return fallback

    def _spawn_deferred_rg_resolve(self, release_group_id: str, size: Optional[str]) -> None:
        """Background full resolve of a release-group cover (the CAA best-release fallback the
        hot path skipped), deduped so concurrent misses spawn only one resolver. Runs at
        BACKGROUND_SYNC so it yields to live users and banks the result to the disk cache."""
        key = f"{release_group_id}:{size or 'orig'}"
        if key in self._deferred_rg_inflight:
            return
        self._deferred_rg_inflight.add(key)

        async def _resolve() -> None:
            try:
                await self.get_release_group_cover(
                    release_group_id, size, priority=RequestPriority.BACKGROUND_SYNC,
                )
            finally:
                self._deferred_rg_inflight.discard(key)

        task = asyncio.create_task(_resolve())
        task.add_done_callback(_log_task_error)

    def is_rg_cover_warming(self, release_group_id: str, size: Optional[str]) -> bool:
        """True while a deferred best-release resolve is in flight for this cover, i.e. the
        placeholder the route would serve is temporary and the real cover will land shortly.
        Lets the route signal 'warming, poll me' (202) vs 'no art' (placeholder)."""
        try:
            release_group_id = validate_mbid(release_group_id, "release-group")
        except ValueError:
            return False
        return (
            self._album_fetcher.is_audiodb_album_warming(release_group_id)
            or f"{release_group_id}:{size or 'orig'}" in self._deferred_rg_inflight
        )

    def is_artist_cover_warming(self, artist_id: str, size: Optional[int]) -> bool:
        """True while a deferred Wikidata resolve is in flight for this artist image."""
        try:
            artist_id = validate_mbid(artist_id, "artist")
        except ValueError:
            return False
        return f"{artist_id}:{size}" in self._deferred_artist_inflight

    async def _embedded_album_cover(
        self,
        release_group_id: str,
        file_path: Path,
    ) -> Optional[tuple[bytes, str, str]]:
        """Front cover embedded in a local file for this release group, cached to
        disk so subsequent loads hit the normal cache path. ``None`` when the native
        library isn't wired, the album has no local files, or none carry raster art."""
        if self._library_db is None:
            return None
        try:
            rows = await self._library_db.get_library_files_for_album(release_group_id)
        except Exception as e:  # noqa: BLE001
            logger.debug(f"Embedded-cover lookup failed for {release_group_id[:8]}: {e}")
            return None

        paths = [row["file_path"] for row in rows if row.get("file_path")]
        if not paths:
            return None

        extracted = await asyncio.to_thread(self._extract_first_embedded_cover, paths)
        if extracted is None:
            return None

        content, content_type = extracted
        await self._disk_cache.write(file_path, content, content_type, {"source": "embedded"})
        return content, content_type, "embedded"

    def _extract_first_embedded_cover(self, paths: list[str]) -> Optional[tuple[bytes, str]]:
        """Synchronous: the first local file holding raster cover art. Runs in a
        worker thread - mutagen reads are blocking."""
        for raw_path in paths:
            path = Path(raw_path)
            if not path.is_file():
                continue
            try:
                data = self._tagger.read_cover_art(path)
            except Exception as e:  # noqa: BLE001
                logger.debug(f"Embedded-cover read failed for {path}: {e}")
                continue
            if not data:
                continue
            content_type = _sniff_image_content_type(data)
            if content_type is None:
                continue
            return data, content_type
        return None

    async def get_release_cover(
        self,
        release_id: str,
        size: Optional[str] = "500",
        priority: RequestPriority = RequestPriority.IMAGE_FETCH,
        is_disconnected: DisconnectCallable | None = None,
    ) -> Optional[tuple[bytes, str, str]]:
        try:
            release_id = validate_mbid(release_id, "release")
        except ValueError as e:
            logger.warning(f"Invalid release MBID: {e}")
            return None

        identifier = f"rel_{release_id}"
        suffix = size or 'orig'
        file_path = self._disk_cache.get_file_path(identifier, suffix)

        if cached_memory := await self._memory_get(identifier, suffix):
            if cached_memory[2] in {"audiodb", "legacy-cache"}:
                return cached_memory
            return await self._prefer_release_audiodb_cover(
                release_id,
                identifier,
                suffix,
                file_path,
                cached_memory,
                priority,
                is_disconnected,
            )

        if cached := await self._disk_cache.read(file_path, ["source"]):
            source = "legacy-cache"
            if cached[2] and isinstance(cached[2], dict):
                source = cached[2].get("source") or source
            result = (cached[0], cached[1], source)
            await self._memory_set_from_result(identifier, suffix, result)
            if source in {"audiodb", "legacy-cache"}:
                return result
            return await self._prefer_release_audiodb_cover(
                release_id,
                identifier,
                suffix,
                file_path,
                result,
                priority,
                is_disconnected,
            )

        if await self._disk_cache.is_negative(file_path):
            return await self._prefer_release_audiodb_cover(
                release_id,
                identifier,
                suffix,
                file_path,
                None,
                priority,
                is_disconnected,
            )

        dedupe_key = f"cover:rel:{release_id}:{size}"
        result = await _deduplicator.dedupe(
            dedupe_key,
            lambda: self._album_fetcher.fetch_release_cover(release_id, size, file_path, priority=priority, is_disconnected=is_disconnected)
        )
        if result is None and self._album_fetcher.is_audiodb_release_warming(
            release_id
        ):
            return None
        if result is None:
            await self._disk_cache.write_negative(file_path, ttl_seconds=COVER_NEGATIVE_TTL_SECONDS)
        else:
            await self._memory_set_from_result(identifier, suffix, result)
        return result

    async def _prefer_release_audiodb_cover(
        self,
        release_id: str,
        identifier: str,
        suffix: str,
        file_path: Path,
        fallback: Optional[tuple[bytes, str, str]],
        priority: RequestPriority,
        is_disconnected: DisconnectCallable | None,
    ) -> Optional[tuple[bytes, str, str]]:
        preferred = await self._album_fetcher.fetch_release_audiodb_cover(
            release_id,
            file_path,
            priority=priority,
            is_disconnected=is_disconnected,
        )
        if preferred is not None:
            await self._memory_set_from_result(identifier, suffix, preferred)
            return preferred
        return fallback

    def is_release_cover_warming(self, release_id: str) -> bool:
        try:
            release_id = validate_mbid(release_id, "release")
        except ValueError:
            return False
        return self._album_fetcher.is_audiodb_release_warming(release_id)
    
    async def batch_prefetch_covers(
        self,
        album_ids: list[str],
        size: str = "250",
        max_concurrent: int = 5
    ) -> None:
        if not album_ids:
            return
        
        from infrastructure.validators import is_valid_mbid
        valid_album_ids = [aid for aid in album_ids if is_valid_mbid(aid)]
        invalid_count = len(album_ids) - len(valid_album_ids)
        
        if not valid_album_ids:
            logger.warning("No valid MBIDs in batch prefetch request")
            return
        
        if invalid_count > 0:
            invalid_rate = (invalid_count / len(album_ids)) * 100
            logger.warning(f"Filtered out {invalid_count} invalid MBIDs from batch prefetch ({invalid_rate:.1f}%)")
            
            if invalid_rate > 10.0:
                logger.error(
                    f"HIGH INVALID MBID RATE: {invalid_count}/{len(album_ids)} "
                    f"({invalid_rate:.1f}%) - This indicates a potential upstream bug!"
                )
        
        semaphore = asyncio.Semaphore(max_concurrent)
        
        async def fetch_with_limit(album_id: str):
            async with semaphore:
                try:
                    await self.get_release_group_cover(album_id, size)
                except Exception:  # noqa: BLE001
                    pass
        
        await asyncio.gather(*[fetch_with_limit(aid) for aid in valid_album_ids], return_exceptions=True)
    
    async def promote_cover_to_persistent(self, identifier: str, identifier_type: str = "album") -> bool:
        return await self._disk_cache.promote_to_persistent(identifier, identifier_type)

    async def debug_artist_image(self, artist_id: str, debug_info: dict) -> dict:
        file_path_250 = self._disk_cache.get_file_path(f"artist_{artist_id}_250", "img")
        file_path_500 = self._disk_cache.get_file_path(f"artist_{artist_id}_500", "img")

        debug_info["disk_cache"]["exists_250"] = file_path_250.exists()
        debug_info["disk_cache"]["exists_500"] = file_path_500.exists()
        debug_info["disk_cache"]["negative_250"] = await self._disk_cache.is_negative(file_path_250)
        debug_info["disk_cache"]["negative_500"] = await self._disk_cache.is_negative(file_path_500)

        debug_info["circuit_breakers"] = {
            "coverart": _coverart_circuit_breaker.get_state(),
            "library": _library_cover_circuit_breaker.get_state(),
            "jellyfin": _jellyfin_cover_circuit_breaker.get_state(),
            "wikidata": _wikidata_cover_circuit_breaker.get_state(),
            "wikimedia": _wikimedia_cover_circuit_breaker.get_state(),
            "generic": _generic_cover_circuit_breaker.get_state(),
        }

        for size, file_path in [("250", file_path_250), ("500", file_path_500)]:
            meta_path = file_path.with_suffix('.meta.json')
            if meta_path.exists():
                try:
                    async with aiofiles.open(meta_path, 'r') as f:
                        debug_info["disk_cache"][f"meta_{size}"] = msgspec.json.decode(
                            (await f.read()).encode("utf-8"),
                            type=dict[str, object],
                        )
                except Exception as e:  # noqa: BLE001
                    debug_info["disk_cache"][f"meta_{size}"] = f"Error reading: {e}"

        if self._library_repo:
            debug_info["library"]["configured"] = True
            try:
                image_url = await self._library_repo.get_artist_image_url(artist_id)
                if image_url:
                    debug_info["library"]["has_image_url"] = True
                    debug_info["library"]["image_url"] = image_url
            except Exception as e:  # noqa: BLE001
                debug_info["library"]["error"] = str(e)

        cache_key = f"{ARTIST_WIKIDATA_PREFIX}{artist_id}"
        cached_wikidata = await self._cache.get(cache_key)
        if cached_wikidata is not None:
            debug_info["memory_cache"]["wikidata_url_cached"] = True
            debug_info["memory_cache"]["cached_value"] = cached_wikidata if cached_wikidata else "(negative cache)"

        if self._mb_repo and not cached_wikidata:
            try:
                artist_data = await self._mb_repo.get_artist_by_id(artist_id)
                if artist_data:
                    debug_info["musicbrainz"]["artist_found"] = True
                    debug_info["musicbrainz"]["artist_name"] = artist_data.get("name")
                    url_relations = artist_data.get("relations", [])
                    if url_relations:
                        for url_rel in url_relations:
                            if isinstance(url_rel, dict):
                                typ = url_rel.get("type") or url_rel.get("link_type")
                                url_obj = url_rel.get("url", {})
                                target = url_obj.get("resource", "") if isinstance(url_obj, dict) else ""
                                if typ == "wikidata" and target:
                                    debug_info["musicbrainz"]["has_wikidata_relation"] = True
                                    debug_info["musicbrainz"]["wikidata_url"] = target
                                    break
            except Exception as e:  # noqa: BLE001
                debug_info["musicbrainz"]["error"] = str(e)
        elif cached_wikidata:
            debug_info["musicbrainz"]["has_wikidata_relation"] = True
            debug_info["musicbrainz"]["wikidata_url"] = cached_wikidata

        return debug_info
