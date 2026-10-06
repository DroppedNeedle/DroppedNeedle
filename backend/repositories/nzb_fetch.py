"""Shared Newznab NZB retrieval for the Usenet download clients.

Fetching the release NZB and proving the bytes are really an NZB is a property of
the *indexer*, not of the download client, so SABnzbd and NZBGet share it rather
than keeping two copies of a security-sensitive check. Both the credential
scrubbing and the two exception markers below are relied on by the enqueue paths:

* ``transport_failure`` - no response at all (DNS/refused/timeout). The caller may
  fall back to handing the client the enclosure URL to fetch itself, for indexers
  only the download host can reach.
* ``content_rejection`` - a definite non-NZB body (an indexer error or limit page).
  The caller blocklists the release; retrying cannot help.

Newznab enclosure URLs carry the indexer's apikey, so anything raised past this
module has credential-looking query values scrubbed before it can reach a log.
"""

import re
from typing import Any, Protocol

import httpx

from core.exceptions import NewznabApiError

# Covers both Newznab credential forms: ``apikey=`` and DrunkenSlug-style ``r=``
# (the per-user key). The ``r`` alternative is anchored so innocent words like
# ``error=`` don't match, and the ``i=`` user id is left visible (an identifier,
# not a credential).
_QUERY_SECRET_RE = re.compile(r"(?i)((?:apikey|api_key|(?<![A-Za-z0-9_])r)=)[^&\s'\"]+")


def redact_query_secrets(text: str) -> str:
    """Mask credential-looking query values in any text headed for a log."""
    return _QUERY_SECRET_RE.sub(r"\1***", text)


class _Send(Protocol):
    """An idempotent-request sender. Each client passes its own retry wrapper so
    the backoff policy stays with the client that owns the connection."""

    async def __call__(
        self, method: str, url: str, *, timeout: float, **kwargs: Any
    ) -> httpx.Response: ...


async def fetch_nzb(send: _Send, url: str, *, timeout: float = 60.0) -> bytes:
    """GET the release's NZB URL (the Newznab enclosure, apikey already embedded)
    and validate the bytes are a real NZB (XML), not an indexer error page."""
    try:
        response = await send("GET", url, timeout=timeout, follow_redirects=True)
    except httpx.HTTPError as exc:
        error = NewznabApiError(f"NZB fetch failed: {exc}")
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
        error.content_rejection = True
        raise error
    return content
