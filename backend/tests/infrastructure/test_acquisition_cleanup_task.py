from types import SimpleNamespace

import pytest

from core import tasks


@pytest.mark.asyncio
async def test_cleanup_worker_resolves_service_each_iteration(monkeypatch):
    calls: list[tuple[str, str]] = []
    services = []
    for name in ("first", "second"):

        async def reconcile(value=name):
            calls.append((value, "reconcile"))

        async def run_once(worker_id, value=name):
            calls.append((value, worker_id))

        services.append(
            SimpleNamespace(reconcile_legacy_mount=reconcile, run_once=run_once)
        )
    getter_calls = 0

    def getter():
        nonlocal getter_calls
        service = services[getter_calls]
        getter_calls += 1
        return service

    sleeps: list[float] = []

    async def stop_after_two_intervals(interval: float):
        sleeps.append(interval)
        if len(sleeps) == 2:
            raise tasks.asyncio.CancelledError

    monkeypatch.setattr(tasks.asyncio, "sleep", stop_after_two_intervals)

    await tasks.run_acquisition_cleanup_periodically(getter, interval=30.0)

    assert getter_calls == 2
    assert sleeps == [30.0, 30.0]
    assert calls == [
        ("first", "reconcile"),
        ("first", "acquisition-cleanup-worker"),
        ("second", "reconcile"),
        ("second", "acquisition-cleanup-worker"),
    ]
