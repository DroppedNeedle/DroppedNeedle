import asyncio
import threading
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import msgspec
import pytest

from infrastructure.persistence.discovery_snapshot_store import DiscoverySnapshotStore
from infrastructure.observability.optional_work import reserve_optional_operation
from repositories.musicbrainz_base import capture_mb_source_context
from services.discover.demand_service import DiscoveryDemandService
from services.native.background_workload_gate import BackgroundWorkloadGate


@pytest.fixture
def demand(tmp_path, monkeypatch):
    monkeypatch.setattr('services.discover.demand_service.get_settings', lambda: SimpleNamespace(discover_warmer_enabled=True))
    store = DiscoverySnapshotStore(tmp_path / 'demand.sqlite', threading.Lock())
    discover = SimpleNamespace(warm_cache=AsyncMock())
    home = SimpleNamespace(warm_cache=AsyncMock())
    queue = SimpleNamespace(scheduled_enabled=lambda: True, start_build=AsyncMock(), wait_for_build=AsyncMock())
    artist = SimpleNamespace(warm_requested_section=AsyncMock())
    auth = SimpleNamespace(get_user_by_id=AsyncMock(return_value=SimpleNamespace(id='u')))
    service = DiscoveryDemandService(
        store, lambda: discover, lambda: home, lambda: queue, lambda: artist, lambda: auth,
        workload_gate=BackgroundWorkloadGate(),
    )
    return service, store, discover, home, queue, artist, auth


async def enroll(store, feature, *, user='u', age=0):
    source = capture_mb_source_context()
    key = msgspec.json.encode((source.source_mode, source.source_id, source.generation)).decode()
    await store.record_activity(user, feature, key, time.time() - age)


@pytest.mark.asyncio
async def test_missing_expired_and_deleted_users_do_not_warm(demand):
    service, store, discover, home, queue, artist, auth = demand
    await service.run_due_tick()
    await enroll(store, 'discover', age=86401)
    await service.run_due_tick()
    await enroll(store, 'home', user='deleted')
    auth.get_user_by_id.return_value = None
    await service.run_due_tick()
    assert discover.warm_cache.await_count == home.warm_cache.await_count == 0
    assert queue.start_build.await_count == artist.warm_requested_section.await_count == 0


@pytest.mark.asyncio
async def test_disabled_queue_does_not_disable_home_and_success_survives_restart(demand):
    service, store, discover, home, queue, artist, auth = demand
    queue.scheduled_enabled = lambda: False
    await enroll(store, 'home')
    await enroll(store, 'queue')
    await service.run_due_tick()
    replacement = DiscoveryDemandService(
        store, lambda: discover, lambda: home, lambda: queue, lambda: artist, lambda: auth,
        workload_gate=BackgroundWorkloadGate(),
    )
    await replacement.run_due_tick()
    assert home.warm_cache.await_count == 1
    assert queue.start_build.await_count == 0


@pytest.mark.asyncio
async def test_features_share_ten_operations_and_yield_without_success(demand):
    service, store, discover, home, queue, artist, auth = demand
    operations = []
    async def work(user):
        for _ in range(7):
            token = reserve_optional_operation()
            token.mark_dispatched()
            operations.append(user)
        return True
    discover.warm_cache.side_effect = work
    home.warm_cache.side_effect = work
    await enroll(store, 'discover')
    await enroll(store, 'home')
    await service.run_due_tick()
    assert operations == ['u'] * 10
    source = capture_mb_source_context()
    key = msgspec.json.encode((source.source_mode, source.source_id, source.generation)).decode()
    rows = await store.get_due_activity(key, time.time() + 91)
    assert [row['feature'] for row in rows] == ['home']
    assert rows[0]['last_success'] == 0


@pytest.mark.asyncio
async def test_concurrent_entry_and_tick_do_not_create_second_user_budget(demand):
    service, store, discover, home, queue, artist, auth = demand
    entered, release = asyncio.Event(), asyncio.Event()
    async def work(user):
        entered.set()
        await release.wait()
    discover.warm_cache.side_effect = work
    await enroll(store, 'discover')
    first = asyncio.create_task(service.run_due_tick())
    await entered.wait()
    await service.run_due_tick('u')
    release.set()
    await first
    assert discover.warm_cache.await_count == 1


@pytest.mark.asyncio
async def test_fair_user_order_does_not_repeat_recent_success(demand):
    service, store, discover, home, queue, artist, auth = demand
    await enroll(store, 'home', user='a')
    await enroll(store, 'home', user='b')
    await service.run_due_tick()
    await service.run_due_tick()
    assert [call.args[0] for call in home.warm_cache.await_args_list] == ['a', 'b']


class _GateEntrySignal(BackgroundWorkloadGate):
    """Real gate that signals once a warmer unit has reached gate admission."""

    def __init__(self, **kwargs) -> None:
        super().__init__(**kwargs)
        self.entered = asyncio.Event()

    async def run_warmer_unit[T](self, operation):
        self.entered.set()
        return await super().run_warmer_unit(operation)


@pytest.mark.asyncio
async def test_interactive_hold_defers_demand_warm_until_release(tmp_path, monkeypatch):
    monkeypatch.setattr('services.discover.demand_service.get_settings', lambda: SimpleNamespace(discover_warmer_enabled=True))
    store = DiscoverySnapshotStore(tmp_path / 'demand.sqlite', threading.Lock())
    home = SimpleNamespace(warm_cache=AsyncMock(return_value=True))
    discover = SimpleNamespace(warm_cache=AsyncMock(return_value=True))
    gate = _GateEntrySignal(interactive_quiet_period=0.0, interactive_max_deferral=30.0)
    service = DiscoveryDemandService(
        store, lambda: discover, lambda: home,
        lambda: SimpleNamespace(scheduled_enabled=lambda: True, start_build=AsyncMock(), wait_for_build=AsyncMock()),
        lambda: SimpleNamespace(warm_requested_section=AsyncMock()),
        lambda: SimpleNamespace(get_user_by_id=AsyncMock(return_value=SimpleNamespace(id='u'))),
        workload_gate=gate,
    )
    await enroll(store, 'home')
    await enroll(store, 'discover')

    gate.begin_interactive_request()
    tick = asyncio.create_task(service.run_due_tick('u'))
    try:
        await asyncio.wait_for(gate.entered.wait(), timeout=5.0)
        assert home.warm_cache.await_count == 0
        assert discover.warm_cache.await_count == 0
    finally:
        gate.end_interactive_request()
        try:
            await asyncio.wait_for(tick, timeout=5.0)
        except asyncio.TimeoutError:
            tick.cancel()
            await asyncio.gather(tick, return_exceptions=True)
            raise
    assert home.warm_cache.await_count == 1
    assert discover.warm_cache.await_count == 1


@pytest.mark.asyncio
async def test_successful_demand_warm_records_success_through_gate(tmp_path, monkeypatch):
    monkeypatch.setattr('services.discover.demand_service.get_settings', lambda: SimpleNamespace(discover_warmer_enabled=True))
    store = DiscoverySnapshotStore(tmp_path / 'demand.sqlite', threading.Lock())
    home = SimpleNamespace(warm_cache=AsyncMock(return_value=True))
    gate = BackgroundWorkloadGate()
    service = DiscoveryDemandService(
        store, lambda: home, lambda: home,
        lambda: SimpleNamespace(scheduled_enabled=lambda: True, start_build=AsyncMock(), wait_for_build=AsyncMock()),
        lambda: SimpleNamespace(warm_requested_section=AsyncMock()),
        lambda: SimpleNamespace(get_user_by_id=AsyncMock(return_value=SimpleNamespace(id='u'))),
        workload_gate=gate,
    )
    await enroll(store, 'home')
    await service.run_due_tick('u')

    source = capture_mb_source_context()
    key = msgspec.json.encode((source.source_mode, source.source_id, source.generation)).decode()
    rows = await store.get_due_activity(key, time.time() + 6 * 3600 + 1)
    assert home.warm_cache.await_count == 1
    assert len(rows) == 1
    assert rows[0]['last_success'] > 0


from api.v1.schemas.discover import DiscoverResponse, TopPicksSection
from services.discover.homepage_service import DiscoverHomepageService


def _homepage():
    service = DiscoverHomepageService.__new__(DiscoverHomepageService)
    service._memory_cache = None
    service._workload_gate = None
    service._lfm_repo = MagicMock()
    service._mbid = MagicMock()
    service._integration = MagicMock()
    service._integration.is_jellyfin_enabled.return_value = False
    service._integration.get_discover_cache_key.return_value = 'discover:u1'
    service._resolve_user_music = AsyncMock(return_value=(None, None, 'lbuser', 'lfmuser', True, True, 'listenbrainz'))
    return service


@pytest.mark.asyncio
async def test_peek_freshness_reports_personalizing_from_cache():
    service = _homepage()
    values = {'discover:u1': DiscoverResponse(top_picks=TopPicksSection(personalizing=True))}
    service._memory_cache = MagicMock()
    service._memory_cache.get = AsyncMock(side_effect=lambda key: values.get(key))
    assert await service.peek_freshness('u1') == (True, True)
    values.clear()
    assert await service.peek_freshness('u1') == (False, False)


@pytest.mark.asyncio
async def test_peek_freshness_degraded_empty_top_picks_still_converging():
    service = _homepage()
    service._use_lastfm_for_popularity = MagicMock(return_value=True)
    service._memory_cache = MagicMock()
    service._memory_cache.get = AsyncMock(return_value=DiscoverResponse(top_picks=None))
    assert await service.peek_freshness('u1') == (True, True)
    service._use_lastfm_for_popularity.return_value = False
    assert await service.peek_freshness('u1') == (True, False)
