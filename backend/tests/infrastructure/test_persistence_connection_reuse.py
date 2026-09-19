"""PersistenceBase reuses one SQLite connection per (thread, database) for reads.

Opening a connection re-parses the whole schema, which dominated the CPU cost
of every read. These tests pin the contract that makes sharing a connection
between stores and reads indistinguishable from opening a fresh one: per-store
settings are applied on checkout, nothing an operation leaves behind survives
check-in, writes keep a fresh connection, and anything that customises
``_connect`` still gets a fresh connection.
"""

from __future__ import annotations

import gc
import os
import sqlite3
import threading
from pathlib import Path

import pytest

from infrastructure.persistence import _database
from infrastructure.persistence._database import PersistenceBase


class _PlainStore(PersistenceBase):
    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute("CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY)")
            conn.commit()
        finally:
            conn.close()


class _ForeignKeyStore(_PlainStore):
    foreign_keys = True


class _NoBusyTimeoutStore(_PlainStore):
    busy_timeout_ms = None


class _CustomBusyTimeoutStore(_PlainStore):
    busy_timeout_ms = 1234


class _OverridingStore(_PlainStore):
    def _connect(self) -> sqlite3.Connection:
        return super()._connect()


def _pragma(name: str):
    return lambda conn: conn.execute(f"PRAGMA {name}").fetchone()[0]


def _count(conn: sqlite3.Connection) -> int:
    return conn.execute("SELECT COUNT(*) FROM items").fetchone()[0]


def _same_connection(conn: sqlite3.Connection) -> sqlite3.Connection:
    return conn


def _run(sql: str):
    # Returning the cursor would retire the pooled connection (see
    # test_cursor_escaping_an_operation_cannot_pin_a_stale_snapshot).
    def operation(conn: sqlite3.Connection) -> None:
        conn.execute(sql)

    return operation


def test_reads_from_every_store_on_one_thread_share_a_connection(tmp_path: Path) -> None:
    db = tmp_path / "library.db"
    lock = threading.Lock()
    plain = _PlainStore(db, lock)
    foreign = _ForeignKeyStore(db, lock)

    first = plain._execute(_same_connection, False)
    assert plain._execute(_same_connection, False) is first
    assert foreign._execute(_same_connection, False) is first


def test_writes_use_a_fresh_connection(tmp_path: Path) -> None:
    store = _PlainStore(tmp_path / "library.db", threading.Lock())
    pooled = store._execute(_same_connection, False)

    written_with = store._execute(_same_connection, True)

    assert written_with is not pooled
    with pytest.raises(sqlite3.ProgrammingError):
        written_with.execute("SELECT 1")
    assert store._execute(_same_connection, False) is pooled


def test_each_store_profile_is_applied_on_checkout(tmp_path: Path) -> None:
    db = tmp_path / "library.db"
    lock = threading.Lock()
    plain = _PlainStore(db, lock)
    foreign = _ForeignKeyStore(db, lock)
    no_timeout = _NoBusyTimeoutStore(db, lock)
    custom_timeout = _CustomBusyTimeoutStore(db, lock)
    driver_default = sqlite3.connect(db).execute("PRAGMA busy_timeout").fetchone()[0]

    assert foreign._execute(_pragma("foreign_keys"), False) == 1
    assert plain._execute(_pragma("foreign_keys"), False) == 0
    assert foreign._execute(_pragma("foreign_keys"), False) == 1

    assert custom_timeout._execute(_pragma("busy_timeout"), False) == 1234
    assert no_timeout._execute(_pragma("busy_timeout"), False) == driver_default
    assert plain._execute(_pragma("busy_timeout"), False) == 5000


def test_foreign_keys_are_enforced_through_a_shared_connection(tmp_path: Path) -> None:
    db = tmp_path / "library.db"
    lock = threading.Lock()
    plain = _PlainStore(db, lock)
    foreign = _ForeignKeyStore(db, lock)

    def create_child(conn: sqlite3.Connection) -> None:
        conn.execute(
            "CREATE TABLE child (id INTEGER PRIMARY KEY, "
            "item_id INTEGER NOT NULL REFERENCES items(id))"
        )

    plain._execute(create_child, True)
    orphan = "INSERT INTO child (item_id) VALUES (999)"

    with pytest.raises(sqlite3.IntegrityError):
        foreign._execute(_run(orphan), True)
    plain._execute(_run(orphan), True)


def test_mmap_size_changed_by_an_operation_is_restored_on_check_in(tmp_path: Path) -> None:
    store = _PlainStore(tmp_path / "library.db", threading.Lock())
    default = store._execute(_pragma("mmap_size"), False)

    def raise_mmap(conn: sqlite3.Connection) -> sqlite3.Connection:
        conn.execute("PRAGMA mmap_size=1048576")
        return conn

    conn = store._execute(raise_mmap, False)

    # Checked on the connection itself: an idle connection must not keep the
    # mapping until some later operation happens to reset it.
    assert conn.execute("PRAGMA mmap_size").fetchone()[0] == default


def test_uncommitted_changes_from_a_read_are_discarded(tmp_path: Path) -> None:
    store = _PlainStore(tmp_path / "library.db", threading.Lock())

    store._execute(_run("INSERT INTO items DEFAULT VALUES"), False)

    assert store._execute(_count, False) == 0
    assert store._execute(lambda conn: conn.in_transaction, False) is False


def test_failed_operation_rolls_back_and_replaces_the_connection(tmp_path: Path) -> None:
    store = _PlainStore(tmp_path / "library.db", threading.Lock())
    before = store._execute(_same_connection, False)

    def insert_then_fail(conn: sqlite3.Connection) -> None:
        conn.execute("INSERT INTO items DEFAULT VALUES")
        raise RuntimeError("boom")

    with pytest.raises(RuntimeError):
        store._execute(insert_then_fail, False)

    assert store._execute(_count, False) == 0
    assert store._execute(_same_connection, False) is not before
    with pytest.raises(sqlite3.ProgrammingError):
        before.execute("SELECT 1")


def test_nested_operation_gets_its_own_connection(tmp_path: Path) -> None:
    db = tmp_path / "library.db"
    lock = threading.Lock()
    outer_store = _PlainStore(db, lock)
    inner_store = _ForeignKeyStore(db, lock)
    seen: dict[str, object] = {}

    def outer(conn: sqlite3.Connection) -> None:
        seen["outer_conn"] = conn
        conn.execute("INSERT INTO items DEFAULT VALUES")
        seen["inner_conn"] = inner_store._execute(_same_connection, False)
        seen["inner_count"] = inner_store._execute(_count, False)
        seen["outer_foreign_keys"] = conn.execute("PRAGMA foreign_keys").fetchone()[0]
        raise RuntimeError("abort outer")

    with pytest.raises(RuntimeError):
        outer_store._execute(outer, False)

    assert seen["inner_conn"] is not seen["outer_conn"]
    assert seen["inner_count"] == 0
    assert seen["outer_foreign_keys"] == 0
    assert outer_store._execute(_count, False) == 0


def test_cursor_escaping_an_operation_cannot_pin_a_stale_snapshot(tmp_path: Path) -> None:
    db = tmp_path / "library.db"
    store = _PlainStore(db, threading.Lock())
    store._execute(
        lambda conn: conn.executemany("INSERT INTO items DEFAULT VALUES", [()] * 5), True
    )
    escaped: list[sqlite3.Cursor] = []

    def leak_partially_read_cursor(conn: sqlite3.Connection) -> sqlite3.Connection:
        cursor = conn.execute("SELECT id FROM items")
        cursor.fetchone()
        escaped.append(cursor)
        return conn

    leaked_on = store._execute(leak_partially_read_cursor, False)
    with sqlite3.connect(db) as writer:
        writer.execute("INSERT INTO items DEFAULT VALUES")

    assert store._execute(_count, False) == 6
    assert store._execute(_same_connection, False) is not leaked_on


def test_cursors_released_by_an_operation_keep_the_connection(tmp_path: Path) -> None:
    store = _PlainStore(tmp_path / "library.db", threading.Lock())

    def partial_read(conn: sqlite3.Connection) -> sqlite3.Connection:
        conn.execute("SELECT id FROM items").fetchone()
        return conn

    first = store._execute(partial_read, False)

    assert store._execute(_same_connection, False) is first


def _rss_bytes() -> int:
    with open("/proc/self/status") as status:
        for line in status:
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) * 1024
    raise AssertionError("VmRSS not found")


_MIB = 1024 * 1024
_needs_proc = pytest.mark.skipif(
    not Path("/proc/self/status").exists(), reason="measures resident memory via /proc"
)


@_needs_proc
def test_pooled_connection_does_not_retain_bound_payloads(tmp_path: Path) -> None:
    # A cached statement keeps its compiled program and SQLite's copy of its
    # last bound values alive for the life of the connection; close() used to
    # free them. Pooled connections are therefore retired once the cache holds
    # more than the 2MiB budget (and run uncached where sqlite_stmt is missing).
    store = _PlainStore(tmp_path / "library.db", threading.Lock())
    body = os.urandom(64 * _MIB)

    def read_with_large_parameter(conn: sqlite3.Connection) -> None:
        conn.execute("SELECT length(?)", (body,)).fetchone()

    store._execute(_count, False)
    gc.collect()
    before = _rss_bytes()
    store._execute(read_with_large_parameter, False)
    gc.collect()

    assert _rss_bytes() - before < 24 * _MIB


_needs_sqlite_stmt = pytest.mark.skipif(
    not _database._can_measure_statement_memory(),
    reason="SQLite built without the sqlite_stmt virtual table",
)


def _cached_statement_count(conn: sqlite3.Connection, sql: str) -> int:
    return conn.execute("SELECT COUNT(*) FROM sqlite_stmt WHERE sql = ?", (sql,)).fetchone()[0]


@_needs_sqlite_stmt
def test_bulk_reads_keep_their_statement_cache(tmp_path: Path) -> None:
    store = _PlainStore(tmp_path / "library.db", threading.Lock())
    sql = "SELECT id FROM items WHERE id = ?"

    def repeated_lookups(conn: sqlite3.Connection) -> sqlite3.Connection:
        for item_id in range(50):
            conn.execute(sql, (item_id,)).fetchall()
        return conn

    conn = store._execute(repeated_lookups, False)

    assert _cached_statement_count(conn, sql) == 1


@_needs_sqlite_stmt
def test_cached_statements_survive_checkouts_across_stores(tmp_path: Path) -> None:
    db = tmp_path / "library.db"
    lock = threading.Lock()
    plain = _PlainStore(db, lock)
    foreign = _ForeignKeyStore(db, lock)
    sql = "SELECT id FROM items WHERE id = ?"

    def lookup(conn: sqlite3.Connection) -> sqlite3.Connection:
        conn.execute(sql, (1,)).fetchall()
        return conn

    conn = foreign._execute(lookup, False)
    foreign._execute(lookup, False)
    foreign._execute(lookup, False)

    reprepared = conn.execute(
        "SELECT reprep FROM sqlite_stmt WHERE sql = ?", (sql,)
    ).fetchone()[0]
    assert reprepared == 0
    assert plain._execute(_pragma("foreign_keys"), False) == 0
    assert foreign._execute(_pragma("foreign_keys"), False) == 1


@_needs_sqlite_stmt
def test_connection_whose_statements_hold_too_much_memory_is_retired(tmp_path: Path) -> None:
    store = _PlainStore(tmp_path / "library.db", threading.Lock())
    body = b"x" * (2 * _database._POOLED_STATEMENT_MEMORY_BUDGET_BYTES)

    def read_with_large_parameter(conn: sqlite3.Connection) -> sqlite3.Connection:
        conn.execute("SELECT length(?)", (body,)).fetchone()
        return conn

    used = store._execute(read_with_large_parameter, False)

    assert store._execute(_same_connection, False) is not used


@_needs_sqlite_stmt
def test_without_sqlite_stmt_pooled_connections_run_uncached(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(_database, "_statement_memory_measurable", False)
    store = _PlainStore(tmp_path / "library.db", threading.Lock())
    sql = "SELECT id FROM items WHERE id = ?"

    def lookup(conn: sqlite3.Connection) -> sqlite3.Connection:
        conn.execute(sql, (1,)).fetchall()
        return conn

    conn = store._execute(lookup, False)

    assert store._execute(_same_connection, False) is conn
    assert _cached_statement_count(conn, sql) == 0


def test_replaced_database_file_is_not_served_by_a_stale_connection(tmp_path: Path) -> None:
    db = tmp_path / "library.db"
    store = _PlainStore(db, threading.Lock())
    store._execute(_run("INSERT INTO items DEFAULT VALUES"), True)
    stale = store._execute(_same_connection, False)

    replacement = tmp_path / "replacement.db"
    with sqlite3.connect(replacement) as conn:
        conn.execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
    replacement.replace(db)
    for suffix in ("-wal", "-shm"):
        Path(f"{db}{suffix}").unlink(missing_ok=True)

    assert store._execute(_same_connection, False) is not stale
    assert store._execute(_count, False) == 0


def test_instance_patched_connect_is_used_for_every_operation(tmp_path: Path) -> None:
    store = _PlainStore(tmp_path / "library.db", threading.Lock())
    original_connect = store._connect
    calls = 0

    def traced_connect() -> sqlite3.Connection:
        nonlocal calls
        calls += 1
        return original_connect()

    store._connect = traced_connect  # type: ignore[method-assign]
    store._execute(_count, False)
    store._execute(_count, True)

    assert calls == 2


def test_subclass_overriding_connect_keeps_fresh_connections(tmp_path: Path) -> None:
    store = _OverridingStore(tmp_path / "library.db", threading.Lock())

    first = store._execute(_same_connection, False)

    assert store._execute(_same_connection, False) is not first
    with pytest.raises(sqlite3.ProgrammingError):
        first.execute("SELECT 1")


def test_connections_are_not_shared_between_threads(tmp_path: Path) -> None:
    store = _PlainStore(tmp_path / "library.db", threading.Lock())
    here = store._execute(_same_connection, False)
    there: list[sqlite3.Connection] = []

    worker = threading.Thread(target=lambda: there.append(store._execute(_same_connection, False)))
    worker.start()
    worker.join()

    assert there and there[0] is not here


def test_pooled_connections_per_thread_are_bounded(tmp_path: Path) -> None:
    limit = _database._POOLED_CONNECTIONS_PER_THREAD
    stores = [
        _PlainStore(tmp_path / f"db-{index}.sqlite3", threading.Lock())
        for index in range(limit + 2)
    ]

    connections = [store._execute(_same_connection, False) for store in stores]

    assert len(_database._thread_connections.by_path) <= limit
    with pytest.raises(sqlite3.ProgrammingError):
        connections[0].execute("SELECT 1")
    connections[-1].execute("SELECT 1")


@pytest.mark.asyncio
async def test_async_reads_and_writes_use_pooled_connections(tmp_path: Path) -> None:
    store = _ForeignKeyStore(tmp_path / "library.db", threading.Lock())

    await store._write(_run("INSERT INTO items DEFAULT VALUES"))

    assert await store._read(_count) == 1
    assert await store._read(_pragma("foreign_keys")) == 1
