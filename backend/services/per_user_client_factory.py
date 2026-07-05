"""Resolve request-scoped ListenBrainz / Last.fm clients for a user from their
encrypted ``user_connections``.

Factory is a singleton; the clients it returns are per-request. Last.fm builds from
the global app api_key/shared_secret (one registered app) plus the user's per-user
session_key. Username is exposed separately via ``resolve_lastfm_username`` because
the Last.fm repo holds no username field (reads take it per-method-call).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from core.config import Settings
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.http.client import get_http_client, get_listenbrainz_http_client
from infrastructure.persistence.user_connections_store import UserConnectionsStore
from services.preferences_service import PreferencesService

if TYPE_CHECKING:
    from repositories.lastfm_repository import LastFmRepository
    from repositories.listenbrainz_repository import ListenBrainzRepository

_LISTENBRAINZ = "listenbrainz"
_LASTFM = "lastfm"
_SPOTIFY = "spotify"


class PerUserClientFactory:
    def __init__(
        self,
        connections_store: UserConnectionsStore,
        preferences_service: PreferencesService,
        cache: CacheInterface,
        settings: Settings,
    ):
        self._connections_store = connections_store
        self._preferences_service = preferences_service
        self._cache = cache
        self._settings = settings

    def _http_timeouts(self) -> tuple[float, float, int]:
        advanced = self._preferences_service.get_advanced_settings()
        return (
            float(advanced.http_timeout),
            float(advanced.http_connect_timeout),
            advanced.http_max_connections,
        )

    async def resolve_listenbrainz(self, user_id: str) -> "ListenBrainzRepository | None":
        data = await self._connections_store.get(user_id, _LISTENBRAINZ)
        if not data:
            return None
        user_token = data.get("user_token", "")
        if not user_token:
            return None

        from repositories.listenbrainz_repository import ListenBrainzRepository

        timeout, connect_timeout, _ = self._http_timeouts()
        http_client = get_listenbrainz_http_client(
            settings=self._settings, timeout=timeout, connect_timeout=connect_timeout
        )
        return ListenBrainzRepository(
            http_client=http_client,
            cache=self._cache,
            username=data.get("username", ""),
            user_token=user_token,
        )

    async def resolve_lastfm(self, user_id: str) -> "LastFmRepository | None":
        data = await self._connections_store.get(user_id, _LASTFM)
        if not data:
            return None
        session_key = data.get("session_key", "")
        if not session_key:
            return None
        lf = self._preferences_service.get_lastfm_connection()
        if not (lf.api_key and lf.shared_secret):
            return None

        from repositories.lastfm_repository import LastFmRepository

        timeout, connect_timeout, max_connections = self._http_timeouts()
        http_client = get_http_client(
            self._settings,
            timeout=timeout,
            connect_timeout=connect_timeout,
            max_connections=max_connections,
        )
        return LastFmRepository(
            http_client=http_client,
            cache=self._cache,
            api_key=lf.api_key,
            shared_secret=lf.shared_secret,
            session_key=session_key,
        )

    async def resolve_lastfm_username(self, user_id: str) -> str | None:
        data = await self._connections_store.get(user_id, _LASTFM)
        if not data:
            return None
        return data.get("username") or None

    async def resolve_listenbrainz_username(self, user_id: str) -> str | None:
        data = await self._connections_store.get(user_id, _LISTENBRAINZ)
        if not data:
            return None
        return data.get("username") or None

    async def is_listenbrainz_linked(self, user_id: str) -> bool:
        """Lightweight enable check (no client build) mirroring resolve_listenbrainz."""
        data = await self._connections_store.get(user_id, _LISTENBRAINZ)
        return bool(data and data.get("user_token"))

    async def is_lastfm_linked(self, user_id: str) -> bool:
        """Lightweight enable check (no client build) mirroring resolve_lastfm."""
        data = await self._connections_store.get(user_id, _LASTFM)
        if not (data and data.get("session_key")):
            return False
        lf = self._preferences_service.get_lastfm_connection()
        return bool(lf.api_key and lf.shared_secret)

    async def resolve_spotify(self, user_id: str) -> "SpotifyClient | None":
        data = await self._connections_store.get(user_id, _SPOTIFY)
        if not data:
            return None
        access_token = data.get("access_token", "")
        refresh_token = data.get("refresh_token", "")
        if not access_token and not refresh_token:
            return None
        settings = self._preferences_service.get_spotify_settings_raw()
        if not settings.client_id or not settings.client_secret:
            return None

        from services.spotify_client import SpotifyClient

        return SpotifyClient(
            client_id=settings.client_id,
            client_secret=settings.client_secret,
            access_token=access_token,
            refresh_token=refresh_token,
            expires_at=data.get("expires_at", ""),
            user_id=user_id,
            connections_store=self._connections_store,
            spotify_user_id=data.get("spotify_user_id", ""),
        )

    async def is_spotify_linked(self, user_id: str) -> bool:
        data = await self._connections_store.get(user_id, _SPOTIFY)
        if not data:
            return False
        settings = self._preferences_service.get_spotify_settings_raw()
        return bool(
            settings.enabled
            and settings.client_id
            and settings.client_secret
            and (data.get("access_token") or data.get("refresh_token"))
        )
