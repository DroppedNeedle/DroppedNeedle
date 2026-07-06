"""Wanted-watcher loop shape (Wanted plan §5.3): initial delay, exactly one
sleep per iteration INCLUDING the error path, cancellation breaks cleanly, and
the watcher is re-resolved via the getter every iteration (a settings save
rebuilds the singletons it dispatches through)."""

import asyncio
from unittest.mock import AsyncMock

import pytest

from core import tasks


def _break_after(n: int):
    sleeps: list = []

    async def fake_sleep(secs):
        sleeps.append(secs)
        if len(sleeps) >= n:
            raise asyncio.CancelledError

    return sleeps, fake_sleep


@pytest.mark.asyncio
async def test_loop_survives_a_failed_sweep_with_one_sleep_per_iteration(monkeypatch):
    # initial delay + 2 interval sleeps -> two sweeps, the first one exploding
    sleeps, fake_sleep = _break_after(3)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    svc = AsyncMock()
    svc.run_sweep.side_effect = [RuntimeError("boom"), None]

    with pytest.raises(asyncio.CancelledError):
        await tasks.run_wanted_watcher_periodically(lambda: svc)

    assert svc.run_sweep.await_count == 2  # the error did not kill the loop
    assert sleeps[0] == tasks._WANTED_WATCHER_INITIAL_DELAY
    # exactly one sleep per iteration, error path included
    assert sleeps[1:] == [tasks._WANTED_WATCHER_INTERVAL, tasks._WANTED_WATCHER_INTERVAL]


@pytest.mark.asyncio
async def test_cancellation_during_sweep_breaks_cleanly(monkeypatch):
    sleeps, fake_sleep = _break_after(10)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    svc = AsyncMock()
    svc.run_sweep.side_effect = asyncio.CancelledError

    await tasks.run_wanted_watcher_periodically(lambda: svc)  # returns, no raise

    assert svc.run_sweep.await_count == 1
    assert sleeps == [tasks._WANTED_WATCHER_INITIAL_DELAY]  # no post-cancel sleep


@pytest.mark.asyncio
async def test_getter_resolved_fresh_every_iteration(monkeypatch):
    sleeps, fake_sleep = _break_after(4)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    first, second, third = AsyncMock(), AsyncMock(), AsyncMock()
    instances = [first, second, third]
    getter_calls = []

    def getter():
        getter_calls.append(1)
        return instances[len(getter_calls) - 1]

    with pytest.raises(asyncio.CancelledError):
        await tasks.run_wanted_watcher_periodically(getter)

    # three iterations -> three getter calls, each instance swept exactly once
    assert len(getter_calls) == 3
    for instance in (first, second, third):
        assert instance.run_sweep.await_count == 1


@pytest.mark.asyncio
async def test_store_prune_also_prunes_wanted(monkeypatch):
    sleeps, fake_sleep = _break_after(2)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    request_history, mbid_store, youtube_store = AsyncMock(), AsyncMock(), AsyncMock()
    wanted_store = AsyncMock()

    with pytest.raises(asyncio.CancelledError):
        await tasks.prune_stores_periodically(
            request_history, mbid_store, youtube_store,
            request_retention_days=180, wanted_store=wanted_store,
        )

    wanted_store.prune.assert_awaited_once_with(180)
