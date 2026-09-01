import asyncio
import hashlib
import math
import threading
import time
from typing import Any, Awaitable, Callable

import httpx

import msgspec
from core.exceptions import (
    ExternalServiceError,
    RateLimitedError,
    ServiceDisabledUpstreamError,
)
from infrastructure.cache.cache_keys import (
    LB_PREFIX,
    listenbrainz_management_genres_key,
)
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.observability.provider_counters import (
    record_provider_call,
    record_rate_limit_headers,
)
from infrastructure.resilience.retry import CircuitOpenError, CircuitBreaker, with_retry
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from repositories.listenbrainz_models import (
    ListenBrainzArtist,
    ListenBrainzReleaseGroup,
    ListenBrainzRecording,
    ListenBrainzListen,
    ListenBrainzGenreActivity,
    ListenBrainzSimilarArtist,
    ListenBrainzFeedbackRecording,
    ListenBrainzRecommendationTrack,
    ListenBrainzRecommendationPlaylist,
    ALLOWED_STATS_RANGE,
    parse_artist,
    parse_release_group,
    parse_recording,
    parse_listen,
    parse_artist_recording,
    parse_feedback_recording,
    parse_similar_artist,
    parse_recommendation_track,
)
from models.library_management_genres import GenreCandidate
from infrastructure.service_health import report_breaker_health
from repositories.listenbrainz_management_models import (
    LbManagementReleaseGroupMetadata,
)
from infrastructure.degradation import try_get_degradation_context
from infrastructure.http.deduplication import RequestDeduplicator
from infrastructure.integration_result import IntegrationResult

_SOURCE = "listenbrainz"


def _record_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx is not None:
        ctx.record(IntegrationResult.error(source=_SOURCE, msg=msg))


_RATE_LIMIT_SAFETY_MARGIN_SECONDS = 0.5
_RATE_LIMIT_DEFAULT_COOLDOWN_SECONDS = 2.0
_RATE_LIMIT_MAX_DELAY_SECONDS = 3600.0
_RATE_LIMIT_HEALTH_MESSAGE = (
    "ListenBrainz is temporarily rate-limiting this server. Try again shortly."
)


def _parse_nonnegative_header(headers: Any, name: str) -> float | None:
    try:
        value = headers.get(name)
        if value is None:
            value = headers.get(name.lower())
    except Exception:  # noqa: BLE001 - malformed test/provider headers are ignored
        return None
    if value is None:
        return None
    try:
        parsed = float(str(value).strip())
    except (TypeError, ValueError):
        return None
    if not math.isfinite(parsed) or parsed < 0:
        return None
    return parsed


def _parse_retry_after(response: httpx.Response) -> float:
    """Extract retry delay from ListenBrainz 429 response headers."""
    headers = response.headers
    for header in ("X-RateLimit-Reset-In", "Retry-After"):
        seconds = _parse_nonnegative_header(headers, header)
        if seconds is not None and seconds > 0:
            return min(seconds, _RATE_LIMIT_MAX_DELAY_SECONDS)
    return _RATE_LIMIT_DEFAULT_COOLDOWN_SECONDS


def _mark_rate_limit_degraded(ttl_seconds: float) -> None:
    from infrastructure.service_health import service_health

    service_health.mark_degraded(
        "listenbrainz",
        "rate limit",
        message=_RATE_LIMIT_HEALTH_MESSAGE,
        severity="degraded",
        ttl_seconds=max(1.0, ttl_seconds + _RATE_LIMIT_SAFETY_MARGIN_SECONDS),
    )


def _heal_rate_limit() -> None:
    from infrastructure.service_health import service_health

    service_health.heal("listenbrainz", "rate limit")


class _ListenBrainzRateLimitState:
    """Process-global response-window reservation and cooldown state."""

    def __init__(self, clock: Callable[[], float] | None = None) -> None:
        self._lock = threading.Lock()
        self._clock = clock or (lambda: time.monotonic())
        self._window_reset_at: float | None = None
        self._remaining: float | None = None
        self._cooldown_until = 0.0
        self._unknown_in_flight = 0

    def _release_unknown_locked(self) -> None:
        if self._unknown_in_flight > 0:
            self._unknown_in_flight -= 1

    def _reserve_with_tracking(self) -> tuple[float | None, bool]:
        """Reserve an upstream slot and report whether its window is unknown."""
        now = self._clock()
        with self._lock:
            self._expire_locked(now)
            blocked_delay = self._blocked_delay_locked(now)
            if blocked_delay is not None:
                return blocked_delay, False
            if self._remaining is None:
                self._unknown_in_flight += 1
                return None, True
            self._remaining = max(0.0, self._remaining - 1.0)
            return None, False

    def _expire_locked(self, now: float) -> None:
        cooldown_expired = False
        if self._cooldown_until and self._cooldown_until <= now:
            self._cooldown_until = 0.0
            cooldown_expired = True

        window_expired = (
            self._window_reset_at is not None and self._window_reset_at <= now
        )
        if window_expired:
            self._window_reset_at = None
            self._remaining = None
        elif cooldown_expired and self._window_reset_at is None:
            # A fallback cooldown without a known upstream window has no
            # remaining budget to carry into the next admission.
            self._remaining = None

        if cooldown_expired and (
            window_expired
            or self._window_reset_at is None
            or (self._remaining is not None and self._remaining > 0)
        ):
            _heal_rate_limit()

    def _activate_cooldown_locked(self, now: float, seconds: float) -> None:
        if not math.isfinite(seconds) or seconds < 0:
            seconds = _RATE_LIMIT_DEFAULT_COOLDOWN_SECONDS
        seconds = min(seconds, _RATE_LIMIT_MAX_DELAY_SECONDS)
        until = now + seconds + _RATE_LIMIT_SAFETY_MARGIN_SECONDS
        if until > self._cooldown_until:
            self._cooldown_until = until
            _mark_rate_limit_degraded(until - now)

    def _reset_delay_locked(self, now: float) -> float:
        return max(
            0.0,
            self._window_reset_at - now
            if self._window_reset_at is not None
            else _RATE_LIMIT_DEFAULT_COOLDOWN_SECONDS,
        )

    def _blocked_delay_locked(self, now: float) -> float | None:
        if self._cooldown_until > now:
            return self._cooldown_until - now
        if self._remaining is None or self._remaining > 0:
            return None
        self._activate_cooldown_locked(now, self._reset_delay_locked(now))
        return max(self._cooldown_until - now, _RATE_LIMIT_SAFETY_MARGIN_SECONDS)

    def admission_delay(self) -> float | None:
        """Return a retry delay before pacing if the upstream window is exhausted."""
        now = self._clock()
        with self._lock:
            self._expire_locked(now)
            return self._blocked_delay_locked(now)

    def reserve(self) -> float | None:
        """Reserve an upstream slot, returning a safe retry delay if blocked."""
        blocked_delay, _unknown = self._reserve_with_tracking()
        return blocked_delay

    def release_unknown(self) -> None:
        """Release an unknown-window reservation after a transport failure."""
        with self._lock:
            self._release_unknown_locked()

    def observe(self, headers: Any, reservation_unknown: bool | None = None) -> None:
        """Merge a response's finite window headers conservatively.

        A response completes its own unknown-window reservation before the
        observed budget is merged.  Other unknown requests remain reserved so
        the first learned budget cannot be double-consumed.
        """
        remaining = _parse_nonnegative_header(headers, "X-RateLimit-Remaining")
        reset_in = _parse_nonnegative_header(headers, "X-RateLimit-Reset-In")
        if reset_in is not None and reset_in <= 0:
            reset_in = None

        now = self._clock()
        with self._lock:
            self._expire_locked(now)
            if reservation_unknown is None:
                reservation_unknown = self._unknown_in_flight > 0
            if reservation_unknown:
                self._release_unknown_locked()
            if remaining is None:
                return

            candidate_reset_at = (
                now + min(reset_in, _RATE_LIMIT_MAX_DELAY_SECONDS)
                if reset_in is not None
                else None
            )

            if candidate_reset_at is not None:
                if self._window_reset_at is None:
                    self._window_reset_at = candidate_reset_at
                else:
                    # Never let a delayed response from an older window shorten the
                    # active reset deadline.
                    self._window_reset_at = max(
                        self._window_reset_at, candidate_reset_at
                    )

            # Unknown requests are conservatively treated as consuming slots until
            # their own response or transport failure settles them.
            adjusted_remaining = max(0.0, remaining - self._unknown_in_flight)
            if self._remaining is None:
                self._remaining = adjusted_remaining
            else:
                # Retain local reservations as well as the lowest observed budget;
                # an incomplete earlier header must not regain slots later.
                self._remaining = min(self._remaining, adjusted_remaining)

            if adjusted_remaining <= 0:
                reset_delay = (
                    max(0.0, self._window_reset_at - now)
                    if self._window_reset_at is not None
                    else _RATE_LIMIT_DEFAULT_COOLDOWN_SECONDS
                )
                self._activate_cooldown_locked(now, reset_delay)

    def activate_cooldown(self, seconds: float) -> float:
        now = self._clock()
        with self._lock:
            self._expire_locked(now)
            self._activate_cooldown_locked(now, seconds)
            return max(self._cooldown_until - now, _RATE_LIMIT_SAFETY_MARGIN_SECONDS)

    def cooldown_active(self) -> bool:
        now = self._clock()
        with self._lock:
            self._expire_locked(now)
            # Use the same admission predicate so retained exhausted windows
            # re-establish their cooldown and health state before callers gate
            # work on this exported helper.
            return self._blocked_delay_locked(now) is not None

    def cooldown_remaining(self) -> float:
        now = self._clock()
        with self._lock:
            self._expire_locked(now)
            return max(0.0, self._cooldown_until - now)

    def reset(self) -> None:
        with self._lock:
            self._window_reset_at = None
            self._remaining = None
            self._cooldown_until = 0.0
            self._unknown_in_flight = 0
            self._clock = lambda: time.monotonic()
            _heal_rate_limit()


_listenbrainz_rate_limit_state = _ListenBrainzRateLimitState()


def listenbrainz_rate_limit_cooldown_active() -> bool:
    """Return whether the process-wide ListenBrainz cooldown is active."""
    return _listenbrainz_rate_limit_state.cooldown_active()


def _reset_listenbrainz_rate_limit_state() -> None:
    _listenbrainz_rate_limit_state.reset()


# LB popularity outages last hours; a short TTL would expire during any idle gap (no calls
# to re-mark it) and the NEXT build would wrongly take the dead LB path. Keep the flag alive
# well past idle gaps, and heal it INSTANTLY the moment a popularity call succeeds again.
_POPULARITY_DEGRADED_TTL = 1800.0  # 30 minutes


def _mark_popularity_degraded() -> None:
    """Flag LB popularity as genuinely degraded (drives fallbacks + the UI status
    dot). Only ever called on LB's own explicit "disabled"/"auth-gate" replies."""
    from infrastructure.service_health import service_health

    service_health.mark_degraded(
        "listenbrainz",
        "popularity",
        message="ListenBrainz's popularity data is temporarily unavailable.",
        fallback="lastfm",
        severity="degraded",
        ttl_seconds=_POPULARITY_DEGRADED_TTL,
    )


def _heal_popularity() -> None:
    """LB popularity answered successfully - clear the degraded flag immediately."""
    from infrastructure.service_health import service_health

    service_health.heal("listenbrainz", "popularity")


def lb_popularity_degraded() -> bool:
    """True ONLY when ListenBrainz's popularity API is DEFINITELY degraded - i.e. LB
    itself has recently returned an explicit outage response ("Popularity API currently
    disabled due to high load" 500, or the anti-scraper 401), recorded via
    _mark_popularity_degraded() with a sliding TTL that auto-heals. It is NOT set by
    timeouts, network blips, or empty results. Callers use this as the single, shared
    gate for 'may I fall back to Last.fm?' - the answer defaults to NO (prefer LB)."""
    from infrastructure.service_health import service_health

    return service_health.is_degraded("listenbrainz", "popularity")


def _is_upstream_policy_block(response: httpx.Response) -> bool:
    """LB deterministically refuses some endpoints for an outage's duration; these
    must fail fast (no retry storm) and NOT trip the shared breaker (they'd take
    down endpoints that still work, e.g. authenticated similar-artists):
      - popularity feature-flag 500 ("currently disabled due to high load"),
      - anti-scraper 401 ("...please provide an Auth token", added 2026-07 when LB
        began gating anonymous popularity calls) - retrying/tripping on this let one
        token-less caller open the shared breaker and blind every other LB feature.
    """
    text = response.text
    if response.status_code == 500 and "currently disabled" in text:
        return True
    if response.status_code == 401 and (
        "provide an Auth token" in text or "AI scrapers" in text
    ):
        return True
    return False


class _ListenBrainzAuthenticationError(ExternalServiceError):
    """Deterministic credential rejection that must never be retried."""


class _ListenBrainzValidationOutcome(Exception):
    """A validator's expected negative status, neutral to retry and breaker state."""

    def __init__(self, status_code: int) -> None:
        self.status_code = status_code
        super().__init__(f"ListenBrainz validation outcome ({status_code})")


_MAX_LISTENBRAINZ_TOKEN_LENGTH = 1024


def _is_header_safe_listenbrainz_token(token: object) -> bool:
    if not isinstance(token, str) or not token:
        return False
    if len(token) > _MAX_LISTENBRAINZ_TOKEN_LENGTH:
        return False
    return all(0x21 <= ord(character) <= 0x7E for character in token)


def _listenbrainz_endpoint_category(endpoint: str) -> str:
    path = endpoint.split("?", 1)[0]
    categories = (
        ("/validate-token", "token validation"),
        ("/popularity/", "popularity"),
        ("/metadata/", "metadata"),
        ("/feedback/", "feedback"),
        ("/submit-listens", "listen submission"),
        ("/playing-now", "now-playing"),
        ("/stats/", "statistics"),
        ("/user/", "user data"),
    )
    for marker, category in categories:
        if marker in path:
            return category
    return "request"


_listenbrainz_circuit_breaker = CircuitBreaker(
    failure_threshold=10,
    success_threshold=2,
    timeout=60.0,
    name="listenbrainz",
    on_state_change=report_breaker_health(
        "listenbrainz",
        "music data",
        message="ListenBrainz music data is temporarily unavailable.",
    ),
)

# Live edge evidence (2026-08-26): 30 requests/10 seconds. LB's API docs
# (https://listenbrainz.readthedocs.io/en/latest/users/api/#rate-limiting) make
# X-RateLimit-* dynamic, so keep the local baseline evenly paced with no burst.
_listenbrainz_rate_limiter = TokenBucketRateLimiter(rate=2.5, capacity=1)
_metadata_deduplicator = RequestDeduplicator()

LISTENBRAINZ_API_URL = "https://api.listenbrainz.org"

ListenBrainzJsonObject = dict[str, Any]
ListenBrainzJsonArray = list[ListenBrainzJsonObject]
ListenBrainzJson = ListenBrainzJsonObject | ListenBrainzJsonArray


def _decode_json_response(response: httpx.Response) -> ListenBrainzJson:
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray, memoryview)):
        return msgspec.json.decode(content, type=ListenBrainzJson)
    return response.json()


class ListenBrainzRepository:
    def __init__(
        self,
        http_client: httpx.AsyncClient,
        cache: CacheInterface,
        username: str = "",
        user_token: str = "",
        fallback_token_provider: "Callable[[], Awaitable[str | None]] | None" = None,
    ):
        self._client = http_client
        self._cache = cache
        self._username = username
        # Keep the configured token separate from any token borrowed for a public
        # read.  ``require_auth`` must only ever accept this repository's own token.
        self._user_token = user_token
        self._base_url = LISTENBRAINZ_API_URL
        self._request_semaphore = asyncio.Semaphore(2)
        # borrowed token for PUBLIC reads when this (usually global/enrichment) repo
        # has none of its own; LB now anti-scraper-gates anonymous popularity calls.
        # Resolved once, lazily, and NEVER used for require_auth writes.
        self._fallback_token_provider = fallback_token_provider
        self._fallback_resolved = False
        self._borrowed_read_token: str | None = None
        self._fallback_token_lock = asyncio.Lock()
        self._fallback_generation = 0

    def configure(self, username: str, user_token: str = "") -> None:
        self._username = username
        self._user_token = user_token
        self._fallback_generation += 1
        self._fallback_resolved = False
        self._borrowed_read_token = None

    async def _ensure_read_token(self) -> None:
        if (
            self._user_token
            or self._fallback_token_provider is None
            or self._fallback_resolved
        ):
            return

        while True:
            generation = self._fallback_generation
            async with self._fallback_token_lock:
                if (
                    self._user_token
                    or self._fallback_token_provider is None
                    or self._fallback_resolved
                ):
                    return
                try:
                    token = await self._fallback_token_provider()
                except Exception:  # noqa: BLE001 - a missing borrowed token means anonymous
                    token = None
                if generation != self._fallback_generation:
                    # A synchronous configure() replaced the credentials while the
                    # provider was running; never publish its stale result.
                    continue
                self._borrowed_read_token = token if token else None
                # Publish only after the provider call has completed.  A provider
                # failure is a completed anonymous resolution, not a leaked race.
                self._fallback_resolved = True
                return

    @staticmethod
    def reset_circuit_breaker() -> None:
        _listenbrainz_circuit_breaker.reset()

    def _get_headers(self) -> dict[str, str]:
        headers = {
            "Accept": "application/json",
            "Content-Type": "application/json",
        }
        token = self._user_token or self._borrowed_read_token
        if token:
            if not _is_header_safe_listenbrainz_token(token):
                raise _ListenBrainzAuthenticationError(
                    "ListenBrainz credentials rejected"
                )
            headers["Authorization"] = f"Token {token}"
        return headers

    def _require_own_token(self) -> None:
        if not self._user_token:
            raise _ListenBrainzAuthenticationError(
                "ListenBrainz user token required for this request"
            )
        if not _is_header_safe_listenbrainz_token(self._user_token):
            raise _ListenBrainzAuthenticationError("ListenBrainz credentials rejected")

    def _validate_read_token(self) -> None:
        token = self._user_token or self._borrowed_read_token
        if token and not _is_header_safe_listenbrainz_token(token):
            raise _ListenBrainzAuthenticationError("ListenBrainz credentials rejected")

    @with_retry(
        max_attempts=3,
        base_delay=1.0,
        max_delay=3.0,
        circuit_breaker=_listenbrainz_circuit_breaker,
        retriable_exceptions=(
            httpx.HTTPError,
            ExternalServiceError,
            _ListenBrainzValidationOutcome,
        ),
        non_breaking_exceptions=(
            RateLimitedError,
            _ListenBrainzAuthenticationError,
            _ListenBrainzValidationOutcome,
        ),
        non_retriable_exceptions=(
            RateLimitedError,
            _ListenBrainzAuthenticationError,
            _ListenBrainzValidationOutcome,
        ),
    )
    async def _request(
        self,
        method: str,
        endpoint: str,
        params: dict[str, Any] | None = None,
        json_data: dict[str, Any] | None = None,
        require_auth: bool = False,
        accepted_statuses: tuple[int, ...] = (),
    ) -> Any:
        url = f"{self._base_url}{endpoint}"

        # Credential preparation belongs to the _get/_post boundaries.  Those
        # helpers run before this retry/circuit-breaker wrapper is entered, so
        # malformed read tokens cannot consult or affect breaker state.
        if require_auth:
            # Keep this as a defensive guard for any future internal caller;
            # normal require_auth calls are already rejected by their boundary.
            self._require_own_token()

        # A borrowed fallback token authenticates public reads only; writes must
        # use this repo's own (real user) token, never someone else's.
        # _get/_post perform the read-token resolution and validation before
        # entering this decorated method.

        admission_delay = _listenbrainz_rate_limit_state.admission_delay()
        if admission_delay is not None:
            raise RateLimitedError(
                _RATE_LIMIT_HEALTH_MESSAGE,
                retry_after_seconds=admission_delay,
            )

        async with self._request_semaphore:
            # Pace only requests that have won a wire-attempt slot.  Waiting for
            # this semaphore must not consume the capacity-1 pacing token.
            await _listenbrainz_rate_limiter.acquire()

            # Reserve immediately before the wire attempt.  A request can wait
            # for this semaphore while another request observes a 429 and opens
            # the shared cooldown; checking here prevents it from bypassing that
            # newly activated window.
            cooldown_remaining, reservation_unknown = (
                _listenbrainz_rate_limit_state._reserve_with_tracking()
            )
            if cooldown_remaining is not None:
                raise RateLimitedError(
                    _RATE_LIMIT_HEALTH_MESSAGE,
                    retry_after_seconds=cooldown_remaining,
                )

            try:
                response = await self._client.request(
                    method,
                    url,
                    headers=self._get_headers(),
                    params=params,
                    json=json_data,
                    timeout=15.0,
                )
                # Complete this request's reservation before merging headers so
                # only other unknown in-flight requests are subtracted.
                _listenbrainz_rate_limit_state.observe(
                    response.headers,
                    reservation_unknown=reservation_unknown,
                )
                reservation_unknown = False
                record_rate_limit_headers("listenbrainz", response.headers)

                # QW9 Part 3: one increment per wire attempt, classified from
                # the status; this funnel has no priority lane -> "unlaned"
                record_provider_call("listenbrainz", None, response.status_code)
                if response.status_code == 204:
                    return None

                if response.status_code == 429:
                    retry_after = _listenbrainz_rate_limit_state.activate_cooldown(
                        _parse_retry_after(response)
                    )
                    raise RateLimitedError(
                        _RATE_LIMIT_HEALTH_MESSAGE,
                        retry_after_seconds=retry_after,
                    )

                if response.status_code != 200:
                    # Validators explicitly classify a few deterministic statuses
                    # as ordinary validation results.  Keep those statuses out of
                    # retry/breaker handling, even if a provider error body happens
                    # to contain policy-block wording.
                    if response.status_code in accepted_statuses:
                        raise _ListenBrainzValidationOutcome(response.status_code)
                    # deterministic upstream policy blocks (disabled-under-load 500,
                    # anti-scraper 401): fail fast and keep the shared LB breaker
                    # closed for endpoints that still work
                    if _is_upstream_policy_block(response):
                        _mark_popularity_degraded()
                        category = _listenbrainz_endpoint_category(endpoint)
                        raise ServiceDisabledUpstreamError(
                            f"ListenBrainz {method} {category} endpoint unavailable "
                            f"upstream ({response.status_code})"
                        )
                    if response.status_code in (401, 403):
                        raise _ListenBrainzAuthenticationError(
                            f"ListenBrainz credentials rejected ({response.status_code})"
                        )
                    category = _listenbrainz_endpoint_category(endpoint)
                    raise ExternalServiceError(
                        f"ListenBrainz {method} {category} request failed "
                        f"({response.status_code})"
                    )

                # a 200 from a popularity endpoint means LB popularity recovered - heal now
                # so the degraded flag doesn't linger for the full TTL after LB comes back
                if "/popularity/" in endpoint:
                    _heal_popularity()

                try:
                    return _decode_json_response(response)
                except (msgspec.DecodeError, ValueError, TypeError):
                    _record_degradation(
                        f"ListenBrainz returned invalid JSON for {method} "
                        f"{_listenbrainz_endpoint_category(endpoint)}"
                    )
                    return None

            except httpx.HTTPError:
                record_provider_call("listenbrainz", None, None)
                category = _listenbrainz_endpoint_category(endpoint)
                raise ExternalServiceError(
                    f"ListenBrainz {method} {category} request failed during transport"
                ) from None
            finally:
                if reservation_unknown:
                    # A transport failure has no response to complete the
                    # reservation.  Release only its in-flight marker; the
                    # consumed budget remains conservative because the request
                    # may have reached ListenBrainz.
                    _listenbrainz_rate_limit_state.release_unknown()

    async def _get(
        self,
        endpoint: str,
        params: dict[str, Any] | None = None,
        require_auth: bool = False,
        accepted_statuses: tuple[int, ...] = (),
    ) -> Any:
        if require_auth:
            self._require_own_token()
        else:
            await self._ensure_read_token()
            self._validate_read_token()
        return await self._request(
            "GET",
            endpoint,
            params=params,
            require_auth=require_auth,
            accepted_statuses=accepted_statuses,
        )

    async def _post(
        self, endpoint: str, data: dict[str, Any], require_auth: bool = False
    ) -> Any:
        if require_auth:
            self._require_own_token()
        else:
            await self._ensure_read_token()
            self._validate_read_token()
        return await self._request(
            "POST", endpoint, json_data=data, require_auth=require_auth
        )

    async def validate_username(self, username: str | None = None) -> tuple[bool, str]:
        user = username or self._username
        if not user:
            return False, "No username provided"

        try:
            result = await self._get(
                f"/1/user/{user}/listen-count",
                accepted_statuses=(404,),
            )
            if result is None:
                return False, f"User '{user}' not found"
            if isinstance(result, dict) and "payload" in result:
                payload = result.get("payload")
                if isinstance(payload, dict):
                    count = payload.get("count", 0)
                    return True, f"User found with {count:,} listens"
            return False, "User not found"
        except _ListenBrainzValidationOutcome:
            return False, f"User '{user}' not found"
        except RateLimitedError:
            raise
        except CircuitOpenError:
            return False, "ListenBrainz is temporarily unavailable. Try again shortly."
        except _ListenBrainzAuthenticationError:
            return False, "ListenBrainz could not validate this username."
        except (ServiceDisabledUpstreamError, ExternalServiceError):
            return False, "ListenBrainz is temporarily unavailable. Try again shortly."
        except httpx.TimeoutException:
            return False, "Connection timed out"
        except httpx.ConnectError:
            return False, "Could not connect to ListenBrainz"
        except Exception:  # noqa: BLE001 - validation must not leak provider details
            return False, "ListenBrainz is temporarily unavailable. Try again shortly."

    async def validate_token(self) -> tuple[bool, str]:
        if not self._user_token:
            return False, "No token provided"

        try:
            result = await self._get(
                "/1/validate-token",
                accepted_statuses=(401, 403),
            )
            if isinstance(result, dict) and result.get("valid"):
                username = result.get("user_name", self._username)
                return True, f"Successfully connected as '{username}'"
            return False, "Token invalid or expired"
        except _ListenBrainzValidationOutcome:
            return False, "Token invalid or expired"
        except RateLimitedError:
            raise
        except CircuitOpenError:
            return False, "ListenBrainz is temporarily unavailable. Try again shortly."
        except _ListenBrainzAuthenticationError:
            return False, "Token invalid or expired"
        except (ServiceDisabledUpstreamError, ExternalServiceError):
            return False, "ListenBrainz is temporarily unavailable. Try again shortly."
        except httpx.TimeoutException:
            return False, "Connection timed out"
        except httpx.ConnectError:
            return False, "Could not connect to ListenBrainz"
        except Exception:  # noqa: BLE001 - validation must not leak provider details
            return False, "ListenBrainz is temporarily unavailable. Try again shortly."

    async def get_user_listens(
        self,
        username: str | None = None,
        count: int = 25,
        max_ts: int | None = None,
        min_ts: int | None = None,
    ) -> list[ListenBrainzListen]:
        user = username or self._username
        if not user:
            return []

        params: dict[str, Any] = {"count": min(count, 100)}
        if max_ts:
            params["max_ts"] = max_ts
        if min_ts:
            params["min_ts"] = min_ts

        result = await self._get(f"/1/user/{user}/listens", params=params)
        if not result:
            return []
        return [
            parse_listen(item) for item in result.get("payload", {}).get("listens", [])
        ]

    async def get_user_loved_recordings(
        self,
        username: str | None = None,
        count: int = 25,
        offset: int = 0,
    ) -> list[ListenBrainzFeedbackRecording]:
        user = username or self._username
        if not user:
            return []

        cache_key = f"{LB_PREFIX}user_loved_recordings:{user}:{count}:{offset}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        params: dict[str, Any] = {
            "score": 1,
            "count": min(count, 100),
            "offset": offset,
            "metadata": "true",
        }
        result = await self._get(f"/1/feedback/user/{user}/get-feedback", params=params)
        if not result:
            return []

        payload = result.get("payload", result)
        feedback_items: list[dict[str, Any]]
        if isinstance(payload, dict):
            feedback_raw = payload.get("feedback") or payload.get("recordings") or []
            if isinstance(feedback_raw, list):
                feedback_items = [
                    item for item in feedback_raw if isinstance(item, dict)
                ]
            else:
                feedback_items = []
        elif isinstance(payload, list):
            feedback_items = [item for item in payload if isinstance(item, dict)]
        else:
            feedback_items = []

        loved_recordings = [parse_feedback_recording(item) for item in feedback_items]
        if loved_recordings:
            await self._cache.set(cache_key, loved_recordings, ttl_seconds=300)
        return loved_recordings

    async def get_user_top_artists(
        self,
        username: str | None = None,
        range_: str = "this_month",
        count: int = 25,
        offset: int = 0,
    ) -> list[ListenBrainzArtist]:
        user = username or self._username
        if not user:
            return []

        if range_ not in ALLOWED_STATS_RANGE:
            range_ = "this_month"

        cache_key = f"{LB_PREFIX}user_artists:{user}:{range_}:{count}:{offset}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        params = {"count": min(count, 100), "offset": offset, "range": range_}
        result = await self._get(f"/1/stats/user/{user}/artists", params=params)
        if not result:
            return []
        artists = [
            parse_artist(item) for item in result.get("payload", {}).get("artists", [])
        ]
        if artists:
            await self._cache.set(cache_key, artists, ttl_seconds=300)
        return artists

    async def get_user_top_release_groups(
        self,
        username: str | None = None,
        range_: str = "this_month",
        count: int = 25,
        offset: int = 0,
    ) -> list[ListenBrainzReleaseGroup]:
        user = username or self._username
        if not user:
            return []

        if range_ not in ALLOWED_STATS_RANGE:
            range_ = "this_month"

        cache_key = f"{LB_PREFIX}user_release_groups:{user}:{range_}:{count}:{offset}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        params = {"count": min(count, 100), "offset": offset, "range": range_}
        result = await self._get(f"/1/stats/user/{user}/release-groups", params=params)
        if not result:
            return []
        groups = [
            parse_release_group(item)
            for item in result.get("payload", {}).get("release_groups", [])
        ]
        if groups:
            await self._cache.set(cache_key, groups, ttl_seconds=300)
        return groups

    async def get_user_top_recordings(
        self,
        username: str | None = None,
        range_: str = "this_month",
        count: int = 25,
        offset: int = 0,
    ) -> list[ListenBrainzRecording]:
        user = username or self._username
        if not user:
            return []

        if range_ not in ALLOWED_STATS_RANGE:
            range_ = "this_month"

        params = {"count": min(count, 100), "offset": offset, "range": range_}
        result = await self._get(f"/1/stats/user/{user}/recordings", params=params)
        if not result:
            return []
        return [
            parse_recording(item)
            for item in result.get("payload", {}).get("recordings", [])
        ]

    async def _stale_chart(self, cache_key: str, label: str) -> list:
        """QW11 Part 3: breaker-open fallback for chart/stats getters.

        Serves the expired entry past its TTL (peek reads without TTL
        enforcement) so home/discover render last-known-good rankings instead
        of empty sections during an LB outage. AGENTS.md Errors rule: a
        fallback WITHOUT a degradation record is the anti-pattern, so the
        staleness is always recorded. When no stale entry exists either the
        original CircuitOpenError propagates - callers keep their current
        empty-render behavior.
        """
        stale = await self._cache.peek(cache_key)
        if stale:
            _record_degradation(f"Circuit open: serving stale ListenBrainz {label}")
            return stale
        raise CircuitOpenError(
            f"Circuit breaker 'listenbrainz' is OPEN and no stale data for {label}",
            breaker_name="listenbrainz",
        )

    async def get_user_genre_activity(
        self, username: str | None = None
    ) -> list[ListenBrainzGenreActivity]:
        user = username or self._username
        if not user:
            return []

        cache_key = f"{LB_PREFIX}user_genres:{user}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        try:
            result = await self._get(f"/1/stats/user/{user}/genre-activity")
        except CircuitOpenError:
            return await self._stale_chart(cache_key, f"user genre activity ({user})")

        if not result:
            return []

        genre_counts: dict[str, int] = {}
        for item in result.get("result", []):
            genre = item.get("genre", "Unknown")
            count = item.get("listen_count", 0)
            genre_counts[genre] = genre_counts.get(genre, 0) + count

        genres = [
            ListenBrainzGenreActivity(genre=g, listen_count=c)
            for g, c in sorted(genre_counts.items(), key=lambda x: -x[1])
        ]

        if genres:
            await self._cache.set(cache_key, genres, ttl_seconds=300)
        return genres

    async def get_sitewide_top_artists(
        self, range_: str = "week", count: int = 25, offset: int = 0
    ) -> list[ListenBrainzArtist]:
        if range_ not in ALLOWED_STATS_RANGE:
            range_ = "week"

        cache_key = f"{LB_PREFIX}sitewide_artists:{range_}:{count}:{offset}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        params = {"count": min(count, 100), "offset": offset, "range": range_}
        try:
            result = await self._get("/1/stats/sitewide/artists", params=params)
        except CircuitOpenError:
            return await self._stale_chart(
                cache_key, f"sitewide top artists ({range_})"
            )
        if not result:
            return []
        artists = [
            parse_artist(item) for item in result.get("payload", {}).get("artists", [])
        ]
        if artists:
            await self._cache.set(cache_key, artists, ttl_seconds=3600)
        return artists

    async def get_sitewide_top_release_groups(
        self, range_: str = "week", count: int = 25, offset: int = 0
    ) -> list[ListenBrainzReleaseGroup]:
        if range_ not in ALLOWED_STATS_RANGE:
            range_ = "week"

        cache_key = f"{LB_PREFIX}sitewide_release_groups:{range_}:{count}:{offset}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        params = {"count": min(count, 100), "offset": offset, "range": range_}
        try:
            result = await self._get("/1/stats/sitewide/release-groups", params=params)
        except CircuitOpenError:
            return await self._stale_chart(
                cache_key, f"sitewide top release-groups ({range_})"
            )
        if not result:
            return []
        groups = [
            parse_release_group(item)
            for item in result.get("payload", {}).get("release_groups", [])
        ]
        if groups:
            await self._cache.set(cache_key, groups, ttl_seconds=3600)
        return groups

    async def get_sitewide_top_recordings(
        self, range_: str = "week", count: int = 25, offset: int = 0
    ) -> list[ListenBrainzRecording]:
        if range_ not in ALLOWED_STATS_RANGE:
            range_ = "week"

        cache_key = f"{LB_PREFIX}sitewide_recordings:{range_}:{count}:{offset}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        params = {"count": min(count, 100), "offset": offset, "range": range_}
        try:
            result = await self._get("/1/stats/sitewide/recordings", params=params)
        except CircuitOpenError:
            return await self._stale_chart(
                cache_key, f"sitewide top recordings ({range_})"
            )
        if not result:
            return []
        recordings = [
            parse_recording(item)
            for item in result.get("payload", {}).get("recordings", [])
        ]
        if recordings:
            await self._cache.set(cache_key, recordings, ttl_seconds=3600)
        return recordings

    async def get_artist_top_recordings(
        self, artist_mbid: str, count: int = 10
    ) -> list[ListenBrainzRecording]:
        cache_key = f"{LB_PREFIX}artist_recordings:{artist_mbid}:{count}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        # This outage is capability-wide, not artist-specific. Once LB explicitly
        # disables popularity, let the shared health TTL expire before probing again.
        if lb_popularity_degraded():
            _record_degradation("ListenBrainz popularity is temporarily unavailable")
            return []

        result = await self._get(
            f"/1/popularity/top-recordings-for-artist/{artist_mbid}"
        )
        if not result:
            return []
        recordings = [parse_artist_recording(item) for item in result[:count]]
        if recordings:
            await self._cache.set(cache_key, recordings, ttl_seconds=3600)
        return recordings

    async def get_recording_release_groups_batch(
        self,
        recording_mbids: list[str],
    ) -> dict[str, str]:
        """Resolve recordings to release groups through ListenBrainz metadata.

        Live-verified against ListenBrainz 2026-07-17: POST /1/metadata/recording/
        with ``inc=release`` returns an object keyed by recording MBID whose release
        object carries ``release_group_mbid``.
        """
        unique_mbids = list(dict.fromkeys(mbid for mbid in recording_mbids if mbid))
        if not unique_mbids:
            return {}

        resolved: dict[str, str] = {}
        pending: list[str] = []
        for mbid in unique_mbids:
            cache_key = f"{LB_PREFIX}recording_release_group:{mbid}"
            cached = await self._cache.get(cache_key)
            if cached is None:
                pending.append(mbid)
            elif cached:
                resolved[mbid] = cached

        pending.sort()
        for start in range(0, len(pending), 50):
            batch = pending[start : start + 50]
            dedupe_key = "listenbrainz:recording-metadata:" + ",".join(batch)
            result = await _metadata_deduplicator.dedupe(
                dedupe_key,
                lambda batch=batch: self._post(
                    "/1/metadata/recording/",
                    {"recording_mbids": batch, "inc": "release"},
                ),
            )
            if not isinstance(result, dict):
                _record_degradation("ListenBrainz returned no recording metadata")
                continue
            payload = result
            for mbid in batch:
                metadata = payload.get(mbid)
                release = (
                    metadata.get("release") if isinstance(metadata, dict) else None
                )
                release_group_mbid = (
                    release.get("release_group_mbid")
                    if isinstance(release, dict)
                    else None
                )
                cache_value = (
                    release_group_mbid if isinstance(release_group_mbid, str) else ""
                )
                await self._cache.set(
                    f"{LB_PREFIX}recording_release_group:{mbid}",
                    cache_value,
                    ttl_seconds=86400,
                )
                if cache_value:
                    resolved[mbid] = cache_value

        return resolved

    async def get_release_group_genres_batch(
        self, release_group_mbids: list[str]
    ) -> dict[str, tuple[GenreCandidate, ...]]:
        """Fetch live-verified GET-only release-group genre metadata.

        Verified against production on 2026-07-21; see
        ``listenbrainz_MANAGEMENT_API_NOTES.md``. The local 25-ID ceiling bounds
        request URLs and response work and is not an asserted upstream limit.
        """
        unique_mbids = list(
            dict.fromkeys(
                value.strip() for value in release_group_mbids if value.strip()
            )
        )
        if len(unique_mbids) > 500:
            raise ValueError("ListenBrainz genre lookup accepts at most 500 IDs.")
        resolved: dict[str, tuple[GenreCandidate, ...]] = {}
        pending: list[str] = []
        for mbid in unique_mbids:
            cache_key = listenbrainz_management_genres_key(mbid)
            cached = await self._cache.get(cache_key)
            if isinstance(cached, tuple):
                resolved[mbid] = cached
            else:
                pending.append(mbid)

        pending.sort()
        for start in range(0, len(pending), 25):
            batch = pending[start : start + 25]
            dedupe_key = "listenbrainz:management:release-group-genres:" + ",".join(
                batch
            )
            result = await _metadata_deduplicator.dedupe(
                dedupe_key,
                lambda batch=batch: self._get(
                    "/1/metadata/release_group/",
                    params={
                        "release_group_mbids": ",".join(batch),
                        "inc": "artist tag release",
                    },
                ),
            )
            if result is None:
                raise ExternalServiceError(
                    "ListenBrainz returned no release-group metadata."
                )
            try:
                decoded = msgspec.convert(
                    result,
                    type=dict[str, LbManagementReleaseGroupMetadata],
                )
            except (msgspec.ValidationError, TypeError, ValueError) as error:
                _record_degradation(
                    "ListenBrainz returned invalid release-group metadata"
                )
                raise ExternalServiceError(
                    "ListenBrainz returned invalid release-group metadata."
                ) from error

            fetched_at = time.time()
            for mbid in batch:
                metadata = decoded.get(mbid)
                tagged_values = (
                    tuple(
                        ("release_group", value) for value in metadata.tag.release_group
                    )
                    + tuple(("artist", value) for value in metadata.tag.artist)
                    if metadata is not None
                    else ()
                )
                revision_material = "|".join(
                    f"{value.tag}:{value.count}:{value.genre_mbid or ''}"
                    for _entity, value in tagged_values
                )
                revision = hashlib.sha256(revision_material.encode()).hexdigest()
                candidates = tuple(
                    GenreCandidate(
                        display_name=value.tag,
                        folded_name=" ".join(value.tag.split()).casefold(),
                        provider="listenbrainz",
                        provider_entity=entity,
                        genre_mbid=value.genre_mbid,
                        count=value.count,
                        curated=bool(value.genre_mbid),
                        fetched_at=fetched_at,
                        source_document_revision=revision,
                    )
                    for entity, value in tagged_values
                    if value.tag
                )
                resolved[mbid] = candidates
                await self._cache.set(
                    listenbrainz_management_genres_key(mbid),
                    candidates,
                    ttl_seconds=3600,
                )
        return resolved

    async def get_similar_users(
        self, username: str | None = None
    ) -> list[dict[str, Any]]:
        user = username or self._username
        if not user:
            return []

        result = await self._get(f"/1/user/{user}/similar-users")

        if not result:
            return []

        return result.get("payload", [])

    async def get_user_fresh_releases(
        self, username: str | None = None, past: bool = True, future: bool = False
    ) -> list[dict[str, Any]]:
        user = username or self._username
        if not user:
            return []

        cache_key = f"{LB_PREFIX}fresh_releases:{user}:{past}:{future}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        params = {"past": str(past).lower(), "future": str(future).lower()}
        result = await self._get(f"/1/user/{user}/fresh_releases", params=params)

        if not result:
            return []

        releases = result.get("payload", {}).get("releases", [])
        if releases:
            await self._cache.set(cache_key, releases, ttl_seconds=3600)
        return releases

    async def get_similar_artists(
        self, artist_mbid: str, max_similar: int = 15, mode: str = "easy"
    ) -> list[ListenBrainzSimilarArtist]:
        cache_key = f"{LB_PREFIX}similar_artists:{artist_mbid}:{max_similar}:{mode}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        params = {
            "mode": mode,
            "max_similar_artists": max_similar,
            "max_recordings_per_artist": 5,
            "pop_begin": 0,
            "pop_end": 100,
        }
        result = await self._get(f"/1/lb-radio/artist/{artist_mbid}", params=params)
        if not result or "error" in result:
            return []

        similar_artists: list[ListenBrainzSimilarArtist] = []
        for mbid, recordings in result.items():
            if mbid == artist_mbid:
                continue
            if not isinstance(recordings, list):
                continue
            similar_artists.append(parse_similar_artist(mbid, recordings))

        similar_artists.sort(key=lambda a: a.listen_count, reverse=True)
        if similar_artists:
            await self._cache.set(cache_key, similar_artists, ttl_seconds=3600)
        return similar_artists

    async def get_artist_top_release_groups(
        self, artist_mbid: str, count: int = 10
    ) -> list[ListenBrainzReleaseGroup]:
        cache_key = f"{LB_PREFIX}artist_release_groups:{artist_mbid}:{count}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        if lb_popularity_degraded():
            _record_degradation("ListenBrainz popularity is temporarily unavailable")
            return []

        result = await self._get(
            f"/1/popularity/top-release-groups-for-artist/{artist_mbid}"
        )
        if not result or not isinstance(result, list):
            return []

        release_groups = []
        for item in result[:count]:
            rg = item.get("release_group", {})
            release_groups.append(
                ListenBrainzReleaseGroup(
                    release_group_name=rg.get("name", "Unknown"),
                    artist_name=item.get("artist", {}).get("name", "Unknown"),
                    listen_count=item.get("total_listen_count", 0),
                    release_group_mbid=item.get("release_group_mbid"),
                    caa_release_mbid=rg.get("caa_release_mbid"),
                    caa_id=rg.get("caa_id"),
                )
            )

        if release_groups:
            await self._cache.set(cache_key, release_groups, ttl_seconds=3600)
        return release_groups

    async def get_release_group_popularity_batch(
        self, release_group_mbids: list[str]
    ) -> dict[str, int]:
        """Get listen counts for multiple release groups in a single call.

        B4 Change 2: per-MBID cache-aside in front of the popularity POST.
        Keys ``{LB_PREFIX}rg_popularity:{mbid}`` join listenbrainz_prefixes()
        sweeps with zero new wiring. TTL 3600 s mirrors the sibling artist
        popularity cache; MBIDs absent from a successful response are
        negative-cached with the house False sentinel at 300 s so obscure
        release groups don't re-POST on every view.

        Poisoning guards - an outage must never read as "zero listens":
        nothing is written when lb_popularity_degraded() short-circuits,
        when the POST raises, or when the response is not a well-formed list.
        Concurrent identical batches share one leader via
        _metadata_deduplicator (recording-metadata precedent).
        """
        if not release_group_mbids:
            return {}

        unique_mbids = list(dict.fromkeys(release_group_mbids))
        keys = {mbid: f"{LB_PREFIX}rg_popularity:{mbid}" for mbid in unique_mbids}

        cached_values = await asyncio.gather(
            *(self._cache.get(key) for key in keys.values())
        )
        counts: dict[str, int] = {}
        misses: list[str] = []
        for mbid, cached in zip(unique_mbids, cached_values):
            if cached is None:
                misses.append(mbid)
            elif isinstance(cached, int) and not isinstance(cached, bool):
                counts[mbid] = cached
            # False sentinel: known-absent within its short TTL - no value,
            # and no reason to hit the wire again yet.

        if not misses:
            return counts

        if lb_popularity_degraded():
            _record_degradation("ListenBrainz popularity is temporarily unavailable")
            return {}

        sorted_misses = sorted(misses)
        dedupe_key = "listenbrainz:rg-popularity:" + ",".join(sorted_misses)
        result = await _metadata_deduplicator.dedupe(
            dedupe_key,
            lambda: self._post(
                "/1/popularity/release-group", {"release_group_mbids": sorted_misses}
            ),
        )
        if result is None or not isinstance(result, list):
            # Malformed/absent payload = outage signal, never "zero listens":
            # write nothing. A well-formed empty LIST is legitimate - it
            # negative-caches all misses below.
            return counts

        found: list[str] = []
        for item in result:
            mbid = item.get("release_group_mbid")
            count = item.get("total_listen_count")
            if mbid and count is not None:
                counts[mbid] = count
                found.append(mbid)

        await asyncio.gather(
            *(
                self._cache.set(keys[mbid], counts[mbid], ttl_seconds=3600)
                for mbid in found
            ),
            *(
                self._cache.set(keys[mbid], False, ttl_seconds=300)
                for mbid in sorted_misses
                if mbid not in counts
            ),
        )
        return counts

    def is_configured(self) -> bool:
        return bool(self._username)

    async def submit_now_playing(
        self,
        artist_name: str,
        track_name: str,
        release_name: str = "",
        duration_ms: int = 0,
    ) -> bool:
        track_metadata: dict[str, Any] = {
            "artist_name": artist_name,
            "track_name": track_name,
        }
        if release_name:
            track_metadata["release_name"] = release_name
        if duration_ms > 0:
            track_metadata["additional_info"] = {"duration_ms": duration_ms}

        payload = {
            "listen_type": "playing_now",
            "payload": [{"track_metadata": track_metadata}],
        }
        await self._post("/1/submit-listens", payload, require_auth=True)
        return True

    async def submit_single_listen(
        self,
        artist_name: str,
        track_name: str,
        listened_at: int,
        release_name: str = "",
        duration_ms: int = 0,
    ) -> bool:
        track_metadata: dict[str, Any] = {
            "artist_name": artist_name,
            "track_name": track_name,
        }
        if release_name:
            track_metadata["release_name"] = release_name
        if duration_ms > 0:
            track_metadata["additional_info"] = {"duration_ms": duration_ms}

        payload = {
            "listen_type": "single",
            "payload": [
                {
                    "listened_at": listened_at,
                    "track_metadata": track_metadata,
                }
            ],
        }
        await self._post("/1/submit-listens", payload, require_auth=True)
        return True

    async def get_recommendation_playlists(
        self, username: str | None = None
    ) -> list[dict[str, Any]]:
        user = username or self._username
        if not user:
            return []

        cache_key = f"{LB_PREFIX}rec_playlists:{user}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        # 404 means the user has no recommendation playlists yet: absence, not
        # failure (same accepted_statuses pattern as validate_username).
        try:
            result = await self._get(
                f"/1/user/{user}/playlists/recommendations", accepted_statuses=(404,)
            )
        except _ListenBrainzValidationOutcome:
            return []
        if not result or not isinstance(result, dict):
            return []

        playlists_raw = result.get("playlists", [])
        playlists: list[dict[str, Any]] = []
        for entry in playlists_raw:
            pl = entry.get("playlist", {})
            if not isinstance(pl, dict):
                continue

            identifier = pl.get("identifier", "")
            playlist_id = identifier.rsplit("/", 1)[-1] if identifier else ""
            if not playlist_id:
                continue

            ext = pl.get("extension", {})
            mb_ext = ext.get("https://musicbrainz.org/doc/jspf#playlist", {})
            algo = mb_ext.get("additional_metadata", {}).get("algorithm_metadata", {})

            playlists.append(
                {
                    "playlist_id": playlist_id,
                    "identifier": identifier,
                    "title": pl.get("title", ""),
                    "date": pl.get("date", ""),
                    "source_patch": algo.get("source_patch", ""),
                }
            )

        if playlists:
            await self._cache.set(cache_key, playlists, ttl_seconds=21600)
        return playlists

    async def get_playlist_tracks(
        self, playlist_id: str
    ) -> ListenBrainzRecommendationPlaylist | None:
        if not playlist_id:
            return None

        cache_key = f"{LB_PREFIX}rec_playlist:{playlist_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        # 404 means the playlist no longer exists upstream: absence, not
        # failure (same accepted_statuses pattern as validate_username).
        try:
            result = await self._get(
                f"/1/playlist/{playlist_id}", accepted_statuses=(404,)
            )
        except _ListenBrainzValidationOutcome:
            return None
        if not result or not isinstance(result, dict):
            return None

        pl = result.get("playlist", {})
        if not isinstance(pl, dict):
            return None

        ext = pl.get("extension", {})
        mb_ext = ext.get("https://musicbrainz.org/doc/jspf#playlist", {})
        algo = mb_ext.get("additional_metadata", {}).get("algorithm_metadata", {})

        tracks: list[ListenBrainzRecommendationTrack] = []
        for raw_track in pl.get("track", []):
            parsed = parse_recommendation_track(raw_track)
            if parsed:
                tracks.append(parsed)

        playlist = ListenBrainzRecommendationPlaylist(
            identifier=pl.get("identifier", ""),
            title=pl.get("title", ""),
            date=pl.get("date", ""),
            source_patch=algo.get("source_patch", ""),
            tracks=tracks,
        )

        if tracks:
            await self._cache.set(cache_key, playlist, ttl_seconds=21600)

        return playlist
