"""In-process registry of degraded external-service capabilities.

Single source of truth for "is <service> currently degraded?", read by both the
fallback logic (skip the dead source, use the fallback) and the UI status indicator.
Entries auto-expire after a TTL that is refreshed on every fresh degraded signal, so
a capability heals on its own once the upstream stops reporting itself down - no
manual reset, no background sweeper.

Single-process invariant: like the app's other singletons/limiters, this lives in one
uvicorn worker. It is deliberately signal-driven - we only mark a capability degraded
on an UNAMBIGUOUS upstream signal (e.g. ListenBrainz literally replying "currently
disabled" / "provide an Auth token"), never on a transient timeout, so it reflects a
genuine outage rather than a blip.
"""

import time
from typing import Any, Callable

import msgspec


class ServiceHealthEntry(msgspec.Struct, frozen=True):
    service: str  # "listenbrainz"
    capability: str  # "popularity"
    severity: str  # "degraded" | "down"
    message: str  # user-facing, one line
    fallback: str | None  # what we're using instead, e.g. "lastfm"
    degraded_seconds: int  # how long it's been degraded (for the UI)


class ServiceHealthRegistry:
    def __init__(self, clock: Callable[[], float] = time.monotonic) -> None:
        self._clock = clock
        # (service, capability) -> (severity, message, fallback, since, until)
        self._entries: dict[tuple[str, str], tuple[str, str, str | None, float, float]] = {}

    def mark_degraded(
        self,
        service: str,
        capability: str,
        *,
        message: str,
        fallback: str | None = None,
        severity: str = "degraded",
        ttl_seconds: float = 300.0,
    ) -> None:
        """Record (or refresh) a degraded capability. The TTL slides forward on every
        call, so as long as the upstream keeps reporting itself down the entry stays
        live; once the signals stop, it expires and the capability is considered healthy
        again on the next check."""
        now = self._clock()
        key = (service, capability)
        existing = self._entries.get(key)
        since = existing[3] if existing else now
        self._entries[key] = (severity, message, fallback, since, now + ttl_seconds)

    def is_degraded(self, service: str, capability: str | None = None) -> bool:
        now = self._clock()
        for (svc, cap), (_sev, _msg, _fb, _since, until) in self._entries.items():
            if until >= now and svc == service and (capability is None or cap == capability):
                return True
        return False

    def current(self) -> list[ServiceHealthEntry]:
        """Live degraded entries; prunes expired ones as a side effect."""
        now = self._clock()
        expired = [k for k, v in self._entries.items() if v[4] < now]
        for k in expired:
            self._entries.pop(k, None)
        out: list[ServiceHealthEntry] = []
        for (svc, cap), (sev, msg, fb, since, _until) in self._entries.items():
            out.append(
                ServiceHealthEntry(
                    service=svc,
                    capability=cap,
                    severity=sev,
                    message=msg,
                    fallback=fb,
                    degraded_seconds=int(now - since),
                )
            )
        out.sort(key=lambda e: (e.service, e.capability))
        return out

    def heal(self, service: str, capability: str) -> None:
        """Clear one capability's degraded flag - call when the upstream succeeds again,
        for an instant recovery instead of waiting out the TTL."""
        self._entries.pop((service, capability), None)

    def clear(self) -> None:
        self._entries.clear()


service_health = ServiceHealthRegistry()


def report_breaker_health(
    service: str,
    capability: str,
    *,
    message: str,
    fallback: str | None = None,
    severity: str = "degraded",
    ttl_seconds: float = 300.0,
    registry: ServiceHealthRegistry | None = None,
    also: Callable[..., None] | None = None,
) -> Callable[..., None]:
    """Build a circuit-breaker ``on_state_change`` callback that mirrors a breaker's
    OPEN/CLOSED state into the health registry, so a sustained outage of ``service``
    surfaces in the header status indicator and auto-heals when the breaker recovers.

    A breaker only OPENs after ``failure_threshold`` consecutive failures (each already
    retried), so this is a genuine sustained outage - matching this registry's signal-
    driven contract of never flagging a single transient blip. Under live traffic the
    entry re-marks on every failed half-open probe (~every breaker ``timeout``), sliding
    the TTL, and clears the moment the breaker closes. If the traffic that opened the
    breaker then stops entirely, no probe fires to close it, so the entry rides its TTL
    out instead - kept at the registry's own 300s default so a no-longer-exercised
    capability self-heals promptly. ``also`` composes an existing callback (e.g. a
    logging one). The optional ``registry`` is for tests; production uses the singleton.
    """
    target = registry if registry is not None else service_health

    def _on_state_change(
        breaker: Any,
        previous_state: Any,
        new_state: Any,
        reason: str,
    ) -> None:
        state = getattr(new_state, "value", new_state)
        if state == "open":
            target.mark_degraded(
                service,
                capability,
                message=message,
                fallback=fallback,
                severity=severity,
                ttl_seconds=ttl_seconds,
            )
        elif state == "closed":
            target.heal(service, capability)
        if also is not None:
            also(breaker, previous_state, new_state, reason)

    return _on_state_change
