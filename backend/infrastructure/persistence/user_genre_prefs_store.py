"""Per-user genre-balance preferences for recommendations.

Default-normal semantics: only "reduce"/"mute" levels are stored, so every
genre family is at full weight unless the user dials it down. Families are
stored in normalised form (see ``services.discover.genre_balance.genre_family``)
by the API layer; this store is a dumb (user, family) -> level table.
"""

import sqlite3
from datetime import datetime, timezone

from infrastructure.persistence._database import PersistenceBase

_VALID_LEVELS = ("reduce", "mute")


class UserGenrePrefsStore(PersistenceBase):
    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS user_genre_prefs (
                  user_id      TEXT NOT NULL,
                  genre_family TEXT NOT NULL,
                  level        TEXT NOT NULL,
                  updated_at   TEXT NOT NULL,
                  PRIMARY KEY (user_id, genre_family)
                )
                """
            )
            conn.commit()
        finally:
            conn.close()

    async def get_levels(self, user_id: str) -> dict[str, str]:
        """{genre_family: level} for this user's non-normal families."""

        def operation(conn: sqlite3.Connection) -> dict[str, str]:
            rows = conn.execute(
                "SELECT genre_family, level FROM user_genre_prefs WHERE user_id = ?",
                (user_id,),
            ).fetchall()
            return {
                row["genre_family"]: row["level"]
                for row in rows
                if row["level"] in _VALID_LEVELS
            }

        return await self._read(operation)

    async def set_levels(self, user_id: str, levels: dict[str, str]) -> None:
        """Replace the user's non-normal levels in one transaction."""
        now = datetime.now(timezone.utc).isoformat()
        rows = [
            (user_id, family, level, now)
            for family, level in sorted(levels.items())
            if family and level in _VALID_LEVELS
        ]

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute("DELETE FROM user_genre_prefs WHERE user_id = ?", (user_id,))
            conn.executemany(
                "INSERT INTO user_genre_prefs (user_id, genre_family, level, updated_at) "
                "VALUES (?, ?, ?, ?)",
                rows,
            )

        await self._write(operation)
