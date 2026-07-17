"""Tests for DiscoverQueueManager background queue building."""

import asyncio
import threading
import time
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from api.v1.schemas.discover import DiscoverQueueEnrichment, DiscoverQueueItemLight, DiscoverQueueResponse
from services.discover_queue_manager import DiscoverQueueManager, QueueBuildStatus
from infrastructure.persistence.discovery_snapshot_store import DiscoverySnapshotStore

_UID = "u1"


def _make_queue(n: int = 3) -> DiscoverQueueResponse:
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
        queue_id="test-queue-id",
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
    prefs.get_advanced_settings.return_value = adv

    return DiscoverQueueManager(discover, prefs, snapshot_store=snapshot_store)


@pytest.mark.asyncio
async def test_initial_status_is_idle():
    expect_assertions = True
    mgr = _make_manager()
    status = mgr.get_status(_UID)
    assert status.status == "idle"


@pytest.mark.asyncio
async def test_start_build_changes_status():
    expect_assertions = True
    mgr = _make_manager()
    result = await mgr.start_build(_UID)
    assert result.action == "started"
    assert result.status in ("building", "ready")


@pytest.mark.asyncio
async def test_build_produces_ready_queue():
    expect_assertions = True
    queue = _make_queue(5)
    mgr = _make_manager(queue=queue)
    await mgr.start_build(_UID)
    await mgr.wait_for_build(_UID)

    status = mgr.get_status(_UID)
    assert status.status == "ready"
    assert status.item_count == 5
    built_queue = mgr.get_queue(_UID)
    assert built_queue is not None
    assert all(item.enrichment is not None for item in built_queue.items)


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
async def test_enrichment_failures_fall_back_to_empty_enrichment():
    expect_assertions = True
    queue = _make_queue(2)
    mgr = _make_manager(queue=queue)
    mgr._discover.enrich_queue_item.side_effect = RuntimeError("enrichment failed")

    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    built_queue = mgr.get_queue(_UID)
    assert built_queue is not None
    assert all(item.enrichment is not None for item in built_queue.items)


@pytest.mark.asyncio
async def test_get_queue_returns_cached():
    expect_assertions = True
    queue = _make_queue(3)
    mgr = _make_manager(queue=queue)
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    result = mgr.get_queue(_UID)
    assert result is not None
    assert len(result.items) == 3


@pytest.mark.asyncio
async def test_consume_queue_returns_and_clears():
    expect_assertions = True
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    consumed = await mgr.consume_queue(_UID)
    assert consumed is not None
    assert len(consumed.items) == 3

    assert mgr.get_queue(_UID) is None
    assert mgr.get_status(_UID).status == "idle"


@pytest.mark.asyncio
async def test_build_error_sets_error_status():
    expect_assertions = True
    mgr = _make_manager(build_error=RuntimeError("test fail"))
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    status = mgr.get_status(_UID)
    assert status.status == "error"
    assert "test fail" in (status.error or "")


@pytest.mark.asyncio
async def test_already_building_is_no_op():
    expect_assertions = True

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
    expect_assertions = True
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    result = await mgr.start_build(_UID, force=True)
    assert result.action == "started"


@pytest.mark.asyncio
async def test_invalidate_resets_state():
    expect_assertions = True
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    mgr.invalidate(_UID)
    status = mgr.get_status(_UID)
    assert status.status == "idle"


@pytest.mark.asyncio
async def test_separate_users_are_independent():
    expect_assertions = True
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    built_status = mgr.get_status(_UID)
    other_status = mgr.get_status("someone-else")
    assert built_status.status == "ready"
    assert other_status.status == "idle"


@pytest.mark.asyncio
async def test_consume_queue_rejects_stale():
    expect_assertions = True
    mgr = _make_manager(ttl=1)
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    assert mgr.get_status(_UID).status == "ready"

    mgr._get_state(_UID).built_at = time.time() - 10

    consumed = await mgr.consume_queue(_UID)
    assert consumed is None
    assert mgr.get_status(_UID).status == "idle"


@pytest.mark.asyncio
async def test_get_queue_rejects_stale():
    expect_assertions = True
    mgr = _make_manager(ttl=1)
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    assert mgr.get_queue(_UID) is not None

    mgr._get_state(_UID).built_at = time.time() - 10

    assert mgr.get_queue(_UID) is None


@pytest.mark.asyncio
async def test_stale_flag_in_status():
    expect_assertions = True
    mgr = _make_manager(ttl=1)
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    status = mgr.get_status(_UID)
    assert status.stale is False

    mgr._get_state(_UID).built_at = time.time() - 10

    status = mgr.get_status(_UID)
    assert status.stale is True


@pytest.mark.asyncio
async def test_build_prewarms_covers():
    expect_assertions = True
    queue = _make_queue(3)
    cover_repo = AsyncMock()
    cover_repo.get_release_group_cover = AsyncMock(return_value=(b"img", "image/jpeg", "caa"))

    discover = AsyncMock()
    discover.build_queue.return_value = queue
    discover.enrich_queue_item = AsyncMock(return_value=DiscoverQueueEnrichment())

    prefs = MagicMock()
    adv = MagicMock()
    adv.discover_queue_ttl = 86400
    prefs.get_advanced_settings.return_value = adv

    mgr = DiscoverQueueManager(discover, prefs, cover_repo=cover_repo)
    await mgr.start_build(_UID)
    await asyncio.sleep(0.3)

    assert cover_repo.get_release_group_cover.call_count == 3
    called_mbids = sorted(
        call.args[0] for call in cover_repo.get_release_group_cover.call_args_list
    )
    assert called_mbids == ["mbid-0", "mbid-1", "mbid-2"]


@pytest.mark.asyncio
async def test_build_prewarm_skipped_without_cover_repo():
    expect_assertions = True
    mgr = _make_manager()
    await mgr.start_build(_UID)
    await asyncio.sleep(0.1)

    assert mgr.get_status(_UID).status == "ready"


@pytest.mark.asyncio
async def test_build_prewarm_failure_does_not_break_queue():
    expect_assertions = True
    queue = _make_queue(2)
    cover_repo = AsyncMock()
    cover_repo.get_release_group_cover = AsyncMock(side_effect=RuntimeError("fetch failed"))

    discover = AsyncMock()
    discover.build_queue.return_value = queue
    discover.enrich_queue_item = AsyncMock(return_value=DiscoverQueueEnrichment())

    prefs = MagicMock()
    adv = MagicMock()
    adv.discover_queue_ttl = 86400
    prefs.get_advanced_settings.return_value = adv

    mgr = DiscoverQueueManager(discover, prefs, cover_repo=cover_repo)
    await mgr.start_build(_UID)
    await asyncio.sleep(0.3)

    assert mgr.get_status(_UID).status == "ready"
    assert mgr.get_queue(_UID) is not None
