"""QW11 Part 2: MB rate-limit header telemetry - parsing, low-remaining warn
throttle (once per minute), windowed low events, and proof that telemetry
observations never perturb the ok/empty_404/http_error call counters."""

import logging

import pytest

from infrastructure.cache.cache_metrics import WindowedCounterMap
from infrastructure.observability import provider_counters
from infrastructure.observability.provider_counters import (
    LOW_REMAINING_THRESHOLD,
    RateLimitGauge,
)


class FakeHeaders:
    """Case-insensitive header mapping like httpx.Headers."""

    def __init__(self, values: dict[str, str] | None = None):
        self._values = {k.lower(): v for k, v in (values or {}).items()}

    def get(self, name: str):
        return self._values.get(name.lower())


class FakeClock:
    def __init__(self):
        self.now = 1000.0

    def __call__(self) -> float:
        return self.now

    def advance(self, seconds: float) -> None:
        self.now += seconds


def _mb_headers(remaining: str = "10") -> FakeHeaders:
    return FakeHeaders(
        {
            "x-ratelimit-limit": "15",
            "x-ratelimit-remaining": remaining,
            "x-ratelimit-reset": "1787600000",
            "x-mb-rate-limiter": "lua",
        }
    )


class TestHeaderParsing:
    @pytest.mark.asyncio
    async def test_full_headers_parse(self):
        gauge = RateLimitGauge()
        gauge.observe("musicbrainz", _mb_headers())

        rows = gauge.snapshot_rows()
        assert rows == [
            {
                "provider": "musicbrainz",
                "limit": 15,
                "remaining": 10,
                "reset_epoch": 1787600000.0,
                "observed_at": rows[0]["observed_at"],
                "low_remaining_events_window": 0,
            }
        ]
        assert rows[0]["observed_at"] > 0

    @pytest.mark.asyncio
    async def test_missing_and_garbage_headers_yield_none_fields(self):
        gauge = RateLimitGauge()

        gauge.observe("musicbrainz", FakeHeaders())
        row = gauge.snapshot_rows()[0]
        assert (
            row["limit"] is None
            and row["remaining"] is None
            and row["reset_epoch"] is None
        )

        gauge.observe(
            "musicbrainz",
            FakeHeaders(
                {"x-ratelimit-limit": "abc", "x-ratelimit-reset": "not-a-number"}
            ),
        )
        row = gauge.snapshot_rows()[0]
        assert row["limit"] is None and row["reset_epoch"] is None

    @pytest.mark.asyncio
    async def test_latest_observation_wins_per_provider(self):
        gauge = RateLimitGauge()
        gauge.observe("musicbrainz", _mb_headers(remaining="9"))
        gauge.observe("musicbrainz", _mb_headers(remaining="4"))

        assert gauge.snapshot_rows()[0]["remaining"] == 4


class TestLowRemainingTelemetry:
    @pytest.mark.asyncio
    async def test_low_remaining_counts_windowed_events(self):
        clock = FakeClock()
        gauge = RateLimitGauge(clock=clock)
        gauge.observe(
            "musicbrainz", _mb_headers(remaining=str(LOW_REMAINING_THRESHOLD))
        )
        gauge.observe("musicbrainz", _mb_headers(remaining="1"))
        gauge.observe("musicbrainz", _mb_headers(remaining="50"))  # not low

        assert gauge.snapshot_rows()[0]["low_remaining_events_window"] == 2

    @pytest.mark.asyncio
    async def test_warn_logged_at_most_once_per_minute(self, caplog):
        clock = FakeClock()
        gauge = RateLimitGauge(clock=clock)

        with caplog.at_level(
            logging.WARNING, logger="infrastructure.observability.provider_counters"
        ):
            gauge.observe("musicbrainz", _mb_headers(remaining="2"))
            gauge.observe("musicbrainz", _mb_headers(remaining="1"))
            gauge.observe("musicbrainz", _mb_headers(remaining="0"))
            warnings = [r for r in caplog.records if "low_remaining" in r.message]
            assert len(warnings) == 1

            clock.advance(59.0)
            gauge.observe("musicbrainz", _mb_headers(remaining="1"))
            warnings = [r for r in caplog.records if "low_remaining" in r.message]
            assert len(warnings) == 1  # still throttled inside the minute

            clock.advance(61.0)
            gauge.observe("musicbrainz", _mb_headers(remaining="1"))
            warnings = [r for r in caplog.records if "low_remaining" in r.message]
            assert len(warnings) == 2  # a new minute allows one more

    @pytest.mark.asyncio
    async def test_warn_throttle_is_per_provider(self, caplog):
        gauge = RateLimitGauge()
        with caplog.at_level(
            logging.WARNING,
            logger="infrastructure.observability.provider_counters",
        ):
            gauge.observe("musicbrainz", _mb_headers(remaining="2"))
            gauge.observe("listenbrainz", _mb_headers(remaining="2"))
            warnings = [r for r in caplog.records if "low_remaining" in r.message]
            assert len(warnings) == 2


class TestCallCountsUnaffected:
    @pytest.fixture
    def fresh_counters(self, monkeypatch):
        counters = WindowedCounterMap()
        monkeypatch.setattr(provider_counters, "_counters", counters)
        return counters

    @pytest.mark.asyncio
    async def test_telemetry_does_not_perturb_call_outcomes(
        self, fresh_counters: WindowedCounterMap
    ):
        before = provider_counters.snapshot_provider_rows()

        # A burst of telemetry observations, including low-remaining ones.
        for remaining in ("14", "3", "2", "0"):
            provider_counters.record_rate_limit_headers(
                "musicbrainz", _mb_headers(remaining)
            )

        after = provider_counters.snapshot_provider_rows()
        assert after == before  # call outcome counts untouched by telemetry
