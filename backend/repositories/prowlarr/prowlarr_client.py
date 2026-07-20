"""Raw httpx wrapper around Prowlarr's v1 JSON API. No business logic (that's
``ProwlarrIndexer``).

Auth is the ``X-Api-Key`` header on every call. Search hits ``/api/v1/search``
with ``query`` + repeated ``categories`` params; Prowlarr fans the query out
across every enabled indexer it manages (usenet AND torrent) and tags each
result with its ``protocol``, so DroppedNeedle needs exactly one connection
instead of one Newznab entry per indexer. Health uses ``/api/v1/system/status``
(``/ping`` answers without auth, so it can't validate the key).

The httpx client is INJECTED (AUD-12, mirrors ``SabnzbdClient``). Transient
transport errors + 5xx on idempotent GETs are retried with backoff.
"""

import asyncio
import logging
from urllib.parse import urljoin
from typing import Any

import httpx
import msgspec

from core.exceptions import ExternalServiceError

from .prowlarr_models import (
    ProwlarrIndexerInfo,
    ProwlarrSearchResult,
    ProwlarrSystemStatus,
)

logger = logging.getLogger(__name__)


class ProwlarrApiError(ExternalServiceError):
    """Transport/HTTP/Prowlarr error. Mapped to HTTP 503 by the registered handler."""

    def __init__(self, message: str, details: Any = None, *, auth: bool = False) -> None:
        super().__init__(message, details)
        self.auth = auth


class ProwlarrClient:
    def __init__(
        self,
        http: httpx.AsyncClient,
        base_url: str,
        api_key: str,
        *,
        max_attempts: int = 3,
        retry_backoff: float = 0.5,
    ) -> None:
        self._http = http
        self._base_url = base_url.rstrip("/")
        self._api_key = api_key
        self._max_attempts = max(1, max_attempts)
        self._retry_backoff = retry_backoff

    def _url(self, path: str) -> str:
        return f"{self._base_url}/api/v1/{path.lstrip('/')}"

    def _headers(self) -> dict[str, str]:
        return {"X-Api-Key": self._api_key}

    def absolute_url(self, value: str) -> str:
        """Resolve Prowlarr-relative result links against the configured origin."""
        return urljoin(f"{self._base_url}/", value)

    async def system_status(self, *, timeout: float = 15.0) -> ProwlarrSystemStatus:
        data = await self._get_json("system/status", timeout=timeout)
        return msgspec.convert(data, type=ProwlarrSystemStatus, strict=False)

    async def indexers(self, *, timeout: float = 15.0) -> list[ProwlarrIndexerInfo]:
        data = await self._get_json("indexer", timeout=timeout)
        if not isinstance(data, list):
            return []
        return msgspec.convert(data, type=list[ProwlarrIndexerInfo], strict=False)

    async def search(
        self,
        query: str,
        categories: list[int],
        *,
        limit: int = 100,
        timeout: float = 30.0,
    ) -> list[ProwlarrSearchResult]:
        """``GET /api/v1/search`` - Prowlarr's aggregate search across all its
        enabled indexers. ``type=search`` (free-text) is the reliable baseline:
        Prowlarr maps it onto each indexer's best-supported mode itself."""
        params: list[tuple[str, str]] = [
            ("query", query),
            ("type", "search"),
            ("limit", str(limit)),
        ]
        params.extend(("categories", str(c)) for c in categories)
        data = await self._get_json("search", params=params, timeout=timeout)
        if not isinstance(data, list):
            return []
        return msgspec.convert(data, type=list[ProwlarrSearchResult], strict=False)

    async def _get_json(
        self,
        path: str,
        *,
        params: list[tuple[str, str]] | None = None,
        timeout: float,
    ) -> Any:
        url = self._url(path)
        last_exc: httpx.HTTPError | None = None
        resp: httpx.Response | None = None
        for attempt in range(self._max_attempts):
            if attempt:
                await asyncio.sleep(self._retry_backoff * (2 ** (attempt - 1)))
            try:
                resp = await self._http.get(
                    url, params=params, headers=self._headers(), timeout=timeout
                )
            except httpx.HTTPError as exc:
                last_exc, resp = exc, None
                continue
            if resp.status_code < 500:
                break
        if resp is None:
            raise ProwlarrApiError(f"Prowlarr request failed: {last_exc}") from last_exc
        if resp.status_code in (401, 403):
            raise ProwlarrApiError("Prowlarr rejected the API key", auth=True)
        if resp.status_code >= 400:
            raise ProwlarrApiError(
                f"Prowlarr returned HTTP {resp.status_code}", details=resp.text[:200]
            )
        try:
            return resp.json()
        except ValueError as exc:
            raise ProwlarrApiError(
                f"Prowlarr returned non-JSON: {resp.text[:120]}"
            ) from exc
