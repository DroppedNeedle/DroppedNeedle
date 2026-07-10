import asyncio
import logging
from typing import Any

import httpx
import msgspec

from models.search import SearchResult
from core.exceptions import ExternalServiceError
from services.preferences_service import PreferencesService
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.cache_keys import (
    mb_artist_search_key, mb_artist_detail_key,
    MB_ARTISTS_BY_TAG_PREFIX, MB_ARTIST_RELS_PREFIX,
    MB_ARTIST_EXPANSION_PREFIX, MB_LABEL_RELEASES_PREFIX,
)
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.resilience.retry import CircuitOpenError
from repositories.musicbrainz_base import (
    mb_api_get,
    mb_deduplicator,
    dedupe_by_id,
    get_score,
    build_musicbrainz_tag_query,
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
    release_groups: list[dict[str, Any]] = msgspec.field(name="release-groups", default_factory=list)
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


class MusicBrainzArtistMixin:
    _cache: CacheInterface
    _preferences_service: PreferencesService

    def _map_artist_to_result(self, artist: dict[str, Any]) -> SearchResult | None:
        artist_id = artist.get("id", "")
        if artist_id in FILTERED_ARTIST_MBIDS:
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
        self,
        query: str,
        limit: int = 10,
        offset: int = 0
    ) -> list[SearchResult]:
        cache_key = mb_artist_search_key(query, limit, offset)

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

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
            artists = result.artists
            artists = dedupe_by_id(artists)

            results = []
            for a in artists:
                mapped = self._map_artist_to_result(a)
                if mapped:
                    results.append(mapped)
                if len(results) >= limit:
                    break

            advanced_settings = self._preferences_service.get_advanced_settings()
            await self._cache.set(cache_key, results, ttl_seconds=advanced_settings.cache_ttl_search)
            return results
        except Exception as e:  # noqa: BLE001
            logger.error(f"MusicBrainz artist search failed: {e}")
            _record_mb_degradation(f"artist search failed: {e}")
            return []

    async def search_artists_by_tag(
        self,
        tag: str,
        limit: int = 50,
        offset: int = 0
    ) -> list[SearchResult]:
        cache_key = f"{MB_ARTISTS_BY_TAG_PREFIX}{tag.lower()}:{limit}:{offset}"

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

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
            artists = result.artists
            artists = dedupe_by_id(artists)

            results = [r for a in artists[:limit] if (r := self._map_artist_to_result(a)) is not None]

            advanced_settings = self._preferences_service.get_advanced_settings()
            await self._cache.set(cache_key, results, ttl_seconds=advanced_settings.cache_ttl_search * 2)
            return results
        except Exception as e:  # noqa: BLE001
            logger.error(f"MusicBrainz artist tag search failed for '{tag}': {e}")
            _record_mb_degradation(f"artist tag search failed: {e}")
            return []

    async def get_artist_by_id(self, mbid: str) -> dict | None:
        cache_key = mb_artist_detail_key(mbid)

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        dedupe_key = f"mb:artist:{mbid}"
        return await mb_deduplicator.dedupe(dedupe_key, lambda: self._fetch_artist_by_id(mbid, cache_key))

    async def get_artist_relations(self, mbid: str) -> dict | None:
        detail_key = mb_artist_detail_key(mbid)
        cached = await self._cache.get(detail_key)
        if cached is not None:
            return cached

        rels_key = f"{MB_ARTIST_RELS_PREFIX}{mbid}"
        cached_rels = await self._cache.get(rels_key)
        if cached_rels is not None:
            return cached_rels

        dedupe_key = f"{MB_ARTIST_RELS_PREFIX}{mbid}"
        return await mb_deduplicator.dedupe(dedupe_key, lambda: self._fetch_artist_relations(mbid, rels_key))

    async def _fetch_artist_relations(self, mbid: str, cache_key: str) -> dict | None:
        try:
            result = await mb_api_get(
                f"/artist/{mbid}",
                params={"inc": "url-rels"},
                priority=RequestPriority.IMAGE_FETCH,
            )
            if not result:
                return None
            await self._cache.set(cache_key, result, ttl_seconds=86400)
            return result
        except (CircuitOpenError, httpx.HTTPError, ExternalServiceError):
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch artist relations {mbid}: {e}")
            _record_mb_degradation(f"artist relations failed: {e}")
            return None

    async def _fetch_artist_by_id(self, mbid: str, cache_key: str) -> dict | None:
        try:
            limit = 50

            artist_result, browse_result = await asyncio.gather(
                mb_api_get(
                    f"/artist/{mbid}",
                    params={"inc": "tags+aliases+url-rels"},
                    priority=RequestPriority.USER_INITIATED,
                ),
                mb_api_get(
                    "/release-group",
                    params={"artist": mbid, "limit": limit, "offset": 0},
                    priority=RequestPriority.USER_INITIATED,
                    decode_type=_ArtistReleaseGroupsPayload,
                ),
            )

            if not artist_result:
                return None

            all_release_groups = browse_result.release_groups
            total_count = int(browse_result.release_group_count)

            if all_release_groups:
                artist_result["release-group-list"] = all_release_groups

            artist_result["release-group-count"] = total_count

            await self._cache.set(cache_key, artist_result, ttl_seconds=21600)

            from core.task_registry import TaskRegistry
            registry = TaskRegistry.get_instance()
            if not registry.is_running("mb-release-group-warmup"):
                _rg_task = asyncio.create_task(self._warm_release_group_cache(all_release_groups[:6]))
                try:
                    registry.register("mb-release-group-warmup", _rg_task)
                except RuntimeError:
                    pass

            return artist_result
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch artist {mbid}: {e}")
            _record_mb_degradation(f"artist fetch failed: {e}")
            return None

    async def _warm_release_group_cache(self, release_groups: list[dict[str, Any]]) -> None:
        for rg in release_groups:
            rg_id = rg.get("id")
            if not rg_id:
                continue
            try:
                await self.get_release_group_by_id(rg_id, priority=RequestPriority.BACKGROUND_SYNC)
            except (CircuitOpenError, ExternalServiceError, httpx.HTTPError):
                pass

    async def get_artist_expansion(self, mbid: str) -> dict | None:
        """Artist relationships (band members, collaborations, tributes), label
        relations, and tags — the taste-graph expansion payload. One MB call per
        artist per 24h (cached + deduplicated); BACKGROUND_SYNC priority so it can
        never starve user-initiated lookups."""
        cache_key = f"{MB_ARTIST_EXPANSION_PREFIX}{mbid}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        return await mb_deduplicator.dedupe(
            cache_key, lambda: self._fetch_artist_expansion(mbid, cache_key)
        )

    async def _fetch_artist_expansion(self, mbid: str, cache_key: str) -> dict | None:
        try:
            result = await mb_api_get(
                f"/artist/{mbid}",
                params={"inc": "artist-rels+label-rels+tags"},
                priority=RequestPriority.BACKGROUND_SYNC,
            )
            if not result:
                return None
            await self._cache.set(cache_key, result, ttl_seconds=86400)
            return result
        except Exception as e:  # noqa: BLE001 - one seed failing must not sink the graph
            logger.error(f"Failed to fetch artist expansion {mbid}: {e}")
            _record_mb_degradation(f"artist expansion failed: {e}")
            return None

    async def get_label_releases(self, label_mbid: str, limit: int = 25) -> list[dict[str, Any]]:
        """Releases on a label (with release-groups + artist credits), for
        label-mate discovery. Cached 24h; BACKGROUND_SYNC priority."""
        cache_key = f"{MB_LABEL_RELEASES_PREFIX}{label_mbid}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        try:
            result = await mb_api_get(
                "/release",
                params={
                    "label": label_mbid,
                    "inc": "release-groups+artist-credits",
                    "limit": min(100, limit),
                },
                priority=RequestPriority.BACKGROUND_SYNC,
            )
            releases = result.get("releases", []) if isinstance(result, dict) else []
            await self._cache.set(cache_key, releases, ttl_seconds=86400)
            return releases
        except Exception as e:  # noqa: BLE001 - one label failing must not sink the graph
            logger.error(f"Failed to fetch label releases {label_mbid}: {e}")
            _record_mb_degradation(f"label releases failed: {e}")
            return []

    async def get_artist_release_groups(
        self,
        artist_mbid: str,
        offset: int = 0,
        limit: int = 50,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
    ) -> tuple[list[dict[str, Any]], int]:
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
            logger.error(f"Failed to fetch release groups for artist {artist_mbid} at offset {offset}: {e}")
            _record_mb_degradation(f"release groups failed: {e}")
            return [], 0

    async def get_artist_release_groups_or_raise(
        self,
        artist_mbid: str,
        offset: int = 0,
        limit: int = 50,
    ) -> tuple[list[dict[str, Any]], int]:
        """Like get_artist_release_groups but PROPAGATES failures (CircuitOpenError,
        ExternalServiceError, httpx errors) instead of masking them as an empty
        result. The follow poller needs to tell 'this artist has no releases' apart
        from 'MusicBrainz is unavailable', so it never seeds an empty baseline or
        treats a back-catalog as new on a transient error."""
        result = await mb_api_get(
            "/release-group",
            params={"artist": artist_mbid, "limit": limit, "offset": offset},
            priority=RequestPriority.BACKGROUND_SYNC,
            decode_type=_ArtistReleaseGroupsPayload,
        )
        return result.release_groups, int(result.release_group_count)

    async def get_release_groups_by_artist(
        self,
        artist_mbid: str,
        limit: int = 10,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
    ) -> list[dict[str, Any]]:
        release_groups, _ = await self.get_artist_release_groups(
            artist_mbid, offset=0, limit=limit, priority=priority
        )
        return release_groups
