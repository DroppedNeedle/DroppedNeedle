"""GH-265: PersistenceBase must not dispatch SQLite work through the loop's
shared default executor - that pool is a process-wide resource any blocking
work can saturate, which previously left quick reads (the polled
/library/activity and /home endpoints among them) queued behind unrelated
background work and surfacing as random multi-second "Slow request" warnings.
"""

from __future__ import annotations

import asyncio
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from infrastructure.persistence import _database
from infrastructure.persistence._database import PersistenceBase


class _MinimalStore(PersistenceBase):
    def _ensure_tables(self) -> None:
        pass


@pytest.mark.asyncio
async def test_read_does_not_block_on_starved_default_executor(tmp_path: Path) -> None:
    loop = asyncio.get_running_loop()
    starved_executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="starved-default-test")
    original = loop._default_executor  # type: ignore[attr-defined]
    loop.set_default_executor(starved_executor)
    wedged = threading.Event()
    occupy_future = loop.run_in_executor(None, wedged.wait, 5.0)
    try:
        store = _MinimalStore(tmp_path / "db.sqlite3", threading.Lock())

        # If _read still dispatched via asyncio.to_thread() (the loop's shared,
        # now-starved default executor), this would block until wedged.set()
        # frees the only worker thread. The dedicated _database._DB_EXECUTOR
        # must let it complete immediately regardless of default-executor state.
        result = await asyncio.wait_for(
            store._read(lambda conn: 1),
            timeout=1.0,
        )
        assert result == 1
    finally:
        wedged.set()
        try:
            loop.set_default_executor(original)  # type: ignore[arg-type]
        except TypeError:
            loop._default_executor = original  # type: ignore[attr-defined]
        starved_executor.shutdown(wait=True)
        await occupy_future


@pytest.mark.asyncio
async def test_write_does_not_block_on_starved_default_executor(tmp_path: Path) -> None:
    loop = asyncio.get_running_loop()
    starved_executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="starved-default-test")
    original = loop._default_executor  # type: ignore[attr-defined]
    loop.set_default_executor(starved_executor)
    wedged = threading.Event()
    occupy_future = loop.run_in_executor(None, wedged.wait, 5.0)
    try:
        store = _MinimalStore(tmp_path / "db.sqlite3", threading.Lock())

        def create_and_count(conn) -> int:
            conn.execute("CREATE TABLE IF NOT EXISTS probe (id INTEGER)")
            conn.execute("INSERT INTO probe (id) VALUES (1)")
            return conn.execute("SELECT COUNT(*) FROM probe").fetchone()[0]

        result = await asyncio.wait_for(
            store._write(create_and_count),
            timeout=1.0,
        )
        assert result == 1
    finally:
        wedged.set()
        try:
            loop.set_default_executor(original)  # type: ignore[arg-type]
        except TypeError:
            loop._default_executor = original  # type: ignore[attr-defined]
        starved_executor.shutdown(wait=True)
        await occupy_future


def test_db_executor_is_dedicated_and_sized_independently_of_cpu_count() -> None:
    assert isinstance(_database._DB_EXECUTOR, ThreadPoolExecutor)
    assert _database._DB_EXECUTOR._max_workers == 32  # type: ignore[attr-defined]
