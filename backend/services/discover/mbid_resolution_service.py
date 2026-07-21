import asyncio
import logging
from contextvars import ContextVar
from typing import Any

from api.v1.schemas.discover import DiscoverQueueItemLight
from infrastructure.persistence import LibraryDB, MBIDStore
from infrastructure.queue.priority_queue import RequestPriority
from repositories.protocols import (
    LibraryRepositoryProtocol,
    ListenBrainzRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
)

logger = logging.getLogger(__name__)

# Set (per-task) by the background warmer's thorough build. When true, a build runs with its
# section budgets relaxed AND resolves ALL pending album->release-group lookups instead of
# capping at max_lookups - so a section like Top Picks fully personalises in one pass rather
# than banking only ~10 albums per build and being frozen partially-personalised by the cache.
# On-visit builds leave it False (stay fast + tightly budgeted).
discover_build_thorough: ContextVar[bool] = ContextVar("discover_build_thorough", default=False)


class MbidResolutionService:
    def __init__(
        self,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
        library_repo: LibraryRepositoryProtocol,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        library_db: LibraryDB | None = None,
        mbid_store: MBIDStore | None = None,
    ) -> None:
        self._mb_repo = musicbrainz_repo
        self._library_repo = library_repo
        self._lb_repo = listenbrainz_repo
        self._library_db = library_db
        self._mbid_store = mbid_store

    @staticmethod
    def normalize_mbid(mbid: str | None) -> str | None:
        if not mbid:
            return None
        normalized = mbid.strip().lower()
        return normalized or None

    async def resolve_lastfm_release_group_mbids(
        self,
        album_mbids: list[str],
        *,
        max_lookups: int = 10,
        allow_passthrough: bool = True,
        resolver_cache: dict[str, str | None] | None = None,
    ) -> dict[str, str]:
        normalized: list[str] = []
        seen: set[str] = set()
        for mbid in album_mbids:
            mbid_normalized = self.normalize_mbid(mbid)
            if not mbid_normalized or mbid_normalized in seen:
                continue
            normalized.append(mbid_normalized)
            seen.add(mbid_normalized)

        if not normalized:
            return {}

        cache = resolver_cache if resolver_cache is not None else {}
        resolved: dict[str, str] = {}
        pending: list[str] = []

        for mbid in normalized:
            if mbid in cache:
                cached_value = cache[mbid]
                if cached_value:
                    resolved[mbid] = cached_value
                elif allow_passthrough:
                    resolved[mbid] = mbid
                continue
            pending.append(mbid)

        if pending and self._mbid_store:
            try:
                persisted = await self._mbid_store.get_mbid_resolution_map(pending)
                still_pending: list[str] = []
                for mbid in pending:
                    if mbid in persisted:
                        rg_mbid = persisted[mbid]
                        cache[mbid] = rg_mbid
                        if rg_mbid:
                            resolved[mbid] = rg_mbid
                        elif allow_passthrough:
                            resolved[mbid] = mbid
                    else:
                        still_pending.append(mbid)
                pending = still_pending
            except Exception:  # noqa: BLE001
                logger.warning("Failed to load MBID resolution from persistent cache")

        if not pending:
            return resolved

        new_resolutions: dict[str, str | None] = {}

        # thorough (warmer) builds resolve everything; on-visit builds cap for speed
        lookup_limit = len(pending) if discover_build_thorough.get() else max_lookups
        lookup_mbids = pending[:lookup_limit]
        skipped_mbids = pending[lookup_limit:]
        for mbid in skipped_mbids:
            if allow_passthrough:
                resolved[mbid] = mbid
                cache[mbid] = mbid
            else:
                cache[mbid] = None

        unresolved: list[str] = []

        # Resolve release->RG and bank EACH hit the instant it lands (shielded), NOT after
        # the whole gather. MusicBrainz is a hard 1 req/s, so during the ListenBrainz-
        # popularity outage this gather drains slowly and is routinely cancelled mid-flight
        # by a build budget. asyncio.gather is all-or-nothing: a cancellation at the await
        # discards EVERY already-completed result, so persisting post-gather banked nothing
        # and the store never warmed. Per-completion persistence means a cancelled build
        # still keeps every resolution it earned, so on-visit reloads (and the prewarm)
        # accumulate durably and personalisation converges.
        async def _resolve_and_bank(mbid: str) -> None:
            try:
                result = await self._mb_repo.get_release_group_id_from_release(mbid)
            except Exception:  # noqa: BLE001
                unresolved.append(mbid)
                return
            rg_mbid = self.normalize_mbid(result)
            if rg_mbid:
                resolved[mbid] = rg_mbid
                cache[mbid] = rg_mbid
                await self._persist_resolutions({mbid: rg_mbid})
            else:
                unresolved.append(mbid)

        await asyncio.gather(
            *[_resolve_and_bank(mbid) for mbid in lookup_mbids],
            return_exceptions=True,
        )

        if not unresolved:
            return resolved

        # BACKGROUND_SYNC: these run inside background discover builds. At the default
        # USER_INITIATED they'd compete with real user page loads AND re-arm the priority
        # queue's user-activity timer, starving the build's own background lookups. The
        # primary release->RG lookup on this path is already BACKGROUND_SYNC.
        rg_checks = await asyncio.gather(
            *[
                self._mb_repo.get_release_group_by_id(
                    mbid, includes=["artist-credits"], priority=RequestPriority.BACKGROUND_SYNC
                )
                for mbid in unresolved
            ],
            return_exceptions=True,
        )

        for mbid, result in zip(unresolved, rg_checks):
            if isinstance(result, Exception):
                if allow_passthrough:
                    resolved[mbid] = mbid
                    cache[mbid] = mbid
                else:
                    cache[mbid] = None
                continue
            if isinstance(result, dict) and result.get("id"):
                resolved[mbid] = mbid
                cache[mbid] = mbid
                new_resolutions[mbid] = mbid
            elif allow_passthrough:
                resolved[mbid] = mbid
                cache[mbid] = mbid
            else:
                cache[mbid] = None
                new_resolutions[mbid] = None

        await self._persist_resolutions(new_resolutions)

        return resolved

    async def _persist_resolutions(self, new_resolutions: dict[str, str | None]) -> None:
        """Durably bank resolved LB->RG mappings so the cache warms across builds.

        Shielded: the discover build that drives this resolver may cancel it on a
        budget timeout (Top Picks / radio / queue) while MB lookups are still draining
        at 1 req/s. The SQLite write that records the lookups already completed must
        finish regardless, or a starved build banks nothing and the next build re-does
        the same 1/s resolutions from scratch."""
        if not new_resolutions or not self._mbid_store:
            return
        try:
            await asyncio.shield(self._mbid_store.save_mbid_resolution_map(dict(new_resolutions)))
        except Exception:  # noqa: BLE001
            logger.warning("Failed to persist MBID resolutions")

    async def lastfm_albums_to_queue_items(
        self,
        artist_albums_pairs: list[tuple[Any, list]],
        *,
        exclude: set[str] | None = None,
        target: int,
        reason: str,
        is_wildcard: bool = False,
        resolver_cache: dict[str, str | None] | None = None,
        use_album_artist_name: bool = True,
    ) -> list[DiscoverQueueItemLight]:
        all_album_mbids: list[str] = []
        for _, albums in artist_albums_pairs:
            all_album_mbids.extend(a.mbid for a in albums if a.mbid)
        rg_mbid_map = await self.resolve_lastfm_release_group_mbids(
            all_album_mbids, resolver_cache=resolver_cache,
        )
        items: list[DiscoverQueueItemLight] = []
        seen_rg_mbids: set[str] = {mbid.lower() for mbid in (exclude or set())}
        for artist, albums in artist_albums_pairs:
            if len(items) >= target:
                break
            artist_mbid = self.normalize_mbid(artist.mbid)
            for album in albums:
                if len(items) >= target:
                    break
                raw_album_mbid = self.normalize_mbid(album.mbid)
                if not raw_album_mbid:
                    continue
                rg_mbid = rg_mbid_map.get(raw_album_mbid)
                if not rg_mbid:
                    continue
                rg_mbid_lower = rg_mbid.lower()
                if rg_mbid_lower in seen_rg_mbids:
                    continue
                artist_name = (album.artist_name or artist.name) if use_album_artist_name else artist.name
                items.append(DiscoverQueueItemLight(
                    release_group_mbid=rg_mbid,
                    album_name=album.name,
                    artist_name=artist_name,
                    artist_mbid=artist_mbid or "",
                    cover_url=f"/api/v1/covers/release-group/{rg_mbid}?size=500",
                    recommendation_reason=reason,
                    is_wildcard=is_wildcard,
                    in_library=False,
                ))
                seen_rg_mbids.add(rg_mbid_lower)
        return items

    async def resolve_release_mbids(
        self,
        release_ids: list[str],
    ) -> dict[str, str]:
        return await self.resolve_lastfm_release_group_mbids(
            release_ids, allow_passthrough=False,
        )

    async def get_library_artist_mbids(
        self, library_configured: bool, candidate_ids: list[str] | None = None
    ) -> set[str]:
        if not library_configured:
            return set()
        try:
            if candidate_ids is not None:
                return await self._library_repo.existing_artist_mbids(candidate_ids)
            artists = await self._library_repo.get_home_artists(limit=500)
            return {a.get("mbid", "").lower() for a in artists if a.get("mbid")}
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch library artists from Lidarr")
            return set()

    async def get_library_album_mbids(
        self, library_configured: bool, candidate_ids: list[str] | None = None
    ) -> set[str]:
        if not library_configured:
            if self._library_db and candidate_ids is not None:
                try:
                    return await self._library_db.existing_library_mbids(candidate_ids)
                except Exception:  # noqa: BLE001
                    logger.warning("Failed to fetch album MBIDs from library cache")
            return set()
        try:
            if candidate_ids is not None:
                return await self._library_repo.existing_album_mbids(candidate_ids)
            albums = await self._library_repo.get_home_albums(limit=500)
            return {
                album.musicbrainz_id.casefold()
                for album in albums
                if album.musicbrainz_id
            }
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch library album MBIDs from Lidarr")
            return set()

    async def get_user_listened_release_group_mbids(
        self,
        lb_enabled: bool,
        username: str | None,
        resolved_source: str,
    ) -> set[str]:
        if resolved_source != "listenbrainz" or not lb_enabled or not username:
            return set()
        try:
            listened = await self._lb_repo.get_user_top_release_groups(
                username=username,
                range_="all_time",
                count=100,
            )
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch user listened release groups from ListenBrainz")
            return set()
        return {
            rg.release_group_mbid.lower()
            for rg in listened
            if getattr(rg, "release_group_mbid", None)
        }

    def make_queue_item(
        self,
        *,
        release_group_mbid: str,
        album_name: str,
        artist_name: str,
        artist_mbid: str,
        reason: str,
        is_wildcard: bool = False,
    ) -> DiscoverQueueItemLight:
        return DiscoverQueueItemLight(
            release_group_mbid=release_group_mbid,
            album_name=album_name,
            artist_name=artist_name,
            artist_mbid=artist_mbid,
            cover_url=f"/api/v1/covers/release-group/{release_group_mbid}?size=500",
            recommendation_reason=reason,
            is_wildcard=is_wildcard,
            in_library=False,
        )
