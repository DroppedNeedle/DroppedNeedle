"""Proactive per-user Discover/Home warmer (core/tasks.py): eligibility, neediest-user
ordering, per-user warm (prewarm -> registered build -> attempt tracking), dedup with the
on-visit SWR build via the shared TaskRegistry name, and the loop's disabled kill switch."""

import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from core import tasks as T
from core.task_registry import TaskRegistry


def _user(uid: str):
    return MagicMock(id=uid)


def _auth_store(uids: list[str]):
    store = MagicMock()
    # single short page terminates enumeration
    store.list_users = AsyncMock(side_effect=lambda limit, offset: (
        [_user(u) for u in uids] if offset == 0 else []
    ))
    return store


@pytest.mark.asyncio
async def test_enumerate_includes_users_without_a_linked_music_source():
    store = _auth_store(["u1", "u2", "u3"])

    eligible = await T._enumerate_warmer_users(store)

    assert eligible == ["u1", "u2", "u3"]


@pytest.mark.asyncio
async def test_pick_due_prefers_never_warmed_then_personalizing_then_stale():
    discover = MagicMock()
    # u_personalizing has a cache and is still personalising; u_stale is old but converged
    discover.peek_freshness = AsyncMock(side_effect=lambda uid: {
        "u_personalizing": (True, True),
        "u_stale": (True, False),
    }[uid])
    now = 10_000.0
    last_warmed = {
        "u_personalizing": now - (T.DISCOVER_WARMER_PERSONALIZING_RETRY + 10),
        "u_stale": now - (T.DISCOVER_WARMER_REFRESH_INTERVAL + 10),
    }
    # never-warmed wins outright
    picked = await T._pick_due_warmer_user(
        ["u_stale", "u_never", "u_personalizing"], last_warmed, {}, now, discover
    )
    assert picked == "u_never"

    # without a never-warmed user, a still-personalising user beats a merely-stale one
    picked = await T._pick_due_warmer_user(
        ["u_stale", "u_personalizing"], last_warmed, {}, now, discover
    )
    assert picked == "u_personalizing"


@pytest.mark.asyncio
async def test_pick_due_retries_warmed_user_with_no_cache():
    # a thorough warm cut at the 300s hard cap (heavy user, mid-outage) caches nothing ->
    # peek (False, False); it must stay in the fast-retry tier, not be treated as converged
    discover = MagicMock()
    discover.peek_freshness = AsyncMock(return_value=(False, False))
    now = 10_000.0
    last_warmed = {"u1": now - (T.DISCOVER_WARMER_PERSONALIZING_RETRY + 10)}
    picked = await T._pick_due_warmer_user(["u1"], last_warmed, {}, now, discover)
    assert picked == "u1"


@pytest.mark.asyncio
async def test_pick_due_skips_user_with_a_live_on_visit_build():
    registry = TaskRegistry.get_instance()
    live = asyncio.create_task(asyncio.sleep(5))
    registry.register("discover-homepage-warm-u1", live)
    try:
        discover = MagicMock()
        discover.peek_freshness = AsyncMock(return_value=(False, False))
        picked = await T._pick_due_warmer_user(["u1"], {}, {}, 0.0, discover)
        assert picked is None  # u1 is being built by a live GET, so it's skipped
    finally:
        live.cancel()
        registry.unregister("discover-homepage-warm-u1")


@pytest.mark.asyncio
async def test_warm_one_user_runs_thorough_build_and_tracks_attempts():
    discover = MagicMock()
    discover.warm_cache_thorough = AsyncMock()
    # still personalising after the build -> attempts increments
    discover.peek_freshness = AsyncMock(return_value=(True, True))
    home = MagicMock()
    home.warm_cache = AsyncMock()
    queue = MagicMock()
    queue.start_build = AsyncMock()
    queue.wait_for_build = AsyncMock()
    last_warmed: dict = {}
    attempts: dict = {}

    await T._warm_one_user("u1", discover, home, last_warmed, attempts, queue)

    discover.warm_cache_thorough.assert_awaited_once_with("u1")
    home.warm_cache.assert_awaited_once_with("u1")
    queue.start_build.assert_awaited_once_with("u1")
    queue.wait_for_build.assert_awaited_once_with("u1")
    assert "u1" in last_warmed
    assert attempts["u1"] == 1

    # a converged build resets attempts
    discover.peek_freshness = AsyncMock(return_value=(True, False))
    await T._warm_one_user("u1", discover, home, last_warmed, attempts)
    assert attempts["u1"] == 0


@pytest.mark.asyncio
async def test_warm_one_user_skips_when_live_build_running():
    registry = TaskRegistry.get_instance()
    live = asyncio.create_task(asyncio.sleep(5))
    registry.register("discover-homepage-warm-u1", live)
    try:
        discover = MagicMock()
        discover.warm_cache_thorough = AsyncMock()
        home = MagicMock()
        home.warm_cache = AsyncMock()

        await T._warm_one_user("u1", discover, home, {}, {})

        discover.warm_cache_thorough.assert_not_awaited()
    finally:
        live.cancel()
        registry.unregister("discover-homepage-warm-u1")


@pytest.mark.asyncio
async def test_loop_disabled_kill_switch_warms_nobody_and_still_sleeps():
    settings = MagicMock(discover_warmer_enabled=False)
    sleeps: list = []

    async def fake_sleep(d):
        sleeps.append(d)
        if len(sleeps) >= 2:  # startup sleep + one loop sleep, then stop
            raise asyncio.CancelledError()

    get_discover = MagicMock()
    with patch("core.tasks.asyncio.sleep", side_effect=fake_sleep), \
         patch("core.config.get_settings", return_value=settings):
        await T.warm_discover_home_periodically(
            get_discover, MagicMock(), MagicMock()
        )

    assert len(sleeps) >= 2  # slept (startup + loop) despite doing no work
    get_discover.assert_not_called()  # never resolved the service while disabled


# the two homepage-service methods the warmer drives
from api.v1.schemas.discover import DiscoverResponse, TopPicksSection  # noqa: E402
from services.discover.homepage_service import DiscoverHomepageService  # noqa: E402


def _homepage() -> DiscoverHomepageService:
    svc = DiscoverHomepageService.__new__(DiscoverHomepageService)
    svc._memory_cache = None
    svc._lfm_repo = MagicMock()
    svc._mbid = MagicMock()
    svc._integration = MagicMock()
    svc._integration.is_jellyfin_enabled.return_value = False
    svc._integration.get_discover_cache_key = MagicMock(return_value="discover:u1")
    svc._resolve_user_music = AsyncMock(
        return_value=(None, None, "lbuser", "lfmuser", True, True, "listenbrainz")
    )
    return svc


def _seeded(svc):
    from types import SimpleNamespace
    svc._lb_repo = MagicMock()
    svc._lb_repo.get_artist_top_release_groups = AsyncMock(return_value=[])
    svc._get_seed_artists = AsyncMock(return_value=[SimpleNamespace(artist_mbids=["seed-1"])])
    return svc


@pytest.mark.asyncio
async def test_thorough_warm_runs_thorough_and_probes_lb():
    from services.discover.mbid_resolution_service import discover_build_thorough

    svc = _seeded(_homepage())
    seen: dict = {}

    async def fake_warm(uid):
        seen["thorough"] = discover_build_thorough.get()  # flag in effect DURING the build

    svc.warm_cache = fake_warm

    await svc.warm_cache_thorough("u1")

    assert seen["thorough"] is True  # build ran thorough (relaxed budgets + uncapped lookups)
    assert discover_build_thorough.get() is False  # reset afterwards
    svc._lb_repo.get_artist_top_release_groups.assert_awaited_once()  # probed LB to fix the gate


@pytest.mark.asyncio
async def test_thorough_warm_survives_a_failing_probe():
    # the probe 500s during the outage (raises ServiceDisabledUpstreamError); its gate side
    # effect already fired, so the warm must proceed regardless
    svc = _seeded(_homepage())
    svc._lb_repo.get_artist_top_release_groups = AsyncMock(side_effect=RuntimeError("500"))
    svc.warm_cache = AsyncMock()

    await svc.warm_cache_thorough("u1")  # must not raise

    svc.warm_cache.assert_awaited_once_with("u1")


@pytest.mark.asyncio
async def test_peek_freshness_reports_personalizing_from_cache():
    svc = _homepage()
    store = {
        "discover:u1": DiscoverResponse(top_picks=TopPicksSection(personalizing=True))
    }
    svc._memory_cache = MagicMock()
    svc._memory_cache.get = AsyncMock(side_effect=lambda k: store.get(k))

    assert await svc.peek_freshness("u1") == (True, True)

    store.clear()
    assert await svc.peek_freshness("u1") == (False, False)  # no cache


@pytest.mark.asyncio
async def test_peek_freshness_degraded_empty_top_picks_still_converging():
    # both-pools-empty degraded build caches top_picks=None; while degraded that means "keep
    # warming", not "converged", so peek must report still_converging=True
    from api.v1.schemas.discover import DiscoverResponse as _DR

    svc = _homepage()
    svc._use_lastfm_for_popularity = MagicMock(return_value=True)  # LB popularity degraded
    store = {"discover:u1": _DR(top_picks=None)}
    svc._memory_cache = MagicMock()
    svc._memory_cache.get = AsyncMock(side_effect=lambda k: store.get(k))

    assert await svc.peek_freshness("u1") == (True, True)

    # not degraded -> top_picks=None is genuinely converged-with-nothing, don't keep warming
    svc._use_lastfm_for_popularity = MagicMock(return_value=False)
    assert await svc.peek_freshness("u1") == (True, False)
