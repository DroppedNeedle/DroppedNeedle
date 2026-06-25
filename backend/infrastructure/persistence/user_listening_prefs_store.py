import asyncio
import logging
import sqlite3
import threading
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
    updated_at: str


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
                updated_at="",
            )
        return UserListeningPrefsRecord(
            user_id=row["user_id"],
            scrobble_to_lastfm=bool(row["scrobble_to_lastfm"]),
            scrobble_to_listenbrainz=bool(row["scrobble_to_listenbrainz"]),
            primary_music_source=row["primary_music_source"],
            now_playing_visibility=row["now_playing_visibility"],
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
        # on UPDATE, NULL keeps the existing column value via COALESCE
        upd_lastfm = int(scrobble_to_lastfm) if scrobble_to_lastfm is not None else None
        upd_lb = int(scrobble_to_listenbrainz) if scrobble_to_listenbrainz is not None else None
        upd_source = primary_music_source
        upd_visibility = now_playing_visibility

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO user_listening_prefs (
                    user_id, scrobble_to_lastfm, scrobble_to_listenbrainz,
                    primary_music_source, now_playing_visibility, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(user_id) DO UPDATE SET
                    scrobble_to_lastfm = COALESCE(?, scrobble_to_lastfm),
                    scrobble_to_listenbrainz = COALESCE(?, scrobble_to_listenbrainz),
                    primary_music_source = COALESCE(?, primary_music_source),
                    now_playing_visibility = COALESCE(?, now_playing_visibility),
                    updated_at = excluded.updated_at
                """,
                (
                    user_id,
                    ins_lastfm,
                    ins_lb,
                    ins_source,
                    ins_visibility,
                    now,
                    upd_lastfm,
                    upd_lb,
                    upd_source,
                    upd_visibility,
                ),
            )

        await self._write(operation)
