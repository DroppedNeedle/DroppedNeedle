"""``EventsStore`` - persistence for the Upcoming Events feature (.dev-notes/Events).

Tables in the shared ``library.db``:

- ``live_event_feed`` - the shared global concert feed, one row per
  (source, source event, matched artist) so festival bills surface for every
  followed co-headliner.
- ``artist_event_check`` - per-artist sweep cursor (same shape as the follow
  store's ``artist_release_check``).
- ``artist_tm_attraction`` / ``artist_skiddle_ids`` + ``artist_skiddle_resolution``
  - cached source-entity resolutions with ``resolved_at`` for the 7-day TTL;
  negative results are cached too (``match_basis='none'`` / empty id set).
- ``user_event_cities`` - each user's picked cities (per-user view filter).
- ``user_event_seen`` - unseen-badge marker, same shape as
  ``user_new_release_seen``.

``foreign_keys = True`` has ``PersistenceBase`` enable ``PRAGMA foreign_keys`` so
the ``ON DELETE CASCADE`` to ``auth_users(id)`` fires when a user is deleted
(the follow-store pattern).
"""

import sqlite3
import threading
import time
from pathlib import Path

from infrastructure.persistence._database import PersistenceBase
from models.events import (
    EventCity,
    LiveEvent,
    LiveEventInput,
    SkiddleResolution,
    SweepArtist,
    TmResolution,
    UserConcert,
)

_EVENT_COLUMNS = (
    "source, source_event_id, artist_mbid_lower, artist_name, event_name, "
    "venue_name, city, region, country_code, latitude, longitude, starts_at, "
    "local_date, status, ticket_url, match_confidence, discovered_at, updated_at"
)


def _row_to_event(row: sqlite3.Row) -> LiveEvent:
    return LiveEvent(
        source=row["source"],
        source_event_id=row["source_event_id"],
        artist_mbid_lower=row["artist_mbid_lower"],
        artist_name=row["artist_name"],
        event_name=row["event_name"],
        local_date=row["local_date"],
        status=row["status"],
        match_confidence=row["match_confidence"],
        discovered_at=row["discovered_at"],
        updated_at=row["updated_at"],
        venue_name=row["venue_name"],
        city=row["city"],
        region=row["region"],
        country_code=row["country_code"],
        latitude=row["latitude"],
        longitude=row["longitude"],
        starts_at=row["starts_at"],
        ticket_url=row["ticket_url"],
    )


class EventsStore(PersistenceBase):
    def __init__(self, db_path: Path, write_lock: threading.Lock) -> None:
        super().__init__(db_path, write_lock)

    foreign_keys = True

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.executescript(
                """
                CREATE TABLE IF NOT EXISTS live_event_feed (
                    source            TEXT NOT NULL CHECK(source IN ('ticketmaster','skiddle')),
                    source_event_id   TEXT NOT NULL,
                    artist_mbid_lower TEXT NOT NULL,
                    artist_name       TEXT NOT NULL,
                    event_name        TEXT NOT NULL,
                    venue_name        TEXT,
                    city              TEXT,
                    region            TEXT,
                    country_code      TEXT,
                    latitude          REAL,
                    longitude         REAL,
                    starts_at         TEXT,
                    local_date        TEXT NOT NULL,
                    status            TEXT NOT NULL DEFAULT 'scheduled'
                        CHECK(status IN ('scheduled','cancelled','rescheduled')),
                    ticket_url        TEXT,
                    match_confidence  TEXT NOT NULL CHECK(match_confidence IN ('mbid','name')),
                    discovered_at     REAL NOT NULL,
                    updated_at        REAL NOT NULL,
                    PRIMARY KEY (source, source_event_id, artist_mbid_lower)
                );
                CREATE INDEX IF NOT EXISTS idx_lef_artist ON live_event_feed(artist_mbid_lower);
                CREATE INDEX IF NOT EXISTS idx_lef_date ON live_event_feed(local_date);

                CREATE TABLE IF NOT EXISTS artist_event_check (
                    artist_mbid_lower TEXT PRIMARY KEY,
                    last_checked_at   REAL,
                    last_status       TEXT,
                    last_error        TEXT
                );

                CREATE TABLE IF NOT EXISTS artist_tm_attraction (
                    artist_mbid_lower TEXT PRIMARY KEY,
                    attraction_id     TEXT,
                    match_basis       TEXT NOT NULL
                        CHECK(match_basis IN ('mbid','exact_name','none')),
                    resolved_at       REAL NOT NULL
                );

                CREATE TABLE IF NOT EXISTS artist_skiddle_ids (
                    artist_mbid_lower TEXT NOT NULL,
                    skiddle_artistid  TEXT NOT NULL,
                    PRIMARY KEY (artist_mbid_lower, skiddle_artistid)
                );

                CREATE TABLE IF NOT EXISTS artist_skiddle_resolution (
                    artist_mbid_lower TEXT PRIMARY KEY,
                    resolved_at       REAL NOT NULL
                );

                CREATE TABLE IF NOT EXISTS user_event_cities (
                    user_id      TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                    city_name    TEXT NOT NULL,
                    country_code TEXT,
                    latitude     REAL NOT NULL,
                    longitude    REAL NOT NULL,
                    radius_km    REAL NOT NULL,
                    position     INTEGER NOT NULL,
                    PRIMARY KEY (user_id, latitude, longitude)
                );

                CREATE TABLE IF NOT EXISTS user_event_seen (
                    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
                    seen_at REAL NOT NULL
                );
                """
            )
            conn.commit()
        finally:
            conn.close()

    # -- feed -----------------------------------------------------------------

    async def apply_sweep_result(
        self,
        artist_mbid_lower: str,
        upserts: list[LiveEventInput],
        delete_keys: list[tuple[str, str]] | None = None,
        now: float | None = None,
    ) -> None:
        """Apply one artist's sweep diff atomically: upsert current rows
        (preserving ``discovered_at`` on conflict) and delete superseded/stale
        keys (``(source, source_event_id)`` pairs) in the same transaction."""
        stamp = now if now is not None else time.time()

        def operation(conn: sqlite3.Connection) -> None:
            for source, event_id in delete_keys or []:
                conn.execute(
                    "DELETE FROM live_event_feed WHERE source = ? AND source_event_id = ?"
                    " AND artist_mbid_lower = ?",
                    (source, event_id, artist_mbid_lower),
                )
            for ev in upserts:
                conn.execute(
                    f"""
                    INSERT INTO live_event_feed ({_EVENT_COLUMNS})
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(source, source_event_id, artist_mbid_lower) DO UPDATE SET
                        artist_name = excluded.artist_name,
                        event_name = excluded.event_name,
                        venue_name = excluded.venue_name,
                        city = excluded.city,
                        region = excluded.region,
                        country_code = excluded.country_code,
                        latitude = excluded.latitude,
                        longitude = excluded.longitude,
                        starts_at = excluded.starts_at,
                        local_date = excluded.local_date,
                        status = excluded.status,
                        ticket_url = excluded.ticket_url,
                        match_confidence = excluded.match_confidence,
                        updated_at = excluded.updated_at
                    """,
                    (
                        ev.source,
                        ev.source_event_id,
                        ev.artist_mbid_lower,
                        ev.artist_name,
                        ev.event_name,
                        ev.venue_name,
                        ev.city,
                        ev.region,
                        ev.country_code,
                        ev.latitude,
                        ev.longitude,
                        ev.starts_at,
                        ev.local_date,
                        ev.status,
                        ev.ticket_url,
                        ev.match_confidence,
                        stamp,
                        stamp,
                    ),
                )

        await self._write(operation)

    async def list_events_for_artist(self, artist_mbid_lower: str) -> list[LiveEvent]:
        def operation(conn: sqlite3.Connection) -> list[LiveEvent]:
            rows = conn.execute(
                f"SELECT {_EVENT_COLUMNS} FROM live_event_feed WHERE artist_mbid_lower = ?",
                (artist_mbid_lower,),
            ).fetchall()
            return [_row_to_event(row) for row in rows]

        return await self._read(operation)

    async def list_library_artists(self) -> list[SweepArtist]:
        """Every artist in the library index with an MBID (library sweep scope).
        ``library_artists`` lives in the same shared database file (created by
        LibraryDB at startup, rebuilt by scans)."""

        def operation(conn: sqlite3.Connection) -> list[SweepArtist]:
            rows = conn.execute(
                "SELECT mbid, mbid_lower, name FROM library_artists"
                " WHERE mbid IS NOT NULL AND mbid != '' ORDER BY mbid_lower"
            ).fetchall()
            return [
                SweepArtist(
                    artist_mbid=row["mbid"],
                    artist_mbid_lower=row["mbid_lower"],
                    artist_name=row["name"],
                )
                for row in rows
            ]

        return await self._read(operation)

    async def cursor_ages(self) -> dict[str, float]:
        """artist_mbid_lower -> last_checked_at, for least-recently-checked-first
        sweep rotation (artists never checked are absent and sort first)."""

        def operation(conn: sqlite3.Connection) -> dict[str, float]:
            rows = conn.execute(
                "SELECT artist_mbid_lower, last_checked_at FROM artist_event_check"
                " WHERE last_checked_at IS NOT NULL"
            ).fetchall()
            return {row["artist_mbid_lower"]: row["last_checked_at"] for row in rows}

        return await self._read(operation)

    async def list_all_events(
        self,
        min_local_date: str,
        discovered_after: float | None = None,
    ) -> list[UserConcert]:
        """Every feed row on/after ``min_local_date`` regardless of follows
        (library sweep scope: the whole library's events are everyone's).
        ``artist_mbid`` is the lowercase form - MBIDs are case-insensitive."""

        def operation(conn: sqlite3.Connection) -> list[UserConcert]:
            sql = f"SELECT {_EVENT_COLUMNS} FROM live_event_feed WHERE local_date >= ?"
            params: list = [min_local_date]
            if discovered_after is not None:
                sql += " AND discovered_at > ?"
                params.append(discovered_after)
            sql += " ORDER BY local_date ASC, event_name ASC"
            rows = conn.execute(sql, params).fetchall()
            return [
                UserConcert(event=_row_to_event(row), artist_mbid=row["artist_mbid_lower"])
                for row in rows
            ]

        return await self._read(operation)

    async def list_events_for_user(
        self,
        user_id: str,
        min_local_date: str,
        discovered_after: float | None = None,
    ) -> list[UserConcert]:
        """Feed rows for the user's followed artists on/after ``min_local_date``
        (venue-local ISO date, string-comparable), oldest first. City filtering
        is the service's job (haversine over these rows)."""

        def operation(conn: sqlite3.Connection) -> list[UserConcert]:
            sql = f"""
                SELECT {', '.join('lef.' + c.strip() for c in _EVENT_COLUMNS.split(','))},
                       ufa.artist_mbid AS artist_mbid
                FROM live_event_feed lef
                JOIN user_followed_artists ufa
                    ON ufa.artist_mbid_lower = lef.artist_mbid_lower AND ufa.user_id = ?
                WHERE lef.local_date >= ?
            """
            params: list = [user_id, min_local_date]
            if discovered_after is not None:
                sql += " AND lef.discovered_at > ?"
                params.append(discovered_after)
            sql += " ORDER BY lef.local_date ASC, lef.event_name ASC"
            rows = conn.execute(sql, params).fetchall()
            return [
                UserConcert(event=_row_to_event(row), artist_mbid=row["artist_mbid"])
                for row in rows
            ]

        return await self._read(operation)

    async def prune_past_events(self, cutoff_local_date: str) -> int:
        """Delete rows whose venue-local date is strictly before the cutoff
        (callers pass today minus the grace window)."""

        def operation(conn: sqlite3.Connection) -> int:
            return conn.execute(
                "DELETE FROM live_event_feed WHERE local_date < ?",
                (cutoff_local_date,),
            ).rowcount

        return await self._write(operation)

    # -- sweep cursor ----------------------------------------------------------

    async def update_cursor(
        self, artist_mbid_lower: str, status: str, error: str | None = None
    ) -> None:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO artist_event_check (artist_mbid_lower, last_checked_at, last_status, last_error)
                VALUES (?, ?, ?, ?)
                ON CONFLICT(artist_mbid_lower) DO UPDATE SET
                    last_checked_at = excluded.last_checked_at,
                    last_status = excluded.last_status,
                    last_error = excluded.last_error
                """,
                (artist_mbid_lower, now, status, error),
            )

        await self._write(operation)

    # -- source resolutions ----------------------------------------------------

    async def get_tm_resolution(self, artist_mbid_lower: str) -> TmResolution | None:
        def operation(conn: sqlite3.Connection) -> TmResolution | None:
            row = conn.execute(
                "SELECT attraction_id, match_basis, resolved_at FROM artist_tm_attraction"
                " WHERE artist_mbid_lower = ?",
                (artist_mbid_lower,),
            ).fetchone()
            if row is None:
                return None
            return TmResolution(
                attraction_id=row["attraction_id"],
                match_basis=row["match_basis"],
                resolved_at=row["resolved_at"],
            )

        return await self._read(operation)

    async def set_tm_resolution(
        self,
        artist_mbid_lower: str,
        attraction_id: str | None,
        match_basis: str,
        now: float | None = None,
    ) -> None:
        stamp = now if now is not None else time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO artist_tm_attraction (artist_mbid_lower, attraction_id, match_basis, resolved_at)
                VALUES (?, ?, ?, ?)
                ON CONFLICT(artist_mbid_lower) DO UPDATE SET
                    attraction_id = excluded.attraction_id,
                    match_basis = excluded.match_basis,
                    resolved_at = excluded.resolved_at
                """,
                (artist_mbid_lower, attraction_id, match_basis, stamp),
            )

        await self._write(operation)

    async def get_skiddle_resolution(self, artist_mbid_lower: str) -> SkiddleResolution | None:
        def operation(conn: sqlite3.Connection) -> SkiddleResolution | None:
            marker = conn.execute(
                "SELECT resolved_at FROM artist_skiddle_resolution WHERE artist_mbid_lower = ?",
                (artist_mbid_lower,),
            ).fetchone()
            if marker is None:
                return None
            rows = conn.execute(
                "SELECT skiddle_artistid FROM artist_skiddle_ids WHERE artist_mbid_lower = ?"
                " ORDER BY skiddle_artistid",
                (artist_mbid_lower,),
            ).fetchall()
            return SkiddleResolution(
                artist_ids=[row["skiddle_artistid"] for row in rows],
                resolved_at=marker["resolved_at"],
            )

        return await self._read(operation)

    async def set_skiddle_resolution(
        self, artist_mbid_lower: str, artist_ids: list[str], now: float | None = None
    ) -> None:
        """Replace the id set and stamp the marker in one transaction (an empty
        set is a valid negative result and still gets a marker for the TTL)."""
        stamp = now if now is not None else time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "DELETE FROM artist_skiddle_ids WHERE artist_mbid_lower = ?",
                (artist_mbid_lower,),
            )
            conn.executemany(
                "INSERT INTO artist_skiddle_ids (artist_mbid_lower, skiddle_artistid) VALUES (?, ?)",
                [(artist_mbid_lower, sid) for sid in artist_ids],
            )
            conn.execute(
                """
                INSERT INTO artist_skiddle_resolution (artist_mbid_lower, resolved_at)
                VALUES (?, ?)
                ON CONFLICT(artist_mbid_lower) DO UPDATE SET resolved_at = excluded.resolved_at
                """,
                (artist_mbid_lower, stamp),
            )

        await self._write(operation)

    # -- user cities -----------------------------------------------------------

    async def list_cities(self, user_id: str) -> list[EventCity]:
        def operation(conn: sqlite3.Connection) -> list[EventCity]:
            rows = conn.execute(
                "SELECT city_name, country_code, latitude, longitude, radius_km, position"
                " FROM user_event_cities WHERE user_id = ? ORDER BY position",
                (user_id,),
            ).fetchall()
            return [
                EventCity(
                    city_name=row["city_name"],
                    country_code=row["country_code"],
                    latitude=row["latitude"],
                    longitude=row["longitude"],
                    radius_km=row["radius_km"],
                    position=row["position"],
                )
                for row in rows
            ]

        return await self._read(operation)

    async def replace_cities(self, user_id: str, cities: list[EventCity]) -> None:
        """Replace-all semantics (the picker submits its full state)."""

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute("DELETE FROM user_event_cities WHERE user_id = ?", (user_id,))
            conn.executemany(
                """
                INSERT INTO user_event_cities
                    (user_id, city_name, country_code, latitude, longitude, radius_km, position)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(user_id, latitude, longitude) DO NOTHING
                """,
                [
                    (
                        user_id,
                        c.city_name,
                        c.country_code,
                        c.latitude,
                        c.longitude,
                        c.radius_km,
                        index,
                    )
                    for index, c in enumerate(cities)
                ],
            )

        await self._write(operation)

    # -- unseen badge ----------------------------------------------------------

    async def get_seen_at(self, user_id: str) -> float:
        def operation(conn: sqlite3.Connection) -> float:
            row = conn.execute(
                "SELECT seen_at FROM user_event_seen WHERE user_id = ?", (user_id,)
            ).fetchone()
            return row["seen_at"] if row else 0.0

        return await self._read(operation)

    async def mark_seen(self, user_id: str, now: float | None = None) -> None:
        stamp = now if now is not None else time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO user_event_seen (user_id, seen_at) VALUES (?, ?)
                ON CONFLICT(user_id) DO UPDATE SET seen_at = excluded.seen_at
                """,
                (user_id, stamp),
            )

        await self._write(operation)
