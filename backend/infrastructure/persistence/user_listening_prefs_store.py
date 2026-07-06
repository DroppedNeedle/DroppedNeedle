import asyncio
import logging
import sqlite3
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

import msgspec

logger = logging.getLogger(__name__)

_DEFAULT_SOURCE = "listenbrainz"
# now-playing presence visibility: 'full' (show track), 'track_hidden'
# (show listening + progress, redact the song), 'offline' (show nothing).
_DEFAULT_VISIBILITY = "full"


class UserListeningPrefsRecord(msgspec.Struct, frozen=True):
    """Per-user scrobble + discovery prefs.

    Not ``UserPreferences``: that name is the global config.json section
    (release-type filters). This is the per-user SQLite table.
    """

    user_id: str
    scrobble_to_lastfm: bool
    scrobble_to_listenbrainz: bool
    primary_music_source: str
    now_playing_visibility: str
    auto_request_personal_mix: bool
    updated_at: str


class PersonalMixApproval(msgspec.Struct, frozen=True):
    """Standing grant for personal-mix auto-request (one per user), mirroring
    ``follow_store.Approval``. The role override is applied in the service layer."""

    user_id: str
    state: str
    requested_at: float
    reviewed_by_id: str | None = None
    reviewed_by_name: str | None = None
    reviewed_at: float | None = None
    user_name: str | None = None


class UserListeningPrefsStore:
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
                CREATE TABLE IF NOT EXISTS user_listening_prefs (
                  user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
                  scrobble_to_lastfm INTEGER NOT NULL DEFAULT 0,
                  scrobble_to_listenbrainz INTEGER NOT NULL DEFAULT 0,
                  primary_music_source TEXT NOT NULL DEFAULT 'listenbrainz',
                  now_playing_visibility TEXT NOT NULL DEFAULT 'full',
                  updated_at TEXT NOT NULL
                )
                """
            )
            # additive, idempotent: DBs created before now-playing presence lack the column
            try:
                conn.execute(
                    "ALTER TABLE user_listening_prefs "
                    "ADD COLUMN now_playing_visibility TEXT NOT NULL DEFAULT 'full'"
                )
            except sqlite3.OperationalError:
                pass  # column already present
            # additive, idempotent: DBs created before the personal-mix feature lack the column
            try:
                conn.execute(
                    "ALTER TABLE user_listening_prefs "
                    "ADD COLUMN auto_request_personal_mix INTEGER NOT NULL DEFAULT 0"
                )
            except sqlite3.OperationalError:
                pass  # column already present
            # standing grants for personal-mix auto-request, one row per user,
            # mirroring auto_download_approvals (follow_store)
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS personal_mix_approvals (
                    user_id          TEXT NOT NULL PRIMARY KEY
                                     REFERENCES auth_users(id) ON DELETE CASCADE,
                    state            TEXT NOT NULL DEFAULT 'pending',
                    requested_at     REAL NOT NULL,
                    reviewed_by_id   TEXT,
                    reviewed_by_name TEXT,
                    reviewed_at      REAL
                )
                """
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_pma_pending "
                "ON personal_mix_approvals(state) WHERE state = 'pending'"
            )
            # backfill, re-run at every startup: non-admin users with intent on but no
            # approval row (pre-gate opt-ins, demoted admins) enter the pending queue.
            # Idempotent (NOT EXISTS); rejected/revoked rows can't be resurrected
            # because their row exists.
            try:
                conn.execute(
                    """
                    INSERT INTO personal_mix_approvals (user_id, state, requested_at)
                    SELECT ulp.user_id, 'pending', ?
                    FROM user_listening_prefs ulp
                    JOIN auth_users au ON au.id = ulp.user_id
                    WHERE ulp.auto_request_personal_mix = 1
                      AND au.role != 'admin'
                      AND NOT EXISTS (
                        SELECT 1 FROM personal_mix_approvals pma
                        WHERE pma.user_id = ulp.user_id
                      )
                    """,
                    (time.time(),),
                )
            except sqlite3.OperationalError:
                pass  # fresh DB without auth_users yet -> nothing to backfill
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

    async def get(self, user_id: str) -> UserListeningPrefsRecord:
        """User's prefs, or a defaults record when no row exists."""

        def operation(conn: sqlite3.Connection) -> sqlite3.Row | None:
            return conn.execute(
                "SELECT * FROM user_listening_prefs WHERE user_id = ?",
                (user_id,),
            ).fetchone()

        row = await self._read(operation)
        if row is None:
            return UserListeningPrefsRecord(
                user_id=user_id,
                scrobble_to_lastfm=False,
                scrobble_to_listenbrainz=False,
                primary_music_source=_DEFAULT_SOURCE,
                now_playing_visibility=_DEFAULT_VISIBILITY,
                auto_request_personal_mix=False,
                updated_at="",
            )
        return UserListeningPrefsRecord(
            user_id=row["user_id"],
            scrobble_to_lastfm=bool(row["scrobble_to_lastfm"]),
            scrobble_to_listenbrainz=bool(row["scrobble_to_listenbrainz"]),
            primary_music_source=row["primary_music_source"],
            now_playing_visibility=row["now_playing_visibility"],
            auto_request_personal_mix=bool(row["auto_request_personal_mix"]),
            updated_at=row["updated_at"],
        )

    async def upsert(
        self,
        user_id: str,
        *,
        scrobble_to_lastfm: bool | None = None,
        scrobble_to_listenbrainz: bool | None = None,
        primary_music_source: str | None = None,
        now_playing_visibility: str | None = None,
        auto_request_personal_mix: bool | None = None,
    ) -> None:
        """Partial upsert: only the provided fields change; others are preserved."""
        now = datetime.now(timezone.utc).isoformat()
        # on INSERT, unset fields take their table defaults
        ins_lastfm = int(scrobble_to_lastfm) if scrobble_to_lastfm is not None else 0
        ins_lb = int(scrobble_to_listenbrainz) if scrobble_to_listenbrainz is not None else 0
        ins_source = primary_music_source if primary_music_source is not None else _DEFAULT_SOURCE
        ins_visibility = (
            now_playing_visibility if now_playing_visibility is not None else _DEFAULT_VISIBILITY
        )
        ins_auto_request = (
            int(auto_request_personal_mix) if auto_request_personal_mix is not None else 0
        )
        # on UPDATE, NULL keeps the existing column value via COALESCE
        upd_lastfm = int(scrobble_to_lastfm) if scrobble_to_lastfm is not None else None
        upd_lb = int(scrobble_to_listenbrainz) if scrobble_to_listenbrainz is not None else None
        upd_source = primary_music_source
        upd_visibility = now_playing_visibility
        upd_auto_request = (
            int(auto_request_personal_mix) if auto_request_personal_mix is not None else None
        )

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO user_listening_prefs (
                    user_id, scrobble_to_lastfm, scrobble_to_listenbrainz,
                    primary_music_source, now_playing_visibility,
                    auto_request_personal_mix, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(user_id) DO UPDATE SET
                    scrobble_to_lastfm = COALESCE(?, scrobble_to_lastfm),
                    scrobble_to_listenbrainz = COALESCE(?, scrobble_to_listenbrainz),
                    primary_music_source = COALESCE(?, primary_music_source),
                    now_playing_visibility = COALESCE(?, now_playing_visibility),
                    auto_request_personal_mix = COALESCE(?, auto_request_personal_mix),
                    updated_at = excluded.updated_at
                """,
                (
                    user_id,
                    ins_lastfm,
                    ins_lb,
                    ins_source,
                    ins_visibility,
                    ins_auto_request,
                    now,
                    upd_lastfm,
                    upd_lb,
                    upd_source,
                    upd_visibility,
                    upd_auto_request,
                ),
            )

        await self._write(operation)

    async def upsert_approval(
        self,
        user_id: str,
        state: str,
        reviewer: tuple[str, str | None] | None = None,
    ) -> None:
        # requested_at is refreshed so a re-request surfaces fresh in the admin
        # queue; reviewer fields are cleared unless a reviewer is given.
        now = time.time()
        reviewed_by_id = reviewer[0] if reviewer else None
        reviewed_by_name = reviewer[1] if reviewer else None
        reviewed_at = now if reviewer else None

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO personal_mix_approvals (
                    user_id, state, requested_at,
                    reviewed_by_id, reviewed_by_name, reviewed_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(user_id) DO UPDATE SET
                    state = excluded.state,
                    requested_at = excluded.requested_at,
                    reviewed_by_id = excluded.reviewed_by_id,
                    reviewed_by_name = excluded.reviewed_by_name,
                    reviewed_at = excluded.reviewed_at
                """,
                (user_id, state, now, reviewed_by_id, reviewed_by_name, reviewed_at),
            )

        await self._write(operation)

    async def set_approval_state(
        self,
        user_id: str,
        state: str,
        reviewer: tuple[str, str | None],
    ) -> bool:
        # admin review transition; leaves requested_at untouched
        now = time.time()
        reviewed_by_id, reviewed_by_name = reviewer

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "UPDATE personal_mix_approvals SET state = ?, reviewed_by_id = ?, "
                "reviewed_by_name = ?, reviewed_at = ? WHERE user_id = ?",
                (state, reviewed_by_id, reviewed_by_name, now, user_id),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    async def get_approval_state(self, user_id: str) -> str | None:
        def operation(conn: sqlite3.Connection) -> sqlite3.Row | None:
            return conn.execute(
                "SELECT state FROM personal_mix_approvals WHERE user_id = ?",
                (user_id,),
            ).fetchone()

        row = await self._read(operation)
        return row["state"] if row else None

    async def list_pending_approvals(self) -> list[PersonalMixApproval]:
        def operation(conn: sqlite3.Connection) -> list[sqlite3.Row]:
            return conn.execute(
                """
                SELECT pma.user_id AS user_id, pma.state AS state,
                       pma.requested_at AS requested_at, au.display_name AS user_name
                FROM personal_mix_approvals pma
                JOIN auth_users au ON au.id = pma.user_id
                WHERE pma.state = 'pending'
                ORDER BY pma.requested_at ASC
                """
            ).fetchall()

        rows = await self._read(operation)
        return [
            PersonalMixApproval(
                user_id=row["user_id"],
                state=row["state"],
                requested_at=row["requested_at"],
                user_name=row["user_name"],
            )
            for row in rows
        ]
