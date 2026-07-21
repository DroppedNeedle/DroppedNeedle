"""Native auto-scan periodic task: computes the next run from the last actual scan
(so a restart catches up an overdue scan instead of restarting the interval), supports
a daily-at-a-fixed-time schedule, skips while a scan is running, and stays idle when set
to manual."""

import asyncio
from datetime import datetime
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.settings import LibraryScanScheduleSettings
from core import tasks
from infrastructure.queue.priority_queue import RequestPriority
from services.native.background_workload_gate import BackgroundWorkloadGate


class _Prefs:
    def __init__(self, freq: str, daily_scan_time: str = "03:00") -> None:
        self._sched = LibraryScanScheduleSettings(
            scan_frequency=freq, daily_scan_time=daily_scan_time
        )
        self.saved: list = []

    def get_library_scan_schedule(self):
        return self._sched

    def save_library_scan_schedule(self, schedule) -> None:
        self.saved.append(schedule)

    def get_typed_library_settings_raw(self):
        return SimpleNamespace(library_roots=[SimpleNamespace(path="/music")])


def _break_after(n: int):
    sleeps: list = []

    async def fake_sleep(secs):
        sleeps.append(secs)
        if len(sleeps) >= n:
            raise asyncio.CancelledError

    return sleeps, fake_sleep


def _scan_state(started_at=None, status="idle"):
    """scan_state stub whose started_at advances to 'now' once a scan runs, mirroring
    how the real store records the scan's start - so the loop then sees the next run as
    not yet due and settles into a sleep instead of scanning again."""
    state = {"status": status, "started_at": started_at}

    async def get_state():
        return dict(state)

    async def scan(_paths):
        state["started_at"] = tasks.time()

    return state, get_state, scan


@pytest.mark.asyncio
async def test_artist_cache_warmer_resolves_rebuilt_service_each_cycle(monkeypatch):
    sleeps = 0

    async def stop_after_first_cycle(_seconds):
        nonlocal sleeps
        sleeps += 1
        if sleeps > 1:
            raise asyncio.CancelledError

    service = SimpleNamespace(precache_artist_discovery=AsyncMock())
    service_getter = MagicMock(return_value=service)
    library = SimpleNamespace(
        get_artist_mbid_page=AsyncMock(
            return_value=["60000000-0000-4000-8000-000000000001"]
        )
    )
    monkeypatch.setattr(tasks.asyncio, "sleep", stop_after_first_cycle)

    await tasks.warm_artist_discovery_cache_periodically(
        service_getter, library, interval=10, delay=0
    )

    service_getter.assert_called_once_with()
    service.precache_artist_discovery.assert_awaited_once()


@pytest.mark.asyncio
async def test_artist_cache_warmer_filters_local_ids_before_upstream_calls(monkeypatch):
    sleeps = 0

    async def stop_after_empty_cycle(_seconds):
        nonlocal sleeps
        sleeps += 1
        if sleeps > 1:
            raise asyncio.CancelledError

    service_getter = MagicMock()
    library = SimpleNamespace(
        get_artist_mbid_page=AsyncMock(
            return_value=[
                "f110a324f991fab25548b41e2efeb1bf",
                "unknown_artist",
            ]
        )
    )
    monkeypatch.setattr(tasks.asyncio, "sleep", stop_after_empty_cycle)

    await tasks.warm_artist_discovery_cache_periodically(
        service_getter, library, interval=10, delay=0
    )

    service_getter.assert_not_called()


@pytest.mark.asyncio
async def test_library_cache_warmer_uses_background_musicbrainz_priority(monkeypatch):
    monkeypatch.setattr(tasks.asyncio, "sleep", AsyncMock())
    album_service = MagicMock()
    album_service.is_album_cached = AsyncMock(return_value=False)
    album_service.get_album_info = AsyncMock()
    library_db = MagicMock()
    library_db.get_recent_albums = AsyncMock(
        return_value=[{"mbid": "11111111-1111-1111-1111-111111111111"}]
    )

    await tasks.warm_library_cache(MagicMock(), album_service, library_db)

    album_service.get_album_info.assert_awaited_once_with(
        "11111111-1111-1111-1111-111111111111",
        priority=RequestPriority.BACKGROUND_SYNC,
    )
    library_db.get_recent_albums.assert_awaited_once_with(limit=30)


@pytest.mark.asyncio
async def test_discover_warmer_rechecks_gate_between_units() -> None:
    gate = BackgroundWorkloadGate()
    discover_finished = asyncio.Event()
    home_finished = asyncio.Event()

    async def warm_discover(_user_id: str) -> None:
        gate.set_scan_active(True)
        discover_finished.set()

    async def warm_home(_user_id: str) -> None:
        gate.set_scan_active(True)
        home_finished.set()

    discover = SimpleNamespace(
        warm_cache_thorough=warm_discover,
        peek_freshness=AsyncMock(return_value=(True, False)),
    )
    home = SimpleNamespace(warm_cache=warm_home)
    queue = SimpleNamespace(
        start_build=AsyncMock(), wait_for_build=AsyncMock(return_value=None)
    )
    worker = asyncio.create_task(
        tasks._warm_one_user(
            "gate-test-user",
            discover,
            home,
            {},
            {},
            queue,
            gate,
        )
    )

    await discover_finished.wait()
    await asyncio.sleep(0)
    assert not home_finished.is_set()
    gate.set_scan_active(False)
    await home_finished.wait()
    await asyncio.sleep(0)
    queue.start_build.assert_not_awaited()
    gate.set_scan_active(False)
    await worker

    queue.start_build.assert_awaited_once_with("gate-test-user")


def test_interval_overdue_runs_immediately():
    now = datetime(2026, 6, 22, 12, 0, 0)
    last = now.timestamp() - 2 * 3600  # two intervals ago -> overdue
    assert tasks._seconds_until_next_scan("1hr", "03:00", last, now) == 0.0


def test_interval_future_waits_remaining_gap():
    now = datetime(2026, 6, 22, 12, 0, 0)
    last = now.timestamp() - 600  # 10 min into a 1hr interval
    assert tasks._seconds_until_next_scan("1hr", "03:00", last, now) == pytest.approx(
        3000.0
    )


def test_interval_never_scanned_runs_immediately():
    now = datetime(2026, 6, 22, 12, 0, 0)
    assert tasks._seconds_until_next_scan("24hr", "03:00", None, now) == 0.0


def test_daily_before_target_waits_until_target():
    now = datetime(2026, 6, 22, 1, 0, 0)  # 01:00, target 03:00
    assert tasks._seconds_until_next_scan("daily", "03:00", None, now) == 2 * 3600


def test_daily_after_target_unscanned_runs_immediately():
    # booted at 09:00; today's 03:00 passed with nothing scanned since -> catch up now
    now = datetime(2026, 6, 22, 9, 0, 0)
    yesterday = datetime(2026, 6, 21, 3, 0, 0).timestamp()
    assert tasks._seconds_until_next_scan("daily", "03:00", yesterday, now) == 0.0


def test_daily_after_target_already_scanned_waits_until_tomorrow():
    now = datetime(2026, 6, 22, 9, 0, 0)
    scanned_today = datetime(2026, 6, 22, 3, 0, 5).timestamp()
    assert (
        tasks._seconds_until_next_scan("daily", "03:00", scanned_today, now)
        == 18 * 3600
    )


def test_parse_daily_time_falls_back_on_garbage():
    assert tasks._parse_daily_time("3am") == (3, 0)
    assert tasks._parse_daily_time("") == (3, 0)
    assert tasks._parse_daily_time("25:99") == (3, 0)
    assert tasks._parse_daily_time("06:30") == (6, 30)


@pytest.mark.asyncio
async def test_auto_scan_runs_when_overdue_then_settles(monkeypatch):
    # never scanned -> the first tick is due immediately (no full-interval wait first)
    sleeps, fake_sleep = _break_after(1)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    _, get_state, scan = _scan_state(started_at=None)
    scanner = SimpleNamespace(scan=AsyncMock(side_effect=scan))
    scan_state = SimpleNamespace(get_state=get_state)
    prefs = _Prefs("30min")

    await tasks.auto_scan_library_periodically(scanner, scan_state, prefs)

    scanner.scan.assert_awaited_once()
    assert prefs.saved and prefs.saved[-1].last_scan_success is True
    # after scanning it waits rather than scanning again -> that wait is what cancels
    assert sleeps and sleeps[0] > 0


@pytest.mark.asyncio
async def test_auto_scan_skips_when_already_scanning(monkeypatch):
    _, fake_sleep = _break_after(2)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    scanner = SimpleNamespace(scan=AsyncMock())
    scan_state = SimpleNamespace(
        get_state=AsyncMock(return_value={"status": "scanning"})
    )
    prefs = _Prefs("30min")

    await tasks.auto_scan_library_periodically(scanner, scan_state, prefs)

    scanner.scan.assert_not_awaited()
    assert prefs.saved == []  # a skipped tick must not touch last_scan status


@pytest.mark.asyncio
async def test_auto_scan_manual_stays_idle(monkeypatch):
    sleeps, fake_sleep = _break_after(1)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    scanner = SimpleNamespace(scan=AsyncMock())
    scan_state = SimpleNamespace(get_state=AsyncMock(return_value={"status": "idle"}))

    await tasks.auto_scan_library_periodically(scanner, scan_state, _Prefs("manual"))

    scanner.scan.assert_not_awaited()
    assert sleeps == [tasks._SCHEDULER_TICK]  # manual -> idle tick, never scans


@pytest.mark.asyncio
async def test_auto_scan_daily_runs_when_due(monkeypatch):
    # 09:00 with target 03:00 already passed and nothing scanned -> scans this tick
    sleeps, fake_sleep = _break_after(1)
    monkeypatch.setattr(tasks.asyncio, "sleep", fake_sleep)
    fixed = datetime(2026, 6, 22, 9, 0, 0)
    monkeypatch.setattr(tasks, "datetime", SimpleNamespace(now=lambda: fixed))
    monkeypatch.setattr(tasks, "time", lambda: fixed.timestamp())

    _, get_state, scan = _scan_state(started_at=None)
    scanner = SimpleNamespace(scan=AsyncMock(side_effect=scan))
    scan_state = SimpleNamespace(get_state=get_state)
    prefs = _Prefs("daily", daily_scan_time="03:00")

    await tasks.auto_scan_library_periodically(scanner, scan_state, prefs)

    scanner.scan.assert_awaited_once()
    assert prefs.saved and prefs.saved[-1].last_scan_success is True
