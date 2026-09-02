"""Tests for discovery precache double-execution prevention and throttling."""

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from services.artist_discovery_service import ArtistDiscoveryService
import services.artist_discovery_service as _ads_module


@pytest.fixture(autouse=True)
def _reset_precache_flag():
    _ads_module._discovery_precache_running = False
    _ads_module._precache_consecutive_failures = 0
    _ads_module._precache_paused_until = 0.0
    yield
    _ads_module._discovery_precache_running = False
    _ads_module._precache_consecutive_failures = 0
    _ads_module._precache_paused_until = 0.0


def _make_service(
    *, lb_configured: bool = True, lastfm_enabled: bool = False,
    client_factory=None, auth_store=None, workload_gate=None,
):
    lb_repo = MagicMock()
    lb_repo.is_configured.return_value = lb_configured

    lastfm_repo = MagicMock() if lastfm_enabled else None
    prefs = MagicMock()
    prefs.is_lastfm_enabled.return_value = lastfm_enabled
    advanced = MagicMock()
    advanced.artist_discovery_precache_concurrency = 2
    prefs.get_advanced_settings.return_value = advanced

    cache = AsyncMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()

    library_db = AsyncMock()
    library_db.get_all_artist_mbids = AsyncMock(return_value=set())

    svc = ArtistDiscoveryService(
        listenbrainz_repo=lb_repo,
        musicbrainz_repo=MagicMock(),
        library_db=library_db,
        library_repo=MagicMock(),
        memory_cache=cache,
        lastfm_repo=lastfm_repo,
        preferences_service=prefs,
        client_factory=client_factory,
        auth_store=auth_store,
        workload_gate=workload_gate,
    )
    return svc


@pytest.mark.asyncio
async def test_duplicate_invocation_skipped():
    """Second call returns 0 immediately when precache is already running."""
    svc = _make_service()

    gate = asyncio.Event()

    async def slow_similar(*args, **kwargs):
        await gate.wait()
        return MagicMock()

    with (
        patch.object(svc, "get_similar_artists", new_callable=AsyncMock, side_effect=slow_similar),
        patch.object(svc, "get_top_songs", new_callable=AsyncMock, return_value=MagicMock()),
        patch.object(svc, "get_top_albums", new_callable=AsyncMock, return_value=MagicMock()),
    ):
        task1 = asyncio.create_task(
            svc.precache_artist_discovery(["mbid-a"], delay=0)
        )
        await asyncio.sleep(0.01)

        assert _ads_module._discovery_precache_running is True
        result2 = await svc.precache_artist_discovery(["mbid-b"], delay=0)
        assert result2 == 0

        gate.set()
        result1 = await task1
        assert result1 >= 0

    assert _ads_module._discovery_precache_running is False


@pytest.mark.asyncio
async def test_lock_released_after_exception():
    """Flag is cleared even when precache raises an unexpected error."""
    svc = _make_service()

    with patch.object(
        svc, "_do_precache_artist_discovery",
        new_callable=AsyncMock,
        side_effect=RuntimeError("boom"),
    ):
        with pytest.raises(RuntimeError, match="boom"):
            await svc.precache_artist_discovery(["mbid-a"], delay=0)

    assert _ads_module._discovery_precache_running is False

    with (
        patch.object(svc, "get_similar_artists", new_callable=AsyncMock, return_value=MagicMock()),
        patch.object(svc, "get_top_songs", new_callable=AsyncMock, return_value=MagicMock()),
        patch.object(svc, "get_top_albums", new_callable=AsyncMock, return_value=MagicMock()),
    ):
        result = await svc.precache_artist_discovery(["mbid-a"], delay=0)
        assert result >= 0


@pytest.mark.asyncio
async def test_precache_rechecks_background_gate_for_each_source() -> None:
    gate = MagicMock()
    gate.wait_until_available = AsyncMock()
    svc = _make_service(lastfm_enabled=True, workload_gate=gate)
    svc._cache.get = AsyncMock(return_value=object())

    await svc.precache_artist_discovery(["mbid-a"], delay=0)

    assert gate.wait_until_available.await_count == 2


@pytest.mark.asyncio
async def test_listenbrainz_fallback_and_expected_misses_reuse_lastfm_cache() -> None:
    svc = _make_service(lastfm_enabled=True)
    cache_values: dict[str, object] = {}

    async def cache_get(key: str):
        return cache_values.get(key)

    async def cache_set(key: str, value: object, ttl_seconds: int):
        assert ttl_seconds > 0
        cache_values[key] = value

    svc._cache.get = AsyncMock(side_effect=cache_get)
    svc._cache.set = AsyncMock(side_effect=cache_set)
    svc._lb_repo.get_similar_artists = AsyncMock(return_value=[])
    svc._lb_repo.get_artist_top_recordings = AsyncMock(return_value=[])
    svc._lb_repo.get_artist_top_release_groups = AsyncMock(return_value=[])
    svc._lastfm_repo.get_similar_artists = AsyncMock(return_value=[])
    svc._lastfm_repo.get_artist_top_tracks = AsyncMock(return_value=[])
    svc._lastfm_repo.get_artist_top_albums = AsyncMock(return_value=[])
    svc._library_repo.get_library_mbids = AsyncMock(return_value=set())
    svc._library_repo.get_requested_mbids = AsyncMock(return_value=set())
    svc._mb_repo.get_release_groups_by_artist = AsyncMock(return_value=[])

    await svc.precache_artist_discovery(["mbid-a"], delay=0)

    svc._lastfm_repo.get_similar_artists.assert_awaited_once()
    svc._lastfm_repo.get_artist_top_tracks.assert_awaited_once()
    svc._lastfm_repo.get_artist_top_albums.assert_awaited_once()
    assert {key for key in cache_values if key.endswith(":lastfm")} == {
        svc._build_cache_key("similar", "mbid-a", 15, "lastfm"),
        svc._build_cache_key("top_songs", "mbid-a", 10, "lastfm"),
        svc._build_cache_key("top_albums", "mbid-a", 10, "lastfm"),
    }


@pytest.mark.asyncio
async def test_delay_does_not_hold_semaphore_slot():
    """Delay is applied outside the semaphore, allowing other artists to start during sleep."""
    svc = _make_service()
    timestamps: list[float] = []

    loop = asyncio.get_event_loop()

    async def track_similar(*args, **kwargs):
        timestamps.append(loop.time())
        return MagicMock()

    with (
        patch.object(svc, "get_similar_artists", new_callable=AsyncMock, side_effect=track_similar),
        patch.object(svc, "get_top_songs", new_callable=AsyncMock, return_value=MagicMock()),
        patch.object(svc, "get_top_albums", new_callable=AsyncMock, return_value=MagicMock()),
    ):
        await svc.precache_artist_discovery(
            ["mbid-a", "mbid-b", "mbid-c", "mbid-d"],
            delay=0.15,
        )

    assert len(timestamps) == 4
    # With concurrency=2 and delay=0.15s OUTSIDE semaphore, the 3rd artist
    # can start as soon as a semaphore slot frees (before the delay finishes).
    # All four API calls should start quickly - the gap between 1st and 3rd
    # should be small since the semaphore isn't held during sleep.
    sorted_ts = sorted(timestamps)
    gap = sorted_ts[2] - sorted_ts[0]
    assert gap < 0.15, f"Expected <0.15s gap since sleep is outside semaphore, got {gap:.3f}s"


@pytest.mark.asyncio
async def test_cached_artists_skip_api_calls():
    """Artists with all cache keys populated skip API fetches entirely."""
    svc = _make_service()

    call_count = 0

    async def counting_similar(*args, **kwargs):
        nonlocal call_count
        call_count += 1
        return MagicMock()

    svc._cache.get = AsyncMock(return_value=MagicMock())

    with (
        patch.object(svc, "get_similar_artists", new_callable=AsyncMock, side_effect=counting_similar),
        patch.object(svc, "get_top_songs", new_callable=AsyncMock, return_value=MagicMock()),
        patch.object(svc, "get_top_albums", new_callable=AsyncMock, return_value=MagicMock()),
    ):
        result = await svc.precache_artist_discovery(
            ["mbid-a", "mbid-b"],
            delay=0,
        )

    assert call_count == 0, "Expected no API calls when all cache keys are populated"
    assert result == 2


@pytest.mark.asyncio
async def test_guard_survives_instance_recreation():
    """Module-level flag prevents overlap even when a new service instance is created."""
    svc1 = _make_service()
    svc2 = _make_service()

    gate = asyncio.Event()

    async def slow_similar(*args, **kwargs):
        await gate.wait()
        return MagicMock()

    with (
        patch.object(svc1, "get_similar_artists", new_callable=AsyncMock, side_effect=slow_similar),
        patch.object(svc1, "get_top_songs", new_callable=AsyncMock, return_value=MagicMock()),
        patch.object(svc1, "get_top_albums", new_callable=AsyncMock, return_value=MagicMock()),
    ):
        task1 = asyncio.create_task(
            svc1.precache_artist_discovery(["mbid-a"], delay=0)
        )
        await asyncio.sleep(0.01)

        result2 = await svc2.precache_artist_discovery(["mbid-b"], delay=0)
        assert result2 == 0, "Second instance should be blocked by module-level flag"

        gate.set()
        await task1

    assert _ads_module._discovery_precache_running is False


@pytest.mark.asyncio
async def test_worker_timeout_fires_and_updates_progress():
    """A worker that exceeds the per-artist timeout is killed and progress is updated."""
    svc = _make_service()

    async def hang_forever(*args, **kwargs):
        await asyncio.sleep(9999)
        return MagicMock()  # pragma: no cover

    status = MagicMock()
    status.is_cancelled = MagicMock(return_value=False)
    status.update_progress = AsyncMock()

    with (
        patch.object(svc, "get_similar_artists", new_callable=AsyncMock, side_effect=hang_forever),
        patch.object(svc, "get_top_songs", new_callable=AsyncMock, side_effect=hang_forever),
        patch.object(svc, "get_top_albums", new_callable=AsyncMock, side_effect=hang_forever),
        patch("services.artist_discovery_service._DISCOVERY_WORKER_TIMEOUT", 0.1),
    ):
        result = await svc.precache_artist_discovery(
            ["mbid-a"], delay=0, status_service=status,
        )

    assert result == 0
    assert status.update_progress.await_count >= 1
    last_call_args = status.update_progress.call_args
    assert "timed out" in str(last_call_args)


@pytest.mark.asyncio
async def test_precache_uses_first_admin_as_credential_source():
    """Precache warms a global cache, so it resolves the first admin and threads
    that id into the per-user fetches."""
    factory = MagicMock()
    factory.resolve_listenbrainz = AsyncMock(return_value=MagicMock())  # admin linked
    factory.resolve_lastfm = AsyncMock(return_value=None)
    auth_store = MagicMock()
    auth_store.get_first_admin = AsyncMock(return_value=SimpleNamespace(id="admin-1"))
    svc = _make_service(lb_configured=False, client_factory=factory, auth_store=auth_store)

    seen = {}

    async def capture(mbid, **kwargs):
        seen["user_id"] = kwargs.get("user_id")
        return MagicMock()

    with (
        patch.object(svc, "get_similar_artists", new_callable=AsyncMock, side_effect=capture),
        patch.object(svc, "get_top_songs", new_callable=AsyncMock, return_value=MagicMock()),
        patch.object(svc, "get_top_albums", new_callable=AsyncMock, return_value=MagicMock()),
    ):
        result = await svc.precache_artist_discovery(["mbid-a"], delay=0)

    assert result == 1
    assert seen["user_id"] == "admin-1"
    factory.resolve_listenbrainz.assert_awaited_with("admin-1")


@pytest.mark.asyncio
async def test_precache_skips_when_no_linked_admin():
    """A factory with no linked admin has no credential source -> skip (return 0)."""
    factory = MagicMock()
    factory.resolve_listenbrainz = AsyncMock(return_value=None)
    factory.resolve_lastfm = AsyncMock(return_value=None)
    auth_store = MagicMock()
    auth_store.get_first_admin = AsyncMock(return_value=None)
    svc = _make_service(lb_configured=False, client_factory=factory, auth_store=auth_store)

    with (
        patch.object(svc, "get_similar_artists", new_callable=AsyncMock) as sim,
        patch.object(svc, "get_top_songs", new_callable=AsyncMock),
        patch.object(svc, "get_top_albums", new_callable=AsyncMock),
    ):
        result = await svc.precache_artist_discovery(["mbid-a"], delay=0)

    assert result == 0
    sim.assert_not_called()
