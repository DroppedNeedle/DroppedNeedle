from __future__ import annotations

import asyncio
import threading
from contextlib import suppress
from datetime import datetime
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from core.task_registry import TaskRegistry
from infrastructure.sse_publisher import KEEPALIVE, SSEPublisher
from models.library_work import ScanRun, ScanRunSnapshot, ScanScope
from services.compat.target_scan_service import TargetCompatScanService
from services.native.library_scan_events import LibraryScanEventPublisher
from services.native.library_inventory_scanner import LibraryInventoryScanner
from services.native.library_scan_scheduler import LibraryAutomaticScanScheduler
from services.native.library_policy_resolver import LibraryPolicyResolver
from api.v1.schemas.library_policies import (
    LibraryPathPolicyRule,
    LibraryRootSettings,
    TypedLibrarySettings,
)
from services.native.library_scan_supervisor import (
    SUPERVISOR_TASK_NAME,
    start_target_scan_supervisor,
    supervise_target_scans,
)
from services.native.target_application_runtime import (
    run_target_identification_worker,
    run_target_operation_worker,
)
from services.native.background_workload_gate import BackgroundWorkloadGate


@pytest.mark.asyncio
async def test_subsonic_target_projection_uses_only_the_coordinator() -> None:
    coordinator = AsyncMock()
    coordinator.current.return_value = [
        ScanRun(
            id="run-1",
            kind="incremental",
            trigger="subsonic",
            state="indexing",
            phase="indexing",
        )
    ]
    coordinator.snapshot.return_value = ScanRunSnapshot(
        run=coordinator.current.return_value[0], counters={"inspected_count": 42}
    )
    resolver = SimpleNamespace(
        policy_revision="policy-1",
        settings=SimpleNamespace(
            library_roots=[
                SimpleNamespace(id="root-a", path="/music", policy="automatic")
            ]
        ),
    )
    service = TargetCompatScanService(coordinator, lambda: resolver)

    await service.start()
    scanning, count = await service.status()

    request = coordinator.request_run.await_args.args[0]
    assert request.trigger == "subsonic"
    assert request.scopes[0].root_id == "root-a"
    assert scanning is True
    assert count == 42


@pytest.mark.asyncio
async def test_supervisor_fetches_the_current_coordinator_each_iteration(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    coordinators = [AsyncMock(), AsyncMock(), AsyncMock()]
    calls = 0

    def getter():
        nonlocal calls
        result = coordinators[min(calls, 2)]
        calls += 1
        return result

    async def stop_after_two(_seconds: float) -> None:
        if calls >= 3:
            raise asyncio.CancelledError

    monkeypatch.setattr(
        "services.native.library_scan_supervisor.asyncio.sleep", stop_after_two
    )
    await supervise_target_scans(getter, lambda: {"root-a": Path("/scratch")})

    assert calls == 3
    coordinators[0].recover.assert_awaited_once()
    coordinators[1].run_once.assert_awaited_once()
    coordinators[2].run_once.assert_awaited_once()


@pytest.mark.asyncio
async def test_only_one_target_supervisor_can_be_registered() -> None:
    registry = TaskRegistry.get_instance()
    registry.reset()
    coordinator = AsyncMock()
    task = start_target_scan_supervisor(lambda: coordinator, lambda: {})
    assert registry.is_running(SUPERVISOR_TASK_NAME)
    with pytest.raises(RuntimeError):
        start_target_scan_supervisor(lambda: coordinator, lambda: {})
    task.cancel()
    with suppress(asyncio.CancelledError):
        await task
    assert task.done()
    registry.reset()


@pytest.mark.asyncio
async def test_supervisor_refreshes_scheduler_and_resolver_each_iteration(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    coordinator = AsyncMock()
    scheduler = AsyncMock()
    resolver = SimpleNamespace(policy_revision="one")
    calls = {"scheduler": 0, "resolver": 0, "settings": 0, "sleep": 0}

    def scheduler_getter():
        calls["scheduler"] += 1
        return scheduler

    def resolver_getter():
        calls["resolver"] += 1
        return resolver

    def settings_getter():
        calls["settings"] += 1
        return {
            "frequency": "manual",
            "daily_time": "03:00",
            "timezone_name": "Europe/London",
        }

    async def stop_after_two(_seconds: float) -> None:
        calls["sleep"] += 1
        if calls["sleep"] == 2:
            raise asyncio.CancelledError

    monkeypatch.setattr(
        "services.native.library_scan_supervisor.asyncio.sleep", stop_after_two
    )
    await supervise_target_scans(
        lambda: coordinator,
        lambda: {},
        scheduler_getter,
        resolver_getter,
        settings_getter,
    )

    assert calls == {"scheduler": 2, "resolver": 2, "settings": 2, "sleep": 2}
    assert scheduler.tick.await_count == 2


@pytest.mark.asyncio
async def test_target_identification_worker_recovers_claims_and_survives_iterations(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    queue = AsyncMock()
    queue.is_paused.return_value = False
    queue.claim.side_effect = [{"id": "job-1"}, None]
    service = AsyncMock()
    sleeps = 0

    async def stop_after_two(_seconds: float) -> None:
        nonlocal sleeps
        sleeps += 1
        if sleeps == 2:
            raise asyncio.CancelledError

    monkeypatch.setattr(
        "services.native.target_application_runtime.asyncio.sleep", stop_after_two
    )
    await run_target_identification_worker(
        lambda: queue, lambda: service, worker_id="test-worker"
    )

    assert queue.recover.await_count == 2
    service.run_claimed_job.assert_awaited_once_with({"id": "job-1"}, "test-worker")


@pytest.mark.asyncio
async def test_identification_worker_starts_no_new_unit_while_scan_is_active(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    queue = AsyncMock()
    queue.is_paused.return_value = False
    queue.claim.return_value = {"id": "job-1"}
    service = AsyncMock()
    gate = BackgroundWorkloadGate()
    gate.set_scan_active(True)
    sleeps = 0

    async def release_then_stop(_seconds: float) -> None:
        nonlocal sleeps
        sleeps += 1
        if sleeps == 1:
            gate.set_scan_active(False)
        else:
            raise asyncio.CancelledError

    monkeypatch.setattr(
        "services.native.target_application_runtime.asyncio.sleep", release_then_stop
    )

    await run_target_identification_worker(
        lambda: queue,
        lambda: service,
        worker_id="test-worker",
        workload_gate=gate,
    )

    queue.claim.assert_awaited_once_with("test-worker")
    service.run_claimed_job.assert_awaited_once_with({"id": "job-1"}, "test-worker")


@pytest.mark.asyncio
async def test_identification_worker_rechecks_gate_immediately_before_claim(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    queue = AsyncMock()
    gate = BackgroundWorkloadGate()

    async def activate_scan() -> bool:
        gate.set_scan_active(True)
        return False

    queue.is_paused.side_effect = activate_scan
    service = AsyncMock()

    async def stop(_seconds: float) -> None:
        raise asyncio.CancelledError

    monkeypatch.setattr(
        "services.native.target_application_runtime.asyncio.sleep", stop
    )

    await run_target_identification_worker(
        lambda: queue,
        lambda: service,
        worker_id="test-worker",
        workload_gate=gate,
    )

    queue.claim.assert_not_awaited()
    service.run_claimed_job.assert_not_awaited()


@pytest.mark.asyncio
async def test_target_operation_worker_recovers_and_dispatches_each_iteration(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    supervisor = AsyncMock()
    sleeps = 0

    async def stop_after_two(_seconds: float) -> None:
        nonlocal sleeps
        sleeps += 1
        if sleeps == 2:
            raise asyncio.CancelledError

    monkeypatch.setattr(
        "services.native.target_application_runtime.asyncio.sleep", stop_after_two
    )
    await run_target_operation_worker(lambda: supervisor, worker_id="test-worker")

    assert supervisor.recover.await_count == 2
    assert supervisor.run_once.await_count == 2
    supervisor.run_once.assert_awaited_with("test-worker")


@pytest.mark.asyncio
async def test_scan_event_ids_are_monotonic_and_counters_are_throttled() -> None:
    store = AsyncMock()
    store.get_stream_revision.side_effect = [7, 8, 9]
    bus = AsyncMock()
    times = iter([0.0, 0.5, 1.0, 2.5])
    events = LibraryScanEventPublisher(store, bus, clock=lambda: next(times))
    run = ScanRun(
        id="run-1",
        kind="incremental",
        trigger="manual",
        state="indexing",
        phase="indexing",
        row_revision=4,
        event_revision=3,
    )

    assert await events.publish(run, event="scan.transition")
    assert await events.publish(run, event="scan.progress", counter=True)
    assert not await events.publish(run, event="scan.progress", counter=True)
    assert await events.publish(run, event="scan.progress", counter=True)

    ids = [call.args[2]["id"] for call in bus.publish.await_args_list]
    assert ids == ["scan:7", "scan:8", "scan:9"]
    assert bus.publish.await_count == 3


@pytest.mark.asyncio
async def test_scan_event_reconnect_gets_latest_and_idle_stream_heartbeats() -> None:
    store = AsyncMock()
    store.get_stream_revision.return_value = 11
    bus = SSEPublisher()
    events = LibraryScanEventPublisher(store, bus, clock=lambda: 0)
    run = ScanRun(
        id="run-1",
        kind="incremental",
        trigger="manual",
        state="discovering",
        phase="discovering",
    )
    await events.publish(run, event="scan.transition")

    subscription = bus.subscribe("target-library-scan")
    latest = await anext(subscription)
    assert latest["data"]["id"] == "scan:11"
    await subscription.aclose()

    idle = bus.subscribe("unused", keepalive_interval=0.001)
    assert await anext(idle) == KEEPALIVE
    await idle.aclose()


def test_target_compat_module_has_no_legacy_scanner_dependency() -> None:
    module = __import__("services.compat.target_scan_service", fromlist=["unused"])
    names = set(module.__dict__)
    assert "LibraryScanner" not in names


@pytest.mark.asyncio
async def test_inventory_file_stat_runs_outside_the_event_loop_thread(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    track = root / "track.flac"
    track.touch()
    event_loop_thread = threading.get_ident()
    stat_threads: list[int] = []
    original_stat = Path.stat

    def record_stat(path: Path, *args, **kwargs):
        if path.name == "track.flac":
            stat_threads.append(threading.get_ident())
        return original_stat(path, *args, **kwargs)

    monkeypatch.setattr(Path, "stat", record_stat)
    store = AsyncMock()
    store.classify_scan_paths.return_value = {"track.flac": ("new", None)}
    store.add_scan_inventory_batch.return_value = (2, 1)
    scanner = LibraryInventoryScanner(
        store,
        directory_walker=lambda *_args, **_kwargs: iter(
            [(str(root), [], ["track.flac"])]
        ),
    )
    run = ScanRun(
        id="run-1",
        kind="incremental",
        trigger="manual",
        state="discovering",
        phase="discovering",
    )
    scope = ScanScope(root_id="root", policy_revision="policy-1")

    _updated, completed = await scanner._walk_scope(
        run,
        scope,
        root,
        root,
        SimpleNamespace(resolve=lambda _path: None),
        AsyncMock(return_value=True),
    )

    assert completed is True
    assert stat_threads
    assert event_loop_thread not in stat_threads


@pytest.mark.asyncio
async def test_automatic_scheduler_uses_terminal_history_and_coordinator() -> None:
    coordinator = AsyncMock()
    coordinator.latest_filesystem_terminal.return_value = ScanRun(
        id="finished",
        kind="incremental",
        trigger="subsonic",
        state="failed",
        phase="reconciling",
        terminal_at=1_800_000_000,
    )
    resolver = SimpleNamespace(
        policy_revision="policy-1",
        settings=SimpleNamespace(
            library_roots=[
                SimpleNamespace(id="root-a", path="/music", policy="automatic")
            ]
        ),
    )
    scheduler = LibraryAutomaticScanScheduler()

    before_due = await scheduler.tick(
        coordinator,
        resolver,
        frequency="24hr",
        daily_time="03:00",
        timezone_name="Europe/London",
        now=datetime.fromtimestamp(1_800_000_100).astimezone(),
    )
    assert before_due is False
    coordinator.request_run.assert_not_awaited()

    due = await scheduler.tick(
        coordinator,
        resolver,
        frequency="24hr",
        daily_time="03:00",
        timezone_name="Europe/London",
        now=datetime.fromtimestamp(1_800_086_401).astimezone(),
    )
    assert due is True
    request = coordinator.request_run.await_args.args[0]
    assert request.trigger == "automatic"

    coordinator.reset_mock()
    manual = await scheduler.tick(
        coordinator,
        resolver,
        frequency="manual",
        daily_time="03:00",
        timezone_name="Europe/London",
        now=datetime.now().astimezone(),
    )
    assert manual is False
    coordinator.latest_filesystem_terminal.assert_not_awaited()


@pytest.mark.asyncio
async def test_automatic_scheduler_scans_allowed_children_of_excluded_roots(
    tmp_path: Path,
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-a",
                    path=str(root),
                    label="Music",
                    policy="excluded",
                    rules=[
                        LibraryPathPolicyRule(
                            id="allowed",
                            relative_path="Allowed",
                            policy="local_metadata",
                        ),
                        LibraryPathPolicyRule(
                            id="nested",
                            relative_path="Allowed/Automatic",
                            policy="automatic",
                        ),
                        LibraryPathPolicyRule(
                            id="excluded-sibling",
                            relative_path="Excluded",
                            policy="excluded",
                        ),
                    ],
                )
            ]
        )
    )
    coordinator = AsyncMock()
    coordinator.latest_filesystem_terminal.return_value = None

    queued = await LibraryAutomaticScanScheduler().tick(
        coordinator,
        resolver,
        frequency="5min",
        daily_time="03:00",
        timezone_name="Europe/London",
        now=datetime.now().astimezone(),
    )

    assert queued is True
    scopes = coordinator.request_run.await_args.args[0].scopes
    assert [(scope.scope_id, scope.relative_path) for scope in scopes] == [
        ("allowed", "Allowed")
    ]
