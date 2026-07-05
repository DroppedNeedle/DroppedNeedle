import logging
import asyncio
import os
from pathlib import Path
import time
from typing import Any, TYPE_CHECKING
from repositories.protocols import LibraryRepositoryProtocol, CoverArtRepositoryProtocol
from api.v1.schemas.library import (
    LibraryAlbum,
    LibraryArtist,
    LibraryGroupedArtist,
    LibraryStatsResponse,
    SyncLibraryResponse,
    AlbumRemovePreviewResponse,
    AlbumRemoveResponse,
    ResolvedTrack,
    TrackResolveResponse,
)
from infrastructure.persistence import LibraryDB, SyncStateStore, GenreIndex
from infrastructure.cache.cache_keys import (
    library_requested_mbids_key,
    SOURCE_RESOLUTION_PREFIX,
    ALBUM_INFO_PREFIX, ARTIST_INFO_PREFIX, LIBRARY_PREFIX,
    LIBRARY_ALBUM_DETAILS_PREFIX, LIBRARY_ALBUM_TRACKS_PREFIX,
    LIBRARY_ALBUM_TRACKFILES_PREFIX, LIBRARY_ARTIST_ALBUMS_PREFIX,
    LIBRARY_ARTIST_DETAILS_PREFIX, LIBRARY_ARTIST_IMAGE_PREFIX,
    LIBRARY_ALBUM_IMAGE_PREFIX, LIBRARY_REQUESTED_PREFIX,
)
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.disk_cache import DiskMetadataCache
from infrastructure.cover_urls import prefer_release_group_cover_url
from infrastructure.serialization import clone_with_updates
from core.exceptions import ExternalServiceError, ResourceNotFoundError
from infrastructure.resilience.retry import CircuitOpenError
from services.cache_status_service import CacheStatusService
from services.library_precache_service import LibraryPrecacheService

if TYPE_CHECKING:
    from services.preferences_service import PreferencesService
    from services.local_files_service import LocalFilesService
    from services.jellyfin_library_service import JellyfinLibraryService
    from services.navidrome_library_service import NavidromeLibraryService

logger = logging.getLogger(__name__)

MAX_RESOLVE_ITEMS = 50


class LibraryService:
    def __init__(
        self,
        library_repo: LibraryRepositoryProtocol,
        library_db: LibraryDB,
        cover_repo: CoverArtRepositoryProtocol,
        preferences_service: 'PreferencesService',
        memory_cache: CacheInterface | None = None,
        disk_cache: DiskMetadataCache | None = None,
        artist_discovery_service: Any = None,
        audiodb_image_service: Any = None,
        local_files_service: 'LocalFilesService | None' = None,
        jellyfin_library_service: 'JellyfinLibraryService | None' = None,
        navidrome_library_service: 'NavidromeLibraryService | None' = None,
        sync_state_store: SyncStateStore | None = None,
        genre_index: GenreIndex | None = None,
    ):
        self._library_repo = library_repo
        self._library_db = library_db
        self._cover_repo = cover_repo
        self._preferences_service = preferences_service
        self._memory_cache = memory_cache
        self._disk_cache = disk_cache
        self._local_files_service = local_files_service
        self._jellyfin_library_service = jellyfin_library_service
        self._navidrome_library_service = navidrome_library_service
        self._sync_state_store = sync_state_store
        self._can_precache = sync_state_store is not None and genre_index is not None
        self._precache_service: LibraryPrecacheService | None = None
        if self._can_precache:
            self._precache_service = LibraryPrecacheService(
                library_repo, cover_repo, preferences_service,
                sync_state_store, genre_index, library_db,
                artist_discovery_service=artist_discovery_service,
                audiodb_image_service=audiodb_image_service,
            )
        self._last_sync_time: float = 0.0
        self._last_manual_sync: float = 0.0
        self._manual_sync_cooldown: float = 60.0
        self._global_sync_cooldown: float = 30.0
        self._sync_lock = asyncio.Lock()
        self._sync_future: asyncio.Future | None = None

    def _update_last_sync_timestamp(self) -> None:
        try:
            library_settings = self._preferences_service.get_library_sync_settings()
            updated_settings = clone_with_updates(library_settings, {'last_sync': int(time.time())})
            self._preferences_service.save_library_sync_settings(updated_settings)
        except Exception as e:  # noqa: BLE001
            logger.warning(f"Failed to update last_sync timestamp: {e}")

    @staticmethod
    def _normalized_album_cover_url(album_mbid: str | None, cover_url: str | None) -> str | None:
        return prefer_release_group_cover_url(album_mbid, cover_url, size=500)

    async def get_library(self) -> list[LibraryAlbum]:
        try:
            albums_data = await self._library_db.get_albums()
            
            if not albums_data:
                await self.sync_library()
                albums_data = await self._library_db.get_albums()
            
            albums = [
                LibraryAlbum(
                    artist=album['artist_name'],
                    album=album['title'],
                    year=album.get('year'),
                    quality=None,
                    cover_url=self._normalized_album_cover_url(
                        album.get('mbid'),
                        album.get('cover_url'),
                    ),
                    musicbrainz_id=album.get('mbid'),
                    artist_mbid=album.get('artist_mbid'),
                    date_added=album.get('date_added')
                )
                for album in albums_data
            ]
            
            return albums
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch library: {e}")
            raise ExternalServiceError(f"Failed to fetch library: {e}")
    
    async def get_library_mbids(self) -> list[str]:
        if not self._library_repo.is_configured():
            return []
        try:
            library_mbids_coro = self._library_repo.get_library_mbids(include_release_ids=False)
            local_mbids_coro = self._library_db.get_all_album_mbids()
            results = await asyncio.gather(
                library_mbids_coro, local_mbids_coro, return_exceptions=True,
            )
            library_mbids = results[0] if not isinstance(results[0], BaseException) else set()
            local_mbids = results[1] if not isinstance(results[1], BaseException) else []
            if isinstance(results[0], BaseException):
                logger.warning("Lidarr library mbids fetch failed, degrading: %s", results[0])
            if isinstance(results[1], BaseException):
                logger.warning("Local library_db mbids fetch failed, degrading: %s", results[1])
            if isinstance(library_mbids, BaseException) and isinstance(local_mbids, BaseException):
                raise ExternalServiceError("Both library mbid sources failed")
            # union catches recently-imported albums the cached response may not yet reflect
            merged = (library_mbids if isinstance(library_mbids, set) else set()) | {m.lower() for m in local_mbids}
            return list(merged)
        except ExternalServiceError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch library mbids: {e}")
            raise ExternalServiceError(f"Failed to fetch library mbids: {e}")

    async def get_requested_mbids(self) -> list[str]:
        if not self._library_repo.is_configured():
            return []
        try:
            requested_set = await self._library_repo.get_requested_mbids()
            return list(requested_set)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch requested mbids: {e}")
            raise ExternalServiceError(f"Failed to fetch requested mbids: {e}")

    async def get_artists(self, limit: int | None = None) -> list[LibraryArtist]:
        try:
            artists_data = await self._library_db.get_artists(limit=limit)
            
            if not artists_data:
                await self.sync_library()
                artists_data = await self._library_db.get_artists(limit=limit)
            
            artists = [
                LibraryArtist(
                    mbid=artist['mbid'],
                    name=artist['name'],
                    album_count=artist.get('album_count', 0),
                    date_added=artist.get('date_added')
                )
                for artist in artists_data
            ]
            
            return artists
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch artists: {e}")
            raise ExternalServiceError(f"Failed to fetch artists: {e}")

    async def get_albums_paginated(
        self,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "date_added",
        sort_order: str = "desc",
        search: str | None = None,
    ) -> tuple[list[LibraryAlbum], int]:
        try:
            albums_data, total = await self._library_db.get_albums_paginated(
                limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order, search=search,
            )

            if not albums_data and offset == 0 and not search:
                await self.sync_library()
                albums_data, total = await self._library_db.get_albums_paginated(
                    limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order, search=search,
                )

            albums = [
                LibraryAlbum(
                    artist=album['artist_name'],
                    album=album['title'],
                    year=album.get('year'),
                    quality=None,
                    cover_url=self._normalized_album_cover_url(album.get('mbid'), album.get('cover_url')),
                    musicbrainz_id=album.get('mbid'),
                    artist_mbid=album.get('artist_mbid'),
                    date_added=album.get('date_added'),
                )
                for album in albums_data
            ]
            return albums, total
        except (ExternalServiceError, CircuitOpenError):
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch paginated albums: {e}")
            raise ExternalServiceError(f"Failed to fetch paginated albums: {e}")

    async def get_artists_paginated(
        self,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "name",
        sort_order: str = "asc",
        search: str | None = None,
    ) -> tuple[list[LibraryArtist], int]:
        try:
            artists_data, total = await self._library_db.get_artists_paginated(
                limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order, search=search,
            )

            if not artists_data and offset == 0 and not search:
                await self.sync_library()
                artists_data, total = await self._library_db.get_artists_paginated(
                    limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order, search=search,
                )

            artists = [
                LibraryArtist(
                    mbid=artist['mbid'],
                    name=artist['name'],
                    album_count=artist.get('album_count', 0),
                    date_added=artist.get('date_added'),
                )
                for artist in artists_data
            ]
            return artists, total
        except (ExternalServiceError, CircuitOpenError):
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch paginated artists: {e}")
            raise ExternalServiceError(f"Failed to fetch paginated artists: {e}")
    
    async def get_recently_added(self, limit: int = 20) -> list[LibraryAlbum]:

        try:
            if self._library_repo.is_configured():
                albums = await self._library_repo.get_recently_imported(limit=limit)
            else:
                albums = []
            
            if not albums:
                albums_data = await self._library_db.get_recently_added(limit=limit)
                
                albums = [
                    LibraryAlbum(
                        artist=album['artist_name'],
                        album=album['title'],
                        year=album.get('year'),
                        quality=None,
                        cover_url=self._normalized_album_cover_url(
                            album.get('mbid'),
                            album.get('cover_url'),
                        ),
                        musicbrainz_id=album.get('mbid'),
                        artist_mbid=album.get('artist_mbid'),
                        date_added=album.get('date_added')
                    )
                    for album in albums_data
                ]
            
            return albums
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch recently added: {e}")
            raise ExternalServiceError(f"Failed to fetch recently added: {e}")
    
    async def sync_library(self, is_manual: bool = False, force_full: bool = False) -> SyncLibraryResponse:
        if not self._library_repo.is_configured():
            raise ExternalServiceError("Lidarr is not configured. Set a Lidarr API key in Settings to sync your library.")

        try:
            status_service = CacheStatusService()
            
            async with self._sync_lock:
                current_time = time.time()
                
                time_since_last_sync = current_time - self._last_sync_time
                if time_since_last_sync < self._global_sync_cooldown:
                    remaining = int(self._global_sync_cooldown - time_since_last_sync)
                    raise ExternalServiceError(
                        f"Sync cooldown active. Please wait {remaining} seconds before syncing again."
                    )
                
                if is_manual:
                    time_since_last_manual = current_time - self._last_manual_sync
                    if time_since_last_manual < self._manual_sync_cooldown:
                        remaining = int(self._manual_sync_cooldown - time_since_last_manual)
                        raise ExternalServiceError(
                            f"Manual sync cooldown active. Please wait {remaining} seconds before syncing again."
                        )
                
                if status_service.is_syncing():
                    if is_manual:
                        logger.warning("Library sync already in progress - cancelling previous sync to start fresh")
                        await status_service.cancel_current_sync()
                        await status_service.wait_for_completion()
                    else:
                        return SyncLibraryResponse(status="skipped", artists=0, albums=0)

                if self._sync_future is not None and not self._sync_future.done():
                    existing_future = self._sync_future
                else:
                    existing_future = None
                    loop = asyncio.get_running_loop()
                    self._sync_future = loop.create_future()

            # shield so waiter cancellation doesn't poison the shared future
            if existing_future is not None:
                return await asyncio.shield(existing_future)
            
            sync_succeeded = False
            try:
                albums = await self._library_repo.get_library(include_unmonitored=True)
                artists = await self._library_repo.get_artists_from_library(include_unmonitored=True)
                
                albums_data = [
                    {
                        'mbid': album.musicbrainz_id or f"unknown_{album.album}",
                        'artist_mbid': album.artist_mbid,
                        'artist_name': album.artist,
                        'title': album.album,
                        'year': album.year,
                        'cover_url': self._normalized_album_cover_url(
                            album.musicbrainz_id,
                            album.cover_url,
                        ),
                        'date_added': album.date_added
                    }
                    for album in albums
                ]
                
                await self._library_db.save_library(artists, albums_data)

                now = time.time()
                self._last_sync_time = now
                if is_manual:
                    self._last_manual_sync = now

                if self._precache_service is None:
                    logger.warning("Precache skipped: sync_state_store/genre_index not provided")
                    self._update_last_sync_timestamp()
                    result = SyncLibraryResponse(status='success', artists=len(artists), albums=len(albums))
                    self._sync_future.set_result(result)
                    return result

                resume = False
                if not force_full and self._sync_state_store:
                    try:
                        last_state = await self._sync_state_store.get_sync_state()
                        if last_state and last_state.get('status') == 'failed':
                            resume = True
                    except Exception as e:  # noqa: BLE001
                        logger.warning("Failed to check sync state for resume: %s", e)

                if force_full and self._sync_state_store:
                    try:
                        await self._sync_state_store.clear_processed_items()
                        await self._sync_state_store.clear_sync_state()
                    except Exception as e:  # noqa: BLE001
                        logger.warning("Failed to clear sync state for force_full: %s", e)

                task = asyncio.create_task(self._precache_service.precache_library_resources(artists, albums, resume=resume))

                def on_task_done(t: asyncio.Task):
                    try:
                        exc = t.exception()
                        if exc:
                            logger.error(f"Precache task failed: {exc}")
                            task_success = False
                        else:
                            task_success = not t.cancelled()
                    except asyncio.CancelledError:
                        task_success = False
                    finally:
                        status_service.set_current_task(None)
                        try:
                            library_settings = self._preferences_service.get_library_sync_settings()
                            if sync_started_at >= (library_settings.last_sync or 0):
                                updated = clone_with_updates(library_settings, {
                                    'last_sync_success': task_success,
                                })
                                self._preferences_service.save_library_sync_settings(updated)
                        except Exception as e:  # noqa: BLE001
                            logger.warning(f"Failed to update last_sync_success: {e}")

                task.add_done_callback(on_task_done)
                status_service.set_current_task(task)

                self._update_last_sync_timestamp()
                sync_started_at = self._preferences_service.get_library_sync_settings().last_sync or 0

                result = SyncLibraryResponse(
                    status='success',
                    artists=len(artists),
                    albums=len(albums),
                )
                sync_succeeded = True
                self._sync_future.set_result(result)
                return result
            except BaseException as exc:
                if self._sync_future is not None and not self._sync_future.done():
                    self._sync_future.set_exception(exc)
                raise
            finally:
                if not sync_succeeded:
                    future = self._sync_future
                    self._sync_future = None
                    # suppress "Future exception was never retrieved" if no waiter
                    if future is not None and future.done() and not future.cancelled():
                        try:
                            future.exception()
                        except BaseException:  # noqa: BLE001
                            pass
        except (ExternalServiceError, CircuitOpenError):
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Couldn't sync the library: {e}")
            raise ExternalServiceError(f"Couldn't sync the library: {e}")

    async def get_stats(self) -> LibraryStatsResponse:
        try:
            stats = await self._library_db.get_stats()
            
            return LibraryStatsResponse(
                artist_count=stats['artist_count'],
                album_count=stats['album_count'],
                last_sync=stats['last_sync'],
                db_size_bytes=stats['db_size_bytes'],
                db_size_mb=round(stats['db_size_bytes'] / (1024 * 1024), 2)
            )
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch library stats: {e}")
            raise ExternalServiceError(f"Failed to fetch library stats: {e}")
    
    async def clear_cache(self) -> None:
        try:
            await self._library_db.clear()
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to clear library cache: {e}")
            raise ExternalServiceError(f"Failed to clear library cache: {e}")
    
    async def get_library_grouped(self) -> list[LibraryGroupedArtist]:
        if not self._library_repo.is_configured():
            return []
        try:
            return await self._library_repo.get_library_grouped()
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch grouped library: {e}")
            raise ExternalServiceError(f"Failed to fetch grouped library: {e}")

    @staticmethod
    def _album_artist_identity(rows: list[dict]) -> tuple[str | None, str | None]:
        """Album-artist (mbid, name) from an album's ``library_files`` rows."""
        first = rows[0]
        artist_mbid = (first.get("album_artist_mbid") or "").strip() or None
        artist_name = first.get("album_artist_name") or first.get("artist_name") or "Unknown"
        return artist_mbid, artist_name

    def _recycle_paths(self, paths: list[str]) -> int:
        """Move files to the recycle bin (falls back to hard delete when no bin
        can be resolved - e.g. no library path configured)."""
        from services.native.recycle_bin import recycle, resolve_bin_path

        policy = self._preferences.get_download_policy()
        lib = self._preferences.get_library_settings()
        bin_path = resolve_bin_path(policy.recycle_bin_path, lib.library_paths)
        if bin_path is None:
            logger.warning("No recycle bin available; hard-deleting %d files", len(paths))
            return self._unlink_paths(paths)
        moved = 0
        parents: set[str] = set()
        for path in paths:
            parents.add(os.path.dirname(path))
            try:
                recycle(Path(path), bin_path)
                moved += 1
            except FileNotFoundError:
                moved += 1
            except OSError as e:
                logger.warning("Couldn't recycle library file: %s (%s)", path, e)
        for parent in sorted(parents, key=len, reverse=True):
            current = parent
            while current and current != os.path.dirname(current):
                try:
                    os.rmdir(current)
                except OSError:
                    break
                current = os.path.dirname(current)
        return moved

    @staticmethod
    def _unlink_paths(paths: list[str]) -> int:
        # a missing file counts as success (already gone); other OS errors are logged
        # and skipped so one bad path doesn't abort the rest
        removed = 0
        parents: set[str] = set()
        for path in paths:
            parents.add(os.path.dirname(path))
            try:
                os.remove(path)
                removed += 1
            except FileNotFoundError:
                removed += 1
            except OSError as e:
                logger.warning("Couldn't delete library file from disk: %s (%s)", path, e)
        # tidy up now-empty album folders, climbing to catch multi-disc roots
        # (Album/CD1 -> Album). rmdir only removes an EMPTY dir, so the climb self-limits
        # at the first non-empty ancestor (the artist/library root, or a folder still
        # holding a mis-attributed straggler), which is left untouched.
        for parent in sorted(parents, key=len, reverse=True):
            current = parent
            while current and current != os.path.dirname(current):
                try:
                    os.rmdir(current)
                except OSError:
                    break  # not empty, gone, or not ours - stop climbing
                current = os.path.dirname(current)
        return removed

    async def get_album_removal_preview(self, album_mbid: str) -> AlbumRemovePreviewResponse:
        try:
            rows = await self._library_db.get_library_files_for_album(album_mbid)
            if not rows:
                # not in the native library, nothing to remove
                return AlbumRemovePreviewResponse(success=True, artist_will_be_removed=False)

            artist_mbid, artist_name = self._album_artist_identity(rows)
            # exclude this album (still present at preview time): if the artist has no
            # other albums, removing this one empties the artist
            remaining = await self._library_db.count_artist_albums(
                artist_mbid=artist_mbid,
                artist_name=artist_name,
                exclude_release_group_mbid=album_mbid,
            )
            artist_will_be_removed = remaining == 0

            return AlbumRemovePreviewResponse(
                success=True,
                artist_will_be_removed=artist_will_be_removed,
                artist_name=artist_name if artist_will_be_removed else None,
            )
        except ExternalServiceError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to build removal preview for album {album_mbid}: {e}")
            raise ExternalServiceError(f"Failed to load removal preview: {e}")

    async def remove_album(
        self, album_mbid: str, delete_files: bool = False, *, to_recycle: bool = False
    ) -> AlbumRemoveResponse:
        try:
            rows = await self._library_db.get_library_files_for_album(album_mbid)
            artist_mbid: str | None = None
            artist_name: str | None = None
            paths: list[str] = []

            if rows:
                artist_mbid, artist_name = self._album_artist_identity(rows)
                # soft-delete the DB rows (recoverable via re-import); delete_files
                # (which the UI now always requests) also unlinks from disk.
                # to_recycle (discovery-batch removal) moves files to the recycle
                # bin instead, so an undone discovery spree is restorable.
                paths = await self._library_db.soft_delete_album_files(album_mbid)
                if to_recycle and paths:
                    await asyncio.to_thread(self._recycle_paths, paths)
                elif delete_files and paths:
                    await asyncio.to_thread(self._unlink_paths, paths)
            else:
                # ghost album: no active files, but a stale library_albums row may
                # persist (e.g. failed queued download, or files removed without a
                # reconcile). fall back to the materialised row for artist identity.
                cached = await self._library_db.get_album_by_mbid(album_mbid)
                if cached:
                    artist_mbid = cached.get("artist_mbid") or None
                    artist_name = cached.get("artist_name") or None

            # drop the materialised library_albums row too: /basic derives in_library
            # from this row and the table has no soft-delete, so otherwise the album
            # keeps reporting "In Library" (blocking re-download) until a full sync
            await self._library_db.delete_album_by_mbid(album_mbid)

            # artists are aggregated on read from library_files, so an artist with no
            # remaining (non-soft-deleted) albums is effectively removed too
            remaining = await self._library_db.count_artist_albums(
                artist_mbid=artist_mbid, artist_name=artist_name,
            )
            artist_removed = remaining == 0

            try:
                await self._invalidate_caches_after_removal(album_mbid, artist_mbid, artist_removed=artist_removed)
            except Exception as e:  # noqa: BLE001
                logger.warning(f"Album '{album_mbid}' removed but cache invalidation failed: {e}")

            return AlbumRemoveResponse(
                success=True,
                artist_removed=artist_removed,
                artist_name=artist_name if artist_removed else None,
            )
        except ExternalServiceError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Couldn't remove album {album_mbid}: {e}")
            raise ExternalServiceError(f"Couldn't remove this album: {e}")

    async def remove_file(self, file_id: str) -> "StatusMessageResponse":
        """Remove ONE library file - the album page's orphan-review action (P5,
        2026-07-05 incident): a held file that matches none of the album's expected
        tracks. Soft-deletes the row (recoverable via re-import) and unlinks the
        audio; the ghost ``library_albums`` row is dropped when this was the album's
        last active file, and album caches are invalidated so the page, badge, and
        Play All converge immediately."""
        from api.v1.schemas.common import StatusMessageResponse

        row = await self._library_db.get_library_file_by_id(file_id)
        if row is None or row.get("deleted_at"):
            raise ResourceNotFoundError("Library file not found")
        try:
            await self._library_db.soft_delete_library_file(row["file_path"])
            await asyncio.to_thread(self._unlink_paths, [row["file_path"]])

            album_mbid = row.get("release_group_mbid")
            if album_mbid:
                remaining = await self._library_db.get_library_files_for_album(album_mbid)
                if not remaining:
                    # mirror remove_album: /basic derives in_library from this row
                    await self._library_db.delete_album_by_mbid(album_mbid)
                artist_mbid = row.get("album_artist_mbid") or row.get("artist_mbid")
                try:
                    await self._invalidate_caches_after_removal(
                        album_mbid, artist_mbid, artist_removed=False
                    )
                except Exception as e:  # noqa: BLE001 - removal succeeded; stale cache only
                    logger.warning(
                        f"File {file_id} removed but cache invalidation failed: {e}"
                    )
            return StatusMessageResponse(status="ok", message="File removed")
        except ResourceNotFoundError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"Couldn't remove library file {file_id}: {e}")
            raise ExternalServiceError("Couldn't remove this file")

    async def _invalidate_caches_after_removal(self, album_mbid: str, artist_mbid: str | None, *, artist_removed: bool = False) -> None:
        # do NOT call library_db.clear() here: it would wipe unrelated sync_state and
        # the jellyfin/navidrome MBID indexes
        if self._memory_cache:
            keys_to_delete = [
                f"{ALBUM_INFO_PREFIX}{album_mbid}",
                f"{LIBRARY_ALBUM_DETAILS_PREFIX}{album_mbid}",
                library_requested_mbids_key(),
            ]
            if artist_mbid:
                keys_to_delete.extend([
                    f"{LIBRARY_ARTIST_ALBUMS_PREFIX}{artist_mbid}",
                    f"{LIBRARY_ARTIST_DETAILS_PREFIX}{artist_mbid}",
                    f"{ARTIST_INFO_PREFIX}{artist_mbid}",
                ])
            await asyncio.gather(
                *[self._memory_cache.delete(k) for k in keys_to_delete],
                self._memory_cache.clear_prefix(f"{LIBRARY_PREFIX}library:"),
                self._memory_cache.clear_prefix(f"{LIBRARY_PREFIX}artists:"),
                self._memory_cache.clear_prefix(LIBRARY_ALBUM_IMAGE_PREFIX),
                self._memory_cache.clear_prefix(LIBRARY_ALBUM_DETAILS_PREFIX),
                self._memory_cache.clear_prefix(LIBRARY_ALBUM_TRACKS_PREFIX),
                self._memory_cache.clear_prefix(LIBRARY_ALBUM_TRACKFILES_PREFIX),
                self._memory_cache.clear_prefix(LIBRARY_REQUESTED_PREFIX),
                self._memory_cache.clear_prefix(LIBRARY_ARTIST_IMAGE_PREFIX),
                self._memory_cache.clear_prefix(LIBRARY_ARTIST_DETAILS_PREFIX),
                self._memory_cache.clear_prefix(LIBRARY_ARTIST_ALBUMS_PREFIX),
            )

        if self._disk_cache:
            coros = [self._disk_cache.delete_album(album_mbid)]
            if artist_mbid:
                coros.append(self._disk_cache.delete_artist(artist_mbid))
            await asyncio.gather(*coros)

        if self._cover_repo:
            try:
                await self._cover_repo.delete_covers_for_album(album_mbid)
                if artist_mbid and artist_removed:
                    await self._cover_repo.delete_covers_for_artist(artist_mbid)
            except Exception:  # noqa: BLE001
                logger.warning("Failed to clean up cover images after removal", exc_info=True)

    async def _resolve_album_tracks(
        self,
        album_mbid: str,
    ) -> dict[str, tuple[str, str, str | None, float | None]]:
        # resolve to {disc:track: (source, source_id, format, duration)};
        # priority local -> navidrome -> jellyfin; source_resolution cache (1h TTL)
        if self._memory_cache is None:
            raise ExternalServiceError("Memory cache not available for track resolution")

        cache_key = f"{SOURCE_RESOLUTION_PREFIX}_tracks:{album_mbid}"
        cached = await self._memory_cache.get(cache_key)
        if cached is not None:
            return cached

        result: dict[str, tuple[str, str, str | None, float | None]] = {}

        def _track_key(disc: int, track: int) -> str:
            return f"{disc}:{track}"

        if self._local_files_service:
            try:
                match = await self._local_files_service.match_album_by_mbid(album_mbid)
                if match.found:
                    for t in match.tracks:
                        key = _track_key(getattr(t, "disc_number", 1) or 1, t.track_number)
                        if key not in result:
                            result[key] = (
                                "local",
                                str(t.track_file_id),
                                t.format or None,
                                t.duration_seconds,
                            )
            except Exception:  # noqa: BLE001
                logger.debug("Local track resolution failed for %s", album_mbid, exc_info=True)

        nd_enabled = False
        try:
            nd_settings = self._preferences_service.get_navidrome_connection_raw()
            nd_enabled = nd_settings.enabled
        except AttributeError:
            logger.debug("Navidrome settings unavailable during track resolution", exc_info=True)

        if nd_enabled and self._navidrome_library_service:
            try:
                nav_id = self._navidrome_library_service.lookup_navidrome_id(album_mbid)
                if nav_id:
                    detail = await self._navidrome_library_service.get_album_detail(nav_id)
                    if detail:
                        for t in detail.tracks:
                            key = _track_key(getattr(t, "disc_number", 1) or 1, t.track_number)
                            if key not in result:
                                result[key] = (
                                    "navidrome",
                                    t.navidrome_id,
                                    t.codec,
                                    t.duration_seconds,
                                )
            except Exception:  # noqa: BLE001
                logger.debug("Navidrome track resolution failed for %s", album_mbid, exc_info=True)

        jf_enabled = False
        try:
            jf_settings = self._preferences_service.get_jellyfin_connection()
            jf_enabled = jf_settings.enabled
        except AttributeError:
            logger.debug("Jellyfin settings unavailable during track resolution", exc_info=True)

        if jf_enabled and self._jellyfin_library_service:
            try:
                match = await self._jellyfin_library_service.match_album_by_mbid(album_mbid)
                if match.found:
                    all_same = len(match.tracks) > 1 and len({t.track_number for t in match.tracks}) == 1
                    if not all_same:
                        for t in match.tracks:
                            key = _track_key(getattr(t, "disc_number", 1) or 1, t.track_number)
                            if key not in result:
                                result[key] = (
                                    "jellyfin",
                                    t.jellyfin_id,
                                    t.codec,
                                    t.duration_seconds,
                                )
            except Exception:  # noqa: BLE001
                logger.debug("Jellyfin track resolution failed for %s", album_mbid, exc_info=True)

        await self._memory_cache.set(cache_key, result, ttl_seconds=3600)
        return result

    async def resolve_tracks_batch(
        self,
        items: list,
    ) -> TrackResolveResponse:
        items = items[:MAX_RESOLVE_ITEMS]
        if not items:
            return TrackResolveResponse(items=[])

        album_mbids = {it.release_group_mbid for it in items if it.release_group_mbid}

        sem = asyncio.Semaphore(5)

        async def _resolve_one(mbid: str) -> tuple[str, dict]:
            async with sem:
                return mbid, await self._resolve_album_tracks(mbid)

        tasks = [_resolve_one(mbid) for mbid in album_mbids]
        album_maps: dict[str, dict] = {}
        for r in await asyncio.gather(*tasks, return_exceptions=True):
            if isinstance(r, Exception):
                logger.warning("Album resolution failed: %s", r)
                continue
            mbid, track_map = r
            album_maps[mbid] = track_map

        resolved: list[ResolvedTrack] = []
        for item in items:
            base = ResolvedTrack(
                release_group_mbid=item.release_group_mbid,
                disc_number=item.disc_number,
                track_number=item.track_number,
            )

            if not item.release_group_mbid or item.track_number is None:
                resolved.append(base)
                continue

            track_map = album_maps.get(item.release_group_mbid, {})
            lookup_key = f"{item.disc_number or 1}:{item.track_number}"
            match = track_map.get(lookup_key)
            if not match:
                resolved.append(base)
                continue

            source, source_id, fmt, duration = match
            stream_url = None
            if source == "local":
                stream_url = f"/api/v1/stream/local/{source_id}"
            elif source == "navidrome":
                stream_url = f"/api/v1/stream/navidrome/{source_id}"
            elif source == "jellyfin":
                stream_url = f"/api/v1/stream/jellyfin/{source_id}"

            resolved.append(ResolvedTrack(
                release_group_mbid=item.release_group_mbid,
                disc_number=item.disc_number,
                track_number=item.track_number,
                source=source,
                track_source_id=source_id,
                stream_url=stream_url,
                format=fmt,
                duration=duration,
            ))

        return TrackResolveResponse(items=resolved)
