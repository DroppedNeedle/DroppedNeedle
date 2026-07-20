"""Raw httpx wrapper around qBittorrent's Web API v2. No business logic (that's
``QbittorrentDownloadClient``).

Authentication uses qBittorrent 5.2+'s ``Authorization: Bearer`` API key.
Torrents are added by URL/magnet via ``torrents/add`` with a correlation ``tag``
(the pre-enqueue ``job_name``) because the add endpoint returns no hash - the
hash is recovered by listing ``torrents/info?tag=…`` (mirrors SABnzbd's
job_name correlation).

The httpx client is INJECTED (AUD-12). The API key is encrypted at rest and
never logged.
"""

import asyncio
import logging
from typing import Any

import httpx
import msgspec

from core.exceptions import ExternalServiceError

from .qbittorrent_models import QbtTorrentFile, QbtTorrentInfo

logger = logging.getLogger(__name__)


class QbittorrentApiError(ExternalServiceError):
    """Transport/HTTP/qBittorrent error. Mapped to HTTP 503 by the registered handler."""

    def __init__(self, message: str, details: Any = None, *, auth: bool = False) -> None:
        super().__init__(message, details)
        self.auth = auth


class QbittorrentClient:
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
        return f"{self._base_url}/api/v2/{path.lstrip('/')}"

    async def _request(
        self,
        method: str,
        path: str,
        *,
        timeout: float,
        retry: bool = True,
        **kwargs: Any,
    ) -> httpx.Response:
        """Send a Bearer-authenticated request; retry transient failures."""
        attempts = self._max_attempts if retry else 1
        last_exc: httpx.HTTPError | None = None
        resp: httpx.Response | None = None
        for attempt in range(attempts):
            if attempt:
                await asyncio.sleep(self._retry_backoff * (2 ** (attempt - 1)))
            try:
                resp = await self._http.request(
                    method, self._url(path), timeout=timeout,
                    headers={"Authorization": f"Bearer {self._api_key}"}, **kwargs
                )
            except httpx.HTTPError as exc:
                last_exc, resp = exc, None
                continue
            if resp.status_code in (401, 403):
                raise QbittorrentApiError("qBittorrent rejected the API key", auth=True)
            if resp.status_code < 500:
                return resp
        if resp is not None:
            return resp
        raise QbittorrentApiError(f"qBittorrent request failed: {last_exc}") from last_exc

    async def version(self, *, timeout: float = 15.0) -> str:
        resp = await self._request("GET", "app/version", timeout=timeout)
        if resp.status_code >= 400:
            raise QbittorrentApiError(
                f"qBittorrent returned HTTP {resp.status_code}", details=resp.text[:200]
            )
        return resp.text.strip()

    async def add_torrent(
        self,
        *,
        urls: str,
        category: str | None = None,
        tag: str | None = None,
        timeout: float = 60.0,
    ) -> None:
        """``POST torrents/add`` with a magnet and/or .torrent URL (newline-separated
        in ``urls``; qBittorrent fetches .torrent URLs itself, private-tracker cookies
        already embedded in Prowlarr's proxied download link). NOT retried (a re-send
        can race the first add); the endpoint returns no hash - correlate by ``tag``."""
        data: dict[str, str] = {"urls": urls}
        if category:
            data["category"] = category
        if tag:
            data["tags"] = tag
        resp = await self._request(
            "POST", "torrents/add", data=data, timeout=timeout, retry=False
        )
        if resp.status_code == 415:
            raise QbittorrentApiError("qBittorrent rejected the torrent (invalid)")
        if resp.text.strip().lower().startswith("fails"):
            raise QbittorrentApiError("qBittorrent rejected the torrent")
        if resp.status_code >= 400:
            raise QbittorrentApiError(
                f"qBittorrent add returned HTTP {resp.status_code}", details=resp.text[:200]
            )

    async def torrents_info(
        self,
        *,
        category: str | None = None,
        tag: str | None = None,
        hashes: str | None = None,
        timeout: float = 30.0,
    ) -> list[QbtTorrentInfo]:
        params: dict[str, str] = {}
        if category:
            params["category"] = category
        if tag:
            params["tag"] = tag
        if hashes:
            params["hashes"] = hashes
        resp = await self._request("GET", "torrents/info", params=params, timeout=timeout)
        if resp.status_code >= 400:
            raise QbittorrentApiError(
                f"qBittorrent info returned HTTP {resp.status_code}", details=resp.text[:200]
            )
        try:
            data = resp.json()
        except ValueError as exc:
            raise QbittorrentApiError("qBittorrent returned non-JSON") from exc
        if not isinstance(data, list):
            return []
        return msgspec.convert(data, type=list[QbtTorrentInfo], strict=False)

    async def torrent_files(
        self, torrent_hash: str, *, timeout: float = 30.0
    ) -> list[QbtTorrentFile]:
        resp = await self._request(
            "GET", "torrents/files", params={"hash": torrent_hash}, timeout=timeout
        )
        if resp.status_code == 404:
            return []
        if resp.status_code >= 400:
            raise QbittorrentApiError(
                f"qBittorrent files returned HTTP {resp.status_code}", details=resp.text[:200]
            )
        try:
            data = resp.json()
        except ValueError as exc:
            raise QbittorrentApiError("qBittorrent returned non-JSON") from exc
        if not isinstance(data, list):
            return []
        return msgspec.convert(data, type=list[QbtTorrentFile], strict=False)

    async def delete_torrents(
        self, hashes: str, *, delete_files: bool, timeout: float = 30.0
    ) -> bool:
        resp = await self._request(
            "POST",
            "torrents/delete",
            data={"hashes": hashes, "deleteFiles": "true" if delete_files else "false"},
            timeout=timeout,
            retry=False,
        )
        return resp.status_code < 400
