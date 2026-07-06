"""``WantedStore`` - persistence for wanted-watcher album watches (plan §5.1).

Two tables in the shared ``library.db``:

- ``wanted_watches`` - one row per release group (D7), carrying its own display
  metadata so a pruned request row never blanks the UI (§4.4).
- ``wanted_seen_candidates`` - the seen set that dedups manual-tier badge events
  (D2). ``identity`` uses the ``models.download_identity`` encoding.

Multi-statement cycle writes (counter bump + seen-set insert) run in ONE
``_write`` closure so a crash can't record a check without its seen rows.
"""

import sqlite3
import threading
import time
from pathlib import Path

from infrastructure.persistence._database import PersistenceBase
from models.wanted import WantedWatch

_TERMINAL_STATES = ("stopped", "fulfilled")


def _row_to_watch(row: sqlite3.Row | None) -> WantedWatch | None:
    if row is None:
        return None
    return WantedWatch(
        release_group_mbid=row["release_group_mbid"],
        user_id=row["user_id"],
        artist_name=row["artist_name"],
        album_title=row["album_title"],
        artist_mbid=row["artist_mbid"],
        year=row["year"],
        cover_url=row["cover_url"],
        kind=row["kind"],
        state=row["state"],
        created_at=row["created_at"],
        first_release_date=row["first_release_date"],
        check_count=row["check_count"],
        quiet_streak=row["quiet_streak"],
        last_checked_at=row["last_checked_at"],
        next_check_at=row["next_check_at"],
        last_outcome=row["last_outcome"],
        new_candidate_count=row["new_candidate_count"],
    )


class WantedStore(PersistenceBase):
    def __init__(self, db_path: Path, write_lock: threading.Lock) -> None:
        super().__init__(db_path, write_lock)

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.executescript(
                """
                CREATE TABLE IF NOT EXISTS wanted_watches (
                    release_group_mbid_lower TEXT PRIMARY KEY,
                    release_group_mbid TEXT NOT NULL,
                    user_id TEXT NOT NULL,
                    artist_name TEXT NOT NULL,
                    album_title TEXT NOT NULL,
                    artist_mbid TEXT,
                    year INTEGER,
                    cover_url TEXT,
                    kind TEXT NOT NULL CHECK(kind IN ('missing','partial')),
                    state TEXT NOT NULL DEFAULT 'watching'
                        CHECK(state IN ('watching','dormant','stopped','fulfilled')),
                    created_at REAL NOT NULL,
                    first_release_date TEXT,
                    check_count INTEGER NOT NULL DEFAULT 0,
                    quiet_streak INTEGER NOT NULL DEFAULT 0,
                    last_checked_at REAL,
                    next_check_at REAL NOT NULL,
                    last_outcome TEXT,
                    new_candidate_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_wanted_due
                    ON wanted_watches(state, next_check_at);

                CREATE TABLE IF NOT EXISTS wanted_seen_candidates (
                    release_group_mbid_lower TEXT NOT NULL,
                    source TEXT NOT NULL,
                    identity TEXT NOT NULL,
                    first_seen_at REAL NOT NULL,
                    PRIMARY KEY (release_group_mbid_lower, source, identity)
                );
                """
            )
            conn.commit()
        finally:
            conn.close()

    async def create_watch(
        self,
        *,
        release_group_mbid: str,
        user_id: str,
        artist_name: str,
        album_title: str,
        kind: str,
        next_check_at: float,
        artist_mbid: str | None = None,
        year: int | None = None,
        cover_url: str | None = None,
        first_release_date: str | None = None,
        created_at: float | None = None,
    ) -> bool:
        """Insert a new watch; returns False (untouched row) when one already
        exists for the release group - enrolment never overwrites (§5.2.1)."""
        now = created_at if created_at is not None else time.time()

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                """INSERT INTO wanted_watches (
                       release_group_mbid_lower, release_group_mbid, user_id,
                       artist_name, album_title, artist_mbid, year, cover_url,
                       kind, state, created_at, first_release_date, next_check_at
                   ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'watching', ?, ?, ?)
                   ON CONFLICT(release_group_mbid_lower) DO NOTHING""",
                (
                    release_group_mbid.lower(), release_group_mbid, user_id,
                    artist_name, album_title, artist_mbid, year, cover_url,
                    kind, now, first_release_date, next_check_at,
                ),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    async def rearm_watch(
        self,
        release_group_mbid: str,
        *,
        user_id: str,
        kind: str,
        next_check_at: float,
        now: float | None = None,
    ) -> bool:
        """Re-arm a FULFILLED watch after the user re-requested and it failed
        again (§5.2.1): back to 'watching' with fresh counters. Never touches
        'stopped'/'dormant' (the human's choice stands)."""
        stamp = now if now is not None else time.time()

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                """UPDATE wanted_watches
                   SET state = 'watching', user_id = ?, kind = ?, created_at = ?,
                       check_count = 0, quiet_streak = 0, last_checked_at = NULL,
                       next_check_at = ?, last_outcome = NULL, new_candidate_count = 0
                   WHERE release_group_mbid_lower = ? AND state = 'fulfilled'""",
                (user_id, kind, stamp, next_check_at, release_group_mbid.lower()),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    async def get_watch(self, release_group_mbid: str) -> WantedWatch | None:
        def operation(conn: sqlite3.Connection) -> WantedWatch | None:
            row = conn.execute(
                "SELECT * FROM wanted_watches WHERE release_group_mbid_lower = ?",
                (release_group_mbid.lower(),),
            ).fetchone()
            return _row_to_watch(row)

        return await self._read(operation)

    async def list_due(self, now: float, limit: int) -> list[WantedWatch]:
        def operation(conn: sqlite3.Connection) -> list[WantedWatch]:
            rows = conn.execute(
                """SELECT * FROM wanted_watches
                   WHERE state = 'watching' AND next_check_at <= ?
                   ORDER BY next_check_at LIMIT ?""",
                (now, limit),
            ).fetchall()
            return [w for row in rows if (w := _row_to_watch(row)) is not None]

        return await self._read(operation)

    async def list_watches(self, user_id: str | None = None) -> list[WantedWatch]:
        """All watches, newest first; scoped to one user unless ``user_id`` is
        None (admin sees all)."""

        def operation(conn: sqlite3.Connection) -> list[WantedWatch]:
            if user_id is None:
                rows = conn.execute(
                    "SELECT * FROM wanted_watches ORDER BY created_at DESC"
                ).fetchall()
            else:
                rows = conn.execute(
                    "SELECT * FROM wanted_watches WHERE user_id = ?"
                    " ORDER BY created_at DESC",
                    (user_id,),
                ).fetchall()
            return [w for row in rows if (w := _row_to_watch(row)) is not None]

        return await self._read(operation)

    async def stop_watch(self, release_group_mbid: str) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                """UPDATE wanted_watches SET state = 'stopped'
                   WHERE release_group_mbid_lower = ? AND state IN ('watching','dormant')""",
                (release_group_mbid.lower(),),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    async def resume_watch(self, release_group_mbid: str, now: float | None = None) -> bool:
        """Dormant/stopped -> watching, due immediately. Resume re-anchors the
        dormancy clock by resetting ``created_at`` (plan §8.2: simplest semantic;
        a resumed watch earns a full fresh watch window)."""
        stamp = now if now is not None else time.time()

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                """UPDATE wanted_watches
                   SET state = 'watching', next_check_at = ?, created_at = ?
                   WHERE release_group_mbid_lower = ? AND state IN ('dormant','stopped')""",
                (stamp, stamp, release_group_mbid.lower()),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    async def mark_fulfilled(
        self, release_group_mbid: str, outcome: str, now: float | None = None
    ) -> None:
        stamp = now if now is not None else time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """UPDATE wanted_watches
                   SET state = 'fulfilled', last_outcome = ?, last_checked_at = ?,
                       new_candidate_count = 0
                   WHERE release_group_mbid_lower = ?""",
                (outcome, stamp, release_group_mbid.lower()),
            )

        await self._write(operation)

    async def reschedule(self, release_group_mbid: str, next_check_at: float) -> None:
        """Push the next check without recording a cycle (the active-work guard
        path, §5.2.b: outcome and counters untouched)."""

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE wanted_watches SET next_check_at = ?"
                " WHERE release_group_mbid_lower = ?",
                (next_check_at, release_group_mbid.lower()),
            )

        await self._write(operation)

    async def record_cycle(
        self,
        release_group_mbid: str,
        *,
        outcome: str,
        next_check_at: float,
        quiet: bool,
        go_dormant: bool = False,
        new_candidate_count: int | None = None,
        seen: list[tuple[str, str]] | None = None,
        now: float | None = None,
    ) -> None:
        """One check cycle's result: counter bump, outcome, reschedule, streak,
        optional dormancy flip and badge count, plus the cycle's seen-candidate
        inserts - all one transaction."""
        stamp = now if now is not None else time.time()
        mbid_lower = release_group_mbid.lower()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """UPDATE wanted_watches
                   SET check_count = check_count + 1,
                       last_checked_at = ?,
                       next_check_at = ?,
                       last_outcome = ?,
                       quiet_streak = CASE WHEN ? THEN quiet_streak + 1 ELSE 0 END,
                       state = CASE WHEN ? AND state = 'watching' THEN 'dormant' ELSE state END,
                       new_candidate_count = COALESCE(?, new_candidate_count)
                   WHERE release_group_mbid_lower = ?""",
                (
                    stamp, next_check_at, outcome, int(quiet), int(go_dormant),
                    new_candidate_count, mbid_lower,
                ),
            )
            for source, identity in seen or []:
                conn.execute(
                    """INSERT INTO wanted_seen_candidates
                       (release_group_mbid_lower, source, identity, first_seen_at)
                       VALUES (?, ?, ?, ?)
                       ON CONFLICT(release_group_mbid_lower, source, identity) DO NOTHING""",
                    (mbid_lower, source, identity, stamp),
                )

        await self._write(operation)

    async def add_seen(
        self,
        release_group_mbid: str,
        seen: list[tuple[str, str]],
        now: float | None = None,
    ) -> None:
        """Insert seen candidates outside a check cycle (the review-dismiss path:
        a human rejected these copies, so the watcher must never badge them)."""
        stamp = now if now is not None else time.time()
        mbid_lower = release_group_mbid.lower()

        def operation(conn: sqlite3.Connection) -> None:
            for source, identity in seen:
                conn.execute(
                    """INSERT INTO wanted_seen_candidates
                       (release_group_mbid_lower, source, identity, first_seen_at)
                       VALUES (?, ?, ?, ?)
                       ON CONFLICT(release_group_mbid_lower, source, identity) DO NOTHING""",
                    (mbid_lower, source, identity, stamp),
                )

        await self._write(operation)

    async def seen_identities(self, release_group_mbid: str) -> set[tuple[str, str]]:
        def operation(conn: sqlite3.Connection) -> set[tuple[str, str]]:
            rows = conn.execute(
                "SELECT source, identity FROM wanted_seen_candidates"
                " WHERE release_group_mbid_lower = ?",
                (release_group_mbid.lower(),),
            ).fetchall()
            return {(row["source"], row["identity"]) for row in rows}

        return await self._read(operation)

    async def clear_new_candidates(self, release_group_mbid: str) -> None:
        """The mark-seen path: the user visited the candidates, drop the badge."""

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE wanted_watches SET new_candidate_count = 0"
                " WHERE release_group_mbid_lower = ?",
                (release_group_mbid.lower(),),
            )

        await self._write(operation)

    async def prune(self, retention_days: int) -> tuple[int, int]:
        """Growth control (§5.1): drop terminal (stopped/fulfilled) watches older
        than the request retention window, then any seen-candidate rows whose
        mbid no longer has a watch. Returns ``(watches, seen_rows)`` deleted."""
        cutoff = time.time() - retention_days * 86400

        def operation(conn: sqlite3.Connection) -> tuple[int, int]:
            placeholders = ",".join("?" for _ in _TERMINAL_STATES)
            watches = conn.execute(
                f"""DELETE FROM wanted_watches
                    WHERE state IN ({placeholders})
                      AND COALESCE(last_checked_at, created_at) < ?""",
                (*_TERMINAL_STATES, cutoff),
            ).rowcount
            seen = conn.execute(
                """DELETE FROM wanted_seen_candidates
                   WHERE release_group_mbid_lower NOT IN
                         (SELECT release_group_mbid_lower FROM wanted_watches)"""
            ).rowcount
            return watches, seen

        return await self._write(operation)
