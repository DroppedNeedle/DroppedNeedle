"""Metadata-only lyrics lookup (LRCLIB) for tracks without a local file.

Powers ``GET /api/v1/lyrics/lookup`` - the Plex-playback lyrics path, where the
player only knows artist/track/album strings and there is no library file to
extract from or write a sidecar next to.

Gated on the same ``lyrics_fetch_enabled`` library setting as the local
service's online fetch (off by default; the route answers 404 when off, which
the frontend already treats as "no lyrics").

Results - hits AND misses - are cached in-process for ~24h keyed by the
normalized (artist, track, album) signature, so seek-driven refetches from the
player never re-hit LRCLIB.
"""

import logging
import re
import time
from collections import OrderedDict
from typing import TYPE_CHECKING

from api.v1.schemas.library import LibraryLyricsResponse
from services.local_lyrics_service import _build_response

if TYPE_CHECKING:
    from repositories.lrclib_repository import LrcLibRepository
    from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

_WS_RE = re.compile(r"\s+")


def _normalize(value: str | None) -> str:
    return _WS_RE.sub(" ", (value or "").strip()).casefold()


class LyricsLookupService:
    _TTL_SECONDS = 24 * 60 * 60  # one LRCLIB ask per track signature per ~24h
    _CACHE_MAX = 2048

    def __init__(
        self,
        preferences_service: "PreferencesService",
        lrclib_repository: "LrcLibRepository",
    ) -> None:
        self._preferences = preferences_service
        self._lrclib = lrclib_repository
        # signature -> (expires_at, response-or-None); misses are cached too
        self._cache: OrderedDict[
            tuple[str, str, str], tuple[float, LibraryLyricsResponse | None]
        ] = OrderedDict()

    def enabled(self) -> bool:
        """Mirror of the local service's gate: the admin's opt-in LRCLIB fetch."""
        try:
            return bool(self._preferences.get_library_settings().lyrics_fetch_enabled)
        except Exception:  # noqa: BLE001 - unreadable settings must not break lyrics
            return False

    async def lookup(
        self,
        *,
        artist: str,
        track: str,
        album: str | None = None,
        duration_seconds: float | None = None,
    ) -> LibraryLyricsResponse | None:
        """Lyrics for a track signature, or None on an LRCLIB miss.

        Duration is a match-narrowing hint only - it is deliberately NOT part
        of the cache key, so a repeat request with/without it still hits."""
        key = (_normalize(artist), _normalize(track), _normalize(album))
        now = time.time()
        cached = self._cache.get(key)
        if cached is not None:
            expires_at, response = cached
            if now < expires_at:
                self._cache.move_to_end(key)
                return response
            self._cache.pop(key, None)

        result = await self._lrclib.fetch_lyrics(
            artist_name=artist,
            track_name=track,
            album_name=album,
            duration_seconds=duration_seconds,
        )
        response = _build_response(result[0]) if result else None
        self._cache[key] = (now + self._TTL_SECONDS, response)
        if len(self._cache) > self._CACHE_MAX:
            self._cache.popitem(last=False)
        return response
