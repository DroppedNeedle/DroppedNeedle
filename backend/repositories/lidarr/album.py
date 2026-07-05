import asyncio
import collections
import logging
import time
from typing import Any, Optional
from core.exceptions import ExternalServiceError
from infrastructure.cover_urls import prefer_release_group_cover_url
from infrastructure.cache.cache_keys import (
    LIDARR_ALBUM_IMAGE_PREFIX, LIDARR_ALBUM_DETAILS_PREFIX,
    LIDARR_ALBUM_TRACKS_PREFIX, LIDARR_TRACKFILE_PREFIX, LIDARR_ALBUM_TRACKFILES_PREFIX,
    LIDARR_PREFIX,
)
from infrastructure.http.deduplication import RequestDeduplicator
from .base import LidarrBase
from .history import LidarrHistoryRepository

logger = logging.getLogger(__name__)

_album_details_deduplicator = RequestDeduplicator()

_MAX_ARTIST_LOCKS = 64
_artist_locks: collections.OrderedDict[str, asyncio.Lock] = collections.OrderedDict()


def _get_artist_lock(artist_mbid: str) -> asyncio.Lock:
    """Get or create a per-artist lock. Evicts only unlocked entries when over limit."""
    if artist_mbid in _artist_locks:
        _artist_locks.move_to_end(artist_mbid)
        return _artist_locks[artist_mbid]
    lock = asyncio.Lock()
    _artist_locks[artist_mbid] = lock
    while len(_artist_locks) > _MAX_ARTIST_LOCKS:
        # Find the oldest unlocked entry to evict
        evicted = False
        for key in list(_artist_locks.keys()):
            if key == artist_mbid:
                continue
            if not _artist_locks[key].locked():
                del _artist_locks[key]
                evicted = True
                break
        if not evicted:
            break
    return lock


def _safe_int(value: Any, fallback: int = 0) -> int:
    """Coerce a value to int, returning fallback for non-numeric inputs."""
    if isinstance(value, int):
        return value
    try:
        return int(value)
    except (TypeError, ValueError):
        return fallback


class LidarrAlbumRepository(LidarrHistoryRepository):
    async def get_all_albums(self) -> list[dict[str, Any]]:
        return await self._get_all_albums_raw()

    async def get_album_by_id(self, album_id: int) -> dict[str, Any] | None:
        try:
            data = await self._get(f"/api/v1/album/{album_id}")
            return data
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to get album %s from Lidarr: %s", album_id, e)
            return None

    async def get_album_by_mbid(self, mbid: str) -> dict[str, Any] | None:
        """Look up a Lidarr album by MusicBrainz release-group ID."""
        try:
            data = await self._get("/api/v1/album", params={"foreignAlbumId": mbid})
            if not data or not isinstance(data, list) or len(data) == 0:
                return None
            return data[0]
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to get album by MBID %s from Lidarr: %s", mbid, e)
            return None

    async def search_for_album(self, term: str) -> list[dict]:
        params = {"term": term}
        return await self._get("/api/v1/album/lookup", params=params)

    async def get_album_image_url(self, album_mbid: str, size: Optional[int] = 500) -> Optional[str]:
        cache_key = f"{LIDARR_ALBUM_IMAGE_PREFIX}{album_mbid}:{size or 'orig'}"
        cached_url = await self._cache.get(cache_key)
        if cached_url is not None:
            return cached_url if cached_url else None

        try:
            data = await self._get("/api/v1/album", params={"foreignAlbumId": album_mbid})
            if not data or not isinstance(data, list) or len(data) == 0:
                await self._cache.set(cache_key, "", ttl_seconds=300)
                return None

            album = data[0]
            album_id = album.get("id")
            images = album.get("images", [])

            if not album_id or not images:
                await self._cache.set(cache_key, "", ttl_seconds=300)
                return None

            cover_url = None
            for img in images:
                cover_type = img.get("coverType", "").lower()
                url_path = img.get("url", "")

                if not url_path:
                    continue

                if url_path.startswith("http"):
                    constructed_url = url_path
                else:
                    constructed_url = self._build_api_media_cover_url_album(album_id, url_path, size)

                if cover_type == "cover":
                    cover_url = constructed_url
                    break
                elif not cover_url:
                    cover_url = constructed_url

            if cover_url:
                await self._cache.set(cache_key, cover_url, ttl_seconds=3600)
                return cover_url

            await self._cache.set(cache_key, "", ttl_seconds=300)
            return None

        except Exception as e:  # noqa: BLE001
            return None

    async def get_album_details(self, album_mbid: str) -> Optional[dict[str, Any]]:
        cache_key = f"{LIDARR_ALBUM_DETAILS_PREFIX}{album_mbid}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached if cached else None

        return await _album_details_deduplicator.dedupe(
            f"lidarr-album-details:{album_mbid}",
            lambda: self._fetch_album_details(album_mbid, cache_key),
        )

    async def _fetch_album_details(self, album_mbid: str, cache_key: str) -> Optional[dict[str, Any]]:

        try:
            data = await self._get("/api/v1/album", params={"foreignAlbumId": album_mbid})
            if not data or not isinstance(data, list) or len(data) == 0:
                await self._cache.set(cache_key, "", ttl_seconds=300)
                return None

            album = data[0]
            album_id = album.get("id")

            cover_url = prefer_release_group_cover_url(
                album.get("foreignAlbumId"),
                self._get_album_cover_url(album.get("images", []), album_id),
                size=500,
            )

            links = []
            for link in album.get("links", []):
                link_name = link.get("name", "")
                link_url = link.get("url", "")
                if link_url:
                    links.append({"name": link_name, "url": link_url})

            artist_data = album.get("artist", {})

            releases = album.get("releases", [])
            primary_release = None
            for rel in releases:
                if rel.get("monitored"):
                    primary_release = rel
                    break
            if not primary_release and releases:
                primary_release = releases[0]

            media = []
            track_count = 0
            if primary_release:
                for medium in primary_release.get("media", []):
                    medium_info = {
                        "number": medium.get("mediumNumber", 1),
                        "format": medium.get("mediumFormat", "Unknown"),
                        "track_count": medium.get("mediumTrackCount", 0),
                    }
                    media.append(medium_info)
                    track_count += medium.get("mediumTrackCount", 0)

            result = {
                "id": album_id,
                "title": album.get("title", "Unknown"),
                "mbid": album.get("foreignAlbumId"),
                "overview": album.get("overview"),
                "disambiguation": album.get("disambiguation"),
                "album_type": album.get("albumType"),
                "secondary_types": album.get("secondaryTypes", []),
                "release_date": album.get("releaseDate"),
                "genres": album.get("genres", []),
                "links": links,
                "cover_url": cover_url,
                "monitored": album.get("monitored", False),
                "statistics": album.get("statistics", {}),
                "ratings": album.get("ratings", {}),
                "artist_name": artist_data.get("artistName", "Unknown"),
                "artist_mbid": artist_data.get("foreignArtistId"),
                "media": media,
                "track_count": track_count,
            }

            await self._cache.set(cache_key, result, ttl_seconds=300)
            return result

        except Exception as e:  # noqa: BLE001
            return None

    async def get_album_tracks(self, album_id: int) -> list[dict[str, Any]]:
        cache_key = f"{LIDARR_ALBUM_TRACKS_PREFIX}{album_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        try:
            data = await self._get("/api/v1/track", params={"albumId": album_id})
            if not data or not isinstance(data, list):
                await self._cache.set(cache_key, [], ttl_seconds=300)
                return []

            tracks = []
            for track in data:
                raw_track_num = track.get("trackNumber") or track.get("position") or track.get("absoluteTrackNumber", 0)
                track_number = _safe_int(raw_track_num)
                track_info = {
                    "position": track_number,
                    "track_number": track_number,
                    "absolute_position": _safe_int(track.get("absoluteTrackNumber", 0)),
                    "disc_number": _safe_int(track.get("mediumNumber", 1), fallback=1),
                    "title": track.get("title", "Unknown"),
                    "duration_ms": track.get("duration", 0),
                    "track_file_id": track.get("trackFileId"),
                    "has_file": track.get("hasFile", False),
                }
                tracks.append(track_info)

            tracks.sort(key=lambda t: (t["disc_number"], t["track_number"]))

            await self._cache.set(cache_key, tracks, ttl_seconds=300)
            return tracks

        except Exception as e:  # noqa: BLE001
            return []

    async def get_track_file(self, track_file_id: int) -> dict[str, Any] | None:
        cache_key = f"{LIDARR_TRACKFILE_PREFIX}{track_file_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        try:
            data = await self._get(f"/api/v1/trackfile/{track_file_id}")
            if data:
                await self._cache.set(cache_key, data, ttl_seconds=600)
            return data
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to get track file %s: %s", track_file_id, e)
            return None

    async def get_track_files_by_album(self, album_id: int) -> list[dict[str, Any]]:
        cache_key = f"{LIDARR_ALBUM_TRACKFILES_PREFIX}{album_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        try:
            data = await self._get(
                "/api/v1/trackfile",
                params={"albumId": album_id},
            )
            if not data or not isinstance(data, list):
                return []
            await self._cache.set(cache_key, data, ttl_seconds=300)
            return data
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to get track files for album %s: %s", album_id, e)
            return []

    async def _get_album_by_foreign_id(self, album_mbid: str) -> Optional[dict[str, Any]]:
        try:
            items = await self._get("/api/v1/album", params={"foreignAlbumId": album_mbid})
            return items[0] if items else None
        except Exception as e:  # noqa: BLE001
            return None

    _ALBUM_MUTABLE_FIELDS = frozenset({"monitored"})

    async def _update_album(self, album_id: int, updates: dict[str, Any]) -> dict[str, Any]:
        """Update album via PUT /album/{id} - synchronous 200 OK, returns updated object.

        Callers must hold the per-artist lock to avoid lost-update races.
        Only fields in _ALBUM_MUTABLE_FIELDS are applied unknown keys are silently dropped.
        """
        safe_updates = {k: v for k, v in updates.items() if k in self._ALBUM_MUTABLE_FIELDS}
        album = await self._get(f"/api/v1/album/{album_id}")
        album.update(safe_updates)
        return await self._put(f"/api/v1/album/{album_id}", album)

    async def delete_album(self, album_id: int, delete_files: bool = False) -> bool:
        try:
            params = {"deleteFiles": str(delete_files).lower(), "addImportListExclusion": "false"}
            await self._delete(f"/api/v1/album/{album_id}", params=params)
            await self._invalidate_album_list_caches()
            return True
        except Exception as e:
            logger.error(f"Failed to delete album {album_id}: {e}")
            raise

    async def set_monitored(self, album_mbid: str, monitored: bool) -> bool:
        lidarr_album = await self._get_album_by_foreign_id(album_mbid)
        if not lidarr_album:
            return False
        album_id = lidarr_album.get("id")
        artist_mbid = (lidarr_album.get("artist", {}).get("foreignArtistId") or "")
        if not album_id:
            return False
        lock = _get_artist_lock(artist_mbid)
        async with lock:
            await self._update_album(album_id, {"monitored": monitored})
        await self._invalidate_album_list_caches()
        return True

    async def add_album(self, musicbrainz_id: str, artist_repo) -> dict:
        t0 = time.monotonic()
        if not musicbrainz_id or not isinstance(musicbrainz_id, str):
            raise ExternalServiceError("Invalid MBID provided")

        lookup = await self._get("/api/v1/album/lookup", params={"term": f"mbid:{musicbrainz_id}"})
        if not lookup:
            raise ExternalServiceError(
                f"Album not found in Lidarr lookup (MBID: {musicbrainz_id})"
            )

        candidate = next(
            (a for a in lookup if a.get("foreignAlbumId") == musicbrainz_id),
            lookup[0]
        )
        album_title = candidate.get("title", "Unknown Album")
        album_type = candidate.get("albumType", "Unknown")
        secondary_types = candidate.get("secondaryTypes", [])

        artist_info = candidate.get("artist") or {}
        artist_mbid = artist_info.get("mbId") or artist_info.get("foreignArtistId")
        artist_name = artist_info.get("artistName")
        if not artist_mbid:
            raise ExternalServiceError("Album lookup did not include artist MBID")

        # Serialize per-artist to prevent duplicate artist creation from concurrent requests
        lock = _get_artist_lock(artist_mbid)
        async with lock:
            return await self._add_album_locked(
                musicbrainz_id, artist_repo, t0,
                candidate, album_title, album_type, secondary_types,
                artist_mbid, artist_name,
            )

    async def _add_album_locked(
        self,
        musicbrainz_id: str,
        artist_repo,
        t0: float,
        candidate: dict,
        album_title: str,
        album_type: str,
        secondary_types: list,
        artist_mbid: str,
        artist_name: str | None,
    ) -> dict:
        # Capture which albums are already monitored so we can revert any Lidarr auto-monitors after the add
        pre_add_monitored_ids: set[int] = set()
        try:
            existing_items = await self._get("/api/v1/artist", params={"mbId": artist_mbid})
            if existing_items:
                existing_artist_id = existing_items[0].get("id")
                if existing_artist_id:
                    albums_before = await self._get(
                        "/api/v1/album", params={"artistId": existing_artist_id}
                    )
                    if isinstance(albums_before, list):
                        pre_add_monitored_ids = {
                            a["id"] for a in albums_before if a.get("monitored")
                        }
        except ExternalServiceError:
            pass

        t_artist = time.monotonic()
        artist, artist_created = await artist_repo._ensure_artist_exists(artist_mbid, artist_name)
        artist_id = artist["id"]
        artist_ensure_ms = int((time.monotonic() - t_artist) * 1000)

        album_obj = await self._get_album_by_foreign_id(musicbrainz_id)

        if album_obj:
            album_id = album_obj["id"]
            has_files = (album_obj.get("statistics") or {}).get("trackFileCount", 0) > 0
            is_monitored = album_obj.get("monitored", False)

            if has_files and is_monitored:
                await self._invalidate_album_list_caches()
                return {
                    "message": f"Album already downloaded: {album_title}",
                    "payload": album_obj,
                }

            if not is_monitored:
                album_obj = await self._update_album(album_id, {"monitored": True})

            if not artist.get("monitored"):
                await self._put(
                    "/api/v1/artist/editor",
                    {"artistIds": [artist_id], "monitored": True, "monitorNewItems": "none"},
                )

            try:
                await self._post_command({"name": "AlbumSearch", "albumIds": [album_id]})
            except ExternalServiceError:
                pass

            await self._unmonitor_auto_monitored_albums(
                artist_id, musicbrainz_id, album_id, pre_add_monitored_ids
            )
            await self._invalidate_album_list_caches()
            await self._cache.clear_prefix(f"{LIDARR_PREFIX}artists:mbids")

            return {
                "message": f"Album monitored & search triggered: {album_title}",
                "monitored": True,
                "payload": album_obj,
            }

        # The album does not exist yet, so wait for indexing after the artist add or refresh.
        if artist_created:
            await self._wait_for_artist_commands_to_complete(artist_id, timeout=120.0)

        async def album_is_indexed():
            a = await self._get_album_by_foreign_id(musicbrainz_id)
            return a if a and a.get("id") else None

        # Only wait for auto-indexing if we just created/refreshed the artist;
        # for existing artists nothing triggered new indexing, so skip the long wait.
        if artist_created:
            album_obj = await self._wait_for(album_is_indexed, timeout=60.0, poll=5.0)
        else:
            album_obj = await album_is_indexed()

        if not album_obj:
            # Album not auto-indexed; POST to add it directly
            profile_id = artist.get("qualityProfileId")
            if profile_id is None:
                try:
                    qps = await self._get("/api/v1/qualityprofile")
                    if not qps:
                        raise ExternalServiceError("No quality profiles in Lidarr")
                    profile_id = qps[0]["id"]
                except Exception:  # noqa: BLE001
                    profile_id = self._settings.quality_profile_id

            payload = {
                "title": album_title,
                "artistId": artist_id,
                "artist": {
                    "id": artist["id"],
                    "artistName": artist.get("artistName"),
                    "foreignArtistId": artist.get("foreignArtistId"),
                    "qualityProfileId": artist.get("qualityProfileId"),
                    "metadataProfileId": artist.get("metadataProfileId"),
                    "rootFolderPath": artist.get("rootFolderPath"),
                },
                "foreignAlbumId": musicbrainz_id,
                "monitored": True,
                "anyReleaseOk": True,
                "profileId": profile_id,
                "images": [],
                "addOptions": {"addType": "automatic", "searchForNewAlbum": True},
            }

            try:
                album_obj = await self._post("/api/v1/album", payload)
                album_obj = await self._wait_for(album_is_indexed, timeout=120.0, poll=2.0)
            except ExternalServiceError as e:
                err_str = str(e).lower()
                if "already exists" in err_str:
                    album_obj = await self._get_album_by_foreign_id(musicbrainz_id)
                    if album_obj:
                        if not album_obj.get("monitored"):
                            album_obj = await self._update_album(album_obj["id"], {"monitored": True})
                        try:
                            await self._post_command(
                                {"name": "AlbumSearch", "albumIds": [album_obj["id"]]}
                            )
                        except ExternalServiceError:
                            pass
                elif "post failed" in err_str or "405" in err_str or "metadata" in err_str:
                    raise ExternalServiceError(
                        f"Lidarr rejected '{album_title}' ({album_type}"
                        f"{': ' + ', '.join(secondary_types) if secondary_types else ''}). "
                        f"Your Metadata Profile probably excludes {album_type}s. "
                        f"Go to Lidarr > Settings > Profiles > Metadata Profiles and enable '{album_type}'."
                    )
                else:
                    logger.error("Unexpected error adding '%s': %s", album_title, e)
                    raise

        if not album_obj or "id" not in album_obj:
            raise ExternalServiceError(
                f"'{album_title}' wasn't found in Lidarr after refreshing the artist. "
                f"Your Metadata Profile may exclude {album_type}s. "
                f"Go to Lidarr > Settings > Profiles > Metadata Profiles, enable '{album_type}', then refresh the artist."
            )

        album_id = album_obj["id"]

        # Only monitor the specific album; only set artist monitored if newly created
        await self._monitor_artist_and_album(
            artist_id, album_id, musicbrainz_id, album_title,
            set_artist_monitored=artist_created,
        )

        try:
            await self._post_command({"name": "AlbumSearch", "albumIds": [album_id]})
        except ExternalServiceError:
            pass

        # Unmonitor albums that Lidarr auto-monitored during the add
        await self._unmonitor_auto_monitored_albums(
            artist_id, musicbrainz_id, album_id, pre_add_monitored_ids
        )

        final_album = await self._get_album_by_foreign_id(musicbrainz_id)

        await self._invalidate_album_list_caches()
        await self._cache.clear_prefix(f"{LIDARR_PREFIX}artists:mbids")

        return {
            "message": f"Album added & monitored: {album_title}",
            "monitored": True,
            "payload": final_album or album_obj,
        }

    async def _wait_for_artist_commands_to_complete(self, artist_id: int, timeout: float = 120.0) -> None:
        deadline = time.monotonic() + timeout

        while time.monotonic() < deadline:
            try:
                commands = await self._get("/api/v1/command")
                if not commands:
                    break

                has_running_commands = False
                for cmd in commands:
                    status = cmd.get("status") or cmd.get("state")
                    if str(status).lower() in ["queued", "started"]:
                        body = cmd.get("body", {})
                        cmd_artist_id = body.get("artistId")
                        cmd_artist_ids = body.get("artistIds", [])

                        if not isinstance(cmd_artist_ids, list):
                            cmd_artist_ids = [cmd_artist_ids] if cmd_artist_ids else []

                        if cmd_artist_id == artist_id or artist_id in cmd_artist_ids:
                            has_running_commands = True
                            break

                if not has_running_commands:
                    break

            except Exception:  # noqa: BLE001
                pass

            await asyncio.sleep(5.0)

        await asyncio.sleep(1.0)

    async def _monitor_artist_and_album(
        self,
        artist_id: int,
        album_id: int,
        album_mbid: str,
        album_title: str,
        max_attempts: int = 2,
        set_artist_monitored: bool = False,
    ) -> None:
        for attempt in range(max_attempts):
            try:
                if set_artist_monitored and attempt == 0:
                    await self._put(
                        "/api/v1/artist/editor",
                        {"artistIds": [artist_id], "monitored": True, "monitorNewItems": "none"},
                    )

                updated = await self._update_album(album_id, {"monitored": True})
                if updated and updated.get("monitored"):
                    return

                if attempt < max_attempts - 1:
                    logger.warning("Album monitoring verification failed, attempt %d/%d", attempt + 1, max_attempts)
                    await asyncio.sleep(2.0 + (attempt * 2.0))

            except Exception as e:  # noqa: BLE001
                if attempt == max_attempts - 1:
                    raise ExternalServiceError(
                        f"Failed to set monitoring status after {max_attempts} attempts: {str(e)}"
                    )
                await asyncio.sleep(3.0)

    async def _unmonitor_auto_monitored_albums(
        self,
        artist_id: int,
        requested_mbid: str,
        requested_album_id: int,
        pre_add_monitored_ids: set[int],
    ) -> None:
        """Unmonitor albums that Lidarr auto-monitored during artist add (Aurral pattern)."""
        try:
            current_albums = await self._get(
                "/api/v1/album", params={"artistId": artist_id}
            )
            if not isinstance(current_albums, list):
                return

            to_unmonitor = [
                a["id"]
                for a in current_albums
                if a.get("monitored")
                and a["id"] != requested_album_id
                and a["id"] not in pre_add_monitored_ids
            ]

            if to_unmonitor:
                await self._put(
                    "/api/v1/album/monitor",
                    {"albumIds": to_unmonitor, "monitored": False},
                )
        except ExternalServiceError:
            pass
