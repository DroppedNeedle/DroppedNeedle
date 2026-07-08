"""Read-only httpx client for the Lidarr import feature (LidarrImport).

Consumes exactly two Lidarr v1 GET endpoints - ``/system/status`` (the Test probe) and
``/artist`` (the whole import) - authenticated with the ``X-Api-Key`` header so the key
never lands in proxy logs. Verified against live Lidarr 3.1.3.4968 (see
``lidarr_import_API_NOTES.md``).

Actionable-failure repository: non-2xx and decode failures raise ``LidarrImportError``
(handler-mapped to 503); a rejected API key raises ``LidarrImportError(auth=True)`` so the
Test route can report it in the response body. Raw httpx/msgspec errors never escape.

Lidarr is a LAN service, so there is **no external rate limiter** (CLAUDE.md). Both methods
take the url/api_key as params so the Test route can validate *submitted* credentials before
they are saved; the import convenience path reads them from ``PreferencesService``.
"""

import logging

import httpx
import msgspec

from core.exceptions import LidarrImportError
from infrastructure.resilience.retry import CircuitBreaker, with_retry

from .lidarr_import_models import LidarrArtist, LidarrSystemStatus

logger = logging.getLogger(__name__)

_lidarr_import_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="lidarr_import",
)


class LidarrImportRepository:
    def __init__(self, http_client: httpx.AsyncClient) -> None:
        self._client = http_client

    async def get_system_status(self, url: str, api_key: str) -> LidarrSystemStatus:
        content = await self._fetch(url, api_key, "/system/status")
        return self._decode(content, LidarrSystemStatus)

    async def list_artists(self, url: str, api_key: str) -> list[LidarrArtist]:
        content = await self._fetch(url, api_key, "/artist")
        return self._decode(content, list[LidarrArtist])

    async def _fetch(self, url: str, api_key: str, path: str) -> bytes:
        # Convert a transport error that survived all retries into LidarrImportError so a
        # raw httpx error never escapes the repository (CLAUDE.md). httpx.InvalidURL is NOT
        # a subclass of httpx.HTTPError, so a malformed base URL (e.g. a bad-port typo) must
        # be caught explicitly - else it escapes as a raw 500 instead of the friendly Test
        # body / 503. LidarrImportError and CircuitOpenError (handler-mapped to 503) pass
        # through.
        try:
            return await self._get(url, api_key, path)
        except LidarrImportError:
            raise
        except (httpx.HTTPError, httpx.InvalidURL) as exc:
            raise LidarrImportError(f"Lidarr request failed: {type(exc).__name__}") from exc

    @with_retry(
        max_attempts=3,
        base_delay=0.5,
        max_delay=5.0,
        circuit_breaker=_lidarr_import_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError,),
    )
    async def _get(self, url: str, api_key: str, path: str) -> bytes:
        endpoint = f"{url.rstrip('/')}/api/v1{path}"
        response = await self._client.get(
            endpoint, headers={"X-Api-Key": api_key}, timeout=30.0
        )
        if response.status_code in (401, 403):
            raise LidarrImportError("Lidarr rejected the API key", auth=True)
        if response.status_code >= 400:
            raise LidarrImportError(f"Lidarr returned HTTP {response.status_code}")
        return response.content

    @staticmethod
    def _decode(content: bytes, decode_type: type):
        try:
            return msgspec.json.decode(content, type=decode_type)
        except msgspec.MsgspecError as exc:
            raise LidarrImportError(f"Lidarr response decode failed: {exc}") from exc
