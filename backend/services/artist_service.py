from contextvars import ContextVar

import asyncio
import copy
import logging
import msgspec
from typing import Any, Optional, TYPE_CHECKING
from api.v1.schemas.artist import (
    ArtistInfo,
    ArtistExtendedInfo,
    ArtistReleases,
    ExternalLink,
    ReleaseItem,
)
from repositories.protocols import (
    MusicBrainzRepositoryProtocol,
    LibraryRepositoryProtocol,
    WikidataRepositoryProtocol,
)
from services.preferences_service import PreferencesService
from services.artist_utils import (
    extract_tags,
    extract_aliases,
    extract_life_span,
    extract_external_links,
    categorize_release_groups,
    extract_wiki_info,
    build_base_artist_info,
)
from infrastructure.cache.cache_keys import (
    ARTIST_INFO_PREFIX,
    mb_artist_release_groups_key,
)
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.disk_cache import DiskMetadataCache
from infrastructure.degradation import try_get_degradation_context
from infrastructure.validators import validate_mbid
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.http.disconnect import DisconnectCallable, check_disconnected
from core.exceptions import (
    ClientDisconnectedError,
    ExternalServiceError,
    ResourceNotFoundError,
)
from infrastructure.resilience.retry import CircuitOpenError
from repositories.musicbrainz_base import (
    MbSourceContext,
    capture_mb_source_context,
    extract_artist_name,
    get_mb_source_generation,
    is_mb_source_current,
    mb_deduplicator,
    mb_publish_if_current,
    normalize_mb_id,
)
from services.audiodb_image_service import AudioDBImageService
from repositories.audiodb_models import AudioDBArtistImages
from core.task_registry import TaskRegistry

if TYPE_CHECKING:
    from infrastructure.persistence import LibraryDB
    from infrastructure.persistence.native_library_store import NativeLibraryStore
    from services.native.library_ownership_service import LibraryOwnershipService

logger = logging.getLogger(__name__)

_artist_source_context: ContextVar[MbSourceContext | None] = ContextVar(
    "artist_source_context", default=None
)


def _clear_release_group_warm_seed(
    task: "asyncio.Task[None]",
    seeds: dict[str, tuple[MbSourceContext, list[dict[str, Any]], int]],
    cache_key: str,
    source_context: MbSourceContext,
) -> None:
    """Drop a partial seed after its generation-specific walker settles."""
    del task
    state = seeds.get(cache_key)
    if state is not None and state[0] == source_context:
        seeds.pop(cache_key, None)


def _log_task_error(task: "asyncio.Task[None]") -> None:
    """A3/ST4: background release-group warming failures must surface in logs
    without ever touching a response path (coverart_repository pattern)."""
    if task.cancelled():
        return
    exc = task.exception()
    if exc is not None:
        logger.error("Background release-group warm failed", exc_info=exc)


def _log_deferred_disk_write_failure(task: "asyncio.Task[None]") -> None:
    """B3.1: the deferred artist disk mirror must never crash the response
    path - a failed/cancelled mirror only costs one disk re-fetch after
    restart, so warn-and-swallow is the correct disposition."""
    if task.cancelled():
        return
    exc = task.exception()
    if exc is not None:
        logger.warning("Deferred artist disk-cache write failed: %s", exc)


# (1000 release groups) so pathological artists don't hog the limiter.
_MAX_RG_PAGES = 10
# Keep every retained first-page seed bounded to the repository's canonical
# browse width; the full catalog lives only in the completion cache.
_MAX_RG_SEED_ITEMS = 100


def _artist_info_cache_key(artist_id: str, profile: str = "full") -> str:
    suffix = ":basic" if profile == "basic" else ""
    return f"{ARTIST_INFO_PREFIX}{normalize_mb_id(artist_id)}{suffix}"


def _artist_inflight_key(
    artist_id: str, source_context: MbSourceContext
) -> tuple[str, int]:
    return normalize_mb_id(artist_id), source_context.generation


def _release_group_warm_state_key(
    cache_key: str, source_context: MbSourceContext
) -> str:
    return f"{cache_key}:g{source_context.generation}"


class ArtistService:
    def __init__(
        self,
        mb_repo: MusicBrainzRepositoryProtocol,
        library_repo: LibraryRepositoryProtocol,
        wikidata_repo: WikidataRepositoryProtocol,
        preferences_service: PreferencesService,
        memory_cache: CacheInterface,
        disk_cache: DiskMetadataCache,
        audiodb_image_service: AudioDBImageService | None = None,
        audiodb_browse_queue: Any = None,
        library_db: "LibraryDB | None" = None,
        ownership_service: "LibraryOwnershipService | None" = None,
        native_library_store: "NativeLibraryStore | None" = None,
    ):
        self._mb_repo = mb_repo
        self._library_repo = library_repo
        self._wikidata_repo = wikidata_repo
        self._preferences_service = preferences_service
        self._cache = memory_cache
        self._disk_cache = disk_cache
        self._audiodb_image_service = audiodb_image_service
        self._audiodb_browse_queue = audiodb_browse_queue
        self._library_db = library_db
        self._ownership = ownership_service
        self._native_library_store = native_library_store
        self._artist_in_flight: dict[
            tuple[str, int], asyncio.Future[ArtistInfo]
        ] = {}
        self._artist_basic_in_flight: dict[
            tuple[str, int], asyncio.Future[ArtistInfo]
        ] = {}
        # B1: coalesces concurrent cold /extended renders onto one leader chain.
        self._artist_extended_in_flight: dict[
            tuple[str, int], asyncio.Future[ArtistExtendedInfo]
        ] = {}
        # Partial release-group seeds are retained only while their
        # generation-specific walker is alive. This prevents successive page-0
        # warming polls from re-browsing the same provider window.
        self._release_group_warm_seeds: dict[
            str, tuple[MbSourceContext, list[dict[str, Any]], int]
        ] = {}

    async def _get_library_cache_mbids(self) -> set[str]:
        if self._library_db is None:
            return set()
        try:
            raw = await self._library_db.get_all_album_mbids()
            return {m.lower() for m in raw if m}
        except Exception as e:  # noqa: BLE001
            logger.warning("Failed to read library cache MBIDs: %s", e)
            return set()

    async def _revalidate_library_status(self, artist_info: ArtistInfo) -> ArtistInfo:
        if self._ownership is not None:
            result = copy.deepcopy(artist_info)
            await self._refresh_library_flags(result)
            return result
        cache_mbids = await self._get_library_cache_mbids()
        try:
            library_mbids = await self._library_repo.get_library_mbids(
                include_release_ids=True
            )
        except Exception:  # noqa: BLE001
            library_mbids = set()
        all_mbids = library_mbids | cache_mbids
        if not all_mbids:
            return artist_info

        result = copy.deepcopy(artist_info)
        for release_list in (result.albums, result.singles, result.eps):
            if not release_list:
                continue
            for release in release_list:
                if isinstance(release, dict):
                    rid = (release.get("id") or "").lower()
                else:
                    rid = (release.id or "").lower()
                if not rid:
                    continue
                new_in_library = rid in all_mbids
                old_in_library = (
                    release.get("in_library", False)
                    if isinstance(release, dict)
                    else release.in_library
                )
                if new_in_library != old_in_library:
                    if isinstance(release, dict):
                        release["in_library"] = new_in_library
                        if new_in_library and release.get("requested"):
                            release["requested"] = False
                    else:
                        release.in_library = new_in_library
                        if new_in_library and release.requested:
                            release.requested = False

        artist_mbids = await self._get_library_artist_mbids()
        new_artist_in_library = (
            result.musicbrainz_id and result.musicbrainz_id.lower() in artist_mbids
        )
        if new_artist_in_library != result.in_library:
            result.in_library = new_artist_in_library

        return result

    async def _get_library_artist_mbids(self) -> set[str]:
        if self._library_db is None:
            return set()
        try:
            raw = await self._library_db.get_all_artist_mbids()
            return {m.lower() for m in raw if m}
        except Exception as e:  # noqa: BLE001
            logger.warning("Failed to read library artist cache MBIDs: %s", e)
            return set()

    async def _apply_audiodb_artist_images(
        self,
        artist_info: ArtistInfo,
        mbid: str,
        name: str | None,
        *,
        allow_fetch: bool = False,
        is_monitored: bool = False,
    ) -> ArtistInfo:
        if self._audiodb_image_service is None:
            return artist_info
        try:
            images: AudioDBArtistImages | None
            if allow_fetch:
                images = (
                    await self._audiodb_image_service.fetch_and_cache_artist_images(
                        mbid,
                        name,
                        is_monitored=is_monitored,
                    )
                )
            else:
                images = await self._audiodb_image_service.get_cached_artist_images(
                    mbid
                )
            if images is None or images.is_negative:
                if not allow_fetch and images is None and self._audiodb_browse_queue:
                    settings = self._preferences_service.get_advanced_settings()
                    if settings.audiodb_enabled:
                        await self._audiodb_browse_queue.enqueue(
                            "artist", mbid, name=name
                        )
                return artist_info
            if not artist_info.fanart_url and images.fanart_url:
                artist_info.fanart_url = images.fanart_url
            if not artist_info.banner_url and images.banner_url:
                artist_info.banner_url = images.banner_url
            if images.thumb_url:
                artist_info.thumb_url = images.thumb_url
            if images.fanart_url_2:
                artist_info.fanart_url_2 = images.fanart_url_2
            if images.fanart_url_3:
                artist_info.fanart_url_3 = images.fanart_url_3
            if images.fanart_url_4:
                artist_info.fanart_url_4 = images.fanart_url_4
            if images.wide_thumb_url:
                artist_info.wide_thumb_url = images.wide_thumb_url
            if images.logo_url:
                artist_info.logo_url = images.logo_url
            if images.clearart_url:
                artist_info.clearart_url = images.clearart_url
            if images.cutout_url:
                artist_info.cutout_url = images.cutout_url
        except Exception as e:  # noqa: BLE001
            logger.warning(
                "Failed to apply AudioDB images for artist %s: %s", mbid[:8], e
            )
        return artist_info

    async def get_artist_info(
        self,
        artist_id: str,
        library_artist_mbids: set[str] = None,
        library_album_mbids: dict[str, Any] = None,
    ) -> ArtistInfo:
        source_context = capture_mb_source_context()
        _artist_source_context.set(source_context)
        try:
            artist_id = normalize_mb_id(validate_mbid(artist_id, "artist"))
        except ValueError as e:
            logger.error(f"Invalid artist MBID: {e}")
            raise
        inflight_key = _artist_inflight_key(artist_id, source_context)
        try:
            cached = await self._get_cached_artist(artist_id, profile="full")
            if not is_mb_source_current(source_context):
                cached = None
            if cached:
                cached = await self._revalidate_library_status(cached)
                cached = await self._apply_audiodb_artist_images(
                    cached,
                    artist_id,
                    cached.name,
                    allow_fetch=False,
                    is_monitored=cached.in_library,
                )
                if not is_mb_source_current(source_context):
                    raise ExternalServiceError(
                        "MusicBrainz source changed during artist cache read"
                    )
                return cached

            existing = self._artist_in_flight.get(inflight_key)
            if existing is not None:
                result = await asyncio.shield(existing)
                if not is_mb_source_current(source_context):
                    raise ExternalServiceError(
                        "MusicBrainz source changed during artist lookup"
                    )
                return result

            loop = asyncio.get_running_loop()
            future: asyncio.Future[ArtistInfo] = loop.create_future()
            self._artist_in_flight[inflight_key] = future
            try:
                artist_info = await self._do_get_artist_info(
                    artist_id,
                    library_artist_mbids,
                    library_album_mbids,
                    source_context=source_context,
                )
                if not is_mb_source_current(source_context):
                    raise ExternalServiceError(
                        "MusicBrainz source changed during artist lookup"
                    )
                if not future.done():
                    future.set_result(artist_info)
                return artist_info
            except (ClientDisconnectedError, asyncio.CancelledError):
                if not future.done():
                    future.cancel()
                raise
            except BaseException as exc:
                if not future.done():
                    future.set_exception(exc)
                    # Mark the leader's exception retrieved before re-raising.
                    future.exception()
                raise
            finally:
                if not future.done():
                    future.cancel()
                self._artist_in_flight.pop(inflight_key, None)
        except (CircuitOpenError, ExternalServiceError, ClientDisconnectedError):
            raise
        except (ValueError, ResourceNotFoundError):
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"API call failed for artist {artist_id}: {e}")
            raise ResourceNotFoundError("Failed to get artist info") from e

    async def _do_get_artist_info(
        self,
        artist_id: str,
        library_artist_mbids: set[str] | None,
        library_album_mbids: dict[str, Any] | None,
        *,
        source_context: MbSourceContext,
    ) -> ArtistInfo:
        try:
            artist_info = await self._build_artist_from_musicbrainz(
                artist_id,
                library_artist_mbids,
                library_album_mbids,
                source_context=source_context,
            )
        except (CircuitOpenError, ExternalServiceError):
            # Use local rows only while MusicBrainz is unavailable.
            local_info = await self._build_artist_info_from_local(artist_id)
            if local_info is not None:
                logger.warning(
                    "Artist detail artist=%s source=local-degraded (musicbrainz unavailable)",
                    artist_id[:8],
                )
                return local_info
            raise
        await self._refresh_library_flags(artist_info)
        artist_info = await self._apply_audiodb_artist_images(
            artist_info,
            artist_id,
            artist_info.name,
            allow_fetch=False,
            is_monitored=artist_info.in_library,
        )
        await self._save_artist_to_cache(artist_id, artist_info)
        return artist_info

    async def _build_artist_info_from_local(self, artist_id: str) -> ArtistInfo | None:
        """Degraded-mode artist payload built purely from local catalog rows.

        Serves locally known artists when MusicBrainz cannot answer. Nothing
        here consults MB, so the result is never cached under the MB-derived
        key. Releases stay empty: the page's album grid is library-backed
        elsewhere, and the MB discography degrades to empty on its own.
        """
        if self._native_library_store is None:
            return None
        artist_rows, _total = await self._native_library_store.list_target_artists(
            limit=1, artist_ids=[artist_id], scope="all"
        )
        if not artist_rows:
            return None
        row = artist_rows[0]
        provider_artist_mbid = row.get("provider_artist_mbid")
        return ArtistInfo(
            name=str(row["artist_name"] or ""),
            musicbrainz_id=(
                str(provider_artist_mbid) if provider_artist_mbid else artist_id
            ),
            in_library=True,
            appears_in_library=True,
            release_group_count=int(row.get("album_count") or 0),
            service_status=None,
        )

    async def _build_artist_from_musicbrainz(
        self,
        artist_id: str,
        library_artist_mbids: set[str] = None,
        library_album_mbids: dict[str, Any] = None,
        include_extended: bool = True,
        include_releases: bool = True,
        *,
        source_context: MbSourceContext,
    ) -> ArtistInfo:
        (
            mb_artist,
            library_mbids,
            album_mbids,
            requested_mbids,
        ) = await self._fetch_artist_data(
            artist_id,
            library_artist_mbids,
            library_album_mbids,
            include_releases=include_releases,
            source_context=source_context,
        )
        in_library = artist_id.lower() in library_mbids
        albums, singles, eps = (
            (
                await self._get_categorized_releases(
                    mb_artist, album_mbids, requested_mbids
                )
            )
            if include_releases
            else ([], [], [])
        )
        description, image = (
            (await self._fetch_wikidata_info(mb_artist))
            if include_extended
            else (None, None)
        )
        info = build_base_artist_info(
            mb_artist,
            artist_id,
            in_library,
            extract_tags(mb_artist),
            extract_aliases(mb_artist),
            extract_life_span(mb_artist),
            self._build_external_links(mb_artist),
            albums,
            singles,
            eps,
            description,
            image,
        )
        return ArtistInfo(**info)

    async def get_artist_info_basic(self, artist_id: str) -> ArtistInfo:
        source_context = capture_mb_source_context()
        _artist_source_context.set(source_context)
        artist_id = normalize_mb_id(validate_mbid(artist_id, "artist"))
        inflight_key = _artist_inflight_key(artist_id, source_context)
        cached = await self._get_cached_artist(artist_id, profile="basic")
        if not is_mb_source_current(source_context):
            cached = None
        if cached:
            # B3.1: refresh (library flags on releases + artist-level flags)
            # and AudioDB image application (fanart/thumb/logo URL fields)
            # mutate DISJOINT fields of the same object; under cooperative
            # asyncio their field writes interleave but never parallelize.
            await asyncio.gather(
                self._apply_audiodb_artist_images(
                    cached,
                    artist_id,
                    cached.name,
                    allow_fetch=False,
                ),
                self._refresh_library_flags(cached),
            )
            if not is_mb_source_current(source_context):
                raise ExternalServiceError(
                    "MusicBrainz source changed during basic artist cache read"
                )
            return cached

        existing = self._artist_basic_in_flight.get(inflight_key)
        if existing is not None:
            result = await asyncio.shield(existing)
            if not is_mb_source_current(source_context):
                raise ExternalServiceError(
                    "MusicBrainz source changed during basic artist lookup"
                )
            return result

        loop = asyncio.get_running_loop()
        future: asyncio.Future[ArtistInfo] = loop.create_future()
        self._artist_basic_in_flight[inflight_key] = future
        try:
            try:
                artist_info = await self._build_artist_from_musicbrainz(
                    artist_id,
                    include_extended=False,
                    include_releases=False,
                    source_context=source_context,
                )
            except (CircuitOpenError, ExternalServiceError):
                # Use local rows only while MusicBrainz is unavailable.
                local_info = await self._build_artist_info_from_local(artist_id)
                if local_info is not None:
                    logger.warning(
                        "Artist info artist=%s source=local-degraded (musicbrainz unavailable)",
                        artist_id[:8],
                    )
                    if not future.done():
                        future.set_result(local_info)
                    return local_info
                raise
            # B3.1: same disjoint-field gather as the cached path above.
            await asyncio.gather(
                self._refresh_library_flags(artist_info),
                self._apply_audiodb_artist_images(
                    artist_info,
                    artist_id,
                    artist_info.name,
                    allow_fetch=False,
                ),
            )
            await self._save_artist_to_cache(
                artist_id, artist_info, profile="basic"
            )
            if not is_mb_source_current(source_context):
                raise ExternalServiceError(
                    "MusicBrainz source changed during basic artist lookup"
                )
            if not future.done():
                future.set_result(artist_info)
            return artist_info
        except (ClientDisconnectedError, asyncio.CancelledError):
            if not future.done():
                future.cancel()
            raise
        except BaseException as exc:
            if not future.done():
                future.set_exception(exc)
                # Mark the leader's exception retrieved before re-raising.
                future.exception()
            raise
        finally:
            if not future.done():
                future.cancel()
            self._artist_basic_in_flight.pop(inflight_key, None)

    async def _refresh_library_flags(self, artist_info: ArtistInfo) -> None:
        if not self._library_repo.is_configured():
            return
        try:
            if self._ownership is not None:
                from services.native.library_ownership_service import (
                    AlbumOwnershipCandidate,
                )

                releases = [
                    release
                    for release_list in (
                        artist_info.albums,
                        artist_info.singles,
                        artist_info.eps,
                    )
                    for release in release_list
                ]
                projections = await self._ownership.project_albums(
                    [
                        AlbumOwnershipCandidate(
                            release_group_mbid=release.id,
                            title=release.title or "",
                            album_artist=artist_info.name,
                            year=release.year,
                        )
                        for release in releases
                    ]
                )
                release_ids = [release.id for release in releases if release.id]
                requested_mbids = await self._library_repo.get_requested_mbids(
                    release_ids
                )
                for release, projection in zip(releases, projections):
                    release.in_library = projection.owned
                    release.requested = bool(
                        release.id
                        and release.id.casefold() in requested_mbids
                        and not projection.owned
                    )
                (
                    artist_info.in_library,
                    artist_info.appears_in_library,
                ) = await self._ownership.provider_artist_relationship(
                    artist_info.musicbrainz_id
                )
                return
            library_mbids, requested_mbids, artist_mbids = await asyncio.gather(
                self._library_repo.get_library_mbids(include_release_ids=False),
                self._library_repo.get_requested_mbids(),
                self._library_repo.get_artist_mbids(),
            )
            for release_list in (
                artist_info.albums,
                artist_info.singles,
                artist_info.eps,
            ):
                for rg in release_list:
                    rg_id = (rg.id or "").lower()
                    if not rg_id:
                        continue
                    rg.in_library = rg_id in library_mbids
                    rg.requested = rg_id in requested_mbids and not rg.in_library
            mbid_lower = artist_info.musicbrainz_id.lower()
            artist_info.in_library = mbid_lower in artist_mbids
            artist_info.appears_in_library = False
        except Exception as e:  # noqa: BLE001
            logger.warning(f"Failed to refresh library flags: {e}")

    async def _get_cached_artist(
        self, artist_id: str, *, profile: str = "full"
    ) -> Optional[ArtistInfo]:
        artist_id = normalize_mb_id(artist_id)
        cache_key = _artist_info_cache_key(artist_id, profile)
        cached_info = await self._cache.get(cache_key)
        if cached_info:
            return cached_info
        disk_data = await self._disk_cache.get_artist(artist_id, profile=profile)
        if disk_data:
            try:
                artist_info = msgspec.convert(disk_data, ArtistInfo, strict=False)
            except (msgspec.ValidationError, TypeError, ValueError) as e:
                logger.warning(
                    f"Corrupt disk cache for artist {artist_id[:8]}, clearing: {e}"
                )
                await self._disk_cache.delete_artist(artist_id, profile=profile)
                return None
            return artist_info
        return None

    async def _save_artist_to_cache(
        self, artist_id: str, artist_info: ArtistInfo, *, profile: str = "full"
    ) -> None:
        artist_id = normalize_mb_id(artist_id)
        cache_key = _artist_info_cache_key(artist_id, profile)
        ttl = self._get_artist_ttl(artist_info.in_library)
        context = _artist_source_context.get()
        # B3.1: memory write stays inline - coalesced followers and the next
        # request read it. The disk mirror remains deferred, but both tiers
        # carry the captured source context so a source switch cannot admit a
        # delayed stale write.
        published = await mb_publish_if_current(
            context,
            lambda: self._cache.set(cache_key, artist_info, ttl_seconds=ttl),
        )
        if not published:
            return

        async def publish_disk() -> None:
            await mb_publish_if_current(
                context,
                lambda: self._disk_cache.set_artist(
                    artist_id,
                    artist_info,
                    is_monitored=artist_info.in_library,
                    ttl_seconds=ttl if not artist_info.in_library else None,
                    profile=profile,
                ),
            )

        task = asyncio.create_task(publish_disk())
        task.add_done_callback(_log_deferred_disk_write_failure)

    def _get_artist_ttl(self, in_library: bool) -> int:
        advanced_settings = self._preferences_service.get_advanced_settings()
        return (
            advanced_settings.cache_ttl_artist_library
            if in_library
            else advanced_settings.cache_ttl_artist_non_library
        )

    async def get_artist_extended_info(self, artist_id: str) -> ArtistExtendedInfo:
        source_context = capture_mb_source_context()
        _artist_source_context.set(source_context)
        try:
            artist_id = normalize_mb_id(validate_mbid(artist_id, "artist"))
            cache_key = _artist_info_cache_key(artist_id, "full")
            cached_info = await self._cache.get(cache_key)
            if not is_mb_source_current(source_context):
                cached_info = None
            if cached_info and cached_info.description is not None:
                return ArtistExtendedInfo(
                    description=cached_info.description, image=cached_info.image
                )

            # B1: coalesce concurrent cold renders - K viewers of a wiki-less
            # or cold artist share ONE leader chain; followers await (shielded)
            # the identical object the leader built. Lifecycle mirrors
            # get_artist_info_basic above.
            inflight_key = _artist_inflight_key(artist_id, source_context)
            existing = self._artist_extended_in_flight.get(inflight_key)
            if existing is not None:
                result = await asyncio.shield(existing)
                if not is_mb_source_current(source_context):
                    return ArtistExtendedInfo(description=None, image=None)
                return result

            loop = asyncio.get_running_loop()
            future: asyncio.Future[ArtistExtendedInfo] = loop.create_future()
            self._artist_extended_in_flight[inflight_key] = future
            try:
                # B1: url-rels only. extract_wiki_info reads just
                # mb_artist["relations"], so the full detail fetch (2 wire
                # calls when the detail entry expired) was pure waste;
                # get_artist_relations serves the same need from a 24 h
                # relations cache. Lane moves USER_INITIATED -> IMAGE_FETCH
                # (_fetch_artist_relations pins IMAGE_FETCH) - this is a
                # cosmetic enrichment leg, not primary content.
                mb_artist = await self._mb_repo.get_artist_relations(artist_id)
                if not is_mb_source_current(source_context):
                    raise ExternalServiceError(
                        "MusicBrainz source changed during extended artist lookup"
                    )
                if not mb_artist:
                    raise ResourceNotFoundError("Artist not found")
                description, image = await self._fetch_wikidata_info(mb_artist)
                if not is_mb_source_current(source_context):
                    raise ExternalServiceError(
                        "MusicBrainz source changed during extended artist lookup"
                    )
                if cached_info:
                    cached_info.description = description
                    cached_info.image = image
                    await self._save_artist_to_cache(artist_id, cached_info)
                result = ArtistExtendedInfo(description=description, image=image)
                if not is_mb_source_current(source_context):
                    raise ExternalServiceError(
                        "MusicBrainz source changed during extended artist lookup"
                    )
                if not future.done():
                    future.set_result(result)
                return result
            except BaseException as exc:
                if not future.done():
                    future.set_exception(exc)
                    future.exception()  # mark retrieved: no orphan-log when leader alone
                raise
            finally:
                self._artist_extended_in_flight.pop(inflight_key, None)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Error fetching extended artist info for {artist_id}: {e}")
            return ArtistExtendedInfo(description=None, image=None)

    async def get_artist_releases(
        self,
        artist_id: str,
        offset: int = 0,
        limit: int = 50,
        *,
        is_disconnected: DisconnectCallable | None = None,
    ) -> ArtistReleases:
        source_context = capture_mb_source_context()
        _artist_source_context.set(source_context)
        artist_id = normalize_mb_id(artist_id)
        try:
            await check_disconnected(is_disconnected)
            album_mbids: set[str] = set()
            requested_mbids: set[str] = set()
            if self._ownership is None:
                album_mbids, requested_mbids, cache_mbids = await asyncio.gather(
                    self._library_repo.get_library_mbids(include_release_ids=True),
                    self._library_repo.get_requested_mbids(),
                    self._get_library_cache_mbids(),
                )
                album_mbids = album_mbids | cache_mbids

            prefs = self._preferences_service.get_preferences()
            included_primary_types = set(t.lower() for t in prefs.primary_types)
            included_secondary_types = set(t.lower() for t in prefs.secondary_types)

            result = await self._filter_aware_release_page(
                artist_id,
                offset,
                limit,
                album_mbids,
                requested_mbids,
                included_primary_types,
                included_secondary_types,
                is_disconnected,
                source_context,
            )
            if not is_mb_source_current(source_context):
                raise ExternalServiceError(
                    "MusicBrainz source changed during artist release lookup"
                )
            if (
                result.returned_count == 0
                and result.source_total_count == 0
                and not result.warming
            ):
                # get_artist_release_groups collapses every MB failure into
                # ([], 0): an outage looks like an empty catalog. Locally
                # owned artists serve their local discography instead.
                # Not cached.
                local = await self._build_artist_releases_from_local(
                    artist_id, offset, limit
                )
                if local is not None:
                    logger.warning(
                        "Artist releases artist=%s source=local-degraded (musicbrainz unavailable)",
                        artist_id[:8],
                    )
                    if not is_mb_source_current(source_context):
                        raise ExternalServiceError(
                            "MusicBrainz source changed during local artist release fallback"
                        )
                    return local
            if not is_mb_source_current(source_context):
                raise ExternalServiceError(
                    "MusicBrainz source changed during artist release lookup"
                )
            return result
        except ClientDisconnectedError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(
                f"Error fetching releases for artist {artist_id} at offset {offset}: {e}"
            )
            return ArtistReleases(
                albums=[],
                singles=[],
                eps=[],
                offset=offset,
                limit=limit,
                returned_count=0,
                next_offset=None,
                has_more=False,
                source_total_count=None,
            )

    async def _build_artist_releases_from_local(
        self, artist_id: str, offset: int, limit: int
    ) -> ArtistReleases | None:
        """Degraded-mode discography built purely from local catalog rows.

        Serves the in-library slice of an artist's discography when MB cannot
        answer. Type buckets follow each album's primary identity (compilations
        count as albums); the external-only part of the catalog reappears when
        MB does. Requested flags are unknown here (the MB catalog is what
        carries them) and stay False.
        """
        if self._native_library_store is None:
            return None
        artist_rows, _total = await self._native_library_store.list_target_artists(
            limit=1, artist_ids=[artist_id], scope="all"
        )
        if not artist_rows:
            return None
        rows, _albums_total = await self._native_library_store.list_target_albums(
            limit=10_000,
            offset=0,
            sort="name",
            artist_id=str(artist_rows[0]["artist_mbid"]),
        )
        if not rows:
            return None

        albums: list[ReleaseItem] = []
        eps: list[ReleaseItem] = []
        singles: list[ReleaseItem] = []
        for row in rows:
            rg_id = row.get("provider_release_group_mbid") or str(
                row["release_group_mbid"]
            )
            release_date = row.get("original_release_date")
            year = row.get("year")
            if year is None and release_date:
                head = str(release_date)[:4]
                year = int(head) if head.isdigit() else None
            track_count = int(row.get("track_count") or 0)
            # A single-track local row is usually a single; type is display
            # metadata only, so bucket heuristics are fine here.
            bucket = albums
            item_type = "Album"
            if row.get("is_compilation"):
                item_type = "Album"
            elif track_count <= 2:
                bucket = singles
                item_type = "Single"
            elif track_count <= 6:
                bucket = eps
                item_type = "EP"
            bucket.append(
                ReleaseItem(
                    id=rg_id,
                    title=str(row["album_title"]),
                    type=item_type,
                    first_release_date=str(release_date) if release_date else None,
                    year=int(year) if year is not None else None,
                    in_library=True,
                )
            )
        for lst in (albums, eps, singles):
            lst.sort(key=lambda x: (x.year is None, -(x.year or 0)))
        tagged: list[tuple[str, ReleaseItem]] = (
            [("albums", item) for item in albums]
            + [("eps", item) for item in eps]
            + [("singles", item) for item in singles]
        )
        page = tagged[offset : offset + limit]
        next_offset = offset + limit if offset + limit < len(tagged) else None
        return ArtistReleases(
            albums=[item for kind, item in page if kind == "albums"],
            singles=[item for kind, item in page if kind == "singles"],
            eps=[item for kind, item in page if kind == "eps"],
            offset=offset,
            limit=limit,
            returned_count=len(page),
            next_offset=next_offset,
            has_more=next_offset is not None,
            source_total_count=len(tagged),
        )

    async def _filter_aware_release_page(
        self,
        artist_id: str,
        offset: int,
        limit: int,
        album_mbids: set[str],
        requested_mbids: set[str],
        included_primary_types: set[str],
        included_secondary_types: set[str],
        is_disconnected: DisconnectCallable | None,
        source_context: MbSourceContext,
    ) -> ArtistReleases:
        if not included_primary_types:
            return ArtistReleases(
                albums=[],
                singles=[],
                eps=[],
                offset=offset,
                limit=limit,
                returned_count=0,
                next_offset=None,
                has_more=False,
                source_total_count=None,
            )

        full_list, complete = await self._fetch_all_release_groups(
            artist_id, is_disconnected, source_context
        )
        warming = not complete

        if self._ownership is not None:
            album_mbids, requested_mbids = await self._target_release_group_flags(
                full_list, artist_name=""
            )

        albums, singles, eps = categorize_release_groups(
            {"release-group-list": full_list},
            album_mbids,
            included_primary_types,
            included_secondary_types,
            requested_mbids,
        )
        # Stream order = UI section order; categorize_release_groups already
        # sorts each bucket by year desc, so don't re-sort here.

        tagged: list[tuple[str, ReleaseItem]] = (
            [("albums", item) for item in albums]
            + [("eps", item) for item in eps]
            + [("singles", item) for item in singles]
        )

        page = tagged[offset : offset + limit]
        page_albums = [item for kind, item in page if kind == "albums"]
        page_singles = [item for kind, item in page if kind == "singles"]
        page_eps = [item for kind, item in page if kind == "eps"]

        next_offset = offset + limit if offset + limit < len(tagged) else None
        # A3/ST4: while the catalog is only partially known (walker running),
        # a partial total would render "N of N" and hide Load-more - report
        # null instead, plus the explicit warming flag.
        return ArtistReleases(
            albums=page_albums,
            singles=page_singles,
            eps=page_eps,
            offset=offset,
            limit=limit,
            returned_count=len(page),
            next_offset=next_offset,
            has_more=next_offset is not None,
            source_total_count=None if warming else len(tagged),
            warming=warming,
        )

    async def _get_artist_release_groups_with_context(
        self,
        artist_id: str,
        offset: int,
        limit: int,
        *,
        priority: RequestPriority,
        preserve_fetch_width: bool = False,
    ) -> tuple[list[dict[str, Any]], int, MbSourceContext | None]:
        return await self._mb_repo.get_artist_release_groups_with_context(
            artist_id,
            offset,
            limit,
            priority=priority,
            preserve_fetch_width=preserve_fetch_width,
        )

    async def _fetch_all_release_groups(
        self,
        artist_id: str,
        is_disconnected: DisconnectCallable | None,
        source_context: MbSourceContext,
    ) -> tuple[list[dict[str, Any]], bool]:
        """Cached, request-coalesced release-group browse (A3/ST4).

        Returns ``(known_slice, complete)``. A catalog that fits page 1 is
        cached and returned complete - byte-identical to the pre-A3 walk.
        Larger catalogs return only the first page with ``complete=False``
        while a background walker finishes the remaining pages; the shared
        key stays unwritten until that walk completes (outage-safety rule).
        Raw MB dicts only; in_library/requested flags are recomputed per
        request from library state, so library changes never invalidate it.
        """
        artist_id = normalize_mb_id(artist_id)
        cache_key = mb_artist_release_groups_key(artist_id)
        cached = await self._cache.get(cache_key)
        if not is_mb_source_current(source_context):
            raise ExternalServiceError(
                "MusicBrainz source changed during release-group cache read"
            )
        if cached is not None:
            return cached, True

        state_key = _release_group_warm_state_key(cache_key, source_context)
        warm_state = self._release_group_warm_seeds.get(state_key)
        if warm_state is not None:
            warm_context, seed_items, _total = warm_state
            if (
                warm_context == source_context
                and is_mb_source_current(warm_context)
            ):
                return list(seed_items), False
            # Source switches use a monotonically increasing generation. Do
            # not let a stale task's seed block the first request on a new
            # source; its done callback will preserve any replacement state.
            self._release_group_warm_seeds.pop(state_key, None)

        dedupe_key = f"{cache_key}:g{source_context.generation}"
        page_items, total, response_context = await mb_deduplicator.dedupe(
            dedupe_key,
            lambda: self._fetch_first_release_group_page(
                artist_id, is_disconnected, source_context
            ),
        )
        if (
            not is_mb_source_current(source_context)
            or response_context is not None and response_context != source_context
        ):
            raise ExternalServiceError(
                "MusicBrainz source changed during release-group browse"
            )
        if total > 0 and len(page_items) >= total:
            await mb_publish_if_current(
                source_context,
                lambda: self._cache.set(
                    cache_key,
                    page_items,
                    ttl_seconds=self._get_artist_ttl(in_library=False),
                ),
            )
            return page_items, True
        if not page_items and not total:
            # Definitive zero-RG catalog: complete, not warming.
            return page_items, True

        self._spawn_release_group_warm(
            artist_id,
            page_items,
            total,
            raw_offset=len(page_items),
            source_context=source_context,
        )
        return page_items, False

    async def _fetch_first_release_group_page(
        self,
        artist_id: str,
        is_disconnected: DisconnectCallable | None,
        source_context: MbSourceContext,
    ) -> tuple[list[dict[str, Any]], int, MbSourceContext | None]:
        """Offset-0 browse page (limit 100) at USER_INITIATED - the only leg
        still riding the user's request. First-wins id dedupe survives MB
        page re-sorting; the caller decides completeness from ``total``."""
        await check_disconnected(is_disconnected)
        (
            release_groups,
            total,
            response_context,
        ) = await self._get_artist_release_groups_with_context(
            artist_id,
            0,
            100,
            priority=RequestPriority.USER_INITIATED,
            preserve_fetch_width=True,
        )
        collected: dict[str, dict[str, Any]] = {}
        for group in release_groups or []:
            group_id = group.get("id")
            if not group_id:
                continue
            collected.setdefault(str(group_id).casefold(), group)
        return list(collected.values()), total or 0, response_context or source_context

    def _spawn_release_group_warm(
        self,
        artist_id: str,
        seed_items: list[dict[str, Any]],
        total: int,
        *,
        raw_offset: int | None = None,
        source_context: MbSourceContext,
    ) -> None:
        """Spawn one bounded, generation-specific background walker.

        The seed is kept only for the walk lifetime, so page-0 retries can
        reuse it without issuing another browse. A source generation in the
        task name lets a new source start its own walker while an old one
        exits safely.
        """
        registry = TaskRegistry.get_instance()
        task_name = f"mb-rg-warm-{artist_id.casefold()}:{source_context.generation}"
        if registry.is_running(task_name):
            return
        collected: dict[str, dict[str, Any]] = {}
        bounded_seed = seed_items[:_MAX_RG_SEED_ITEMS]
        for group in bounded_seed:
            group_id = group.get("id")
            if group_id:
                collected.setdefault(str(group_id).casefold(), group)
        if raw_offset is None:
            raw_offset = len(bounded_seed)
        task = asyncio.create_task(
            self._warm_release_group_pages(
                artist_id,
                collected,
                total,
                raw_offset=raw_offset,
                source_context=source_context,
            )
        )
        task.add_done_callback(_log_task_error)
        try:
            registry.register(task_name, task)
        except RuntimeError:
            task.cancel()
            return
        cache_key = mb_artist_release_groups_key(normalize_mb_id(artist_id))
        state_key = _release_group_warm_state_key(cache_key, source_context)
        self._release_group_warm_seeds[state_key] = (
            source_context,
            list(bounded_seed),
            total,
        )
        task.add_done_callback(
            lambda done: _clear_release_group_warm_seed(
                done,
                self._release_group_warm_seeds,
                state_key,
                source_context,
            )
        )

    async def _warm_release_group_pages(
        self,
        artist_id: str,
        collected: dict[str, dict[str, Any]],
        total: int,
        *,
        raw_offset: int,
        source_context: MbSourceContext | None = None,
    ) -> None:
        """A3 Part 2 / ST4: finish the browse walk off the user's critical
        path on the repo's default BACKGROUND_SYNC lane (yields to interactive
        traffic via the 2 s gate instead of competing with it).

        The shared cache key is written ONLY when the walk completed
        (total > 0 and raw_offset >= total): CancelledError or any failure
        breaks out leaving the key untouched, exactly like today's failed
        walk - an outage can never pin a truncated catalog.
        """
        artist_id = normalize_mb_id(artist_id)
        source_context = source_context or _artist_source_context.get() or capture_mb_source_context()
        if not is_mb_source_current(source_context):
            return
        cache_key = mb_artist_release_groups_key(artist_id)
        pages_done = 1 if raw_offset else 0

        while pages_done < _MAX_RG_PAGES and raw_offset < max(total, 1):
            if not is_mb_source_current(source_context):
                return
            try:
                (
                    release_groups,
                    mb_total,
                    response_context,
                ) = await self._get_artist_release_groups_with_context(
                    artist_id,
                    raw_offset,
                    100,
                    priority=RequestPriority.BACKGROUND_SYNC,
                )
            except asyncio.CancelledError:
                logger.info("Release-group warm cancelled for %s", artist_id[:8])
                return
            except Exception:
                logger.error(
                    "Release-group warm failed for %s", artist_id[:8], exc_info=True
                )
                return
            if (
                not is_mb_source_current(source_context)
                or response_context is not None
                and response_context != source_context
            ):
                return

            total = mb_total or total
            if not release_groups:
                break
            for group in release_groups:
                group_id = group.get("id")
                if not group_id:
                    continue
                collected.setdefault(str(group_id).casefold(), group)
            raw_offset += len(release_groups)
            pages_done += 1
            if raw_offset >= total:
                break

        if total > 0 and raw_offset >= total:
            full_list = list(collected.values())
            await mb_publish_if_current(
                source_context,
                lambda: self._cache.set(
                    cache_key,
                    full_list,
                    ttl_seconds=self._get_artist_ttl(in_library=False),
                ),
            )

    async def _fetch_artist_data(
        self,
        artist_id: str,
        library_artist_mbids: set[str] = None,
        library_album_mbids: dict[str, Any] = None,
        *,
        include_releases: bool = True,
        source_context: MbSourceContext,
    ) -> tuple[dict, set[str], set[str], set[str]]:
        artist_fetch_kwargs: dict[str, Any] = {"include_releases": include_releases}
        if include_releases:
            artist_fetch_kwargs["release_group_limit"] = _MAX_RG_SEED_ITEMS
        if library_artist_mbids is not None and library_album_mbids is not None:
            # B3.1: cache-mbid read rides alongside the requested fetch instead
            # of a serial tail after the main gather.
            mb_artist = await self._mb_repo.get_artist_by_id(
                artist_id, **artist_fetch_kwargs
            )
            library_mbids = library_artist_mbids
            album_mbids = library_album_mbids
            try:
                requested_mbids, cache_mbids = await asyncio.gather(
                    self._library_repo.get_requested_mbids(),
                    self._get_library_cache_mbids(),
                )
                album_mbids = album_mbids | cache_mbids
            except Exception as exc:  # noqa: BLE001
                logger.warning(
                    f"Lidarr unavailable, proceeding without requested data: {exc}"
                )
                requested_mbids = set()
        elif self._ownership is not None:
            # B3.1: cache-mbid read joins the gather (spare width).
            mb_artist, artist_relationship, cache_mbids = await asyncio.gather(
                self._mb_repo.get_artist_by_id(artist_id, **artist_fetch_kwargs),
                self._ownership.provider_artist_relationship(artist_id),
                self._get_library_cache_mbids(),
            )
            if not mb_artist:
                raise ResourceNotFoundError("Artist not found")
            library_mbids = {artist_id.casefold()} if artist_relationship[0] else set()
            if include_releases:
                album_mbids, requested_mbids = await self._target_release_group_flags(
                    mb_artist.get("release-group-list", []),
                    artist_name=mb_artist.get("name") or "",
                )
            else:
                album_mbids, requested_mbids = set(), set()
            album_mbids = album_mbids | cache_mbids
        else:
            # B3.1: cache-mbid read joins the gather (4-wide already had room;
            # failures degrade to an empty set like the other library reads).
            mb_artist, *library_results = await asyncio.gather(
                self._mb_repo.get_artist_by_id(artist_id, **artist_fetch_kwargs),
                self._library_repo.get_artist_mbids(),
                self._library_repo.get_library_mbids(include_release_ids=True),
                self._library_repo.get_requested_mbids(),
                self._get_library_cache_mbids(),
                return_exceptions=True,
            )
            if isinstance(mb_artist, BaseException):
                if isinstance(
                    mb_artist,
                    (
                        CircuitOpenError,
                        ExternalServiceError,
                        ResourceNotFoundError,
                        ClientDisconnectedError,
                        asyncio.CancelledError,
                    ),
                ):
                    raise mb_artist
                if isinstance(mb_artist, Exception):
                    logger.error(
                        "Error fetching artist data for %s",
                        artist_id,
                        exc_info=mb_artist,
                    )
                    raise ExternalServiceError(
                        "MusicBrainz artist metadata is temporarily unavailable."
                    ) from mb_artist
                raise mb_artist
            library_failed = any(isinstance(r, BaseException) for r in library_results)
            if library_failed:
                logger.warning(
                    f"Lidarr unavailable for artist {artist_id}, proceeding with MusicBrainz data only"
                )
            library_mbids = (
                library_results[0]
                if not isinstance(library_results[0], BaseException)
                else set()
            )
            album_mbids = (
                library_results[1]
                if not isinstance(library_results[1], BaseException)
                else set()
            )
            requested_mbids = (
                library_results[2]
                if not isinstance(library_results[2], BaseException)
                else set()
            )
            cache_mbids = (
                library_results[3]
                if not isinstance(library_results[3], BaseException)
                else set()
            )
            album_mbids = album_mbids | cache_mbids

        if not mb_artist:
            raise ResourceNotFoundError("Artist not found")

        # A3/ST4 Part 3: only full-detail consumers may seed the release-group
        # walker. Basic metadata callers receive their detail fields and count
        # without triggering a release browse/warm side effect.
        if include_releases:
            embedded = (
                mb_artist.get("release-group-list")
                if isinstance(mb_artist, dict)
                else None
            ) or []
            rg_count = (
                int(mb_artist.get("release-group-count") or 0)
                if isinstance(mb_artist, dict)
                else 0
            )
            if embedded:
                try:
                    await self._seed_release_group_warm_from_embedded(
                        artist_id,
                        embedded,
                        rg_count,
                        source_context=source_context,
                        raw_offset=len(embedded),
                    )
                except Exception:  # noqa: BLE001 - warming must never break the build
                    logger.warning(
                        "Failed to seed release-group warm for %s",
                        artist_id[:8],
                        exc_info=True,
                    )

        if include_releases and isinstance(mb_artist, dict):
            embedded = mb_artist.get("release-group-list") or []
            if len(embedded) > 50:
                mb_artist = dict(mb_artist)
                mb_artist["release-group-list"] = embedded[:50]
        return mb_artist, library_mbids, album_mbids, requested_mbids

    async def _seed_release_group_warm_from_embedded(
        self,
        artist_id: str,
        embedded: list[dict[str, Any]],
        rg_count: int,
        *,
        source_context: MbSourceContext,
        raw_offset: int,
    ) -> None:
        artist_id = normalize_mb_id(artist_id)
        cache_key = mb_artist_release_groups_key(artist_id)
        cached = await self._cache.get(cache_key)
        if not is_mb_source_current(source_context):
            return
        if cached is not None:
            return  # already fully cached by an earlier walk
        # _spawn_release_group_warm owns generation-specific deduplication;
        # an old-source task must not block a new-source seed.
        seed_items = [
            g
            for g in embedded[:_MAX_RG_SEED_ITEMS]
            if isinstance(g, dict) and g.get("id")
        ]
        if not seed_items:
            return

        if rg_count > 0 and len(seed_items) >= rg_count:
            # Catalog fits the embedded page: complete inline - no task,
            # byte-identical write to what the old walk would have cached.
            deduped: dict[str, dict[str, Any]] = {}
            for group in seed_items:
                gid = str(group["id"]).casefold()
                deduped.setdefault(gid, group)
            published = await mb_publish_if_current(
                source_context,
                lambda: self._cache.set(
                    cache_key,
                    list(deduped.values()),
                    ttl_seconds=self._get_artist_ttl(in_library=False),
                ),
            )
            if not published:
                return
            return

        self._spawn_release_group_warm(
            artist_id,
            seed_items,
            rg_count,
            raw_offset=raw_offset,
            source_context=source_context,
        )
    async def _target_release_group_flags(
        self,
        release_groups: list[dict[str, Any]],
        *,
        artist_name: str,
    ) -> tuple[set[str], set[str]]:
        if self._ownership is None or not release_groups:
            return set(), set()
        from services.native.library_ownership_service import AlbumOwnershipCandidate

        candidates = [
            AlbumOwnershipCandidate(
                release_group_mbid=group.get("id"),
                title=group.get("title") or "",
                album_artist=artist_name or extract_artist_name(group) or "",
                year=(
                    int(str(group["first-release-date"]).split("-")[0])
                    if str(group.get("first-release-date") or "")[:4].isdigit()
                    else None
                ),
            )
            for group in release_groups
        ]
        projections = await self._ownership.project_albums(candidates)
        owned = {
            str(group["id"]).casefold()
            for group, projection in zip(release_groups, projections)
            if group.get("id") and projection.owned
        }
        ids = [str(group["id"]) for group in release_groups if group.get("id")]
        requested = await self._library_repo.get_requested_mbids(ids)
        return owned, requested

    def _build_external_links(self, mb_artist: dict[str, Any]) -> list[ExternalLink]:
        external_links_data = extract_external_links(mb_artist)
        return [
            ExternalLink(type=link["type"], url=link["url"], label=link["label"])
            for link in external_links_data
        ]

    async def _get_categorized_releases(
        self,
        mb_artist: dict[str, Any],
        album_mbids: set[str],
        requested_mbids: set[str] = None,
    ) -> tuple[list[ReleaseItem], list[ReleaseItem], list[ReleaseItem]]:
        prefs = self._preferences_service.get_preferences()
        included_primary_types = set(t.lower() for t in prefs.primary_types)
        included_secondary_types = set(t.lower() for t in prefs.secondary_types)
        return categorize_release_groups(
            mb_artist,
            album_mbids,
            included_primary_types,
            included_secondary_types,
            requested_mbids or set(),
        )

    async def _fetch_wikidata_info(
        self, mb_artist: dict[str, Any]
    ) -> tuple[Optional[str], Optional[str]]:
        wikidata_id, wiki_urls = self._extract_wiki_info(mb_artist)

        tasks = []
        if wiki_urls:
            tasks.append(self._wikidata_repo.get_wikipedia_extract(wiki_urls[0]))
        else:
            tasks.append(asyncio.create_task(asyncio.sleep(0)))

        if wikidata_id:
            tasks.append(
                self._wikidata_repo.get_artist_image_from_wikidata(wikidata_id)
            )
        else:
            tasks.append(asyncio.create_task(asyncio.sleep(0)))

        results = await asyncio.gather(*tasks, return_exceptions=True)

        description = (
            results[0]
            if len(results) > 0 and not isinstance(results[0], Exception) and results[0]
            else None
        )
        image = (
            results[1]
            if len(results) > 1 and not isinstance(results[1], Exception) and results[1]
            else None
        )

        return description, image

    def _extract_wiki_info(
        self, mb_artist: dict[str, Any]
    ) -> tuple[Optional[str], list[str]]:
        return extract_wiki_info(
            mb_artist, self._wikidata_repo.get_wikidata_id_from_url
        )
