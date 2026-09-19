"""Follow-poll loop shape (BrainzMashEfficiency Step 04-3 / T6): 60 s mean tick
with +/-20% jitter, exactly one sleep per iteration INCLUDING the error path,
cancellation breaks cleanly, the service is re-resolved via the getter every
iteration, and a due artist is still serviced on the next tick after due."""

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
async def test_follow_poll_sleep_within_jitter_band(monkeypatch):
    """The 60 s tick carries +/-20% jitter -> [48, 72]. The fake uniform
    returns the low edge then the high edge, pinning the derived band and
    that the loop sleeps the jittered value as-is. Initial delay unjittered."""
    sleeps, fake_sleep = _break_after(3)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    bounds: list = []

    def fake_uniform(a, b):
        bounds.append((a, b))
        return a if len(bounds) % 2 == 1 else b

    monkeypatch.setattr(tasks.random, "uniform", fake_uniform)
    svc = AsyncMock()

    with pytest.raises(asyncio.CancelledError):
        await tasks.poll_followed_artists_new_releases(lambda: svc)

    assert svc.run_poll.await_count == 2
    assert sleeps[0] == tasks._FOLLOW_POLL_INITIAL_DELAY
    flat = [edge for pair in bounds for edge in pair]
    assert flat == pytest.approx([48.0, 72.0, 48.0, 72.0])
    assert sleeps[1] == bounds[0][0]
    assert sleeps[2] == bounds[1][1]


@pytest.mark.asyncio
async def test_follow_poll_mean_preserved(monkeypatch):
    """Midpoint uniform pins the mean: jitter must preserve the 60 s average."""
    sleeps, fake_sleep = _break_after(3)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    monkeypatch.setattr(tasks.random, "uniform", lambda a, b: (a + b) / 2)
    svc = AsyncMock()

    with pytest.raises(asyncio.CancelledError):
        await tasks.poll_followed_artists_new_releases(lambda: svc)

    assert svc.run_poll.await_count == 2
    assert sleeps[0] == tasks._FOLLOW_POLL_INITIAL_DELAY
    assert sleeps[1:] == pytest.approx([tasks._FOLLOW_POLL_INTERVAL] * 2)


@pytest.mark.asyncio
async def test_loop_survives_a_failed_poll_with_one_sleep_per_iteration(monkeypatch):
    # initial delay + 2 interval sleeps -> two polls, the first one exploding
    sleeps, fake_sleep = _break_after(3)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    monkeypatch.setattr(tasks.random, "uniform", lambda a, b: (a + b) / 2)
    svc = AsyncMock()
    svc.run_poll.side_effect = [RuntimeError("boom"), None]

    with pytest.raises(asyncio.CancelledError):
        await tasks.poll_followed_artists_new_releases(lambda: svc)

    assert svc.run_poll.await_count == 2  # the error did not kill the loop
    assert sleeps[0] == tasks._FOLLOW_POLL_INITIAL_DELAY
    # exactly one sleep per iteration, error path included
    assert sleeps[1:] == pytest.approx([tasks._FOLLOW_POLL_INTERVAL] * 2)


@pytest.mark.asyncio
async def test_cancellation_during_poll_breaks_cleanly(monkeypatch):
    sleeps, fake_sleep = _break_after(10)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    monkeypatch.setattr(tasks.random, "uniform", lambda a, b: (a + b) / 2)
    svc = AsyncMock()
    svc.run_poll.side_effect = asyncio.CancelledError

    await tasks.poll_followed_artists_new_releases(lambda: svc)  # returns, no raise

    assert svc.run_poll.await_count == 1
    assert sleeps == [tasks._FOLLOW_POLL_INITIAL_DELAY]  # no post-cancel sleep


@pytest.mark.asyncio
async def test_getter_resolved_fresh_every_iteration(monkeypatch):
    sleeps, fake_sleep = _break_after(4)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    monkeypatch.setattr(tasks.random, "uniform", lambda a, b: (a + b) / 2)
    first, second, third = AsyncMock(), AsyncMock(), AsyncMock()
    instances = [first, second, third]
    getter_calls = []

    def getter():
        getter_calls.append(1)
        return instances[len(getter_calls) - 1]

    with pytest.raises(asyncio.CancelledError):
        await tasks.poll_followed_artists_new_releases(getter)

    # three iterations -> three getter calls, each instance polled exactly once
    assert len(getter_calls) == 3
    for instance in (first, second, third):
        assert instance.run_poll.await_count == 1


@pytest.mark.asyncio
async def test_due_artist_serviced_next_tick_after_due(monkeypatch):
    """Latency pin (T6): jitter changes tick spacing, never tick coverage -
    every tick still polls, so an artist due just after tick k is serviced at
    tick k+1. Worst case (max jitter pinned): one 72 s interval later."""
    now = 0.0
    sleeps: list = []
    poll_times: list = []

    async def clock_sleep(secs):
        nonlocal now
        now += secs
        sleeps.append(secs)
        if len(sleeps) >= 4:  # initial delay + 3 ticks
            raise asyncio.CancelledError

    monkeypatch.setattr(tasks.asyncio, "sleep", clock_sleep)
    monkeypatch.setattr(tasks.random, "uniform", lambda a, b: 72.0)
    svc = AsyncMock()

    async def record_poll():
        poll_times.append(now)

    svc.run_poll.side_effect = record_poll

    with pytest.raises(asyncio.CancelledError):
        await tasks.poll_followed_artists_new_releases(lambda: svc)

    assert poll_times == [300.0, 372.0, 444.0]  # no skipped ticks under jitter
    due_at = poll_times[0] + 1
    serviced_at = next(t for t in poll_times if t >= due_at)
    assert serviced_at == poll_times[1]
    assert serviced_at - due_at <= tasks._FOLLOW_POLL_INTERVAL * 1.2
