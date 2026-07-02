"""Per-album edition pins (CollectionManagement Feature E, D16).

A pin names the MusicBrainz release (edition) an album should display and
acquire, overriding the mode-over-files inference. Library-wide (one pin per
release group, not per user), admin/trusted-set, and durable across rescans -
which is why it lives in its own table rather than on ``library_albums``.
"""

import sqlite3
from datetime import datetime, timezone

from ._database import PersistenceBase


class AlbumReleasePinStore(PersistenceBase):
    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS album_release_pins (
                    release_group_mbid TEXT PRIMARY KEY,
                    release_mbid       TEXT NOT NULL,
                    set_by_user_id     TEXT,
                    set_at             TEXT
                )
                """
            )
            conn.commit()
        finally:
            conn.close()

    async def get(self, release_group_mbid: str) -> str | None:
        """The pinned release MBID, or None (fall back to mode-over-files/auto)."""

        def operation(conn: sqlite3.Connection) -> str | None:
            row = conn.execute(
                "SELECT release_mbid FROM album_release_pins WHERE release_group_mbid = ?",
                (release_group_mbid.lower(),),
            ).fetchone()
            return str(row["release_mbid"]) if row else None

        return await self._read(operation)

    async def set(
        self, release_group_mbid: str, release_mbid: str, set_by_user_id: str | None = None
    ) -> None:
        set_at = datetime.now(timezone.utc).isoformat()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """INSERT INTO album_release_pins
                       (release_group_mbid, release_mbid, set_by_user_id, set_at)
                   VALUES (?, ?, ?, ?)
                   ON CONFLICT(release_group_mbid) DO UPDATE SET
                       release_mbid = excluded.release_mbid,
                       set_by_user_id = excluded.set_by_user_id,
                       set_at = excluded.set_at""",
                (release_group_mbid.lower(), release_mbid, set_by_user_id, set_at),
            )

        await self._write(operation)

    async def clear(self, release_group_mbid: str) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "DELETE FROM album_release_pins WHERE release_group_mbid = ?",
                (release_group_mbid.lower(),),
            )
            return cursor.rowcount > 0

        return await self._write(operation)
