"""Real durable hygiene progress after admission, using virtual time only."""

import asyncio
import json
import sqlite3
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

from core.exceptions import StaleRevisionError
from infrastructure.persistence.gh293_calibration import (
    BACKGROUND_TIMESLICE_SECONDS,
    BACKGROUND_YIELD_COOLDOWN_SECONDS,
)
from services.native import catalog_identity_hygiene_service as module
from services.native.library_operation_supervisor import LibraryOperationSupervisor
from tests.services.native.test_catalog_identity_hygiene_service import (
    _seed_split,
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


async def _claim(store, clock, worker="worker"):
    claimed = await store.claim_operation_job(
        worker, now=clock.wall(), lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    return claimed


@pytest.mark.asyncio
@pytest.mark.parametrize("gate_delay,bootstrap_delay", [(120, 0), (0, 5), (120, 5)])
async def test_delayed_admission_advances_real_hygiene_units_same_job(
    store, db_path, clock, monkeypatch, gate_delay, bootstrap_delay
):
    source_id, target_id = await _seed_split(store)

    async def gate_wait():
        clock.advance(gate_delay)

    async def bootstrap_wait():
        clock.advance(bootstrap_delay)

    gate = SimpleNamespace(
        scan_active=False, wait_until_available=AsyncMock(side_effect=gate_wait)
    )
    bootstrap = SimpleNamespace(wait_until_idle=AsyncMock(side_effect=bootstrap_wait))
    changed = AsyncMock()
    service = module.CatalogIdentityHygieneService(
        store, gate, changed, bootstrap_demand=bootstrap, clock=clock.wall
    )
    original_apply = store.apply_catalog_identity_hygiene_work

    async def one_unit(*args, **kwargs):
        result = await original_apply(*args, **kwargs)
        clock.advance(BACKGROUND_TIMESLICE_SECONDS + 0.05)
        return result

    monkeypatch.setattr(store, "apply_catalog_identity_hygiene_work", one_unit)
    job = await service.enqueue_backfill()
    for completed in (1, 2):
        claimed = await _claim(store, clock)
        assert claimed["id"] == job["id"]
        yielded = await service.run_claimed(claimed, "worker")
        assert yielded["state"] == "queued"
        assert yielded["completed_count"] == completed
        assert yielded["next_attempt_at"] == pytest.approx(
            clock.wall() + BACKGROUND_YIELD_COOLDOWN_SECONDS
        )
        with sqlite3.connect(db_path) as connection:
            rows = connection.execute(
                "SELECT state FROM library_operation_work WHERE job_id=? ORDER BY ordinal",
                (job["id"],),
            ).fetchall()
            retired = connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id=?",
                (source_id,),
            ).fetchone()[0]
        assert sum(state != "pending" for (state,) in rows) == completed
        assert retired == target_id
        assert (
            await store.claim_operation_job(
                "worker", now=clock.wall(), lease_seconds=60, kind="repair"
            )
            is None
        )
        clock.advance(BACKGROUND_YIELD_COOLDOWN_SECONDS)
    terminal = await service.run_claimed(await _claim(store, clock), "worker")
    assert terminal["id"] == job["id"]
    assert terminal["state"] == "succeeded"
    assert terminal["completed_count"] == 2
    assert terminal["succeeded_count"] == 1
    assert terminal["skipped_count"] == 1
    changed.assert_awaited_once()
    assert gate.wait_until_available.await_count == 3
    assert bootstrap.wait_until_idle.await_count == 3


@pytest.mark.asyncio
async def test_exact_manual_identity_safely_completes_single_album_after_admission(
    store, db_path, clock
):
    source_id, _ = await _seed_split(store, exact_track_map=True)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities SET decision_source='manual' WHERE local_album_id=?",
            (source_id,),
        )
        before = connection.execute(
            "SELECT * FROM local_album_external_identities WHERE local_album_id=?",
            (source_id,),
        ).fetchall()

    async def delayed_gate():
        clock.advance(120)

    gate = SimpleNamespace(
        scan_active=False, wait_until_available=AsyncMock(side_effect=delayed_gate)
    )
    changed = AsyncMock()
    service = module.CatalogIdentityHygieneService(
        store, gate, changed, clock=clock.wall
    )
    job = await service.enqueue_album(source_id)
    terminal = await service.run_claimed(await _claim(store, clock), "worker")
    assert terminal["id"] == job["id"]
    assert terminal["state"] == "succeeded"
    assert terminal["completed_count"] == terminal["skipped_count"] == 1
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT * FROM local_album_external_identities WHERE local_album_id=?",
                (source_id,),
            ).fetchall()
            == before
        )
        result = json.loads(
            connection.execute(
                "SELECT result_json FROM library_operation_work WHERE job_id=?",
                (job["id"],),
            ).fetchone()[0]
        )
    assert result["reason_code"] == "NO_SAFE_CHANGE"
    assert result["filesystem_writes"] == 0
    changed.assert_not_awaited()


@pytest.mark.asyncio
async def test_materialization_budget_remains_finite(store, clock, monkeypatch):
    await _seed_split(store)
    gate = SimpleNamespace(scan_active=False, wait_until_available=AsyncMock())
    service = module.CatalogIdentityHygieneService(store, gate, clock=clock.wall)
    await service.enqueue_backfill()
    claimed = await _claim(store, clock)

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
@pytest.mark.parametrize("guard", ["scan", "wal", "pause", "stop", "lease"])
async def test_guards_changed_during_admission_block_catalog_changes(
    store, clock, monkeypatch, guard
):
    source_id, _ = await _seed_split(store)
    wal = SimpleNamespace(background_suspended=False)
    gate = SimpleNamespace(scan_active=False)
    changed = AsyncMock()
    service = module.CatalogIdentityHygieneService(
        store, gate, changed, wal_checkpoint=wal, clock=clock.wall
    )
    await service.enqueue_album(source_id)
    claimed = await _claim(store, clock)
    apply = AsyncMock(wraps=store.apply_catalog_identity_hygiene_work)
    monkeypatch.setattr(store, "apply_catalog_identity_hygiene_work", apply)

    async def admission():
        clock.advance(120)
        if guard == "scan":
            gate.scan_active = True
        elif guard == "wal":
            wal.background_suspended = True
        elif guard in ("pause", "stop"):
            current = await store.get_operation_job(claimed["id"])
            await store.request_operation_control(
                claimed["id"],
                control=guard,
                expected_row_revision=current["row_revision"],
                now=clock.wall(),
            )
        else:
            assert await store.recover_expired_operation_leases(now=clock.wall()) == 1

    gate.wait_until_available = AsyncMock(side_effect=admission)
    if guard == "lease":
        with pytest.raises(StaleRevisionError):
            await service.run_claimed(claimed, "worker")
    else:
        result = await service.run_claimed(claimed, "worker")
        assert result["state"] == {"pause": "paused", "stop": "stopped"}.get(
            guard, "queued"
        )
        assert result["completed_count"] == 0
        if guard == "wal":
            assert result["next_attempt_at"] == pytest.approx(
                clock.wall() + BACKGROUND_YIELD_COOLDOWN_SECONDS
            )
    apply.assert_not_awaited()
    changed.assert_not_awaited()


@pytest.mark.asyncio
async def test_scan_beginning_between_units_defers_next_unit(store, clock, monkeypatch):
    await _seed_split(store)
    gate = SimpleNamespace(scan_active=False, wait_until_available=AsyncMock())
    service = module.CatalogIdentityHygieneService(store, gate, clock=clock.wall)
    original_apply = store.apply_catalog_identity_hygiene_work

    async def scan_after_unit(*args, **kwargs):
        result = await original_apply(*args, **kwargs)
        gate.scan_active = True
        return result

    apply = AsyncMock(side_effect=scan_after_unit)
    monkeypatch.setattr(store, "apply_catalog_identity_hygiene_work", apply)
    await service.enqueue_backfill()
    result = await service.run_claimed(await _claim(store, clock), "worker")
    assert result["state"] == "queued"
    assert result["completed_count"] == 1
    apply.assert_awaited_once()


@pytest.mark.asyncio
async def test_real_supervisor_releases_cancelled_admission_then_same_job_resumes(
    store, clock
):
    source_id, _ = await _seed_split(store)
    gate = SimpleNamespace(
        scan_active=False,
        wait_until_available=AsyncMock(side_effect=asyncio.CancelledError),
    )
    service = module.CatalogIdentityHygieneService(store, gate, clock=clock.wall)
    job = await service.enqueue_album(source_id)
    operations = Mock()
    operations.response_for.side_effect = lambda row: row
    supervisor = LibraryOperationSupervisor(
        store, operations, AsyncMock(), AsyncMock(), catalog_identity_hygiene=service
    )
    with pytest.raises(asyncio.CancelledError):
        await supervisor.run_once("worker", now=clock.wall())
    released = await store.get_operation_job(job["id"])
    assert released["state"] == "queued"
    assert released["lease_owner"] is None
    assert released["completed_count"] == 0
    gate.wait_until_available.side_effect = None
    terminal = await supervisor.run_once("next-worker", now=clock.wall())
    assert terminal["id"] == job["id"]
    assert terminal["state"] == "succeeded"
    assert terminal["completed_count"] == 1


@pytest.mark.asyncio
async def test_delayed_real_control_checkpoint_does_not_consume_subject_budget(
    store, clock, monkeypatch
):
    source_id, _ = await _seed_split(store)
    service = module.CatalogIdentityHygieneService(store, clock=clock.wall)
    job = await service.enqueue_album(source_id)
    original_checkpoint = store.checkpoint_operation_control

    async def delayed_checkpoint(*args, **kwargs):
        result = await original_checkpoint(*args, **kwargs)
        clock.advance(5)
        return result

    monkeypatch.setattr(store, "checkpoint_operation_control", delayed_checkpoint)
    terminal = await service.run_claimed(await _claim(store, clock), "worker")
    assert terminal["id"] == job["id"]
    assert terminal["completed_count"] == 1
    assert terminal["state"] == "succeeded"


@pytest.mark.asyncio
@pytest.mark.parametrize("pause", [False, True])
async def test_completed_materialization_over_budget_still_checks_control_before_yield(
    store, clock, monkeypatch, pause
):
    source_id, _ = await _seed_split(store)
    gate = SimpleNamespace(scan_active=False, wait_until_available=AsyncMock())
    service = module.CatalogIdentityHygieneService(store, gate, clock=clock.wall)
    await service.enqueue_album(source_id)
    claimed = await _claim(store, clock)
    original_materialize = store.materialize_repair_operation_batch

    async def slow_completed_page(*args, **kwargs):
        result = await original_materialize(*args, **kwargs)
        assert result["complete"] is True
        clock.advance(BACKGROUND_TIMESLICE_SECONDS + 0.05)
        if pause:
            current = await store.get_operation_job(claimed["id"])
            await store.request_operation_control(
                claimed["id"],
                control="pause",
                expected_row_revision=current["row_revision"],
                now=clock.wall(),
            )
        return result

    monkeypatch.setattr(
        store, "materialize_repair_operation_batch", slow_completed_page
    )
    result = await service.run_claimed(claimed, "worker")
    assert result["completed_count"] == 0
    assert result["state"] == ("paused" if pause else "queued")
    gate.wait_until_available.assert_not_awaited()
    if not pause:
        assert result["next_attempt_at"] == pytest.approx(
            clock.wall() + BACKGROUND_YIELD_COOLDOWN_SECONDS
        )
