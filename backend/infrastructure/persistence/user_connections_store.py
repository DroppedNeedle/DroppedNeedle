import asyncio
import json
import logging
import sqlite3
import threading
from datetime import datetime, timezone
from pathlib import Path

import msgspec

from infrastructure.crypto import decrypt, encrypt
from infrastructure.serialization import to_jsonable

logger = logging.getLogger(__name__)


class UserConnectionRecord(msgspec.Struct, frozen=True):
    """Non-secret view: only username + enabled flag; the encrypted
    ``connection_data`` ciphertext never leaves the store."""

    user_id: str
    service: str
    enabled: bool
    username: str
    created_at: str
    updated_at: str


class UserConnectionsStore:
    """Per-user external scrobble/discovery accounts (AMU-3).

    ``connection_data`` is Fernet-encrypted JSON: lastfm = {session_key, username};
    listenbrainz = {user_token, username}.
    """

    def __init__(self, db_path: Path, write_lock: threading.Lock | None = None):
        self.db_path = Path(db_path)
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self._write_lock = write_lock or threading.Lock()
        with self._write_lock:
            self._ensure_tables()

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.db_path, check_same_thread=False)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA synchronous=NORMAL")
        conn.execute("PRAGMA busy_timeout=5000")
        conn.execute("PRAGMA foreign_keys=ON")
        return conn

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS user_connections (
                  user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                  service TEXT NOT NULL,
                  connection_data TEXT NOT NULL,
                  enabled INTEGER NOT NULL DEFAULT 1,
                  created_at TEXT NOT NULL,
                  updated_at TEXT NOT NULL,
                  PRIMARY KEY (user_id, service)
                )
                """
            )
            conn.commit()
        finally:
            conn.close()

    def _execute(self, operation, write: bool):
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

    async def _read(self, operation):
        return await asyncio.to_thread(self._execute, operation, False)

    async def _write(self, operation):
        return await asyncio.to_thread(self._execute, operation, True)

    async def upsert(
        self,
        user_id: str,
        service: str,
        connection_data: dict,
        enabled: bool = True,
    ) -> None:
        now = datetime.now(timezone.utc).isoformat()
        ciphertext = encrypt(json.dumps(to_jsonable(connection_data)))

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO user_connections (
                    user_id, service, connection_data, enabled, created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(user_id, service) DO UPDATE SET
                    connection_data = excluded.connection_data,
                    enabled = excluded.enabled,
                    updated_at = excluded.updated_at
                """,
                (user_id, service, ciphertext, int(enabled), now, now),
            )

        await self._write(operation)

    async def get(self, user_id: str, service: str) -> dict | None:
        """Decrypted ``connection_data`` for an enabled connection, else ``None``
        (absent or disabled), matching the resolver's "no client" contract (B5)."""

        def operation(conn: sqlite3.Connection) -> sqlite3.Row | None:
            return conn.execute(
                "SELECT connection_data FROM user_connections "
                "WHERE user_id = ? AND service = ? AND enabled = 1",
                (user_id, service),
            ).fetchone()

        row = await self._read(operation)
        if row is None:
            return None
        plaintext, failed = decrypt(row["connection_data"])
        if failed:
            # rotated/lost key: no usable connection, upholds the resolver's None contract
            return None
        try:
            return json.loads(plaintext)
        except (json.JSONDecodeError, ValueError):
            return None

    async def list_for_user(self, user_id: str) -> list[UserConnectionRecord]:
        def operation(conn: sqlite3.Connection) -> list[sqlite3.Row]:
            # enabled-only, matching get(), so a disabled row never shows as "linked"
            return conn.execute(
                "SELECT * FROM user_connections WHERE user_id = ? AND enabled = 1 ORDER BY service ASC",
                (user_id,),
            ).fetchall()

        rows = await self._read(operation)
        records: list[UserConnectionRecord] = []
        for row in rows:
            plaintext, _ = decrypt(row["connection_data"])
            try:
                data = json.loads(plaintext)
            except (json.JSONDecodeError, ValueError):
                data = {}
            records.append(
                UserConnectionRecord(
                    user_id=row["user_id"],
                    service=row["service"],
                    enabled=bool(row["enabled"]),
                    username=str(data.get("username", "")),
                    created_at=row["created_at"],
                    updated_at=row["updated_at"],
                )
            )
        return records

    async def get_service_token(self, service: str, token_field: str = "user_token") -> str | None:
        """Any stored auth token for a service, from the earliest enabled connection
        (typically the admin/owner). Used to authenticate app-wide PUBLIC-data lookups
        (e.g. ListenBrainz popularity) that run outside a user context - never writes."""

        def operation(conn: sqlite3.Connection) -> list[sqlite3.Row]:
            return conn.execute(
                "SELECT connection_data FROM user_connections "
                "WHERE service = ? AND enabled = 1 ORDER BY created_at ASC",
                (service,),
            ).fetchall()

        rows = await self._read(operation)
        for row in rows:
            plaintext, failed = decrypt(row["connection_data"])
            if failed:
                continue
            try:
                data = json.loads(plaintext)
            except (json.JSONDecodeError, ValueError):
                continue
            token = data.get(token_field)
            if token:
                return str(token)
        return None

    async def list_user_ids_for_service(self, service: str) -> list[str]:
        def operation(conn: sqlite3.Connection) -> list[sqlite3.Row]:
            return conn.execute(
                "SELECT user_id FROM user_connections WHERE service = ? AND enabled = 1",
                (service,),
            ).fetchall()

        rows = await self._read(operation)
        return [row["user_id"] for row in rows]

    async def set_enabled(self, user_id: str, service: str, enabled: bool) -> None:
        now = datetime.now(timezone.utc).isoformat()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE user_connections SET enabled = ?, updated_at = ? "
                "WHERE user_id = ? AND service = ?",
                (int(enabled), now, user_id, service),
            )

        await self._write(operation)

    async def delete(self, user_id: str, service: str) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "DELETE FROM user_connections WHERE user_id = ? AND service = ?",
                (user_id, service),
            )
            return cursor.rowcount > 0

        return await self._write(operation)
