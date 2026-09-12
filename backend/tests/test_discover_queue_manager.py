"""Tests for DiscoverQueueManager background queue building."""

import asyncio
import threading
import time
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.discover import (
    DiscoverQueueEnrichment,
    DiscoverQueueItemLight,
    DiscoverQueueResponse,
)
from repositories import musicbrainz_base as mb_base
from services.discover_queue_manager import DiscoverQueueManager
from services.native.background_workload_gate import BackgroundWorkloadGate
from infrastructure.persistence.discovery_snapshot_store import DiscoverySnapshotStore

from infrastructure.observability.optional_work import (
    OptionalWorkDeferred,
    optional_work_budget,
)
_UID = "u1"


def _make_queue(n: int = 3, queue_id: str = "test-queue-id") -> DiscoverQueueResponse:
    return DiscoverQueueResponse(
        items=[
            DiscoverQueueItemLight(
                release_group_mbid=f"mbid-{i}",
                album_name=f"Album {i}",
                artist_name=f"Artist {i}",
                artist_mbid=f"artist-{i}",
                cover_url=None,
                recommendation_reason="test",
            )
            for i in range(n)
        ],
        queue_id=queue_id,
    )


def _make_manager(
    queue: DiscoverQueueResponse | None = None,
    build_error: Exception | None = None,
    ttl: int = 86400,
    snapshot_store: DiscoverySnapshotStore | None = None,
) -> DiscoverQueueManager:
    discover = AsyncMock()
    if build_error:
        discover.build_queue.side_effect = build_error
    else:
        discover.build_queue.return_value = queue or _make_queue()
    discover.enrich_queue_item = AsyncMock(return_value=DiscoverQueueEnrichment())

    prefs = MagicMock()
    adv = MagicMock()
    adv.discover_queue_ttl = ttl
    adv.discover_queue_warm_cycle_build = True
    prefs.get_advanced_settings.return_value = adv

    return DiscoverQueueManager(discover, prefs, snapshot_store=snapshot_store)


@pytest.mark.asyncio
async def test_initial_status_is_idle():
    
    mgr = _make_manager()
    status = mgr.get_status(_UID)
    assert status.status == "idle"


@pytest.mark.asyncio
async def test_start_build_changes_status():
    
    mgr = _make_manager()
    result = await mgr.start_build(_UID)
    assert result.action == "started"
    assert result.status in ("building", "ready")
    await mgr.wait_for_build(_UID)


@pytest.mark.asyncio
async def test_build_produces_ready_queue():
    
    queue = _make_queue(5)
    mgr = _make_manager(queue=queue)
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    status = mgr.get_status(_UID)
    assert status.status == "ready"
    assert status.item_count == 5
    built_queue = mgr.get_queue(_UID)
    assert built_queue is not None
    assert [item.release_group_mbid for item in built_queue.items] == [
        f"mbid-{index}" for index in range(5)
    ]
    assert all(
        isinstance(item, DiscoverQueueItemLight) and not hasattr(item, "enrichment")
        for item in built_queue.items
    )
    mgr._discover.enrich_queue_item.assert_not_awaited()


@pytest.mark.asyncio
async def test_ready_queue_survives_manager_restart(tmp_path):
    snapshot_store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    first = _make_manager(snapshot_store=snapshot_store)
    await first.start_build(_UID)
    await first.wait_for_build(_UID)

    restarted = _make_manager(snapshot_store=snapshot_store)
    await restarted.ensure_loaded(_UID)

    assert restarted.get_status(_UID).status == "ready"
    queue = await restarted.consume_queue(_UID)
    assert queue is not None
    assert queue.queue_id == "test-queue-id"


@pytest.mark.asyncio
async def test_in_memory_shutdown_cleanup_keeps_durable_queue(tmp_path):
    snapshot_store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    first = _make_manager(snapshot_store=snapshot_store)
    await first.start_build(_UID)
    await first.wait_for_build(_UID)
    first.invalidate()

    restarted = _make_manager(snapshot_store=snapshot_store)
    await restarted.ensure_loaded(_UID)

    assert restarted.get_status(_UID).status == "ready"


@pytest.mark.asyncio
async def test_persisted_invalidation_marks_queue_stale(tmp_path):
    snapshot_store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    first = _make_manager(snapshot_store=snapshot_store)
    await first.start_build(_UID)
    await first.wait_for_build(_UID)
    await snapshot_store.mark_discover_stale()

    restarted = _make_manager(snapshot_store=snapshot_store)
    await restarted.ensure_loaded(_UID)

    assert restarted.get_status(_UID).stale is True


@pytest.mark.asyncio
async def test_lightweight_publication_does_not_wait_for_card_enrichment():
    mgr = _make_manager(queue=_make_queue(2))
    mgr._discover.enrich_queue_item.side_effect = AssertionError("card demand owns enrichment")
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    built_queue = await mgr.consume_queue(_UID)
    assert built_queue is not None
    assert [item.album_name for item in built_queue.items] == ["Album 0", "Album 1"]
    assert all(
        isinstance(item, DiscoverQueueItemLight) and not hasattr(item, "enrichment")
        for item in built_queue.items
    )
    mgr._discover.enrich_queue_item.assert_not_awaited()


@pytest.mark.asyncio
async def test_get_queue_returns_cached():
    
    queue = _make_queue(3)
    mgr = _make_manager(queue=queue)
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    result = mgr.get_queue(_UID)
    assert result is not None
    assert len(result.items) == 3


@pytest.mark.asyncio
async def test_consume_queue_retains_last_good_for_repeat_visits():
    
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    consumed = await mgr.consume_queue(_UID)
    assert consumed is not None
    assert len(consumed.items) == 3

    again = await mgr.consume_queue(_UID)
    assert again is not None
    assert again.queue_id == consumed.queue_id
    assert mgr.get_status(_UID).status == "ready"


@pytest.mark.asyncio
async def test_build_error_sets_error_status():
    
    mgr = _make_manager(build_error=RuntimeError("test fail"))
    await mgr.start_build(_UID)
    assert await mgr.wait_for_build(_UID) is False

    status = mgr.get_status(_UID)
    assert status.status == "error"
    assert "test fail" in (status.error or "")


@pytest.mark.asyncio
async def test_already_building_is_no_op():
    

    slow_discover = AsyncMock()

    async def slow_build(*args, **kwargs):
        await asyncio.sleep(5)
        return _make_queue()

    slow_discover.build_queue.side_effect = slow_build
    prefs = MagicMock()
    adv = MagicMock()
    adv.discover_queue_ttl = 86400
    prefs.get_advanced_settings.return_value = adv

    mgr = DiscoverQueueManager(slow_discover, prefs)
    await mgr.start_build(_UID)
    result = await mgr.start_build(_UID)
    assert result.action == "already_building"

    mgr.invalidate(_UID)


@pytest.mark.asyncio
async def test_force_rebuild_when_ready():
    
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    result = await mgr.start_build(_UID, force=True)
    assert result.action == "started"
    await mgr.wait_for_build(_UID)


@pytest.mark.asyncio
async def test_invalidate_resets_state():
    
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    mgr.invalidate(_UID)
    status = mgr.get_status(_UID)
    assert status.status == "idle"


@pytest.mark.asyncio
async def test_separate_users_are_independent():
    
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    built_status = mgr.get_status(_UID)
    other_status = mgr.get_status("someone-else")
    assert built_status.status == "ready"
    assert other_status.status == "idle"


@pytest.mark.asyncio
async def test_consume_queue_serves_stale_last_good():
    
    mgr = _make_manager(ttl=1)
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    assert mgr.get_status(_UID).status == "ready"

    mgr._get_state(_UID).built_at = time.time() - 10

    consumed = await mgr.consume_queue(_UID)
    assert consumed is not None
    assert consumed.queue_id == "test-queue-id"
    assert mgr.get_status(_UID).stale is True


@pytest.mark.asyncio
async def test_get_queue_rejects_stale():
    
    mgr = _make_manager(ttl=1)
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    assert mgr.get_queue(_UID) is not None

    mgr._get_state(_UID).built_at = time.time() - 10

    assert mgr.get_queue(_UID) is None


@pytest.mark.asyncio
async def test_stale_flag_in_status():
    
    mgr = _make_manager(ttl=1)
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    status = mgr.get_status(_UID)
    assert status.stale is False

    mgr._get_state(_UID).built_at = time.time() - 10

    status = mgr.get_status(_UID)
    assert status.stale is True


@pytest.mark.asyncio
async def test_build_leaves_cover_fetches_to_visible_card_demand():
    mgr = _make_manager()
    cover_repo = AsyncMock()
    cover_repo.get_release_group_cover.side_effect = AssertionError("unexpected cover warm")
    mgr._cover_repo = cover_repo
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    queue = await mgr.consume_queue(_UID)
    assert queue is not None
    assert [item.release_group_mbid for item in queue.items] == ["mbid-0", "mbid-1", "mbid-2"]
    cover_repo.get_release_group_cover.assert_not_awaited()


@pytest.mark.asyncio
async def test_source_switch_fences_old_build_and_snapshot():
    original_source = mb_base.capture_mb_source_context()
    original_source_id = mb_base.get_mb_source_id()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    old_generation = original_source.generation + 1
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="discover-queue-old",
        generation=old_generation,
    )
    old_started = asyncio.Event()
    old_gate = asyncio.Event()

    old_queue = _make_queue(1, queue_id="old-queue")
    new_queue = _make_queue(1, queue_id="new-queue")

    async def build_queue(*_args, **_kwargs):
        if mb_base.get_mb_source_generation() == old_generation:
            old_started.set()
            await old_gate.wait()
            return old_queue
        return new_queue

    discover = AsyncMock()
    discover.build_queue.side_effect = build_queue
    discover.enrich_queue_item = AsyncMock(return_value=DiscoverQueueEnrichment())
    prefs = MagicMock()
    adv = MagicMock()
    adv.discover_queue_ttl = 86400
    prefs.get_advanced_settings.return_value = adv
    snapshot_store = AsyncMock()
    snapshot_store.user_lease = MagicMock(return_value=MagicMock(active=True))
    snapshot_store.get_with_stale.return_value = None
    snapshot_store.delete_source_dependent_snapshots = AsyncMock()
    mgr = DiscoverQueueManager(discover, prefs, snapshot_store=snapshot_store)

    try:
        await mgr.start_build(_UID)
        await old_started.wait()
        old_state = mgr._states[_UID]
        old_task = old_state.task

        async with mb_base.mb_source_commit_lock:
            await snapshot_store.delete_source_dependent_snapshots()
            mb_base.set_mb_api_base(
                "https://new.example/ws/2",
                source_mode="mirror",
                source_id="discover-queue-new",
                generation=old_generation + 1,
            )
        assert snapshot_store.delete_source_dependent_snapshots.await_count == 1

        result = await mgr.start_build(_UID)
        assert result.action == "started"
        new_task = mgr._states[_UID].task
        assert new_task is not old_task
        await mgr.wait_for_build(_UID)

        old_gate.set()
        with pytest.raises(OptionalWorkDeferred):
            await old_task

        built_queue = mgr.get_queue(_UID)
        assert built_queue is not None
        assert built_queue.queue_id == "new-queue"
        assert snapshot_store.save.await_count == 1
        assert snapshot_store.save.await_args.args[0] == "discover_queue:u1"
        assert b"new-queue" in snapshot_store.save.await_args.args[2]
        assert b"old-queue" not in snapshot_store.save.await_args.args[2]
        assert snapshot_store.save.await_args.args[3] == mgr._states[_UID].built_at
    finally:
        old_gate.set()
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )


@pytest.mark.asyncio
async def test_consuming_queue_preserves_snapshot_for_restart(tmp_path):
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    mgr = _make_manager(snapshot_store=store)
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)
    before = await store.get("discover_queue:u1")
    consumed = await mgr.consume_queue(_UID)

    assert consumed is not None
    assert await store.get("discover_queue:u1") == before
    restarted = _make_manager(snapshot_store=store)
    loaded = await restarted.consume_queue(_UID)
    assert loaded is not None
    assert loaded.queue_id == consumed.queue_id


@pytest.mark.asyncio
async def test_scheduled_switch_blocks_background_but_not_explicit_generation():
    mgr = _make_manager()
    mgr._preferences.get_advanced_settings.return_value.discover_queue_warm_cycle_build = False
    skipped = await mgr.start_build(_UID, scheduled=True)
    assert skipped.action == "disabled"
    mgr._discover.build_queue.assert_not_awaited()

    explicit = await mgr.start_build(_UID, force=True)
    await mgr.wait_for_build(_UID)
    assert explicit.action == "started"
    assert (await mgr.consume_queue(_UID)).queue_id == "test-queue-id"


@pytest.mark.asyncio
async def test_scheduled_refresh_keeps_fresh_twenty_four_hour_queue():
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)
    mgr._get_state(_UID).built_at = time.time() - 21600
    mgr._discover.build_queue.reset_mock()

    response = await mgr.start_build(_UID, scheduled=True)

    assert response.action == "already_ready"
    assert (await mgr.consume_queue(_UID)).queue_id == "test-queue-id"
    mgr._discover.build_queue.assert_not_awaited()


@pytest.mark.asyncio
async def test_switch_disabled_during_build_keeps_last_good_and_durable_snapshot(tmp_path):
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    mgr = _make_manager(snapshot_store=store)
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)
    snapshot = await store.get("discover_queue:u1")
    mgr._get_state(_UID).built_at = time.time() - 86401
    started = asyncio.Event()
    release = asyncio.Event()

    async def build(*args, **kwargs):
        started.set()
        await release.wait()
        return _make_queue(queue_id="must-not-publish")

    mgr._discover.build_queue.side_effect = build
    with optional_work_budget():
        await mgr.start_build(_UID, scheduled=True)
    task = mgr._states[_UID].task
    await started.wait()
    try:
        assert (await mgr.consume_queue(_UID)).queue_id == "test-queue-id"
        mgr._preferences.get_advanced_settings.return_value.discover_queue_warm_cycle_build = False
    finally:
        release.set()
        outcome = (await asyncio.gather(task, return_exceptions=True))[0]
    assert outcome is None or isinstance(outcome, OptionalWorkDeferred)
    assert (await mgr.consume_queue(_UID)).queue_id == "test-queue-id"
    assert await store.get("discover_queue:u1") == snapshot
    assert mgr.get_status(_UID).status == "ready"


@pytest.mark.asyncio
async def test_cancelled_scheduled_waiter_does_not_cancel_foreground_build():
    from services.discover.demand_service import DiscoveryDemandService

    mgr = _make_manager()
    entered = asyncio.Event()
    release = asyncio.Event()

    async def build(*args, **kwargs):
        entered.set()
        await release.wait()
        return _make_queue(queue_id="foreground-survived")

    mgr._discover.build_queue.side_effect = build
    auth = AsyncMock()
    demand = DiscoveryDemandService(
        None, None, None, lambda: mgr, None, lambda: auth,
        workload_gate=BackgroundWorkloadGate(),
    )
    await mgr.start_build(_UID)
    await entered.wait()
    foreground = mgr._states[_UID].task
    waiter = asyncio.create_task(demand._run_feature({"user_id": _UID, "feature": "queue"}))
    await asyncio.sleep(0)
    waiter.cancel()
    with pytest.raises(asyncio.CancelledError):
        await waiter
    try:
        assert not foreground.cancelled()
        release.set()
        assert await mgr.wait_for_build(_UID) is True
        assert (await mgr.consume_queue(_UID)).queue_id == "foreground-survived"
    finally:
        release.set()
        await asyncio.gather(foreground, return_exceptions=True)


@pytest.mark.asyncio
async def test_budget_deferral_preserves_last_good_queue(tmp_path):
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    mgr = _make_manager(snapshot_store=store)
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)
    snapshot = await store.get("discover_queue:u1")
    mgr._get_state(_UID).built_at = time.time() - 86401
    mgr._discover.build_queue.side_effect = OptionalWorkDeferred()

    with optional_work_budget():
        await mgr.start_build(_UID, scheduled=True)
    task = mgr._states[_UID].task
    with pytest.raises(OptionalWorkDeferred):
        await task

    assert (await mgr.consume_queue(_UID)).queue_id == "test-queue-id"
    assert mgr.get_status(_UID).status == "ready"
    assert await store.get("discover_queue:u1") == snapshot


@pytest.mark.asyncio
async def test_failed_scheduled_build_retries_instead_of_recording_success(tmp_path, monkeypatch):
    from types import SimpleNamespace
    from services.discover import demand_service

    monkeypatch.setattr(demand_service, "get_settings", lambda: SimpleNamespace(discover_warmer_enabled=True))
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    now = time.time()
    await store.record_activity(_UID, "queue", "source", now)
    rows = await store.get_due_activity("source", now)
    mgr = _make_manager(build_error=RuntimeError("provider unavailable"))
    auth = AsyncMock()
    demand = demand_service.DiscoveryDemandService(
        store, None, None, lambda: mgr, None, lambda: auth,
        workload_gate=BackgroundWorkloadGate(),
    )
    await demand._run_user(_UID, rows, mb_base.capture_mb_source_context())
    retry = (await store.get_due_activity("source", now + 120))[0]
    assert retry["last_success"] == 0
    assert retry["retry_at"] <= now + 120
    assert mgr.get_status(_UID).status == "error"


@pytest.mark.asyncio
async def test_explicit_generation_takes_over_scheduled_build_without_losing_last_good():
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)
    mgr._get_state(_UID).built_at = time.time() - 86401
    scheduled_started = asyncio.Event()
    explicit_started = asyncio.Event()
    release_explicit = asyncio.Event()
    calls = 0

    async def build(*args, **kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            scheduled_started.set()
            await asyncio.Event().wait()
        explicit_started.set()
        await release_explicit.wait()
        return _make_queue(queue_id="explicit")

    mgr._discover.build_queue.side_effect = build
    with optional_work_budget():
        await mgr.start_build(_UID, scheduled=True)
    scheduled_task = mgr._states[_UID].task
    await scheduled_started.wait()
    mgr._preferences.get_advanced_settings.return_value.discover_queue_warm_cycle_build = False
    result = await mgr.start_build(_UID, force=True)
    await explicit_started.wait()
    try:
        assert result.action == "started"
        assert (await mgr.consume_queue(_UID)).queue_id == "test-queue-id"
        with pytest.raises(asyncio.CancelledError):
            await scheduled_task
    finally:
        release_explicit.set()
        await mgr.wait_for_build(_UID)
    assert (await mgr.consume_queue(_UID)).queue_id == "explicit"
