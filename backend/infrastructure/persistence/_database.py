"""Shared SQLite infrastructure for all persistence stores."""

import asyncio
from contextlib import contextmanager
import json
import sqlite3
import threading
import unicodedata
from pathlib import Path
from typing import Any, TypeVar

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


class PersistenceBase:
    """Shared base for all domain-specific SQLite stores.

    All stores receive the *same* ``db_path`` and ``write_lock`` so they
    operate on a single database file with serialised writes.
    """

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
        conn.execute("PRAGMA busy_timeout=5000")
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
        return await asyncio.to_thread(self._execute, operation, False)

    async def _write(self, operation: Any) -> Any:
        return await asyncio.to_thread(self._execute, operation, True)

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
        return await asyncio.to_thread(self._execute_background, operation)

    def _ensure_tables(self) -> None:
        raise NotImplementedError
