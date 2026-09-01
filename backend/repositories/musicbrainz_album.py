import asyncio
import hashlib
import logging
from typing import Any

import httpx
import msgspec

from core.exceptions import (
    ConfigurationError,
    ExternalServiceError,
    InvalidExternalPayloadError,
)
from models.album import AlbumInfo
from models.search import SearchResult
from services.preferences_service import PreferencesService
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.cache_keys import (
    mb_album_search_key,
    mb_recording_canonical_id_key,
    mb_release_group_key,
    mb_release_key,
    mb_management_release_key,
    mb_release_edition_search_key,
    MB_RG_BY_TAG_PREFIX,
    MB_RG_DETAIL_PREFIX,
    MB_RELEASE_DETAIL_PREFIX,
    MB_RELEASE_TO_RG_PREFIX,
    MB_RELEASE_REC_PREFIX,
    MB_RECORDING_PREFIX,
    MB_RECORDING_SEARCH_PREFIX,
    MB_RECORDING_TO_RG_PREFIX,
    MB_URL_RESOLUTION_PREFIX,
    MB_RELEASE_VERIFY_PREFIX,
    MB_DUPLICATE_SEARCH_PREFIX,
)
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.resilience.retry import CircuitOpenError
from models.musicbrainz import recording_release_group_rank
from repositories.musicbrainz_base import (
    MbSourceContext,
    build_recording_search_query,
    build_release_group_search_query,
    build_release_search_query,
    mb_api_get,
    mb_cache_get_if_current,
    mb_cache_set_if_current,
    mb_deduplicator,
    clear_mb_response_context,
    capture_mb_source_context,
    dedupe_by_id,
    escape_lucene_phrase,
    get_mb_response_context,
    is_mb_source_current,
    normalize_mb_id,
    get_score,
    should_include_release,
    extract_artist_name,
    parse_year,
    build_musicbrainz_tag_query,
)
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from models.library_contribution import (
    MusicBrainzDuplicateFacts,
    MusicBrainzUrlResolution,
    MusicBrainzVerifiedRelease,
    MusicBrainzVerifiedTrack,
)
from models.identification import ReleaseEdition, ReleaseEditionSearchPage
from repositories.musicbrainz_contribution_models import (
    MbContributionRelease,
    MbContributionReleaseSearch,
    MbContributionUrl,
)
from repositories.musicbrainz_management_models import (
    MbManagementRecording,
    MbManagementRelease,
)
from repositories.musicbrainz_release_search_models import MbReleaseSearchResponse

logger = logging.getLogger(__name__)


def _record_mb_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx:
        ctx.record(IntegrationResult.error(source="musicbrainz", msg=msg))


def _record_mb_unmappable_payload(method: str) -> None:
    """Record a deterministic payload-shape failure for candidate recall.

    Recall methods return empty results on failure; this marker lets the
    identification service classify the empty recall as
    ``UNMAPPABLE_PROVIDER_PAYLOAD`` instead of a transient provider outage
    (F-IDENT-02). The shared breaker stays closed either way.
    """
    ctx = try_get_degradation_context()
    if ctx:
        ctx.record(
            IntegrationResult.deterministic_error(
                source="musicbrainz",
                msg=f"MusicBrainz {method} returned an unmappable payload.",
            )
        )


def _raise_unmappable_payload(
    exc: InvalidExternalPayloadError, method: str
) -> InvalidExternalPayloadError:
    """Record a deterministic MusicBrainz payload-shape failure and re-raise it.

    `InvalidExternalPayloadError` is deliberately non-breaking and non-retriable at
    `mb_api_get`, so this keeps the shared breaker closed and never caches the failed
    result. It stays a typed error so contribution callers can separate a deterministic
    payload problem from a genuine provider outage.
    """
    _record_mb_degradation(
        f"MusicBrainz {method} returned an unmappable payload: {exc}"
    )
    return exc


class _ReleaseGroupSearchPayload(msgspec.Struct):
    release_groups: list[dict[str, Any]] = msgspec.field(
        name="release-groups", default_factory=list
    )


class _ReleaseLookupPayload(msgspec.Struct):
    release_group: dict[str, Any] = msgspec.field(
        name="release-group", default_factory=dict
    )
    media: list[dict[str, Any]] = msgspec.field(default_factory=list)


class _RecordingReleaseGroupPayload(msgspec.Struct):
    # A recording lookup with inc=releases+release-groups returns a `releases`
    # list, each release carrying its `release-group` sub-object.
    releases: list[dict[str, Any]] = msgspec.field(default_factory=list)


class _RecordingSearchPayload(msgspec.Struct):
    recordings: list[dict[str, Any]] = msgspec.field(default_factory=list)


class RecordingReleaseGroup(msgspec.Struct):
    """A release group a searched recording appears on."""

    release_group_mbid: str
    release_group_title: str
    release_mbid: str | None
    primary_type: str | None
    secondary_types: tuple[str, ...]
    release_status: str | None = None
    release_date: str | None = None


class RecordingMatch(msgspec.Struct):
    """A recording-search candidate with the release groups it belongs to."""

    recording_mbid: str
    title: str
    artist: str | None
    score: int
    release_groups: list[RecordingReleaseGroup]


def _release_rank(release: dict, rg: dict) -> tuple[int, int, int, int, str, str]:
    return recording_release_group_rank(
        release_status=release.get("status"),
        secondary_types=rg.get("secondary-types") or (),
        primary_type=rg.get("primary-type"),
        release_date=release.get("date") or rg.get("first-release-date"),
        release_group_mbid=rg.get("id", ""),
    )


def _pick_best_release_group(releases: list[dict]) -> tuple[str, str] | None:
    candidates: dict[str, tuple[str, str, tuple[int, int, int, int, str, str]]] = {}
    for release in releases:
        rg = release.get("release-group", {})
        rg_id = rg.get("id")
        rg_title = rg.get("title", "")
        if rg_id:
            normalized_rg_id = normalize_mb_id(rg_id)
            candidate = (rg_id, rg_title, _release_rank(release, rg))
            current = candidates.get(normalized_rg_id)
            if current is None or candidate[2] < current[2]:
                candidates[normalized_rg_id] = candidate
    if not candidates:
        return None
    _normalized_id, (rg_id, title, _rank) = min(
        candidates.items(), key=lambda item: item[1][2]
    )
    return (rg_id, title)


def _verified_release(
    release: MbContributionRelease,
) -> MusicBrainzVerifiedRelease | None:
    if not release.id or not release.release_group.id or not release.title:
        return None
    artist_name = "".join(
        (credit.name or credit.artist.name) + credit.joinphrase
        for credit in release.artist_credit
    ).strip()
    unique_artist_ids = {
        credit.artist.id for credit in release.artist_credit if credit.artist.id
    }
    first_label = release.label_info[0] if release.label_info else None
    return MusicBrainzVerifiedRelease(
        release_mbid=release.id,
        release_group_mbid=release.release_group.id,
        title=release.title,
        artist_name=artist_name,
        artist_mbid=(
            next(iter(unique_artist_ids)) if len(unique_artist_ids) == 1 else None
        ),
        date=release.date or None,
        country=release.country or None,
        status=release.status or None,
        packaging=release.packaging or None,
        barcode=release.barcode or None,
        label=(
            first_label.label.name
            if first_label is not None and first_label.label is not None
            else None
        ),
        catalogue_number=(
            first_label.catalog_number if first_label is not None else None
        )
        or None,
        tracks=[
            MusicBrainzVerifiedTrack(
                title=track.title or track.recording.title,
                position=track.position,
                disc_number=medium.position,
                duration_seconds=(
                    track.length / 1000 if track.length is not None else None
                ),
                recording_mbid=track.recording.id or None,
                release_track_mbid=track.id or None,
            )
            for medium in release.media
            for track in medium.tracks
            if track.title or track.recording.title
        ],
    )


def _url_cache_key(resource_url: str, includes: tuple[str, ...]) -> str:
    digest = hashlib.sha256(resource_url.encode()).hexdigest()
    return f"{MB_URL_RESOLUTION_PREFIX}{digest}:{'+'.join(sorted(includes))}"


def _duplicate_search_key(facts: MusicBrainzDuplicateFacts, limit: int) -> str:
    material = "|".join(
        (
            facts.title,
            facts.artist_name,
            facts.barcode or "",
            facts.country or "",
            facts.date or "",
            str(limit),
        )
    )
    return (
        f"{MB_DUPLICATE_SEARCH_PREFIX}{hashlib.sha256(material.encode()).hexdigest()}"
    )


class MusicBrainzAlbumMixin:
    _cache: CacheInterface
    _preferences_service: PreferencesService

    async def search_release_editions(
        self,
        title: str,
        artist: str,
        *,
        limit: int = 12,
        offset: int = 0,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> ReleaseEditionSearchPage:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        normalized_title = " ".join(title.split())
        normalized_artist = " ".join(artist.split())
        normalized_limit = max(1, min(limit, 12))
        normalized_offset = max(0, offset)
        cache_key = mb_release_edition_search_key(
            normalized_title,
            normalized_artist,
            normalized_limit,
            normalized_offset,
        )
        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if isinstance(cached, ReleaseEditionSearchPage):
            return cached

        async def load() -> ReleaseEditionSearchPage:
            try:
                payload = await mb_api_get(
                    "/release",
                    params={
                        "query": build_release_search_query(
                            normalized_title, normalized_artist
                        ),
                        "limit": normalized_limit,
                        "offset": normalized_offset,
                    },
                    priority=priority,
                    decode_type=MbReleaseSearchResponse,
                    source_context=source_context,
                )
                response_context = get_mb_response_context() or source_context
            except (httpx.HTTPError, CircuitOpenError, ExternalServiceError) as error:
                raise ExternalServiceError(
                    "MusicBrainz release search is temporarily unavailable."
                ) from error

            items: list[ReleaseEdition] = []
            for release in payload.releases:
                if not release.id or not release.release_group.id:
                    continue
                artist_name = "".join(
                    (credit.name or credit.artist.name) + credit.joinphrase
                    for credit in release.artist_credit
                ).strip()
                label_info = release.label_info[0] if release.label_info else None
                formats = list(
                    dict.fromkeys(
                        medium.format for medium in release.media if medium.format
                    )
                )
                items.append(
                    ReleaseEdition(
                        release_mbid=release.id,
                        release_group_mbid=release.release_group.id,
                        artist_name=artist_name,
                        title=release.title,
                        date=release.date or None,
                        country=release.country or None,
                        status=release.status or None,
                        packaging=release.packaging or None,
                        media_formats=formats,
                        disc_count=len(release.media),
                        track_count=sum(medium.track_count for medium in release.media),
                        label=(
                            label_info.label.name
                            if label_info is not None and label_info.label is not None
                            else None
                        ),
                        catalogue_number=(
                            label_info.catalog_number
                            if label_info is not None
                            else None
                        )
                        or None,
                        barcode=release.barcode or None,
                        disambiguation=release.disambiguation or None,
                        musicbrainz_url=f"https://musicbrainz.org/release/{release.id}",
                        score=release.score,
                    )
                )
            page = ReleaseEditionSearchPage(
                items=items,
                total=payload.count,
                offset=payload.offset,
                limit=normalized_limit,
            )
            ttl = self._preferences_service.get_advanced_settings().cache_ttl_search
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                page,
                ttl_seconds=ttl,
                context=response_context,
            )
            return page

        dedupe_key = f"{cache_key}:priority={int(priority)}"
        return await mb_deduplicator.dedupe(dedupe_key, load)

    def _map_release_group_to_result(
        self,
        rg: dict[str, Any],
        included_secondary_types: set[str] | None = None,
        include_all_types: bool = False,
        included_primary_types: set[str] | None = None,
    ) -> SearchResult | None:
        if not include_all_types and not should_include_release(
            rg, included_secondary_types, included_primary_types
        ):
            return None

        primary_type = rg.get("primary-type", "")
        secondary_types = rg.get("secondary-types", [])
        if secondary_types:
            type_info = f"{primary_type} + {', '.join(secondary_types)}"
        else:
            type_info = primary_type or None

        return SearchResult(
            type="album",
            title=rg.get("title", "Unknown Album"),
            artist=extract_artist_name(rg),
            year=parse_year(rg.get("first-release-date")),
            musicbrainz_id=rg.get("id", ""),
            in_library=False,
            type_info=type_info,
            disambiguation=rg.get("disambiguation") or None,
            score=get_score(rg),
        )

    async def search_albums(
        self,
        query: str,
        limit: int = 10,
        offset: int = 0,
        included_secondary_types: set[str] | None = None,
        include_all_types: bool = False,
        included_primary_types: set[str] | None = None,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> list[SearchResult]:
        cache_key = mb_album_search_key(
            query, limit, offset, included_secondary_types, included_primary_types
        )
        provider_query = f'releasegroup:"{query}"^3 OR release:"{query}"^2 OR {query}'
        return await self._search_release_groups(
            provider_query=provider_query,
            cache_key=cache_key,
            limit=limit,
            offset=offset,
            included_secondary_types=included_secondary_types,
            include_all_types=include_all_types,
            included_primary_types=included_primary_types,
            priority=priority,
        )

    async def search_release_groups(
        self,
        artist: str,
        title: str,
        limit: int = 10,
        offset: int = 0,
        included_secondary_types: set[str] | None = None,
        include_all_types: bool = False,
        included_primary_types: set[str] | None = None,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> list[SearchResult]:
        normalized_artist = " ".join(artist.split())
        normalized_title = " ".join(title.split())
        cache_key = mb_album_search_key(
            f"structured:{normalized_artist}\x00{normalized_title}",
            limit,
            offset,
            included_secondary_types,
            included_primary_types,
        )
        return await self._search_release_groups(
            provider_query=build_release_group_search_query(
                normalized_title, normalized_artist
            ),
            cache_key=cache_key,
            limit=limit,
            offset=offset,
            included_secondary_types=included_secondary_types,
            include_all_types=include_all_types,
            included_primary_types=included_primary_types,
            priority=priority,
        )

    async def _search_release_groups(
        self,
        *,
        provider_query: str,
        cache_key: str,
        limit: int,
        offset: int,
        included_secondary_types: set[str] | None,
        include_all_types: bool,
        included_primary_types: set[str] | None,
        priority: RequestPriority,
    ) -> list[SearchResult]:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        if include_all_types:
            cache_key = f"{cache_key}:all"

        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached

        async def load() -> list[SearchResult]:
            try:
                internal_limit = min(100, max(int(limit * 1.5), 25))

                result = await mb_api_get(
                    "/release-group",
                    params={
                        "query": provider_query,
                        "limit": internal_limit,
                        "offset": offset,
                    },
                    priority=priority,
                    decode_type=_ReleaseGroupSearchPayload,
                    source_context=source_context,
                )
                response_context = get_mb_response_context() or source_context
                release_groups = dedupe_by_id(result.release_groups)

                results = []
                for rg in release_groups:
                    mapped = self._map_release_group_to_result(
                        rg,
                        included_secondary_types,
                        include_all_types,
                        included_primary_types,
                    )
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
            except InvalidExternalPayloadError:
                _record_mb_unmappable_payload("release-group search")
                return []
            except Exception as e:  # noqa: BLE001
                # CircuitOpenError is expected while the breaker is open; the
                # degradation record is the signal, an error log per call is spam.
                if not isinstance(e, CircuitOpenError):
                    logger.error("MusicBrainz album search failed")
                _record_mb_degradation("album search failed")
                return []

        dedupe_key = f"{cache_key}:priority={int(priority)}"
        return await mb_deduplicator.dedupe(dedupe_key, load)

    async def search_release_groups_by_tag(
        self,
        tag: str,
        limit: int = 50,
        offset: int = 0,
        included_secondary_types: set[str] | None = None,
    ) -> list[SearchResult]:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        type_policy = (
            "<default>"
            if included_secondary_types is None
            else repr(tuple(sorted(str(value) for value in included_secondary_types)))
        )
        cache_key = (
            f"{MB_RG_BY_TAG_PREFIX}{tag.lower()}:{limit}:{offset}:"
            f"secondary={type_policy}"
        )

        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached

        async def load() -> list[SearchResult]:
            try:
                internal_limit = min(100, max(int(limit * 1.5), 25))

                result = await mb_api_get(
                    "/release-group",
                    params={
                        "query": build_musicbrainz_tag_query(tag),
                        "limit": internal_limit,
                        "offset": offset,
                    },
                    priority=RequestPriority.BACKGROUND_SYNC,
                    decode_type=_ReleaseGroupSearchPayload,
                    source_context=source_context,
                )
                response_context = get_mb_response_context() or source_context
                release_groups = dedupe_by_id(result.release_groups)

                results = []
                for rg in release_groups:
                    mapped = self._map_release_group_to_result(
                        rg, included_secondary_types
                    )
                    if mapped:
                        results.append(mapped)
                    if len(results) >= limit:
                        break

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
                if not isinstance(e, CircuitOpenError):
                    logger.error("MusicBrainz release group tag search failed")
                _record_mb_degradation("release group tag search failed")
                return []

        return await mb_deduplicator.dedupe(cache_key, load)

    async def get_release_group_by_id(
        self,
        mbid: str,
        includes: list[str] | None = None,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        *,
        source_context: MbSourceContext | None = None,
    ) -> dict | None:
        clear_mb_response_context()
        source_context = source_context or capture_mb_source_context()
        if includes is None:
            includes = ["artist-credits", "releases"]
        mbid = normalize_mb_id(mbid)
        includes_str = "+".join(sorted(includes))
        cache_key = mb_release_group_key(mbid, includes)

        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return None if cached == {} else cached

        dedupe_key = (
            f"{MB_RG_DETAIL_PREFIX}{mbid}:{includes_str}:priority={int(priority)}"
            f":g{source_context.generation}"
        )
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_release_group_by_id(
                mbid, includes, cache_key, priority, source_context=source_context
            ),
        )

    async def _fetch_release_group_by_id(
        self,
        mbid: str,
        includes: list[str],
        cache_key: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        *,
        source_context: MbSourceContext | None = None,
    ) -> dict | None:
        source_context = source_context or capture_mb_source_context()
        mbid = normalize_mb_id(mbid)
        try:
            inc_str = "+".join(sorted(includes))
            result = await mb_api_get(
                f"/release-group/{mbid}",
                params={"inc": inc_str},
                priority=priority,
                source_context=source_context,
            )
            response_context = get_mb_response_context() or source_context
            if not result:
                # Definitive miss (mb_api_get returns {} only on HTTP 404 - 503/5xx
                # raise instead and land in the except below). Negative-cache it briefly
                # so a merged/deleted/passthrough-garbage RG mbid isn't re-fetched on every
                # discover build. Short TTL bounds staleness and self-heals; the except
                # (transient) path stays uncached on purpose.
                await mb_cache_set_if_current(
                    self._cache,
                    cache_key,
                    {},
                    ttl_seconds=600,
                    context=response_context,
                )
                return None
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                result,
                ttl_seconds=3600,
                context=response_context,
            )
            return result
        except InvalidExternalPayloadError:
            _record_mb_unmappable_payload("release-group fetch")
            return None
        except Exception as e:  # noqa: BLE001
            if not isinstance(e, CircuitOpenError):
                logger.error("MusicBrainz release group fetch failed")
            _record_mb_degradation("release group fetch failed")
            return None

    async def get_release_group(self, release_group_mbid: str) -> AlbumInfo | None:
        """Fetch a release group and map it to ``AlbumInfo`` (the
        MusicBrainzRepository protocol method). Used to backfill an album's
        year/title/artist when a request omits them."""
        rg = await self.get_release_group_by_id(release_group_mbid)
        if not rg:
            return None
        credit = (rg.get("artist-credit") or [{}])[0]
        artist = credit.get("artist") if isinstance(credit, dict) else None
        return AlbumInfo(
            title=rg.get("title") or "Unknown Album",
            musicbrainz_id=rg.get("id") or release_group_mbid,
            artist_name=extract_artist_name(rg) or "Unknown Artist",
            artist_id=(artist.get("id") if isinstance(artist, dict) else "") or "",
            release_date=rg.get("first-release-date"),
            year=parse_year(rg.get("first-release-date")),
            type=rg.get("primary-type"),
        )

    async def get_release_by_id(
        self,
        release_id: str,
        includes: list[str] | None = None,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> dict | None:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        if includes is None:
            includes = ["recordings", "labels"]
        release_id = normalize_mb_id(release_id)
        cache_key = mb_release_key(release_id, includes)

        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached

        includes_str = "+".join(sorted(includes))
        dedupe_key = (
            f"{MB_RELEASE_DETAIL_PREFIX}{release_id}:{includes_str}:"
            f"priority={int(priority)}"
        )
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_release_by_id(
                release_id, includes, cache_key, priority, source_context=source_context
            ),
        )

    async def get_canonical_release(
        self,
        release_mbid: str,
        *,
        includes: tuple[str, ...],
        preferred_locales: tuple[str, ...] = (),
        artist_standardization: str = "credited",
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        bypass_cache: bool = False,
        source_context: MbSourceContext | None = None,
    ) -> MbManagementRelease | None:
        """Fetch the selected edition for authoritative Library Management.

        Locale and artist-standardization inputs are part of the cache identity because
        downstream canonical projection depends on them, even though MusicBrainz sends
        all requested alias data in the same response.
        """
        clear_mb_response_context()
        source_context = source_context or capture_mb_source_context()
        release_mbid = normalize_mb_id(release_mbid)
        normalized_includes = tuple(sorted(set(includes)))
        cache_key = mb_management_release_key(
            release_mbid,
            normalized_includes,
            preferred_locales,
            artist_standardization,
        )
        if not bypass_cache:
            cached = await mb_cache_get_if_current(
                self._cache, cache_key, source_context
            )
            if isinstance(cached, MbManagementRelease):
                return cached
            if cached is False:
                return None

        async def load() -> MbManagementRelease | None:
            try:
                result = await mb_api_get(
                    f"/release/{release_mbid}",
                    params={"inc": "+".join(normalized_includes)},
                    priority=priority,
                    decode_type=MbManagementRelease,
                    source_context=source_context,
                )
                response_context = get_mb_response_context() or source_context
            except (httpx.HTTPError, CircuitOpenError, ExternalServiceError) as error:
                _record_mb_degradation("canonical release fetch unavailable")
                raise ExternalServiceError(
                    "MusicBrainz canonical metadata is temporarily unavailable."
                ) from error

            if not result.id:
                await mb_cache_set_if_current(
                    self._cache,
                    cache_key,
                    False,
                    ttl_seconds=600,
                    context=response_context,
                )
                return None
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                result,
                ttl_seconds=3600,
                context=response_context,
            )
            return result

        dedupe_key = f"{cache_key}:fresh" if bypass_cache else cache_key
        dedupe_key = f"{dedupe_key}:priority={int(priority)}"
        return await mb_deduplicator.dedupe(dedupe_key, load)

    async def resolve_recording_mbid(
        self,
        recording_mbid: str,
        *,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> str | None:
        """Resolve a recording MBID through MusicBrainz merge redirects."""
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        recording_mbid = normalize_mb_id(recording_mbid)
        cache_key = mb_recording_canonical_id_key(recording_mbid)
        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if isinstance(cached, str):
            return cached
        if cached is False:
            return None

        # ST2 P1 read-tier: durable canonical_redirect (identity lane ->
        # official-source-only gate). Store errors log-and-fall-through.
        canonical_store = getattr(self, "_mb_canonical_store", None)
        if canonical_store is not None:
            try:
                persisted = await canonical_store.get_canonical_redirect(
                    "recording",
                    [recording_mbid],
                    source_context=source_context,
                    trusted_identity_source_only=True,
                )
                if not is_mb_source_current(source_context):
                    raise ConfigurationError(
                        "MusicBrainz source changed during canonical read"
                    )
                if recording_mbid.casefold() in persisted:
                    resolved_id = persisted[recording_mbid.casefold()]
                    await mb_cache_set_if_current(
                        self._cache,
                        cache_key,
                        resolved_id,
                        ttl_seconds=3600,
                        context=source_context,
                    )
                    return resolved_id
            except Exception:  # noqa: BLE001 - store miss falls to wire
                logger.warning("MusicBrainz canonical redirect store read failed")

        async def load() -> str | None:
            operation_context = source_context
            try:
                result = await mb_api_get(
                    f"/recording/{recording_mbid}",
                    priority=priority,
                    decode_type=MbManagementRecording,
                    source_context=operation_context,
                )
            except (httpx.HTTPError, CircuitOpenError, ExternalServiceError) as error:
                _record_mb_degradation("recording identity resolution unavailable")
                raise ExternalServiceError(
                    "MusicBrainz recording identity is temporarily unavailable."
                ) from error
            response_context = get_mb_response_context() or operation_context
            if not result.id:
                # 600 s miss sentinel STAYS memory-only - do not durably bank
                # absence (a 404 today may resolve after later edits).
                await mb_cache_set_if_current(
                    self._cache,
                    cache_key,
                    False,
                    ttl_seconds=600,
                    context=response_context,
                )
                return None
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                result.id,
                ttl_seconds=3600,
                context=response_context,
            )
            # ST2 P1 write-through: bank the redirect durably under the same
            if canonical_store is not None and operation_context is not None:
                try:
                    await canonical_store.save_canonical_redirect(
                        [
                            {
                                "entity_kind": "recording",
                                "from_mbid": recording_mbid,
                                "to_mbid": result.id,
                            }
                        ],
                        source_context=operation_context,
                    )
                except Exception:  # noqa: BLE001
                    logger.warning("MusicBrainz canonical redirect persistence failed")
            return result.id

        dedupe_key = f"{cache_key}:priority={int(priority)}"
        return await mb_deduplicator.dedupe(dedupe_key, load)

    async def resolve_url(
        self,
        resource_url: str,
        *,
        includes: tuple[str, ...],
        priority: RequestPriority,
        bypass_cache: bool = False,
    ) -> MusicBrainzUrlResolution:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        cache_key = _url_cache_key(resource_url, includes)
        if not bypass_cache:
            cached = await mb_cache_get_if_current(
                self._cache, cache_key, source_context
            )
            if isinstance(cached, MusicBrainzUrlResolution):
                return cached

        async def load() -> MusicBrainzUrlResolution:
            try:
                result = await mb_api_get(
                    "/url",
                    params={
                        "resource": resource_url,
                        "inc": "+".join(sorted(includes)),
                    },
                    priority=priority,
                    decode_type=MbContributionUrl,
                    source_context=source_context,
                )
                response_context = get_mb_response_context() or source_context
            except InvalidExternalPayloadError as exc:
                raise _raise_unmappable_payload(exc, "url resolution") from exc
            except (httpx.HTTPError, CircuitOpenError) as error:
                raise ExternalServiceError(
                    "MusicBrainz URL resolution is temporarily unavailable."
                ) from error
            resolution = MusicBrainzUrlResolution(
                resource_url=resource_url,
                release_mbids=list(
                    dict.fromkeys(
                        relation.release.id
                        for relation in result.relations
                        if relation.release is not None and relation.release.id
                    )
                ),
                release_group_mbids=list(
                    dict.fromkeys(
                        relation.release_group.id
                        for relation in result.relations
                        if relation.release_group is not None
                        and relation.release_group.id
                    )
                ),
                artist_mbids=list(
                    dict.fromkeys(
                        relation.artist.id
                        for relation in result.relations
                        if relation.artist is not None and relation.artist.id
                    )
                ),
                label_mbids=list(
                    dict.fromkeys(
                        relation.label.id
                        for relation in result.relations
                        if relation.label is not None and relation.label.id
                    )
                ),
            )
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                resolution,
                ttl_seconds=3600,
                context=response_context,
            )
            return resolution

        dedupe_key = f"{cache_key}:fresh" if bypass_cache else cache_key
        dedupe_key = f"{dedupe_key}:priority={int(priority)}"
        return await mb_deduplicator.dedupe(dedupe_key, load)

    async def get_release_for_verification(
        self,
        release_mbid: str,
        *,
        priority: RequestPriority,
        bypass_cache: bool = False,
    ) -> MusicBrainzVerifiedRelease | None:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        release_mbid = normalize_mb_id(release_mbid)
        cache_key = f"{MB_RELEASE_VERIFY_PREFIX}{release_mbid}"
        if not bypass_cache:
            cached = await mb_cache_get_if_current(
                self._cache, cache_key, source_context
            )
            if isinstance(cached, MusicBrainzVerifiedRelease):
                return cached

        async def load() -> MusicBrainzVerifiedRelease | None:
            try:
                result = await mb_api_get(
                    f"/release/{release_mbid}",
                    params={
                        "inc": "artist-credits+labels+recordings+release-groups+url-rels"
                    },
                    priority=priority,
                    decode_type=MbContributionRelease,
                    source_context=source_context,
                )
                response_context = get_mb_response_context() or source_context
            except InvalidExternalPayloadError as exc:
                raise _raise_unmappable_payload(exc, "release verification") from exc
            except (httpx.HTTPError, CircuitOpenError) as error:
                raise ExternalServiceError(
                    "MusicBrainz release verification is temporarily unavailable."
                ) from error
            normalized = _verified_release(result)
            if normalized is not None:
                await mb_cache_set_if_current(
                    self._cache,
                    cache_key,
                    normalized,
                    ttl_seconds=3600,
                    context=response_context,
                )
            return normalized

        dedupe_key = f"{cache_key}:fresh" if bypass_cache else cache_key
        dedupe_key = f"{dedupe_key}:priority={int(priority)}"
        return await mb_deduplicator.dedupe(dedupe_key, load)

    async def search_duplicate_releases(
        self,
        facts: MusicBrainzDuplicateFacts,
        *,
        priority: RequestPriority,
        limit: int,
    ) -> list[MusicBrainzVerifiedRelease]:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        bounded_limit = max(1, min(limit, 10))
        cache_key = _duplicate_search_key(facts, bounded_limit)
        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if isinstance(cached, list) and all(
            isinstance(item, MusicBrainzVerifiedRelease) for item in cached
        ):
            return cached

        clauses = [
            f'release:"{escape_lucene_phrase(facts.title)}"',
            f'artist:"{escape_lucene_phrase(facts.artist_name)}"',
        ]
        if facts.barcode:
            clauses.append(f'barcode:"{escape_lucene_phrase(facts.barcode)}"')
        query = " AND ".join(clauses[:2])
        if len(clauses) == 3:
            query = f"({query}) OR {clauses[2]}"

        async def load() -> list[MusicBrainzVerifiedRelease]:
            try:
                result = await mb_api_get(
                    "/release",
                    params={"query": query, "limit": bounded_limit},
                    priority=priority,
                    decode_type=MbContributionReleaseSearch,
                    source_context=source_context,
                )
                response_context = get_mb_response_context() or source_context
            except InvalidExternalPayloadError as exc:
                raise _raise_unmappable_payload(
                    exc, "duplicate release search"
                ) from exc
            except (httpx.HTTPError, CircuitOpenError) as error:
                raise ExternalServiceError(
                    "MusicBrainz release search is temporarily unavailable."
                ) from error
            releases = [
                normalized
                for release in result.releases
                if (normalized := _verified_release(release)) is not None
            ]
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                releases,
                ttl_seconds=900,
                context=response_context,
            )
            return releases

        dedupe_key = f"{cache_key}:priority={int(priority)}"
        return await mb_deduplicator.dedupe(dedupe_key, load)

    async def _fetch_release_by_id(
        self,
        release_id: str,
        includes: list[str],
        cache_key: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        *,
        source_context: MbSourceContext | None = None,
    ) -> dict | None:
        source_context = source_context or capture_mb_source_context()
        release_id = normalize_mb_id(release_id)
        try:
            inc_str = "+".join(sorted(includes))
            result = await mb_api_get(
                f"/release/{release_id}",
                params={"inc": inc_str},
                priority=priority,
                source_context=source_context,
            )
            response_context = get_mb_response_context() or source_context
            if not result:
                return None
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                result,
                ttl_seconds=3600,
                context=response_context,
            )
            return result
        except InvalidExternalPayloadError:
            _record_mb_unmappable_payload("release fetch")
            return None
        except Exception as e:  # noqa: BLE001
            if not isinstance(e, CircuitOpenError):
                logger.error("MusicBrainz release fetch failed")
            _record_mb_degradation("release fetch failed")
            return None

    async def get_release_group_id_from_release(
        self,
        release_id: str,
        *,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
        source_context: MbSourceContext | None = None,
    ) -> str | None:
        clear_mb_response_context()
        source_context = source_context or capture_mb_source_context()
        release_id = normalize_mb_id(release_id)
        cache_key = f"{MB_RELEASE_TO_RG_PREFIX}{release_id}"
        # ST2 P1 read-tier: memory -> durable canonical map -> dedupe + wire.
        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached or None
        canonical_store = getattr(self, "_mb_canonical_store", None)
        if canonical_store is not None:
            # Keep the operation's original source context for the durable read.
            try:
                persisted = await canonical_store.get_release_to_rg_batch(
                    [release_id],
                    source_context=source_context,
                )
                if not is_mb_source_current(source_context):
                    raise ConfigurationError(
                        "MusicBrainz source changed during canonical read"
                    )
                rg_id = persisted.get(release_id.casefold())
                if rg_id is not None:
                    await mb_cache_set_if_current(
                        self._cache,
                        cache_key,
                        rg_id,
                        ttl_seconds=86400,
                        context=source_context,
                    )
                    return rg_id or None
            except Exception:  # noqa: BLE001
                logger.warning("MusicBrainz canonical release map read failed")
        dedupe_key = (
            f"{MB_RELEASE_TO_RG_PREFIX}{release_id}:priority={int(priority)}"
            f":g{source_context.generation}"
        )
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_release_group_id_from_release(
                release_id,
                cache_key,
                priority,
                source_context=source_context,
            ),
        )

    async def get_release_group_ids_batch(
        self, release_ids: list[str]
    ) -> dict[str, str | None]:
        """Resolve release IDs with one identity-normalized fan-out.

        Input keys are retained in the returned mapping for caller-visible
        compatibility, while case variants share one memory/durable lookup,
        dedupe key, and provider wire.
        """
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        resolved_normalized: dict[str, str | None] = {}
        aliases: dict[str, list[str]] = {}
        pending: list[str] = []
        seen: set[str] = set()
        for rid in release_ids:
            caller_id = str(rid)
            normalized_id = normalize_mb_id(caller_id)
            if not normalized_id:
                continue
            alias_list = aliases.setdefault(normalized_id, [])
            if caller_id not in alias_list:
                alias_list.append(caller_id)
            if normalized_id in seen:
                continue
            seen.add(normalized_id)
            cache_key = f"{MB_RELEASE_TO_RG_PREFIX}{normalized_id}"
            cached = await mb_cache_get_if_current(
                self._cache, cache_key, source_context
            )
            if cached is not None:
                resolved_normalized[normalized_id] = cached or None
            else:
                pending.append(normalized_id)

        canonical_store = getattr(self, "_mb_canonical_store", None)
        if pending and canonical_store is not None:
            # Keep the operation's original source context for the durable batch read.
            try:
                persisted = await canonical_store.get_release_to_rg_batch(
                    pending,
                    source_context=source_context,
                )
                if not is_mb_source_current(source_context):
                    raise ConfigurationError(
                        "MusicBrainz source changed during canonical read"
                    )
                still_pending: list[str] = []
                for normalized_id in pending:
                    if normalized_id in persisted:
                        rg_id = persisted[normalized_id]
                        await mb_cache_set_if_current(
                            self._cache,
                            f"{MB_RELEASE_TO_RG_PREFIX}{normalized_id}",
                            rg_id,
                            ttl_seconds=86400,
                            context=source_context,
                        )
                        resolved_normalized[normalized_id] = rg_id or None
                    else:
                        still_pending.append(normalized_id)
                pending = still_pending
            except Exception:  # noqa: BLE001 - store miss falls through to wire
                logger.warning("MusicBrainz canonical release map batch read failed")

        if pending:
            wire_results = await asyncio.gather(
                *[
                    self.get_release_group_id_from_release(
                        normalized_id, source_context=source_context
                    )
                    for normalized_id in pending
                ],
                return_exceptions=True,
            )
            for normalized_id, result in zip(pending, wire_results):
                resolved_normalized[normalized_id] = (
                    None if isinstance(result, BaseException) else result
                )

        if not is_mb_source_current(source_context):
            raise ConfigurationError(
                "MusicBrainz source changed during release-group batch resolution"
            )
        resolved: dict[str, str | None] = {}
        for normalized_id, raw_ids in aliases.items():
            value = resolved_normalized.get(normalized_id)
            for raw_id in raw_ids:
                resolved[raw_id] = value
        return resolved

    async def _fetch_release_group_id_from_release(
        self,
        release_id: str,
        cache_key: str,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
        *,
        source_context: MbSourceContext | None = None,
    ) -> str | None:
        source_context = source_context or capture_mb_source_context()
        operation_context = source_context
        canonical_store = getattr(self, "_mb_canonical_store", None)
        try:
            result = await mb_api_get(
                f"/release/{release_id}",
                params={"inc": "release-groups+recordings"},
                priority=priority,
                decode_type=_ReleaseLookupPayload,
                source_context=operation_context,
            )
            response_context = get_mb_response_context() or operation_context
            rg = result.release_group
            rg_id = rg.get("id")
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                rg_id or "",
                ttl_seconds=86400,
                context=response_context,
            )

            # ST2 P1 write-through: bank the answer durably ('' negative
            # included - a successfully decoded response that proves "no
            # release group" is authoritative) under the source commit fence.
            if canonical_store is not None and operation_context is not None:
                try:
                    await canonical_store.save_release_to_rg(
                        {release_id: rg_id or ""},
                        source_context=operation_context,
                    )
                except Exception:  # noqa: BLE001
                    logger.warning("MusicBrainz release-to-group persistence failed")

            positions: dict[str, list[int]] = {}
            for medium in result.media:
                disc = medium.get("position", 1)
                for track in medium.get("tracks", medium.get("track-list", [])):
                    rec = track.get("recording", {})
                    rec_id = normalize_mb_id(rec.get("id"))
                    trk_pos = track.get("position")
                    if rec_id and trk_pos is not None:
                        positions[rec_id] = [disc, trk_pos]
            if positions:
                pos_cache_key = f"{MB_RELEASE_REC_PREFIX}{release_id}"
                await mb_cache_set_if_current(
                    self._cache,
                    pos_cache_key,
                    positions,
                    ttl_seconds=86400,
                    context=response_context,
                )

            return rg_id
        except Exception as e:  # noqa: BLE001
            # F-MATCH-05: a transient provider failure must never become the
            # definitive empty-string negative cache entry. Only a successfully
            # decoded response that proves "no release group" may cache that;
            # here we record degradation and leave the key unset so the next
            # call can retry once deduplication and the breaker permit it.
            _record_mb_degradation("release-to-rg lookup failed")
            return None

    async def search_recordings(
        self,
        artist: str,
        title: str,
        limit: int = 8,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> list["RecordingMatch"]:
        """Search MusicBrainz recordings by track title + artist, returning each candidate with its release groups."""
        artist = " ".join(artist.split())
        title = " ".join(title.split())
        if not artist or not title:
            return []
        source_context = capture_mb_source_context()
        cache_key = (
            f"{MB_RECORDING_SEARCH_PREFIX}{artist.lower()}|{title.lower()}:{limit}"
        )
        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached

        dedupe_key = (
            f"{cache_key}:priority={int(priority)}:g{source_context.generation}"
        )
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._search_recordings_uncached(
                artist,
                title,
                limit,
                priority,
                cache_key,
                source_context=source_context,
            ),
        )

    async def _search_recordings_uncached(
        self,
        artist: str,
        title: str,
        limit: int,
        priority: RequestPriority,
        cache_key: str,
        *,
        source_context: MbSourceContext,
    ) -> list["RecordingMatch"]:
        try:
            result = await mb_api_get(
                "/recording",
                params={
                    "query": build_recording_search_query(title, artist),
                    "limit": limit,
                },
                priority=priority,
                decode_type=_RecordingSearchPayload,
                source_context=source_context,
            )
            response_context = get_mb_response_context() or source_context
            matches: list[RecordingMatch] = []
            for rec in result.recordings:
                rec_id = rec.get("id")
                if not rec_id:
                    continue
                groups_by_id: dict[str, RecordingReleaseGroup] = {}
                for rel in rec.get("releases") or []:
                    rg = rel.get("release-group") or {}
                    rg_id = rg.get("id")
                    if not rg_id:
                        continue
                    normalized_rg_id = normalize_mb_id(rg_id)
                    candidate = RecordingReleaseGroup(
                        release_group_mbid=rg_id,
                        release_group_title=rg.get("title", ""),
                        release_mbid=rel.get("id"),
                        primary_type=rg.get("primary-type"),
                        secondary_types=tuple(rg.get("secondary-types") or ()),
                        release_status=rel.get("status"),
                        release_date=rel.get("date") or rg.get("first-release-date"),
                    )
                    current = groups_by_id.get(normalized_rg_id)
                    if current is None or recording_release_group_rank(
                        release_status=candidate.release_status,
                        secondary_types=candidate.secondary_types,
                        primary_type=candidate.primary_type,
                        release_date=candidate.release_date,
                        release_group_mbid=candidate.release_group_mbid,
                    ) < recording_release_group_rank(
                        release_status=current.release_status,
                        secondary_types=current.secondary_types,
                        primary_type=current.primary_type,
                        release_date=current.release_date,
                        release_group_mbid=current.release_group_mbid,
                    ):
                        groups_by_id[normalized_rg_id] = candidate
                matches.append(
                    RecordingMatch(
                        recording_mbid=rec_id,
                        title=rec.get("title", ""),
                        artist=extract_artist_name(rec),
                        score=get_score(rec),
                        release_groups=list(groups_by_id.values()),
                    )
                )

            advanced_settings = self._preferences_service.get_advanced_settings()
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                matches,
                ttl_seconds=advanced_settings.cache_ttl_search,
                context=response_context,
            )
            return matches
        except InvalidExternalPayloadError:
            _record_mb_unmappable_payload("recording search")
            return []
        except Exception as e:  # noqa: BLE001
            if not isinstance(e, CircuitOpenError):
                logger.error("MusicBrainz recording search failed")
            _record_mb_degradation("recording search failed")
            return []

    async def get_cached_recording_to_release_group(
        self, recording_mbid: str
    ) -> str | None:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        recording_mbid = normalize_mb_id(recording_mbid)
        if not recording_mbid:
            return None
        cached = await mb_cache_get_if_current(
            self._cache,
            f"{MB_RECORDING_TO_RG_PREFIX}{recording_mbid}",
            source_context,
        )
        return None if cached in (None, "") else cached

    async def resolve_recording_to_release_group(
        self,
        recording_mbid: str,
    ) -> str | None:
        """Resolve an AcoustID recording MBID to its best release-group MBID.

        Uses ``inc=releases+release-groups`` (verified against the live API - a
        bare ``inc=release-groups`` on a recording returns no groups). The best
        of the recording's release groups is chosen by the same Album > EP >
        Single deterministic status/type/date heuristic used elsewhere. Tier 3 of the
        scanner's tiered identification (AcoustID -> recording -> release group).
        """
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        recording_mbid = normalize_mb_id(recording_mbid)
        if not recording_mbid:
            return None
        cache_key = f"{MB_RECORDING_TO_RG_PREFIX}{recording_mbid}"
        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached if cached != "" else None

        dedupe_key = f"{cache_key}:g{source_context.generation}"
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_recording_release_group(
                recording_mbid, cache_key, source_context=source_context
            ),
        )

    async def _fetch_recording_release_group(
        self,
        recording_mbid: str,
        cache_key: str,
        *,
        source_context: MbSourceContext | None = None,
    ) -> str | None:
        source_context = source_context or capture_mb_source_context()
        try:
            result = await mb_api_get(
                f"/recording/{recording_mbid}",
                params={"inc": "releases+release-groups"},
                priority=RequestPriority.BACKGROUND_SYNC,
                decode_type=_RecordingReleaseGroupPayload,
                source_context=source_context,
            )
            response_context = get_mb_response_context() or source_context
            best = _pick_best_release_group(result.releases)
            rg_id = best[0] if best else None
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                rg_id or "",
                ttl_seconds=86400,
                context=response_context,
            )
            return rg_id
        except Exception as e:  # noqa: BLE001
            # F-MATCH-05: same policy as the release-to-rg helper - a transient
            # failure records degradation and stays uncached; only a decoded
            # response proving "no selectable group" may cache the definitive
            # empty sentinel.
            if not isinstance(e, CircuitOpenError):
                logger.error("MusicBrainz recording-to-group lookup failed")
            _record_mb_degradation("recording-to-rg lookup failed")
            return None

    async def get_recording_position_on_release(
        self,
        release_id: str,
        recording_mbid: str,
    ) -> tuple[int, int] | None:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        release_id = normalize_mb_id(release_id)
        recording_mbid = normalize_mb_id(recording_mbid)
        pos_cache_key = f"{MB_RELEASE_REC_PREFIX}{release_id}"
        positions = await mb_cache_get_if_current(
            self._cache, pos_cache_key, source_context
        )
        if positions and recording_mbid in positions:
            disc, track = positions[recording_mbid]
            return (disc, track)
        return None

    @staticmethod
    def extract_youtube_url_from_relations(entity_data: dict) -> str | None:
        for rel in entity_data.get("relations", []):
            url_obj = rel.get("url", {})
            url = url_obj.get("resource", "") if isinstance(url_obj, dict) else ""
            if "youtube.com" in url or "youtu.be" in url:
                return url
        return None

    @staticmethod
    def youtube_url_to_embed(url: str) -> str | None:
        import re

        patterns = [
            r"youtube\.com/watch\?v=([a-zA-Z0-9_-]{11})",
            r"youtu\.be/([a-zA-Z0-9_-]{11})",
            r"youtube\.com/embed/([a-zA-Z0-9_-]{11})",
        ]
        for pattern in patterns:
            match = re.search(pattern, url)
            if match:
                return f"https://www.youtube.com/embed/{match.group(1)}"
        return None

    async def get_recording_by_id(
        self,
        recording_id: str,
        includes: list[str] | None = None,
    ) -> dict | None:
        clear_mb_response_context()
        source_context = capture_mb_source_context()
        if includes is None:
            includes = ["url-rels"]
        recording_id = normalize_mb_id(recording_id)
        inc_str = "+".join(sorted(includes))
        cache_key = f"{MB_RECORDING_PREFIX}{recording_id}:{inc_str}"
        cached = await mb_cache_get_if_current(self._cache, cache_key, source_context)
        if cached is not None:
            return cached
        dedupe_key = (
            f"{cache_key}:priority={int(RequestPriority.BACKGROUND_SYNC)}"
            f":g{source_context.generation}"
        )
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._get_recording_by_id_uncached(
                recording_id, inc_str, cache_key, source_context=source_context
            ),
        )

    async def _get_recording_by_id_uncached(
        self,
        recording_id: str,
        inc_str: str,
        cache_key: str,
        *,
        source_context: MbSourceContext | None = None,
    ) -> dict | None:
        source_context = source_context or capture_mb_source_context()
        try:
            result = await mb_api_get(
                f"/recording/{recording_id}",
                params={"inc": inc_str},
                priority=RequestPriority.BACKGROUND_SYNC,
                source_context=source_context,
            )
            response_context = get_mb_response_context() or source_context
            if not result:
                return None
            await mb_cache_set_if_current(
                self._cache,
                cache_key,
                result,
                ttl_seconds=3600,
                context=response_context,
            )
            return result
        except InvalidExternalPayloadError:
            _record_mb_unmappable_payload("recording fetch")
            return None
        except Exception as e:  # noqa: BLE001
            if not isinstance(e, CircuitOpenError):
                logger.error("MusicBrainz recording fetch failed")
            _record_mb_degradation("recording fetch failed")
            return None
