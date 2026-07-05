import logging
from typing import Any

import msgspec

from models.album import AlbumInfo
from models.search import SearchResult
from services.preferences_service import PreferencesService
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.cache_keys import (
    mb_album_search_key,
    mb_release_group_key,
    mb_release_key,
    MB_RG_BY_TAG_PREFIX,
    MB_RG_DETAIL_PREFIX,
    MB_RELEASE_DETAIL_PREFIX,
    MB_RELEASE_TO_RG_PREFIX,
    MB_RELEASE_REC_PREFIX,
    MB_RECORDING_PREFIX,
    MB_RECORDING_SEARCH_PREFIX,
    MB_RECORDING_TO_RG_PREFIX,
)
from infrastructure.queue.priority_queue import RequestPriority
from repositories.musicbrainz_base import (
    mb_api_get,
    mb_deduplicator,
    dedupe_by_id,
    get_score,
    should_include_release,
    extract_artist_name,
    parse_year,
    build_musicbrainz_tag_query,
)
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult

logger = logging.getLogger(__name__)


def _record_mb_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx:
        ctx.record(IntegrationResult.error(source="musicbrainz", msg=msg))


class _ReleaseGroupSearchPayload(msgspec.Struct):
    release_groups: list[dict[str, Any]] = msgspec.field(name="release-groups", default_factory=list)


class _ReleaseLookupPayload(msgspec.Struct):
    release_group: dict[str, Any] = msgspec.field(name="release-group", default_factory=dict)
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


class RecordingMatch(msgspec.Struct):
    """A recording-search candidate with the release groups it belongs to."""

    recording_mbid: str
    title: str
    artist: str | None
    score: int
    release_groups: list[RecordingReleaseGroup]


def _rg_priority(rg: dict) -> int:
    rg_type = rg.get("primary-type", "")
    priority = 0
    if rg_type == "Album":
        priority = 3
    elif rg_type == "EP":
        priority = 2
    elif rg_type == "Single":
        priority = 1
    secondary = rg.get("secondary-types", [])
    if secondary:
        priority = max(0, priority - 1)
    return priority


def _pick_best_release_group(releases: list[dict]) -> tuple[str, str] | None:
    candidates: list[tuple[str, str, int]] = []
    for release in releases:
        rg = release.get("release-group", {})
        rg_id = rg.get("id")
        rg_title = rg.get("title", "")
        if rg_id:
            candidates.append((rg_id, rg_title, _rg_priority(rg)))
    if not candidates:
        return None
    candidates.sort(key=lambda c: c[2], reverse=True)
    return (candidates[0][0], candidates[0][1])


class MusicBrainzAlbumMixin:
    _cache: CacheInterface
    _preferences_service: PreferencesService

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
    ) -> list[SearchResult]:
        cache_key = mb_album_search_key(query, limit, offset, included_secondary_types, included_primary_types)
        if include_all_types:
            cache_key = f"{cache_key}:all"

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        try:
            internal_limit = min(100, max(int(limit * 1.5), 25))

            result = await mb_api_get(
                "/release-group",
                params={
                    "query": f'releasegroup:"{query}"^3 OR release:"{query}"^2 OR {query}',
                    "limit": internal_limit,
                    "offset": offset,
                },
                priority=RequestPriority.USER_INITIATED,
                decode_type=_ReleaseGroupSearchPayload,
            )
            release_groups = result.release_groups
            release_groups = dedupe_by_id(release_groups)

            results = []
            for rg in release_groups:
                mapped = self._map_release_group_to_result(
                    rg, included_secondary_types, include_all_types, included_primary_types
                )
                if mapped:
                    results.append(mapped)
                if len(results) >= limit:
                    break

            advanced_settings = self._preferences_service.get_advanced_settings()
            await self._cache.set(cache_key, results, ttl_seconds=advanced_settings.cache_ttl_search)
            return results
        except Exception as e:  # noqa: BLE001
            logger.error(f"MusicBrainz album search failed: {e}")
            _record_mb_degradation(f"album search failed: {e}")
            return []

    async def search_release_groups_by_tag(
        self,
        tag: str,
        limit: int = 50,
        offset: int = 0,
        included_secondary_types: set[str] | None = None
    ) -> list[SearchResult]:
        cache_key = f"{MB_RG_BY_TAG_PREFIX}{tag.lower()}:{limit}:{offset}"

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

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
            )
            release_groups = result.release_groups
            release_groups = dedupe_by_id(release_groups)

            results = []
            for rg in release_groups:
                mapped = self._map_release_group_to_result(rg, included_secondary_types)
                if mapped:
                    results.append(mapped)
                if len(results) >= limit:
                    break

            advanced_settings = self._preferences_service.get_advanced_settings()
            await self._cache.set(cache_key, results, ttl_seconds=advanced_settings.cache_ttl_search * 2)
            return results
        except Exception as e:  # noqa: BLE001
            logger.error(f"MusicBrainz release group tag search failed for '{tag}': {e}")
            _record_mb_degradation(f"release group tag search failed: {e}")
            return []

    async def get_release_group_by_id(
        self,
        mbid: str,
        includes: list[str] | None = None,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> dict | None:
        if includes is None:
            includes = ["artist-credits", "releases"]

        cache_key = mb_release_group_key(mbid, includes)

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        includes_str = "+".join(sorted(includes))
        dedupe_key = f"{MB_RG_DETAIL_PREFIX}{mbid}:{includes_str}"
        return await mb_deduplicator.dedupe(dedupe_key, lambda: self._fetch_release_group_by_id(mbid, includes, cache_key, priority))

    async def _fetch_release_group_by_id(
        self,
        mbid: str,
        includes: list[str],
        cache_key: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> dict | None:
        try:
            inc_str = "+".join(sorted(includes))
            result = await mb_api_get(
                f"/release-group/{mbid}",
                params={"inc": inc_str},
                priority=priority,
            )
            if not result:
                # Definitive miss (mb_api_get returns {} only on HTTP 404 - 503/5xx
                # raise instead and land in the except below). Negative-cache it briefly
                # so a merged/deleted/passthrough-garbage RG mbid isn't re-fetched on every
                # discover build. Short TTL bounds staleness and self-heals; the except
                # (transient) path stays uncached on purpose.
                await self._cache.set(cache_key, {}, ttl_seconds=600)
                return None
            await self._cache.set(cache_key, result, ttl_seconds=3600)
            return result
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch release group {mbid}: {e}")
            _record_mb_degradation(f"release group fetch failed: {e}")
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
        if includes is None:
            includes = ["recordings", "labels"]

        cache_key = mb_release_key(release_id, includes)

        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        includes_str = "+".join(sorted(includes))
        dedupe_key = f"{MB_RELEASE_DETAIL_PREFIX}{release_id}:{includes_str}"
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_release_by_id(release_id, includes, cache_key, priority),
        )

    async def _fetch_release_by_id(
        self,
        release_id: str,
        includes: list[str],
        cache_key: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> dict | None:
        try:
            inc_str = "+".join(sorted(includes))
            result = await mb_api_get(
                f"/release/{release_id}",
                params={"inc": inc_str},
                priority=priority,
            )
            if not result:
                return None
            await self._cache.set(cache_key, result, ttl_seconds=3600)
            return result
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch release {release_id}: {e}")
            _record_mb_degradation(f"release fetch failed: {e}")
            return None

    async def get_release_group_id_from_release(
        self,
        release_id: str
    ) -> str | None:
        cache_key = f"{MB_RELEASE_TO_RG_PREFIX}{release_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached if cached != "" else None

        dedupe_key = f"{MB_RELEASE_TO_RG_PREFIX}{release_id}"
        return await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_release_group_id_from_release(release_id, cache_key),
        )

    async def _fetch_release_group_id_from_release(
        self,
        release_id: str,
        cache_key: str,
    ) -> str | None:
        try:
            result = await mb_api_get(
                f"/release/{release_id}",
                params={"inc": "release-groups+recordings"},
                priority=RequestPriority.BACKGROUND_SYNC,
                decode_type=_ReleaseLookupPayload,
            )
            rg = result.release_group
            rg_id = rg.get("id")
            await self._cache.set(cache_key, rg_id or "", ttl_seconds=86400)

            positions: dict[str, list[int]] = {}
            for medium in result.media:
                disc = medium.get("position", 1)
                for track in medium.get("tracks", medium.get("track-list", [])):
                    rec = track.get("recording", {})
                    rec_id = rec.get("id")
                    trk_pos = track.get("position")
                    if rec_id and trk_pos is not None:
                        positions[rec_id] = [disc, trk_pos]
            if positions:
                pos_cache_key = f"{MB_RELEASE_REC_PREFIX}{release_id}"
                await self._cache.set(pos_cache_key, positions, ttl_seconds=86400)

            return rg_id
        except Exception as e:  # noqa: BLE001
            _record_mb_degradation(f"release-to-rg lookup failed: {e}")
            await self._cache.set(cache_key, "", ttl_seconds=3600)
            return None

    async def search_recordings(
        self,
        artist: str,
        title: str,
        limit: int = 8,
    ) -> list["RecordingMatch"]:
        """Search MusicBrainz recordings by track title + artist, returning each candidate with its release groups."""
        artist = artist.replace('"', "").strip()
        title = title.replace('"', "").strip()
        if not artist or not title:
            return []
        cache_key = f"{MB_RECORDING_SEARCH_PREFIX}{artist.lower()}|{title.lower()}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        try:
            result = await mb_api_get(
                "/recording",
                params={
                    "query": f'recording:"{title}" AND artist:"{artist}"',
                    "limit": limit,
                },
                priority=RequestPriority.USER_INITIATED,
                decode_type=_RecordingSearchPayload,
            )
            matches: list[RecordingMatch] = []
            for rec in result.recordings:
                rec_id = rec.get("id")
                if not rec_id:
                    continue
                groups: list[RecordingReleaseGroup] = []
                seen: set[str] = set()
                for rel in rec.get("releases") or []:
                    rg = rel.get("release-group") or {}
                    rg_id = rg.get("id")
                    if not rg_id or rg_id in seen:
                        continue
                    seen.add(rg_id)
                    groups.append(
                        RecordingReleaseGroup(
                            release_group_mbid=rg_id,
                            release_group_title=rg.get("title", ""),
                            release_mbid=rel.get("id"),
                            primary_type=rg.get("primary-type"),
                            secondary_types=tuple(rg.get("secondary-types") or ()),
                        )
                    )
                matches.append(
                    RecordingMatch(
                        recording_mbid=rec_id,
                        title=rec.get("title", ""),
                        artist=extract_artist_name(rec),
                        score=get_score(rec),
                        release_groups=groups,
                    )
                )

            advanced_settings = self._preferences_service.get_advanced_settings()
            await self._cache.set(cache_key, matches, ttl_seconds=advanced_settings.cache_ttl_search)
            return matches
        except Exception as e:  # noqa: BLE001
            logger.error(f"MusicBrainz recording search failed: {e}")
            _record_mb_degradation(f"recording search failed: {e}")
            return []

    async def resolve_recording_to_release_group(
        self,
        recording_mbid: str,
    ) -> str | None:
        """Resolve an AcoustID recording MBID to its best release-group MBID.

        Uses ``inc=releases+release-groups`` (verified against the live API - a
        bare ``inc=release-groups`` on a recording returns no groups). The best
        of the recording's release groups is chosen by the same Album > EP >
        Single / penalise-secondary-types heuristic used elsewhere. Tier 3 of the
        scanner's tiered identification (AcoustID -> recording -> release group).
        """
        if not recording_mbid:
            return None
        cache_key = f"{MB_RECORDING_TO_RG_PREFIX}{recording_mbid}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached if cached != "" else None

        return await mb_deduplicator.dedupe(
            cache_key,
            lambda: self._fetch_recording_release_group(recording_mbid, cache_key),
        )

    async def _fetch_recording_release_group(
        self,
        recording_mbid: str,
        cache_key: str,
    ) -> str | None:
        try:
            result = await mb_api_get(
                f"/recording/{recording_mbid}",
                params={"inc": "releases+release-groups"},
                priority=RequestPriority.BACKGROUND_SYNC,
                decode_type=_RecordingReleaseGroupPayload,
            )
            best = _pick_best_release_group(result.releases)
            rg_id = best[0] if best else None
            await self._cache.set(cache_key, rg_id or "", ttl_seconds=86400)
            return rg_id
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to resolve recording {recording_mbid} to release group: {e}")
            _record_mb_degradation(f"recording-to-rg lookup failed: {e}")
            await self._cache.set(cache_key, "", ttl_seconds=3600)
            return None

    async def get_recording_position_on_release(
        self,
        release_id: str,
        recording_mbid: str,
    ) -> tuple[int, int] | None:
        pos_cache_key = f"{MB_RELEASE_REC_PREFIX}{release_id}"
        positions = await self._cache.get(pos_cache_key)
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
        if includes is None:
            includes = ["url-rels"]
        inc_str = "+".join(sorted(includes))
        cache_key = f"{MB_RECORDING_PREFIX}{recording_id}:{inc_str}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        try:
            result = await mb_api_get(
                f"/recording/{recording_id}",
                params={"inc": inc_str},
                priority=RequestPriority.BACKGROUND_SYNC,
            )
            if not result:
                return None
            await self._cache.set(cache_key, result, ttl_seconds=3600)
            return result
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch recording {recording_id}: {e}")
            _record_mb_degradation(f"recording fetch failed: {e}")
            return None
