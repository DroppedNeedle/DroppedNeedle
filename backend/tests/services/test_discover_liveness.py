"""Whole-operation admission, partial publication, and honest terminal responses."""
import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from infrastructure.observability.optional_work import (
    OptionalWorkBudget, OptionalWorkDeferred, optional_work_budget,
    reserve_optional_operation,
)
from services.discover.demand_service import DiscoveryDemandService
from services.native.background_workload_gate import BackgroundWorkloadGate
from tests.services.test_discover_degraded_terminal import (
    _FakeCache, _make_service, _meaningful_response, _cache_key,
)


async def _turns(count=20):
    for _ in range(count):
        await asyncio.sleep(0)


@pytest.mark.asyncio
@pytest.mark.parametrize('entry', ['direct', 'triggered', 'demand'])
async def test_actual_poll_pattern_admits_one_discover_build_before_feature_deadline(monkeypatch, entry):
    # Deterministic clock: real gate, real Discover entry methods, actual 5s
    # downloads + 10s Discover requests. Completed requests restart the 5s quiet
    # window before it can expire. No scaled sleeps or 120s wall-clock test.
    clock = [0.0]
    monkeypatch.setattr('services.native.background_workload_gate.monotonic', lambda: clock[0])
    gate = BackgroundWorkloadGate()
    cache = _FakeCache()
    service = _make_service(cache)
    service._workload_gate = gate
    checkpoints = []
    async def build(user):
        for _ in range(6):
            await service._wait_for_background_window()
            checkpoints.append(clock[0])
        return _meaningful_response()
    service.build_discover_data = build
    gate.begin_interactive_request()
    gate.end_interactive_request()
    if entry == 'direct':
        operation = service.warm_cache('u1')
    elif entry == 'triggered':
        from repositories.musicbrainz_base import capture_mb_source_context
        operation = service._run_triggered_warm('u1', capture_mb_source_context(), None)
    else:
        demand = DiscoveryDemandService(
            None, lambda: service, None, None, None,
            lambda: SimpleNamespace(get_user_by_id=AsyncMock(return_value=object())),
            workload_gate=gate,
        )
        operation = demand._run_feature({'user_id': 'u1', 'feature': 'discover'})
    task = asyncio.create_task(operation)
    await _turns()
    try:
        for second in range(1, 301):
            clock[0] = float(second)
            if second % 5 == 0:
                gate.begin_interactive_request()
                gate.end_interactive_request()
            if second % 10 == 0:
                gate.begin_interactive_request()
                gate.end_interactive_request()
            await _turns()
            if task.done():
                break
        assert task.done(), 'Discover spent its feature deadline in repeated gate admission'
        await task
        assert checkpoints == [120.0] * 6
        assert cache.data[_cache_key(service)].globally_trending.items
        assert not service._building_keys
        assert not service._refresh_started_at
        assert not gate._warmer_slot.locked()
    finally:
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)


@pytest.mark.asyncio
async def test_budget_deferral_keeps_completed_sections_and_capped_dispatches():
    cache = _FakeCache()
    service = _make_service(cache)
    dispatched = []
    async def section(index):
        token = reserve_optional_operation()
        token.mark_dispatched()
        dispatched.append(index)
        return _meaningful_response().globally_trending
    async def build(user):
        results = await service._execute_tasks({str(i): section(i) for i in range(11)})
        from api.v1.schemas.discover import DiscoverResponse
        assert len([x for x in results.values() if x is not None]) == 10
        return DiscoverResponse(globally_trending=results['0'])
    service.build_discover_data = build
    budget = OptionalWorkBudget()
    with optional_work_budget(budget):
        assert await service.warm_cache('u1') is True
    assert len(dispatched) == 10
    assert budget.remaining == 0
    assert service._last_failed_task_keys == set()
    assert cache.data[_cache_key(service)].globally_trending.items


@pytest.mark.asyncio
async def test_invalidated_guard_discards_successful_partial_results():
    cache = _FakeCache()
    service = _make_service(cache)
    active = [True]
    async def success():
        return _meaningful_response()
    async def invalidated():
        active[0] = False
        raise OptionalWorkDeferred()
    async def build(user):
        await service._execute_tasks({'good': success(), 'invalid': invalidated()})
        return _meaningful_response()
    service.build_discover_data = build
    with optional_work_budget(OptionalWorkBudget(guard=lambda: active[0])):
        with pytest.raises(OptionalWorkDeferred):
            await service.warm_cache('u1')
    assert cache.data == {}


@pytest.mark.asyncio
async def test_source_switch_still_defers_optional_warm_without_publication(monkeypatch):
    from repositories import musicbrainz_base
    cache = _FakeCache()
    service = _make_service(cache)
    generation = musicbrainz_base._mb_source_generation

    async def build(user):
        monkeypatch.setattr(musicbrainz_base, '_mb_source_generation', generation + 1)
        return _meaningful_response()

    service.build_discover_data = build
    with optional_work_budget():
        with pytest.raises(OptionalWorkDeferred):
            await service.warm_cache('u1')
    assert cache.data == {}
    assert not service._building_keys
    assert not service._refresh_started_at


@pytest.mark.asyncio
async def test_cancelled_child_is_not_a_section_value_or_published_result():
    cache = _FakeCache()
    service = _make_service(cache)
    async def cancelled():
        raise asyncio.CancelledError()
    async def build(user):
        await service._execute_tasks({'cancelled': cancelled()})
        return _meaningful_response()
    service.build_discover_data = build
    with pytest.raises(asyncio.CancelledError):
        await service.warm_cache('u1')
    assert cache.data == {}
    assert not service._building_keys


@pytest.mark.asyncio
async def test_deadline_while_scan_blocked_settles_cold_response_and_releases_slot(monkeypatch):
    monkeypatch.setattr('services.discover.homepage_service.DISCOVER_BUILD_TIMEOUT_SECONDS', 0.01)
    gate = BackgroundWorkloadGate()
    gate.set_scan_active(True)
    service = _make_service(_FakeCache())
    service._workload_gate = gate
    build = AsyncMock(return_value=_meaningful_response())
    service.build_discover_data = build
    with pytest.raises(TimeoutError):
        await service.warm_cache('u1')
    build.assert_not_awaited()
    service._trigger_warm = lambda user: pytest.fail('cold backed-off poll retriggered')
    response = await service.get_discover_data('u1')
    assert response.refreshing is False
    assert set(response.section_status.values()) == {'empty'}
    assert not gate._warmer_slot.locked()
    assert not service._building_keys
    assert not service._refresh_started_at


@pytest.mark.asyncio
async def test_real_builder_publishes_local_section_when_other_sections_defer():
    from tests.services.test_discover_service import _make_service as make_facade
    from api.v1.schemas.home import HomeAlbum, HomeSection
    facade, lb, lfm, _ = make_facade(lb_enabled=False, lfm_enabled=False)
    service = facade._homepage
    cache = _FakeCache()
    service._memory_cache = cache
    service._get_seed_artists = AsyncMock(return_value=[])
    dispatched = []
    async def exhausted(*args, **kwargs):
        reservation = reserve_optional_operation()
        reservation.mark_dispatched()
        dispatched.append('external')
        return []
    async def initial(*args, **kwargs):
        for _ in range(10):
            await exhausted()
        return []
    lb.get_sitewide_top_artists.side_effect = initial
    service._build_top_picks = exhausted
    service._build_radio_sections = exhausted
    service._build_popular_in_genres = exhausted
    local = HomeSection(title='Anniversaries', type='albums', items=[HomeAlbum(name='Local')])
    service._build_anniversaries = AsyncMock(return_value=local)
    with optional_work_budget(OptionalWorkBudget()):
        assert await service.warm_cache('u1') is True
    response = cache.data[_cache_key(service)]
    assert response.anniversaries.items[0].name == 'Local'
    assert response.top_picks is None
    assert response.refreshing is False
    assert len(dispatched) == 10
    assert response.service_status is None


@pytest.mark.asyncio
@pytest.mark.parametrize('changed', ['integration', 'source', 'user'])
async def test_scan_timeout_backoff_does_not_block_a_different_context(monkeypatch, changed):
    from dataclasses import replace
    from repositories.musicbrainz_base import capture_mb_source_context
    monkeypatch.setattr('services.discover.homepage_service.DISCOVER_BUILD_TIMEOUT_SECONDS', 0.01)
    service = _make_service(_FakeCache())
    gate = BackgroundWorkloadGate()
    gate.set_scan_active(True)
    service._workload_gate = gate
    with pytest.raises(TimeoutError):
        await service.warm_cache('u1')
    if changed == 'integration':
        service._resolve_user_music = AsyncMock(return_value=(None, None, None, None, False, True, 'lastfm'))
    elif changed == 'source':
        next_source = replace(capture_mb_source_context(), generation=999)
        monkeypatch.setattr('services.discover.homepage_service.capture_mb_source_context', lambda: next_source)
    triggered = []
    service._trigger_warm = triggered.append
    user = 'u2' if changed == 'user' else 'u1'
    assert (await service.get_discover_data(user)).refreshing is True
    assert triggered == [user]
