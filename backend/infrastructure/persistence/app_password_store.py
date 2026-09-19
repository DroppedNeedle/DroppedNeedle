"""App-password persistence store: connect_app_passwords table.

Lives in the shared WAL database (same ``library_db_path`` as AuthStore) so the
``auth_users`` FK resolves. Raw row I/O only; crypto/hashing/verification live in
``services/compat/app_password_service.py``.
"""

from __future__ import annotations

import sqlite3
import threading
from pathlib import Path

import msgspec

from infrastructure.persistence._database import PersistenceBase


class AppPasswordRow(msgspec.Struct, frozen=True):
    """One connect_app_passwords row (includes both secret columns)."""

    id: str
    user_id: str
    name: str
    secret_sha256: str
    secret_encrypted: str
    created_at: str
    last_used_at: str | None = None
    last_client: str | None = None
    revoked: bool = False


class AppPasswordStore(PersistenceBase):
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
                CREATE TABLE IF NOT EXISTS connect_app_passwords (
                    id               TEXT PRIMARY KEY,
                    user_id          TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                    name             TEXT NOT NULL,
                    secret_sha256    TEXT NOT NULL UNIQUE,
                    secret_encrypted TEXT NOT NULL,
                    created_at       TEXT NOT NULL,
                    last_used_at     TEXT,
                    last_client      TEXT,
                    revoked          INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_cap_user
                    ON connect_app_passwords(user_id);
            """)
            conn.commit()
        finally:
            conn.close()

    @staticmethod
    def _to_row(row: sqlite3.Row | None) -> AppPasswordRow | None:
        if row is None:
            return None
        return AppPasswordRow(
            id=row["id"],
            user_id=row["user_id"],
            name=row["name"],
            secret_sha256=row["secret_sha256"],
            secret_encrypted=row["secret_encrypted"],
            created_at=row["created_at"],
            last_used_at=row["last_used_at"],
            last_client=row["last_client"],
            revoked=bool(row["revoked"]),
        )

    async def insert(self, row: AppPasswordRow) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "INSERT INTO connect_app_passwords "
                "(id, user_id, name, secret_sha256, secret_encrypted, created_at, "
                "last_used_at, last_client, revoked) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    row.id, row.user_id, row.name, row.secret_sha256,
                    row.secret_encrypted, row.created_at, row.last_used_at,
                    row.last_client, 1 if row.revoked else 0,
                ),
            )

        await self._write(operation)

    async def get_by_id(self, app_password_id: str) -> AppPasswordRow | None:
        def operation(conn: sqlite3.Connection) -> AppPasswordRow | None:
            return self._to_row(
                conn.execute(
                    "SELECT * FROM connect_app_passwords WHERE id = ?",
                    (app_password_id,),
                ).fetchone()
            )

        return await self._read(operation)

    async def get_active_by_sha256(self, secret_sha256: str) -> AppPasswordRow | None:
        def operation(conn: sqlite3.Connection) -> AppPasswordRow | None:
            return self._to_row(
                conn.execute(
                    "SELECT * FROM connect_app_passwords "
                    "WHERE secret_sha256 = ? AND revoked = 0",
                    (secret_sha256,),
                ).fetchone()
            )

        return await self._read(operation)

    async def list_active_by_user(self, user_id: str) -> list[AppPasswordRow]:
        def operation(conn: sqlite3.Connection) -> list[AppPasswordRow]:
            rows = conn.execute(
                "SELECT * FROM connect_app_passwords "
                "WHERE user_id = ? AND revoked = 0 ORDER BY created_at ASC",
                (user_id,),
            ).fetchall()
            return [r for r in (self._to_row(row) for row in rows) if r is not None]

        return await self._read(operation)

    async def count_active_by_user(self, user_id: str) -> int:
        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                "SELECT COUNT(*) AS n FROM connect_app_passwords "
                "WHERE user_id = ? AND revoked = 0",
                (user_id,),
            ).fetchone()
            return int(row["n"]) if row else 0

        return await self._read(operation)

    async def list_all_active(self) -> list[AppPasswordRow]:
        """Every active app-password across all users (admin oversight)."""
        def operation(conn: sqlite3.Connection) -> list[AppPasswordRow]:
            rows = conn.execute(
                "SELECT * FROM connect_app_passwords "
                "WHERE revoked = 0 ORDER BY user_id ASC, created_at ASC"
            ).fetchall()
            return [r for r in (self._to_row(row) for row in rows) if r is not None]

        return await self._read(operation)

    async def revoke(self, app_password_id: str) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            cur = conn.execute(
                "UPDATE connect_app_passwords SET revoked = 1 WHERE id = ?",
                (app_password_id,),
            )
            return cur.rowcount > 0

        return await self._write(operation)

    async def touch(
        self, secret_sha256: str, *, last_used_at: str, last_client: str | None
    ) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE connect_app_passwords "
                "SET last_used_at = ?, last_client = ? WHERE secret_sha256 = ?",
                (last_used_at, last_client, secret_sha256),
            )

        await self._write(operation)
