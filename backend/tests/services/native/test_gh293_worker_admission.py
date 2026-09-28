"""GH-293 identity-worker admission tests.

Repair workers must yield their lease (requeue truthfully) under WAL
backpressure and after the 250 ms background timeslice, must wait for public
bootstrap demand, must never wait inside the scan safety gate, and must still
complete under sustained demand (forced-fairness progress floor).
"""

from __future__ import annotations

import sqlite3
import threading
import types
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from infrastructure.persistence.native_library_store import NativeLibraryStore
from services.native.background_workload_gate import BackgroundWorkloadGate
from services.native.bootstrap_demand_signal import BootstrapDemandSignal
from services.native.catalog_identity_hygiene_service import (
    CatalogIdentityHygieneService,
)


class _ToggleWalCheckpoint:
    """Fake WAL checkpoint policy with a mutable suspension flag."""

    def __init__(self, suspended: bool = True) -> None:
        self.background_suspended = suspended


class _RecordingDemand:
    """Fake demand signal that records admission waits."""

    def __init__(self) -> None:
        self.waits = 0

    active = False

    async def wait_until_idle(self) -> None:
        self.waits += 1


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "library.db"
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users VALUES ('admin')")
    return path


@pytest.fixture
def store(db_path: Path) -> NativeLibraryStore:
    store = NativeLibraryStore(db_path, threading.Lock())
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, "
            "created_at, updated_at) VALUES ('artist-1', 'Artist', 'artist', 'group', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, "
            "album_artist_id, grouping_source, created_at, updated_at) "
            "VALUES ('album-1', 'root', 'fk-1', 'Album', 'album', 'artist-1', "
            "'automatic', 1, 1)"
        )
    return store


def _service(
    store: NativeLibraryStore,
    *,
    gate: BackgroundWorkloadGate | None = None,
    demand: object | None = None,
    wal: object | None = None,
) -> CatalogIdentityHygieneService:
    return CatalogIdentityHygieneService(
        store,
        gate,
        AsyncMock(),
        bootstrap_demand=demand,
        wal_checkpoint=wal,
        clock=lambda: 3,
    )


async def _create_and_claim(store: NativeLibraryStore) -> dict:
    from models.library_work import OperationJob

    job = OperationJob(
        id="hygiene-job-1",
        kind="repair",
        requested_by_user_id=None,
        input_catalog_revision=0,
        idempotency_key="catalog-identity-hygiene:v1:backfill",
        created_at=3,
    )
    created = await store.create_repair_operation(
        job,
        scope={"purpose": "catalog_identity_hygiene", "album_ids": []},
        source_matcher_version=None,
        target_matcher_version="v1",
    )
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None and claimed["id"] == created["id"]
    return created


@pytest.mark.asyncio
async def test_wal_backpressure_yields_job_back_to_queued(
    store: NativeLibraryStore, db_path: Path,
) -> None:
    created = await _create_and_claim(store)
    wal = _ToggleWalCheckpoint(suspended=True)
    service = _service(store, wal=wal)

    result = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker"
    )
    assert result["state"] == "queued"
    # No unit was in flight, so the worklist stays pending and resumable.
    with sqlite3.connect(db_path) as connection:
        states = connection.execute(
            "SELECT state FROM library_operation_work WHERE job_id = ?",
            (created["id"],),
        ).fetchall()
        job_row = connection.execute(
            "SELECT state, lease_owner, lease_expires_at FROM library_operation_jobs "
            "WHERE id = ?",
            (created["id"],),
        ).fetchone()
    assert states and set(state[0] for state in states) == {"pending"}
    assert job_row[0] == "queued"
    assert job_row[1] is None and job_row[2] is None
    # A later pass with the backpressure cleared completes the same job.
    wal.background_suspended = False
    resumed = await store.claim_operation_job(
        "worker-2", now=4, lease_seconds=60, kind="repair"
    )
    assert resumed is not None
    terminal = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker-2"
    )
    assert terminal["state"] == "succeeded"


@pytest.mark.asyncio
async def test_timeslice_yields_after_250ms_budget(
    store: NativeLibraryStore, monkeypatch: pytest.MonkeyPatch,
) -> None:
    created = await _create_and_claim(store)
    service = _service(store)
    # Zero elapsed inside one unit, then a >250ms jump triggers the yield;
    # later calls stay at the jumped value so a resumed pass can complete.
    values = [0.0, 10.0]
    calls = [0]

    def fake_monotonic() -> float:
        calls[0] += 1
        return values[min(calls[0] - 1, len(values) - 1)]

    monkeypatch.setattr(
        "services.native.catalog_identity_hygiene_service.time",
        types.SimpleNamespace(monotonic=fake_monotonic),
    )
    result = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker"
    )
    assert result["state"] == "queued"
    # The yielded job requeues and resumes to completion on the next pass.
    resumed = await store.claim_operation_job(
        "worker-2", now=4, lease_seconds=60, kind="repair"
    )
    assert resumed is not None
    terminal = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker-2"
    )
    assert terminal["state"] == "succeeded"


@pytest.mark.asyncio
async def test_worker_waits_for_public_bootstrap_demand(store: NativeLibraryStore) -> None:
    demand = _RecordingDemand()
    created = await _create_and_claim(store)
    service = _service(store, demand=demand)
    result = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker"
    )
    assert result["state"] == "succeeded"
    assert demand.waits >= 1


@pytest.mark.asyncio
async def test_sustained_demand_still_completes_under_forced_progress_floor(
    store: NativeLibraryStore,
) -> None:
    signal = BootstrapDemandSignal(max_hold_seconds=0.01)
    signal.begin()  # one public bootstrap read in flight for the whole run
    created = await _create_and_claim(store)
    service = _service(store, demand=signal)
    try:
        result = await service.run_claimed(
            {"id": created["id"], "kind": "repair"}, "worker"
        )
    finally:
        signal.end()
    # The absolute hold forces progress: the job still completes.
    assert result["state"] == "succeeded"


@pytest.mark.asyncio
async def test_yield_persists_cooldown_and_prevents_immediate_reclaim(
    store: NativeLibraryStore, db_path: Path,
) -> None:
    """Timeslice/WAL/demand yields persist a positive next_attempt_at so the
    worker cannot immediately reclaim the same job (no hot loop)."""
    created = await _create_and_claim(store)
    wal = _ToggleWalCheckpoint(suspended=True)
    service = _service(store, wal=wal)
    result = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker"
    )
    assert result["state"] == "queued"
    with sqlite3.connect(db_path) as connection:
        next_attempt = connection.execute(
            "SELECT next_attempt_at FROM library_operation_jobs WHERE id = ?",
            (created["id"],),
        ).fetchone()[0]
    assert next_attempt is not None and next_attempt > 3  # now=3, cooldown 0.5
    # Re-claim before the cooldown elapses returns nothing (no immediate reclaim).
    early = await store.claim_operation_job("worker", now=3.2, lease_seconds=60, kind="repair")
    assert early is None
    # After the cooldown, the same job resumes and completes.
    resumed = await store.claim_operation_job("worker", now=3.6, lease_seconds=60, kind="repair")
    assert resumed is not None and resumed["id"] == created["id"]
    wal.background_suspended = False
    terminal = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker"
    )
    assert terminal["state"] == "succeeded"


@pytest.mark.asyncio
async def test_stale_pin_rebase_caught_by_worker_completes_in_same_claim(
    store: NativeLibraryStore, db_path: Path,
) -> None:
    """The hygiene worker catches the stale-pin failure, rebases the SAME job,
    and re-materializes in the same claim instead of yielding to the back of
    the queue (where the pin would be stale again by its next turn)."""
    from core.exceptions import StaleRevisionError

    from services.native.catalog_identity_hygiene_service import (
        CatalogIdentityHygieneService,
    )

    created = await _create_and_claim(store)
    first = await store.materialize_repair_operation_batch(
        created["id"], "worker", now=3
    )
    assert first["complete"] is True  # single-subject job seals immediately
    # Force an unsealed stale state, as if the pin moved between pages.
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_repair_materialization SET sealed = 0, staged_count = 0 "
            "WHERE job_id = ?",
            (created["id"],),
        )
        connection.execute(
            "DELETE FROM library_operation_work WHERE job_id = ?", (created["id"],)
        )
        connection.execute(
            "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1"
        )
    wal = _ToggleWalCheckpoint(suspended=False)
    service = _service(store, wal=wal, demand=_RecordingDemand())
    result = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker"
    )
    # The worker rebased onto the current revision and completed in this claim.
    assert result["state"] == "succeeded"
    assert result["id"] == created["id"]  # same static job, no fresh id
    with sqlite3.connect(db_path) as connection:
        pin = connection.execute(
            "SELECT pinned_catalog_revision, sealed "
            "FROM library_repair_materialization WHERE job_id = ?",
            (created["id"],),
        ).fetchone()
        current = connection.execute(
            "SELECT value FROM library_catalog_revision WHERE singleton = 1"
        ).fetchone()[0]
    assert pin[0] == current and pin[1] == 1


@pytest.mark.asyncio
async def test_stale_pin_rebase_yields_when_catalog_keeps_moving(
    store: NativeLibraryStore, db_path: Path,
) -> None:
    """If the catalog changes on every retry the inline rebase is bounded: the
    worker yields with the cooldown instead of looping inside one claim."""
    from core.exceptions import StaleRevisionError

    from services.native.catalog_identity_hygiene_service import (
        _MAX_INLINE_REBASES,
    )

    created = await _create_and_claim(store)
    materialize = AsyncMock(side_effect=StaleRevisionError("pin moved again"))
    store.materialize_repair_operation_batch = materialize  # type: ignore[method-assign]
    service = _service(
        store,
        wal=_ToggleWalCheckpoint(suspended=False),
        demand=_RecordingDemand(),
    )

    result = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker"
    )

    assert result["state"] == "queued"
    assert materialize.await_count == _MAX_INLINE_REBASES + 1
    with sqlite3.connect(db_path) as connection:
        next_attempt = connection.execute(
            "SELECT next_attempt_at FROM library_operation_jobs WHERE id = ?",
            (created["id"],),
        ).fetchone()[0]
    assert next_attempt is not None and next_attempt > 3


@pytest.mark.asyncio
async def test_scan_active_defers_without_waiting_in_gate(store: NativeLibraryStore) -> None:
    gate = BackgroundWorkloadGate()
    gate.set_scan_active(True)
    created = await _create_and_claim(store)
    service = _service(store, gate=gate)
    result = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker"
    )
    assert result["state"] == "queued"
    gate.set_scan_active(False)
    resumed = await store.claim_operation_job(
        "worker-2", now=4, lease_seconds=60, kind="repair"
    )
    assert resumed is not None
    terminal = await service.run_claimed(
        {"id": created["id"], "kind": "repair"}, "worker-2"
    )
    assert terminal["state"] == "succeeded"
