"""Compat scrobble adapter: protocol play event -> ScrobbleService.

Forwards to the existing ScrobbleService so compat plays land in ``play_history``
and forward to Last.fm/ListenBrainz like native plays. Stream/`/universal`
requests are never counted - only an explicit Subsonic ``scrobble`` or a Jellyfin
``Sessions/Playing/Stopped`` past threshold.
"""

from __future__ import annotations

import logging
import time
from typing import TYPE_CHECKING

from api.v1.schemas.scrobble import NowPlayingRequest, ScrobbleRequest
from core.exceptions import ResourceNotFoundError

if TYPE_CHECKING:
    from api.v1.schemas.scrobble import ScrobbleResponse
    from services.compat.library_view_service import LibraryViewService
    from services.compat.view_models import ViewTrack
    from services.now_playing_service import NowPlayingService
    from services.scrobble_service import ScrobbleService

logger = logging.getLogger(__name__)

_START_TTL_SECONDS = 2 * 60 * 60  # a stuck session can't leak its start time


def _norm_client(client: str | None) -> str | None:
    if not client:
        return None
    return client.strip().lower() or None


class CompatScrobbleAdapter:
    def __init__(
        self,
        scrobble_service: "ScrobbleService",
        library_view_service: "LibraryViewService",
        now_playing_service: "NowPlayingService | None" = None,
    ) -> None:
        self._scrobble = scrobble_service
        self._view = library_view_service
        self._presence = now_playing_service
        # (user_id, ItemId|PlaySessionId) -> started_at unix
        self._starts: dict[tuple[str, str], float] = {}

    async def now_playing(
        self, file_id: str, *, user_id: str, client: str | None, user_name: str = ""
    ) -> "ScrobbleResponse":
        track = await self._resolve(file_id)
        req = NowPlayingRequest(
            track_name=track.title,
            artist_name=track.artist_name,
            album_name=track.album_title,
            duration_ms=round(track.duration_seconds * 1000),
            mbid=track.recording_mbid,
            source=_norm_client(client),
            release_group_mbid=track.rg_mbid,
        )
        result = await self._scrobble.report_now_playing(req, user_id=user_id)
        await self._write_presence(
            track,
            user_id=user_id,
            user_name=user_name,
            source=_norm_client(client),
            is_paused=False,
            progress_ms=None,
        )
        return result

    async def progress(
        self,
        file_id: str,
        *,
        user_id: str,
        user_name: str = "",
        client: str | None,
        position_ms: int | None,
        is_paused: bool,
    ) -> None:
        """Heartbeat from a Jellyfin client's progress report; keeps presence alive
        and the scrubber live. No scrobble forwarding (that happens on stop)."""
        if self._presence is None:
            return
        track = await self._resolve(file_id)
        await self._write_presence(
            track,
            user_id=user_id,
            user_name=user_name,
            source=_norm_client(client),
            is_paused=is_paused,
            progress_ms=position_ms,
        )

    async def scrobble(
        self,
        file_id: str,
        *,
        user_id: str,
        client: str | None,
        played_at: float | None = None,
        user_name: str = "",
    ) -> "ScrobbleResponse":
        track = await self._resolve(file_id)
        ts = int(played_at if played_at is not None else time.time())
        req = ScrobbleRequest(
            track_name=track.title,
            artist_name=track.artist_name,
            timestamp=ts,
            album_name=track.album_title,
            duration_ms=round(track.duration_seconds * 1000),
            mbid=track.recording_mbid,
            source=_norm_client(client),
            release_group_mbid=track.rg_mbid,
        )
        result = await self._scrobble.submit_scrobble(req, user_id=user_id)
        # the track finished/stopped, so the session is no longer "now playing"
        await self._clear_presence(user_id, _norm_client(client))
        return result

    async def clear_presence(self, user_id: str, client: str | None) -> None:
        """Drop a client's now-playing presence (called on stop, any reason)."""
        await self._clear_presence(user_id, _norm_client(client))

    @staticmethod
    def _presence_key(user_id: str, source: str | None) -> str:
        return f"{user_id}:compat:{source or 'app'}"

    async def _write_presence(
        self,
        track: "ViewTrack",
        *,
        user_id: str,
        user_name: str,
        source: str | None,
        is_paused: bool,
        progress_ms: int | None,
    ) -> None:
        if self._presence is None:
            return
        try:
            await self._presence.update(
                key=self._presence_key(user_id, source),
                user_id=user_id,
                user_name=user_name,
                source=source or "app",
                device_name="",
                track_name=track.title,
                artist_name=track.artist_name,
                album_name=track.album_title or None,
                cover_url=(
                    f"/api/v1/covers/release-group/{track.rg_mbid}" if track.rg_mbid else ""
                ),
                is_paused=is_paused,
                progress_ms=progress_ms,
                duration_ms=round(track.duration_seconds * 1000) or None,
            )
        except Exception as e:  # noqa: BLE001 - presence must never fail a play report
            logger.debug("compat presence update failed: %s", e)

    async def _clear_presence(self, user_id: str, source: str | None) -> None:
        if self._presence is None:
            return
        try:
            await self._presence.remove(self._presence_key(user_id, source))
        except Exception as e:  # noqa: BLE001
            logger.debug("compat presence clear failed: %s", e)

    async def _resolve(self, file_id: str) -> "ViewTrack":
        track = await self._view.get_track(file_id)
        if track is None:
            raise ResourceNotFoundError(f"Track {file_id} not found")
        return track

    def mark_started(self, user_id: str, key: str) -> None:
        self._evict_stale()
        self._starts[(user_id, key)] = time.time()

    def pop_started(self, user_id: str, key: str) -> float | None:
        self._evict_stale()
        return self._starts.pop((user_id, key), None)

    def _evict_stale(self) -> None:
        cutoff = time.time() - _START_TTL_SECONDS
        for k in [k for k, v in self._starts.items() if v < cutoff]:
            self._starts.pop(k, None)
