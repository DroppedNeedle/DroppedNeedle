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
    # midpoint uniform pins the mean: jitter must preserve the 900 s average
    monkeypatch.setattr(tasks.random, "uniform", lambda a, b: (a + b) / 2)
    svc = AsyncMock()
    svc.run_sweep.side_effect = [RuntimeError("boom"), None]

    with pytest.raises(asyncio.CancelledError):
        await tasks.run_wanted_watcher_periodically(lambda: svc)

    assert svc.run_sweep.await_count == 2  # the error did not kill the loop
    assert sleeps[0] == tasks._WANTED_WATCHER_INITIAL_DELAY
    # exactly one sleep per iteration, error path included; mean preserved
    assert sleeps[1:] == pytest.approx([tasks._WANTED_WATCHER_INTERVAL] * 2)


@pytest.mark.asyncio
async def test_wanted_sweep_sleep_within_jitter_band(monkeypatch):
    """Step 04-3: the 900 s sweep tick carries +/-20% jitter -> [720, 1080].
    The fake uniform returns the low edge then the high edge, pinning the
    derived band and that the loop sleeps the jittered value as-is."""
    sleeps, fake_sleep = _break_after(3)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    bounds: list = []

    def fake_uniform(a, b):
        bounds.append((a, b))
        return a if len(bounds) % 2 == 1 else b

    monkeypatch.setattr(tasks.random, "uniform", fake_uniform)
    svc = AsyncMock()

    with pytest.raises(asyncio.CancelledError):
        await tasks.run_wanted_watcher_periodically(lambda: svc)

    assert svc.run_sweep.await_count == 2
    assert sleeps[0] == tasks._WANTED_WATCHER_INITIAL_DELAY
    flat = [edge for pair in bounds for edge in pair]
    assert flat == pytest.approx([720.0, 1080.0, 720.0, 1080.0])
    assert sleeps[1] == bounds[0][0]
    assert sleeps[2] == bounds[1][1]


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


@pytest.mark.asyncio
async def test_store_prune_also_prunes_native_identification(monkeypatch):
    """F-PERF-04: the six-hour store-prune pass invokes the native library
    store's bounded identification retention alongside the other stores."""
    sleeps, fake_sleep = _break_after(2)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    request_history, mbid_store, youtube_store = AsyncMock(), AsyncMock(), AsyncMock()
    wanted_store = AsyncMock()
    native_store = AsyncMock()
    native_store.prune_old_terminal_identification_jobs.return_value = (3, True)

    with pytest.raises(asyncio.CancelledError):
        await tasks.prune_stores_periodically(
            request_history,
            mbid_store,
            youtube_store,
            request_retention_days=180,
            wanted_store=wanted_store,
            native_store=native_store,
        )

    wanted_store.prune.assert_awaited_once_with(180)
    native_store.prune_old_terminal_identification_jobs.assert_awaited_once()
    _, kwargs = native_store.prune_old_terminal_identification_jobs.await_args
    assert set(kwargs) == {"now"}  # signed defaults: 30-day retention, bounded


@pytest.mark.asyncio
async def test_store_prune_survives_native_identification_errors(monkeypatch):
    """One failing store must not break the loop: the error is logged and the
    pass still reaches the interval sleep (house background-loop rule)."""
    sleeps, fake_sleep = _break_after(2)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    request_history, mbid_store, youtube_store = AsyncMock(), AsyncMock(), AsyncMock()
    native_store = AsyncMock()
    native_store.prune_old_terminal_identification_jobs.side_effect = RuntimeError("db busy")

    with pytest.raises(asyncio.CancelledError):
        await tasks.prune_stores_periodically(
            request_history,
            mbid_store,
            youtube_store,
            native_store=native_store,
        )

    # the loop reached the sleep after the failure instead of crashing
    assert len(sleeps) >= 1
