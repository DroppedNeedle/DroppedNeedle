import logging
import asyncio
from typing import Optional, TYPE_CHECKING
import msgspec
from api.v1.schemas.album import AlbumInfo, AlbumBasicInfo, AlbumTracksInfo, Track
from repositories.protocols import LibraryRepositoryProtocol, MusicBrainzRepositoryProtocol
from services.preferences_service import PreferencesService
from services.album_utils import find_primary_release, get_ranked_releases, extract_artist_info, extract_tracks, extract_label, build_album_basic_info, mb_to_basic_info
from infrastructure.persistence import LibraryDB
from infrastructure.cache.cache_keys import ALBUM_INFO_PREFIX, LIBRARY_ALBUM_DETAILS_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.disk_cache import DiskMetadataCache
from infrastructure.validators import validate_mbid
from infrastructure.queue.priority_queue import RequestPriority
from core.exceptions import ResourceNotFoundError
from services.audiodb_image_service import AudioDBImageService
from repositories.audiodb_models import AudioDBAlbumImages

if TYPE_CHECKING:
    from infrastructure.persistence.album_release_pin_store import AlbumReleasePinStore
    from services.audiodb_browse_queue import AudioDBBrowseQueue

logger = logging.getLogger(__name__)


class AlbumService:
    def __init__(
        self, 
        library_repo: LibraryRepositoryProtocol, 
        mb_repo: MusicBrainzRepositoryProtocol,
        library_db: LibraryDB,
        memory_cache: CacheInterface,
        disk_cache: DiskMetadataCache,
        preferences_service: PreferencesService,
        audiodb_image_service: AudioDBImageService | None = None,
        audiodb_browse_queue: 'AudioDBBrowseQueue | None' = None,
        release_pin_store: 'AlbumReleasePinStore | None' = None,
    ):
        self._library_repo = library_repo
        self._mb_repo = mb_repo
        self._library_db = library_db
        self._cache = memory_cache
        self._disk_cache = disk_cache
        self._preferences_service = preferences_service
        self._audiodb_image_service = audiodb_image_service
        self._audiodb_browse_queue = audiodb_browse_queue
        # Per-album edition pins (CollectionManagement Feature E, D16); None in
        # constructions that predate the feature (tests) -> pure auto behaviour.
        self._release_pins = release_pin_store
        self._album_in_flight: dict[str, asyncio.Future[AlbumInfo]] = {}

    async def _get_audiodb_album_thumb(self, release_group_id: str, artist_name: str | None = None, album_name: str | None = None, *, allow_fetch: bool = False) -> str | None:
        if self._audiodb_image_service is None:
            return None
        try:
            if allow_fetch:
                images = await self._audiodb_image_service.fetch_and_cache_album_images(
                    release_group_id, artist_name, album_name, is_monitored=False,
                )
            else:
                images = await self._audiodb_image_service.get_cached_album_images(release_group_id)
            if images and not images.is_negative:
                return images.album_thumb_url
            if not allow_fetch and images is None and self._audiodb_browse_queue:
                settings = self._preferences_service.get_advanced_settings()
                if settings.audiodb_enabled:
                    await self._audiodb_browse_queue.enqueue(
                        "album", release_group_id,
                        name=album_name, artist_name=artist_name,
                    )
        except Exception as e:  # noqa: BLE001
            logger.warning("Failed to get AudioDB album thumb for %s: %s", release_group_id[:8], e)
        return None

    async def _apply_audiodb_album_images(
        self,
        album_info: AlbumInfo,
        release_group_mbid: str,
        artist_name: str | None,
        album_name: str | None,
        *,
        allow_fetch: bool = False,
        is_monitored: bool = False,
    ) -> AlbumInfo:
        if self._audiodb_image_service is None:
            return album_info
        try:
            images: AudioDBAlbumImages | None
            if allow_fetch:
                images = await self._audiodb_image_service.fetch_and_cache_album_images(
                    release_group_mbid, artist_name, album_name, is_monitored=is_monitored,
                )
            else:
                images = await self._audiodb_image_service.get_cached_album_images(release_group_mbid)
            if images is None or images.is_negative:
                if not allow_fetch and images is None and self._audiodb_browse_queue:
                    settings = self._preferences_service.get_advanced_settings()
                    if settings.audiodb_enabled:
                        await self._audiodb_browse_queue.enqueue(
                            "album", release_group_mbid,
                            name=album_name, artist_name=artist_name,
                        )
                return album_info
            album_info.album_thumb_url = images.album_thumb_url
            album_info.album_back_url = images.album_back_url
            album_info.album_cdart_url = images.album_cdart_url
            album_info.album_spine_url = images.album_spine_url
            album_info.album_3d_case_url = images.album_3d_case_url
            album_info.album_3d_flat_url = images.album_3d_flat_url
            album_info.album_3d_face_url = images.album_3d_face_url
            album_info.album_3d_thumb_url = images.album_3d_thumb_url
        except Exception as e:  # noqa: BLE001
            logger.warning("Failed to apply AudioDB images for album %s: %s", release_group_mbid[:8], e)
        return album_info

    async def is_album_cached(self, release_group_id: str) -> bool:
        cache_key = f"{ALBUM_INFO_PREFIX}{release_group_id}"
        return await self._cache.get(cache_key) is not None

    async def _get_cached_album_info(self, release_group_id: str, cache_key: str) -> Optional[AlbumInfo]:
        cached_info = await self._cache.get(cache_key)
        if cached_info:
            return cached_info
        
        disk_data = await self._disk_cache.get_album(release_group_id)
        if disk_data:
            album_info = msgspec.convert(disk_data, AlbumInfo, strict=False)
            advanced_settings = self._preferences_service.get_advanced_settings()
            ttl = advanced_settings.cache_ttl_album_library if album_info.in_library else advanced_settings.cache_ttl_album_non_library
            await self._cache.set(cache_key, album_info, ttl_seconds=ttl)
            return album_info
        
        return None

    async def _save_album_to_cache(self, release_group_id: str, album_info: AlbumInfo) -> None:
        cache_key = f"{ALBUM_INFO_PREFIX}{release_group_id}"
        advanced_settings = self._preferences_service.get_advanced_settings()
        ttl = advanced_settings.cache_ttl_album_library if album_info.in_library else advanced_settings.cache_ttl_album_non_library
        await self._cache.set(cache_key, album_info, ttl_seconds=ttl)
        await self._disk_cache.set_album(release_group_id, album_info, is_monitored=album_info.in_library, ttl_seconds=ttl if not album_info.in_library else None)

    async def warm_full_album_cache(self, release_group_id: str) -> None:
        try:
            cache_key = f"{ALBUM_INFO_PREFIX}{release_group_id}"
            if await self._get_cached_album_info(release_group_id, cache_key):
                return
            await self.get_album_info(release_group_id)
        except Exception:  # noqa: BLE001
            pass

    async def refresh_album(self, release_group_id: str) -> AlbumInfo:
        release_group_id = validate_mbid(release_group_id, "album")

        await self._cache.delete(f"{ALBUM_INFO_PREFIX}{release_group_id}")
        await self._cache.delete(f"{LIBRARY_ALBUM_DETAILS_PREFIX}{release_group_id}")
        await self._disk_cache.delete_album(release_group_id)
        self._album_in_flight.pop(release_group_id, None)

        return await self.get_album_info(release_group_id)

    async def get_album_info(self, release_group_id: str, library_mbids: set[str] = None) -> AlbumInfo:
        try:
            release_group_id = validate_mbid(release_group_id, "album")
        except ValueError as e:
            logger.error(f"Invalid album MBID: {e}")
            raise
        try:
            cache_key = f"{ALBUM_INFO_PREFIX}{release_group_id}"
            cached = await self._get_cached_album_info(release_group_id, cache_key)
            if cached:
                cached = await self._apply_audiodb_album_images(
                    cached, release_group_id, cached.artist_name, cached.title,
                    allow_fetch=True, is_monitored=cached.in_library,
                )
                return cached

            if release_group_id in self._album_in_flight:
                return await asyncio.shield(self._album_in_flight[release_group_id])

            loop = asyncio.get_running_loop()
            future: asyncio.Future[AlbumInfo] = loop.create_future()
            self._album_in_flight[release_group_id] = future
            try:
                album_info = await self._do_get_album_info(release_group_id, cache_key, library_mbids)
                if not future.done():
                    future.set_result(album_info)
                return album_info
            except BaseException as exc:
                if not future.done():
                    future.set_exception(exc)
                raise
            finally:
                self._album_in_flight.pop(release_group_id, None)
        except ValueError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"API call failed for album {release_group_id}: {e}")
            raise ResourceNotFoundError(f"Failed to get album info: {e}")

    async def _do_get_album_info(
        self, release_group_id: str, cache_key: str, library_mbids: set[str] | None
    ) -> AlbumInfo:
        album_info = await self._build_album_from_musicbrainz(release_group_id, library_mbids)
        album_info = await self._apply_audiodb_album_images(
            album_info, release_group_id, album_info.artist_name, album_info.title,
            allow_fetch=True, is_monitored=album_info.in_library,
        )
        await self._save_album_to_cache(release_group_id, album_info)
        return album_info
    
    async def get_album_basic_info(self, release_group_id: str) -> AlbumBasicInfo:
        try:
            release_group_id = validate_mbid(release_group_id, "album")
        except ValueError as e:
            logger.error(f"Invalid album MBID: {e}")
            raise

        try:
            cache_key = f"{ALBUM_INFO_PREFIX}{release_group_id}"

            try:
                if self._library_repo.is_configured():
                    requested_mbids = await self._library_repo.get_requested_mbids()
                else:
                    requested_mbids = set()
            except Exception:  # noqa: BLE001
                logger.warning("Lidarr unavailable, proceeding without requested data")
                requested_mbids = set()
            is_requested = release_group_id.lower() in requested_mbids

            cached_album_info = await self._get_cached_album_info(release_group_id, cache_key)
            if cached_album_info:
                album_thumb = cached_album_info.album_thumb_url
                if not album_thumb:
                    album_thumb = await self._get_audiodb_album_thumb(
                        release_group_id, cached_album_info.artist_name, cached_album_info.title,
                        allow_fetch=False,
                    )
                return AlbumBasicInfo(
                    title=cached_album_info.title,
                    musicbrainz_id=cached_album_info.musicbrainz_id,
                    artist_name=cached_album_info.artist_name,
                    artist_id=cached_album_info.artist_id,
                    release_date=cached_album_info.release_date,
                    year=cached_album_info.year,
                    type=cached_album_info.type,
                    disambiguation=cached_album_info.disambiguation,
                    in_library=cached_album_info.in_library,
                    requested=is_requested and not cached_album_info.in_library,
                    cover_url=cached_album_info.cover_url,
                    album_thumb_url=album_thumb,
                )

            release_group = await self._fetch_release_group(release_group_id)
            # in_library means non-deleted local files exist; the materialised
            # library_albums row lags removals and misses manually-added files.
            in_library = await self._library_db.has_album_files(release_group_id)
            basic = AlbumBasicInfo(**mb_to_basic_info(release_group, release_group_id, in_library, is_requested))
            basic.album_thumb_url = await self._get_audiodb_album_thumb(
                release_group_id, basic.artist_name, basic.title,
                allow_fetch=False,
            )
            return basic
        
        except ValueError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get basic album info for {release_group_id}: {e}")
            raise ResourceNotFoundError(f"Failed to get album info: {e}")
    
    async def get_album_tracks_info(
        self,
        release_group_id: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> AlbumTracksInfo:
        """``priority`` (P4): background callers - the orchestrator's completion-time
        coverage check - pass BACKGROUND_SYNC so a cold-cache finalize never jumps the
        MusicBrainz queue ahead of a user's page load (honest-priority house rule).
        Normally warm: the request flow fetched this at task creation."""
        try:
            release_group_id = validate_mbid(release_group_id, "album")
        except ValueError as e:
            logger.error(f"Invalid album MBID: {e}")
            raise

        try:
            cache_key = f"{ALBUM_INFO_PREFIX}{release_group_id}"
            cached_album_info = await self._get_cached_album_info(release_group_id, cache_key)
            if cached_album_info:
                return AlbumTracksInfo(
                    tracks=cached_album_info.tracks,
                    total_tracks=cached_album_info.total_tracks,
                    total_length=cached_album_info.total_length,
                    label=cached_album_info.label,
                    barcode=cached_album_info.barcode,
                    country=cached_album_info.country,
                )

            release_group = await self._fetch_release_group(release_group_id, priority=priority)
            ranked_releases = get_ranked_releases(release_group)
            
            if not ranked_releases:
                return AlbumTracksInfo(tracks=[], total_tracks=0)
            
            tracks: list[Track] = []
            total_length = 0
            release_data = None

            candidate_ids = [r.get("id") for r in ranked_releases[:3] if r.get("id")]
            # Prefer the edition the library's files belong to, so an owned album shows
            # what's on disc rather than a larger ranked release.
            owned_release_id = await self._owned_release_id(release_group_id)
            if owned_release_id:
                candidate_ids = [owned_release_id, *(cid for cid in candidate_ids if cid != owned_release_id)]
            if candidate_ids:
                release_results = await asyncio.gather(
                    *(
                        self._mb_repo.get_release_by_id(
                            rid, includes=["recordings", "labels"], priority=priority
                        )
                        for rid in candidate_ids
                    ),
                    return_exceptions=True,
                )
                failures = [r for r in release_results if isinstance(r, Exception)]
                if failures:
                    logger.warning(f"Album {release_group_id[:8]}: {len(failures)}/{len(candidate_ids)} release fetches failed")
                for result in release_results:
                    if isinstance(result, Exception) or not result:
                        continue
                    found_tracks, found_length = extract_tracks(result)
                    if found_tracks:
                        tracks = found_tracks
                        total_length = found_length
                        release_data = result
                        break
            
            if not release_data:
                return AlbumTracksInfo(tracks=[], total_tracks=0)
            
            label = extract_label(release_data)
            
            return AlbumTracksInfo(
                tracks=tracks,
                total_tracks=len(tracks),
                total_length=total_length if total_length > 0 else None,
                label=label,
                barcode=release_data.get("barcode"),
                country=release_data.get("country"),
            )
        
        except ValueError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get album tracks for {release_group_id}: {e}")
            raise ResourceNotFoundError(f"Failed to get album tracks: {e}")

    async def annotate_album_coverage(self, release_group_id: str, status):  # noqa: ANN001
        """Fill a ``LibraryAlbumStatus``'s coverage fields (P5, 2026-07-05 incident):
        which held files actually COVER the release's expected tracks, and which are
        orphans ("doesn't match this album"). Uses the same shared matcher as the
        download completeness gate, so the album page and the orchestrator can never
        disagree about what "In Library" means. Fail-open: any tracklist failure
        leaves the coverage fields zeroed and the page falls back to presence-only."""
        if not status.tracks:
            return status
        try:
            info = await self.get_album_tracks_info(release_group_id)
        except Exception:  # noqa: BLE001 - coverage is an annotation, never a page-breaker
            logger.warning(f"Album coverage annotation failed for {release_group_id[:8]}")
            return status
        tracks = list(info.tracks or [])
        if not tracks:
            return status
        from services.native.coverage import match_rows_to_tracks

        rows = [
            {
                "id": t.id,
                "recording_mbid": t.recording_mbid,
                "disc_number": t.disc_number,
                "track_number": t.track_number,
                "track_title": t.track_title,
                "duration_seconds": t.duration_seconds,
            }
            for t in status.tracks
        ]
        covered, orphan_rows, matched_ids = match_rows_to_tracks(rows, tracks)
        orphan_ids = {r["id"] for r in orphan_rows if r.get("id")}
        status.expected_tracks = len(tracks)
        status.covered_tracks = covered
        status.matched_file_ids = matched_ids
        status.orphans = [t for t in status.tracks if t.id in orphan_ids]
        return status

    async def _fetch_release_group(
        self,
        release_group_id: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> dict:
        rg_result = await self._mb_repo.get_release_group_by_id(
            release_group_id,
            includes=["artists", "releases", "tags"],
            priority=priority,
        )
        
        if not rg_result:
            raise ResourceNotFoundError(f"Release group {release_group_id} not found")
        
        return rg_result

    async def _check_in_library(self, release_group_id: str, library_mbids: set[str] = None) -> bool:
        if library_mbids is not None:
            return release_group_id.lower() in library_mbids
        
        library_mbids = await self._library_repo.get_library_mbids(include_release_ids=True)
        return release_group_id.lower() in library_mbids
    
    def _build_basic_info(
        self,
        release_group: dict,
        release_group_id: str,
        artist_name: str,
        artist_id: str,
        in_library: bool
    ) -> AlbumInfo:
        return AlbumInfo(**build_album_basic_info(release_group, release_group_id, artist_name, artist_id, in_library))
    
    async def _owned_release_id(self, release_group_id: str) -> str | None:
        """The release edition the album page follows: the admin/trusted PIN when one
        is set (Feature E, D16), else the edition the library's files belong to (an
        owned album shows what's on disc rather than the top-ranked release).
        ``None`` when neither exists: fall back to ranking. A pinned release that has
        vanished from MusicBrainz fails open downstream - the pinned id simply fails
        its fetch and the ranked candidates take over."""
        pinned = await self._pinned_release_id(release_group_id)
        if pinned:
            return pinned
        try:
            return await self._library_db.get_album_release_mbid(release_group_id)
        except Exception as e:  # noqa: BLE001
            logger.warning("Owned-release lookup failed for %s: %s", release_group_id[:8], e)
            return None

    async def _pinned_release_id(self, release_group_id: str) -> str | None:
        if self._release_pins is None:
            return None
        try:
            return await self._release_pins.get(release_group_id)
        except Exception as e:  # noqa: BLE001 - a pin lookup failure must never break the page
            logger.warning("Edition-pin lookup failed for %s: %s", release_group_id[:8], e)
            return None

    async def resolve_edition(self, release_group_id: str) -> str | None:
        """The edition acquisition should target: pin first, else owned (public
        wrapper for the download layer's 'acquire this edition')."""
        return await self._owned_release_id(release_group_id)

    async def list_editions(self, release_group_id: str) -> dict:
        """All of a release group's editions for the picker (Feature E §1), flagged
        with which one is owned (mode-over-files) and which is pinned."""
        release_group_id = validate_mbid(release_group_id, "album")
        # a dedicated fetch: the shared _fetch_release_group omits the "media"
        # include, and the picker needs per-release track counts
        release_group = await self._mb_repo.get_release_group_by_id(
            release_group_id, includes=["releases", "media"]
        )
        if not release_group:
            raise ResourceNotFoundError(f"Release group {release_group_id} not found")
        releases = release_group.get("releases") or release_group.get("release-list", [])
        owned = await self._library_db.get_album_release_mbid(release_group_id)
        pinned = await self._pinned_release_id(release_group_id)
        items = []
        for rel in releases:
            rel_id = rel.get("id")
            if not rel_id:
                continue
            track_count = sum(int(m.get("track-count") or 0) for m in (rel.get("media") or []))
            items.append(
                {
                    "release_mbid": rel_id,
                    "title": rel.get("title"),
                    "disambiguation": rel.get("disambiguation") or None,
                    "date": rel.get("date") or None,
                    "country": rel.get("country") or None,
                    "packaging": rel.get("packaging") or None,
                    "status": rel.get("status") or None,
                    "track_count": track_count,
                    "is_owned": rel_id == owned,
                    "is_pinned": rel_id == pinned,
                }
            )
        return {"items": items, "pinned_release_mbid": pinned, "owned_release_mbid": owned}

    async def _bust_album_caches(self, release_group_id: str) -> None:
        """The pin changes the served tracklist/disambiguation, so the cached album
        payloads must go (mirrors refresh_album/_build_scan_invalidation)."""
        await self._cache.delete(f"{ALBUM_INFO_PREFIX}{release_group_id}")
        await self._cache.delete(f"{LIBRARY_ALBUM_DETAILS_PREFIX}{release_group_id}")
        await self._disk_cache.delete_album(release_group_id)
        self._album_in_flight.pop(release_group_id, None)

    async def set_edition_pin(
        self, release_group_id: str, release_mbid: str, user_id: str | None
    ) -> None:
        """Pin an edition (admin/trusted; the route guards the role). Validates the
        release belongs to this group before pinning, then busts the album caches."""
        release_group_id = validate_mbid(release_group_id, "album")
        if self._release_pins is None:
            raise ResourceNotFoundError("Edition pinning is unavailable")
        editions = await self.list_editions(release_group_id)
        if not any(item["release_mbid"] == release_mbid for item in editions["items"]):
            raise ResourceNotFoundError("That edition does not belong to this album")
        await self._release_pins.set(release_group_id, release_mbid, user_id)
        await self._bust_album_caches(release_group_id)

    async def clear_edition_pin(self, release_group_id: str) -> bool:
        release_group_id = validate_mbid(release_group_id, "album")
        if self._release_pins is None:
            return False
        cleared = await self._release_pins.clear(release_group_id)
        if cleared:
            await self._bust_album_caches(release_group_id)
        return cleared

    async def _enrich_with_release_details(
        self,
        album_info: AlbumInfo,
        release_id: str
    ) -> None:
        try:
            release_data = await self._mb_repo.get_release_by_id(
                release_id,
                includes=["recordings", "labels"]
            )
            
            if not release_data:
                logger.warning(f"Release {release_id} not found")
                return
            
            tracks, total_length = extract_tracks(release_data)
            album_info.tracks = tracks
            album_info.total_tracks = len(tracks)
            album_info.total_length = total_length if total_length > 0 else None
            
            album_info.label = extract_label(release_data)
            
            album_info.barcode = release_data.get("barcode")
            album_info.country = release_data.get("country")
        
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to enrich with release details: {e}")

    async def _build_album_from_musicbrainz(
        self,
        release_group_id: str,
        library_mbids: set[str] = None
    ) -> AlbumInfo:
        # in_library reflects non-deleted local files, not the library_albums row
        # (see get_album_basic_info).
        has_files = await self._library_db.has_album_files(release_group_id)
        in_library = has_files

        release_group = await self._fetch_release_group(release_group_id)
        primary_release = find_primary_release(release_group)
        artist_name, artist_id = extract_artist_info(release_group)

        if not in_library:
            in_library = await self._check_in_library(release_group_id, library_mbids)

        basic_info = self._build_basic_info(
            release_group, release_group_id, artist_name, artist_id, in_library
        )

        # An owned album shows the edition its files belong to (what's on disc); only fall
        # back to the top-ranked release, which is often a larger deluxe/multi-disc variant.
        owned_release_id = await self._owned_release_id(release_group_id) if has_files else None
        primary_id = primary_release.get("id") if primary_release else None
        for release_id in dict.fromkeys(rid for rid in (owned_release_id, primary_id) if rid):
            await self._enrich_with_release_details(basic_info, release_id)
            if basic_info.tracks:
                break

        return basic_info
