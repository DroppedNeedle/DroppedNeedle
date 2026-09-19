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
    mb_artist_rgs_page_key,
    MB_ARTISTS_BY_TAG_PREFIX,
    MB_ARTIST_RELS_PREFIX,
)
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.resilience.retry import CircuitOpenError
from repositories.musicbrainz_base import (
    MbSourceContext,
    mb_api_get,
    mb_cache_get_if_current,
    mb_cache_set_if_current,
    mb_deduplicator,
    clear_mb_response_context,
    capture_mb_source_context,
    capture_mb_cache_token,
    MbCachePolicy,
    get_mb_response_metadata,
    dedupe_by_id,
    get_mb_response_context,
    normalize_mb_id,
    resolve_redirect_mbid,
    get_score,
    build_musicbrainz_tag_query,
    is_mb_source_current,
)
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from infrastructure.validators import is_valid_mbid
from repositories.musicbrainz_response_cache import merge_mb_metadata, response_metadata

logger = logging.getLogger(__name__)


def _record_mb_degradation(msg: str, *, deterministic: bool = False) -> None:
    """Bank a MusicBrainz degradation; deterministic=True marks caller-input
    failures (invalid MBIDs) for UNMAPPABLE_PROVIDER_PAYLOAD classification
    (F-IDENT-02). Failure/exception records keep the transient default."""
    ctx = try_get_degradation_context()
    if ctx:
        if deterministic:
            ctx.record(
                IntegrationResult.deterministic_error(
                    source="musicbrainz", msg=msg
                )
            )
        else:
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
        logger.error("MusicBrainz artist fetch failed")
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
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        cache_token = capture_mb_cache_token(self._cache)
        cache_key = mb_artist_search_key(query, limit, offset)

        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
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
                    source_context=source_context,
                )
                response_context = get_mb_response_context() or source_context
                artists = dedupe_by_id(result.artists)

                results = []
                for a in artists:
                    mapped = self._map_artist_to_result(a)
                    if mapped:
                        results.append(mapped)
                    if len(results) >= limit:
                        break

                advanced_settings = self._preferences_service.get_advanced_settings()
                await mb_cache_set_if_current(self._cache,
                cache_key,
                results,
                ttl_seconds=advanced_settings.cache_ttl_search,
                context=response_context, cache_token=cache_token)
                return results
            except Exception as e:  # noqa: BLE001
                logger.error("MusicBrainz artist search failed")
                _record_mb_degradation("artist search failed")
                return []

        return await mb_deduplicator.dedupe(cache_key, load)

    async def search_artists_by_tag(
        self, tag: str, limit: int = 50, offset: int = 0
    ) -> list[SearchResult]:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        cache_token = capture_mb_cache_token(self._cache)
        cache_key = f"{MB_ARTISTS_BY_TAG_PREFIX}{tag.lower()}:{limit}:{offset}"

        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
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
                    source_context=source_context,
                )
                response_context = get_mb_response_context() or source_context
                artists = dedupe_by_id(result.artists)

                results = [
                    r
                    for a in artists[:limit]
                    if (r := self._map_artist_to_result(a)) is not None
                ]

                advanced_settings = self._preferences_service.get_advanced_settings()
                await mb_cache_set_if_current(self._cache,
                cache_key,
                results,
                ttl_seconds=advanced_settings.cache_ttl_search * 2,
                context=response_context, cache_token=cache_token)
                return results
            except Exception as e:  # noqa: BLE001
                logger.error("MusicBrainz artist tag search failed")
                _record_mb_degradation("artist tag search failed")
                return []

        return await mb_deduplicator.dedupe(cache_key, load)

    async def get_artist_core(
        self,
        mbid: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> dict[str, Any] | None:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        cache_token = capture_mb_cache_token(self._cache)
        mbid = normalize_mb_id(mbid)
        if not is_valid_mbid(mbid):
            _record_mb_degradation("artist invalid mbid", deterministic=True)
            return None
        # Pre-resolution orphans old-MBID cache entries until TTL (bounded,
        # acceptable); everything below keys off the rewritten MBID.
        mbid = await resolve_redirect_mbid(
            "artist", mbid, source_context, self._cache,
            getattr(self, "_mb_canonical_store", None),
        )
        cache_key = mb_artist_detail_key(mbid, profile="core")
        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached or None
        result = await mb_api_get(
            f"/artist/{mbid}",
            params={"inc": "tags+aliases+url-rels"},
            priority=priority,
            source_context=source_context,
            cache_policy=MbCachePolicy.DISPLAY_FRESH,
        )
        await mb_cache_set_if_current(self._cache, cache_key, result,
        ttl_seconds=21600 if result else _ARTIST_NOT_FOUND_TTL_SECONDS,
        context=get_mb_response_context() or source_context, cache_token=cache_token)
        return result or None

    async def get_artist_by_id(
        self,
        mbid: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        *,
        include_releases: bool = True,
        release_group_limit: int = 50,
    ) -> dict[str, Any] | None:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        cache_token = capture_mb_cache_token(self._cache)
        mbid = normalize_mb_id(mbid)
        cache_key = mb_artist_detail_key(
            mbid, include_releases=include_releases,
            release_group_limit=release_group_limit,
        )

        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached

        profile = "full" if include_releases else "basic"
        dedupe_key = (
            f"mb:artist:{mbid}:profile={profile}:limit={max(int(release_group_limit), 1)}:priority={int(priority)}"
            f":g{source_context.generation}"
        )
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_artist_by_id(mbid,
            cache_key,
            priority,
            include_releases=include_releases,
            release_group_limit=release_group_limit,
            source_context=source_context, cache_token=cache_token),
        )

    async def get_artist_relations(self, mbid: str) -> dict | None:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        cache_token = capture_mb_cache_token(self._cache)
        mbid = normalize_mb_id(mbid)
        detail_key = mb_artist_detail_key(mbid)
        cached = await mb_cache_get_if_current(self._cache, detail_key, source_context)
        if cached is not None:
            return cached

        rels_key = f"{MB_ARTIST_RELS_PREFIX}{mbid}"
        cached_rels = await mb_cache_get_if_current(
            self._cache, rels_key, source_context
        )
        if cached_rels is not None:
            return cached_rels or None

        dedupe_key = f"{MB_ARTIST_RELS_PREFIX}{mbid}"
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_artist_relations(mbid, rels_key, source_context=source_context, cache_token=cache_token),
        )

    async def _fetch_artist_relations(
        self,
        mbid: str,
        cache_key: str,
        *,
        source_context: MbSourceContext,
        cache_token: tuple[object, int],
    ) -> dict | None:
        mbid = normalize_mb_id(mbid)
        if not is_valid_mbid(mbid):
            _record_mb_degradation("artist invalid mbid", deterministic=True)
            return None
        # Keyed by the caller's pre-resolve MBID; the wire call uses the rewritten MBID.
        mbid = await resolve_redirect_mbid(
            "artist", mbid, source_context, self._cache,
            getattr(self, "_mb_canonical_store", None),
        )
        try:
            result = await mb_api_get(
                f"/artist/{mbid}",
                params={"inc": "url-rels"},
                priority=RequestPriority.IMAGE_FETCH,
                source_context=source_context,
                cache_policy=MbCachePolicy.DISPLAY_FRESH,
            )
            response_context = get_mb_response_context() or source_context
            if not result:
                # Definitive miss: mb_api_get returns {} only on 404 (503/5xx
                # raise into the except below). Negative-cache briefly so a
                # merged/deleted artist mbid isn't re-fetched every tick;
                # short TTL bounds staleness and self-heals. The transient
                # except path stays uncached on purpose.
                await mb_cache_set_if_current(self._cache,
                cache_key,
                {},
                ttl_seconds=600,
                context=response_context, cache_token=cache_token)
                return None
            await mb_cache_set_if_current(self._cache,
            cache_key,
            result,
            ttl_seconds=86400,
            context=response_context, cache_token=cache_token)
            return result
        except (CircuitOpenError, httpx.HTTPError, ExternalServiceError):
            raise
        except Exception as e:  # noqa: BLE001
            logger.error("MusicBrainz artist relations fetch failed")
            _record_mb_degradation("artist relations failed")
            return None

    async def _fetch_artist_by_id(
        self,
        mbid: str,
        cache_key: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        *,
        include_releases: bool = True,
        release_group_limit: int = 50,
        source_context: MbSourceContext,
        cache_token: tuple[object, int],
    ) -> dict[str, Any] | None:
        clear_mb_response_context()
        mbid = normalize_mb_id(mbid)
        if not is_valid_mbid(mbid):
            _record_mb_degradation("artist invalid mbid", deterministic=True)
            return None
        # Keyed by the caller's pre-resolve MBID; the wire calls use the rewritten MBID.
        mbid = await resolve_redirect_mbid(
            "artist", mbid, source_context, self._cache,
            getattr(self, "_mb_canonical_store", None),
        )
        limit = max(int(release_group_limit), 1)
        artist_includes = "tags+aliases+url-rels"
        if not include_releases:
            # The basic endpoint still needs the release-group count exposed
            # by ArtistInfo, but does not need a second browse request.
            artist_includes += "+release-groups"

        async def fetch_artist():
            result = await mb_api_get(
                f"/artist/{mbid}",
                params={"inc": artist_includes},
                priority=priority,
                source_context=source_context,
                cache_policy=MbCachePolicy.DISPLAY_FRESH,
            )
            return result, get_mb_response_context() or source_context, get_mb_response_metadata()

        async def fetch_browse():
            try:
                (
                    release_groups,
                    total_count,
                    response_context,
                ) = await self._get_artist_release_groups_or_raise_with_context(
                    mbid,
                    offset=0,
                    limit=limit,
                    priority=priority,
                    source_context=source_context,
                    cache_policy=MbCachePolicy.DISPLAY_FRESH,
                )
            except ExternalServiceError:
                # The Step 04-1 page cache-aside raises this both for a
                # mid-flight source change (QW1 shape) and for genuine
                # provider failure; the merge below drops mixed-generation
                # browse data instead of failing, so disambiguate via the
                # task-local browse response context (this leg's wire call
                # only): stale or mismatched means the source changed ->
                # feed the drop-on-mismatch path an empty payload; missing
                # or current means genuine failure -> re-raise. 5xx leaves
                # the context equal to the caller source (or unset under
                # mocks), 429s re-raise via the check; transport/breaker
                # bypass this handler.
                browse_response_context = get_mb_response_context()
                if browse_response_context is not None and (
                    browse_response_context != source_context
                    or not is_mb_source_current(browse_response_context)
                ):
                    dropped = _ArtistReleaseGroupsPayload(
                        release_groups=[],
                        release_group_count=0,
                    )
                    return (
                        dropped,
                        browse_response_context,
                        get_mb_response_metadata(),
                    )
                raise
            result = _ArtistReleaseGroupsPayload(
                release_groups=release_groups,
                release_group_count=total_count,
            )
            return result, response_context, get_mb_response_metadata()

        # Keep both calls concurrent for the full-detail profile; the basic
        # profile is deliberately detail-only while retaining count fields.
        if include_releases:
            artist_out, browse_out = await asyncio.gather(
                fetch_artist(),
                fetch_browse(),
                return_exceptions=True,
            )
        else:
            artist_out = await fetch_artist()
            browse_out = None

        artist_context = None
        browse_context = None
        artist_metadata = browse_metadata = None
        if not isinstance(artist_out, BaseException):
            artist_result, artist_context, artist_metadata = artist_out
        else:
            artist_result = artist_out
        if browse_out is None:
            browse_result = None
        elif not isinstance(browse_out, BaseException):
            browse_result, browse_context, browse_metadata = browse_out
        else:
            browse_result = browse_out
        response_metadata.set(merge_mb_metadata(artist_metadata, browse_metadata))

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
                    await mb_cache_set_if_current(self._cache,
                    cache_key,
                    {},
                    ttl_seconds=_ARTIST_NOT_FOUND_TTL_SECONDS,
                    context=artist_context, cache_token=cache_token)
                except asyncio.CancelledError:
                    raise
                except Exception:  # noqa: BLE001
                    logger.warning("MusicBrainz missing artist cache write failed")
            return None
        if not include_releases:
            if artist_context is not None and not is_mb_source_current(artist_context):
                return None
            published = await mb_cache_set_if_current(self._cache,
            cache_key,
            artist_result,
            ttl_seconds=21600,
            context=artist_context, cache_token=cache_token)
            return artist_result if is_mb_source_current(artist_context) else None

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

        published = await mb_cache_set_if_current(self._cache,
        cache_key,
        merged_result,
        ttl_seconds=21600,
        context=artist_context, cache_token=cache_token)
        if not published:
            if is_mb_source_current(artist_context):
                return merged_result
            safe_result = dict(artist_result)
            safe_result.pop("release-group-list", None)
            safe_result["release-group-count"] = 0
            return safe_result

        return merged_result


    async def _fetch_artist_release_groups_page(
        self,
        artist_mbid: str,
        offset: int,
        fetch_limit: int,
        priority: RequestPriority,
        *,
        source_context: MbSourceContext | None = None,
        cache_policy: MbCachePolicy = MbCachePolicy.BYPASS,
    ) -> tuple[list[dict[str, Any]], int, MbSourceContext | None]:
        source_context = source_context or capture_mb_source_context()
        result = await mb_api_get(
            "/release-group",
            params={"artist": artist_mbid, "limit": fetch_limit, "offset": offset},
            priority=priority,
            decode_type=_ArtistReleaseGroupsPayload,
            source_context=source_context,
            cache_policy=cache_policy,
        )
        return (
            result.release_groups,
            int(result.release_group_count),
            get_mb_response_context() or source_context,
        )

    async def _get_artist_release_groups_or_raise_with_context(
        self,
        artist_mbid: str,
        offset: int = 0,
        limit: int = 50,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
        *,
        preserve_fetch_width: bool = False,
        source_context: MbSourceContext | None = None,
        cache_policy: MbCachePolicy = MbCachePolicy.BYPASS,
    ) -> tuple[list[dict[str, Any]], int, MbSourceContext | None]:
        """Fetch only the requested page; complete inventory callers bypass L2."""
        artist_mbid = normalize_mb_id(artist_mbid)
        source_context = source_context or capture_mb_source_context()
        if not is_valid_mbid(artist_mbid):
            _record_mb_degradation("artist invalid mbid", deterministic=True)
            return [], 0, source_context
        # Pre-resolution orphans old-MBID cache entries until TTL (bounded,
        # acceptable); everything below keys off the rewritten MBID.
        artist_mbid = await resolve_redirect_mbid(
            "artist", artist_mbid, source_context, self._cache,
            getattr(self, "_mb_canonical_store", None),
        )
        fetch_limit = min(100, max(1, limit))
        if cache_policy is MbCachePolicy.BYPASS:
            # BYPASS callers (e.g. the follow poller) never read or publish
            # cached pages: detection freshness wins over reuse.
            release_groups, total_count, response_context = await self._fetch_artist_release_groups_page(
                artist_mbid, offset, fetch_limit, priority,
                source_context=source_context, cache_policy=cache_policy,
            )
            if preserve_fetch_width:
                return release_groups, total_count, response_context
            return release_groups[:limit], total_count, response_context
        cache_token = capture_mb_cache_token(self._cache)
        cache_key = mb_artist_rgs_page_key(artist_mbid, fetch_limit, offset)
        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            # Stored value is the module-private (items, total) tuple; the
            # context is always the caller's, never None (_fetch_artist_by_id
            # drops browse data on a None/mismatched context).
            items, total = cached
            if preserve_fetch_width:
                return items, total, source_context
            return items[:limit], total, source_context
        dedupe_key = (
            f"{cache_key}:priority={int(priority)}:g{source_context.generation}"
        )
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_artist_rgs_page_cached(
                artist_mbid, offset, fetch_limit, limit, priority,
                cache_key, source_context, cache_policy,
                cache_token=cache_token,
                preserve_fetch_width=preserve_fetch_width,
            ),
        )

    async def _fetch_artist_rgs_page_cached(
        self,
        artist_mbid: str,
        offset: int,
        fetch_limit: int,
        limit: int,
        priority: RequestPriority,
        cache_key: str,
        source_context: MbSourceContext,
        cache_policy: MbCachePolicy,
        *,
        cache_token: tuple[object, int],
        preserve_fetch_width: bool = False,
    ) -> tuple[list[dict[str, Any]], int, MbSourceContext | None]:
        try:
            (
                release_groups,
                total_count,
                response_context,
            ) = await self._fetch_artist_release_groups_page(
                artist_mbid, offset, fetch_limit, priority,
                source_context=source_context, cache_policy=cache_policy,
            )
            if (
                not is_mb_source_current(source_context)
                or response_context is not None
                and response_context != source_context
            ):
                raise ExternalServiceError(
                    "MusicBrainz source changed during release-group browse"
                )
        except Exception as e:  # noqa: BLE001 - telemetry then re-raise (page cache)
            # Failures are never cached; propagation keeps them distinct from
            # "genuinely zero RGs" (QW1). Two-level recording is intentional:
            # page-leg signal under the QW1 aggregate. Stale-source raises
            # are control flow, not provider failure: they propagate silently
            # while genuine failures still record.
            if is_mb_source_current(source_context):
                logger.error("MusicBrainz artist release-group page fetch failed")
                _record_mb_degradation("release groups page failed")
            raise
        publication_context = response_context or source_context
        published = await mb_cache_set_if_current(self._cache,
        cache_key,
        (release_groups, total_count),
        ttl_seconds=600 if (total_count == 0 and not release_groups) else 3600,
        context=publication_context, cache_token=cache_token)
        if not published:
            logger.debug(
                "Release-group page cache publication fenced for %s; "
                "returning the original caller result",
                artist_mbid,
            )
        if preserve_fetch_width:
            return release_groups, total_count, response_context
        return release_groups[:limit], total_count, response_context

    async def get_artist_release_groups_with_context(
        self,
        artist_mbid: str,
        offset: int = 0,
        limit: int = 50,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
        *,
        preserve_fetch_width: bool = False,
        source_context: MbSourceContext | None = None,
        cache_policy: MbCachePolicy = MbCachePolicy.BYPASS,
    ) -> tuple[list[dict[str, Any]], int, MbSourceContext | None]:
        return await self._get_artist_release_groups_or_raise_with_context(
            artist_mbid,
            offset,
            limit,
            priority,
            preserve_fetch_width=preserve_fetch_width,
            source_context=source_context,
            cache_policy=cache_policy,
        )

    async def get_artist_release_groups(
        self,
        artist_mbid: str,
        offset: int = 0,
        limit: int = 50,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
    ) -> tuple[list[dict[str, Any]], int]:
        artist_mbid = normalize_mb_id(artist_mbid)
        source_context = capture_mb_source_context()
        try:
            (
                release_groups,
                total_count,
                response_context,
            ) = await self._get_artist_release_groups_or_raise_with_context(
                artist_mbid,
                offset,
                limit,
                priority,
                source_context=source_context,
            )
            if (
                not is_mb_source_current(source_context)
                or response_context is not None
                and response_context != source_context
            ):
                return [], 0
            return release_groups, total_count
        except Exception as e:  # noqa: BLE001
            logger.error("MusicBrainz artist release-group fetch failed")
            _record_mb_degradation("release groups failed")
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
        source_context = capture_mb_source_context()
        (
            release_groups,
            total_count,
            response_context,
        ) = await self._get_artist_release_groups_or_raise_with_context(
            artist_mbid,
            offset,
            limit,
            priority,
            source_context=source_context,
        )
        if (
            not is_mb_source_current(source_context)
            or response_context is not None
            and response_context != source_context
        ):
            raise ExternalServiceError(
                "MusicBrainz source changed during release-group browse"
            )
        return release_groups, total_count

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
        if not is_valid_mbid(artist_mbid):
            _record_mb_degradation("artist invalid mbid", deterministic=True)
            return []
        source_context = capture_mb_source_context()
        # Pre-resolution orphans old-MBID cache entries until TTL (bounded,
        # acceptable); everything below keys off the rewritten MBID.
        artist_mbid = await resolve_redirect_mbid(
            "artist", artist_mbid, source_context, self._cache,
            getattr(self, "_mb_canonical_store", None),
        )
        cache_token = capture_mb_cache_token(self._cache)
        cache_key = mb_artist_rgs_browse_key(artist_mbid, limit)

        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached

        dedupe_key = (
            f"{cache_key}:priority={int(priority)}:g{source_context.generation}"
        )
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_browse_release_groups(artist_mbid, limit, priority, cache_key, source_context, cache_token=cache_token),
        )

    async def _fetch_browse_release_groups(
        self,
        artist_mbid: str,
        limit: int,
        priority: RequestPriority,
        cache_key: str,
        source_context: MbSourceContext,
        cache_token: tuple[object, int],
    ) -> list[dict[str, Any]]:
        try:
            (
                release_groups,
                _total,
                response_context,
            ) = await self._get_artist_release_groups_or_raise_with_context(
                artist_mbid,
                offset=0,
                limit=limit,
                priority=priority,
                source_context=source_context,
                cache_policy=MbCachePolicy.DISPLAY_FRESH,
            )
            if (
                not is_mb_source_current(source_context)
                or response_context is not None
                and response_context != source_context
            ):
                raise ExternalServiceError(
                    "MusicBrainz source changed during release-group browse"
                )
        except Exception as e:  # noqa: BLE001 - telemetry then re-raise (QW1)
            # Same message site as the old swallowing variant; propagation is
            # what keeps failures distinguishable from "genuinely zero RGs".
            # Two-level recording is intentional: aggregate signal over the page-leg record.
            logger.error("MusicBrainz artist release-group fetch failed")
            _record_mb_degradation("release groups failed")
            raise
        publication_context = response_context or source_context
        published = await mb_cache_set_if_current(self._cache,
        cache_key,
        release_groups,
        ttl_seconds=3600 if release_groups else 600,
        context=publication_context, cache_token=cache_token)
        if not published:
            logger.debug(
                "Release-group browse cache publication fenced for %s; "
                "returning the original caller result",
                artist_mbid,
            )
        return release_groups
