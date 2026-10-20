"""Bounded, identity-aware rate limiting for the compatibility APIs."""

from __future__ import annotations

import time
from collections import OrderedDict, deque
from dataclasses import dataclass, field
from typing import Callable, Generic, TypeVar

from starlette.responses import Response

from api.compat.subsonic.serialization import render_error
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter

_MAX_PRINCIPALS = 10_000
_MAX_IPS = 10_000
_ENTRY_TTL_SECONDS = 15 * 60.0
_AUTH_FAILURE_WINDOW_SECONDS = 60.0
_AUTH_FAILURE_LIMIT = 5
_AUTH_INITIAL_COOLDOWN_SECONDS = 10.0
_AUTH_MAX_COOLDOWN_SECONDS = 5 * 60.0

_T = TypeVar("_T")


def trusted_client_ip(request) -> str:
    """Use only the address established by Uvicorn's trusted-proxy middleware."""
    return request.client.host if request.client else "unknown"


class _BoundedTTLMap(Generic[_T]):
    def __init__(
        self,
        *,
        max_entries: int,
        ttl_seconds: float,
        factory: Callable[[], _T],
        clock: Callable[[], float],
    ) -> None:
        self._max_entries = max_entries
        self._ttl_seconds = ttl_seconds
        self._factory = factory
        self._clock = clock
        self._items: OrderedDict[str, tuple[float, _T]] = OrderedDict()

    def get(self, key: str) -> _T:
        now = self._clock()
        self._evict_expired(now)
        current = self._items.pop(key, None)
        value = current[1] if current else self._factory()
        self._items[key] = (now, value)
        if len(self._items) > self._max_entries:
            self._items.popitem(last=False)
        return value

    def discard(self, key: str) -> None:
        self._items.pop(key, None)

    def clear(self) -> None:
        self._items.clear()

    def _evict_expired(self, now: float) -> None:
        cutoff = now - self._ttl_seconds
        while self._items:
            _, (last_seen, _) = next(iter(self._items.items()))
            if last_seen > cutoff:
                break
            self._items.popitem(last=False)

    def __len__(self) -> int:
        self._evict_expired(self._clock())
        return len(self._items)

    def keys(self) -> tuple[str, ...]:
        self._evict_expired(self._clock())
        return tuple(self._items)


@dataclass
class _AuthFailureState:
    events: deque[float] = field(default_factory=deque)
    blocked_until: float = 0.0
    strikes: int = 0


class CompatRateLimitState:
    """Separate public-IP, principal browse/mutation, and auth-failure state."""

    def __init__(
        self,
        *,
        max_principals: int = _MAX_PRINCIPALS,
        max_ips: int = _MAX_IPS,
        ttl_seconds: float = _ENTRY_TTL_SECONDS,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._clock = clock
        self._public_by_ip = _BoundedTTLMap(
            max_entries=max_ips,
            ttl_seconds=ttl_seconds,
            factory=lambda: TokenBucketRateLimiter(rate=5.0, capacity=20),
            clock=clock,
        )
        self._browse_by_principal = _BoundedTTLMap(
            max_entries=max_principals,
            ttl_seconds=ttl_seconds,
            factory=lambda: TokenBucketRateLimiter(rate=30.0, capacity=120),
            clock=clock,
        )
        self._mutation_by_principal = _BoundedTTLMap(
            max_entries=max_principals,
            ttl_seconds=ttl_seconds,
            factory=lambda: TokenBucketRateLimiter(rate=5.0, capacity=20),
            clock=clock,
        )
        self._auth_failures_by_ip = _BoundedTTLMap(
            max_entries=max_ips,
            ttl_seconds=ttl_seconds,
            factory=_AuthFailureState,
            clock=clock,
        )

    async def public_retry_after(self, ip: str) -> int | None:
        limiter = self._public_by_ip.get(ip)
        if await limiter.try_acquire():
            return None
        return max(1, int(limiter.retry_after()))

    async def principal_retry_after(self, principal: str, *, mutation: bool) -> int | None:
        buckets = self._mutation_by_principal if mutation else self._browse_by_principal
        limiter = buckets.get(principal)
        if await limiter.try_acquire():
            return None
        return max(1, int(limiter.retry_after()))

    def auth_failure_retry_after(self, ip: str) -> int | None:
        state = self._auth_failures_by_ip.get(ip)
        remaining = state.blocked_until - self._clock()
        return max(1, int(remaining + 0.999)) if remaining > 0 else None

    def record_auth_failure(self, ip: str) -> int | None:
        now = self._clock()
        state = self._auth_failures_by_ip.get(ip)
        cutoff = now - _AUTH_FAILURE_WINDOW_SECONDS
        while state.events and state.events[0] <= cutoff:
            state.events.popleft()
        state.events.append(now)
        if len(state.events) < _AUTH_FAILURE_LIMIT:
            return None
        state.events.clear()
        state.strikes += 1
        cooldown = min(
            _AUTH_INITIAL_COOLDOWN_SECONDS * (2 ** (state.strikes - 1)),
            _AUTH_MAX_COOLDOWN_SECONDS,
        )
        state.blocked_until = now + cooldown
        return int(cooldown)

    def clear_principal(self, principal: str) -> None:
        self._browse_by_principal.discard(principal)
        self._mutation_by_principal.discard(principal)

    def reset(self) -> None:
        self._public_by_ip.clear()
        self._browse_by_principal.clear()
        self._mutation_by_principal.clear()
        self._auth_failures_by_ip.clear()

    @property
    def principal_state_sizes(self) -> tuple[int, int]:
        return len(self._browse_by_principal), len(self._mutation_by_principal)

    @property
    def browse_principal_keys(self) -> tuple[str, ...]:
        return self._browse_by_principal.keys()

    @property
    def auth_ip_keys(self) -> tuple[str, ...]:
        return self._auth_failures_by_ip.keys()


compat_rate_limits = CompatRateLimitState()


def is_media_request(path: str) -> bool:
    low = path.casefold()
    if low.startswith("/subsonic/rest/"):
        endpoint = low.rsplit("/", 1)[-1].removesuffix(".view")
        return endpoint in {"stream", "download"}
    return low.startswith("/jellyfin/audio/")


def is_mutation_request(method: str, path: str) -> bool:
    if method.upper() in {"DELETE", "PATCH", "PUT"}:
        return True
    if method.upper() != "POST":
        return False
    low = path.casefold()
    return not low.endswith("/authenticatebyname") and "/playbackinfo" not in low


def reject_subsonic(
    fmt: str,
    callback: str | None,
    retry_after: int,
    *,
    server_name: str = "DroppedNeedle",
    server_version: str = "dev",
    binary: bool = False,
) -> Response:
    if binary:
        return Response(status_code=429, headers={"Retry-After": str(retry_after)})
    response = render_error(
        0,
        "Rate limit exceeded",
        fmt=fmt,
        callback=callback,
        server_name=server_name,
        server_version=server_version,
    )
    response.headers["Retry-After"] = str(retry_after)
    return response


def reject_jellyfin(retry_after: int) -> Response:
    return Response(status_code=429, headers={"Retry-After": str(retry_after)})
