import asyncio
import logging
from typing import Any, NoReturn

import httpx
import msgspec

from models.search import SearchResult
from core.exceptions import ExternalServiceError
from services.preferences_service import PreferencesService
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.cache_keys import (
    mb_artist_search_key,
    mb_artist_detail_key,
    mb_artist_rgs_browse_key,
    MB_ARTISTS_BY_TAG_PREFIX,
    MB_ARTIST_RELS_PREFIX,
)
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.resilience.retry import CircuitOpenError
from repositories.musicbrainz_base import (
    mb_api_get,
    mb_cache_set_if_current,
    mb_deduplicator,
    clear_mb_response_context,
    dedupe_by_id,
    get_mb_response_context,
    normalize_mb_id,
    get_score,
    build_musicbrainz_tag_query,
    is_mb_source_current,
)
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult

logger = logging.getLogger(__name__)


def _record_mb_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx:
        ctx.record(IntegrationResult.error(source="musicbrainz", msg=msg))


class _ArtistSearchPayload(msgspec.Struct):
    artists: list[dict[str, Any]] = msgspec.field(default_factory=list)


class _ArtistReleaseGroupsPayload(msgspec.Struct):
    release_groups: list[dict[str, Any]] = msgspec.field(
        name="release-groups", default_factory=list
    )
    release_group_count: int = msgspec.field(name="release-group-count", default=0)


FILTERED_ARTIST_MBIDS = {
    "89ad4ac3-39f7-470e-963a-56509c546377",  # Various Artists
    "41ece0f7-91f6-4c87-982c-3a39c5a02586",  # /v/
    "125ec42a-7229-4250-afc5-e057484327fe",  # [Unknown]
}

FILTERED_ARTIST_NAMES = {
    "various artists",
    "[unknown]",
    "/v/",
}

_ARTIST_NOT_FOUND_TTL_SECONDS = 600


def _raise_artist_fetch_error(mbid: str, exc: BaseException) -> NoReturn:
    """Preserve typed provider failures instead of reporting a miss."""
    if isinstance(exc, asyncio.CancelledError):
        raise exc
    if not isinstance(exc, Exception):
        raise exc

    if not isinstance(exc, CircuitOpenError):
        logger.error("Failed to fetch artist %s: %s", mbid, exc)
    _record_mb_degradation("artist fetch failed")

    if isinstance(exc, (CircuitOpenError, ExternalServiceError)):
        raise exc
    raise ExternalServiceError(
        "MusicBrainz artist metadata is temporarily unavailable."
    ) from exc


class MusicBrainzArtistMixin:
    _cache: CacheInterface
    _preferences_service: PreferencesService
    def _map_artist_to_result(self, artist: dict[str, Any]) -> SearchResult | None:
        artist_id = artist.get("id", "")
        if normalize_mb_id(artist_id) in FILTERED_ARTIST_MBIDS:
            return None

        name = artist.get("name", "Unknown Artist")
        if name.lower() in FILTERED_ARTIST_NAMES:
            return None

        return SearchResult(
            type="artist",
            title=name,
            musicbrainz_id=artist_id,
            in_library=False,
            disambiguation=artist.get("disambiguation") or None,
            type_info=artist.get("type") or None,
            score=get_score(artist),
        )

    async def search_artists(
        self, query: str, limit: int = 10, offset: int = 0
    ) -> list[SearchResult]:
        cache_key = mb_artist_search_key(query, limit, offset)

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        async def load() -> list[SearchResult]:
            try:
                search_query = f'artist:"{query}"^3 OR artistaccent:"{query}"^3 OR alias:"{query}"^2 OR {query}'

                result = await mb_api_get(
                    "/artist",
                    params={
                        "query": search_query,
                        "limit": min(100, max(limit * 2, 25)),
                        "offset": offset,
                    },
                    priority=RequestPriority.USER_INITIATED,
                    decode_type=_ArtistSearchPayload,
                )
                response_context = get_mb_response_context()
                artists = dedupe_by_id(result.artists)

                results = []
                for a in artists:
                    mapped = self._map_artist_to_result(a)
                    if mapped:
                        results.append(mapped)
                    if len(results) >= limit:
                        break

                advanced_settings = self._preferences_service.get_advanced_settings()
                await mb_cache_set_if_current(
                    self._cache,
                    cache_key,
                    results,
                    ttl_seconds=advanced_settings.cache_ttl_search,
                    context=response_context,
                )
                return results
            except Exception as e:  # noqa: BLE001
                logger.error(f"MusicBrainz artist search failed: {e}")
                _record_mb_degradation(f"artist search failed: {e}")
                return []

        return await mb_deduplicator.dedupe(cache_key, load)

    async def search_artists_by_tag(
        self, tag: str, limit: int = 50, offset: int = 0
    ) -> list[SearchResult]:
        cache_key = f"{MB_ARTISTS_BY_TAG_PREFIX}{tag.lower()}:{limit}:{offset}"

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        async def load() -> list[SearchResult]:
            try:
                result = await mb_api_get(
                    "/artist",
                    params={
                        "query": build_musicbrainz_tag_query(tag),
                        "limit": min(100, limit),
                        "offset": offset,
                    },
                    priority=RequestPriority.BACKGROUND_SYNC,
                    decode_type=_ArtistSearchPayload,
                )
                response_context = get_mb_response_context()
                artists = dedupe_by_id(result.artists)

                results = [
                    r
                    for a in artists[:limit]
                    if (r := self._map_artist_to_result(a)) is not None
                ]

                advanced_settings = self._preferences_service.get_advanced_settings()
                await mb_cache_set_if_current(
                    self._cache,
                    cache_key,
                    results,
                    ttl_seconds=advanced_settings.cache_ttl_search * 2,
                    context=response_context,
                )
                return results
            except Exception as e:  # noqa: BLE001
                logger.error(f"MusicBrainz artist tag search failed for '{tag}': {e}")
                _record_mb_degradation(f"artist tag search failed: {e}")
                return []

        return await mb_deduplicator.dedupe(cache_key, load)

    async def get_artist_by_id(
        self,
        mbid: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> dict[str, Any] | None:
        clear_mb_response_context()
        mbid = normalize_mb_id(mbid)
        cache_key = mb_artist_detail_key(mbid)

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        dedupe_key = f"mb:artist:{mbid}:priority={int(priority)}"
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_artist_by_id(mbid, cache_key, priority),
        )

    async def get_artist_relations(self, mbid: str) -> dict | None:
        clear_mb_response_context()
        mbid = normalize_mb_id(mbid)
        detail_key = mb_artist_detail_key(mbid)
        cached = await self._cache.get(detail_key)
        if cached is not None:
            return cached

        rels_key = f"{MB_ARTIST_RELS_PREFIX}{mbid}"
        cached_rels = await self._cache.get(rels_key)
        if cached_rels is not None:
            return cached_rels

        dedupe_key = f"{MB_ARTIST_RELS_PREFIX}{mbid}"
        return await mb_deduplicator.dedupe(
            dedupe_key, lambda: self._fetch_artist_relations(mbid, rels_key)
        )

    async def _fetch_artist_relations(self, mbid: str, cache_key: str) -> dict | None:
        try:
            result = await mb_api_get(
                f"/artist/{mbid}",
                params={"inc": "url-rels"},
                priority=RequestPriority.IMAGE_FETCH,
            )
            response_context = get_mb_response_context()
            if not result:
                return None
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                result,
                ttl_seconds=86400,
                context=response_context,
            )
            return result
        except (CircuitOpenError, httpx.HTTPError, ExternalServiceError):
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch artist relations {mbid}: {e}")
            _record_mb_degradation(f"artist relations failed: {e}")
            return None

    async def _fetch_artist_by_id(
        self,
        mbid: str,
        cache_key: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> dict[str, Any] | None:
        clear_mb_response_context()
        limit = 50

        async def fetch_artist():
            result = await mb_api_get(
                f"/artist/{mbid}",
                params={"inc": "tags+aliases+url-rels"},
                priority=priority,
            )
            return result, get_mb_response_context()

        async def fetch_browse():
            result = await mb_api_get(
                "/release-group",
                params={"artist": mbid, "limit": limit, "offset": 0},
                priority=priority,
                decode_type=_ArtistReleaseGroupsPayload,
            )
            return result, get_mb_response_context()

        # Keep both calls concurrent; detail 404 remains authoritative.
        artist_out, browse_out = await asyncio.gather(
            fetch_artist(),
            fetch_browse(),
            return_exceptions=True,
        )

        artist_context = None
        browse_context = None
        if not isinstance(artist_out, BaseException):
            artist_result, artist_context = artist_out
        else:
            artist_result = artist_out
        if not isinstance(browse_out, BaseException):
            browse_result, browse_context = browse_out
        else:
            browse_result = browse_out

        if isinstance(artist_result, asyncio.CancelledError):
            raise artist_result
        if isinstance(browse_result, asyncio.CancelledError):
            raise browse_result

        if isinstance(artist_result, BaseException):
            _raise_artist_fetch_error(mbid, artist_result)
        if not artist_result:
            if isinstance(browse_result, BaseException):
                # Retrieve/record the ancillary failure before honoring the 404.
                try:
                    _raise_artist_fetch_error(mbid, browse_result)
                except (CircuitOpenError, ExternalServiceError):
                    pass
            if artist_context is None or is_mb_source_current(artist_context):
                try:
                    await mb_cache_set_if_current(
                        self._cache,
                        cache_key,
                        {},
                        ttl_seconds=_ARTIST_NOT_FOUND_TTL_SECONDS,
                        context=artist_context,
                    )
                except asyncio.CancelledError:
                    raise
                except Exception:  # noqa: BLE001
                    logger.warning("Failed to cache missing artist %s", mbid)
            return None

        if isinstance(browse_result, BaseException):
            _raise_artist_fetch_error(mbid, browse_result)

        contexts_compatible = (
            artist_context is None
            and browse_context is None
            or artist_context is not None
            and browse_context is not None
            and artist_context == browse_context
            and is_mb_source_current(artist_context)
        )
        if not contexts_compatible:
            _record_mb_degradation(
                "artist detail and release-group sources changed during fetch"
            )
            if artist_context is None or not is_mb_source_current(artist_context):
                return None
            safe_result = dict(artist_result)
            safe_result.pop("release-group-list", None)
            safe_result["release-group-count"] = 0
            return safe_result

        all_release_groups = browse_result.release_groups
        total_count = int(browse_result.release_group_count)

        merged_result = dict(artist_result)
        if all_release_groups:
            merged_result["release-group-list"] = all_release_groups
        merged_result["release-group-count"] = total_count

        published = await mb_cache_set_if_current(
            self._cache,
            cache_key,
            merged_result,
            ttl_seconds=21600,
            context=artist_context,
        )
        if not published:
            safe_result = dict(artist_result)
            safe_result.pop("release-group-list", None)
            safe_result["release-group-count"] = 0
            return safe_result

        from core.task_registry import TaskRegistry

        registry = TaskRegistry.get_instance()
        if not registry.is_running("mb-release-group-warmup"):
            _rg_task = asyncio.create_task(
                self._warm_release_group_cache(all_release_groups[:6])
            )
            try:
                registry.register("mb-release-group-warmup", _rg_task)
            except RuntimeError:
                pass

        return merged_result

    async def _warm_release_group_cache(
        self, release_groups: list[dict[str, Any]]
    ) -> None:
        for rg in release_groups:
            rg_id = rg.get("id")
            if not rg_id:
                continue
            try:
                await self.get_release_group_by_id(
                    rg_id, priority=RequestPriority.BACKGROUND_SYNC
                )
            except (CircuitOpenError, ExternalServiceError, httpx.HTTPError):
                pass

    async def get_artist_release_groups(
        self,
        artist_mbid: str,
        offset: int = 0,
        limit: int = 50,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
    ) -> tuple[list[dict[str, Any]], int]:
        artist_mbid = normalize_mb_id(artist_mbid)
        try:
            result = await mb_api_get(
                "/release-group",
                params={"artist": artist_mbid, "limit": limit, "offset": offset},
                priority=priority,
                decode_type=_ArtistReleaseGroupsPayload,
            )

            release_groups = result.release_groups
            total_count = int(result.release_group_count)

            return release_groups, total_count
        except Exception as e:  # noqa: BLE001
            logger.error(
                f"Failed to fetch release groups for artist {artist_mbid} at offset {offset}: {e}"
            )
            _record_mb_degradation(f"release groups failed: {e}")
            return [], 0

    async def get_artist_release_groups_or_raise(
        self,
        artist_mbid: str,
        offset: int = 0,
        limit: int = 50,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
    ) -> tuple[list[dict[str, Any]], int]:
        """Like get_artist_release_groups but PROPAGATES failures (CircuitOpenError,
        ExternalServiceError, httpx errors) instead of masking them as an empty
        result. The follow poller needs to tell 'this artist has no releases' apart
        from 'MusicBrainz is unavailable', so it never seeds an empty baseline or
        treats a back-catalog as new on a transient error."""
        artist_mbid = normalize_mb_id(artist_mbid)
        result = await mb_api_get(
            "/release-group",
            params={"artist": artist_mbid, "limit": limit, "offset": offset},
            priority=priority,
            decode_type=_ArtistReleaseGroupsPayload,
        )
        return result.release_groups, int(result.release_group_count)

    async def get_release_groups_by_artist(
        self,
        artist_mbid: str,
        limit: int = 10,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
    ) -> list[dict[str, Any]]:
        """QW1: single-page artist->RG browse with cache-aside + coalescing.

        Previously a cache-free wrapper, so every more-by-artist render paid
        one wire call against the 1 req/s bucket. Now: positive entries TTL
        3600 s, genuinely-empty results negative-cached briefly (600 s), and
        concurrent cold callers share one wire call via mb_deduplicator.
        Failures are NEVER cached - they propagate after recording degradation
        so outages cannot poison the cache (same rationale as
        musicbrainz_album._fetch_release_group_by_id).
        """
        artist_mbid = normalize_mb_id(artist_mbid)
        cache_key = mb_artist_rgs_browse_key(artist_mbid, limit)

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        dedupe_key = f"{cache_key}:priority={int(priority)}"
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_browse_release_groups(
                artist_mbid, limit, priority, cache_key
            ),
        )

    async def _fetch_browse_release_groups(
        self,
        artist_mbid: str,
        limit: int,
        priority: RequestPriority,
        cache_key: str,
    ) -> list[dict[str, Any]]:
        try:
            release_groups, _total = await self.get_artist_release_groups_or_raise(
                artist_mbid, offset=0, limit=limit, priority=priority
            )
            response_context = get_mb_response_context()
        except Exception as e:  # noqa: BLE001 - telemetry then re-raise (QW1)
            # Same message site as the old swallowing variant; propagation is
            # what keeps failures distinguishable from "genuinely zero RGs".
            logger.error(
                f"Failed to fetch release groups for artist {artist_mbid}: {e}"
            )
            _record_mb_degradation(f"release groups failed: {e}")
            raise
        if release_groups:
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                release_groups,
                ttl_seconds=3600,
                context=response_context,
            )
        else:
            # Genuinely empty discography: short negative cache so a dead-end
            # artist page isn't re-browsed on every view.
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                [],
                ttl_seconds=600,
                context=response_context,
            )
        return release_groups
