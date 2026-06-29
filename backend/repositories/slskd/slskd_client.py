"""Raw httpx wrapper around the slskd 0.25.1 REST API. No business logic.

The httpx client is INJECTED by ``get_slskd_client``, never acquired here
(AUD-12). Non-2xx responses raise ``SlskdApiError`` (429 -> ``RateLimitedError``;
AUD-10). The discrete calls are ``with_retry`` + circuit-breaker wrapped; the
search-poll helpers are NOT, because the repository's poll loop owns its own
deadline.
"""

import logging
from typing import Any

import httpx
import msgspec

from core.exceptions import ExternalServiceError, RateLimitedError, SlskdApiError
from infrastructure.resilience.retry import CircuitBreaker, with_retry

from .slskd_models import (
    SlskdEnqueueResponse,
    SlskdOptions,
    SlskdSearchResponse,
    SlskdTransfer,
    SlskdUserSearchResponse,
)

logger = logging.getLogger(__name__)

_slskd_circuit_breaker = CircuitBreaker(name="slskd")
_RETRIABLE = (httpx.HTTPError, ExternalServiceError)
_NON_BREAKING = (RateLimitedError,)


def _flatten_transfers(payload: Any) -> list[dict[str, Any]]:
    """Walk slskd's per-user transfers tree and collect transfer dicts. Robust
    to the exact nesting: any dict carrying both ``id`` and ``filename`` is a
    transfer."""
    out: list[dict[str, Any]] = []

    def walk(node: Any) -> None:
        if isinstance(node, dict):
            if "id" in node and "filename" in node:
                out.append(node)
            else:
                for value in node.values():
                    walk(value)
        elif isinstance(node, list):
            for item in node:
                walk(item)

    walk(payload)
    return out


class SlskdClient:
    def __init__(self, http: httpx.AsyncClient, base_url: str, api_key: str):
        self._http = http
        self._base_url = base_url.rstrip("/")
        self._headers = {"X-API-Key": api_key}

    def _url(self, path: str) -> str:
        return f"{self._base_url}/api/v0{path}"

    @staticmethod
    def _check(response: httpx.Response) -> None:
        if response.status_code == 429:
            raise RateLimitedError("slskd: only one concurrent operation is permitted")
        if response.status_code >= 400:
            raise SlskdApiError(
                f"slskd returned HTTP {response.status_code}",
                details=response.text[:200],
                code=response.status_code,
            )

    @with_retry(
        circuit_breaker=_slskd_circuit_breaker,
        retriable_exceptions=_RETRIABLE,
        non_breaking_exceptions=_NON_BREAKING,
    )
    async def health_check(self) -> dict[str, Any]:
        # GET /api/v0/application; returns raw JSON (version/server state).
        try:
            response = await self._http.get(self._url("/application"), headers=self._headers)
        except httpx.HTTPError as exc:
            raise SlskdApiError(f"slskd request failed: {exc}") from exc
        self._check(response)
        return response.json()

    @with_retry(
        circuit_breaker=_slskd_circuit_breaker,
        retriable_exceptions=_RETRIABLE,
        non_breaking_exceptions=_NON_BREAKING,
    )
    async def get_options(self) -> SlskdOptions:
        # GET /api/v0/options; we read directories.downloads to tell the user the exact
        # path slskd saves to (for the downloads-mount diagnostic, not a hot path).
        try:
            response = await self._http.get(self._url("/options"), headers=self._headers)
        except httpx.HTTPError as exc:
            raise SlskdApiError(f"slskd get_options failed: {exc}") from exc
        self._check(response)
        return msgspec.convert(response.json(), type=SlskdOptions, strict=False)

    @with_retry(
        circuit_breaker=_slskd_circuit_breaker,
        retriable_exceptions=_RETRIABLE,
        non_breaking_exceptions=_NON_BREAKING,
    )
    async def enqueue(self, username: str, files: list[dict[str, Any]]) -> SlskdEnqueueResponse:
        """POST /api/v0/transfers/downloads/{username}.

        Body is a PLAIN JSON array ``[{"filename","size"}]`` (no options
        envelope, no destination/externalId; C1). Returns 201 ``{Enqueued,
        Failed}``, not a batch GUID. slskd permits only one concurrent enqueue
        (429 otherwise, C3), retried here with backoff.
        """
        try:
            response = await self._http.post(
                self._url(f"/transfers/downloads/{username}"),
                headers=self._headers,
                json=files,
            )
        except httpx.HTTPError as exc:
            raise SlskdApiError(f"slskd enqueue failed: {exc}") from exc
        self._check(response)
        return msgspec.json.decode(response.content, type=SlskdEnqueueResponse)

    @with_retry(
        circuit_breaker=_slskd_circuit_breaker,
        retriable_exceptions=_RETRIABLE,
        non_breaking_exceptions=_NON_BREAKING,
    )
    async def get_downloads(self, username: str) -> list[SlskdTransfer]:
        # GET /api/v0/transfers/downloads/{username}, flattened to transfers.
        try:
            response = await self._http.get(
                self._url(f"/transfers/downloads/{username}"), headers=self._headers
            )
        except httpx.HTTPError as exc:
            raise SlskdApiError(f"slskd get_downloads failed: {exc}") from exc
        if response.status_code == 404:
            return []
        self._check(response)
        return [
            msgspec.convert(t, type=SlskdTransfer, strict=False)
            for t in _flatten_transfers(response.json())
        ]

    @with_retry(
        circuit_breaker=_slskd_circuit_breaker,
        retriable_exceptions=_RETRIABLE,
        non_breaking_exceptions=_NON_BREAKING,
    )
    async def get_all_downloads(self) -> list[SlskdTransfer]:
        # GET /api/v0/transfers/downloads (every peer), username preserved per transfer
        # (the flatten loses it, so it's carried down from the per-user block). For the
        # downloads-mount diagnostic, not the per-task poll.
        try:
            response = await self._http.get(
                self._url("/transfers/downloads"), headers=self._headers
            )
        except httpx.HTTPError as exc:
            raise SlskdApiError(f"slskd get_all_downloads failed: {exc}") from exc
        if response.status_code == 404:
            return []
        self._check(response)
        out: list[SlskdTransfer] = []
        for block in response.json() or []:
            username = block.get("username", "") if isinstance(block, dict) else ""
            for t in _flatten_transfers(block):
                if not t.get("username"):
                    t = {**t, "username": username}
                out.append(msgspec.convert(t, type=SlskdTransfer, strict=False))
        return out

    @with_retry(
        circuit_breaker=_slskd_circuit_breaker,
        retriable_exceptions=_RETRIABLE,
        non_breaking_exceptions=_NON_BREAKING,
    )
    async def cancel_transfer(self, username: str, transfer_id: str) -> bool:
        """DELETE /api/v0/transfers/downloads/{username}/{id}?remove=true.
        Serves both cancellation of an in-flight transfer and post-import
        removal of a completed transfer record (DEC-1)."""
        try:
            response = await self._http.delete(
                self._url(f"/transfers/downloads/{username}/{transfer_id}"),
                headers=self._headers,
                params={"remove": "true"},
            )
        except httpx.HTTPError as exc:
            raise SlskdApiError(f"slskd cancel_transfer failed: {exc}") from exc
        if response.status_code == 404:
            return False
        self._check(response)
        return response.status_code in (200, 204)

    # Search poll: no retry wrapper, repository owns the deadline.

    async def start_search(
        self, search_text: str, *, timeout_seconds: float = 30.0
    ) -> SlskdSearchResponse:
        # POST /api/v0/searches. ``searchTimeout`` is MILLISECONDS (verified).
        body = {
            "searchText": search_text,
            "searchTimeout": int(timeout_seconds * 1000),
        }
        try:
            response = await self._http.post(
                self._url("/searches"), headers=self._headers, json=body
            )
        except httpx.HTTPError as exc:
            raise SlskdApiError(f"slskd start_search failed: {exc}") from exc
        self._check(response)
        return msgspec.json.decode(response.content, type=SlskdSearchResponse)

    async def get_search_state(self, search_id: str) -> SlskdSearchResponse:
        try:
            response = await self._http.get(
                self._url(f"/searches/{search_id}"), headers=self._headers
            )
        except httpx.HTTPError as exc:
            raise SlskdApiError(f"slskd get_search_state failed: {exc}") from exc
        self._check(response)
        return msgspec.json.decode(response.content, type=SlskdSearchResponse)

    async def get_search_responses(self, search_id: str) -> list[SlskdUserSearchResponse]:
        try:
            response = await self._http.get(
                self._url(f"/searches/{search_id}/responses"), headers=self._headers
            )
        except httpx.HTTPError as exc:
            raise SlskdApiError(f"slskd get_search_responses failed: {exc}") from exc
        self._check(response)
        return msgspec.json.decode(response.content, type=list[SlskdUserSearchResponse])
