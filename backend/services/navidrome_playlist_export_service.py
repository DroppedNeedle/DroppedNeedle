"""Export DroppedNeedle playlists as .m3u8 files for Navidrome to import.

Navidrome has no playlist write API but imports playlist files it finds while
scanning (``ND_PLAYLISTSPATH``, enabled by ``ND_AUTOIMPORTPLAYLISTS``). One-way:
Navidrome's own playlists are never read back.

The target directory is not exclusively ours, so ownership is proven by a marker
inside each file, a failed export keeps its previous file, and entries are
relative. No state is kept between runs.
"""

from __future__ import annotations

import asyncio
import logging
import os
import re
import unicodedata
import uuid
from dataclasses import dataclass, field
from pathlib import Path, PurePath

logger = logging.getLogger(__name__)

PLAYLIST_SUFFIX = ".m3u8"
OWNER_MARKER = "#DROPPEDNEEDLE-PLAYLIST-ID:"
_MARKER_SCAN_BYTES = 512

# Applied on every host, not just Windows: an exported directory is often a bind
# mount shared with one.
_WINDOWS_RESERVED = {
    "CON", "PRN", "AUX", "NUL",
    *(f"COM{i}" for i in range(1, 10)),
    *(f"LPT{i}" for i in range(1, 10)),
}
_UNSAFE_CHARS = re.compile(r'[<>:"/\\|?*\x00-\x1f]')
# M3U is line-oriented and has no escaping, so a newline in free text would
# inject a directive.
_CONTROL_CHARS = re.compile(r"[\x00-\x1f\x7f]")
_MAX_STEM = 120


@dataclass
class PlaylistSyncResult:
    success: bool = False
    message: str = ""
    written: int = 0
    unchanged: int = 0
    removed: int = 0
    skipped_empty: int = 0
    skipped_not_ours: int = 0
    removal_failures: int = 0
    tracks_missing_files: int = 0
    tracks_unrepresentable: int = 0
    errors: list[str] = field(default_factory=list)


def _one_line(value: str | None) -> str:
    return _CONTROL_CHARS.sub(" ", (value or "")).strip()


def safe_playlist_filename(name: str, playlist_id: str) -> str:
    """A portable filename. The id suffix carries uniqueness; names collide."""
    stem = unicodedata.normalize("NFC", name or "").strip()
    stem = _UNSAFE_CHARS.sub("_", stem)
    stem = stem.strip(" .")
    if stem.upper().split(".")[0] in _WINDOWS_RESERVED:
        stem = f"_{stem}"
    if len(stem) > _MAX_STEM:
        stem = stem[:_MAX_STEM].rstrip(" .")
    if not stem:
        stem = "playlist"
    return f"{stem} [{playlist_id[:8]}]{PLAYLIST_SUFFIX}"


def owned_playlist_id(path: Path) -> str | None:
    """The playlist id this file declares, or None if it is not ours."""
    try:
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            head = handle.read(_MARKER_SCAN_BYTES)
    except OSError:
        return None
    for line in head.splitlines():
        if line.startswith(OWNER_MARKER):
            return line[len(OWNER_MARKER):].strip() or None
    return None


def _entry_path(track_path: str, playlist_dir: Path) -> str | None:
    """Track path relative to the playlist file, or None on a different drive.

    Not substituted with an absolute path: that resolves only if Navidrome
    mounts the library identically.
    """
    try:
        relative = os.path.relpath(track_path, start=playlist_dir)
    except ValueError:
        return None
    return PurePath(relative).as_posix()


def render_playlist(
    name: str, playlist_id: str, entries: list[dict], playlist_dir: Path
) -> tuple[str, int, int]:
    """Render extended M3U. Returns (text, entries with no file, unrepresentable)."""
    lines = ["#EXTM3U", f"{OWNER_MARKER}{playlist_id}", f"#PLAYLIST:{_one_line(name)}"]
    missing = 0
    unrepresentable = 0
    for entry in entries:
        track_path = entry.get("file_path")
        if not track_path:
            missing += 1
            continue
        rendered = _entry_path(track_path, playlist_dir)
        if rendered is None or _CONTROL_CHARS.search(rendered):
            unrepresentable += 1
            continue
        duration = entry.get("duration")
        seconds = int(duration) if isinstance(duration, (int, float)) else -1
        artist = _one_line(entry.get("artist_name"))
        title = _one_line(entry.get("track_name"))
        label = f"{artist} - {title}" if artist else title
        lines.append(f"#EXTINF:{seconds},{label}")
        lines.append(rendered)
    return "\n".join(lines) + "\n", missing, unrepresentable


class NavidromePlaylistExportService:
    def __init__(self, store) -> None:  # noqa: ANN001 - NativeLibraryStore
        self._store = store
        # Sync Now and the periodic task must not interleave.
        self._lock = asyncio.Lock()

    async def sync(
        self,
        *,
        target_dir: str,
        scope: str = "public",
        remove_deleted: bool = True,
    ) -> PlaylistSyncResult:
        async with self._lock:
            return await self._sync_locked(
                target_dir=target_dir, scope=scope, remove_deleted=remove_deleted
            )

    async def _sync_locked(
        self, *, target_dir: str, scope: str, remove_deleted: bool
    ) -> PlaylistSyncResult:
        """Render every qualifying playlist and write the ones that changed."""
        result = PlaylistSyncResult()

        if not target_dir:
            result.message = "Set a playlist folder before syncing."
            return result

        directory = Path(target_dir).expanduser()
        if not directory.is_absolute():
            # A relative path resolves against the working directory.
            result.message = "The playlist folder must be a full path starting with /."
            return result

        try:
            directory.mkdir(parents=True, exist_ok=True)
        except OSError as error:
            result.message = f"Cannot create the playlist folder: {error}"
            return result
        if not os.access(directory, os.W_OK):
            result.message = (
                "DroppedNeedle cannot write to the playlist folder. Check it is "
                "mounted into this container and writable by it."
            )
            return result

        try:
            playlists = await self._store.list_target_playlists()
        except Exception as error:  # noqa: BLE001 - reported, not raised at the UI
            logger.exception("Playlist export could not read playlists")
            result.message = f"Could not read playlists: {error}"
            return result

        if scope == "public":
            playlists = [p for p in playlists if p.get("is_public")]

        written: dict[str, str] = {}
        failed: set[str] = set()

        for playlist in playlists:
            playlist_id = str(playlist.get("id") or "")
            if not playlist_id:
                continue
            name = str(playlist.get("name") or "Playlist")

            try:
                entries = await self._store.list_target_playlist_export_rows(
                    playlist_id
                )
            except Exception as error:  # noqa: BLE001 - one playlist cannot stop the rest
                logger.exception("Playlist export failed to read %s", playlist_id)
                result.errors.append(f"{name}: {error}")
                failed.add(playlist_id)
                continue

            text, missing, unrepresentable = render_playlist(
                name, playlist_id, entries, directory
            )
            result.tracks_missing_files += missing
            result.tracks_unrepresentable += unrepresentable
            if missing + unrepresentable == len(entries):
                # An empty playlist reads as data loss, not as a failed export.
                result.skipped_empty += 1
                failed.add(playlist_id)
                continue

            filename = safe_playlist_filename(name, playlist_id)
            destination = directory / filename
            if destination.exists() and owned_playlist_id(destination) != playlist_id:
                logger.warning(
                    "Playlist export skipped %s: the file is not DroppedNeedle's",
                    filename,
                )
                result.skipped_not_ours += 1
                failed.add(playlist_id)
                continue

            if self._matches_on_disk(destination, text):
                result.unchanged += 1
                written[playlist_id] = filename
                continue

            try:
                self._write_atomic(destination, text)
            except OSError as error:
                logger.warning(
                    "Playlist export could not write %s: %s", filename, error
                )
                result.errors.append(f"{name}: {error}")
                failed.add(playlist_id)
                continue
            written[playlist_id] = filename

        if remove_deleted:
            result.removed, result.removal_failures = self._remove_stale(
                directory, written, failed
            )

        result.written = len(written) - result.unchanged
        # Not a success while a removal is outstanding: the playlist is still
        # readable in Navidrome.
        result.success = not result.errors and not result.removal_failures
        result.message = self._summarise(result)
        return result

    @staticmethod
    def _matches_on_disk(path: Path, text: str) -> bool:
        """Whether the file already holds exactly this text.

        Skipping the write avoids churning mtimes and provoking a rescan.
        """
        try:
            return path.read_text(encoding="utf-8") == text
        except (OSError, UnicodeDecodeError):
            return False

    @staticmethod
    def _summarise(result: PlaylistSyncResult) -> str:
        parts = [f"{result.written} written"]
        if result.unchanged:
            parts.append(f"{result.unchanged} already current")
        if result.removed:
            parts.append(f"{result.removed} removed")
        if result.skipped_empty:
            parts.append(f"{result.skipped_empty} skipped, no tracks in your library")
        if result.skipped_not_ours:
            parts.append(
                f"{result.skipped_not_ours} skipped, a file of that name was not "
                "created by DroppedNeedle"
            )
        if result.tracks_missing_files:
            parts.append(f"{result.tracks_missing_files} tracks not in your library")
        if result.tracks_unrepresentable:
            parts.append(
                f"{result.tracks_unrepresentable} tracks on a different drive to the "
                "playlist folder"
            )
        if result.removal_failures:
            parts.append(
                f"{result.removal_failures} could not be removed, will retry next sync"
            )
        if result.errors:
            parts.append(f"{len(result.errors)} failed")
        return ", ".join(parts) + "."

    @staticmethod
    def _write_atomic(path: Path, text: str) -> None:
        """Write to a unique temporary file, then replace.

        Navidrome may scan mid-write. The random suffix keeps concurrent
        writers off each other's temporary file.
        """
        temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex[:8]}.tmp")
        try:
            temporary.write_text(text, encoding="utf-8", newline="\n")
            os.replace(temporary, path)
        except OSError:
            temporary.unlink(missing_ok=True)
            raise

    def _remove_stale(
        self, directory: Path, written: dict[str, str], failed: set[str]
    ) -> tuple[int, int]:
        """Delete our exports that this run no longer maintains.

        Candidates come from the directory, not a stored index: each file names
        its playlist, so a failed deletion simply retries next run.

        Returns the count removed and the count that could not be.
        """
        removed = 0
        failures = 0
        current_names = set(written.values())
        for candidate in sorted(directory.glob(f"*{PLAYLIST_SUFFIX}")):
            if candidate.name in current_names or not candidate.is_file():
                continue
            playlist_id = owned_playlist_id(candidate)
            if playlist_id is None:
                # Not ours: a hand-made playlist, or another tool's.
                continue
            if playlist_id in failed:
                # Its export failed this run, so it is not confirmed gone.
                continue
            try:
                candidate.unlink()
                removed += 1
            except OSError as error:
                logger.warning(
                    "Playlist export could not remove %s: %s", candidate.name, error
                )
                failures += 1
        return removed, failures
