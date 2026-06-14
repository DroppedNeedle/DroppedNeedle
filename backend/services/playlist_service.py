import asyncio
import logging
import re
from collections import defaultdict
from difflib import SequenceMatcher
from pathlib import Path
from typing import Any, Optional

from core.exceptions import InvalidPlaylistDataError, PlaylistNotFoundError, SourceResolutionError
from infrastructure.cache.cache_keys import SOURCE_RESOLUTION_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from repositories.async_playlist_repository import AsyncPlaylistRepository
from repositories.playlist_repository import (
    PlaylistRecord,
    PlaylistRepository,
    PlaylistSummaryRecord,
    PlaylistTrackRecord,
)

logger = logging.getLogger(__name__)

ALLOWED_IMAGE_TYPES = {"image/jpeg", "image/png", "image/webp"}
MAX_COVER_SIZE = 2 * 1024 * 1024
_MIME_TO_EXT = {"image/jpeg": ".jpg", "image/png": ".png", "image/webp": ".webp"}
_SAFE_ID_RE = re.compile(r"^[a-f0-9\-]+$")
VALID_SOURCE_TYPES = {"local", "jellyfin", "navidrome", "plex", "youtube", ""}
MAX_NAME_LENGTH = 100
# Albums in a playlist are resolved against external sources concurrently. A
# large playlist (300+ tracks) can span hundreds of albums; resolving them one
# at a time turned the synchronous /resolve-sources call into hundreds of serial
# round-trips and made big playlists appear to hang. Bound the fan-out so we
# don't hammer Navidrome/Plex/Jellyfin all at once.
_ALBUM_RESOLVE_CONCURRENCY = 8

_SOURCE_TYPE_ALIASES = {
    "local": "local",
    "howler": "local",
    "jellyfin": "jellyfin",
    "navidrome": "navidrome",
    "plex": "plex",
    "youtube": "youtube",
    "": "",
}


def _normalize_source_map(by_num: dict) -> dict[int, tuple[str, str]]:
    """Ensure source map keys are ints (guards against cached string keys)."""
    if not by_num:
        return by_num
    first_key = next(iter(by_num))
    if isinstance(first_key, int):
        return by_num
    normalized: dict[int, tuple[str, str]] = {}
    for k, v in by_num.items():
        try:
            normalized[int(k)] = v
        except (TypeError, ValueError):
            continue
    return normalized


def _safe_track_number(value: object) -> int | None:
    """Coerce a track number to int, returning None for non-numeric inputs."""
    if isinstance(value, int):
        return value
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _fuzzy_name_match(name1: str, name2: str) -> bool:
    if not name1 or not name2:
        return False
    n1, n2 = name1.lower().strip(), name2.lower().strip()
    if n1 == n2:
        return True
    if n1 in n2 or n2 in n1:
        return True
    return SequenceMatcher(None, n1, n2).ratio() > 0.6


class PlaylistService:
    def __init__(
        self,
        repo: PlaylistRepository,
        cache_dir: Path,
        cache: Optional[CacheInterface] = None,
        genre_index: Any = None,
    ):
        self._repo = AsyncPlaylistRepository(repo)
        self._cover_dir = cache_dir / "covers" / "playlists"
        self._cache = cache
        self._genre_index = genre_index


    async def create_playlist(self, name: str, *, source_ref: str | None = None) -> PlaylistRecord:
        stripped = name.strip() if name else ""
        if not stripped:
            raise InvalidPlaylistDataError("Playlist name must not be empty")
        if len(stripped) > MAX_NAME_LENGTH:
            raise InvalidPlaylistDataError(f"Playlist name must not exceed {MAX_NAME_LENGTH} characters")
        result = await self._repo.create_playlist(stripped, source_ref=source_ref)
        return result

    async def get_by_source_ref(self, source_ref: str) -> PlaylistRecord | None:
        return await self._repo.get_by_source_ref(source_ref)

    async def get_imported_source_ids(self, prefix: str) -> set[str]:
        return await self._repo.get_imported_source_ids(prefix)

    async def get_playlist(self, playlist_id: str) -> PlaylistRecord:
        result = await self._repo.get_playlist(playlist_id)
        if result is None:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        return result

    async def get_all_playlists(self) -> list[PlaylistSummaryRecord]:
        return await self._repo.get_all_playlists()

    async def get_playlist_with_tracks(
        self, playlist_id: str,
    ) -> tuple[PlaylistRecord, list[PlaylistTrackRecord]]:
        playlist = await self.get_playlist(playlist_id)
        tracks = await self._repo.get_tracks(playlist_id)
        return playlist, tracks

    async def update_playlist(
        self, playlist_id: str, name: Optional[str] = None,
    ) -> PlaylistRecord:
        if name is not None:
            stripped = name.strip()
            if not stripped:
                raise InvalidPlaylistDataError("Playlist name must not be empty")
            if len(stripped) > MAX_NAME_LENGTH:
                raise InvalidPlaylistDataError(f"Playlist name must not exceed {MAX_NAME_LENGTH} characters")
            name = stripped

        result = await self._repo.update_playlist(playlist_id, name=name)
        if result is None:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        return result

    async def update_playlist_with_detail(
        self, playlist_id: str, name: Optional[str] = None,
    ) -> tuple[PlaylistRecord, list[PlaylistTrackRecord]]:
        playlist = await self.update_playlist(playlist_id, name=name)
        tracks = await self._repo.get_tracks(playlist_id)
        return playlist, tracks

    async def delete_playlist(self, playlist_id: str) -> None:
        deleted = await self._repo.delete_playlist(playlist_id)
        if not deleted:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        await asyncio.to_thread(self._delete_cover_file, playlist_id)


    async def add_tracks(
        self,
        playlist_id: str,
        tracks: list[dict],
        position: Optional[int] = None,
    ) -> list[PlaylistTrackRecord]:
        if not tracks:
            raise InvalidPlaylistDataError("Track list must not be empty")
        normalized_tracks: list[dict] = []
        for track in tracks:
            normalized = dict(track)
            st = normalized.get("source_type", "")
            if st and st not in _SOURCE_TYPE_ALIASES:
                raise InvalidPlaylistDataError(
                    f"Invalid source_type '{st}'. Allowed: {', '.join(sorted(_SOURCE_TYPE_ALIASES.keys() - {''}))}"  # noqa: E501
                )
            normalized["source_type"] = _SOURCE_TYPE_ALIASES.get(st, st)

            sources = normalized.get("available_sources")
            if sources is not None:
                normalized_sources: list[str] = []
                for source in sources:
                    if source not in _SOURCE_TYPE_ALIASES:
                        raise InvalidPlaylistDataError(
                            f"Invalid available source '{source}'. Allowed: {', '.join(sorted(_SOURCE_TYPE_ALIASES.keys() - {''}))}"  # noqa: E501
                        )
                    normalized_sources.append(_SOURCE_TYPE_ALIASES[source])
                normalized["available_sources"] = normalized_sources

            normalized_tracks.append(normalized)
        await self.get_playlist(playlist_id)
        result = await self._repo.add_tracks(playlist_id, normalized_tracks, position)
        return result

    async def remove_track(self, playlist_id: str, track_id: str) -> None:
        removed = await self._repo.remove_track(playlist_id, track_id)
        if not removed:
            raise PlaylistNotFoundError(f"Track {track_id} not found in playlist {playlist_id}")

    async def remove_tracks(self, playlist_id: str, track_ids: list[str]) -> int:
        if not track_ids:
            raise InvalidPlaylistDataError("No track IDs provided")
        removed = await self._repo.remove_tracks(playlist_id, track_ids)
        if not removed:
            raise PlaylistNotFoundError(f"No matching tracks found in playlist {playlist_id}")
        return removed

    async def reorder_track(
        self, playlist_id: str, track_id: str, new_position: int,
    ) -> int:
        if new_position < 0:
            raise InvalidPlaylistDataError("Position must be >= 0")
        result = await self._repo.reorder_track(playlist_id, track_id, new_position)
        if result is None:
            raise PlaylistNotFoundError(f"Track {track_id} not found in playlist {playlist_id}")
        return result

    async def update_track_source(
        self,
        playlist_id: str,
        track_id: str,
        source_type: Optional[str] = None,
        available_sources: Optional[list[str]] = None,
        jf_service: object = None,
        local_service: object = None,
        nd_service: object = None,
        plex_service: object = None,
    ) -> PlaylistTrackRecord:
        if source_type is not None and source_type not in _SOURCE_TYPE_ALIASES:
            raise InvalidPlaylistDataError(
                f"Invalid source_type '{source_type}'. Allowed: {', '.join(sorted(_SOURCE_TYPE_ALIASES.keys() - {''}))}"  # noqa: E501
            )

        normalized_source = _SOURCE_TYPE_ALIASES.get(source_type, source_type)
        normalized_available_sources = available_sources
        if available_sources is not None:
            normalized_available_sources = []
            for source in available_sources:
                if source not in _SOURCE_TYPE_ALIASES:
                    raise InvalidPlaylistDataError(
                        f"Invalid available source '{source}'. Allowed: {', '.join(sorted(_SOURCE_TYPE_ALIASES.keys() - {''}))}"  # noqa: E501
                    )
                normalized_available_sources.append(_SOURCE_TYPE_ALIASES[source])

        new_track_source_id: Optional[str] = None
        new_plex_rating_key_resolved = False
        new_plex_rating_key: Optional[str] = None
        if normalized_source:
            current_track = await self._repo.get_track(playlist_id, track_id)
            if current_track is None:
                raise PlaylistNotFoundError(f"Track {track_id} not found in playlist {playlist_id}")
            if normalized_source != current_track.source_type:
                new_track_source_id, new_plex_rating_key = await self._resolve_new_source_id(
                    current_track, normalized_source, jf_service, local_service, nd_service,
                    plex_service,
                )
                new_plex_rating_key_resolved = True

        repo_kwargs: dict[str, Any] = {
            "track_source_id": new_track_source_id,
        }
        if new_plex_rating_key_resolved:
            repo_kwargs["plex_rating_key"] = new_plex_rating_key

        result = await self._repo.update_track_source(
            playlist_id, track_id, normalized_source, normalized_available_sources,
            **repo_kwargs,
        )
        if result is None:
            raise PlaylistNotFoundError(f"Track {track_id} not found in playlist {playlist_id}")
        return result

    async def get_tracks(self, playlist_id: str) -> list[PlaylistTrackRecord]:
        return await self._repo.get_tracks(playlist_id)

    async def analyse_playlist_profile(
        self, playlist_id: str,
    ) -> "PlaylistProfile | None":
        from api.v1.schemas.discover import PlaylistProfile

        playlist = await self._repo.get_playlist(playlist_id)
        if playlist is None:
            return None

        tracks = await self._repo.get_tracks(playlist_id)
        artist_mbids = list({t.artist_id for t in tracks if t.artist_id})

        genre_distribution: dict[str, list[str]] = {}
        if artist_mbids and self._genre_index is not None:
            genre_distribution = await self._genre_index.get_genres_for_artists(artist_mbids)

        return PlaylistProfile(
            artist_mbids=artist_mbids,
            genre_distribution=genre_distribution,
            track_count=len(tracks),
        )

    async def check_track_membership(
        self, tracks: list[tuple[str, str, str]],
    ) -> dict[str, list[int]]:
        return await self._repo.check_track_membership(tracks)


    async def resolve_track_sources(
        self,
        playlist_id: str,
        jf_service: object = None,
        local_service: object = None,
        nd_service: object = None,
        plex_service: object = None,
    ) -> dict[str, list[str]]:
        await self.get_playlist(playlist_id)
        tracks = await self._repo.get_tracks(playlist_id)
        if not tracks:
            return {}

        album_groups: dict[str, list[PlaylistTrackRecord]] = defaultdict(list)
        no_album_tracks: list[PlaylistTrackRecord] = []
        for t in tracks:
            if t.album_id and t.track_number is not None:
                album_groups[t.album_id].append(t)
            else:
                no_album_tracks.append(t)

        result: dict[str, list[str]] = {}
        grouped = list(album_groups.items())
        sem = asyncio.Semaphore(_ALBUM_RESOLVE_CONCURRENCY)

        async def _resolve_group(
            album_tracks: list[PlaylistTrackRecord],
        ) -> tuple[
            dict[int, tuple[str, str]],
            dict[int, tuple[str, str]],
            dict[int, tuple[str, str]],
            dict[int, tuple[str, str, str]],
        ]:
            representative = album_tracks[0]
            async with sem:
                try:
                    return await self._resolve_album_sources(
                        representative.album_id, jf_service, local_service, nd_service, plex_service,
                        album_name=representative.album_name or "",
                        artist_name=representative.artist_name or "",
                    )
                except Exception:  # noqa: BLE001
                    # Now that album groups resolve concurrently, one group failing
                    # (e.g. a cache/infra error - upstream service errors are already
                    # caught inside _resolve_album_sources) must not discard every
                    # other album's results. Degrade to "no extra sources" for this
                    # album; its tracks keep their stored available_sources.
                    logger.warning(
                        "Source resolution failed for album %s; skipping",
                        representative.album_id, exc_info=True,
                    )
                    return ({}, {}, {}, {})

        resolved_maps = await asyncio.gather(
            *(_resolve_group(album_tracks) for _album_id, album_tracks in grouped)
        )

        for (_album_id, album_tracks), (
            jf_by_num,
            local_by_num,
            nd_by_num,
            plex_by_num,
        ) in zip(grouped, resolved_maps):
            for t in album_tracks:
                sources = set()
                if t.source_type:
                    sources.add(t.source_type)

                jf_track = jf_by_num.get(t.track_number)
                if jf_track and _fuzzy_name_match(t.track_name, jf_track[0]):
                    sources.add("jellyfin")

                local_track = local_by_num.get(t.track_number)
                if local_track and _fuzzy_name_match(t.track_name, local_track[0]):
                    sources.add("local")

                nd_track = nd_by_num.get(t.track_number)
                if nd_track and _fuzzy_name_match(t.track_name, nd_track[0]):
                    sources.add("navidrome")

                plex_track = plex_by_num.get(t.track_number)
                if plex_track and _fuzzy_name_match(t.track_name, plex_track[0]):
                    sources.add("plex")

                result[t.id] = sorted(sources)

        for t in no_album_tracks:
            result[t.id] = [t.source_type] if t.source_type else []

        persist_updates: dict[str, list[str]] = {}
        for t in tracks:
            resolved = result.get(t.id)
            if not resolved:
                continue
            existing = set(t.available_sources) if t.available_sources else set()
            if set(resolved) >= existing and set(resolved) != existing:
                persist_updates[t.id] = resolved
        if persist_updates:
            await self._repo.batch_update_available_sources(playlist_id, persist_updates)

        return result

    async def _resolve_album_sources(
        self,
        album_id: str,
        jf_service: object,
        local_service: object,
        nd_service: object = None,
        plex_service: object = None,
        album_name: str = "",
        artist_name: str = "",
    ) -> tuple[dict[int, tuple[str, str]], dict[int, tuple[str, str]], dict[int, tuple[str, str]], dict[int, tuple[str, str, str]]]:
        cache_key = f"{SOURCE_RESOLUTION_PREFIX}:{album_id}"
        if self._cache:
            cached = await self._cache.get(cache_key)
            if cached is not None:
                if len(cached) == 2:
                    return (_normalize_source_map(cached[0]), _normalize_source_map(cached[1]), {}, {})
                if len(cached) == 3:
                    return (
                        _normalize_source_map(cached[0]),
                        _normalize_source_map(cached[1]),
                        _normalize_source_map(cached[2]),
                        {},
                    )
                return (
                    _normalize_source_map(cached[0]),
                    _normalize_source_map(cached[1]),
                    _normalize_source_map(cached[2]),
                    _normalize_source_map(cached[3]),
                )

        jf_by_num: dict[int, tuple[str, str]] = {}
        local_by_num: dict[int, tuple[str, str]] = {}
        nd_by_num: dict[int, tuple[str, str]] = {}
        plex_by_num: dict[int, tuple[str, str, str]] = {}

        if jf_service is not None:
            try:
                match = await jf_service.match_album_by_mbid(album_id)
                if match.found:
                    for t in match.tracks:
                        key = _safe_track_number(t.track_number)
                        if key is not None:
                            jf_by_num[key] = (t.title, t.jellyfin_id)
            except Exception:  # noqa: BLE001
                logger.debug("Jellyfin source resolution failed for album %s", album_id, exc_info=True)

        if local_service is not None:
            try:
                match = await local_service.match_album_by_mbid(album_id)
                if match.found:
                    for t in match.tracks:
                        key = _safe_track_number(t.track_number)
                        if key is not None:
                            local_by_num[key] = (t.title, str(t.track_file_id))
            except Exception:  # noqa: BLE001
                logger.debug("Local source resolution failed for album %s", album_id, exc_info=True)

        if nd_service is not None:
            try:
                match = await nd_service.get_album_match(
                    album_id=album_id, album_name=album_name, artist_name=artist_name,
                )
                if match.found:
                    for t in match.tracks:
                        key = _safe_track_number(t.track_number)
                        if key is not None:
                            nd_by_num[key] = (t.title, t.navidrome_id)
            except Exception:  # noqa: BLE001
                logger.debug("Navidrome source resolution failed for album %s", album_id, exc_info=True)

        if plex_service is not None:
            try:
                match = await plex_service.get_album_match(
                    album_id=album_id, album_name=album_name, artist_name=artist_name,
                )
                if match.found:
                    for t in match.tracks:
                        key = _safe_track_number(t.track_number)
                        if key is not None:
                            plex_by_num[key] = (t.title, t.part_key or t.plex_id, t.plex_id)
            except Exception:  # noqa: BLE001
                logger.debug("Plex source resolution failed for album %s", album_id, exc_info=True)

        resolved = (jf_by_num, local_by_num, nd_by_num, plex_by_num)
        if self._cache:
            await self._cache.set(cache_key, resolved, ttl_seconds=3600)
        return resolved

    async def _resolve_new_source_id(
        self,
        track: PlaylistTrackRecord,
        new_source_type: str,
        jf_service: object,
        local_service: object,
        nd_service: object = None,
        plex_service: object = None,
    ) -> tuple[str, str | None]:
        """Return (source_id, plex_rating_key_or_none)."""
        if not track.album_id or track.track_number is None:
            raise SourceResolutionError(
                f"Cannot switch source for track '{track.track_name}': missing album_id or track_number"
            )

        jf_by_num, local_by_num, nd_by_num, plex_by_num = await self._resolve_album_sources(
            track.album_id, jf_service, local_service, nd_service, plex_service,
            album_name=track.album_name or "",
            artist_name=track.artist_name or "",
        )

        if new_source_type == "jellyfin":
            match_info = jf_by_num.get(track.track_number)
            if match_info and _fuzzy_name_match(track.track_name, match_info[0]):
                return (match_info[1], None)
            raise SourceResolutionError(
                f"Track '{track.track_name}' not found in Jellyfin for album {track.album_id}"
            )

        if new_source_type == "local":
            match_info = local_by_num.get(track.track_number)
            if match_info and _fuzzy_name_match(track.track_name, match_info[0]):
                return (match_info[1], None)
            raise SourceResolutionError(
                f"Track '{track.track_name}' not found in local files for album {track.album_id}"
            )

        if new_source_type == "navidrome":
            match_info = nd_by_num.get(track.track_number)
            if match_info and _fuzzy_name_match(track.track_name, match_info[0]):
                return (match_info[1], None)
            raise SourceResolutionError(
                f"Track '{track.track_name}' not found in Navidrome for album {track.album_id}"
            )

        if new_source_type == "plex":
            match_info = plex_by_num.get(track.track_number)
            if match_info and _fuzzy_name_match(track.track_name, match_info[0]):
                return (match_info[1], match_info[2])
            raise SourceResolutionError(
                f"Track '{track.track_name}' not found in Plex for album {track.album_id}"
            )

        raise SourceResolutionError(f"Unsupported source type for resolution: {new_source_type}")


    async def upload_cover(
        self, playlist_id: str, data: bytes, content_type: str,
    ) -> str:
        await self.get_playlist(playlist_id)
        self._validate_cover_id(playlist_id)

        if content_type not in ALLOWED_IMAGE_TYPES:
            raise InvalidPlaylistDataError(
                f"Invalid image type. Allowed: {', '.join(ALLOWED_IMAGE_TYPES)}"
            )
        if len(data) > MAX_COVER_SIZE:
            raise InvalidPlaylistDataError("Image too large. Maximum size is 2 MB")

        ext = _MIME_TO_EXT.get(content_type, ".jpg")
        file_path = self._cover_dir / f"{playlist_id}{ext}"

        def _write_cover() -> None:
            self._cover_dir.mkdir(parents=True, exist_ok=True)
            for old in self._cover_dir.glob(f"{playlist_id}.*"):
                try:
                    old.unlink()
                except OSError:
                    pass
            file_path.write_bytes(data)

        await asyncio.to_thread(_write_cover)

        cover_path = str(file_path)
        await self._repo.update_playlist(
            playlist_id, cover_image_path=cover_path,
        )

        cover_url = f"/api/v1/playlists/{playlist_id}/cover"
        return cover_url

    async def get_cover_path(self, playlist_id: str) -> Optional[Path]:
        playlist = await self.get_playlist(playlist_id)
        if not playlist.cover_image_path:
            return None
        path = Path(playlist.cover_image_path)
        exists = await asyncio.to_thread(path.exists)
        if exists:
            return path
        return None

    async def remove_cover(self, playlist_id: str) -> None:
        playlist = await self.get_playlist(playlist_id)
        if playlist.cover_image_path:
            cover_path = Path(playlist.cover_image_path)
            try:
                await asyncio.to_thread(cover_path.unlink, True)
            except OSError:
                pass
        await self._repo.update_playlist(
            playlist_id, cover_image_path=None,
        )


    @staticmethod
    def _validate_cover_id(playlist_id: str) -> None:
        if not _SAFE_ID_RE.match(playlist_id):
            raise InvalidPlaylistDataError("Invalid playlist ID for cover path")

    def _delete_cover_file(self, playlist_id: str) -> None:
        if not _SAFE_ID_RE.match(playlist_id):
            return
        for f in self._cover_dir.glob(f"{playlist_id}.*"):
            try:
                f.unlink()
            except OSError:
                pass
