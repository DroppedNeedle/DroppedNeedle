import asyncio
import logging
import time
from datetime import datetime, timezone
from typing import Any

from api.v1.schemas.scrobble import (
    NowPlayingRequest,
    ScrobbleRequest,
    ScrobbleResponse,
    ServiceResult,
)
from infrastructure.persistence.play_history_store import PlayHistoryStore
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore
from services.per_user_client_factory import PerUserClientFactory

logger = logging.getLogger(__name__)

DEDUP_TTL_SECONDS = 3600
DEDUP_MAX_ENTRIES = 200
MIN_TRACK_DURATION_MS = 30_000


class ScrobbleService:
    """Per-user scrobbling + local play_history source of truth.

    Every accepted play is recorded regardless of external linkage; external
    forwarding goes only to the user's own linked Last.fm / ListenBrainz accounts.
    """

    def __init__(
        self,
        client_factory: PerUserClientFactory,
        listening_prefs_store: UserListeningPrefsStore,
        play_history_store: PlayHistoryStore,
        plugin_host=None,  # noqa: ANN001 - PluginHost, optional (01b)
    ):
        self._client_factory = client_factory
        self._listening_prefs_store = listening_prefs_store
        self._play_history_store = play_history_store
        self._plugin_host = plugin_host
        self._dedup_cache: dict[str, float] = {}
        self._plugin_tasks: set[asyncio.Task] = set()

    def _dispatch_to_plugins(self, request: ScrobbleRequest) -> None:
        """Fan the accepted play out to scrobbler plugins, fire-and-forget so a
        slow plugin never adds latency to the client's scrobble call."""
        if self._plugin_host is None:
            return
        from infrastructure.plugins.protocols import ScrobbleEvent

        event = ScrobbleEvent(
            artist=request.artist_name,
            track=request.track_name,
            album=request.album_name,
            timestamp=request.timestamp,
            duration_ms=request.duration_ms or None,
            recording_mbid=request.mbid,
        )
        task = asyncio.create_task(self._plugin_host.dispatch_scrobble(event))
        self._plugin_tasks.add(task)
        task.add_done_callback(self._plugin_tasks.discard)

    def _dedup_key(self, user_id: str, artist: str, track: str, timestamp: int) -> str:
        # user_id-scoped so two users scrobbling the same track aren't cross-deduped
        return f"{user_id}::{artist.lower()}::{track.lower()}::{timestamp}"

    def _is_duplicate(self, key: str) -> bool:
        entry_time = self._dedup_cache.get(key)
        if entry_time is None:
            return False
        return (time.time() - entry_time) < DEDUP_TTL_SECONDS

    def _record_dedup(self, key: str) -> None:
        self._dedup_cache[key] = time.time()
        if len(self._dedup_cache) > DEDUP_MAX_ENTRIES:
            now = time.time()
            expired = [
                k for k, v in self._dedup_cache.items()
                if (now - v) >= DEDUP_TTL_SECONDS
            ]
            for k in expired:
                del self._dedup_cache[k]
            if len(self._dedup_cache) > DEDUP_MAX_ENTRIES:
                oldest = sorted(self._dedup_cache, key=self._dedup_cache.get)  # type: ignore[arg-type]
                for k in oldest[: len(self._dedup_cache) - DEDUP_MAX_ENTRIES]:
                    del self._dedup_cache[k]

    async def report_now_playing(
        self, request: NowPlayingRequest, *, user_id: str
    ) -> ScrobbleResponse:
        prefs = await self._listening_prefs_store.get(user_id)
        if request.source == "navidrome" and prefs.navidrome_handles_external_scrobbles:
            return ScrobbleResponse(accepted=True, services={})

        tasks: dict[str, Any] = {}
        duration_sec = request.duration_ms // 1000 if request.duration_ms > 0 else 0

        if prefs.scrobble_to_lastfm:
            lastfm = await self._client_factory.resolve_lastfm(user_id)
            if lastfm is not None:
                tasks["lastfm"] = lastfm.update_now_playing(
                    artist=request.artist_name,
                    track=request.track_name,
                    album=request.album_name,
                    duration=duration_sec,
                    mbid=request.mbid,
                )

        if prefs.scrobble_to_listenbrainz:
            listenbrainz = await self._client_factory.resolve_listenbrainz(user_id)
            if listenbrainz is not None:
                tasks["listenbrainz"] = listenbrainz.submit_now_playing(
                    artist_name=request.artist_name,
                    track_name=request.track_name,
                    release_name=request.album_name,
                    duration_ms=request.duration_ms,
                )

        if not tasks:
            return ScrobbleResponse(accepted=False, services={})

        services, any_success = await self._gather_results(tasks, "Now playing report failed")
        return ScrobbleResponse(accepted=any_success, services=services)

    async def submit_scrobble(
        self, request: ScrobbleRequest, *, user_id: str
    ) -> ScrobbleResponse:
        dedup = self._dedup_key(
            user_id, request.artist_name, request.track_name, request.timestamp
        )
        if self._is_duplicate(dedup):
            logger.debug(
                "Duplicate scrobble skipped for %s: %s - %s at %d",
                user_id,
                request.artist_name,
                request.track_name,
                request.timestamp,
            )
            return ScrobbleResponse(accepted=True, services={})

        # record the play locally for every user regardless of external linkage or the
        # short-track gate below; recorded once - retries are deduped above
        await self._record_play_history(request, user_id)
        self._record_dedup(dedup)

        # short-track gate blocks only external forwarding; history is already saved
        if 0 < request.duration_ms < MIN_TRACK_DURATION_MS:
            logger.debug(
                "Short track (%dms) recorded locally, not forwarded: %s - %s",
                request.duration_ms,
                request.artist_name,
                request.track_name,
            )
            return ScrobbleResponse(accepted=True, services={})

        prefs = await self._listening_prefs_store.get(user_id)
        if request.source == "navidrome" and prefs.navidrome_handles_external_scrobbles:
            logger.debug(
                "External scrobble forwarding delegated to Navidrome for user %s",
                user_id,
            )
            return ScrobbleResponse(accepted=True, services={})

        self._dispatch_to_plugins(request)

        tasks: dict[str, Any] = {}
        duration_sec = request.duration_ms // 1000 if request.duration_ms > 0 else 0

        if prefs.scrobble_to_lastfm:
            lastfm = await self._client_factory.resolve_lastfm(user_id)
            if lastfm is not None:
                tasks["lastfm"] = lastfm.scrobble(
                    artist=request.artist_name,
                    track=request.track_name,
                    timestamp=request.timestamp,
                    album=request.album_name,
                    duration=duration_sec,
                    mbid=request.mbid,
                )

        if prefs.scrobble_to_listenbrainz:
            listenbrainz = await self._client_factory.resolve_listenbrainz(user_id)
            if listenbrainz is not None:
                tasks["listenbrainz"] = listenbrainz.submit_single_listen(
                    artist_name=request.artist_name,
                    track_name=request.track_name,
                    listened_at=request.timestamp,
                    release_name=request.album_name,
                    duration_ms=request.duration_ms,
                )

        if not tasks:
            # no linked/enabled external account - the play is still recorded
            return ScrobbleResponse(accepted=True, services={})

        # play is already recorded locally, so accepted=True regardless of the external
        # outcome; per-service results are reported in `services`
        services, _ = await self._gather_results(tasks, "Scrobble submission failed")
        return ScrobbleResponse(accepted=True, services=services)

    async def _record_play_history(self, request: ScrobbleRequest, user_id: str) -> None:
        played_at = datetime.fromtimestamp(request.timestamp, tz=timezone.utc).isoformat()
        await self._play_history_store.insert(
            user_id,
            track_name=request.track_name,
            artist_name=request.artist_name,
            album_name=request.album_name or None,
            recording_mbid=request.mbid,
            release_group_mbid=request.release_group_mbid,
            duration_ms=request.duration_ms or None,
            source=request.source,
            played_at=played_at,
        )

    @staticmethod
    async def _gather_results(
        tasks: dict[str, Any], failure_msg: str
    ) -> tuple[dict[str, ServiceResult], bool]:
        results_list = await asyncio.gather(*tasks.values(), return_exceptions=True)
        services: dict[str, ServiceResult] = {}
        any_success = False

        for service_name, result in zip(tasks.keys(), results_list):
            if isinstance(result, BaseException):
                logger.warning("%s for %s: %s", failure_msg, service_name, result)
                services[service_name] = ServiceResult(success=False, error=str(result))
            else:
                services[service_name] = ServiceResult(success=True)
                any_success = True

        return services, any_success
