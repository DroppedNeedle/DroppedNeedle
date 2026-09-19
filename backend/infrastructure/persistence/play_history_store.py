import logging
import sqlite3
import threading
from pathlib import Path
from uuid import uuid4

import msgspec

from infrastructure.persistence._database import PersistenceBase

logger = logging.getLogger(__name__)


class PlayHistoryRecord(msgspec.Struct, frozen=True):
    id: str
    user_id: str
    track_name: str
    artist_name: str
    played_at: str
    album_name: str | None = None
    recording_mbid: str | None = None
    release_group_mbid: str | None = None
    duration_ms: int | None = None
    source: str | None = None


class PlayHistoryStore(PersistenceBase):
    """Append-only per-user listening history; written for every play regardless
    of external linkage (D6)."""

    def __init__(self, db_path: Path, write_lock: threading.Lock | None = None):
        super().__init__(db_path, write_lock or threading.Lock())

    foreign_keys = True

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS play_history (
                  id TEXT PRIMARY KEY,
                  user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                  track_name TEXT NOT NULL, artist_name TEXT NOT NULL, album_name TEXT,
                  recording_mbid TEXT, release_group_mbid TEXT, duration_ms INTEGER,
                  source TEXT,
                  played_at TEXT NOT NULL
                )
                """
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_play_history_user_played "
                "ON play_history(user_id, played_at DESC)"
            )
            conn.commit()
        finally:
            conn.close()

    @staticmethod
    def _row_to_record(row: sqlite3.Row) -> PlayHistoryRecord:
        return PlayHistoryRecord(
            id=row["id"],
            user_id=row["user_id"],
            track_name=row["track_name"],
            artist_name=row["artist_name"],
            played_at=row["played_at"],
            album_name=row["album_name"],
            recording_mbid=row["recording_mbid"],
            release_group_mbid=row["release_group_mbid"],
            duration_ms=row["duration_ms"],
            source=row["source"],
        )

    async def insert(
        self,
        user_id: str,
        *,
        track_name: str,
        artist_name: str,
        played_at: str,
        album_name: str | None = None,
        recording_mbid: str | None = None,
        release_group_mbid: str | None = None,
        duration_ms: int | None = None,
        source: str | None = None,
    ) -> str:
        row_id = uuid4().hex

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO play_history (
                    id, user_id, track_name, artist_name, album_name,
                    recording_mbid, release_group_mbid, duration_ms, source, played_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    row_id,
                    user_id,
                    track_name,
                    artist_name,
                    album_name,
                    recording_mbid,
                    release_group_mbid,
                    duration_ms,
                    source,
                    played_at,
                ),
            )

        await self._write(operation)
        return row_id

    async def recent(self, user_id: str, limit: int = 50) -> list[PlayHistoryRecord]:
        safe_limit = max(1, limit)

        def operation(conn: sqlite3.Connection) -> list[sqlite3.Row]:
            return conn.execute(
                "SELECT * FROM play_history WHERE user_id = ? "
                "ORDER BY played_at DESC LIMIT ?",
                (user_id, safe_limit),
            ).fetchall()

        rows = await self._read(operation)
        return [self._row_to_record(row) for row in rows]

    async def play_counts_by_artist(
        self, user_id: str, artist_name: str
    ) -> dict[str, int]:
        """Play counts for a user's tracks by an artist, keyed both by
        ``rec:<recording_mbid>`` and ``name:<lower track_name>`` so the discovery
        service can match owned library files (06 s11.5)."""

        def operation(conn: sqlite3.Connection) -> dict[str, int]:
            rows = conn.execute(
                "SELECT recording_mbid, track_name, COUNT(*) AS plays "
                "FROM play_history WHERE user_id = ? AND artist_name = ? "
                "GROUP BY recording_mbid, track_name",
                (user_id, artist_name),
            ).fetchall()
            counts: dict[str, int] = {}
            for r in rows:
                plays = int(r["plays"])
                if r["recording_mbid"]:
                    key = f"rec:{r['recording_mbid']}"
                    counts[key] = counts.get(key, 0) + plays
                name_key = f"name:{(r['track_name'] or '').lower()}"
                counts[name_key] = counts.get(name_key, 0) + plays
            return counts

        return await self._read(operation)

    async def album_ids(
        self,
        user_id: str,
        *,
        frequent: bool,
        limit: int,
        offset: int,
    ) -> list[str]:
        order = (
            "COUNT(*) DESC, MAX(played_at) DESC, release_group_mbid"
            if frequent
            else "MAX(played_at) DESC, release_group_mbid"
        )

        def operation(conn: sqlite3.Connection) -> list[str]:
            rows = conn.execute(
                "SELECT release_group_mbid FROM play_history "
                "WHERE user_id = ? AND release_group_mbid IS NOT NULL "
                "GROUP BY release_group_mbid "
                f"ORDER BY {order} LIMIT ? OFFSET ?",
                (user_id, max(limit, 1), max(offset, 0)),
            ).fetchall()
            return [str(row["release_group_mbid"]) for row in rows]

        return await self._read(operation)

    async def compat_stats(
        self,
        user_id: str,
        *,
        recording_mbids: list[str],
        release_group_mbids: list[str],
        artist_names: list[str],
    ) -> dict[str, dict[str, tuple[int, str]]]:
        """Batch play counts/last-played overlays for compat response pages."""
        recordings = list(dict.fromkeys(item for item in recording_mbids if item))
        albums = list(dict.fromkeys(item for item in release_group_mbids if item))
        artists = list(dict.fromkeys(item for item in artist_names if item))

        def grouped(
            conn: sqlite3.Connection, column: str, values: list[str]
        ) -> dict[str, tuple[int, str]]:
            if not values:
                return {}
            placeholders = ", ".join("?" for _ in values)
            rows = conn.execute(
                f"SELECT {column} AS item_key, COUNT(*) AS plays, "
                f"MAX(played_at) AS played_at FROM play_history "
                f"WHERE user_id = ? AND {column} IN ({placeholders}) "
                f"GROUP BY {column}",
                (user_id, *values),
            ).fetchall()
            return {
                str(row["item_key"]): (int(row["plays"]), str(row["played_at"]))
                for row in rows
            }

        def operation(conn: sqlite3.Connection):
            return {
                "track": grouped(conn, "recording_mbid", recordings),
                "album": grouped(conn, "release_group_mbid", albums),
                "artist": grouped(conn, "artist_name", artists),
            }

        return await self._read(operation)
