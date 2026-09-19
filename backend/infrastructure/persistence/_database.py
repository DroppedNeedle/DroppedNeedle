"""Shared SQLite infrastructure for all persistence stores."""

import asyncio
from collections import OrderedDict
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from dataclasses import dataclass
import json
import os
import sqlite3
import threading
import unicodedata
import weakref
from pathlib import Path
from typing import Any, TypeVar, cast

from infrastructure.persistence.connection_settings import (
    report_connection_settings,
)

T = TypeVar("T")


class PriorityWriteLock:
    """A foreground-first process lock with bounded background starvation."""

    def __init__(self, *, foreground_burst: int = 8) -> None:
        if foreground_burst < 1:
            raise ValueError("foreground_burst must be positive")
        self._condition = threading.Condition()
        self._foreground_burst = foreground_burst
        self._active = False
        self._foreground_waiters = 0
        self._background_waiters = 0
        self._foreground_grants = 0

    def __enter__(self) -> "PriorityWriteLock":
        self.acquire()
        return self

    def __exit__(self, *_args: object) -> None:
        self.release()

    def acquire(self) -> None:
        with self._condition:
            self._foreground_waiters += 1
            try:
                while self._active or (
                    self._background_waiters
                    and self._foreground_grants >= self._foreground_burst
                ):
                    self._condition.wait()
                self._active = True
                self._foreground_grants += 1
            finally:
                self._foreground_waiters -= 1

    def acquire_background(self) -> None:
        with self._condition:
            if self._background_waiters == 0:
                self._foreground_grants = 0
            self._background_waiters += 1
            try:
                while self._active or (
                    self._foreground_waiters
                    and self._foreground_grants < self._foreground_burst
                ):
                    self._condition.wait()
                self._active = True
                self._foreground_grants = 0
            finally:
                self._background_waiters -= 1

    def release(self) -> None:
        with self._condition:
            if not self._active:
                raise RuntimeError("Cannot release an unlocked persistence lock")
            self._active = False
            self._condition.notify_all()

    @contextmanager
    def background(self):
        self.acquire_background()
        try:
            yield self
        finally:
            self.release()


def _fold_text(value: Any) -> Any:
    """Casefold, strip diacritics, and normalize whitespace.

    Registered as the SQLite ``fold()`` function and applied to both column and
    pattern in LIKE searches, so library search is accent- and case-insensitive
    for keyboards that can't type the accent. NFKD also folds compatibility forms
    (ligatures, full-width chars) into their plain equivalents, which is desirable
    for forgiving search and matches the codebase's other search normalizers
    (search_service, plex/navidrome). Non-strings (incl. NULL) pass through
    unchanged so the surrounding LIKE keeps its normal semantics."""
    if not isinstance(value, str):
        return value
    decomposed = unicodedata.normalize("NFKD", value)
    without_marks = "".join(
        character for character in decomposed if not unicodedata.combining(character)
    ).casefold()
    return " ".join(without_marks.split())


def _encode_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"))


def _decode_json(text: str) -> Any:
    return json.loads(text)


def _normalize(value: str | None) -> str:
    return value.lower() if isinstance(value, str) else ""


def _decode_rows(rows: list[sqlite3.Row]) -> list[dict[str, Any]]:
    decoded: list[dict[str, Any]] = []
    for row in rows:
        try:
            payload = _decode_json(row["raw_json"])
        except Exception:  # noqa: BLE001
            continue
        if isinstance(payload, dict):
            decoded.append(payload)
    return decoded


def _safe_alter(conn: sqlite3.Connection, sql: str) -> bool:
    """Run an ``ALTER TABLE ... ADD COLUMN`` that may already have been applied.

    Returns True if the column was added, False if it already existed."""
    try:
        conn.execute(sql)
        return True
    except sqlite3.OperationalError as exc:
        if "duplicate column" not in str(exc).lower():
            raise
        return False


# GH-265: every store's reads and writes previously dispatched through
# asyncio.to_thread(), which runs on the event loop's implicit default
# executor (min(32, os.cpu_count() + 4) workers - as few as 5 on a
# single-CPU container). That pool is effectively the process-wide SQLite
# dispatch queue, so a burst of background work (a scan, the identification
# queue) could exhaust it and leave quick, latency-sensitive reads - the
# polled /library/activity and /home endpoints among them - queued for a
# free thread instead of actually running, surfacing as random multi-second
# "Slow request" warnings unrelated to the query itself. Dispatching through
# a dedicated pool sized independently of cpu_count keeps SQLite access off
# the loop's shared executor entirely, the same isolation
# library_management_planner.py's _SOURCE_INSPECTION_EXECUTOR already
# applies to its own blocking reads for the same reason.
_DB_EXECUTOR = ThreadPoolExecutor(max_workers=32, thread_name_prefix="persistence-db")


# Every store operation used to open a brand-new connection and close it again.
# A new connection has to read and parse the entire schema before its first
# statement, and the target schema (hundreds of tables, indexes and triggers)
# makes that about 20ms of CPU against 0.02ms for the query itself. Worse, the
# parse runs with the GIL released, so parallel requests contend on SQLite's
# internal mutexes: on a 2-core container a burst of 32 concurrent reads cost
# ~1.2s of CPU each, mostly system time, pinning the process for the whole
# burst. Reads - every authenticated request's session lookup, and nearly all
# of a page load - therefore reuse one connection per (thread, database).
# PlaylistRepository already uses thread-local connections for the same reason.
#
# Writes keep a fresh connection per operation: they are serialised by the
# write lock, so they never contend with each other, and bulk writes (scans,
# imports) keep the driver's full per-connection statement cache.
#
# Pooled connections are shared by every store on a thread, so everything a
# store can vary per connection is applied on checkout and everything an
# operation can leave behind is undone on check-in (see
# PersistenceBase._checkout_pooled_connection and _return_pooled_connection).
# The pool is bounded per thread so processes that touch many database files
# (the test suite) do not accumulate open files.
_POOLED_CONNECTIONS_PER_THREAD = 4


class _TrackedConnection(sqlite3.Connection):
    """A pooled connection that remembers the cursors it hands out.

    A cursor that outlives its operation part-way through a result set keeps
    its statement active, and with it a WAL read snapshot, so every later read
    on the connection would see stale data. Per-operation connections never
    had that problem because the next operation opened a new one; a pooled
    connection is therefore retired whenever one of its cursors is still alive
    after the operation returns.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self._handed_out: list[weakref.ref[sqlite3.Cursor]] = []

    def _track(self, cursor: sqlite3.Cursor) -> sqlite3.Cursor:
        handed_out = self._handed_out
        if len(handed_out) >= 256:
            handed_out[:] = [ref for ref in handed_out if ref() is not None]
        handed_out.append(weakref.ref(cursor))
        return cursor

    # Connection.execute* create their cursors in C without calling cursor(),
    # so each entry point is wrapped.
    def cursor(self, *args: Any, **kwargs: Any) -> sqlite3.Cursor:
        return self._track(super().cursor(*args, **kwargs))

    def execute(self, *args: Any) -> sqlite3.Cursor:
        return self._track(super().execute(*args))

    def executemany(self, *args: Any) -> sqlite3.Cursor:
        return self._track(super().executemany(*args))

    def executescript(self, *args: Any) -> sqlite3.Cursor:
        return self._track(super().executescript(*args))

    def forget_cursors(self) -> None:
        self._handed_out.clear()

    def has_live_cursors(self) -> bool:
        return any(ref() is not None for ref in self._handed_out)


@dataclass(eq=False)
class _PooledConnection:
    connection: _TrackedConnection
    # (st_dev, st_ino) of the file the connection opened; a replaced or
    # recreated database at the same path must not be served by a stale handle.
    file_identity: tuple[int, int]
    # Connection-local defaults captured at open: the busy timeout is restored
    # on every checkout, mmap_size on every check-in.
    default_busy_timeout_ms: int
    default_mmap_size: int
    # whether sqlite_stmt can report what the statement cache is holding
    measures_statement_memory: bool
    in_use: bool = False


class _ThreadConnections(threading.local):
    def __init__(self) -> None:
        self.by_path: OrderedDict[str, _PooledConnection] = OrderedDict()


_thread_connections = _ThreadConnections()

# A cached statement keeps its compiled program and SQLite's copy of its last
# bound values alive for as long as the connection lives, which close() used to
# bound: a large parameter, or many shapes of a long IN list, would otherwise stay
# resident on every executor thread. Pooled connections keep a statement cache,
# so bulk reads that repeat a statement do not re-prepare it, and are retired on
# check-in when SQLite reports that the cache holds more than the budget. The
# app's own statements measure 3KiB at the median and 45KiB at most, so the
# budget is only reached by large bound values. Where sqlite_stmt is not compiled
# in, pooled connections run without a statement cache instead.
_POOLED_STATEMENT_CACHE_SIZE = 64
_POOLED_STATEMENT_MEMORY_BUDGET_BYTES = 2 * 1024 * 1024
_statement_memory_measurable: bool | None = None


def _can_measure_statement_memory() -> bool:
    global _statement_memory_measurable
    if _statement_memory_measurable is None:
        probe = sqlite3.connect(":memory:")
        try:
            probe.execute("SELECT mem FROM sqlite_stmt LIMIT 1").fetchall()
            _statement_memory_measurable = True
        except sqlite3.Error:
            _statement_memory_measurable = False
        finally:
            probe.close()
    return _statement_memory_measurable


def _file_identity(path: str) -> tuple[int, int] | None:
    try:
        stat = os.stat(path)
    except OSError:
        return None
    return stat.st_dev, stat.st_ino


def _close_quietly(connection: sqlite3.Connection) -> None:
    try:
        connection.close()
    except Exception:  # noqa: BLE001 - discarding a connection must not mask the original error
        pass


class PersistenceBase:
    """Shared base for all domain-specific SQLite stores.

    All stores receive the *same* ``db_path`` and ``write_lock`` so they
    operate on a single database file with serialised writes.
    """

    # (GH-293) Telemetry role label for connection-settings reporting. Subclasses
    # that predate the shared base may pin their historical label (AuthStore).
    connection_label: str = "persistence_base"
    # (AUD-7) Explicit busy-handler timeout in ms applied at connect. None skips
    # the pragma, leaving Python's sqlite3.connect(timeout=5.0) driver default:
    # stores that historically never issued one override this so convergence
    # does not silently pin them to a future change of the base's value.
    busy_timeout_ms: int | None = 5000
    # Enforce foreign keys (and so ON DELETE CASCADE) on this store's
    # connections. Declared rather than added in a _connect override so pooled
    # connections, which are shared between stores, can apply it per operation.
    foreign_keys: bool = False

    def __init__(
        self, db_path: Path, write_lock: threading.Lock | PriorityWriteLock
    ) -> None:
        self.db_path = Path(db_path)
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self._write_lock = write_lock
        with self._write_lock:
            self._ensure_tables()

    def _open_connection(
        self,
        factory: type[sqlite3.Connection] = sqlite3.Connection,
        cached_statements: int = 128,
    ) -> sqlite3.Connection:
        """Open a connection with the settings every store shares."""
        conn = sqlite3.connect(
            self.db_path,
            check_same_thread=False,
            factory=factory,
            cached_statements=cached_statements,
        )
        conn.row_factory = sqlite3.Row
        # accent/case-insensitive LIKE searches (see _fold_text)
        conn.create_function("fold", 1, _fold_text, deterministic=True)
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA synchronous=NORMAL")
        return conn

    def _connect(self) -> sqlite3.Connection:
        conn = self._open_connection()
        # (AUD-7) Uniform backstop: a writer blocked by another writer waits up to
        # 5s for the lock instead of failing immediately with "database is locked".
        # Stores that historically never set one pin busy_timeout_ms = None above.
        if self.busy_timeout_ms is not None:
            conn.execute(f"PRAGMA busy_timeout={self.busy_timeout_ms}")
        # (GH-293) Labeled connection-local settings telemetry (bounded, once per
        # role per process). Never inferred from a fresh probe connection.
        report_connection_settings(self.connection_label, conn)
        if self.foreign_keys:
            conn.execute("PRAGMA foreign_keys=ON")
        return conn

    def _uses_pooled_connections(self) -> bool:
        # A store (or a test) that customises _connect keeps a fresh connection
        # per operation, so whatever the override installs is never bypassed.
        return (
            "_connect" not in vars(self)
            and type(self)._connect is PersistenceBase._connect
        )

    def _checkout_pooled_connection(self) -> _PooledConnection | None:
        """Borrow this thread's connection to ``db_path``, or None to open a fresh one."""
        path = str(self.db_path)
        identity = _file_identity(path)
        if identity is None:
            # Missing database: the fresh path creates it exactly as before.
            return None
        pool = _thread_connections.by_path
        pooled = pool.get(path)
        if pooled is not None and pooled.in_use:
            # Re-entrant use on this thread (an operation calling into another
            # store). Sharing the connection would let the inner operation
            # commit or roll back the outer one's transaction.
            return None
        if pooled is not None and pooled.file_identity != identity:
            del pool[path]
            _close_quietly(pooled.connection)
            pooled = None
        if pooled is None:
            measures_statement_memory = _can_measure_statement_memory()
            conn = cast(
                _TrackedConnection,
                self._open_connection(
                    factory=_TrackedConnection,
                    cached_statements=(
                        _POOLED_STATEMENT_CACHE_SIZE if measures_statement_memory else 0
                    ),
                ),
            )
            try:
                pooled = _PooledConnection(
                    connection=conn,
                    file_identity=identity,
                    default_busy_timeout_ms=int(
                        conn.execute("PRAGMA busy_timeout").fetchone()[0]
                    ),
                    default_mmap_size=int(
                        conn.execute("PRAGMA mmap_size").fetchone()[0]
                    ),
                    measures_statement_memory=measures_statement_memory,
                )
            except BaseException:
                _close_quietly(conn)
                raise
            pool[path] = pooled
            self._evict_idle_pooled_connections(pool)
        else:
            pool.move_to_end(path)
        pooled.in_use = True
        try:
            self._apply_store_settings(pooled)
        except Exception:  # noqa: BLE001 - an unusable cached connection falls back to a fresh one
            self._discard_pooled_connection(pooled)
            return None
        pooled.connection.forget_cursors()
        return pooled

    @staticmethod
    def _evict_idle_pooled_connections(
        pool: OrderedDict[str, _PooledConnection],
    ) -> None:
        for path in list(pool):
            if len(pool) <= _POOLED_CONNECTIONS_PER_THREAD:
                return
            if not pool[path].in_use:
                _close_quietly(pool.pop(path).connection)

    def _apply_store_settings(self, pooled: _PooledConnection) -> None:
        """Give a shared connection the settings this store's ``_connect`` applies.

        The busy timeout (or the driver default for stores that never set one)
        and foreign-key enforcement differ between stores.
        """
        conn = pooled.connection
        busy_timeout_ms = (
            self.busy_timeout_ms
            if self.busy_timeout_ms is not None
            else pooled.default_busy_timeout_ms
        )
        # Only assign what differs: assigning foreign_keys, even to its current
        # value, expires every prepared statement in the connection's cache.
        if conn.execute("PRAGMA busy_timeout").fetchone()[0] != busy_timeout_ms:
            conn.execute(f"PRAGMA busy_timeout={busy_timeout_ms}")
        if bool(conn.execute("PRAGMA foreign_keys").fetchone()[0]) != self.foreign_keys:
            conn.execute(f"PRAGMA foreign_keys={'ON' if self.foreign_keys else 'OFF'}")
        report_connection_settings(self.connection_label, conn)

    def _return_pooled_connection(self, pooled: _PooledConnection) -> None:
        """Undo what an operation can leave on a connection, or retire it.

        Closing a fresh connection used to do all of this implicitly.
        """
        conn = pooled.connection
        if conn.has_live_cursors():
            # A cursor escaped the operation and may pin a read snapshot.
            self._discard_pooled_connection(pooled)
            return
        try:
            if conn.in_transaction:
                conn.rollback()
            # The catalog integrity check raises mmap_size for its own scan.
            conn.execute(f"PRAGMA mmap_size={pooled.default_mmap_size}")
            # Release cached pages so idle connections across the whole
            # executor hold little more than their parsed schema, which is
            # what reuse is for.
            conn.execute("PRAGMA shrink_memory")
            if pooled.measures_statement_memory:
                statement_bytes = conn.execute(
                    "SELECT coalesce(sum(mem), 0) FROM sqlite_stmt"
                ).fetchone()[0]
                if statement_bytes > _POOLED_STATEMENT_MEMORY_BUDGET_BYTES:
                    self._discard_pooled_connection(pooled)
                    return
        except Exception:  # noqa: BLE001 - a connection that cannot be reset is not reused
            self._discard_pooled_connection(pooled)
            return
        pooled.in_use = False

    @staticmethod
    def _discard_pooled_connection(pooled: _PooledConnection) -> None:
        pool = _thread_connections.by_path
        for path, candidate in list(pool.items()):
            if candidate is pooled:
                del pool[path]
        _close_quietly(pooled.connection)

    def _run_operation(self, operation: Any, commit: bool) -> Any:
        pooled = (
            self._checkout_pooled_connection()
            if not commit and self._uses_pooled_connections()
            else None
        )
        if pooled is None:
            conn = self._connect()
            try:
                result = operation(conn)
                if commit:
                    conn.commit()
                return result
            finally:
                conn.close()

        try:
            result = operation(pooled.connection)
        except BaseException:
            # Same outcome as closing a fresh connection: the open transaction
            # is discarded, and nothing half-finished is handed to the next
            # operation on this thread.
            self._discard_pooled_connection(pooled)
            raise
        self._return_pooled_connection(pooled)
        return result

    def _execute(self, operation: Any, write: bool) -> Any:
        if write:
            with self._write_lock:
                return self._run_operation(operation, commit=True)
        return self._run_operation(operation, commit=False)

    async def _read(self, operation: Any) -> Any:
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(_DB_EXECUTOR, self._execute, operation, False)

    async def _write(self, operation: Any) -> Any:
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(_DB_EXECUTOR, self._execute, operation, True)

    def _execute_background(self, operation: Any) -> Any:
        background = getattr(self._write_lock, "background", None)
        lock_context = background() if background is not None else self._write_lock
        with lock_context:
            return self._run_operation(operation, commit=True)

    async def _background_write(self, operation: Any) -> Any:
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(_DB_EXECUTOR, self._execute_background, operation)

    def _ensure_tables(self) -> None:
        raise NotImplementedError
