"""On-demand local artwork reads; no provider calls, cache writes or catalog edits."""

from __future__ import annotations

import asyncio
import hashlib
import os
from pathlib import Path, PurePosixPath
import stat
from typing import Callable

from infrastructure.audio.tagger import AudioTagger
from services.home.cached_local_artwork_service import _content_type

_MAX_IMAGE_BYTES = 25 * 1024 * 1024
_MAX_AUDIO_BYTES = 1024 * 1024 * 1024
_SIDECARS = tuple(
    stem + extension
    for stem in ("cover", "folder", "front", "album", "artwork", "Cover", "Folder", "Front", "Album", "Artwork")
    for extension in (".jpg", ".jpeg", ".png", ".webp")
)


def _open_regular(root: Path, relative: PurePosixPath, maximum: int):
    """Open beneath the approved root without following any symlink component."""
    if not root.is_absolute() or relative.is_absolute() or ".." in relative.parts:
        raise ValueError("Unsafe local artwork path")
    parts = (*root.parts[1:], *relative.parts)
    descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for index, part in enumerate(parts):
            flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
            if index < len(parts) - 1:
                flags |= os.O_DIRECTORY
            following = os.open(part, flags, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = following
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= maximum:
            raise ValueError("Unsupported local artwork source")
        result = os.fdopen(descriptor, "rb")
        descriptor = -1
        return result
    finally:
        if descriptor >= 0:
            os.close(descriptor)


class LocalAlbumArtworkService:
    def __init__(self, store) -> None:
        self._store = store
        self._tagger = AudioTagger()

    async def get(
        self, album_id: str, roots: dict[str, Path],
        admitted_path: Callable[[Path], bool] = lambda _: True,
    ):
        sources = await self._store.get_local_artwork_sources(album_id)
        return await asyncio.to_thread(self._read, sources, roots, admitted_path)

    def _read(self, sources, roots, admitted_path):
        admitted = []
        for source in sources[:65]:
            root = roots.get(source["root_id"])
            relative = PurePosixPath(source["relative_path"])
            if root is None or relative.is_absolute() or ".." in relative.parts:
                continue
            if root / relative != Path(source["file_path"]):
                continue
            if not admitted_path(root / relative):
                continue
            try:
                # A stale indexed path must not authorize reading its sidecars.
                with _open_regular(root, relative, _MAX_AUDIO_BYTES):
                    pass
            except (OSError, ValueError):
                continue
            admitted.append((root, relative))
        # Fixed names only, no directory walks. Explicit sidecars precede embedded art.
        seen = set()
        for root, relative in admitted:
            directory = (root, relative.parent)
            if directory in seen:
                continue
            seen.add(directory)
            for name in _SIDECARS:
                try:
                    with _open_regular(root, relative.parent / name, _MAX_IMAGE_BYTES) as stream:
                        content = stream.read(_MAX_IMAGE_BYTES + 1)
                except (OSError, ValueError):
                    continue
                result = self._result(content, "folder")
                if result is not None:
                    return result
        for root, relative in admitted:
            try:
                with _open_regular(root, relative, _MAX_AUDIO_BYTES) as stream:
                    content = self._tagger.read_cover_art(stream)
            except (OSError, ValueError):
                continue
            result = self._result(content, "embedded")
            if result is not None:
                return result
        return None

    @staticmethod
    def _result(content, source):
        if not content or len(content) > _MAX_IMAGE_BYTES:
            return None
        mime = _content_type(content)
        if mime is None:
            return None
        return content, mime, source, hashlib.sha1(content).hexdigest()
