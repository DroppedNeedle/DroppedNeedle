"""Shared SQLite infrastructure for all persistence stores."""

import asyncio
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import json
import sqlite3
import threading
import unicodedata
from pathlib import Path
from typing import Any, TypeVar

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

    def __init__(
        self, db_path: Path, write_lock: threading.Lock | PriorityWriteLock
    ) -> None:
        self.db_path = Path(db_path)
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self._write_lock = write_lock
        with self._write_lock:
            self._ensure_tables()

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.db_path, check_same_thread=False)
        conn.row_factory = sqlite3.Row
        # accent/case-insensitive LIKE searches (see _fold_text)
        conn.create_function("fold", 1, _fold_text, deterministic=True)
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA synchronous=NORMAL")
        # (AUD-7) Uniform backstop: a writer blocked by another writer waits up to
        # 5s for the lock instead of failing immediately with "database is locked".
        # Stores that historically never set one pin busy_timeout_ms = None above.
        if self.busy_timeout_ms is not None:
            conn.execute(f"PRAGMA busy_timeout={self.busy_timeout_ms}")
        # (GH-293) Labeled connection-local settings telemetry (bounded, once per
        # role per process). Never inferred from a fresh probe connection.
        report_connection_settings(self.connection_label, conn)
        return conn

    def _execute(self, operation: Any, write: bool) -> Any:
        if write:
            with self._write_lock:
                conn = self._connect()
                try:
                    result = operation(conn)
                    conn.commit()
                    return result
                finally:
                    conn.close()

        conn = self._connect()
        try:
            return operation(conn)
        finally:
            conn.close()

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
            conn = self._connect()
            try:
                result = operation(conn)
                conn.commit()
                return result
            finally:
                conn.close()

    async def _background_write(self, operation: Any) -> Any:
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(_DB_EXECUTOR, self._execute_background, operation)

    def _ensure_tables(self) -> None:
        raise NotImplementedError
