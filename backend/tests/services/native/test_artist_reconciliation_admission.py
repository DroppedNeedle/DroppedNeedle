"""Durable reconciliation progress after delayed admission, without real sleeps."""

import sqlite3
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from infrastructure.persistence.gh293_calibration import (
    BACKGROUND_TIMESLICE_SECONDS,
    BACKGROUND_YIELD_COOLDOWN_SECONDS,
)
from services.native import artist_identity_reconciliation_service as module
from tests.services.native.test_artist_identity_reconciliation_service import (
    _accept_exact_identity,
    _membership,
    _release,
    db_path,
    store,
)


class Clock:
    elapsed = 0.0

    def monotonic(self):
        return self.elapsed

    def wall(self):
        return 1000 + self.elapsed

    def advance(self, seconds):
        self.elapsed += seconds


@pytest.fixture
def clock(monkeypatch):
    clock = Clock()
    monkeypatch.setattr(module, "time", SimpleNamespace(monotonic=clock.monotonic))
    return clock


@pytest.mark.asyncio
@pytest.mark.parametrize("gate_delay,bootstrap_delay", [(120, 0), (0, 5), (120, 5)])
async def test_delayed_admission_advances_real_pending_albums_across_bounded_passes(
    store, db_path, clock, monkeypatch, gate_delay, bootstrap_delay
):
    for suffix in ("1", "2"):
        await store.create_catalog_membership(_membership(suffix, "Artist"))
        await _accept_exact_identity(store, suffix)
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()

    async def gate_wait():
        clock.advance(gate_delay)

    async def bootstrap_wait():
        clock.advance(bootstrap_delay)

    gate = SimpleNamespace(
        scan_active=False, wait_until_available=AsyncMock(side_effect=gate_wait)
    )
    bootstrap = SimpleNamespace(wait_until_idle=AsyncMock(side_effect=bootstrap_wait))
    service = module.ArtistIdentityReconciliationService(
        store, provider, gate, bootstrap_demand=bootstrap, clock=clock.wall
    )
    original_apply = store.apply_artist_credit_projection

    async def complete_one_unit(*args, **kwargs):
        result = await original_apply(*args, **kwargs)
        clock.advance(BACKGROUND_TIMESLICE_SECONDS + 0.05)
        return result

    monkeypatch.setattr(store, "apply_artist_credit_projection", complete_one_unit)
    job = await service.enqueue_backfill()
    for expected_completed in (1, 2):
        claimed = await store.claim_operation_job(
            "worker", now=clock.wall(), lease_seconds=60, kind="repair"
        )
        assert claimed is not None
        yielded = await service.run_claimed(claimed, "worker")
        assert yielded["state"] == "queued"
        assert yielded["completed_count"] == expected_completed
        assert yielded["next_attempt_at"] == pytest.approx(
            clock.wall() + BACKGROUND_YIELD_COOLDOWN_SECONDS
        )
        with sqlite3.connect(db_path) as connection:
            rows = connection.execute(
                "SELECT state FROM library_operation_work WHERE job_id=? ORDER BY ordinal",
                (job["id"],),
            ).fetchall()
        assert rows.count(("succeeded",)) == expected_completed
        assert (
            await store.claim_operation_job(
                "worker", now=clock.wall(), lease_seconds=60, kind="repair"
            )
            is None
        )
        clock.advance(BACKGROUND_YIELD_COOLDOWN_SECONDS)
    claimed = await store.claim_operation_job(
        "worker", now=clock.wall(), lease_seconds=60, kind="repair"
    )
    terminal = await service.run_claimed(claimed, "worker")
    assert terminal["state"] == "succeeded"
    assert terminal["completed_count"] == 2
    assert provider.get_canonical_release.await_count == 2
    assert gate.wait_until_available.await_count == 3
    assert bootstrap.wait_until_idle.await_count == 3


@pytest.mark.asyncio
async def test_materialization_retains_its_separate_finite_budget(
    store, clock, monkeypatch
):
    await store.create_catalog_membership(_membership("1", "Artist"))
    gate = SimpleNamespace(scan_active=False, wait_until_available=AsyncMock())
    service = module.ArtistIdentityReconciliationService(
        store, AsyncMock(), gate, clock=clock.wall
    )
    await service.enqueue_backfill()
    claimed = await store.claim_operation_job(
        "worker", now=clock.wall(), lease_seconds=60, kind="repair"
    )

    async def slow_page(*args, **kwargs):
        clock.advance(BACKGROUND_TIMESLICE_SECONDS + 0.05)
        return {"complete": False}

    materialize = AsyncMock(side_effect=slow_page)
    monkeypatch.setattr(store, "materialize_repair_operation_batch", materialize)
    result = await service.run_claimed(claimed, "worker")
    assert result["state"] == "queued"
    assert result["completed_count"] == 0
    assert result["next_attempt_at"] == pytest.approx(
        clock.wall() + BACKGROUND_YIELD_COOLDOWN_SECONDS
    )
    materialize.assert_awaited_once()
    gate.wait_until_available.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("guard", ["scan", "wal", "pause", "lease"])
async def test_guards_changed_during_admission_prevent_provider_work(
    store, clock, guard
):
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    wal = SimpleNamespace(background_suspended=False)
    gate = SimpleNamespace(scan_active=False)
    service = module.ArtistIdentityReconciliationService(
        store, provider, gate, wal_checkpoint=wal, clock=clock.wall
    )
    await service.enqueue_album("album-1")
    claimed = await store.claim_operation_job(
        "worker", now=clock.wall(), lease_seconds=60, kind="repair"
    )

    async def admission():
        clock.advance(120)
        if guard == "scan":
            gate.scan_active = True
        elif guard == "wal":
            wal.background_suspended = True
        elif guard == "pause":
            current = await store.get_operation_job(claimed["id"])
            await store.request_operation_control(
                claimed["id"],
                control="pause",
                expected_row_revision=current["row_revision"],
                now=clock.wall(),
            )
        else:
            assert await store.recover_expired_operation_leases(now=clock.wall()) == 1

    gate.wait_until_available = AsyncMock(side_effect=admission)
    if guard == "lease":
        from core.exceptions import StaleRevisionError

        with pytest.raises(StaleRevisionError):
            await service.run_claimed(claimed, "worker")
    else:
        result = await service.run_claimed(claimed, "worker")
        assert result["state"] == ("paused" if guard == "pause" else "queued")
        if guard == "wal":
            assert result["next_attempt_at"] == pytest.approx(
                clock.wall() + BACKGROUND_YIELD_COOLDOWN_SECONDS
            )
    provider.get_canonical_release.assert_not_awaited()


@pytest.mark.asyncio
async def test_scan_starting_between_units_defers_next_album(store, clock, monkeypatch):
    for suffix in ("1", "2"):
        await store.create_catalog_membership(_membership(suffix, "Artist"))
        await _accept_exact_identity(store, suffix)
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    gate = SimpleNamespace(scan_active=False, wait_until_available=AsyncMock())
    service = module.ArtistIdentityReconciliationService(
        store, provider, gate, clock=clock.wall
    )
    original_apply = store.apply_artist_credit_projection

    async def scan_after_first(*args, **kwargs):
        result = await original_apply(*args, **kwargs)
        gate.scan_active = True
        return result

    monkeypatch.setattr(store, "apply_artist_credit_projection", scan_after_first)
    await service.enqueue_backfill()
    claimed = await store.claim_operation_job(
        "worker", now=clock.wall(), lease_seconds=60, kind="repair"
    )
    deferred = await service.run_claimed(claimed, "worker")
    assert deferred["state"] == "queued"
    assert deferred["completed_count"] == 1
    provider.get_canonical_release.assert_awaited_once()
