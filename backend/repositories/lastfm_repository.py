import hashlib
import logging
from typing import Any

import httpx
import msgspec

from core.exceptions import (
    ConfigurationError,
    ExternalServiceError,
    ResourceNotFoundError,
    TokenNotAuthorizedError,
)
from infrastructure.cache.cache_keys import LFM_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from infrastructure.resilience.retry import CircuitBreaker, with_retry
from repositories.lastfm_models import (
    ALLOWED_LASTFM_PERIOD,
    LastFmAlbum,
    LastFmAlbumInfo,
    LastFmArtist,
    LastFmArtistInfo,
    LastFmLovedTrack,
    LastFmRecentTrack,
    LastFmSession,
    LastFmSimilarArtist,
    LastFmToken,
    LastFmTrack,
    parse_album_info,
    parse_artist_info,
    parse_loved_track,
    parse_recent_track,
    parse_session,
    parse_similar_artist,
    parse_token,
    parse_top_album,
    parse_top_artist,
    parse_top_track,
    parse_weekly_album_chart_item,
)
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from infrastructure.service_health import report_breaker_health

logger = logging.getLogger(__name__)

_SOURCE = "lastfm"


def _record_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx is not None:
        ctx.record(IntegrationResult.error(source=_SOURCE, msg=msg))

LASTFM_API_URL = "https://ws.audioscrobbler.com/2.0/"

_lastfm_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="lastfm",
    on_state_change=report_breaker_health(
        "lastfm",
        "music data",
        message="Last.fm is temporarily unavailable - some listening stats and "
        "recommendations may be missing.",
    ),
)

_lastfm_rate_limiter = TokenBucketRateLimiter(rate=5.0, capacity=10)

LASTFM_ERROR_MAP: dict[int, tuple[type[Exception], str]] = {
    2: (ExternalServiceError, "Invalid service - This service does not exist"),
    3: (ExternalServiceError, "Invalid method - No method with that name in this package"),
    4: (ConfigurationError, "Authentication failed - invalid API key or shared secret"),
    6: (ResourceNotFoundError, "Not found"),
    9: (ConfigurationError, "Session key expired - please re-authorize with Last.fm"),
    10: (ConfigurationError, "Invalid API key - check your Last.fm API key"),
    11: (ExternalServiceError, "Last.fm service is temporarily offline"),
    14: (TokenNotAuthorizedError, "Token not yet authorized"),
    17: (ConfigurationError, "Authentication required - re-authorize Last.fm or make your listening history public"),
    26: (ConfigurationError, "API key has been suspended - contact Last.fm support"),
    29: (ExternalServiceError, "Rate limit exceeded"),
}

LASTFM_USER_CACHE_TTL = 300
LASTFM_ENTITY_CACHE_TTL = 3600
LASTFM_GLOBAL_CACHE_TTL = 3600

LastFmJsonObject = dict[str, Any]
LastFmJsonArray = list[LastFmJsonObject]
LastFmJson = LastFmJsonObject | LastFmJsonArray


def _decode_json_response(response: httpx.Response) -> LastFmJson:
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray, memoryview)):
        return msgspec.json.decode(content, type=LastFmJson)
    return response.json()


class LastFmRepository:
    def __init__(
        self,
        http_client: httpx.AsyncClient,
        cache: CacheInterface,
        api_key: str = "",
        shared_secret: str = "",
        session_key: str = "",
        base_url: str = LASTFM_API_URL,
    ):
        self._client = http_client
        self._cache = cache
        self._api_key = api_key
        self._shared_secret = shared_secret
        self._session_key = session_key
        # admin-configurable API root (libre.fm, Maloja, other audioscrobbler-
        # compatible endpoints); official ws.audioscrobbler.com default
        self._base_url = base_url or LASTFM_API_URL

    @property
    def _can_sign(self) -> bool:
        return bool(self._shared_secret) and bool(self._session_key)

    def configure(self, api_key: str, shared_secret: str, session_key: str = "") -> None:
        self._api_key = api_key
        self._shared_secret = shared_secret
        self._session_key = session_key

    @staticmethod
    def reset_circuit_breaker() -> None:
        _lastfm_circuit_breaker.reset()

    def _build_api_sig(self, params: dict[str, str]) -> str:
        filtered = {k: v for k, v in sorted(params.items()) if k not in ("format", "callback")}
        sig_string = "".join(f"{k}{v}" for k, v in filtered.items())
        sig_string += self._shared_secret
        return hashlib.md5(sig_string.encode("utf-8")).hexdigest()

    def _handle_error_response(self, data: dict[str, Any]) -> None:
        error_code = data.get("error")
        error_message = data.get("message", "Unknown Last.fm error")

        if error_code is None:
            return

        mapped = LASTFM_ERROR_MAP.get(error_code)
        if mapped:
            exc_type, default_msg = mapped
            raise exc_type(f"{default_msg}: {error_message}")

        logger.warning("Last.fm error code=%d message=%s", error_code, error_message)
        raise ExternalServiceError(f"Last.fm error ({error_code}): {error_message}")

    @with_retry(
        max_attempts=3,
        base_delay=1.0,
        max_delay=3.0,
        circuit_breaker=_lastfm_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
    )
    async def _request(
        self,
        method: str,
        params: dict[str, str] | None = None,
        signed: bool = False,
        http_method: str = "GET",
    ) -> dict[str, Any]:
        if not self._api_key:
            raise ConfigurationError("Last.fm API key is not configured")

        await _lastfm_rate_limiter.acquire()

        request_params: dict[str, str] = {
            "method": method,
            "api_key": self._api_key,
            "format": "json",
        }
        if params:
            request_params.update(params)

        if signed:
            if not self._shared_secret:
                raise ConfigurationError("Last.fm shared secret is required for signed requests")
            if self._session_key and "sk" not in request_params:
                request_params["sk"] = self._session_key
            request_params["api_sig"] = self._build_api_sig(request_params)

        try:
            if http_method == "POST":
                response = await self._client.post(
                    self._base_url,
                    data=request_params,
                    timeout=15.0,
                )
            else:
                response = await self._client.get(
                    self._base_url,
                    params=request_params,
                    timeout=15.0,
                )

            if response.status_code != 200:
                raise ExternalServiceError(
                    f"Last.fm request failed ({response.status_code})",
                    response.text,
                )

            try:
                data = _decode_json_response(response)
            except (msgspec.DecodeError, ValueError, TypeError):
                raise ExternalServiceError("Last.fm returned invalid JSON")

            self._handle_error_response(data)
            _lastfm_circuit_breaker.record_success()
            return data

        except (ConfigurationError, ExternalServiceError, ResourceNotFoundError):
            raise
        except httpx.HTTPError as e:
            raise ExternalServiceError(f"Last.fm request failed: {e}")

    async def get_token(self) -> LastFmToken:
        data = await self._request("auth.getToken", signed=True, http_method="GET")
        return parse_token(data)

    async def get_session(self, token: str) -> LastFmSession:
        data = await self._request(
            "auth.getSession",
            params={"token": token},
            signed=True,
            http_method="GET",
        )
        return parse_session(data)

    async def validate_api_key(self) -> tuple[bool, str]:
        try:
            await self._request(
                "chart.getTopArtists",
                params={"limit": "1"},
                http_method="GET",
            )
            return True, "API key is valid"
        except ConfigurationError as e:
            return False, str(e.message)
        except ExternalServiceError as e:
            return False, f"Connection failed: {e.message}"

    async def validate_session(self) -> tuple[bool, str]:
        if not self._session_key:
            return False, "No session key configured"
        try:
            data = await self._request(
                "user.getInfo",
                signed=True,
                http_method="GET",
            )
            user = data.get("user", {})
            username = user.get("name", "")
            return True, f"Connected as {username}"
        except ConfigurationError as e:
            return False, str(e.message)
        except ExternalServiceError as e:
            return False, f"Session validation failed: {e.message}"

    async def update_now_playing(
        self,
        artist: str,
        track: str,
        album: str = "",
        duration: int = 0,
        mbid: str | None = None,
    ) -> bool:
        params: dict[str, str] = {"artist": artist, "track": track}
        if album:
            params["album"] = album
        if duration > 0:
            params["duration"] = str(duration)
        if mbid:
            params["mbid"] = mbid
        await self._request(
            "track.updateNowPlaying",
            params=params,
            signed=True,
            http_method="POST",
        )
        return True

    async def scrobble(
        self,
        artist: str,
        track: str,
        timestamp: int,
        album: str = "",
        duration: int = 0,
        mbid: str | None = None,
    ) -> bool:
        params: dict[str, str] = {
            "artist": artist,
            "track": track,
            "timestamp": str(timestamp),
        }
        if album:
            params["album"] = album
        if duration > 0:
            params["duration"] = str(duration)
        if mbid:
            params["mbid"] = mbid
        await self._request(
            "track.scrobble",
            params=params,
            signed=True,
            http_method="POST",
        )
        return True


    async def get_user_top_artists(
        self, username: str, period: str = "overall", limit: int = 50
    ) -> list[LastFmArtist]:
        if period not in ALLOWED_LASTFM_PERIOD:
            period = "overall"
        cache_key = f"{LFM_PREFIX}user_top_artists:{username}:{period}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "user.getTopArtists",
            params={"user": username, "period": period, "limit": str(limit)},
            signed=self._can_sign,
        )
        artists = [
            parse_top_artist(item)
            for item in data.get("topartists", {}).get("artist", [])
        ]
        await self._cache.set(cache_key, artists, ttl_seconds=LASTFM_USER_CACHE_TTL)
        return artists

    async def get_user_top_albums(
        self, username: str, period: str = "overall", limit: int = 50
    ) -> list[LastFmAlbum]:
        if period not in ALLOWED_LASTFM_PERIOD:
            period = "overall"
        cache_key = f"{LFM_PREFIX}user_top_albums:{username}:{period}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "user.getTopAlbums",
            params={"user": username, "period": period, "limit": str(limit)},
            signed=self._can_sign,
        )
        albums = [
            parse_top_album(item)
            for item in data.get("topalbums", {}).get("album", [])
        ]
        await self._cache.set(cache_key, albums, ttl_seconds=LASTFM_USER_CACHE_TTL)
        return albums

    async def get_user_top_tracks(
        self, username: str, period: str = "overall", limit: int = 50
    ) -> list[LastFmTrack]:
        if period not in ALLOWED_LASTFM_PERIOD:
            period = "overall"
        cache_key = f"{LFM_PREFIX}user_top_tracks:{username}:{period}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "user.getTopTracks",
            params={"user": username, "period": period, "limit": str(limit)},
            signed=self._can_sign,
        )
        tracks = [
            parse_top_track(item)
            for item in data.get("toptracks", {}).get("track", [])
        ]
        await self._cache.set(cache_key, tracks, ttl_seconds=LASTFM_USER_CACHE_TTL)
        return tracks

    async def get_user_recent_tracks(
        self, username: str, limit: int = 50
    ) -> list[LastFmRecentTrack]:
        cache_key = f"{LFM_PREFIX}user_recent:{username}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "user.getRecentTracks",
            params={"user": username, "limit": str(limit), "extended": "0"},
            signed=self._can_sign,
        )
        tracks = [
            parse_recent_track(item)
            for item in data.get("recenttracks", {}).get("track", [])
        ]
        await self._cache.set(cache_key, tracks, ttl_seconds=LASTFM_USER_CACHE_TTL)
        return tracks

    async def get_user_loved_tracks(
        self, username: str, limit: int = 50
    ) -> list[LastFmLovedTrack]:
        cache_key = f"{LFM_PREFIX}user_loved_tracks:{username}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "user.getLovedTracks",
            params={"user": username, "limit": str(limit)},
            signed=self._can_sign,
        )
        tracks = [
            parse_loved_track(item)
            for item in data.get("lovedtracks", {}).get("track", [])
        ]
        await self._cache.set(cache_key, tracks, ttl_seconds=LASTFM_USER_CACHE_TTL)
        return tracks

    async def get_user_weekly_artist_chart(
        self, username: str
    ) -> list[LastFmArtist]:
        cache_key = f"{LFM_PREFIX}user_weekly_artists:{username}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "user.getWeeklyArtistChart",
            params={"user": username},
            signed=self._can_sign,
        )
        artists = [
            parse_top_artist(item)
            for item in data.get("weeklyartistchart", {}).get("artist", [])
        ]
        await self._cache.set(cache_key, artists, ttl_seconds=LASTFM_USER_CACHE_TTL)
        return artists

    async def get_user_weekly_album_chart(
        self, username: str
    ) -> list[LastFmAlbum]:
        cache_key = f"{LFM_PREFIX}user_weekly_albums:{username}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "user.getWeeklyAlbumChart",
            params={"user": username},
            signed=self._can_sign,
        )
        albums = [
            parse_weekly_album_chart_item(item)
            for item in data.get("weeklyalbumchart", {}).get("album", [])
        ]
        await self._cache.set(cache_key, albums, ttl_seconds=LASTFM_USER_CACHE_TTL)
        return albums


    async def get_artist_top_tracks(
        self, artist: str, mbid: str | None = None, limit: int = 10
    ) -> list[LastFmTrack]:
        lookup = mbid or artist
        cache_key = f"{LFM_PREFIX}artist_top_tracks:{lookup}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, str] = {"limit": str(limit)}
        if mbid:
            params["mbid"] = mbid
        else:
            params["artist"] = artist
        data = await self._request("artist.getTopTracks", params=params)
        tracks = [
            parse_top_track(item)
            for item in data.get("toptracks", {}).get("track", [])
        ]
        await self._cache.set(cache_key, tracks, ttl_seconds=LASTFM_ENTITY_CACHE_TTL)
        return tracks

    async def get_artist_top_albums(
        self, artist: str, mbid: str | None = None, limit: int = 10
    ) -> list[LastFmAlbum]:
        lookup = mbid or artist
        cache_key = f"{LFM_PREFIX}artist_top_albums:{lookup}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, str] = {"limit": str(limit)}
        if mbid:
            params["mbid"] = mbid
        else:
            params["artist"] = artist
        data = await self._request("artist.getTopAlbums", params=params)
        albums = [
            parse_top_album(item)
            for item in data.get("topalbums", {}).get("album", [])
        ]
        await self._cache.set(cache_key, albums, ttl_seconds=LASTFM_ENTITY_CACHE_TTL)
        return albums

    async def get_artist_info(
        self, artist: str, mbid: str | None = None, username: str | None = None
    ) -> LastFmArtistInfo | None:
        lookup = mbid or artist
        cache_key = f"{LFM_PREFIX}artist_info:{lookup}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, str] = {}
        if mbid:
            params["mbid"] = mbid
        else:
            params["artist"] = artist
        if username:
            params["username"] = username
        try:
            data = await self._request("artist.getInfo", params=params)
        except ResourceNotFoundError:
            return None
        info = parse_artist_info(data)
        await self._cache.set(cache_key, info, ttl_seconds=LASTFM_ENTITY_CACHE_TTL)
        return info

    async def get_album_info(
        self,
        artist: str,
        album: str,
        mbid: str | None = None,
        username: str | None = None,
    ) -> LastFmAlbumInfo | None:
        lookup = mbid or f"{artist}:{album}"
        cache_key = f"{LFM_PREFIX}album_info:{lookup}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, str] = {}
        if mbid:
            params["mbid"] = mbid
        else:
            params["artist"] = artist
            params["album"] = album
        if username:
            params["username"] = username
        try:
            data = await self._request("album.getInfo", params=params)
        except ResourceNotFoundError:
            return None
        info = parse_album_info(data)
        await self._cache.set(cache_key, info, ttl_seconds=LASTFM_ENTITY_CACHE_TTL)
        return info

    async def get_similar_artists(
        self, artist: str, mbid: str | None = None, limit: int = 30
    ) -> list[LastFmSimilarArtist]:
        lookup = mbid or artist
        cache_key = f"{LFM_PREFIX}similar_artists:{lookup}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, str] = {"limit": str(limit)}
        if mbid:
            params["mbid"] = mbid
        else:
            params["artist"] = artist
        data = await self._request("artist.getSimilar", params=params)
        similar = [
            parse_similar_artist(item)
            for item in data.get("similarartists", {}).get("artist", [])
        ]
        await self._cache.set(cache_key, similar, ttl_seconds=LASTFM_ENTITY_CACHE_TTL)
        return similar


    async def get_global_top_artists(self, limit: int = 50) -> list[LastFmArtist]:
        cache_key = f"{LFM_PREFIX}global_top_artists:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "chart.getTopArtists",
            params={"limit": str(limit)},
        )
        artists = [
            parse_top_artist(item)
            for item in data.get("artists", {}).get("artist", [])
        ]
        await self._cache.set(cache_key, artists, ttl_seconds=LASTFM_GLOBAL_CACHE_TTL)
        return artists

    async def get_global_top_tracks(self, limit: int = 50) -> list[LastFmTrack]:
        cache_key = f"{LFM_PREFIX}global_top_tracks:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "chart.getTopTracks",
            params={"limit": str(limit)},
        )
        tracks = [
            parse_top_track(item)
            for item in data.get("toptracks", {}).get("track", [])
        ]
        await self._cache.set(cache_key, tracks, ttl_seconds=LASTFM_GLOBAL_CACHE_TTL)
        return tracks

    async def get_tag_top_artists(
        self, tag: str, limit: int = 50
    ) -> list[LastFmArtist]:
        cache_key = f"{LFM_PREFIX}tag_top_artists:{tag}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        data = await self._request(
            "tag.getTopArtists",
            params={"tag": tag, "limit": str(limit)},
        )
        artists = [
            parse_top_artist(item)
            for item in data.get("topartists", {}).get("artist", [])
        ]
        await self._cache.set(cache_key, artists, ttl_seconds=LASTFM_GLOBAL_CACHE_TTL)
        return artists
