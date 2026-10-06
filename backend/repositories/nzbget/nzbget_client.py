"""Raw httpx wrapper around the NZBGet JSON-RPC API. No business logic (that's
``NzbgetDownloadClient``).

NZBGet differs from SABnzbd in three ways that shape this module:

* **Transport.** One POST endpoint (``{base}/jsonrpc``) taking
  ``{"method": ..., "params": [...]}``; errors come back as a JSON-RPC
  ``{"error": {"code": ..., "message": ...}}`` envelope rather than in-band.
* **Auth.** HTTP Basic, not a query-string key. NZBGet also accepts credentials
  embedded in the path (``/user:pass/jsonrpc``), which is deliberately NOT used:
  it would put the password in every URL, and from there into exception text.
* **Positional params.** JSON-RPC params are an ordered array, so ``append`` must
  send every argument up to the last one it needs, in NZBGet's declared order.

Adds an NZB by posting the fetched + validated NZB bytes base64-encoded in
``Content``. Passing the enclosure URL as ``Content`` instead (NZBGet fetches it
itself) is the enqueue FALLBACK only, for indexers DroppedNeedle cannot reach.

Requires NZBGet 16 or newer for the three-argument ``editqueue(Command, Param, IDs)``.
The httpx client is INJECTED. The password is never logged and never placed in a URL.
"""

import asyncio
import base64
import logging
from typing import Any

import httpx

from core.exceptions import ExternalServiceError
from repositories.nzb_fetch import fetch_nzb, redact_query_secrets

from .nzbget_models import NzbgetConfigOption, NzbgetGroup, NzbgetHistoryItem

logger = logging.getLogger(__name__)


class NzbgetApiError(ExternalServiceError):
    """Transport/HTTP/NZBGet error. Mapped to HTTP 503 by the registered handler."""

    def __init__(self, message: str, details: Any = None, *, auth: bool = False) -> None:
        super().__init__(message, details)
        self.auth = auth


def _redacted_url_error(exc: NzbgetApiError) -> NzbgetApiError:
    """Rebuild an append-by-URL error with any echoed enclosure credentials scrubbed."""
    details = exc.details
    if isinstance(details, str):
        details = redact_query_secrets(details)
    return NzbgetApiError(redact_query_secrets(exc.message), details, auth=exc.auth)


class NzbgetClient:
    def __init__(
        self,
        http: httpx.AsyncClient,
        base_url: str,
        username: str,
        password: str,
        *,
        max_attempts: int = 3,
        retry_backoff: float = 0.5,
    ) -> None:
        self._http = http
        self._base_url = base_url.rstrip("/")
        self._auth = httpx.BasicAuth(username, password)
        # Same resilience policy as the SABnzbd client: transient transport errors and
        # 5xx on IDEMPOTENT calls are retried with exponential backoff so a momentary
        # blip during a poll doesn't fail the whole download.
        self._max_attempts = max(1, max_attempts)
        self._retry_backoff = retry_backoff

    def _url(self) -> str:
        return f"{self._base_url}/jsonrpc"

    async def _request_with_retry(
        self, method: str, url: str, *, timeout: float, **kwargs: Any
    ) -> httpx.Response:
        """Send an IDEMPOTENT request, retrying transient transport errors + 5xx
        responses with exponential backoff. NOT used for ``append`` (a non-idempotent
        queue mutation): re-sending could double-add a job."""
        last_exc: httpx.HTTPError | None = None
        resp: httpx.Response | None = None
        for attempt in range(self._max_attempts):
            if attempt:
                await asyncio.sleep(self._retry_backoff * (2 ** (attempt - 1)))
            try:
                resp = await self._http.request(method, url, timeout=timeout, **kwargs)
            except httpx.HTTPError as exc:
                last_exc, resp = exc, None
                continue
            if resp.status_code < 500:
                return resp
            # 5xx -> retry until attempts run out, then fall through with the last response
        if resp is not None:
            return resp
        raise last_exc  # every attempt was a transport error

    async def _call(self, method: str, params: list[Any], *, timeout: float) -> Any:
        """Invoke an IDEMPOTENT JSON-RPC method."""
        payload = {"version": "1.1", "id": 1, "method": method, "params": params}
        try:
            response = await self._request_with_retry(
                "POST", self._url(), timeout=timeout, json=payload, auth=self._auth
            )
        except httpx.HTTPError as exc:
            raise NzbgetApiError(f"NZBGet request failed: {exc}") from exc
        return self._parse(response, method)

    def _parse(self, response: httpx.Response, method: str) -> Any:
        # 401/403 is the only unambiguous auth signal NZBGet gives; its JSON-RPC error
        # codes do not distinguish bad credentials from a bad request.
        if response.status_code in (401, 403):
            raise NzbgetApiError(
                "NZBGet rejected the credentials (check the control username and password)",
                auth=True,
            )
        if response.status_code >= 400:
            raise NzbgetApiError(
                f"NZBGet returned HTTP {response.status_code}", details=response.text[:200]
            )
        try:
            data = response.json()
        except ValueError as exc:
            # A non-JSON body here is almost always the NZBGet web UI answering because
            # the configured URL points at the app root rather than its control port.
            raise NzbgetApiError(
                f"NZBGet returned non-JSON for {method}: {response.text[:120]}"
            ) from exc
        if isinstance(data, dict) and data.get("error"):
            error = data["error"]
            message = (
                str(error.get("message") or error)
                if isinstance(error, dict)
                else str(error)
            )
            raise NzbgetApiError(f"NZBGet {method} failed: {message}")
        if not isinstance(data, dict) or "result" not in data:
            raise NzbgetApiError(f"NZBGet returned no result for {method}")
        return data["result"]

    async def version(self, *, timeout: float = 15.0) -> str:
        result = await self._call("version", [], timeout=timeout)
        return str(result or "")

    async def config(self, *, timeout: float = 15.0) -> list[NzbgetConfigOption]:
        """``config`` returns the loaded options as ``{"Name", "Value"}`` pairs."""
        result = await self._call("config", [], timeout=timeout)
        import msgspec

        return msgspec.convert(result or [], type=list[NzbgetConfigOption], strict=False)

    async def listgroups(self, *, timeout: float = 30.0) -> list[NzbgetGroup]:
        """Queued, downloading and post-processing jobs. The ``0`` argument is
        ``NumberOfLogEntries``; we never want the per-job log tail."""
        result = await self._call("listgroups", [0], timeout=timeout)
        import msgspec

        return msgspec.convert(result or [], type=list[NzbgetGroup], strict=False)

    async def history(self, *, hidden: bool = False, timeout: float = 30.0) -> list[NzbgetHistoryItem]:
        """Finished, failed and deleted jobs.

        Unlike SABnzbd's ``history`` there is no server-side limit or search, so the
        whole list comes back and the caller filters it. That is NZBGet's own
        behaviour, not an oversight: its history is capped by ``KeepHistory``.
        """
        result = await self._call("history", [hidden], timeout=timeout)
        import msgspec

        return msgspec.convert(result or [], type=list[NzbgetHistoryItem], strict=False)

    async def append(
        self,
        job_name: str,
        nzb_bytes: bytes,
        *,
        category: str | None = None,
        priority: int | None = None,
        dupe_key: str = "",
        timeout: float = 60.0,
    ) -> int:
        """``append`` with the NZB inline, base64-encoded in ``Content``.

        Returns the new ``NZBID``; NZBGet answers ``0`` when it rejected the NZB.
        Bypasses the retry helper: it mutates the queue, so retrying an uncertain
        response could double-add the job.
        """
        content = base64.b64encode(nzb_bytes).decode("ascii")
        return await self._append(
            job_name, content, category, priority, dupe_key, timeout, redact=False
        )

    async def append_url(
        self,
        job_name: str,
        nzb_url: str,
        *,
        category: str | None = None,
        priority: int | None = None,
        dupe_key: str = "",
        timeout: float = 60.0,
    ) -> int:
        """``append`` with the enclosure URL as ``Content``: NZBGet recognises an
        ``http(s)://`` value and fetches the NZB itself.

        Enqueue FALLBACK only, used when DroppedNeedle gets no response fetching the
        NZB (a no-response transport failure, never a content rejection or a definitive
        indexer HTTP error) - some indexers are reachable only from the NZBGet host.
        NZBGet errors are scrubbed of echoed enclosure credentials before raising.
        """
        return await self._append(
            job_name, nzb_url, category, priority, dupe_key, timeout, redact=True
        )

    async def _append(
        self,
        job_name: str,
        content: str,
        category: str | None,
        priority: int | None,
        dupe_key: str,
        timeout: float,
        *,
        redact: bool,
    ) -> int:
        # Positional, in NZBGet's declared order:
        #   NZBFilename, Content, Category, Priority, AddToTop, AddPaused,
        #   DupeKey, DupeScore, DupeMode, PPParameters
        # DupeMode FORCE stops NZBGet silently discarding our job as a duplicate of an
        # earlier grab of the same release: the orchestrator already decided to fetch it.
        params: list[Any] = [
            f"{job_name}.nzb",
            content,
            category or "",
            priority if priority is not None else 0,
            False,
            False,
            dupe_key,
            0,
            "FORCE",
            [],
        ]
        payload = {"version": "1.1", "id": 1, "method": "append", "params": params}
        try:
            response = await self._http.post(
                self._url(), json=payload, timeout=timeout, auth=self._auth
            )
        except httpx.HTTPError as exc:
            message = f"NZBGet append failed: {exc}"
            raise NzbgetApiError(
                redact_query_secrets(message) if redact else message
            ) from exc
        try:
            result = self._parse(response, "append")
        except NzbgetApiError as exc:
            if not redact:
                raise
            # from None: the cause IS the unredacted original - chaining it would print
            # the secret when the strategy logs the chain.
            raise _redacted_url_error(exc) from None
        try:
            return int(result)
        except (TypeError, ValueError):
            return 0

    async def editqueue(self, command: str, ids: list[int], *, param: str = "", timeout: float = 30.0) -> bool:
        """``editqueue(Command, Param, IDs)`` (NZBGet 16+). Returns NZBGet's own
        boolean: ``False`` means it refused the edit, which the caller surfaces rather
        than treating as success."""
        if not ids:
            return True
        result = await self._call("editqueue", [command, param, ids], timeout=timeout)
        return bool(result)

    async def fetch_nzb(self, url: str, *, timeout: float = 60.0) -> bytes:
        """GET and validate the release's NZB. Shared with the SABnzbd client; see
        ``repositories/nzb_fetch`` for the ``transport_failure`` /
        ``content_rejection`` markers the enqueue path branches on."""
        return await fetch_nzb(self._request_with_retry, url, timeout=timeout)
