"""iTunes Search API client - the "Get it" purchase-link fallback (phase 01).

Keyless public API; see ``ITUNES_API_NOTES.md`` for the live-verified shape.
Degradation source: a failed lookup records into the request-scoped
DegradationContext and returns ``None`` - the "Where to buy" section still
renders its Bandcamp fallback row, so iTunes being down never fails a page.
"""

import logging
from typing import Any

import httpx
import msgspec
from rapidfuzz import fuzz

from core.exceptions import ExternalServiceError, RateLimitedError
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from infrastructure.resilience.retry import CircuitBreaker, CircuitOpenError, with_retry
from infrastructure.service_health import report_breaker_health

logger = logging.getLogger(__name__)

_SOURCE = "itunes"

ITUNES_SEARCH_URL = "https://itunes.apple.com/search"

# Apple documents ~20 calls/minute/IP for the keyless Search API (429 above).
# 0.3/s with a small burst stays safely under it; results are cached upstream
# so this only paces genuinely cold lookups.
_rate_limiter = TokenBucketRateLimiter(rate=0.3, capacity=2)

_itunes_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="itunes",
    on_state_change=report_breaker_health(
        "itunes",
        "purchase links",
        message="iTunes buy-link lookups are temporarily unavailable.",
    ),
)

# the requested artist must actually be the found album's artist - the search
# ranks tribute/cover albums above the real thing for popular queries
_ARTIST_MATCH_THRESHOLD = 80


def _record_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx is not None:
        ctx.record(IntegrationResult.error(source=_SOURCE, msg=msg))


class ITunesAlbumResult(msgspec.Struct):
    """One matched album on the iTunes/Apple Music store."""

    url: str
    collection_name: str
    artist_name: str


class ITunesRepository:
    def __init__(self, http_client: httpx.AsyncClient) -> None:
        self._client = http_client

    @staticmethod
    def reset_circuit_breaker() -> None:
        _itunes_circuit_breaker.reset()

    async def find_album(
        self, artist_name: str, album_title: str, *, country: str = "US"
    ) -> ITunesAlbumResult | None:
        """The store page for an album, or None (absence OR degraded - a
        degradation record distinguishes the latter). The artist must fuzzy-match
        the result's artist, or a tribute album would win the slot."""
        term = f"{artist_name} {album_title}".strip()
        if not term:
            return None
        try:
            data = await self._search(term, country)
        except CircuitOpenError:
            _record_degradation(f"Circuit open: album search {term!r}")
            return None
        except (ExternalServiceError, RateLimitedError) as exc:
            _record_degradation(f"Album search failed for {term!r}: {exc}")
            return None

        for result in data.get("results", []):
            if result.get("wrapperType") != "collection":
                continue
            url = result.get("collectionViewUrl")
            found_artist = result.get("artistName") or ""
            if not url or not found_artist:
                continue
            if fuzz.token_set_ratio(artist_name.lower(), found_artist.lower()) >= _ARTIST_MATCH_THRESHOLD:
                return ITunesAlbumResult(
                    url=url,
                    collection_name=result.get("collectionName") or album_title,
                    artist_name=found_artist,
                )
        return None

    @with_retry(
        max_attempts=2,
        circuit_breaker=_itunes_circuit_breaker,
        retriable_exceptions=(httpx.TimeoutException, httpx.TransportError),
    )
    async def _search(self, term: str, country: str) -> dict[str, Any]:
        await _rate_limiter.acquire()
        try:
            response = await self._client.get(
                ITUNES_SEARCH_URL,
                params={
                    "term": term,
                    "entity": "album",
                    "country": country,
                    "limit": 10,
                },
            )
        except httpx.TransportError:
            # timeouts and connection failures stay raw so @with_retry's
            # retriable_exceptions actually sees them; wrapping them here would
            # silently disable the retry
            raise
        except httpx.HTTPError as exc:
            raise ExternalServiceError(f"iTunes search transport error: {exc}") from exc
        if response.status_code == 429:
            raise RateLimitedError("iTunes search rate limited", retry_after_seconds=60)
        if response.status_code != 200:
            raise ExternalServiceError(
                f"iTunes search returned HTTP {response.status_code}"
            )
        try:
            return msgspec.json.decode(response.content, type=dict[str, Any])
        except msgspec.DecodeError as exc:
            raise ExternalServiceError("iTunes search returned invalid JSON") from exc
