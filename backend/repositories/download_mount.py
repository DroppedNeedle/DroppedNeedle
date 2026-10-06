"""Remapping a Usenet client's completed-downloads path onto DroppedNeedle's mount.

Both Usenet clients report finished jobs in *their own* filesystem namespace
(SABnzbd's ``storage``, NZBGet's ``FinalDir``/``DestDir``), which is only the same
string as DroppedNeedle's path when both containers happen to mount the volume
identically. This module turns one into the other and confines every result to the
mount, so a client-reported path can never walk the importer out of the volume.

Shared by ``repositories/sabnzbd`` and ``repositories/nzbget``: the logic is about
the mount, not about either client's API. Each client supplies its own
``complete_dir`` resolver, since SABnzbd reads ``misc.complete_dir`` from
``get_config`` and NZBGet reads ``DestDir`` from ``config``.

Sync filesystem work is kept in plain methods so callers can hand them to
``asyncio.to_thread`` rather than blocking the event loop.
"""

import asyncio
import logging
from collections.abc import Awaitable, Callable
from pathlib import Path, PurePosixPath

logger = logging.getLogger(__name__)

# Mirror of library_manager._AUDIO_SUFFIXES (the importer's accepted set). Kept local
# so this repository doesn't import from services/native (layering).
AUDIO_SUFFIXES = {".flac", ".mp3", ".m4a", ".m4b", ".mp4", ".ogg", ".oga", ".opus", ".wav"}

# Bound for the failure-path-only suffix walk below (same order as the slskd walk
# budget; a correct mount never walks, so this only prices misconfig).
_REMAP_WALK_BUDGET = 10_000
_ENUMERATE_BUDGET = 10_000
_HAS_FILE_BUDGET = 5_000


def posix_norm(value: str) -> str:
    """Fold client-reported separators to posix before any remap. A Windows-native
    client reports backslash paths, which ``PurePosixPath`` would otherwise treat as
    one opaque segment (the same folding the slskd path handling applies)."""
    return value.replace("\\", "/")


class DownloadsMount:
    """DroppedNeedle's view of a download client's completed-downloads directory.

    ``complete_dir`` is resolved lazily through the supplied coroutine because it is
    a remote call; the client caches it, not this class.
    """

    def __init__(
        self,
        root: Path,
        complete_dir: Callable[[], Awaitable[str]],
        *,
        client_name: str,
    ) -> None:
        self._root = Path(root)
        self._complete_dir = complete_dir
        self._client_name = client_name

    @property
    def root(self) -> Path:
        return self._root

    async def healthy(self) -> bool:
        """Whether the downloads MOUNT itself is usable. False ONLY when the configured
        mount root is missing or unreadable (a real environment fault - failing over or
        blocklisting can't fix it). A healthy mount whose per-job folder is merely
        empty/absent is a RELEASE problem (garbage / incomplete NZB), NOT a mount fault,
        so this returns True and lets the orchestrator blocklist the bad release."""

        def _ok() -> bool:
            try:
                if not self._root.is_dir():
                    return False
                next(self._root.iterdir(), None)  # force a readdir; raises if unreadable
                return True
            except OSError:
                return False

        return await asyncio.to_thread(_ok)

    async def local_storage(self, storage: str) -> Path:
        """Remap a client-namespace job folder onto the mount by stripping the client's
        completed-downloads prefix; fall back to the job-folder basename when the prefix
        doesn't match.

        Two deterministic breakages are repaired before the fallback (#245): a
        Windows-native client reports backslash paths (normalised first), and a
        ``downloads_mount`` pointed at a category subfolder resolves via a bounded suffix
        walk of the mount. A remap that still fails logs a WARNING with the client path,
        the completed-downloads dir, and the mount - user config paths, never secrets."""
        complete_dir = await self._complete_dir()
        remote = PurePosixPath(posix_norm(storage))
        if complete_dir:
            complete = PurePosixPath(posix_norm(complete_dir))
            try:
                rel = remote.relative_to(complete)
            except ValueError:
                rel = None
            if rel is not None:
                if not rel.parts:
                    return self._root
                direct = self._root / Path(*rel.parts)
                try:
                    if direct.is_dir():
                        return direct
                except OSError:
                    pass
                walked = await asyncio.to_thread(self._suffix_walk, rel)
                if walked is not None:
                    return walked
            logger.warning(
                "%s storage remap failed: client path %s (complete dir %s) does not "
                "resolve under mount %s; falling back to basename %s",
                self._client_name,
                storage,
                complete_dir,
                str(self._root),
                remote.name,
            )
        return self._root / remote.name

    async def exact_local_storage(self, storage: str) -> Path | None:
        """Map cleanup evidence only when the client's complete root is known exactly."""
        complete_dir = await self._complete_dir()
        if not complete_dir:
            return None
        try:
            relative = PurePosixPath(posix_norm(storage)).relative_to(
                PurePosixPath(posix_norm(complete_dir))
            )
        except ValueError:
            return None
        return self._root / Path(*relative.parts)

    def _suffix_walk(self, rel: PurePosixPath) -> Path | None:
        """Bounded walk for a directory under the mount whose path ends with the
        storage-relative suffix (the category-subfolder mount: the mount IS
        ``complete_dir/<cat>``, so the stripped remainder still carries the category
        component). Longest suffix wins; failure-path only. Sync filesystem I/O - the
        caller offloads it off the event loop."""
        try:
            mount = self._root.resolve()
        except OSError:
            return None
        suffixes = ["/".join(rel.parts[i:]) for i in range(len(rel.parts))]
        best: Path | None = None
        best_len = -1
        seen_dirs: set[Path] = set()
        stack = [mount]
        seen = 0
        while stack:
            try:
                current = stack.pop().resolve()
            except OSError:
                continue
            if not current.is_relative_to(mount) or current in seen_dirs:
                continue
            seen_dirs.add(current)
            dir_rel = current.relative_to(mount).as_posix()
            for suffix in suffixes:
                if dir_rel == suffix or dir_rel.endswith("/" + suffix):
                    if len(suffix) > best_len:
                        best, best_len = current, len(suffix)
                    break
            try:
                entries = list(current.iterdir())
            except OSError:
                continue
            for entry in entries:
                seen += 1
                if seen > _REMAP_WALK_BUDGET:
                    return best
                try:
                    if entry.is_dir():
                        stack.append(entry)
                except OSError:
                    continue
        return best

    def enumerate_audio(self, folder: Path) -> list[Path]:
        """Audio files under the finished job folder (bounded DFS), confined to the
        mount. Sync filesystem I/O - the caller offloads it off the event loop."""
        try:
            mount = self._root.resolve()
            root = folder.resolve()
        except OSError:
            return []
        if not root.is_relative_to(mount) or not root.is_dir():
            return []
        out: list[Path] = []
        stack = [root]
        seen = 0
        while stack:
            try:
                entries = list(stack.pop().iterdir())
            except OSError:
                continue
            for entry in entries:
                seen += 1
                if seen > _ENUMERATE_BUDGET:
                    return out
                if entry.is_dir():
                    stack.append(entry)
                elif entry.is_file() and entry.suffix.lower() in AUDIO_SUFFIXES:
                    out.append(entry)
        return out

    def has_any_file(self) -> bool:
        """Whether the mount contains any file at all. Sync filesystem I/O."""
        try:
            stack = [self._root]
            seen = 0
            while stack:
                for entry in stack.pop().iterdir():
                    seen += 1
                    if seen > _HAS_FILE_BUDGET:
                        return True
                    if entry.is_file():
                        return True
                    if entry.is_dir():
                        stack.append(entry)
        except OSError:
            return False
        return False


def dir_has_file(folder: Path) -> bool:
    try:
        return folder.is_dir() and any(p.is_file() for p in folder.iterdir())
    except OSError:
        return False
