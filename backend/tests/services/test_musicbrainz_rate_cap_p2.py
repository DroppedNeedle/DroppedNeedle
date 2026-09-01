"""Full-mirror tier (owner decision 2026-08-24): widened non-official
bounds, the rate_limit=0 "Unlimited" sentinel, and the absolute official clamp.

Companion to test_musicbrainz_rate_cap.py, which keeps pinning the unchanged
official-host semantics (clamp down, never up)."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest

from api.v1.schemas.settings import (
    MusicBrainzConnectionSettings,
    _OFFICIAL_MB_RATE_LIMIT,
)
import repositories.musicbrainz_base as mb_base
from repositories.musicbrainz_base import (
    mb_circuit_breaker,
    mb_deduplicator,
    mb_rate_limiter,
    mb_rate_limiter_bypassed,
    mb_api_get,
    get_mb_api_base,
    set_mb_api_base,
    set_mb_rate_limiter_bypass,
)
from repositories.musicbrainz_repository import MusicBrainzRepository

MIRROR = "https://mirror.example.com/ws/2"
OFFICIAL = "https://musicbrainz.org/ws/2"


class _FakeSemaphore:
    async def __aenter__(self):
        return None

    async def __aexit__(self, *_exc):
        return False


class _FakePriorityQueue:
    def __init__(self):
        self.slots_acquired = 0

    async def acquire_slot(self, priority):
        self.slots_acquired += 1
        return _FakeSemaphore()


class TestLimiterSentinelBypass:
    """rate_limit=0 off-official bypasses the token bucket entirely while the
    priority lane, dedup, and breaker stay fully engaged."""

    @pytest.fixture(autouse=True)
    def restore_limiter_state(self):
        rate = mb_rate_limiter.rate
        capacity = mb_rate_limiter.capacity
        source = mb_base.capture_mb_source_context()
        source_id = mb_base.get_mb_source_id()
        runtime = mb_base.brainzmash_runtime_enabled()
        yield
        set_mb_rate_limiter_bypass(False)
        mb_rate_limiter.update_rate(rate)
        mb_rate_limiter.update_capacity(capacity)
        mb_base.set_mb_api_base(
            source.source_url,
            source_mode=source.source_mode,
            source_id=source_id,
            generation=source.generation,
            brainzmash_binding_valid=runtime,
        )

    @staticmethod
    def _make_repository(api_url: str, rate_limit: float, concurrent: int):
        prefs = MagicMock()
        prefs.get_musicbrainz_connection.return_value = MusicBrainzConnectionSettings(
            source_mode="mirror",
            api_url=api_url,
            rate_limit=rate_limit,
            concurrent_searches=concurrent,
        )
        return MusicBrainzRepository(
            http_client=httpx.AsyncClient(),
            cache=MagicMock(),
            preferences_service=prefs,
        )

    @staticmethod
    async def _one_request(monkeypatch):
        queue = _FakePriorityQueue()
        monkeypatch.setattr(mb_base, "get_priority_queue", lambda: queue)
        acquire_spy = AsyncMock(wraps=mb_rate_limiter.acquire)
        monkeypatch.setattr(mb_rate_limiter, "acquire", acquire_spy)
        payload = SimpleNamespace(
            status_code=200, content=b"{}", json=lambda: {}, headers={}
        )
        client = SimpleNamespace(get=AsyncMock(return_value=payload))
        monkeypatch.setattr(mb_base, "_http_client", client)
        await mb_api_get("/artist", params={"query": "test"})
        return queue, acquire_spy

    @pytest.mark.asyncio
    async def test_sentinel_settings_bypass_the_token_bucket(self, monkeypatch):
        breaker_threshold_before = mb_circuit_breaker.failure_threshold

        self._make_repository(MIRROR, rate_limit=0, concurrent=64)
        assert mb_rate_limiter_bypassed() is True

        queue, acquire_spy = await self._one_request(monkeypatch)

        # limiter skipped, lane still taken, resilience layers untouched
        assert acquire_spy.await_count == 0
        assert queue.slots_acquired == 1
        assert mb_circuit_breaker.failure_threshold == breaker_threshold_before
        assert isinstance(
            mb_deduplicator,
            __import__(
                "infrastructure.http.deduplication", fromlist=["RequestDeduplicator"]
            ).RequestDeduplicator,
        )

    @pytest.mark.asyncio
    async def test_positive_rate_keeps_the_bucket_engaged(self, monkeypatch):
        self._make_repository(MIRROR, rate_limit=25.0, concurrent=20)
        assert mb_rate_limiter_bypassed() is False
        assert mb_rate_limiter.rate == 25.0

        _, acquire_spy = await self._one_request(monkeypatch)
        assert acquire_spy.await_count == 1

    @pytest.mark.asyncio
    async def test_construction_applies_official_clamp_regardless_of_input(self):
        self._make_repository(OFFICIAL, rate_limit=999.0, concurrent=64)
        assert mb_rate_limiter_bypassed() is False
        assert mb_rate_limiter.rate == _OFFICIAL_MB_RATE_LIMIT
        assert mb_rate_limiter.capacity == 1


class TestOnSettingsChangedSentinel:
    @pytest.fixture
    def service(self):
        from services.settings_service import SettingsService

        service = SettingsService.__new__(SettingsService)
        service._disk_cache = None
        return service

    @pytest.fixture(autouse=True)
    def restore_limiter_state(self):
        rate = mb_rate_limiter.rate
        capacity = mb_rate_limiter.capacity
        source = mb_base.capture_mb_source_context()
        source_id = mb_base.get_mb_source_id()
        runtime = mb_base.brainzmash_runtime_enabled()
        yield
        set_mb_rate_limiter_bypass(False)
        mb_rate_limiter.update_rate(rate)
        mb_rate_limiter.update_capacity(capacity)
        mb_base.set_mb_api_base(
            source.source_url,
            source_mode=source.source_mode,
            source_id=source_id,
            generation=source.generation,
            brainzmash_binding_valid=runtime,
        )

    @staticmethod
    def _cache_counter(counter: dict):
        class _Cache:
            async def clear_prefix(self, prefix: str) -> int:
                counter[prefix] = counter.get(prefix, 0) + 1
                return 1

        return _Cache()

    @pytest.mark.asyncio
    async def test_sentinel_save_flips_bypass_and_still_sweeps(
        self, service, monkeypatch
    ):
        counter: dict = {}
        monkeypatch.setattr(
            service, "_cache", self._cache_counter(counter), raising=False
        )

        await service.on_musicbrainz_settings_changed(
            MusicBrainzConnectionSettings(
                source_mode="mirror",
                api_url=MIRROR,
                rate_limit=0,
                concurrent_searches=64,
            )
        )

        assert mb_rate_limiter_bypassed() is True
        # The off-official Unlimited sentinel keeps its requested capacity;
        # only official MusicBrainz is pinned to one.
        assert mb_rate_limiter.capacity == 64
        assert len(counter) > 0  # musicbrainz_prefixes() sweep fired

    @pytest.mark.asyncio
    async def test_leaving_sentinel_restores_rate_driven_mode(
        self, service, monkeypatch
    ):
        counter: dict = {}
        monkeypatch.setattr(
            service, "_cache", self._cache_counter(counter), raising=False
        )

        await service.on_musicbrainz_settings_changed(
            MusicBrainzConnectionSettings(
                source_mode="mirror",
                api_url=MIRROR,
                rate_limit=12.0,
                concurrent_searches=8,
            )
        )

        assert mb_rate_limiter_bypassed() is False
        assert mb_rate_limiter.rate == 12.0
        assert mb_rate_limiter.capacity == 8
