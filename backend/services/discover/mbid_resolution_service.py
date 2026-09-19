import asyncio
import hashlib
import json
import logging
import time
from contextvars import ContextVar
from typing import Any
from functools import wraps
from weakref import WeakValueDictionary

from api.v1.schemas.discover import DiscoverQueueItemLight
from core.exceptions import ConfigurationError
from infrastructure.persistence import LibraryDB, MBIDStore
from infrastructure.observability.optional_work import OptionalWorkDeferred, check_optional_dispatch
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.validators import is_valid_mbid
from repositories.musicbrainz_base import (
    capture_mb_source_context,
    clear_mb_response_context,
    get_mb_response_context,
    is_mb_source_current,
    mb_publish_if_current,
)
from repositories.protocols import (
    LibraryRepositoryProtocol,
    ListenBrainzRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
)

logger = logging.getLogger(__name__)

resolution_user: ContextVar[str | None] = ContextVar("resolution_user", default=None)


def with_resolution_user(method):
    @wraps(method)
    async def scoped(self, user_id: str, *args, **kwargs):
        token = resolution_user.set(user_id)
        try:
            return await method(self, user_id, *args, **kwargs)
        finally:
            resolution_user.reset(token)
    return scoped


class MbidResolutionService:
    def __init__(
        self,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
        library_repo: LibraryRepositoryProtocol,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        library_db: LibraryDB | None = None,
        mbid_store: MBIDStore | None = None,
        mb_canonical_store=None,
        progress_store=None,
    ) -> None:
        self._mb_repo = musicbrainz_repo
        self._library_repo = library_repo
        self._lb_repo = listenbrainz_repo
        self._library_db = library_db
        self._mbid_store = mbid_store
        # ST2 cutover: durable canonical map replaces mbid_resolution_map as
        # the persistent tier for discover-lane release->RG.
        self._mb_canonical_store = mb_canonical_store
        self._progress_store = progress_store
        self._progress_locks: WeakValueDictionary[tuple[str | None, str], asyncio.Lock] = WeakValueDictionary()

    @staticmethod
    def normalize_mbid(mbid: str | None) -> str | None:
        if not mbid:
            return None
        normalized = mbid.strip().lower()
        return normalized or None

    async def resolve_lastfm_release_group_mbids(
        self, album_mbids: list[str], *, max_lookups: int = 10,
        allow_passthrough: bool = True,
        resolver_cache: dict[str, str | None] | None = None,
        user_id: str | None = None, work_key: str = "release-resolution",
    ) -> dict[str, str]:
        operation_context = capture_mb_source_context()
        user_id = user_id or resolution_user.get()
        key = (user_id, work_key)
        lock = self._progress_locks.get(key)
        if lock is None:
            lock = asyncio.Lock()
            self._progress_locks[key] = lock
        async with lock:
            if not is_mb_source_current(operation_context):
                return {}
            return await self._resolve_lastfm_release_group_mbids(
                album_mbids, max_lookups=max_lookups,
                allow_passthrough=allow_passthrough, resolver_cache=resolver_cache,
                user_id=user_id, work_key=work_key,
            )

    async def _resolve_lastfm_release_group_mbids(
        self,
        album_mbids: list[str],
        *,
        max_lookups: int = 10,
        allow_passthrough: bool = True,
        resolver_cache: dict[str, str | None] | None = None,
        user_id: str | None = None,
        work_key: str = "release-resolution",
    ) -> dict[str, str]:
        operation_context = capture_mb_source_context()
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
        normalized.sort()

        cache = resolver_cache if resolver_cache is not None else {}
        cache_writes: set[str] = set()

        def cache_value(mbid: str, value: str | None) -> None:
            cache[mbid] = value
            cache_writes.add(mbid)

        def abort_for_source_change() -> dict[str, str]:
            for mbid in cache_writes:
                cache.pop(mbid, None)
            return {}

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

        if pending and self._mb_canonical_store:
            # ST2 cutover: the durable canonical map replaces
            # mbid_resolution_map as the persistent read tier.
            try:
                persisted = await self._mb_canonical_store.get_release_to_rg_batch(
                    pending,
                    source_context=operation_context,
                )
                if not is_mb_source_current(operation_context):
                    raise ConfigurationError(
                        "MusicBrainz source changed during canonical read"
                    )
                still_pending: list[str] = []
                for mbid in pending:
                    if mbid.casefold() in persisted:
                        rg_mbid = persisted[mbid.casefold()]
                        if rg_mbid:
                            resolved[mbid] = rg_mbid
                            cache_value(mbid, rg_mbid)
                        elif allow_passthrough:
                            resolved[mbid] = mbid
                            cache_value(mbid, mbid)
                        else:
                            cache_value(mbid, None)
                    else:
                        still_pending.append(mbid)
                pending = still_pending
            except ConfigurationError:
                return abort_for_source_change()
            except OptionalWorkDeferred:
                raise
            except Exception:  # noqa: BLE001
                logger.warning("Failed to load from canonical store")

        if not pending:
            if not is_mb_source_current(operation_context):
                return abort_for_source_change()
            return resolved

        user_id = user_id or resolution_user.get()
        revision = hashlib.sha256(
            json.dumps(
                [operation_context.source_url, operation_context.generation,
                 operation_context.source_mode, operation_context.source_id, normalized],
                separators=(",", ":"),
            ).encode()
        ).hexdigest()
        progress_key = f"mbid:{work_key}"
        cursor = 0
        lease = (
            self._progress_store.user_lease(user_id)
            if self._progress_store is not None and user_id is not None else None
        )
        if self._progress_store is not None and user_id is not None:
            cursor = await self._progress_store.get_progress(user_id, progress_key, revision)
        if not is_mb_source_current(operation_context):
            return abort_for_source_change()
        start = cursor % len(normalized)
        pending_set = set(pending)
        ordered = [
            (index, normalized[index])
            for offset in range(len(normalized))
            if normalized[index := (start + offset) % len(normalized)] in pending_set
        ]
        selected = ordered[:max(0, min(max_lookups, 10))]
        for mbid in pending:
            if allow_passthrough:
                resolved[mbid] = mbid

        async def save_cursor(index: int) -> None:
            if self._progress_store is None or user_id is None:
                return
            check_optional_dispatch()
            if not is_mb_source_current(operation_context):
                raise ConfigurationError("MusicBrainz source changed during resolution")
            async def publish() -> None:
                if lease is not None and not lease.active:
                    raise OptionalWorkDeferred()
                await self._progress_store.save_progress(
                    user_id, progress_key, revision, index, time.time()
                )
            if not await mb_publish_if_current(operation_context, publish):
                raise ConfigurationError("MusicBrainz source changed during resolution")

        for index, mbid in selected:
            try:
                check_optional_dispatch()
                if lease is not None and not lease.active:
                    raise OptionalWorkDeferred()
                # Claim the next position before dispatch so cancellation cannot
                # strand the remaining inputs behind a persistently slow head.
                await save_cursor(index + 1)
                clear_mb_response_context()
                try:
                    result = await self._mb_repo.get_release_group_id_from_release(
                        mbid, source_context=operation_context
                    )
                except (OptionalWorkDeferred, ConfigurationError):
                    raise
                except Exception:  # noqa: BLE001
                    result = None
                response_context = get_mb_response_context() or operation_context
                if response_context != operation_context or not is_mb_source_current(operation_context):
                    return abort_for_source_change()
                rg_mbid = self.normalize_mbid(result)
                if not rg_mbid:
                    clear_mb_response_context()
                    # The release probe above accepts arbitrary Last.fm input,
                    # but this RG fallback probe needs a lookup-shaped id:
                    # invalid strings skip the wire.
                    checked = None
                    if is_valid_mbid(mbid):
                        try:
                            checked = await self._mb_repo.get_release_group_by_id(
                                mbid, includes=["artist-credits"],
                                priority=RequestPriority.BACKGROUND_SYNC,
                                source_context=operation_context,
                            )
                        except (OptionalWorkDeferred, ConfigurationError):
                            raise
                        except Exception:  # noqa: BLE001
                            checked = None
                    response_context = get_mb_response_context() or operation_context
                    if response_context != operation_context or not is_mb_source_current(operation_context):
                        return abort_for_source_change()
                    if isinstance(checked, dict) and checked.get("id"):
                        rg_mbid = mbid
                if rg_mbid:
                    resolved[mbid] = rg_mbid
                    cache_value(mbid, rg_mbid)
                    await self._persist_resolutions(
                        {mbid: rg_mbid}, source_context=operation_context
                    )
            except OptionalWorkDeferred:
                raise
            except ConfigurationError:
                return abort_for_source_change()

        return (
            resolved
            if is_mb_source_current(operation_context)
            else abort_for_source_change()
        )

    async def _persist_resolutions(
        self,
        new_resolutions: dict[str, str | None],
        *,
        source_context=None,
    ) -> None:
        """Durably bank resolved LB->RG mappings under the source commit fence."""
        if not new_resolutions or not self._mb_canonical_store:
            return
        try:
            if source_context is None:
                return
            await self._mb_canonical_store.save_release_to_rg(
                {mbid: rg or "" for mbid, rg in new_resolutions.items()},
                source_context=source_context,
            )
        except OptionalWorkDeferred:
            raise
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
        user_id: str | None = None,
        work_key: str | None = None,
    ) -> list[DiscoverQueueItemLight]:
        all_album_mbids: list[str] = []
        for _, albums in artist_albums_pairs:
            all_album_mbids.extend(a.mbid for a in albums if a.mbid)
        rg_mbid_map = await self.resolve_lastfm_release_group_mbids(
            all_album_mbids,
            resolver_cache=resolver_cache,
            user_id=user_id,
            work_key=work_key or reason,
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
                artist_name = (
                    (album.artist_name or artist.name)
                    if use_album_artist_name
                    else artist.name
                )
                items.append(
                    DiscoverQueueItemLight(
                        release_group_mbid=rg_mbid,
                        album_name=album.name,
                        artist_name=artist_name,
                        artist_mbid=artist_mbid or "",
                        cover_url=f"/api/v1/covers/release-group/{rg_mbid}?size=500",
                        recommendation_reason=reason,
                        is_wildcard=is_wildcard,
                        in_library=False,
                    )
                )
                seen_rg_mbids.add(rg_mbid_lower)
        return items

    async def resolve_release_mbids(
        self,
        release_ids: list[str],
        *,
        user_id: str | None = None,
        work_key: str = "release-resolution",
    ) -> dict[str, str]:
        return await self.resolve_lastfm_release_group_mbids(
            release_ids,
            allow_passthrough=False,
            user_id=user_id,
            work_key=work_key,
        )

    async def get_library_album_mbids(
        self, library_configured: bool, candidate_ids: list[str] | None = None
    ) -> set[str]:
        if not library_configured:
            if self._library_db and candidate_ids is not None:
                try:
                    return await self._library_db.existing_library_mbids(candidate_ids)
                except OptionalWorkDeferred:
                    raise
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
        except OptionalWorkDeferred:
            raise
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
        except OptionalWorkDeferred:
            raise
        except Exception:  # noqa: BLE001
            logger.warning(
                "Failed to fetch user listened release groups from ListenBrainz"
            )
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
