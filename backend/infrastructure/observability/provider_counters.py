"""Outbound provider-call counters (QW9 Part 3) and upstream rate-limit
telemetry (QW11 Part 2).

Call counters: one increment per wire attempt at each provider funnel,
classified from the HTTP response status at the site's existing error mapping:
``200``/``204`` -> ``ok``, ``404`` -> ``empty_404``, everything else
(including transport errors, recorded with a ``None`` status) ->
``http_error``.

Rate-limit telemetry: a separate bounded gauge fed from response headers
(``x-ratelimit-limit/remaining/reset``). It is deliberately NOT part of the
call counters so telemetry observations can never
perturb ok/empty_404/http_error counts. WARN logs at most once per minute per
provider when remaining slots drop to the low threshold.

Notes carried over from the plans:
- Counters observe only: no limiter value, retry policy, lane assignment, or
  dedup behavior is touched by recording.
- Breaker-open rejections raised by ``@with_retry`` before a funnel body runs
  stay implicit (attempts-vs-success delta) - there is no hook in the shared
  retry decorator.
- Last.fm returns application errors as HTTP 200 bodies; those count as
  ``ok`` here because classification is status-based by design.
- Process-local state, correct under the single-worker invariant, reset on
  restart, never reset by cache clears. No locks needed at one worker.
"""

from __future__ import annotations

import logging
import time
from collections.abc import Callable
from typing import Any

from infrastructure.cache.cache_metrics import WindowedCounterMap
from infrastructure.queue.priority_queue import RequestPriority

logger = logging.getLogger(__name__)

PROVIDER_NAMES = (
    "musicbrainz",
    "listenbrainz",
    "lastfm",
    "coverart",
    "audiodb",
    "discogs",
)

OUTCOME_OK = "ok"
OUTCOME_EMPTY_404 = "empty_404"
OUTCOME_HTTP_ERROR = "http_error"

# Funnels that take no priority-lane parameter (ListenBrainz, Last.fm,
# AudioDB) record under this label.
UNLANED = "unlaned"

DEFAULT_WINDOW_SECONDS = 3600

_counters = WindowedCounterMap()
_started_at = int(time.time())


def classify_outcome(status_code: int | None) -> str:
    if status_code in (200, 204):
        return OUTCOME_OK
    if status_code == 404:
        return OUTCOME_EMPTY_404
    return OUTCOME_HTTP_ERROR


def lane_label(priority: RequestPriority | str | None) -> str:
    if priority is None:
        return UNLANED
    if isinstance(priority, RequestPriority):
        return priority.name.lower()
    return str(priority)


def record_provider_call(
    provider: str,
    priority: RequestPriority | str | None,
    status_code: int | None,
    source_context: Any | None = None,
) -> None:
    """Record one wire attempt and, for source-bound calls, its identity.

    Source metadata is deliberately limited to the mode, opaque source id, and
    generation. Endpoints and request URLs never enter telemetry.
    """
    key: tuple[Any, ...] = (
        provider,
        lane_label(priority),
        classify_outcome(status_code),
    )
    if source_context is not None:
        key += (
            str(getattr(source_context, "source_mode", "")),
            str(getattr(source_context, "source_id", "")),
            int(getattr(source_context, "generation", 0)),
        )
    _counters.increment(key)


def snapshot_provider_rows(
    window_seconds: int | None = None,
) -> list[dict[str, Any]]:
    """Rows sorted by (provider, lane, outcome) for stable rendering."""
    window = DEFAULT_WINDOW_SECONDS if window_seconds is None else int(window_seconds)
    snapshot = _counters.snapshot(window)
    totals = _counters.totals()
    per_minute_divisor = window / 60
    rows: list[dict[str, Any]] = []
    for key, window_count in sorted(snapshot.items()):
        provider, lane, outcome = key[:3]
        row: dict[str, Any] = {
            "provider": provider,
            "priority": lane,
            "outcome": outcome,
            "count_total": totals.get(key, 0),
            "rate_per_min_window": round(window_count / per_minute_divisor, 2),
        }
        if len(key) == 6:
            row.update(
                {
                    "source_mode": key[3],
                    "source_id": key[4],
                    "source_generation": key[5],
                }
            )
        rows.append(row)
    return rows


def counters_since() -> int:
    """Wall-clock epoch seconds when this process started counting."""
    return _started_at


LOW_REMAINING_THRESHOLD = 3
LOW_REMAINING_WARN_INTERVAL_SECONDS = 60.0
_RATELIMIT_HEADER_NAMES = (
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
)


def _header_int(headers: Any, name: str) -> int | None:
    raw = headers.get(name)
    if raw is None:
        return None
    try:
        return int(str(raw).strip())
    except (TypeError, ValueError):
        return None


def _header_float(headers: Any, name: str) -> float | None:
    raw = headers.get(name)
    if raw is None:
        return None
    try:
        return float(str(raw).strip())
    except (TypeError, ValueError):
        return None


class RateLimitGauge:
    """Bounded per-provider gauge of the latest observed upstream rate-limit
    headers plus a windowed count of low-remaining observations.

    Storage is one dict entry per provider seen - never per request or key -
    so it cannot grow with traffic. Deliberately separate from the call
    counters above: observing telemetry must not perturb call outcomes.
    """

    __slots__ = ("_latest", "_low_events", "_last_warn", "_clock")

    def __init__(self, clock: Callable[[], float] | None = None):
        self._latest: dict[str, dict[str, Any]] = {}
        self._low_events = WindowedCounterMap()
        self._last_warn: dict[str, float] = {}
        self._clock = clock or time.monotonic

    def observe(self, provider: str, headers: Any) -> None:
        """Record one observation from *headers* (httpx.Headers or any
        case-insensitive mapping-like object). Malformed header values are
        skipped field-by-field; this never raises into the wire funnel."""
        remaining = _header_int(headers, "x-ratelimit-remaining")
        self._latest[provider] = {
            "provider": provider,
            "limit": _header_int(headers, "x-ratelimit-limit"),
            "remaining": remaining,
            # Verbatim x-ratelimit-reset value (MusicBrainz sends epoch
            # seconds); consumers compute seconds-until themselves.
            "reset_epoch": _header_float(headers, "x-ratelimit-reset"),
            "observed_at": time.time(),
        }
        if remaining is not None and remaining <= LOW_REMAINING_THRESHOLD:
            self._low_events.increment((provider,))
            self._warn_low_remaining(provider, remaining)

    def _warn_low_remaining(self, provider: str, remaining: int) -> None:
        now = self._clock()
        last = self._last_warn.get(provider)
        if last is not None and now - last < LOW_REMAINING_WARN_INTERVAL_SECONDS:
            return
        self._last_warn[provider] = now
        logger.warning(
            "provider_rate_limit.low_remaining provider=%s remaining=%d threshold=%d",
            provider,
            remaining,
            LOW_REMAINING_THRESHOLD,
        )

    def snapshot_rows(self) -> list[dict[str, Any]]:
        low_totals = self._low_events.snapshot()
        rows = []
        for provider in sorted(self._latest):
            row = dict(self._latest[provider])
            row["low_remaining_events_window"] = low_totals.get((provider,), 0)
            rows.append(row)
        return rows


_rate_limit_gauge = RateLimitGauge()


def record_rate_limit_headers(provider: str, headers: Any) -> None:
    """Feed one upstream rate-limit observation into the telemetry gauge.

    Never raises into the funnel; never touches the call counters.
    """
    try:
        _rate_limit_gauge.observe(provider, headers)
    except Exception:  # noqa: BLE001 - telemetry must not break the wire path
        logger.debug("rate_limit observation failed for %s", provider, exc_info=True)


def snapshot_rate_limit_rows() -> list[dict[str, Any]]:
    return _rate_limit_gauge.snapshot_rows()
