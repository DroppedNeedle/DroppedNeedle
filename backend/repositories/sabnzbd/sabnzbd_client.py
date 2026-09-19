"""Raw httpx wrapper around the SABnzbd API. No business logic (that's
``SabnzbdDownloadClient``).

Verified against the owner's SABnzbd 5.0.4. Every call appends ``output=json`` +
``apikey`` as suffix query params (never headers; Lidarr ``SabnzbdProxy``). Adds an
NZB via ``mode=addfile`` (multipart POST of the fetched+validated NZB bytes): we
validate the bytes are a real NZB (not an indexer error page) before handing off.
``mode=addurl`` (SABnzbd fetches the enclosure URL itself) is the enqueue FALLBACK
only, for indexers DroppedNeedle cannot reach - it skips pre-validation by design.
Errors arrive as ``{"status": false, "error": …}`` or plain-text
``error: …``; both are handled, auth failures detected by message.

The httpx client is INJECTED (AUD-12). The full ``apikey`` is required (the add-only
``nzbkey`` can't do queue/history/delete); it's encrypted at rest + never logged.
"""

import asyncio
import logging
import re
from typing import Any

import httpx

from core.exceptions import ExternalServiceError, NewznabApiError

from .sabnzbd_models import (
    SabnzbdAddResponse,
    SabnzbdConfig,
    SabnzbdHistory,
    SabnzbdQueue,
)

logger = logging.getLogger(__name__)

# addurl embeds the Newznab enclosure URL - which carries the indexer's apikey - in
# the SABnzbd request, and SABnzbd may echo the submitted URL in an error body.
# Scrub credential-looking query values from anything raised past this client so
# indexer secrets never reach logs via the strategy's exception logging. Covers both
# Newznab credential forms: ``apikey=`` and DrunkenSlug-style ``r=`` (the per-user
# key); the ``r`` alternative is anchored so innocent words like ``error=`` don't
# match, and the ``i=`` user id is left visible (an identifier, not a credential).
_QUERY_SECRET_RE = re.compile(r"(?i)((?:apikey|api_key|(?<![A-Za-z0-9_])r)=)[^&\s'\"]+")


def _redact_query_secrets(text: str) -> str:
    return _QUERY_SECRET_RE.sub(r"\1***", text)


class SabnzbdApiError(ExternalServiceError):
    """Transport/HTTP/SABnzbd error. Mapped to HTTP 503 by the registered handler."""

    def __init__(self, message: str, details: Any = None, *, auth: bool = False) -> None:
        super().__init__(message, details)
        self.auth = auth


def _redacted_addurl_error(exc: SabnzbdApiError) -> SabnzbdApiError:
    """Rebuild an addurl error with any echoed enclosure credentials scrubbed."""
    details = exc.details
    if isinstance(details, str):
        details = _redact_query_secrets(details)
    return SabnzbdApiError(
        _redact_query_secrets(exc.message), details, auth=exc.auth
    )


class SabnzbdClient:
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
        # Resilience (step 6): transient transport errors + 5xx on IDEMPOTENT calls are
        # retried with exponential backoff so a momentary SABnzbd/network blip during a poll
        # doesn't fail the whole download. (No full circuit-breaker: for a single self-hosted
        # SABnzbd client, backoff'd retries already prevent hammering a down server, and a
        # breaker's open/half-open machinery would be speculative complexity here.)
        self._max_attempts = max(1, max_attempts)
        self._retry_backoff = retry_backoff

    def _url(self) -> str:
        return f"{self._base_url}/api"

    async def _request_with_retry(
        self, method: str, url: str, *, timeout: float, **kwargs: Any
    ) -> httpx.Response:
        """Send an IDEMPOTENT request, retrying transient transport errors + 5xx responses
        with exponential backoff. NOT used for ``addfile``/``addurl`` (non-idempotent
        queue mutations): re-sending could double-add a job and re-create the ``.1/.2``
        orphan. A 4xx / SABnzbd-logical error is returned for the caller to surface
        (never retried)."""
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
                return resp  # success or a non-retryable client error (caller parses it)
            # 5xx -> retry until attempts run out, then fall through with the last response
        if resp is not None:
            return resp  # last attempt was a 5xx; let the caller's _parse raise on it
        raise last_exc  # every attempt was a transport error

    def _params(self, extra: dict[str, str]) -> dict[str, str]:
        # output/apikey forced to the end (Lidarr SabnzbdProxy.BuildRequest).
        return {**extra, "output": "json", "apikey": self._api_key}

    async def version(self, *, timeout: float = 15.0) -> str:
        data = await self._get({"mode": "version"}, timeout=timeout)
        return str(data.get("version", "")) if isinstance(data, dict) else ""

    async def get_config(self, *, timeout: float = 15.0) -> SabnzbdConfig:
        data = await self._get({"mode": "get_config"}, timeout=timeout)
        import msgspec

        return msgspec.convert(data.get("config", {}), type=SabnzbdConfig, strict=False)

    async def get_cats(self, *, timeout: float = 15.0) -> list[str]:
        data = await self._get({"mode": "get_cats"}, timeout=timeout)
        cats = data.get("categories", []) if isinstance(data, dict) else []
        return [str(c) for c in cats]

    async def queue(self, *, timeout: float = 30.0) -> SabnzbdQueue:
        data = await self._get({"mode": "queue"}, timeout=timeout)
        import msgspec

        return msgspec.convert(data.get("queue", {}), type=SabnzbdQueue, strict=False)

    async def history(
        self,
        *,
        limit: int = 50,
        search: str | None = None,
        nzo_ids: str | None = None,
        timeout: float = 30.0,
    ) -> SabnzbdHistory:
        """``mode=history``. ``nzo_ids`` (exact) or ``search`` (name substring) narrow the
        result to a single job so a busy SABnzbd can't bury it past the ``limit`` window."""
        params: dict[str, str] = {"mode": "history", "limit": str(limit)}
        if nzo_ids:
            params["nzo_ids"] = nzo_ids
        if search:
            params["search"] = search
        data = await self._get(params, timeout=timeout)
        import msgspec

        return msgspec.convert(data.get("history", {}), type=SabnzbdHistory, strict=False)

    async def add_file(
        self,
        job_name: str,
        nzb_bytes: bytes,
        *,
        category: str | None = None,
        priority: int | None = None,
        post_processing: int | None = None,
        timeout: float = 60.0,
    ) -> SabnzbdAddResponse:
        """``mode=addfile`` multipart POST. ``cat`` not ``category`` on add (Lidarr
        quirk). The multipart field is ``name`` = (``{job_name}.nzb``, bytes, x-nzb)."""
        params: dict[str, str] = {"mode": "addfile", "nzbname": job_name}
        if category:
            params["cat"] = category
        if priority is not None:
            params["priority"] = str(priority)
        if post_processing is not None:
            params["pp"] = str(post_processing)
        files = {"name": (f"{job_name}.nzb", nzb_bytes, "application/x-nzb")}
        try:
            response = await self._http.post(
                self._url(), params=self._params(params), files=files, timeout=timeout
            )
        except httpx.HTTPError as exc:
            raise SabnzbdApiError(f"SABnzbd addfile failed: {exc}") from exc
        data = self._parse(response)
        import msgspec

        return msgspec.convert(data, type=SabnzbdAddResponse, strict=False)

    async def add_url(
        self,
        job_name: str,
        nzb_url: str,
        *,
        category: str | None = None,
        priority: int | None = None,
        post_processing: int | None = None,
        timeout: float = 60.0,
    ) -> SabnzbdAddResponse:
        """``mode=addurl`` GET: hand SABnzbd the Newznab enclosure URL and let it fetch
        the NZB itself. ``name`` is the URL, ``nzbname`` the job name (same ``cat`` /
        ``priority`` / ``pp`` options as ``add_file``; params per the SABnzbd 5.1 API
        reference, and the ``nzo_ids`` envelope matches the live-verified 5.0.4
        addfile contract - addurl itself was validated against a live SABnzbd for
        #457, SABnzbd version unstated there).

        Enqueue FALLBACK only: used when DroppedNeedle itself gets no response
        fetching the NZB (a no-response transport failure, never a content rejection
        or definitive indexer HTTP error) - some indexers are reachable only from the
        SABnzbd host. Bypasses the retry helper like ``add_file``: it mutates the
        queue, so retrying an uncertain response could double-add the job. SABnzbd
        errors are scrubbed of echoed enclosure credentials before raising.
        """
        params: dict[str, str] = {
            "mode": "addurl",
            "name": nzb_url,
            "nzbname": job_name,
        }
        if category:
            params["cat"] = category
        if priority is not None:
            params["priority"] = str(priority)
        if post_processing is not None:
            params["pp"] = str(post_processing)
        try:
            response = await self._http.get(
                self._url(), params=self._params(params), timeout=timeout
            )
        except httpx.HTTPError as exc:
            raise SabnzbdApiError(f"SABnzbd addurl failed: {exc}") from exc
        try:
            data = self._parse(response)
        except SabnzbdApiError as exc:
            # from None: the cause IS the unredacted original - chaining it (explicitly
            # or implicitly) would print the secret when the strategy logs the chain.
            raise _redacted_addurl_error(exc) from None
        import msgspec

        return msgspec.convert(data, type=SabnzbdAddResponse, strict=False)

    async def delete_queue(self, nzo_id: str, *, del_files: bool, timeout: float = 30.0) -> bool:
        data = await self._get(
            {"mode": "queue", "name": "delete", "value": nzo_id, "del_files": "1" if del_files else "0"},
            timeout=timeout,
        )
        return _ok(data)

    async def delete_history(
        self, nzo_id: str, *, del_files: bool, archive: bool = False, timeout: float = 30.0
    ) -> bool:
        data = await self._get(
            {
                "mode": "history",
                "name": "delete",
                "value": nzo_id,
                "del_files": "1" if del_files else "0",
                "archive": "1" if archive else "0",
            },
            timeout=timeout,
        )
        return _ok(data)

    async def fetch_nzb(self, url: str, *, timeout: float = 60.0) -> bytes:
        """GET the release's NZB URL (the Newznab enclosure, apikey already embedded)
        and validate the bytes are a real NZB (XML), not an indexer error page."""
        try:
            response = await self._request_with_retry(
                "GET", url, timeout=timeout, follow_redirects=True
            )
        except httpx.HTTPError as exc:
            error = NewznabApiError(f"NZB fetch failed: {exc}")
            # No-response transport failure (DNS/refused/timeout - cf.
            # content_rejection below): the enqueue path falls back to addurl on
            # this marker. A definitive indexer HTTP error carries neither marker.
            error.transport_failure = True
            raise error from exc
        if response.status_code >= 400:
            raise NewznabApiError(
                f"NZB fetch returned HTTP {response.status_code}", details=response.text[:200]
            )
        content = response.content
        head = content[:512].lstrip().lower()
        if not (head.startswith(b"<?xml") or head.startswith(b"<nzb") or b"<nzb" in head):
            error = NewznabApiError(
                "indexer returned a non-NZB body (likely an error/limit page), not an NZB",
                details={
                    "status": response.status_code,
                    "content_type": response.headers.get("content-type"),
                    "snippet": response.text[:200],
                },
                code=response.status_code,
            )
            # Deterministic content rejection (an indexer error/limit page, not a
            # transport failure): the Usenet enqueue path blocklists on this marker.
            error.content_rejection = True
            raise error
        return content

    async def _get(self, extra: dict[str, str], *, timeout: float) -> Any:
        try:
            response = await self._request_with_retry(
                "GET", self._url(), params=self._params(extra), timeout=timeout
            )
        except httpx.HTTPError as exc:
            raise SabnzbdApiError(f"SABnzbd request failed: {exc}") from exc
        return self._parse(response)

    def _parse(self, response: httpx.Response) -> Any:
        if response.status_code >= 400:
            raise SabnzbdApiError(
                f"SABnzbd returned HTTP {response.status_code}", details=response.text[:200]
            )
        text = response.text.strip()
        # Plain-text error form: a body starting "error" (strip "error: ").
        if text.lower().startswith("error"):
            msg = text.split(":", 1)[1].strip() if ":" in text else text
            raise SabnzbdApiError(msg, auth=_is_auth_error(msg))
        try:
            data = response.json()
        except ValueError as exc:
            raise SabnzbdApiError(f"SABnzbd returned non-JSON: {text[:120]}") from exc
        # JSON error form: {"status": false, "error": "..."}.
        if isinstance(data, dict) and _is_false(data.get("status")) and data.get("error"):
            msg = str(data["error"])
            raise SabnzbdApiError(msg, auth=_is_auth_error(msg))
        return data


def _ok(data: Any) -> bool:
    if isinstance(data, dict):
        return not _is_false(data.get("status", True))
    return True


def _is_false(value: Any) -> bool:
    """SABnzbd serialises the status boolean as ``false`` or ``"False"`` (matched
    case-insensitively)."""
    if isinstance(value, bool):
        return value is False
    return str(value).strip().lower() == "false"


def _is_auth_error(message: str) -> bool:
    low = message.lower()
    return "api key incorrect" in low or "api key required" in low
