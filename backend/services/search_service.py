import asyncio
import logging
import re
import time
import unicodedata
import msgspec
from math import ceil
from typing import Optional, TYPE_CHECKING
from api.v1.schemas.search import (
    SearchResult,
    SearchRemoteStatus,
    SearchResponse,
    SuggestResult,
    SuggestResponse,
)
from repositories.protocols import (
    MusicBrainzRepositoryProtocol,
    LibraryRepositoryProtocol,
    CoverArtRepositoryProtocol,
)
from services.preferences_service import PreferencesService
from infrastructure.http.deduplication import deduplicate
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from repositories.musicbrainz_base import get_mb_source_generation

if TYPE_CHECKING:
    from services.audiodb_image_service import AudioDBImageService
    from services.audiodb_browse_queue import AudioDBBrowseQueue
    from services.native.library_ownership_service import LibraryOwnershipService

logger = logging.getLogger(__name__)

COVER_PREFETCH_LIMIT = 12
SEARCH_CACHE_TTL = 90
SEARCH_CACHE_MAX_SIZE = 200
SEARCH_STALE_CACHE_TTL = 6 * 60 * 60
SEARCH_STALE_CACHE_MAX_SIZE = 200
TOP_RESULT_SCORE_THRESHOLD = 90
FULL_SEARCH_TIMEOUT_SECONDS = 6.0
SUGGEST_TIMEOUT_SECONDS = 3.0


class SearchService:
    _search_cache: dict[str, tuple[float, SearchResponse]] = {}
    _stale_bucket_cache: dict[
        str,
        tuple[float, tuple[SearchResult, ...]],
    ] = {}

    @classmethod
    def clear_cached_results(cls) -> None:
        cls._search_cache.clear()
        cls._stale_bucket_cache.clear()

    @staticmethod
    def _source_changed_response(
        buckets: Optional[list[str]] = None,
    ) -> SearchResponse:
        names = (
            ("artists", "albums")
            if not buckets
            else tuple(name for name in ("artists", "albums") if name in buckets)
        )
        return SearchResponse(
            artists=[],
            albums=[],
            top_artist=None,
            top_album=None,
            bucket_status={name: "error" for name in names},
        )
    @staticmethod
    def _bucket_stale_cache_key(
        bucket: str,
        query: str,
        limit: int,
        offset: int,
        included_primary_types: set[str],
        included_secondary_types: set[str],
    ) -> str:
        primary = ",".join(sorted(included_primary_types))
        secondary = ",".join(sorted(included_secondary_types))
        return (
            f"g{get_mb_source_generation()}:{bucket}:{query.strip().casefold()}:{limit}:{offset}:"
            f"{primary}|{secondary}"
        )

    @classmethod
    def _get_stale_bucket(
        cls,
        cache_key: str,
        now: float,
    ) -> list[SearchResult] | None:
        cached = cls._stale_bucket_cache.get(cache_key)
        if cached is None:
            return None
        cached_at, results = cached
        if now - cached_at >= SEARCH_STALE_CACHE_TTL:
            del cls._stale_bucket_cache[cache_key]
            return None
        return [msgspec.structs.replace(item) for item in results]

    @classmethod
    def _store_stale_bucket(
        cls,
        cache_key: str,
        results: list[SearchResult],
        now: float,
    ) -> None:
        cls._stale_bucket_cache[cache_key] = (
            now,
            tuple(msgspec.structs.replace(item) for item in results),
        )
        expired = [
            key
            for key, (cached_at, _) in cls._stale_bucket_cache.items()
            if now - cached_at >= SEARCH_STALE_CACHE_TTL
        ]
        for key in expired:
            del cls._stale_bucket_cache[key]
        while len(cls._stale_bucket_cache) > SEARCH_STALE_CACHE_MAX_SIZE:
            oldest_key = min(
                cls._stale_bucket_cache,
                key=lambda key: cls._stale_bucket_cache[key][0],
            )
            del cls._stale_bucket_cache[oldest_key]

    def __init__(
        self,
        mb_repo: MusicBrainzRepositoryProtocol,
        library_repo: LibraryRepositoryProtocol,
        coverart_repo: CoverArtRepositoryProtocol,
        preferences_service: PreferencesService,
        audiodb_image_service: "AudioDBImageService | None" = None,
        audiodb_browse_queue: "AudioDBBrowseQueue | None" = None,
        ownership_service: "LibraryOwnershipService | None" = None,
    ):
        self._mb_repo = mb_repo
        self._library_repo = library_repo
        self._coverart_repo = coverart_repo
        self._preferences_service = preferences_service
        self._audiodb_image_service = audiodb_image_service
        self._audiodb_browse_queue = audiodb_browse_queue
        self._ownership = ownership_service

    async def _apply_album_ownership(
        self, albums: list[SearchResult], library_mbids: set[str] | None = None
    ) -> None:
        if self._ownership is None:
            library_mbids = library_mbids or set()
            for item in albums:
                item.in_library = (item.musicbrainz_id or "").lower() in library_mbids
                item.requested = False
            return
        from services.native.library_ownership_service import AlbumOwnershipCandidate

        projections = await self._ownership.project_albums(
            [
                AlbumOwnershipCandidate(
                    release_group_mbid=item.musicbrainz_id,
                    title=item.title,
                    album_artist=item.artist or "",
                    year=item.year,
                )
                for item in albums
            ]
        )
        for item, projection in zip(albums, projections):
            item.in_library = projection.owned
            item.requested = False

    async def _safe_gather(self, *tasks):
        results = await asyncio.gather(*tasks, return_exceptions=True)
        return [r if not isinstance(r, Exception) else None for r in results]

    @staticmethod
    def _record_musicbrainz_error(message: str) -> None:
        context = try_get_degradation_context()
        if context is not None:
            context.record(IntegrationResult.error(source="musicbrainz", msg=message))

    @staticmethod
    def _remote_status(has_results: bool) -> SearchRemoteStatus:
        context = try_get_degradation_context()
        if context is None or context.summary().get("musicbrainz") is None:
            return "ok"
        return "partial" if has_results else "error"

    @staticmethod
    def _normalize_tokens(text: str) -> set[str]:
        """Strip diacritics and non-alphanumeric chars, then tokenize."""
        nfkd = unicodedata.normalize("NFKD", text.lower())
        stripped = "".join(c for c in nfkd if not unicodedata.combining(c))
        cleaned = re.sub(r"[^a-z0-9\s]", "", stripped)
        return set(cleaned.split())

    @staticmethod
    def _tokens_match(query_tokens: set[str], title_tokens: set[str]) -> bool:
        """Check token overlap allowing prefix matching for partial queries."""
        min_prefix = 2
        if all(
            any(
                qt == tt or (len(qt) >= min_prefix and tt.startswith(qt))
                for tt in title_tokens
            )
            for qt in query_tokens
        ):
            return True
        if all(
            any(
                tt == qt or (len(tt) >= min_prefix and qt.startswith(tt))
                for qt in query_tokens
            )
            for tt in title_tokens
        ):
            return True
        return False

    @staticmethod
    def _detect_top_result(
        results: list[SearchResult], query: str
    ) -> SearchResult | None:
        if not results:
            return None
        best = results[0]
        if best.score < TOP_RESULT_SCORE_THRESHOLD:
            return None
        query_tokens = SearchService._normalize_tokens(query)
        title_tokens = SearchService._normalize_tokens(best.title)
        if not query_tokens or not title_tokens:
            return None
        if SearchService._tokens_match(query_tokens, title_tokens):
            return best
        return None

    async def _apply_audiodb_search_overlay(self, results: list[SearchResult]) -> None:
        if self._audiodb_image_service is None:
            return

        tasks = []
        task_indices = []
        for i, item in enumerate(results):
            if not item.musicbrainz_id:
                continue
            if item.type == "artist":
                tasks.append(
                    self._audiodb_image_service.get_cached_artist_images(
                        item.musicbrainz_id
                    )
                )
                task_indices.append(i)
            elif item.type == "album":
                tasks.append(
                    self._audiodb_image_service.get_cached_album_images(
                        item.musicbrainz_id
                    )
                )
                task_indices.append(i)

        if not tasks:
            return

        images_results = await asyncio.gather(*tasks, return_exceptions=True)

        for idx, images in zip(task_indices, images_results):
            item = results[idx]
            if isinstance(images, Exception):
                logger.warning(
                    "AudioDB search overlay failed for %s %s: %s",
                    item.type,
                    item.musicbrainz_id[:8],
                    images,
                )
                continue
            try:
                if item.type == "artist":
                    if images and not images.is_negative:
                        if not item.thumb_url and images.thumb_url:
                            item.thumb_url = images.thumb_url
                        if not item.fanart_url and images.fanart_url:
                            item.fanart_url = images.fanart_url
                        if not item.banner_url and images.banner_url:
                            item.banner_url = images.banner_url
                    elif images is None and self._audiodb_browse_queue:
                        settings = self._preferences_service.get_advanced_settings()
                        if settings.audiodb_enabled:
                            await self._audiodb_browse_queue.enqueue(
                                "artist",
                                item.musicbrainz_id,
                                name=item.title,
                            )
                elif item.type == "album":
                    if images and not images.is_negative:
                        if not item.album_thumb_url and images.album_thumb_url:
                            item.album_thumb_url = images.album_thumb_url
                    elif images is None and self._audiodb_browse_queue:
                        settings = self._preferences_service.get_advanced_settings()
                        if settings.audiodb_enabled:
                            await self._audiodb_browse_queue.enqueue(
                                "album",
                                item.musicbrainz_id,
                                name=item.title,
                                artist_name=item.artist,
                            )
            except Exception as e:  # noqa: BLE001
                logger.warning(
                    "AudioDB search overlay apply failed for %s %s: %s",
                    item.type,
                    item.musicbrainz_id[:8],
                    e,
                )

    @deduplicate(
        lambda self,
        query,
        limit_artists=10,
        limit_albums=10,
        buckets=None: (
            f"search:g{get_mb_source_generation()}:{query}:{limit_artists}:"
            f"{limit_albums}:{buckets}"
        )
    )
    async def search(
        self,
        query: str,
        limit_artists: int = 10,
        limit_albums: int = 10,
        buckets: Optional[list[str]] = None,
    ) -> SearchResponse:
        # ST1: the response bakes the user's primary/secondary type filters,
        # so they MUST join the cache key - this closes the one proven
        # prefs-baked gap that forced wholesale sweeps on preference saves.
        prefs = self._preferences_service.get_preferences()
        source_generation = get_mb_source_generation()
        primary_types = ",".join(sorted(t.strip().lower() for t in prefs.primary_types))
        secondary_types = ",".join(
            sorted(t.strip().lower() for t in prefs.secondary_types)
        )
        cache_key = (
            f"g{source_generation}:{query.strip().lower()}:{limit_artists}:{limit_albums}:"
            f"{','.join(sorted(buckets)) if buckets else ''}"
            f":{primary_types}|{secondary_types}"
        )
        now = time.monotonic()
        cached = self._search_cache.get(cache_key)
        if cached and (now - cached[0]) < SEARCH_CACHE_TTL:
            return cached[1]

        included_secondary_types = set(t.lower() for t in prefs.secondary_types)
        included_primary_types = set(t.lower() for t in prefs.primary_types)

        limits = {}
        if not buckets or "artists" in buckets:
            limits["artists"] = limit_artists
        if not buckets or "albums" in buckets:
            limits["albums"] = limit_albums

        async def fetch_grouped():
            async with asyncio.timeout(FULL_SEARCH_TIMEOUT_SECONDS):
                return await self._mb_repo.search_grouped(
                    query,
                    limits=limits,
                    buckets=buckets,
                    included_secondary_types=included_secondary_types,
                    included_primary_types=included_primary_types,
                )

        tasks = [fetch_grouped()]
        if self._ownership is None:
            tasks.append(self._library_repo.get_library_mbids(include_release_ids=True))
        gathered = await asyncio.gather(*tasks, return_exceptions=True)
        if source_generation != get_mb_source_generation():
            return self._source_changed_response(buckets)
        grouped_result, *library_results = gathered
        remote_status_override: SearchRemoteStatus | None = None
        failed_buckets: set[str] | None = None
        if isinstance(grouped_result, TimeoutError):
            self._record_musicbrainz_error("Search exceeded the response deadline")
            logger.warning("MusicBrainz search exceeded the response deadline")
            grouped = None
            remote_status_override = "timeout"
        elif isinstance(grouped_result, Exception):
            self._record_musicbrainz_error("Search provider request failed")
            logger.warning(
                "MusicBrainz search failed: %s", type(grouped_result).__name__
            )
            grouped = None
            remote_status_override = "error"
        else:
            if isinstance(grouped_result, tuple):
                grouped, failed_buckets = grouped_result
                if failed_buckets:
                    self._record_musicbrainz_error("One or more search buckets failed")
            else:
                grouped = grouped_result
        library_result = library_results[0] if library_results else None
        library_mbids_raw = (
            None if isinstance(library_result, Exception) else library_result
        )

        if grouped is None:
            logger.warning("MusicBrainz search returned no results or failed")
        grouped = grouped or {"artists": [], "albums": []}
        library_mbids = library_mbids_raw or set()

        await self._apply_album_ownership(grouped.get("albums", []), library_mbids)
        all_results = grouped.get("artists", []) + grouped.get("albums", [])

        await self._apply_audiodb_search_overlay(all_results)
        if source_generation != get_mb_source_generation():
            return self._source_changed_response(buckets)

        top_artist = self._detect_top_result(grouped.get("artists", []), query)
        top_album = self._detect_top_result(grouped.get("albums", []), query)

        has_results = bool(grouped.get("artists") or grouped.get("albums"))
        remote_status = remote_status_override or self._remote_status(has_results)
        if remote_status_override is not None:
            bucket_status = {
                name: remote_status_override
                for name in ("artists", "albums")
                if not buckets or name in buckets
            }
        elif failed_buckets is not None:
            bucket_status = {
                name: "error" if name in failed_buckets else "ok"
                for name in ("artists", "albums")
                if not buckets or name in buckets
            }
        else:
            bucket_status = {
                name: remote_status
                for name in ("artists", "albums")
                if not buckets or name in buckets
            }
        response = SearchResponse(
            artists=grouped.get("artists", []),
            albums=grouped.get("albums", []),
            top_artist=top_artist,
            top_album=top_album,
            bucket_status=bucket_status,
        )
        if (
            all(status == "ok" for status in bucket_status.values())
            and source_generation == get_mb_source_generation()
        ):
            self._search_cache[cache_key] = (now, response)
        if len(self._search_cache) > SEARCH_CACHE_MAX_SIZE:
            expired = [
                k
                for k, (ts, _) in self._search_cache.items()
                if (now - ts) >= SEARCH_CACHE_TTL
            ]
            for k in expired:
                del self._search_cache[k]
            if len(self._search_cache) > SEARCH_CACHE_MAX_SIZE:
                oldest_key = min(
                    self._search_cache, key=lambda k: self._search_cache[k][0]
                )
                del self._search_cache[oldest_key]
        return response

    def schedule_cover_prefetch(self, albums: list[SearchResult]) -> list[str]:
        return [
            item.musicbrainz_id
            for item in albums[:COVER_PREFETCH_LIMIT]
            if item.musicbrainz_id
        ]

    @deduplicate(
        lambda self,
        bucket,
        query,
        limit=50,
        offset=0: (
            f"search_bucket:g{get_mb_source_generation()}:{bucket}:{query}:{limit}:{offset}"
        )
    )
    async def search_bucket(
        self, bucket: str, query: str, limit: int = 50, offset: int = 0
    ) -> tuple[list[SearchResult], SearchResult | None, SearchRemoteStatus]:
        prefs = self._preferences_service.get_preferences()
        included_secondary_types = set(t.lower() for t in prefs.secondary_types)
        included_primary_types = set(t.lower() for t in prefs.primary_types)
        source_generation = get_mb_source_generation()
        stale_cache_key = self._bucket_stale_cache_key(
            bucket,
            query,
            limit,
            offset,
            included_primary_types,
            included_secondary_types,
        )

        try:
            async with asyncio.timeout(FULL_SEARCH_TIMEOUT_SECONDS):
                if bucket == "artists":
                    results = await self._mb_repo.search_artists(
                        query, limit=limit, offset=offset
                    )
                elif bucket == "albums":
                    results = await self._mb_repo.search_albums(
                        query,
                        limit=limit,
                        offset=offset,
                        included_secondary_types=included_secondary_types,
                        included_primary_types=included_primary_types,
                    )
                else:
                    return [], None, "error"
        except TimeoutError:
            self._record_musicbrainz_error(
                "Search bucket exceeded the response deadline"
            )
            logger.warning(
                "MusicBrainz %s search exceeded the response deadline", bucket
            )
            if source_generation != get_mb_source_generation():
                return [], None, "timeout"
            results = self._get_stale_bucket(stale_cache_key, time.monotonic())
            if results is None:
                return [], None, "timeout"
            status: SearchRemoteStatus = "stale"
        except Exception as error:  # noqa: BLE001 - provider failures degrade one bucket
            self._record_musicbrainz_error("Search bucket provider request failed")
            logger.warning(
                "MusicBrainz %s search failed: %s", bucket, type(error).__name__
            )
            if source_generation != get_mb_source_generation():
                return [], None, "error"
            results = self._get_stale_bucket(stale_cache_key, time.monotonic())
            if results is None:
                return [], None, "error"
            status = "stale"
        else:
            if source_generation != get_mb_source_generation():
                return [], None, "error"
            status = self._remote_status(bool(results))
            if status == "ok" and results:
                self._store_stale_bucket(
                    stale_cache_key,
                    results,
                    time.monotonic(),
                )

        if bucket == "albums":
            library_mbids_raw = None
            if self._ownership is None:
                [library_mbids_raw] = await self._safe_gather(
                    self._library_repo.get_library_mbids(include_release_ids=True),
                )
            library_mbids = library_mbids_raw or set()

            await self._apply_album_ownership(results, library_mbids)

        await self._apply_audiodb_search_overlay(results)
        if source_generation != get_mb_source_generation():
            return [], None, "error"

        top_result = self._detect_top_result(results, query) if offset == 0 else None
        return results, top_result, status

    @deduplicate(
        lambda self, query, limit=5: (
            f"suggest:g{get_mb_source_generation()}:{query.strip().lower()}:{limit}"
        )
    )
    async def suggest(self, query: str, limit: int = 5) -> SuggestResponse:
        query = query.strip()
        if len(query) < 2:
            return SuggestResponse()

        source_generation = get_mb_source_generation()
        prefs = self._preferences_service.get_preferences()
        included_secondary_types = set(t.lower() for t in prefs.secondary_types)
        included_primary_types = set(t.lower() for t in prefs.primary_types)
        bucket_limit = ceil(limit * 0.6)

        try:
            async with asyncio.timeout(SUGGEST_TIMEOUT_SECONDS):
                grouped_result = await self._mb_repo.search_grouped(
                    query,
                    limits={"artists": bucket_limit, "albums": bucket_limit},
                    included_secondary_types=included_secondary_types,
                    included_primary_types=included_primary_types,
                )
        except TimeoutError:
            self._record_musicbrainz_error("Suggestions exceeded the response deadline")
            logger.warning("MusicBrainz suggest exceeded the response deadline")
            if source_generation != get_mb_source_generation():
                return SuggestResponse(remote_status="error")
            return SuggestResponse(remote_status="timeout")
        except Exception as e:  # noqa: BLE001
            self._record_musicbrainz_error("Suggestions provider request failed")
            logger.warning(
                "MusicBrainz suggest failed (query_len=%d): %s",
                len(query),
                type(e).__name__,
            )
            return SuggestResponse(remote_status="error")

        if source_generation != get_mb_source_generation():
            return SuggestResponse(remote_status="error")
        failed_buckets: set[str] | None = None
        if isinstance(grouped_result, tuple):
            grouped, failed_buckets = grouped_result
            if failed_buckets:
                self._record_musicbrainz_error("One or more suggestion buckets failed")
        else:
            grouped = grouped_result
        grouped = grouped or {"artists": [], "albums": []}

        library_mbids_raw = None
        if self._ownership is None:
            [library_mbids_raw] = await self._safe_gather(
                self._library_repo.get_library_mbids(include_release_ids=True),
            )
        if source_generation != get_mb_source_generation():
            return SuggestResponse(remote_status="error")
        library_mbids = library_mbids_raw or set()

        await self._apply_album_ownership(grouped.get("albums", []), library_mbids)
        if source_generation != get_mb_source_generation():
            return SuggestResponse(remote_status="error")

        suggestions: list[SuggestResult] = []
        for item in grouped.get("artists", []) + grouped.get("albums", []):
            suggestions.append(
                SuggestResult(
                    type=item.type,
                    title=item.title,
                    artist=item.artist,
                    year=item.year,
                    musicbrainz_id=item.musicbrainz_id,
                    in_library=item.in_library,
                    requested=item.requested,
                    disambiguation=item.disambiguation,
                    score=item.score,
                )
            )

        type_order = {"artist": 0, "album": 1}
        suggestions.sort(
            key=lambda s: (-s.score, type_order.get(s.type, 2), s.title.lower())
        )
        remote_status = self._remote_status(bool(suggestions))
        if failed_buckets:
            remote_status = "partial" if suggestions else "error"
        if source_generation != get_mb_source_generation():
            return SuggestResponse(remote_status="error")
        return SuggestResponse(results=suggestions[:limit], remote_status=remote_status)
