"""Favorites persistence store: user_favorites table.

Unified on UserRecord.id regardless of which protocol wrote the row. Lives in the
shared WAL db (FK to auth_users).
"""

from __future__ import annotations

import sqlite3
import threading
from pathlib import Path

from infrastructure.persistence._database import PersistenceBase


class FavoritesStore(PersistenceBase):
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
                CREATE TABLE IF NOT EXISTS user_favorites (
                    user_id    TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                    item_kind  TEXT NOT NULL,
                    item_id    TEXT NOT NULL,
                    created_at REAL NOT NULL,
                    PRIMARY KEY (user_id, item_kind, item_id)
                );
                CREATE INDEX IF NOT EXISTS idx_fav_user_kind
                    ON user_favorites(user_id, item_kind);
            """)
            conn.commit()
        finally:
            conn.close()

    async def add(
        self, user_id: str, item_kind: str, item_id: str, created_at: float
    ) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            # re-starring keeps the original created_at
            conn.execute(
                "INSERT INTO user_favorites (user_id, item_kind, item_id, created_at) "
                "VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
                (user_id, item_kind, item_id, created_at),
            )

        await self._write(operation)

    async def apply_many(
        self,
        user_id: str,
        targets: list[tuple[str, str]],
        *,
        add: bool,
        created_at: float,
    ) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            if add:
                conn.executemany(
                    "INSERT INTO user_favorites "
                    "(user_id, item_kind, item_id, created_at) VALUES (?, ?, ?, ?) "
                    "ON CONFLICT DO NOTHING",
                    [
                        (user_id, kind, item_id, created_at)
                        for kind, item_id in targets
                    ],
                )
            else:
                conn.executemany(
                    "DELETE FROM user_favorites "
                    "WHERE user_id = ? AND item_kind = ? AND item_id = ?",
                    [(user_id, kind, item_id) for kind, item_id in targets],
                )

        await self._write(operation)

    async def remove(self, user_id: str, item_kind: str, item_id: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "DELETE FROM user_favorites "
                "WHERE user_id = ? AND item_kind = ? AND item_id = ?",
                (user_id, item_kind, item_id),
            )

        await self._write(operation)

    async def is_favorite(self, user_id: str, item_kind: str, item_id: str) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            row = conn.execute(
                "SELECT 1 FROM user_favorites "
                "WHERE user_id = ? AND item_kind = ? AND item_id = ? LIMIT 1",
                (user_id, item_kind, item_id),
            ).fetchone()
            return row is not None

        return await self._read(operation)

    async def list(self, user_id: str, item_kind: str) -> list[tuple[str, float]]:
        def operation(conn: sqlite3.Connection) -> list[tuple[str, float]]:
            rows = conn.execute(
                "SELECT item_id, created_at FROM user_favorites "
                "WHERE user_id = ? AND item_kind = ? ORDER BY created_at DESC",
                (user_id, item_kind),
            ).fetchall()
            return [(r["item_id"], float(r["created_at"])) for r in rows]

        return await self._read(operation)

    async def map_for_items(
        self, user_id: str, item_kind: str, item_ids: list[str]
    ) -> dict[str, float]:
        if not item_ids:
            return {}

        def operation(conn: sqlite3.Connection) -> dict[str, float]:
            placeholders = ", ".join("?" for _ in item_ids)
            rows = conn.execute(
                f"SELECT item_id, created_at FROM user_favorites "
                f"WHERE user_id = ? AND item_kind = ? AND item_id IN ({placeholders})",
                (user_id, item_kind, *item_ids),
            ).fetchall()
            return {r["item_id"]: float(r["created_at"]) for r in rows}

        return await self._read(operation)
