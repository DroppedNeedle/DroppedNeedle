"""QW1: repo-level cache + dedup on get_release_groups_by_artist.

Covers: cache-hit-no-refetch, concurrent-coalesce-to-one wire call,
failure-not-cached (propagation + degradation record), empty negative cache,
and priority passthrough to mb_api_get.

BrainzMashEfficiency artist slice: RG page L1 cache-aside on
_get_artist_release_groups_or_raise_with_context (04-1), MBID gates at the
artist choke points (05-1), and redirect pre-resolution (03-8).
"""

import asyncio
import inspect
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest

import repositories.musicbrainz_artist as artist_module
import repositories.musicbrainz_base as mb_base
from core.exceptions import ExternalServiceError
from infrastructure.cache.cache_keys import mb_redirect_key
from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
)
from infrastructure.queue.priority_queue import RequestPriority
from repositories.musicbrainz_artist import MusicBrainzArtistMixin
from repositories.musicbrainz_base import MbCachePolicy

_ARTIST = "f4a31f0a-51dd-4fa7-986d-3095c40c5ed9"
_RGS = [
    {"id": "bbbbbbbb-0000-0000-0000-000000000001", "title": "RG One"},
    {"id": "bbbbbbbb-0000-0000-0000-000000000002", "title": "RG Two"},
]
_OLD_ARTIST = "11111111-1111-1111-1111-111111111111"
_NEW_ARTIST = "22222222-2222-2222-2222-222222222222"
# 05-1 boundary inputs: falsy-shape, truncated-shape, Spotify-shaped (22-char).
_INVALID_MBIDS = ["0", "2047108-7", "0OdUWJ0sBjDrqHygGUXeCF"]


class _Repo(MusicBrainzArtistMixin):
    def __init__(self) -> None:
        self._cache = InMemoryCache(max_entries=100)
        self._preferences_service = SimpleNamespace(
            get_advanced_settings=lambda: SimpleNamespace(cache_ttl_search=3600)
        )


def _payload(release_groups):
    return SimpleNamespace(
        release_groups=release_groups, release_group_count=len(release_groups)
    )


@pytest.fixture
def fresh_deduplicator(monkeypatch):
    """Isolate the module-singleton deduplicator per test."""
    artist_module.mb_deduplicator.clear()
    yield artist_module.mb_deduplicator
    artist_module.mb_deduplicator.clear()


class TestCacheHitNoRefetch:
    @pytest.mark.asyncio
    async def test_second_call_served_from_cache(self, monkeypatch, fresh_deduplicator):
        repo = _Repo()
        calls = {"n": 0}

        async def fake_get(*args, **kwargs):
            calls["n"] += 1
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        first = await repo.get_release_groups_by_artist(_ARTIST, limit=10)
        second = await repo.get_release_groups_by_artist(_ARTIST, limit=10)

        assert first == _RGS and second == _RGS
        assert calls["n"] == 1  # one wire call total

    @pytest.mark.asyncio
    async def test_different_limits_use_distinct_keys(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        seen_params: list[dict] = []

        async def fake_get(path, params=None, **kwargs):
            seen_params.append(params or {})
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        await repo.get_release_groups_by_artist(_ARTIST, limit=15)
        await repo.get_release_groups_by_artist(_ARTIST, limit=100)

        assert len(seen_params) == 2  # distinct limit -> distinct key -> refetch


class TestConcurrentCoalesce:
    @pytest.mark.asyncio
    async def test_concurrent_cold_callers_share_one_wire_call(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        calls = {"n": 0}

        async def slow_get(*args, **kwargs):
            # Long enough that both tasks enter dedupe() before the leader lands.
            await asyncio.sleep(0.05)
            calls["n"] += 1
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", slow_get)

        results = await asyncio.gather(
            repo.get_release_groups_by_artist(_ARTIST, limit=10),
            repo.get_release_groups_by_artist(_ARTIST, limit=10),
        )

        assert results[0] == _RGS and results[1] == _RGS
        assert calls["n"] == 1  # exactly one wire call for N cold viewers


class TestFailureNotCached:
    @pytest.mark.asyncio
    async def test_failure_propagates_records_and_never_caches(
        self, monkeypatch, fresh_deduplicator
    ):
        recorded: list = []
        ctx = init_degradation_context()
        original_record = ctx.record

        class _Spy:
            def record(self, result):
                recorded.append(result)
                original_record(result)

            def __getattr__(self, name):
                return getattr(ctx, name)

        monkeypatch.setattr(
            artist_module, "try_get_degradation_context", lambda: _Spy()
        )

        repo = _Repo()

        async def failing_get(*args, **kwargs):
            raise ExternalServiceError("MusicBrainz API error (503)")

        monkeypatch.setattr(artist_module, "mb_api_get", failing_get)

        with pytest.raises(ExternalServiceError):
            await repo.get_release_groups_by_artist(_ARTIST, limit=10)

        assert any("release groups failed" in r.error_message for r in recorded)
        # Nothing cached under the browse key: a retry goes back to the wire.
        key = artist_module.mb_artist_rgs_browse_key(_ARTIST, 10)
        assert await repo._cache.get(key) is None

        clear_degradation_context()


class TestArtistDetailFailureSemantics:
    @pytest.mark.asyncio
    async def test_authoritative_404_is_negative_cached(
        self, monkeypatch, fresh_deduplicator
    ) -> None:
        repo = _Repo()
        calls: list[str] = []

        async def not_found_get(path, params=None, **kwargs):
            calls.append(path)
            if path.startswith("/artist/"):
                return {}
            return _payload([])

        monkeypatch.setattr(artist_module, "mb_api_get", not_found_get)

        assert await repo.get_artist_by_id(_ARTIST) is None
        assert not await repo.get_artist_by_id(_ARTIST)

        assert calls == [f"/artist/{_ARTIST}", "/release-group"]

    @pytest.mark.asyncio
    async def test_transient_http_failure_is_typed_and_not_cached(
        self, monkeypatch, fresh_deduplicator
    ) -> None:
        repo = _Repo()
        ctx = init_degradation_context()
        request = httpx.Request("GET", f"https://musicbrainz.org/artist/{_ARTIST}")
        calls: list[str] = []

        async def failing_get(path, params=None, **kwargs):
            # Both legs fail: a succeeding browse leg would legitimately
            # page-cache its (empty) result, which is not a cached failure.
            calls.append(path)
            raise httpx.ConnectError("connection reset", request=request)

        monkeypatch.setattr(artist_module, "mb_api_get", failing_get)

        try:
            with pytest.raises(ExternalServiceError) as first:
                await repo.get_artist_by_id(_ARTIST)
            with pytest.raises(ExternalServiceError):
                await repo.get_artist_by_id(_ARTIST)

            assert isinstance(first.value.__cause__, httpx.HTTPError)
            assert ctx.summary() == {"musicbrainz": "error"}
            assert repo._cache.size() == 0
            key = artist_module.mb_artist_detail_key(_ARTIST)
            assert await repo._cache.get(key) is None
            assert calls.count(f"/artist/{_ARTIST}") == 2
        finally:
            clear_degradation_context()

    @pytest.mark.asyncio
    async def test_detail_404_wins_over_ancillary_browse_failure(
        self, monkeypatch, fresh_deduplicator
    ) -> None:
        repo = _Repo()
        ctx = init_degradation_context()

        async def detail_missing_browse_fails(path, params=None, **kwargs):
            if path.startswith("/artist/"):
                return {}
            raise ExternalServiceError("browse unavailable")

        monkeypatch.setattr(artist_module, "mb_api_get", detail_missing_browse_fails)

        try:
            assert await repo.get_artist_by_id(_ARTIST) is None
            key = artist_module.mb_artist_detail_key(_ARTIST)
            assert await repo._cache.get(key) == {}
            assert ctx.summary() == {"musicbrainz": "error"}
        finally:
            clear_degradation_context()


class TestEmptyNegativeCache:
    @pytest.mark.asyncio
    async def test_genuinely_empty_result_negative_cached(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        calls = {"n": 0}

        async def empty_get(*args, **kwargs):
            calls["n"] += 1
            return _payload([])

        monkeypatch.setattr(artist_module, "mb_api_get", empty_get)

        first = await repo.get_release_groups_by_artist(_ARTIST, limit=10)
        second = await repo.get_release_groups_by_artist(_ARTIST, limit=10)

        assert first == [] and second == []
        assert calls["n"] == 1  # negative entry prevents refetch within TTL


class TestPriorityPassthrough:
    @pytest.mark.asyncio
    async def test_priority_reaches_mb_api_get(self, monkeypatch, fresh_deduplicator):
        repo = _Repo()
        captured: dict = {}

        async def capture_get(path, params=None, priority=None, **kwargs):
            captured["priority"] = priority
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", capture_get)

        await repo.get_release_groups_by_artist(
            _ARTIST, limit=10, priority=RequestPriority.USER_INITIATED
        )
        assert captured["priority"] == RequestPriority.USER_INITIATED

    def test_or_raise_default_lane_unchanged_for_poller(self):
        # The follow poller relies on the defaulted kwarg keeping its lane.
        sig = inspect.signature(
            MusicBrainzArtistMixin.get_artist_release_groups_or_raise
        )
        assert sig.parameters["priority"].default == RequestPriority.BACKGROUND_SYNC


async def _no_wire(*args, **kwargs):
    raise AssertionError("gated lookup reached the wire")


async def _seed_redirect(repo, old: str, new: str) -> None:
    source_context = artist_module.capture_mb_source_context()
    cache_token = artist_module.capture_mb_cache_token(repo._cache)
    await artist_module.mb_cache_set_if_current(
        repo._cache,
        mb_redirect_key("artist", old),
        new,
        ttl_seconds=86400,
        context=source_context,
        cache_token=cache_token,
    )


def _sole_entry_ttl(repo) -> float:
    entries = list(repo._cache._cache.values())
    assert len(entries) == 1
    return entries[0].expires_at - time.time()


def _expire_sole_entry(repo) -> None:
    entries = list(repo._cache._cache.values())
    assert len(entries) == 1
    entries[0].expires_at = time.time() - 1


class TestRgPageCacheAside:
    @pytest.mark.asyncio
    async def test_user_then_background_same_page_one_wire(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        calls = {"n": 0}

        async def fake_get(*args, **kwargs):
            calls["n"] += 1
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        user_items, user_total, _ = await repo.get_artist_release_groups_with_context(
            _ARTIST,
            limit=10,
            priority=RequestPriority.USER_INITIATED,
            cache_policy=MbCachePolicy.DISPLAY_FRESH,
        )
        bg_items, bg_total, _ = await repo.get_artist_release_groups_with_context(
            _ARTIST,
            limit=10,
            priority=RequestPriority.BACKGROUND_SYNC,
            cache_policy=MbCachePolicy.DISPLAY_FRESH,
        )

        assert (user_items, user_total) == (_RGS, 2)
        assert (bg_items, bg_total) == (_RGS, 2)
        assert calls["n"] == 1  # L1 read shared across priority lanes

    @pytest.mark.asyncio
    async def test_l1_hit_returns_callers_context(self, monkeypatch, fresh_deduplicator):
        repo = _Repo()

        async def fake_get(*args, **kwargs):
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        await repo.get_artist_release_groups_with_context(
            _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
        )
        caller_context = artist_module.capture_mb_source_context()
        _, _, hit_context = await repo.get_artist_release_groups_with_context(
            _ARTIST,
            limit=10,
            source_context=caller_context,
            cache_policy=MbCachePolicy.DISPLAY_FRESH,
        )

        # Pin vs the _fetch_artist_by_id merge: a None/mismatched browse
        # context drops the release-group leg.
        assert hit_context is caller_context

    @pytest.mark.asyncio
    async def test_bypass_never_reads_or_publishes_pages(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        stale = [{"id": "cccccccc-0000-0000-0000-000000000001", "title": "Stale"}]

        async def stale_get(*args, **kwargs):
            return _payload(stale)

        monkeypatch.setattr(artist_module, "mb_api_get", stale_get)
        await repo.get_artist_release_groups_with_context(
            _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
        )

        seen: dict = {}

        async def fresh_get(path, params=None, **kwargs):
            seen.update(kwargs)
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fresh_get)
        page_gets: list[str] = []
        page_sets: list[str] = []
        orig_get_with_metadata = repo._cache.get_with_metadata
        orig_set_if_token = repo._cache.set_if_token

        async def spy_get(key):
            page_gets.append(key)
            return await orig_get_with_metadata(key)

        async def spy_set_if_token(token, key, value, ttl_seconds=60, **kwargs):
            page_sets.append(key)
            return await orig_set_if_token(
                token, key, value, ttl_seconds=ttl_seconds, **kwargs
            )

        monkeypatch.setattr(repo._cache, "get_with_metadata", spy_get)
        monkeypatch.setattr(repo._cache, "set_if_token", spy_set_if_token)

        items, total, _ = await repo.get_artist_release_groups_with_context(
            _ARTIST, limit=10
        )

        assert items == _RGS  # fresh wire data, not the seeded stale page
        assert seen["cache_policy"] is MbCachePolicy.BYPASS  # L2 skipped too
        assert [k for k in page_gets if ":page:" in k] == []
        assert [k for k in page_sets if ":page:" in k] == []

    @pytest.mark.asyncio
    async def test_empty_page_cached_600s(self, monkeypatch, fresh_deduplicator):
        repo = _Repo()
        calls = {"n": 0}

        async def empty_get(*args, **kwargs):
            calls["n"] += 1
            return _payload([])

        monkeypatch.setattr(artist_module, "mb_api_get", empty_get)

        first = await repo.get_artist_release_groups_with_context(
            _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
        )
        assert first[0] == [] and first[1] == 0
        assert _sole_entry_ttl(repo) == pytest.approx(600, abs=5)

        second = await repo.get_artist_release_groups_with_context(
            _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
        )
        assert second[0] == [] and second[1] == 0
        assert calls["n"] == 1

        _expire_sole_entry(repo)
        await repo.get_artist_release_groups_with_context(
            _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
        )
        assert calls["n"] == 2  # past TTL the page rewires

    @pytest.mark.asyncio
    async def test_positive_page_cached_3600s(self, monkeypatch, fresh_deduplicator):
        repo = _Repo()

        async def fake_get(*args, **kwargs):
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        await repo.get_artist_release_groups_with_context(
            _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
        )
        assert _sole_entry_ttl(repo) == pytest.approx(3600, abs=5)

    @pytest.mark.asyncio
    async def test_failure_propagates_records_and_uncached(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        calls = {"n": 0}
        ctx = init_degradation_context()

        async def failing_get(*args, **kwargs):
            calls["n"] += 1
            raise ExternalServiceError("MusicBrainz API error (503)")

        monkeypatch.setattr(artist_module, "mb_api_get", failing_get)

        try:
            with pytest.raises(ExternalServiceError):
                await repo.get_artist_release_groups_with_context(
                    _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
                )
            with pytest.raises(ExternalServiceError):
                await repo.get_artist_release_groups_with_context(
                    _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
                )

            assert ctx.summary() == {"musicbrainz": "error"}
            assert repo._cache.size() == 0
            assert calls["n"] == 2  # retry rewires; failures never cached
        finally:
            clear_degradation_context()

    @pytest.mark.asyncio
    async def test_preserve_fetch_width_slicing_identical_on_cached(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        calls = {"n": 0}

        async def fake_get(*args, **kwargs):
            calls["n"] += 1
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        # limit=0 fetches width 1 but slices to []; both widths share one key.
        wide = await repo.get_artist_release_groups_with_context(
            _ARTIST,
            limit=0,
            preserve_fetch_width=True,
            cache_policy=MbCachePolicy.DISPLAY_FRESH,
        )
        assert wide[0] == _RGS
        narrow = await repo.get_artist_release_groups_with_context(
            _ARTIST,
            limit=0,
            preserve_fetch_width=False,
            cache_policy=MbCachePolicy.DISPLAY_FRESH,
        )
        assert narrow[0] == []
        assert calls["n"] == 1

    @pytest.mark.asyncio
    async def test_fetch_limit_clamp_shares_keys(self, monkeypatch, fresh_deduplicator):
        repo = _Repo()
        calls = {"n": 0}

        async def fake_get(*args, **kwargs):
            calls["n"] += 1
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        # limit=150 clamps to fetch width 100: same page key as limit=100.
        await repo.get_artist_release_groups_with_context(
            _ARTIST, limit=150, cache_policy=MbCachePolicy.DISPLAY_FRESH
        )
        await repo.get_artist_release_groups_with_context(
            _ARTIST, limit=100, cache_policy=MbCachePolicy.DISPLAY_FRESH
        )
        assert calls["n"] == 1


class TestRgPageSourceRace:
    """A source change mid-flight is control flow (propagate silently);
    a genuine provider failure still records the page-leg degradation."""

    @pytest.mark.asyncio
    async def test_source_switch_race_propagates_without_recording(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        ctx = init_degradation_context()
        old_generation = mb_base.get_mb_source_generation()

        async def racing_get(*args, **kwargs):
            monkeypatch.setattr(
                mb_base, "_mb_source_generation", old_generation + 1
            )
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", racing_get)

        try:
            with pytest.raises(ExternalServiceError, match="source changed"):
                await repo.get_artist_release_groups_with_context(
                    _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
                )
            assert ctx.summary() == {}
            assert repo._cache.size() == 0
        finally:
            clear_degradation_context()

    @pytest.mark.asyncio
    async def test_genuine_failure_propagates_and_records_page_leg(
        self, monkeypatch, fresh_deduplicator
    ):
        recorded: list = []
        ctx = init_degradation_context()
        original_record = ctx.record

        class _Spy:
            def record(self, result):
                recorded.append(result)
                original_record(result)

            def __getattr__(self, name):
                return getattr(ctx, name)

        monkeypatch.setattr(
            artist_module, "try_get_degradation_context", lambda: _Spy()
        )
        repo = _Repo()

        async def failing_get(*args, **kwargs):
            raise ExternalServiceError("MusicBrainz API error (503)")

        monkeypatch.setattr(artist_module, "mb_api_get", failing_get)

        try:
            with pytest.raises(ExternalServiceError):
                await repo.get_artist_release_groups_with_context(
                    _ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
                )
            assert any(
                "release groups page failed" in r.error_message for r in recorded
            )
        finally:
            clear_degradation_context()


class TestArtistMbidGates:
    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        "bad_mbid", _INVALID_MBIDS, ids=["zero", "truncated", "spotify-shaped"]
    )
    async def test_gated_methods_return_absence_without_wire(
        self, monkeypatch, fresh_deduplicator, bad_mbid
    ):
        repo = _Repo()
        monkeypatch.setattr(artist_module, "mb_api_get", _no_wire)

        assert await repo.get_artist_core(bad_mbid) is None
        assert await repo.get_artist_by_id(bad_mbid) is None
        assert await repo.get_artist_relations(bad_mbid) is None
        assert await repo.get_release_groups_by_artist(bad_mbid) == []
        items, total, context = await repo.get_artist_release_groups_with_context(
            bad_mbid
        )
        assert items == [] and total == 0
        assert context is not None and artist_module.is_mb_source_current(context)
        # Zero-caller variants inherit the gate instead of raising.
        assert await repo.get_artist_release_groups(bad_mbid) == ([], 0)
        assert await repo.get_artist_release_groups_or_raise(bad_mbid) == ([], 0)

    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        "bad_mbid", _INVALID_MBIDS, ids=["zero", "truncated", "spotify-shaped"]
    )
    async def test_gate_records_degradation_with_request_scope(
        self, monkeypatch, fresh_deduplicator, bad_mbid
    ):
        repo = _Repo()
        monkeypatch.setattr(artist_module, "mb_api_get", _no_wire)
        ctx = init_degradation_context()
        try:
            assert await repo.get_artist_core(bad_mbid) is None
            assert ctx.summary() == {"musicbrainz": "error"}
        finally:
            clear_degradation_context()

    @pytest.mark.asyncio
    async def test_gate_records_deterministic_failure(
        self, monkeypatch, fresh_deduplicator
    ):
        """T7: an invalid-MBID gate records a deterministic failure so
        identification jobs classify garbage input as
        UNMAPPABLE_PROVIDER_PAYLOAD (F-IDENT-02), not a transient outage."""
        repo = _Repo()
        monkeypatch.setattr(artist_module, "mb_api_get", _no_wire)
        ctx = init_degradation_context()
        try:
            assert await repo.get_artist_core("0") is None
            assert ctx.summary() == {"musicbrainz": "error"}
            assert ctx.deterministic_sources() == {"musicbrainz"}
        finally:
            clear_degradation_context()

    @pytest.mark.asyncio
    async def test_transient_failure_records_non_deterministic(
        self, monkeypatch, fresh_deduplicator
    ):
        """T7: a transient provider failure records degradation WITHOUT the
        deterministic flag, so identification jobs keep deferring as outage."""
        repo = _Repo()

        async def failing_get(*args, **kwargs):
            raise ExternalServiceError("MusicBrainz API error (503)")

        monkeypatch.setattr(artist_module, "mb_api_get", failing_get)
        ctx = init_degradation_context()
        try:
            with pytest.raises(ExternalServiceError):
                await repo.get_release_groups_by_artist(_ARTIST, limit=10)
            assert ctx.summary() == {"musicbrainz": "error"}
            assert ctx.deterministic_sources() == set()
        finally:
            clear_degradation_context()

    @pytest.mark.asyncio
    async def test_gate_silent_without_request_scope(
        self, monkeypatch, fresh_deduplicator
    ):
        clear_degradation_context()
        repo = _Repo()
        monkeypatch.setattr(artist_module, "mb_api_get", _no_wire)

        # Background jobs have no scope: absence, no crash.
        assert await repo.get_artist_core("0") is None
        assert await repo.get_release_groups_by_artist("0") == []

    @pytest.mark.asyncio
    async def test_lowercase_valid_mbid_wires(self, monkeypatch, fresh_deduplicator):
        repo = _Repo()
        calls: list[str] = []

        async def fake_get(path, params=None, **kwargs):
            calls.append(path)
            return {"id": _ARTIST, "name": "Valid"}

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        assert await repo.get_artist_core(_ARTIST) == {"id": _ARTIST, "name": "Valid"}
        assert calls == [f"/artist/{_ARTIST}"]

    @pytest.mark.asyncio
    async def test_uppercase_valid_mbid_wires_casefolded(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        calls: list[str] = []

        async def fake_get(path, params=None, **kwargs):
            calls.append(path)
            return {"id": _ARTIST, "name": "Valid"}

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        assert await repo.get_artist_core(_ARTIST.upper()) is not None
        assert calls == [f"/artist/{_ARTIST}"]


class TestArtistRedirectPreresolve:
    @pytest.mark.asyncio
    async def test_memory_redirect_rewrites_artist_core_wire(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        await _seed_redirect(repo, _OLD_ARTIST, _NEW_ARTIST)
        calls: list[str] = []

        async def fake_get(path, params=None, **kwargs):
            calls.append(path)
            return {"id": _NEW_ARTIST, "name": "Survivor"}

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        result = await repo.get_artist_core(_OLD_ARTIST)

        assert result == {"id": _NEW_ARTIST, "name": "Survivor"}
        assert calls == [f"/artist/{_NEW_ARTIST}"]

    @pytest.mark.asyncio
    async def test_memory_redirect_rewrites_artist_detail_both_legs(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        await _seed_redirect(repo, _OLD_ARTIST, _NEW_ARTIST)
        calls: list[tuple] = []

        async def fake_get(path, params=None, **kwargs):
            calls.append((path, params or {}))
            if path.startswith("/artist/"):
                return {"id": _NEW_ARTIST, "name": "Survivor"}
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        result = await repo.get_artist_by_id(_OLD_ARTIST)

        assert result is not None and result["name"] == "Survivor"
        assert len(calls) == 2
        assert all(_OLD_ARTIST not in path for path, _ in calls)
        detail = [path for path, _ in calls if path.startswith("/artist/")]
        assert detail == [f"/artist/{_NEW_ARTIST}"]
        browse = [params for path, params in calls if path == "/release-group"]
        assert browse and all(p.get("artist") == _NEW_ARTIST for p in browse)

    @pytest.mark.asyncio
    async def test_memory_redirect_rewrites_browse_by_mbid(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        await _seed_redirect(repo, _OLD_ARTIST, _NEW_ARTIST)
        seen_params: list[dict] = []

        async def fake_get(path, params=None, **kwargs):
            seen_params.append(params or {})
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        items, total, _ = await repo.get_artist_release_groups_with_context(
            _OLD_ARTIST, limit=10, cache_policy=MbCachePolicy.DISPLAY_FRESH
        )

        assert (items, total) == (_RGS, 2)
        assert len(seen_params) == 1
        assert seen_params[0]["artist"] == _NEW_ARTIST

    @pytest.mark.asyncio
    async def test_memory_redirect_rewrites_qw1_browse(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        await _seed_redirect(repo, _OLD_ARTIST, _NEW_ARTIST)
        seen_params: list[dict] = []

        async def fake_get(path, params=None, **kwargs):
            seen_params.append(params or {})
            return _payload(_RGS)

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        assert await repo.get_release_groups_by_artist(_OLD_ARTIST, limit=10) == _RGS
        assert len(seen_params) == 1
        assert seen_params[0]["artist"] == _NEW_ARTIST

    @pytest.mark.asyncio
    async def test_durable_redirect_backfills_memory(
        self, monkeypatch, fresh_deduplicator
    ):
        """03-8: a durable-only mapping backfills memory; repeat lookups
        never re-read the old MBID durably.

        The hop budget re-checks the terminal hop durably on every lookup,
        and durable misses are positives-only by design (never backfilled),
        so the total durable count is not pinned. Backfill evidence is
        per-key: OLD is read durably exactly once across both lookups, and
        no wire call ever targets the old MBID.
        """
        repo = _Repo()
        mapping = {_OLD_ARTIST: _NEW_ARTIST}

        async def durable_read(kind, from_mbids, **kwargs):
            return {m: mapping[m] for m in from_mbids if m in mapping}

        store = SimpleNamespace(
            get_canonical_redirect=AsyncMock(side_effect=durable_read)
        )
        repo._mb_canonical_store = store
        calls: list[str] = []

        async def fake_get(path, params=None, **kwargs):
            calls.append(path)
            return {"id": _NEW_ARTIST, "name": "Survivor", "relations": []}

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        assert await repo.get_artist_core(_OLD_ARTIST) is not None
        assert calls == [f"/artist/{_NEW_ARTIST}"]

        backfilled = await artist_module.mb_cache_get_if_current(
            repo._cache,
            mb_redirect_key("artist", _OLD_ARTIST),
            artist_module.capture_mb_source_context(),
        )
        assert backfilled == _NEW_ARTIST

        # A second entry point resolves from the backfilled memory entry;
        # the durable store never sees the old MBID again.
        assert await repo.get_artist_relations(_OLD_ARTIST) is not None
        assert calls == [f"/artist/{_NEW_ARTIST}", f"/artist/{_NEW_ARTIST}"]
        old_reads = [
            c
            for c in store.get_canonical_redirect.await_args_list
            if c.args[1] == [_OLD_ARTIST]
        ]
        assert len(old_reads) == 1

    @pytest.mark.asyncio
    async def test_redirect_cycle_terminates(self, monkeypatch, fresh_deduplicator):
        repo = _Repo()
        await _seed_redirect(repo, _OLD_ARTIST, _NEW_ARTIST)
        await _seed_redirect(repo, _NEW_ARTIST, _OLD_ARTIST)
        calls: list[str] = []

        async def fake_get(path, params=None, **kwargs):
            calls.append(path)
            return {"id": _NEW_ARTIST, "name": "Survivor"}

        monkeypatch.setattr(artist_module, "mb_api_get", fake_get)

        assert await repo.get_artist_core(_OLD_ARTIST) is not None
        assert len(calls) == 1  # bounded hops: A->B->A resolves once to B

    @pytest.mark.asyncio
    async def test_redirect_to_404_uses_terminal_neg_cache(
        self, monkeypatch, fresh_deduplicator
    ):
        repo = _Repo()
        await _seed_redirect(repo, _OLD_ARTIST, _NEW_ARTIST)
        calls = {"n": 0}

        async def not_found_get(*args, **kwargs):
            calls["n"] += 1
            return {}

        monkeypatch.setattr(artist_module, "mb_api_get", not_found_get)

        assert await repo.get_artist_core(_OLD_ARTIST) is None
        assert await repo.get_artist_core(_OLD_ARTIST) is None
        assert calls["n"] == 1  # terminal 404 negative-cached under the new MBID
