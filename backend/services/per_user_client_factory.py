"""Resolve request-scoped ListenBrainz / Last.fm / media-server clients for a
user from their encrypted ``user_connections``.

Factory is a singleton; the clients it returns are per-request. Last.fm builds from
the global app api_key/shared_secret (one registered app) plus the user's per-user
session_key. Username is exposed separately via ``resolve_lastfm_username`` because
the Last.fm repo holds no username field (reads take it per-method-call).

Media servers (Navidrome/Jellyfin/Plex): the server URL and enabled flag stay
admin-owned in preferences; only the credential is per-user. Resolvers return a
fresh repo configured with the admin URL + the user's own credential, used solely
for playback attribution calls (scrobble/now-playing/session reporting) - never
for library reads, so the shared cache never mixes per-user data.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from core.config import Settings
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.http.client import get_http_client, get_listenbrainz_http_client
from infrastructure.persistence.user_connections_store import UserConnectionsStore
from services.preferences_service import PreferencesService

if TYPE_CHECKING:
    from repositories.jellyfin_repository import JellyfinRepository
    from repositories.lastfm_repository import LastFmRepository
    from repositories.listenbrainz_repository import ListenBrainzRepository
    from repositories.navidrome_repository import NavidromeRepository
    from repositories.plex_repository import PlexRepository

_LISTENBRAINZ = "listenbrainz"
_LASTFM = "lastfm"
_SPOTIFY = "spotify"
_NAVIDROME = "navidrome"
_JELLYFIN = "jellyfin"
_PLEX = "plex"


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
            base_url=self._preferences_service.get_listenbrainz_connection().api_url,
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
            base_url=lf.api_url,
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

    def _media_http_client(self):
        timeout, connect_timeout, max_connections = self._http_timeouts()
        return get_http_client(
            self._settings,
            timeout=timeout,
            connect_timeout=connect_timeout,
            max_connections=max_connections,
        )

    async def resolve_navidrome(self, user_id: str) -> "NavidromeRepository | None":
        nd = self._preferences_service.get_navidrome_connection_raw()
        if not (nd.enabled and nd.navidrome_url):
            return None
        data = await self._connections_store.get(user_id, _NAVIDROME)
        if not data:
            return None
        username = data.get("username", "")
        password = data.get("password", "")
        if not (username and password):
            return None

        from repositories.navidrome_repository import NavidromeRepository

        repo = NavidromeRepository(http_client=self._media_http_client(), cache=self._cache)
        repo.configure(url=nd.navidrome_url, username=username, password=password)
        return repo

    async def resolve_jellyfin(self, user_id: str) -> "JellyfinRepository | None":
        jf = self._preferences_service.get_jellyfin_connection()
        if not (jf.enabled and jf.jellyfin_url):
            return None
        data = await self._connections_store.get(user_id, _JELLYFIN)
        if not data:
            return None
        access_token = data.get("access_token", "")
        jellyfin_user_id = data.get("jellyfin_user_id", "")
        if not (access_token and jellyfin_user_id):
            return None

        from repositories.jellyfin_repository import JellyfinRepository

        # user access tokens ride the same `Authorization: MediaBrowser Token="…"`
        # header as server API keys; #151 verified the API-key case on 10.11.11,
        # the user-token case still needs a live check (PerUserPlayback DECISIONS-LIVE)
        return JellyfinRepository(
            http_client=self._media_http_client(),
            cache=self._cache,
            base_url=jf.jellyfin_url,
            api_key=access_token,
            user_id=jellyfin_user_id,
        )

    async def resolve_plex(self, user_id: str) -> "PlexRepository | None":
        plex = self._preferences_service.get_plex_connection_raw()
        if not (plex.enabled and plex.plex_url):
            return None
        data = await self._connections_store.get(user_id, _PLEX)
        if not data:
            return None
        auth_token = data.get("auth_token", "")
        if not auth_token:
            return None

        from repositories.plex_repository import PlexRepository

        repo = PlexRepository(http_client=self._media_http_client(), cache=self._cache)
        repo.configure(
            url=plex.plex_url,
            token=auth_token,
            client_id=self._preferences_service.get_setting("plex_client_id") or "",
        )
        return repo

    async def validate_navidrome_credentials(self, username: str, password: str) -> tuple[bool, str]:
        """Live-check a user's own Navidrome credentials against the admin-configured
        server before persisting them (link flow). Returns ``(ok, message)``."""
        nd = self._preferences_service.get_navidrome_connection_raw()
        if not (nd.enabled and nd.navidrome_url):
            return False, "Navidrome is not configured by the administrator"

        from repositories.navidrome_repository import NavidromeRepository

        repo = NavidromeRepository(http_client=self._media_http_client(), cache=self._cache)
        repo.configure(url=nd.navidrome_url, username=username, password=password)
        return await repo.validate_connection()

    async def is_navidrome_linked(self, user_id: str) -> bool:
        """Lightweight enable check (no client build) mirroring resolve_navidrome."""
        nd = self._preferences_service.get_navidrome_connection_raw()
        if not (nd.enabled and nd.navidrome_url):
            return False
        data = await self._connections_store.get(user_id, _NAVIDROME)
        return bool(data and data.get("username") and data.get("password"))

    async def is_jellyfin_linked(self, user_id: str) -> bool:
        """Lightweight enable check (no client build) mirroring resolve_jellyfin."""
        jf = self._preferences_service.get_jellyfin_connection()
        if not (jf.enabled and jf.jellyfin_url):
            return False
        data = await self._connections_store.get(user_id, _JELLYFIN)
        return bool(data and data.get("access_token") and data.get("jellyfin_user_id"))

    async def is_plex_linked(self, user_id: str) -> bool:
        """Lightweight enable check (no client build) mirroring resolve_plex."""
        plex = self._preferences_service.get_plex_connection_raw()
        if not (plex.enabled and plex.plex_url):
            return False
        data = await self._connections_store.get(user_id, _PLEX)
        return bool(data and data.get("auth_token"))
