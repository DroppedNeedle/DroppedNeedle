"""NowPlayingService - in-memory live presence registry.

Holds the current playing session for every source (the native web player, inbound
Subsonic/Jellyfin connected apps, and the polled upstream Jellyfin/Navidrome/Plex
servers) and broadcasts a privacy-projected snapshot over the SSE ``now-playing``
channel whenever it changes.

Single-process invariant: correct only under ``uvicorn --workers 1`` (the current
Dockerfile), like ``SSEPublisher``. Presence is transient and intentionally lost on
restart.

Privacy is enforced here, server-side, keyed on the **owner's** visibility setting,
so a hidden track is never serialized to other clients. A viewer always sees their
own track in full because the frontend overlays its own local player state - the
shared channel only ever carries other people's (projected) entries.
"""

import asyncio
import logging
import time

import msgspec

from api.v1.schemas.now_playing import NowPlayingSnapshotEntry
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore
from infrastructure.sse_publisher import SSEPublisher

logger = logging.getLogger(__name__)

CHANNEL = "now-playing"
SNAPSHOT_EVENT = "snapshot"
# drop a session that has not heartbeated within this window (covers browsers that
# closed without a stop, and Subsonic clients that never send progress)
ENTRY_TTL_SECONDS = 45.0

VISIBILITY_FULL = "full"
VISIBILITY_TRACK_HIDDEN = "track_hidden"
VISIBILITY_OFFLINE = "offline"
VALID_VISIBILITY = (VISIBILITY_FULL, VISIBILITY_TRACK_HIDDEN, VISIBILITY_OFFLINE)


class _Entry(msgspec.Struct):
    key: str
    user_id: str | None
    user_name: str
    source: str
    device_name: str
    track_name: str
    artist_name: str
    album_name: str | None
    cover_url: str
    is_paused: bool
    progress_ms: int | None
    duration_ms: int | None
    updated_at: float
    # library file id when the reporter knows it (compat clients); lets the
    # Subsonic getNowPlaying endpoint serve a real Child for the session
    track_file_id: str | None = None


class ExternalSession(msgspec.Struct):
    """One mapped session from a polled upstream server (user_id is always None)."""

    key: str
    user_name: str
    device_name: str
    track_name: str
    artist_name: str
    album_name: str | None
    cover_url: str
    is_paused: bool
    progress_ms: int | None
    duration_ms: int | None


class NowPlayingService:
    def __init__(
        self,
        sse: SSEPublisher,
        prefs_store: UserListeningPrefsStore,
        ttl_seconds: float = ENTRY_TTL_SECONDS,
    ):
        self._sse = sse
        self._prefs = prefs_store
        self._ttl = ttl_seconds
        self._entries: dict[str, _Entry] = {}
        self._visibility: dict[str, str] = {}
        self._lock = asyncio.Lock()

    def subscribe(self):
        """Async iterator of SSE messages for the now-playing channel (snapshot-then-deltas)."""
        return self._sse.subscribe(CHANNEL)

    async def update(
        self,
        *,
        key: str,
        user_id: str | None,
        user_name: str,
        source: str,
        device_name: str,
        track_name: str,
        artist_name: str,
        album_name: str | None,
        cover_url: str,
        is_paused: bool,
        progress_ms: int | None,
        duration_ms: int | None,
        track_file_id: str | None = None,
    ) -> None:
        """Upsert a DroppedNeedle-origin session (native player or inbound compat)."""
        if user_id is not None and user_id not in self._visibility:
            await self._load_visibility(user_id)
        async with self._lock:
            self._entries[key] = _Entry(
                key=key,
                user_id=user_id,
                user_name=user_name,
                source=source,
                device_name=device_name,
                track_name=track_name,
                artist_name=artist_name,
                album_name=album_name,
                cover_url=cover_url,
                is_paused=is_paused,
                progress_ms=progress_ms,
                duration_ms=duration_ms,
                updated_at=time.time(),
                track_file_id=track_file_id,
            )
        await self._publish()

    async def remove(self, key: str) -> None:
        async with self._lock:
            existed = self._entries.pop(key, None) is not None
        if existed:
            await self._publish()

    async def reconcile_source(self, source: str, sessions: list[ExternalSession]) -> None:
        """Replace all entries for a polled upstream `source` with the fresh poll result.

        No-op (no publish) when the source had no entries and still has none, so an idle
        integration doesn't churn the channel every poll cycle.
        """
        now = time.time()
        async with self._lock:
            had_any = any(
                e.source == source and e.user_id is None for e in self._entries.values()
            )
            if not sessions and not had_any:
                return
            self._entries = {
                k: e
                for k, e in self._entries.items()
                if not (e.source == source and e.user_id is None)
            }
            for s in sessions:
                self._entries[s.key] = _Entry(
                    key=s.key,
                    user_id=None,
                    user_name=s.user_name,
                    source=source,
                    device_name=s.device_name,
                    track_name=s.track_name,
                    artist_name=s.artist_name,
                    album_name=s.album_name,
                    cover_url=s.cover_url,
                    is_paused=s.is_paused,
                    progress_ms=s.progress_ms,
                    duration_ms=s.duration_ms,
                    updated_at=now,
                )
        await self._publish()

    async def sweep(self) -> None:
        """Drop sessions that stopped heartbeating; publish only if any were removed."""
        cutoff = time.time() - self._ttl
        async with self._lock:
            stale = [k for k, e in self._entries.items() if e.updated_at < cutoff]
            for k in stale:
                del self._entries[k]
        if stale:
            await self._publish()

    async def set_visibility(self, user_id: str, visibility: str) -> None:
        """Apply a user's privacy choice and re-broadcast so it takes effect live."""
        normalized = visibility if visibility in VALID_VISIBILITY else VISIBILITY_FULL
        async with self._lock:
            self._visibility[user_id] = normalized
        await self._publish()

    def snapshot(self) -> list[NowPlayingSnapshotEntry]:
        """Privacy-projected current sessions, for the GET hydrate endpoint."""
        return [p for e in self._entries.values() if (p := self._project(e)) is not None]

    def compat_now_playing(self) -> list[tuple[NowPlayingSnapshotEntry, str, float]]:
        """(projection, track_file_id, updated_at) for sessions Subsonic getNowPlaying
        can serve: a library file id is known and the owner's visibility is full -
        a redacted projection carries no track, so it is skipped, not leaked."""
        out: list[tuple[NowPlayingSnapshotEntry, str, float]] = []
        for e in self._entries.values():
            if e.track_file_id is None:
                continue
            p = self._project(e)
            if p is None or p.redacted:
                continue
            out.append((p, e.track_file_id, e.updated_at))
        return out

    def _project(self, entry: _Entry) -> NowPlayingSnapshotEntry | None:
        if entry.user_id is None:
            # upstream-server sessions aren't DroppedNeedle accounts: no privacy setting
            visibility = VISIBILITY_FULL
        else:
            # fail closed: if we couldn't load this user's setting (e.g. a transient
            # prefs-DB error left them uncached), redact rather than risk leaking a
            # hidden track. A normal user's "full" default is loaded successfully and
            # cached, so this only bites the genuine error case.
            visibility = self._visibility.get(entry.user_id, VISIBILITY_TRACK_HIDDEN)
        if visibility == VISIBILITY_OFFLINE:
            return None
        redacted = visibility == VISIBILITY_TRACK_HIDDEN
        return NowPlayingSnapshotEntry(
            id=entry.key,
            user_name=entry.user_name,
            track_name="" if redacted else entry.track_name,
            artist_name="" if redacted else entry.artist_name,
            album_name=None if redacted else entry.album_name,
            cover_url="" if redacted else entry.cover_url,
            device_name=entry.device_name,
            is_paused=entry.is_paused,
            source=entry.source,
            progress_ms=entry.progress_ms,
            duration_ms=entry.duration_ms,
            redacted=redacted,
        )

    async def _publish(self) -> None:
        async with self._lock:
            sessions = [
                msgspec.to_builtins(p)
                for e in self._entries.values()
                if (p := self._project(e)) is not None
            ]
        await self._sse.publish(CHANNEL, SNAPSHOT_EVENT, {"sessions": sessions})

    async def _load_visibility(self, user_id: str) -> None:
        try:
            prefs = await self._prefs.get(user_id)
        except Exception as e:  # noqa: BLE001 - presence must never fail a play report
            # don't cache a fail-open default on a transient DB error: leaving the user
            # uncached makes the next report retry rather than pinning a 'track_hidden'
            # user to 'full' until restart
            logger.debug("now-playing visibility load failed for %s: %s", user_id, e)
            return
        async with self._lock:
            self._visibility[user_id] = prefs.now_playing_visibility
