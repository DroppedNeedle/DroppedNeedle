import logging
import time
from typing import Any

import httpx
import msgspec

from core.exceptions import ExternalServiceError, RateLimitedError
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from infrastructure.resilience.retry import CircuitBreaker, CircuitOpenError, with_retry
from repositories.audiodb_models import (
    AudioDBAlbumResponse,
    AudioDBArtistResponse,
)
from services.preferences_service import PreferencesService
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from infrastructure.service_health import report_breaker_health

logger = logging.getLogger(__name__)

_SOURCE = "audiodb"


def _record_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx is not None:
        ctx.record(IntegrationResult.error(source=_SOURCE, msg=msg))

AUDIODB_API_URL = "https://www.theaudiodb.com/api/v1/json"


def _log_circuit_state_change(
    breaker: CircuitBreaker,
    previous_state,
    new_state,
    reason: str,
) -> None:
    level = logging.INFO if new_state.value == "closed" else logging.WARNING
    logger.log(
        level,
        "audiodb.circuit_state_change service=%s previous_state=%s state=%s reason=%s",
        breaker.name,
        previous_state.value,
        new_state.value,
        reason,
    )

_audiodb_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="audiodb",
    on_state_change=report_breaker_health(
        "audiodb",
        "artist info",
        message="Extra artist artwork (TheAudioDB) is temporarily unavailable.",
        also=_log_circuit_state_change,
    ),
)

AUDIODB_FREE_KEY = "123"

AudioDBJson = dict[str, Any]


def _make_rate_limiter(premium: bool = False) -> TokenBucketRateLimiter:
    if premium:
        return TokenBucketRateLimiter(rate=5.0, capacity=10)
    return TokenBucketRateLimiter(rate=0.5, capacity=2)


def _decode_json_response(response: httpx.Response) -> AudioDBJson:
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray, memoryview)):
        return msgspec.json.decode(content, type=AudioDBJson)
    return response.json()


def _extract_first(data: dict[str, Any], key: str) -> dict[str, Any] | None:
    items = data.get(key)
    if not items or not isinstance(items, list) or len(items) == 0:
        return None
    return items[0]


class AudioDBRepository:
    def __init__(
        self,
        http_client: httpx.AsyncClient,
        preferences_service: PreferencesService,
        api_key: str = "123",
        premium: bool = False,
    ):
        self._client = http_client
        self._preferences_service = preferences_service
        self._api_key = api_key
        self._rate_limiter = _make_rate_limiter(premium)

    def _is_enabled(self) -> bool:
        return self._preferences_service.get_advanced_settings().audiodb_enabled

    def _effective_api_key(self) -> str:
        settings_key = self._preferences_service.get_advanced_settings().audiodb_api_key
        if settings_key and settings_key.strip():
            return settings_key
        return self._api_key

    @staticmethod
    def reset_circuit_breaker() -> None:
        _audiodb_circuit_breaker.reset()

    @with_retry(
        max_attempts=3,
        base_delay=2.0,
        max_delay=10.0,
        circuit_breaker=_audiodb_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError, RateLimitedError),
    )
    async def _request(self, endpoint: str, params: dict[str, str] | None = None) -> dict[str, Any] | None:
        await self._rate_limiter.acquire()

        url = f"{AUDIODB_API_URL}/{self._effective_api_key()}/{endpoint}"

        try:
            t0 = time.monotonic()
            response = await self._client.get(url, params=params, timeout=15.0)
            elapsed_ms = (time.monotonic() - t0) * 1000

            if response.status_code == 429:
                logger.warning("audiodb.ratelimit status=429 elapsed_ms=%.1f retry_after_s=60", elapsed_ms)
                raise RateLimitedError("AudioDB rate limit exceeded", retry_after_seconds=60)

            if response.status_code == 404:
                return None

            if response.status_code != 200:
                raise ExternalServiceError(
                    f"AudioDB request failed ({response.status_code})"
                )

            try:
                data = _decode_json_response(response)
            except (msgspec.DecodeError, ValueError, TypeError):
                raise ExternalServiceError("AudioDB returned invalid JSON")

            return data

        except (ExternalServiceError, RateLimitedError):
            raise
        except httpx.HTTPError as e:
            raise ExternalServiceError(f"AudioDB request failed: {e}")

    async def get_artist_by_mbid(self, mbid: str) -> AudioDBArtistResponse | None:
        if not self._is_enabled() or not mbid:
            return None

        try:
            return await self._get_artist_by_mbid(mbid)
        except CircuitOpenError:
            logger.warning("audiodb.circuit_open entity=artist lookup_type=mbid mbid=%s", mbid)
            _record_degradation(f"Circuit open: artist lookup by mbid {mbid}")
            return None

    async def _get_artist_by_mbid(self, mbid: str) -> AudioDBArtistResponse | None:
        data = await self._request("artist-mb.php", params={"i": mbid})

        if data is None:
            return None

        item = _extract_first(data, "artists")
        if item is None:
            return None

        try:
            result = msgspec.convert(item, type=AudioDBArtistResponse)
        except (msgspec.ValidationError, msgspec.DecodeError, TypeError, KeyError) as exc:
            logger.warning("audiodb.schema_error entity=artist lookup_type=mbid mbid=%s error=%s", mbid, exc)
            _record_degradation(f"Schema error for artist mbid {mbid}: {exc}")
            return None
        return result

    async def get_album_by_mbid(self, mbid: str) -> AudioDBAlbumResponse | None:
        if not self._is_enabled() or not mbid:
            return None

        try:
            return await self._get_album_by_mbid(mbid)
        except CircuitOpenError:
            logger.warning("audiodb.circuit_open entity=album lookup_type=mbid mbid=%s", mbid)
            _record_degradation(f"Circuit open: album lookup by mbid {mbid}")
            return None

    async def _get_album_by_mbid(self, mbid: str) -> AudioDBAlbumResponse | None:
        data = await self._request("album-mb.php", params={"i": mbid})

        if data is None:
            return None

        item = _extract_first(data, "album")
        if item is None:
            return None

        try:
            result = msgspec.convert(item, type=AudioDBAlbumResponse)
        except (msgspec.ValidationError, msgspec.DecodeError, TypeError, KeyError) as exc:
            logger.warning("audiodb.schema_error entity=album lookup_type=mbid mbid=%s error=%s", mbid, exc)
            _record_degradation(f"Schema error for album mbid {mbid}: {exc}")
            return None
        return result

    async def search_artist_by_name(self, name: str) -> AudioDBArtistResponse | None:
        if not self._is_enabled() or not name:
            return None

        try:
            return await self._search_artist_by_name(name)
        except CircuitOpenError:
            logger.warning("audiodb.circuit_open entity=artist lookup_type=name name=%s", name)
            _record_degradation("Circuit open: artist search by name")
            return None

    async def _search_artist_by_name(self, name: str) -> AudioDBArtistResponse | None:
        data = await self._request("search.php", params={"s": name})

        if data is None:
            return None

        item = _extract_first(data, "artists")
        if item is None:
            return None

        try:
            result = msgspec.convert(item, type=AudioDBArtistResponse)
        except (msgspec.ValidationError, msgspec.DecodeError, TypeError, KeyError) as exc:
            logger.warning("audiodb.schema_error entity=artist lookup_type=name name=%s error=%s", name, exc)
            _record_degradation(f"Schema error for artist name search: {exc}")
            return None
        return result

    async def search_album_by_name(self, artist: str, album: str) -> AudioDBAlbumResponse | None:
        if not self._is_enabled() or not artist or not album:
            return None

        try:
            return await self._search_album_by_name(artist, album)
        except CircuitOpenError:
            logger.warning("audiodb.circuit_open entity=album lookup_type=name artist=%s album=%s", artist, album)
            _record_degradation("Circuit open: album search by name")
            return None

    async def _search_album_by_name(self, artist: str, album: str) -> AudioDBAlbumResponse | None:
        data = await self._request("searchalbum.php", params={"s": artist, "a": album})

        if data is None:
            return None

        item = _extract_first(data, "album")
        if item is None:
            return None

        try:
            result = msgspec.convert(item, type=AudioDBAlbumResponse)
        except (msgspec.ValidationError, msgspec.DecodeError, TypeError, KeyError) as exc:
            logger.warning(
                "audiodb.schema_error entity=album lookup_type=name artist=%s album=%s error=%s",
                artist,
                album,
                exc,
            )
            _record_degradation(f"Schema error for album name search: {exc}")
            return None
        return result
