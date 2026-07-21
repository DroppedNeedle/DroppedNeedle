import sqlite3
import threading
from pathlib import Path

from infrastructure.persistence._database import PersistenceBase


class DiscoverySnapshotStore(PersistenceBase):
    """Last known-good Discover responses, retained across process restarts."""

    def __init__(self, db_path: Path, write_lock: threading.Lock) -> None:
        super().__init__(db_path, write_lock)

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS discovery_snapshots (
                    snapshot_key TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    payload BLOB NOT NULL,
                    saved_at REAL NOT NULL,
                    stale INTEGER NOT NULL DEFAULT 0,
                    catalog_revision INTEGER NOT NULL DEFAULT 0
                )
                """
            )
            self._safe_alter(
                conn,
                "ALTER TABLE discovery_snapshots "
                "ADD COLUMN stale INTEGER NOT NULL DEFAULT 0",
            )
            self._safe_alter(
                conn,
                "ALTER TABLE discovery_snapshots "
                "ADD COLUMN catalog_revision INTEGER NOT NULL DEFAULT 0",
            )
            conn.execute(
                "CREATE TABLE IF NOT EXISTS library_catalog_revision ("
                "singleton INTEGER PRIMARY KEY CHECK(singleton = 1), "
                "value INTEGER NOT NULL DEFAULT 0)"
            )
            conn.execute(
                "INSERT OR IGNORE INTO library_catalog_revision(singleton, value) "
                "VALUES (1, 0)"
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_discovery_snapshots_user "
                "ON discovery_snapshots(user_id)"
            )
            conn.commit()
        finally:
            conn.close()

    @staticmethod
    def _safe_alter(conn: sqlite3.Connection, sql: str) -> None:
        try:
            conn.execute(sql)
        except sqlite3.OperationalError as exc:
            if "duplicate column name" not in str(exc).lower():
                raise

    async def get(self, snapshot_key: str) -> bytes | None:
        def operation(conn: sqlite3.Connection) -> bytes | None:
            row = conn.execute(
                "SELECT snapshot.payload FROM discovery_snapshots snapshot "
                "JOIN library_catalog_revision revision ON revision.singleton = 1 "
                "WHERE snapshot.snapshot_key = ? "
                "AND snapshot.catalog_revision = revision.value",
                (snapshot_key,),
            ).fetchone()
            return bytes(row["payload"]) if row is not None else None

        return await self._read(operation)

    async def get_with_stale(self, snapshot_key: str) -> tuple[bytes, bool] | None:
        def operation(conn: sqlite3.Connection) -> tuple[bytes, bool] | None:
            row = conn.execute(
                "SELECT snapshot.payload, snapshot.stale "
                "FROM discovery_snapshots snapshot "
                "JOIN library_catalog_revision revision ON revision.singleton = 1 "
                "WHERE snapshot.snapshot_key = ? "
                "AND snapshot.catalog_revision = revision.value",
                (snapshot_key,),
            ).fetchone()
            if row is None:
                return None
            return (bytes(row["payload"]), bool(row["stale"]))

        return await self._read(operation)

    async def save(
        self, snapshot_key: str, user_id: str, payload: bytes, saved_at: float
    ) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            catalog_revision = int(
                conn.execute(
                    "SELECT value FROM library_catalog_revision WHERE singleton = 1"
                ).fetchone()[0]
            )
            conn.execute(
                """
                INSERT INTO discovery_snapshots
                    (snapshot_key, user_id, payload, saved_at, stale, catalog_revision)
                VALUES (?, ?, ?, ?, 0, ?)
                ON CONFLICT(snapshot_key) DO UPDATE SET
                    user_id = excluded.user_id,
                    payload = excluded.payload,
                    saved_at = excluded.saved_at,
                    stale = 0,
                    catalog_revision = excluded.catalog_revision
                """,
                (snapshot_key, user_id, payload, saved_at, catalog_revision),
            )

        await self._write(operation)

    async def mark_discover_stale(self) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE discovery_snapshots SET stale = 1 "
                "WHERE snapshot_key LIKE 'discover_response:%' "
                "OR snapshot_key LIKE 'discover_queue:%'"
            )

        await self._write(operation)

    async def delete_user(self, user_id: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "DELETE FROM discovery_snapshots WHERE user_id = ?", (user_id,)
            )

        await self._write(operation)

    async def delete(self, snapshot_key: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "DELETE FROM discovery_snapshots WHERE snapshot_key = ?",
                (snapshot_key,),
            )

        await self._write(operation)
