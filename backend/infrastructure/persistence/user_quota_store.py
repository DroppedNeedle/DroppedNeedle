"""Per-user quota overrides (CollectionManagement Feature C, D8/D9).

One row per user who has an override; a NULL column inherits the global default
from ``DownloadPolicySettings`` (``default_request_quota_count`` / ``_days`` /
``default_storage_quota_gb``). Admin-managed only - users can't edit their own.
"""

import sqlite3

from infrastructure.msgspec_fastapi import AppStruct

from ._database import PersistenceBase


class UserQuota(AppStruct):
    """A user's quota override row. ``None`` = inherit the global default."""

    user_id: str
    request_quota_count: int | None = None
    request_quota_days: int | None = None
    storage_quota_gb: int | None = None


class UserQuotaStore(PersistenceBase):
    def _connect(self) -> sqlite3.Connection:
        # enforce user_id -> auth_users(id): rejects quota rows for nonexistent
        # users and lets the ON DELETE CASCADE fire from this connection too
        conn = super()._connect()
        conn.execute("PRAGMA foreign_keys=ON")
        return conn

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS user_quotas (
                    user_id             TEXT PRIMARY KEY
                                        REFERENCES auth_users(id) ON DELETE CASCADE,
                    request_quota_count INTEGER,
                    request_quota_days  INTEGER,
                    storage_quota_gb    INTEGER
                )
                """
            )
            conn.commit()
        finally:
            conn.close()

    async def get(self, user_id: str) -> UserQuota | None:
        def operation(conn: sqlite3.Connection) -> UserQuota | None:
            row = conn.execute(
                "SELECT * FROM user_quotas WHERE user_id = ?", (user_id,)
            ).fetchone()
            return UserQuota(**dict(row)) if row is not None else None

        return await self._read(operation)

    async def list_all(self) -> dict[str, UserQuota]:
        def operation(conn: sqlite3.Connection) -> dict[str, UserQuota]:
            rows = conn.execute("SELECT * FROM user_quotas").fetchall()
            return {row["user_id"]: UserQuota(**dict(row)) for row in rows}

        return await self._read(operation)

    async def set(
        self,
        user_id: str,
        *,
        request_quota_count: int | None,
        request_quota_days: int | None,
        storage_quota_gb: int | None,
    ) -> None:
        """Upsert a user's overrides. All-NULL deletes the row (pure inherit)."""

        def operation(conn: sqlite3.Connection) -> None:
            if request_quota_count is None and request_quota_days is None \
                    and storage_quota_gb is None:
                conn.execute("DELETE FROM user_quotas WHERE user_id = ?", (user_id,))
                return
            conn.execute(
                """INSERT INTO user_quotas
                       (user_id, request_quota_count, request_quota_days, storage_quota_gb)
                   VALUES (?, ?, ?, ?)
                   ON CONFLICT(user_id) DO UPDATE SET
                       request_quota_count = excluded.request_quota_count,
                       request_quota_days = excluded.request_quota_days,
                       storage_quota_gb = excluded.storage_quota_gb""",
                (user_id, request_quota_count, request_quota_days, storage_quota_gb),
            )

        await self._write(operation)
