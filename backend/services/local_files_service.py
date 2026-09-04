import asyncio
import logging
import random
import re
import shutil
import tempfile
import zipfile
from collections.abc import AsyncGenerator
from pathlib import Path
from typing import Any

import aiofiles

from api.v1.schemas.local_files import (
    CrateTrack,
    DecadeShelf,
    FormatInfo,
    LocalAlbumMatch,
    LocalAlbumSummary,
    LocalPaginatedResponse,
    LocalSearchResponse,
    LocalStorageStats,
    LocalTrackInfo,
)
from core.exceptions import (
    ExternalServiceError,
    RangeNotSatisfiableError,
    ResourceNotFoundError,
)
from infrastructure.cache.cache_keys import LOCAL_FILES_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cover_urls import prefer_release_group_cover_url
from infrastructure.constants import STREAM_CHUNK_SIZE
from infrastructure.serialization import to_jsonable
from repositories.protocols import LibraryRepositoryProtocol
from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

AUDIO_EXTENSIONS: set[str] = {
    ".flac",
    ".mp3",
    ".ogg",
    ".m4a",
    ".aac",
    ".wav",
    ".wma",
    ".opus",
}

CONTENT_TYPE_MAP: dict[str, str] = {
    ".flac": "audio/flac",
    ".mp3": "audio/mpeg",
    ".ogg": "audio/ogg",
    ".m4a": "audio/mp4",
    ".aac": "audio/aac",
    ".wav": "audio/wav",
    ".wma": "audio/x-ms-wma",
    ".opus": "audio/opus",
}

_LOCAL_SORT_TO_NATIVE: dict[str, str] = {
    "name": "title",
    "date_added": "recent",
    "year": "recent",
    "random": "random",
    "rediscover": "random",
}

_INVALID_FILENAME_CHARS = re.compile(r'[\x00-\x1f\\/:*?"<>|]')


def sanitize_filename(name: str) -> str:
    """Replace characters that are invalid in filenames across OS platforms."""
    return _INVALID_FILENAME_CHARS.sub("_", name).strip() or "Untitled"


class LocalFilesService:
    _DEFAULT_RECENTLY_ADDED_TTL = 120

    def __init__(
        self,
        library_repo: LibraryRepositoryProtocol,
        preferences_service: PreferencesService,
        cache: CacheInterface,
    ):
        self._library_repo = library_repo
        self._preferences = preferences_service
        self._cache = cache

    def is_configured(self) -> bool:
        try:
            return bool(self._library_repo.is_configured())
        except Exception:  # noqa: BLE001
            return True

    def _get_library_roots(self) -> list[Path]:
        """Configured native-library scan roots (Settings -> Library).

        The native scanner walks these and stores absolute host paths, so file
        access validates against them directly - no Lidarr-era path remapping."""
        settings = self._preferences.get_typed_library_settings()
        return [Path(root.path) for root in settings.library_roots if root.path]

    def _get_recently_added_ttl(self) -> int:
        try:
            return self._preferences.get_advanced_settings().cache_ttl_local_files_recently_added
        except Exception:  # noqa: BLE001
            return self._DEFAULT_RECENTLY_ADDED_TTL

    def _resolve_and_validate_path(self, library_path: str) -> Path:
        canonical = Path(library_path).resolve()
        roots = [root.resolve() for root in self._get_library_roots()]
        if not any(canonical.is_relative_to(root) for root in roots):
            raise PermissionError("Path outside library directories")
        if not canonical.exists():
            raise ResourceNotFoundError(f"File not found: {canonical.name}")
        return canonical

    async def get_track_file_path(self, file_id: str) -> str:
        try:
            data = await self._library_repo.get_file_row_by_id(file_id)
            if not data:
                raise ResourceNotFoundError(f"Track file {file_id} not found")
            return str(data.get("file_path") or "")
        except ResourceNotFoundError:
            raise
        except Exception as e:  # noqa: BLE001
            raise ExternalServiceError(f"Failed to get track file: {e}")

    async def resolve_validated_path(self, file_id: str) -> Path:
        """Public within-roots-validated absolute path for a file_id (compat
        transcode input, 05 s8). Only this validated Path is ever handed to ffmpeg;
        no client-supplied filename reaches the argv."""
        data = await self._library_repo.get_file_row_by_id(file_id)
        if not data or data.get("deleted_at"):
            raise ResourceNotFoundError(f"Track file {file_id} not found")
        return self._resolve_and_validate_path(str(data.get("file_path") or ""))

    async def head_track(self, file_id: str) -> dict[str, str]:
        library_path = await self.get_track_file_path(file_id)
        file_path = self._resolve_and_validate_path(library_path)

        suffix = file_path.suffix.lower()
        if suffix not in AUDIO_EXTENSIONS:
            raise ExternalServiceError(
                f"Unsupported audio format: {suffix or 'unknown'}"
            )

        try:
            stat_result = await asyncio.to_thread(file_path.stat)
        except OSError as exc:
            raise ResourceNotFoundError(f"Cannot access file: {file_path.name} ({exc})")

        content_type = CONTENT_TYPE_MAP.get(suffix, "application/octet-stream")
        return {
            "Content-Type": content_type,
            "Content-Length": str(stat_result.st_size),
            "Accept-Ranges": "bytes",
        }

    async def stream_track(
        self,
        file_id: str,
        range_header: str | None = None,
    ) -> tuple[AsyncGenerator[bytes, None], dict[str, str], int]:
        library_path = await self.get_track_file_path(file_id)
        file_path = self._resolve_and_validate_path(library_path)

        suffix = file_path.suffix.lower()
        if suffix not in AUDIO_EXTENSIONS:
            raise ExternalServiceError(
                f"Unsupported audio format: {suffix or 'unknown'}"
            )

        try:
            stat_result = await asyncio.to_thread(file_path.stat)
        except OSError as exc:
            raise ResourceNotFoundError(f"Cannot access file: {file_path.name} ({exc})")

        file_size = stat_result.st_size
        content_type = CONTENT_TYPE_MAP.get(suffix, "application/octet-stream")

        if range_header:
            match = re.fullmatch(r"bytes=([0-9]*)-([0-9]*)", range_header)
            if match is None or not any(match.groups()) or file_size == 0:
                raise RangeNotSatisfiableError(file_size)
            start_str, end_str = match.groups()
            if not start_str:
                suffix_len = int(end_str)
                if suffix_len <= 0:
                    raise RangeNotSatisfiableError(file_size)
                start = max(0, file_size - suffix_len)
                end = file_size - 1
            elif not end_str:
                start = int(start_str)
                end = file_size - 1
            else:
                start = int(start_str)
                end = int(end_str)

            end = min(end, file_size - 1)
            if start < 0 or start > end or start >= file_size:
                raise RangeNotSatisfiableError(file_size)

            length = end - start + 1

            headers = {
                "Content-Type": content_type,
                "Content-Length": str(length),
                "Content-Range": f"bytes {start}-{end}/{file_size}",
                "Accept-Ranges": "bytes",
            }
            return self._iter_file(file_path, start, length), headers, 206

        headers = {
            "Content-Type": content_type,
            "Content-Length": str(file_size),
            "Accept-Ranges": "bytes",
        }
        return self._iter_file(file_path, 0, file_size), headers, 200

    async def _iter_file(
        self, path: Path, offset: int, length: int
    ) -> AsyncGenerator[bytes, None]:
        remaining = length
        try:
            async with aiofiles.open(path, "rb") as f:
                await f.seek(offset)
                while remaining > 0:
                    chunk_size = min(STREAM_CHUNK_SIZE, remaining)
                    data = await f.read(chunk_size)
                    if not data:
                        break
                    remaining -= len(data)
                    yield data
        except OSError as exc:
            logger.warning(
                "Local file read error mid-stream",
                extra={"path": str(path), "error": str(exc)},
            )

    async def get_album_track_files(self, album_id: int) -> list[dict[str, Any]]:
        data = await self._library_repo.get_track_files_by_album(album_id)
        if not data:
            return []

        track_files = []
        for tf in data:
            path_str: str = tf.get("path", "")
            suffix = Path(path_str).suffix.lower().lstrip(".")
            quality = tf.get("quality", {})
            quality_detail = quality.get("quality", {})

            track_files.append(
                {
                    "track_file_id": tf.get("id"),
                    "path": path_str,
                    "size_bytes": tf.get("size", 0),
                    "format": suffix if suffix else "unknown",
                    "bitrate": quality_detail.get("bitrate"),
                    "date_added": tf.get("dateAdded"),
                }
            )

        return track_files

    async def _build_track_list(
        self, album_id: int
    ) -> tuple[list[LocalTrackInfo], int, dict[str, int]]:
        tracks = await self._library_repo.get_album_tracks(album_id)
        track_files = await self.get_album_track_files(album_id)

        file_map: dict[int, dict[str, Any]] = {
            tf["track_file_id"]: tf for tf in track_files if tf.get("track_file_id")
        }

        result: list[LocalTrackInfo] = []
        total_size = 0
        format_counts: dict[str, int] = {}

        for track in tracks:
            tf_id = track.get("track_file_id")
            has_file = track.get("has_file", False)
            if not has_file or not tf_id:
                continue

            tf = file_map.get(tf_id, {})
            fmt = tf.get("format", "unknown")
            size = tf.get("size_bytes", 0)
            total_size += size
            format_counts[fmt] = format_counts.get(fmt, 0) + 1

            raw_track_num = track.get("track_number") or track.get("position") or 0
            raw_disc_num = track.get("disc_number", 1) or 1
            try:
                track_num = int(raw_track_num)
            except (TypeError, ValueError):
                track_num = 0
            try:
                disc_num = int(raw_disc_num)
            except (TypeError, ValueError):
                disc_num = 1

            result.append(
                LocalTrackInfo(
                    track_file_id=tf_id,
                    title=track.get("title", "Unknown"),
                    track_number=track_num,
                    disc_number=disc_num,
                    duration_seconds=(track.get("duration_ms", 0) or 0) / 1000.0,
                    size_bytes=size,
                    format=fmt,
                    bitrate=tf.get("bitrate"),
                    date_added=tf.get("date_added"),
                )
            )

        return result, total_size, format_counts

    async def match_album_by_mbid(self, musicbrainz_id: str) -> LocalAlbumMatch:
        tracks = await self._library_repo.get_tracks(musicbrainz_id)
        if not tracks:
            return LocalAlbumMatch(found=False, musicbrainz_id=musicbrainz_id)

        result_tracks = [self._native_track_to_info(t) for t in tracks]
        total_size = sum(t.size_bytes for t in result_tracks)
        format_counts: dict[str, int] = {}
        for t in result_tracks:
            format_counts[t.format] = format_counts.get(t.format, 0) + 1
        primary_format = (
            max(format_counts, key=lambda k: format_counts[k])
            if format_counts
            else None
        )

        return LocalAlbumMatch(
            found=True,
            musicbrainz_id=musicbrainz_id,
            tracks=result_tracks,
            total_size_bytes=total_size,
            primary_format=primary_format,
        )

    async def get_download_track(self, file_id: str) -> tuple[Path, str, str]:
        """Resolve a track file for download. Returns (path, filename, media_type)."""
        library_path = await self.get_track_file_path(file_id)
        file_path = self._resolve_and_validate_path(library_path)
        suffix = file_path.suffix.lower()
        if suffix not in AUDIO_EXTENSIONS:
            raise ExternalServiceError(f"Unsupported audio format: {suffix}")
        media_type = CONTENT_TYPE_MAP.get(suffix, "application/octet-stream")
        filename = file_path.name
        return file_path, filename, media_type

    async def create_album_zip(self, album_id: int) -> tuple[Path, str]:
        """Build a ZIP of all tracks in an album. Returns (zip_path, zip_filename)."""
        album_data = await self._library_repo.get_album_by_id(album_id)
        if not album_data:
            raise ResourceNotFoundError(f"Album {album_id} not found")

        album_title = album_data.get("title") or "Unknown Album"
        artist_data = album_data.get("artist") or {}
        artist_name = artist_data.get("artistName") or "Unknown Artist"

        result_tracks, _, _ = await self._build_track_list(album_id)
        if not result_tracks:
            raise ResourceNotFoundError(f"No track files found for album {album_id}")

        # Pre-resolve all paths in the async context
        resolved: list[tuple[Path, LocalTrackInfo]] = []
        for track in result_tracks:
            try:
                library_path = await self.get_track_file_path(track.track_file_id)
                file_path = self._resolve_and_validate_path(library_path)
                resolved.append((file_path, track))
            except (ResourceNotFoundError, PermissionError, ExternalServiceError):
                logger.warning(
                    "Skipping track %s in album %s ZIP",
                    track.track_file_id,
                    album_id,
                )
                continue

        if not resolved:
            raise ResourceNotFoundError(f"No accessible files for album {album_id}")

        zip_filename = sanitize_filename(f"{artist_name} - {album_title}.zip")
        tmp_path = await asyncio.to_thread(self._write_zip_sync, resolved)
        return tmp_path, zip_filename

    async def create_album_zip_by_mbid(self, mbid: str) -> tuple[Path, str]:
        """Build a ZIP by MusicBrainz release-group ID."""
        album_data = await self._library_repo.get_album_by_mbid(mbid)
        if not album_data:
            raise ResourceNotFoundError(f"Album with MBID {mbid} not found")
        album_id = album_data.get("id")
        if not album_id:
            raise ResourceNotFoundError(f"Album with MBID {mbid} could not be resolved")
        return await self.create_album_zip(album_id)

    @staticmethod
    def _write_zip_sync(
        resolved: list[tuple[Path, "LocalTrackInfo"]],
    ) -> Path:
        tmp = tempfile.NamedTemporaryFile(suffix=".zip", delete=False)
        try:
            multi_disc = len({t.disc_number for _, t in resolved}) > 1
            with zipfile.ZipFile(tmp, "w", zipfile.ZIP_STORED) as zf:
                for file_path, track in sorted(
                    resolved, key=lambda r: (r[1].disc_number, r[1].track_number)
                ):
                    ext = file_path.suffix.lower()
                    title = sanitize_filename(track.title)
                    if multi_disc:
                        arcname = f"{track.disc_number:02d}-{track.track_number:02d} {title}{ext}"
                    else:
                        arcname = f"{track.track_number:02d} {title}{ext}"
                    zf.write(file_path, arcname)
            tmp.close()
            return Path(tmp.name)
        except BaseException:
            tmp.close()
            Path(tmp.name).unlink(missing_ok=True)
            raise

    def _native_album_to_summary(self, album: Any) -> LocalAlbumSummary:
        """Map a native ``LibraryAlbumSummary`` onto the local-files DTO."""
        mbid = album.release_group_mbid
        cover_url = prefer_release_group_cover_url(mbid, album.cover_url, size=500)
        return LocalAlbumSummary(
            musicbrainz_id=mbid,
            name=album.album_title or "Unknown",
            artist_name=album.album_artist_name or "Unknown",
            artist_mbid=None,
            year=album.year,
            track_count=album.track_count,
            total_size_bytes=album.total_size_bytes,
            primary_format=album.quality_format,
            cover_url=cover_url,
            date_added=str(album.last_imported_at) if album.last_imported_at else None,
        )

    def _native_track_to_info(self, track: Any) -> LocalTrackInfo:
        """Map a native ``LibraryTrack`` (UUID file id) onto the local-files DTO."""
        return LocalTrackInfo(
            track_file_id=track.id,
            title=track.track_title or "Unknown",
            track_number=track.track_number,
            disc_number=track.disc_number,
            duration_seconds=track.duration_seconds,
            size_bytes=track.file_size_bytes,
            format=track.file_format or "unknown",
            bitrate=track.bit_rate,
            date_added=None,
        )

    async def get_albums(
        self,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "name",
        sort_order: str = "asc",
        search_query: str | None = None,
        decade: int | None = None,
    ) -> LocalPaginatedResponse:
        page = (offset // max(limit, 1)) + 1
        sort = _LOCAL_SORT_TO_NATIVE.get(sort_by, "recent")
        albums, total = await self._library_repo.get_albums_page(
            page=page,
            page_size=limit,
            sort=sort,
            q=search_query,
            file_format=None,
            decade=decade,
        )
        summaries = [self._native_album_to_summary(a) for a in albums]
        return LocalPaginatedResponse(
            items=summaries, total=total, offset=offset, limit=limit
        )

    def _row_to_crate_track(self, row: dict, reason: str) -> CrateTrack:
        mbid = row.get("release_group_mbid")
        cover_url = prefer_release_group_cover_url(mbid, row.get("cover_url"), size=300)
        return CrateTrack(
            track_file_id=str(row.get("id") or ""),
            title=row.get("track_title") or "Unknown",
            album_name=row.get("album_title") or "Unknown",
            artist_name=row.get("album_artist_name")
            or row.get("artist_name")
            or "Unknown",
            album_mbid=mbid,
            cover_url=cover_url,
            format=row.get("file_format") or "",
            year=row.get("year"),
            duration_seconds=row.get("duration_seconds"),
            reason=reason,
        )

    async def get_crate_suggestions(
        self, limit: int = 12, decade: int | None = None
    ) -> list[CrateTrack]:
        """A shuffled mix of reason-tagged track suggestions for the crate: newest
        imports, rediscoveries (oldest), surprises (random), and - when a decade is
        supplied (the now-playing era) - same-era picks."""
        pools: list[tuple[str, dict]] = [
            ("recent", {"order": "recent"}),
            ("rediscover", {"order": "oldest"}),
            ("surprise", {"order": "random"}),
        ]
        if decade is not None:
            pools.append(("same_era", {"order": "random", "decade": decade}))
        per = max(1, -(-limit // len(pools)))  # ceil so we over-fetch then trim

        items: list[CrateTrack] = []
        seen: set[str] = set()
        for reason, kwargs in pools:
            rows = await self._library_repo.get_crate_tracks(limit=per, **kwargs)
            for row in rows:
                fid = str(row.get("id") or "")
                if not fid or fid in seen:
                    continue
                seen.add(fid)
                items.append(self._row_to_crate_track(row, reason))

        random.shuffle(items)
        return items[:limit]

    async def get_decades(self, albums_per_decade: int = 12) -> list[DecadeShelf]:
        """Decade shelves (newest first), each with a preview of its albums."""
        buckets = await self._library_repo.get_decades()
        shelves: list[DecadeShelf] = []
        for bucket in buckets:
            decade = int(bucket.get("decade") or 0)
            count = int(bucket.get("album_count") or 0)
            if decade <= 0 or count == 0:
                continue
            albums, _ = await self._library_repo.get_albums_page(
                page=1, page_size=albums_per_decade, sort="recent", decade=decade
            )
            shelves.append(
                DecadeShelf(
                    decade=decade,
                    label=f"{decade}s",
                    album_count=count,
                    albums=[self._native_album_to_summary(a) for a in albums],
                )
            )
        return shelves

    async def get_album_tracks_by_id(self, mbid: str) -> list[LocalTrackInfo]:
        tracks = await self._library_repo.get_tracks(mbid)
        return [self._native_track_to_info(t) for t in tracks]

    async def search_tracks(self, query: str, limit: int = 30) -> list[CrateTrack]:
        rows = await self._library_repo.search_tracks(query, limit=limit)
        return [self._row_to_crate_track(row, "surprise") for row in rows]

    async def search(self, query: str) -> LocalSearchResponse:
        """Combined library search: matching albums plus matching individual
        tracks (so typing an album name surfaces that album's songs)."""
        album_result = await self.get_albums(limit=20, offset=0, search_query=query)
        tracks = await self.search_tracks(query, limit=30)
        return LocalSearchResponse(albums=album_result.items, tracks=tracks)

    async def get_recently_added(self, limit: int = 20) -> list[LocalAlbumSummary]:
        ttl_seconds = self._get_recently_added_ttl()
        cache_key = f"{LOCAL_FILES_PREFIX}recently_added:{limit}"
        cached = await self._cache.get(cache_key)
        if isinstance(cached, list):
            try:
                return [
                    LocalAlbumSummary(**item)
                    for item in cached
                    if isinstance(item, dict)
                ]
            except (TypeError, ValueError):
                logger.debug("Ignoring invalid cached recently-added payload")

        albums, _ = await self._library_repo.get_albums_page(
            page=1, page_size=limit, sort="recent"
        )
        summaries = [self._native_album_to_summary(a) for a in albums]

        await self._cache.set(
            cache_key,
            [to_jsonable(summary) for summary in summaries],
            ttl_seconds=ttl_seconds,
        )
        return summaries

    async def get_storage_stats(self) -> LocalStorageStats:
        """Counts come from the same library aggregate (``get_stats``) the "Manage
        your Library" card uses, so the two home cards can't disagree and on-disk
        junk (empty/stub dirs, NAS metadata, unsupported formats) can't skew them.
        Only disk-free is read from the filesystem."""
        roots = [root for root in self._get_library_roots() if root.exists()]
        if not roots:
            return LocalStorageStats()
        stats = await self._library_repo.get_stats()
        disk = shutil.disk_usage(roots[0])
        return LocalStorageStats(
            total_tracks=stats.total_tracks,
            total_albums=stats.total_albums,
            total_artists=stats.total_artists,
            total_size_bytes=stats.total_size_bytes,
            total_size_human=self._human_size(stats.total_size_bytes),
            disk_free_bytes=disk.free,
            disk_free_human=self._human_size(disk.free),
            format_breakdown={
                fmt: FormatInfo(count=count)
                for fmt, count in stats.format_breakdown.items()
            },
        )

    @staticmethod
    def _human_size(size_bytes: int) -> str:
        size = float(size_bytes)
        for unit in ("B", "KB", "MB", "GB", "TB"):
            if abs(size) < 1024.0:
                return f"{size:.1f} {unit}"
            size /= 1024.0
        return f"{size:.1f} PB"
