"""Daily personal-mix refresh loop: initial delay, one sleep per iteration, an
error is logged and the loop continues, cancellation breaks cleanly."""

import asyncio
from unittest.mock import AsyncMock

import pytest

from core import tasks
from services.personal_mix_service import PersonalMixSummary


def _break_after(n: int):
    sleeps: list = []

    async def fake_sleep(secs):
        sleeps.append(secs)
        if len(sleeps) >= n:
            raise asyncio.CancelledError

    return sleeps, fake_sleep


@pytest.mark.asyncio
async def test_loop_survives_a_failed_run(monkeypatch):
    # initial delay + 2 interval sleeps -> two runs, the first one exploding
    sleeps, fake_sleep = _break_after(3)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    svc = AsyncMock()
    svc.run_for_all_users.side_effect = [RuntimeError("boom"), PersonalMixSummary()]

    with pytest.raises(asyncio.CancelledError):
        await tasks.refresh_personal_mixes_periodically(svc)

    assert svc.run_for_all_users.await_count == 2  # the error did not kill the loop
    assert sleeps[0] == tasks._PERSONAL_MIX_INITIAL_DELAY
    assert sleeps[1] == tasks._PERSONAL_MIX_REFRESH_INTERVAL


@pytest.mark.asyncio
async def test_cancellation_during_run_breaks_cleanly(monkeypatch):
    sleeps, fake_sleep = _break_after(10)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    svc = AsyncMock()
    svc.run_for_all_users.side_effect = asyncio.CancelledError

    await tasks.refresh_personal_mixes_periodically(svc)  # returns, no raise

    assert svc.run_for_all_users.await_count == 1
    assert sleeps == [tasks._PERSONAL_MIX_INITIAL_DELAY]  # no post-cancel sleep
