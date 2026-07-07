from infrastructure.resilience.retry import CircuitBreaker
from infrastructure.service_health import ServiceHealthRegistry, report_breaker_health


class _Clock:
    def __init__(self) -> None:
        self.t = 1000.0

    def __call__(self) -> float:
        return self.t


def test_mark_and_is_degraded():
    clock = _Clock()
    reg = ServiceHealthRegistry(clock=clock)
    reg.mark_degraded("listenbrainz", "popularity", message="down", fallback="lastfm", ttl_seconds=300)

    assert reg.is_degraded("listenbrainz", "popularity")
    assert reg.is_degraded("listenbrainz")  # any capability
    assert not reg.is_degraded("musicbrainz")


def test_heal_clears_a_capability_instantly():
    clock = _Clock()
    reg = ServiceHealthRegistry(clock=clock)
    reg.mark_degraded("listenbrainz", "popularity", message="down", fallback="lastfm", ttl_seconds=1800)
    reg.mark_degraded("listenbrainz", "listens", message="down", ttl_seconds=1800)

    reg.heal("listenbrainz", "popularity")  # upstream recovered

    assert not reg.is_degraded("listenbrainz", "popularity")  # healed before its TTL
    assert reg.is_degraded("listenbrainz", "listens")  # other capability untouched
    reg.heal("listenbrainz", "does-not-exist")  # no-op, must not raise


def test_current_reports_entry_details():
    clock = _Clock()
    reg = ServiceHealthRegistry(clock=clock)
    reg.mark_degraded("listenbrainz", "popularity", message="LB down", fallback="lastfm")
    clock.t += 42  # 42s later

    entries = reg.current()
    assert len(entries) == 1
    e = entries[0]
    assert e.service == "listenbrainz"
    assert e.capability == "popularity"
    assert e.fallback == "lastfm"
    assert e.message == "LB down"
    assert e.degraded_seconds == 42


def test_ttl_slides_forward_on_refresh():
    clock = _Clock()
    reg = ServiceHealthRegistry(clock=clock)
    reg.mark_degraded("listenbrainz", "popularity", message="down", ttl_seconds=300)
    since = reg.current()[0].degraded_seconds  # 0

    clock.t += 250
    reg.mark_degraded("listenbrainz", "popularity", message="down", ttl_seconds=300)  # refresh
    clock.t += 250  # 500s after first mark, but only 250s after refresh
    assert reg.is_degraded("listenbrainz", "popularity")  # still live
    # 'since' preserved across refresh -> degraded_seconds grows from the first mark
    assert reg.current()[0].degraded_seconds == 500
    assert since == 0


def test_auto_expires_after_ttl():
    clock = _Clock()
    reg = ServiceHealthRegistry(clock=clock)
    reg.mark_degraded("listenbrainz", "popularity", message="down", ttl_seconds=300)

    clock.t += 301
    assert not reg.is_degraded("listenbrainz", "popularity")
    assert reg.current() == []  # pruned


class _State:
    """Stand-in for a CircuitState with just the .value the callback reads."""

    def __init__(self, value: str) -> None:
        self.value = value


def test_report_breaker_health_marks_on_open_and_heals_on_close():
    reg = ServiceHealthRegistry()
    cb = report_breaker_health(
        "musicbrainz", "metadata", message="MB down", fallback="none", registry=reg
    )

    cb(None, _State("closed"), _State("open"), "failure_threshold_reached")
    assert reg.is_degraded("musicbrainz", "metadata")
    entry = reg.current()[0]
    assert entry.message == "MB down"
    assert entry.fallback == "none"

    cb(None, _State("half_open"), _State("closed"), "success_threshold_reached")
    assert not reg.is_degraded("musicbrainz", "metadata")


def test_report_breaker_health_ignores_half_open_probe():
    reg = ServiceHealthRegistry()
    cb = report_breaker_health("lastfm", "music data", message="down", registry=reg)

    cb(None, _State("closed"), _State("open"), "threshold")
    cb(None, _State("open"), _State("half_open"), "timeout_elapsed")  # a probe, not recovery

    assert reg.is_degraded("lastfm", "music data")  # still degraded until it truly closes


def test_report_breaker_health_composes_also_callback():
    reg = ServiceHealthRegistry()
    seen: list[tuple] = []

    def _also(breaker, prev, new, reason):
        seen.append((prev.value, new.value, reason))

    cb = report_breaker_health(
        "audiodb", "artist info", message="down", registry=reg, also=_also
    )
    cb("brk", _State("closed"), _State("open"), "threshold")

    assert reg.is_degraded("audiodb", "artist info")
    assert seen == [("closed", "open", "threshold")]


def test_report_breaker_health_end_to_end_with_real_breaker():
    """The callback wired to a real CircuitBreaker: crossing the failure threshold
    marks the service degraded; recovery through half-open heals it."""
    reg = ServiceHealthRegistry()
    breaker = CircuitBreaker(
        failure_threshold=2,
        success_threshold=1,
        timeout=0.0,  # half-open immediately available on recovery
        name="mb-test",
        on_state_change=report_breaker_health(
            "musicbrainz", "metadata", message="MB down", registry=reg
        ),
    )

    breaker.record_failure()
    assert not reg.is_degraded("musicbrainz", "metadata")  # one failure, not open yet
    breaker.record_failure()  # threshold reached -> OPEN
    assert reg.is_degraded("musicbrainz", "metadata")

    assert not breaker.is_open()  # timeout 0 -> OPEN transitions to HALF_OPEN
    breaker.record_success()  # HALF_OPEN -> CLOSED
    assert not reg.is_degraded("musicbrainz", "metadata")
