"""LRCLIB lyrics client (https://lrclib.net) - the optional online lyrics fetch.

``GET /api/get`` looks up lyrics by the track's signature (artist + title +
album + duration). Lyrics are optional enrichment, never user-blocking: every
failure mode (network error, non-200, undecodable body, instrumental track)
surfaces as ``None`` so the lyrics route falls through to its normal 404.

LRCLIB is free and keyless but asks integrators to send a descriptive
User-Agent identifying the application.
"""

import logging

import httpx
import msgspec

logger = logging.getLogger(__name__)

LRCLIB_API_URL = "https://lrclib.net/api/get"
LRCLIB_USER_AGENT = "DroppedNeedle/2.x (https://github.com/HabiRabbu/DroppedNeedle)"
LRCLIB_TIMEOUT_SECONDS = 5.0


class LrcLibLyrics(msgspec.Struct):
    """Decoded subset of the /api/get response (field names are LRCLIB's)."""

    syncedLyrics: str | None = None  # noqa: N815 - LRCLIB JSON field name
    plainLyrics: str | None = None  # noqa: N815 - LRCLIB JSON field name
    instrumental: bool = False


class LrcLibRepository:
    def __init__(self, http_client: httpx.AsyncClient) -> None:
        self._client = http_client

    async def fetch_lyrics(
        self,
        *,
        artist_name: str | None,
        track_name: str | None,
        album_name: str | None = None,
        duration_seconds: float | None = None,
    ) -> tuple[str, bool] | None:
        """``(lyrics_text, is_synced)`` for the track, or ``None`` when LRCLIB
        has nothing (or the request failed). Synced lyrics are preferred; the
        returned text is LRC content for synced hits, plain lines otherwise."""
        if not artist_name or not track_name:
            return None  # /api/get cannot match without the core signature
        params: dict[str, str] = {
            "artist_name": artist_name,
            "track_name": track_name,
        }
        if album_name:
            params["album_name"] = album_name
        if duration_seconds:
            params["duration"] = str(int(round(duration_seconds)))
        try:
            response = await self._client.get(
                LRCLIB_API_URL,
                params=params,
                headers={"User-Agent": LRCLIB_USER_AGENT},
                timeout=LRCLIB_TIMEOUT_SECONDS,
            )
        except httpx.HTTPError as exc:
            logger.debug("LRCLIB request failed: %s", type(exc).__name__)
            return None
        if response.status_code == 404:
            return None  # LRCLIB's documented "no such track" answer
        if response.status_code != 200:
            logger.debug("LRCLIB returned HTTP %s", response.status_code)
            return None
        try:
            data = msgspec.json.decode(response.content, type=LrcLibLyrics)
        except msgspec.MsgspecError as exc:
            logger.debug("LRCLIB response decode failed: %s", exc)
            return None
        if data.instrumental:
            return None
        if data.syncedLyrics and data.syncedLyrics.strip():
            return data.syncedLyrics, True
        if data.plainLyrics and data.plainLyrics.strip():
            return data.plainLyrics, False
        return None
