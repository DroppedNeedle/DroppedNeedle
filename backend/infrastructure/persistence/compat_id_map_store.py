"""Jellyfin id-map persistence: compat_id_map table.

Maps stable 32-hex Jellyfin GUIDs <-> (kind, internal_id). Bijective so
``/Items/{id}`` (which carries no type) resolves. Lives in the shared WAL db.
"""

from __future__ import annotations

import sqlite3
import threading
from pathlib import Path

from infrastructure.persistence._database import PersistenceBase


class CompatIdMapStore(PersistenceBase):
    # never issued an explicit busy_timeout pragma historically (Python's driver
    # default of 5s applied); None keeps that instead of pinning the base's value.
    busy_timeout_ms: int | None = None

    def __init__(self, db_path: Path, write_lock: threading.Lock | None = None) -> None:
        super().__init__(db_path, write_lock or threading.Lock())

    foreign_keys = True

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.executescript("""
                CREATE TABLE IF NOT EXISTS compat_id_map (
                    jf_id       TEXT PRIMARY KEY,
                    kind        TEXT NOT NULL,
                    internal_id TEXT NOT NULL,
                    UNIQUE (kind, internal_id)
                );
            """)
            conn.commit()
        finally:
            conn.close()

    async def get_jf_id(self, kind: str, internal_id: str) -> str | None:
        def operation(conn: sqlite3.Connection) -> str | None:
            row = conn.execute(
                "SELECT jf_id FROM compat_id_map WHERE kind = ? AND internal_id = ?",
                (kind, internal_id),
            ).fetchone()
            return row["jf_id"] if row else None

        return await self._read(operation)

    async def get_mapping(self, jf_id: str) -> tuple[str, str] | None:
        def operation(conn: sqlite3.Connection) -> tuple[str, str] | None:
            row = conn.execute(
                "SELECT kind, internal_id FROM compat_id_map WHERE jf_id = ?",
                (jf_id,),
            ).fetchone()
            return (row["kind"], row["internal_id"]) if row else None

        return await self._read(operation)

    async def insert(self, jf_id: str, kind: str, internal_id: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            # deterministic derivation means an existing row is identical; ignore
            # conflicts on either the jf_id PK or the (kind, internal_id) UNIQUE
            conn.execute(
                "INSERT INTO compat_id_map (jf_id, kind, internal_id) "
                "VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
                (jf_id, kind, internal_id),
            )

        await self._write(operation)
