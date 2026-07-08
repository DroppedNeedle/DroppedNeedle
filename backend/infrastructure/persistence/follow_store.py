"""Per-user Follow + auto-download persistence.

``PRAGMA foreign_keys=ON`` in ``_connect`` is what makes the
``ON DELETE CASCADE`` to ``auth_users(id)`` fire when a user is deleted.

Per DD1, follow state lives here and never as a column on ``library_artists`` /
``library_albums`` - those are wiped and rebuilt on every full library scan.
"""

import asyncio
import logging
import sqlite3
import threading
import time
from datetime import date, timedelta
from pathlib import Path

import msgspec

logger = logging.getLogger(__name__)


class FollowState(msgspec.Struct, frozen=True):
    # auto_download_state (none|pending|approved|rejected|revoked) is derived
    # from the approval row here; the admin role override is applied in the
    # service layer, which knows the role.
    followed: bool
    auto_download: bool
    auto_download_state: str


class FollowedArtist(msgspec.Struct, frozen=True):
    artist_mbid: str
    artist_name: str
    auto_download: bool
    auto_download_state: str
    followed_at: float


class Approval(msgspec.Struct, frozen=True):
    user_id: str
    artist_mbid: str
    artist_name: str
    state: str
    requested_at: float
    reviewed_by_id: str | None = None
    reviewed_by_name: str | None = None
    reviewed_at: float | None = None
    user_name: str | None = None


class ApprovalBatch(msgspec.Struct, frozen=True):
    """A grouped bulk auto-download approval (LidarrImport D3): N per-artist
    ``auto_download_approvals`` rows sharing a ``batch_id``, presented as one admin card."""

    batch_id: str
    user_id: str
    artist_count: int
    sample_names: list[str]
    requested_at: float
    source: str
    user_name: str | None = None


class DistinctFollowedArtist(msgspec.Struct, frozen=True):
    artist_mbid: str
    artist_mbid_lower: str
    artist_name: str


class NewRelease(msgspec.Struct, frozen=True):
    release_group_mbid: str
    artist_mbid: str
    artist_name: str
    title: str
    primary_type: str | None
    secondary_types: str | None
    first_release_date: str | None
    discovered_at: float
    # set by the recent-releases log view; the needs-requesting view leaves it
    # False by construction (owned rows are filtered out there)
    in_library: bool = False


class NewReleaseInput(msgspec.Struct, frozen=True):
    release_group_mbid: str
    release_group_mbid_lower: str
    artist_mbid_lower: str
    artist_name: str
    title: str
    primary_type: str | None = None
    secondary_types: str | None = None
    first_release_date: str | None = None


class FollowStore:
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
            conn.executescript(
                """
                CREATE TABLE IF NOT EXISTS user_followed_artists (
                    user_id           TEXT    NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                    artist_mbid       TEXT    NOT NULL,
                    artist_mbid_lower TEXT    NOT NULL,
                    artist_name       TEXT    NOT NULL,
                    auto_download     INTEGER NOT NULL DEFAULT 0,
                    followed_at       REAL    NOT NULL,
                    updated_at        REAL    NOT NULL,
                    PRIMARY KEY (user_id, artist_mbid_lower)
                );
                CREATE INDEX IF NOT EXISTS idx_ufa_user ON user_followed_artists(user_id);
                CREATE INDEX IF NOT EXISTS idx_ufa_mbid ON user_followed_artists(artist_mbid_lower);
                CREATE INDEX IF NOT EXISTS idx_ufa_autodl
                    ON user_followed_artists(auto_download) WHERE auto_download = 1;

                CREATE TABLE IF NOT EXISTS auto_download_approvals (
                    user_id           TEXT    NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                    artist_mbid       TEXT    NOT NULL,
                    artist_mbid_lower TEXT    NOT NULL,
                    artist_name       TEXT    NOT NULL,
                    state             TEXT    NOT NULL DEFAULT 'pending',
                    requested_at      REAL    NOT NULL,
                    reviewed_by_id    TEXT,
                    reviewed_by_name  TEXT,
                    reviewed_at       REAL,
                    batch_id          TEXT,
                    source            TEXT,
                    PRIMARY KEY (user_id, artist_mbid_lower)
                );
                CREATE INDEX IF NOT EXISTS idx_ada_pending
                    ON auto_download_approvals(state) WHERE state = 'pending';

                CREATE TABLE IF NOT EXISTS new_release_feed (
                    release_group_mbid_lower TEXT    PRIMARY KEY,
                    release_group_mbid       TEXT    NOT NULL,
                    artist_mbid_lower        TEXT    NOT NULL,
                    artist_name              TEXT    NOT NULL,
                    title                    TEXT    NOT NULL,
                    primary_type             TEXT,
                    secondary_types          TEXT,
                    first_release_date       TEXT,
                    discovered_at            REAL    NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_nrf_artist ON new_release_feed(artist_mbid_lower);
                CREATE INDEX IF NOT EXISTS idx_nrf_date ON new_release_feed(first_release_date DESC);

                CREATE TABLE IF NOT EXISTS artist_release_check (
                    artist_mbid_lower TEXT PRIMARY KEY,
                    last_checked_at   REAL,
                    last_status       TEXT,
                    last_error        TEXT
                );

                CREATE TABLE IF NOT EXISTS artist_known_releases (
                    artist_mbid_lower TEXT NOT NULL,
                    rg_mbid_lower     TEXT NOT NULL,
                    PRIMARY KEY (artist_mbid_lower, rg_mbid_lower)
                );

                CREATE TABLE IF NOT EXISTS user_new_release_seen (
                    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
                    seen_at REAL NOT NULL
                );
                """
            )
            # Additive ratchet for DBs created before the LidarrImport bulk-approval columns
            # (LidarrImport DR5). Idempotent: a no-op once the columns exist.
            self._safe_alter(
                conn, "ALTER TABLE auto_download_approvals ADD COLUMN batch_id TEXT"
            )
            self._safe_alter(
                conn, "ALTER TABLE auto_download_approvals ADD COLUMN source TEXT"
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_ada_batch "
                "ON auto_download_approvals(batch_id) WHERE batch_id IS NOT NULL"
            )
            conn.commit()
        finally:
            conn.close()

    @staticmethod
    def _safe_alter(conn: sqlite3.Connection, sql: str) -> None:
        """Idempotent additive migration: an ``ADD COLUMN`` that already ran raises
        ``OperationalError: duplicate column name`` - swallow only that."""
        try:
            conn.execute(sql)
        except sqlite3.OperationalError:
            pass

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

    @staticmethod
    def _derive_state(intent: bool, approval_state: str | None) -> str:
        """Role-agnostic display state. The service applies the admin override."""
        if intent:
            return approval_state or "none"
        if approval_state in ("rejected", "revoked"):
            return approval_state
        return "none"

    async def follow_artist(self, user_id: str, artist_mbid: str, artist_name: str) -> None:
        # re-following preserves auto_download intent and followed_at, only
        # refreshing the name snapshot and updated_at.
        mbid_lower = artist_mbid.lower()
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO user_followed_artists (
                    user_id, artist_mbid, artist_mbid_lower, artist_name,
                    auto_download, followed_at, updated_at
                ) VALUES (?, ?, ?, ?, 0, ?, ?)
                ON CONFLICT(user_id, artist_mbid_lower) DO UPDATE SET
                    artist_name = excluded.artist_name,
                    updated_at = excluded.updated_at
                """,
                (user_id, artist_mbid, mbid_lower, artist_name, now, now),
            )

        await self._write(operation)

    async def unfollow_artist(self, user_id: str, artist_mbid: str) -> bool:
        # any approval row is deliberately left intact (L4)
        mbid_lower = artist_mbid.lower()

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "DELETE FROM user_followed_artists WHERE user_id = ? AND artist_mbid_lower = ?",
                (user_id, mbid_lower),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    async def set_auto_download_intent(self, user_id: str, artist_mbid: str, enabled: bool) -> None:
        mbid_lower = artist_mbid.lower()
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE user_followed_artists SET auto_download = ?, updated_at = ? "
                "WHERE user_id = ? AND artist_mbid_lower = ?",
                (1 if enabled else 0, now, user_id, mbid_lower),
            )

        await self._write(operation)

    async def follow_artists_bulk(
        self, user_id: str, artists: list[tuple[str, str]]
    ) -> None:
        """Bulk follow (LidarrImport DR2): one transaction, names supplied by the caller
        (0 MusicBrainz calls). Reuses ``follow_artist``'s idempotent UPSERT semantics -
        existing rows keep their ``auto_download`` intent + ``followed_at`` and only refresh
        the name snapshot + ``updated_at`` (D6/DR4). ``artists`` = [(artist_mbid, name)]."""
        if not artists:
            return
        now = time.time()
        rows = [(user_id, mbid, mbid.lower(), name, now, now) for mbid, name in artists]

        def operation(conn: sqlite3.Connection) -> None:
            conn.executemany(
                """
                INSERT INTO user_followed_artists (
                    user_id, artist_mbid, artist_mbid_lower, artist_name,
                    auto_download, followed_at, updated_at
                ) VALUES (?, ?, ?, ?, 0, ?, ?)
                ON CONFLICT(user_id, artist_mbid_lower) DO UPDATE SET
                    artist_name = excluded.artist_name,
                    updated_at = excluded.updated_at
                """,
                rows,
            )

        await self._write(operation)

    async def set_auto_download_intent_bulk(
        self, user_id: str, artist_mbids: list[str], enabled: bool
    ) -> None:
        """Flip auto-download intent on/off for many followed artists in one transaction."""
        if not artist_mbids:
            return
        now = time.time()
        value = 1 if enabled else 0
        rows = [(value, now, user_id, mbid.lower()) for mbid in artist_mbids]

        def operation(conn: sqlite3.Connection) -> None:
            conn.executemany(
                "UPDATE user_followed_artists SET auto_download = ?, updated_at = ? "
                "WHERE user_id = ? AND artist_mbid_lower = ?",
                rows,
            )

        await self._write(operation)

    async def existing_followed_lower(
        self, user_id: str, artist_mbids_lower: list[str]
    ) -> set[str]:
        """The subset of ``artist_mbids_lower`` this user already follows. The pre-read the
        importer does BEFORE any write, to compute real imported/already-following counts and
        the D9 brand-new-only auto-download subset (LidarrImport DR6)."""
        if not artist_mbids_lower:
            return set()

        def operation(conn: sqlite3.Connection) -> set[str]:
            placeholders = ",".join("?" * len(artist_mbids_lower))
            rows = conn.execute(
                f"SELECT artist_mbid_lower FROM user_followed_artists "
                f"WHERE user_id = ? AND artist_mbid_lower IN ({placeholders})",
                (user_id, *artist_mbids_lower),
            ).fetchall()
            return {row["artist_mbid_lower"] for row in rows}

        return await self._read(operation)

    async def get_follow_state(self, user_id: str, artist_mbid: str) -> FollowState:
        mbid_lower = artist_mbid.lower()

        def operation(conn: sqlite3.Connection) -> tuple[sqlite3.Row | None, sqlite3.Row | None]:
            follow = conn.execute(
                "SELECT auto_download FROM user_followed_artists "
                "WHERE user_id = ? AND artist_mbid_lower = ?",
                (user_id, mbid_lower),
            ).fetchone()
            approval = conn.execute(
                "SELECT state FROM auto_download_approvals "
                "WHERE user_id = ? AND artist_mbid_lower = ?",
                (user_id, mbid_lower),
            ).fetchone()
            return follow, approval

        follow, approval = await self._read(operation)
        if follow is None:
            return FollowState(followed=False, auto_download=False, auto_download_state="none")
        intent = bool(follow["auto_download"])
        approval_state = approval["state"] if approval else None
        return FollowState(
            followed=True,
            auto_download=intent,
            auto_download_state=self._derive_state(intent, approval_state),
        )

    async def list_followed_artists(self, user_id: str) -> list[FollowedArtist]:
        def operation(conn: sqlite3.Connection) -> list[sqlite3.Row]:
            return conn.execute(
                """
                SELECT ufa.artist_mbid AS artist_mbid, ufa.artist_name AS artist_name,
                       ufa.auto_download AS auto_download, ufa.followed_at AS followed_at,
                       ada.state AS approval_state
                FROM user_followed_artists ufa
                LEFT JOIN auto_download_approvals ada
                    ON ada.user_id = ufa.user_id AND ada.artist_mbid_lower = ufa.artist_mbid_lower
                WHERE ufa.user_id = ?
                ORDER BY ufa.followed_at DESC
                """,
                (user_id,),
            ).fetchall()

        rows = await self._read(operation)
        result = []
        for row in rows:
            intent = bool(row["auto_download"])
            result.append(
                FollowedArtist(
                    artist_mbid=row["artist_mbid"],
                    artist_name=row["artist_name"],
                    auto_download=intent,
                    auto_download_state=self._derive_state(intent, row["approval_state"]),
                    followed_at=row["followed_at"],
                )
            )
        return result

    async def upsert_approval(
        self,
        user_id: str,
        artist_mbid: str,
        artist_name: str,
        state: str,
        reviewer: tuple[str, str | None] | None = None,
    ) -> None:
        # requested_at is refreshed so a re-request surfaces fresh in the admin
        # queue; reviewer fields are cleared unless a reviewer is given.
        mbid_lower = artist_mbid.lower()
        now = time.time()
        reviewed_by_id = reviewer[0] if reviewer else None
        reviewed_by_name = reviewer[1] if reviewer else None
        reviewed_at = now if reviewer else None

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO auto_download_approvals (
                    user_id, artist_mbid, artist_mbid_lower, artist_name,
                    state, requested_at, reviewed_by_id, reviewed_by_name, reviewed_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(user_id, artist_mbid_lower) DO UPDATE SET
                    artist_name = excluded.artist_name,
                    state = excluded.state,
                    requested_at = excluded.requested_at,
                    reviewed_by_id = excluded.reviewed_by_id,
                    reviewed_by_name = excluded.reviewed_by_name,
                    reviewed_at = excluded.reviewed_at
                """,
                (
                    user_id,
                    artist_mbid,
                    mbid_lower,
                    artist_name,
                    state,
                    now,
                    reviewed_by_id,
                    reviewed_by_name,
                    reviewed_at,
                ),
            )

        await self._write(operation)

    async def set_approval_state(
        self,
        user_id: str,
        artist_mbid: str,
        state: str,
        reviewer: tuple[str, str | None],
    ) -> bool:
        # admin review transition; leaves requested_at untouched
        mbid_lower = artist_mbid.lower()
        now = time.time()
        reviewed_by_id, reviewed_by_name = reviewer

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "UPDATE auto_download_approvals SET state = ?, reviewed_by_id = ?, "
                "reviewed_by_name = ?, reviewed_at = ? "
                "WHERE user_id = ? AND artist_mbid_lower = ?",
                (state, reviewed_by_id, reviewed_by_name, now, user_id, mbid_lower),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    async def get_approval(self, user_id: str, artist_mbid: str) -> Approval | None:
        mbid_lower = artist_mbid.lower()

        def operation(conn: sqlite3.Connection) -> sqlite3.Row | None:
            return conn.execute(
                "SELECT * FROM auto_download_approvals WHERE user_id = ? AND artist_mbid_lower = ?",
                (user_id, mbid_lower),
            ).fetchone()

        row = await self._read(operation)
        if row is None:
            return None
        return Approval(
            user_id=row["user_id"],
            artist_mbid=row["artist_mbid"],
            artist_name=row["artist_name"],
            state=row["state"],
            requested_at=row["requested_at"],
            reviewed_by_id=row["reviewed_by_id"],
            reviewed_by_name=row["reviewed_by_name"],
            reviewed_at=row["reviewed_at"],
        )

    async def list_pending_approvals(self) -> list[Approval]:
        def operation(conn: sqlite3.Connection) -> list[sqlite3.Row]:
            return conn.execute(
                """
                SELECT ada.user_id AS user_id, ada.artist_mbid AS artist_mbid,
                       ada.artist_name AS artist_name, ada.state AS state,
                       ada.requested_at AS requested_at, au.display_name AS user_name
                FROM auto_download_approvals ada
                JOIN auth_users au ON au.id = ada.user_id
                WHERE ada.state = 'pending' AND ada.batch_id IS NULL
                ORDER BY ada.requested_at ASC
                """
            ).fetchall()

        rows = await self._read(operation)
        return [
            Approval(
                user_id=row["user_id"],
                artist_mbid=row["artist_mbid"],
                artist_name=row["artist_name"],
                state=row["state"],
                requested_at=row["requested_at"],
                user_name=row["user_name"],
            )
            for row in rows
        ]

    async def create_import_approval_batch(
        self,
        user_id: str,
        artists: list[tuple[str, str]],
        batch_id: str,
        source: str = "lidarr_import",
    ) -> None:
        """Create one grouped pending approval batch (LidarrImport D3): N per-artist rows
        sharing ``batch_id``. Guarded conflict clause NEVER downgrades an already-``approved``
        row (D9 defence-in-depth) - only non-approved rows are (re-)armed to this batch.
        ``artists`` = [(artist_mbid, name)]. One transaction."""
        if not artists:
            return
        now = time.time()
        rows = [
            (user_id, mbid, mbid.lower(), name, now, batch_id, source)
            for mbid, name in artists
        ]

        def operation(conn: sqlite3.Connection) -> None:
            conn.executemany(
                """
                INSERT INTO auto_download_approvals (
                    user_id, artist_mbid, artist_mbid_lower, artist_name,
                    state, requested_at, reviewed_by_id, reviewed_by_name, reviewed_at,
                    batch_id, source
                ) VALUES (?, ?, ?, ?, 'pending', ?, NULL, NULL, NULL, ?, ?)
                ON CONFLICT(user_id, artist_mbid_lower) DO UPDATE SET
                    artist_name = excluded.artist_name,
                    state = 'pending',
                    requested_at = excluded.requested_at,
                    reviewed_by_id = NULL,
                    reviewed_by_name = NULL,
                    reviewed_at = NULL,
                    batch_id = excluded.batch_id,
                    source = excluded.source
                WHERE auto_download_approvals.state != 'approved'
                """,
                rows,
            )

        await self._write(operation)

    async def list_pending_approval_batches(self) -> list[ApprovalBatch]:
        """Grouped pending batches (LidarrImport D3), oldest first. Batched rows are grouped
        by ``(batch_id, user_id)``; ``sample_names`` carries up to 5 names for the card."""

        def operation(conn: sqlite3.Connection) -> list[sqlite3.Row]:
            return conn.execute(
                """
                SELECT ada.batch_id AS batch_id, ada.user_id AS user_id,
                       ada.artist_name AS artist_name, ada.requested_at AS requested_at,
                       ada.source AS source, au.display_name AS user_name
                FROM auto_download_approvals ada
                JOIN auth_users au ON au.id = ada.user_id
                WHERE ada.state = 'pending' AND ada.batch_id IS NOT NULL
                ORDER BY ada.requested_at ASC, ada.artist_name ASC
                """
            ).fetchall()

        rows = await self._read(operation)
        grouped: dict[tuple[str, str], dict] = {}
        for row in rows:
            key = (row["batch_id"], row["user_id"])
            entry = grouped.get(key)
            if entry is None:
                entry = {
                    "batch_id": row["batch_id"],
                    "user_id": row["user_id"],
                    "user_name": row["user_name"],
                    "source": row["source"] or "lidarr_import",
                    "requested_at": row["requested_at"],
                    "count": 0,
                    "names": [],
                }
                grouped[key] = entry
            entry["count"] += 1
            entry["requested_at"] = min(entry["requested_at"], row["requested_at"])
            if len(entry["names"]) < 5:
                entry["names"].append(row["artist_name"])
        batches = [
            ApprovalBatch(
                batch_id=e["batch_id"],
                user_id=e["user_id"],
                user_name=e["user_name"],
                artist_count=e["count"],
                sample_names=e["names"],
                requested_at=e["requested_at"],
                source=e["source"],
            )
            for e in grouped.values()
        ]
        batches.sort(key=lambda b: b.requested_at)
        return batches

    async def set_batch_approval_state(
        self,
        batch_id: str,
        state: str,
        reviewer: tuple[str, str | None],
    ) -> int:
        """Approve/reject/revoke every still-pending row of a batch in ONE transaction.
        For reject/revoke, also flips ``auto_download`` intent off for that batch's artists
        (matching the single-row reject contract). Returns rows affected."""
        now = time.time()
        reviewed_by_id, reviewed_by_name = reviewer

        def operation(conn: sqlite3.Connection) -> int:
            cursor = conn.execute(
                "UPDATE auto_download_approvals SET state = ?, reviewed_by_id = ?, "
                "reviewed_by_name = ?, reviewed_at = ? "
                "WHERE batch_id = ? AND state = 'pending'",
                (state, reviewed_by_id, reviewed_by_name, now, batch_id),
            )
            affected = cursor.rowcount
            if affected and state in ("rejected", "revoked"):
                conn.execute(
                    """
                    UPDATE user_followed_artists SET auto_download = 0, updated_at = ?
                    WHERE (user_id, artist_mbid_lower) IN (
                        SELECT user_id, artist_mbid_lower FROM auto_download_approvals
                        WHERE batch_id = ?
                    )
                    """,
                    (now, batch_id),
                )
            return affected

        return await self._write(operation)

    async def list_distinct_followed_artists(self) -> list[DistinctFollowedArtist]:
        def operation(conn: sqlite3.Connection) -> list[sqlite3.Row]:
            return conn.execute(
                """
                SELECT artist_mbid_lower AS artist_mbid_lower,
                       MIN(artist_mbid) AS artist_mbid,
                       MIN(artist_name) AS artist_name
                FROM user_followed_artists
                GROUP BY artist_mbid_lower
                ORDER BY artist_mbid_lower
                """
            ).fetchall()

        rows = await self._read(operation)
        return [
            DistinctFollowedArtist(
                artist_mbid=row["artist_mbid"],
                artist_mbid_lower=row["artist_mbid_lower"],
                artist_name=row["artist_name"],
            )
            for row in rows
        ]

    async def has_cursor(self, artist_mbid_lower: str) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            return (
                conn.execute(
                    "SELECT 1 FROM artist_release_check WHERE artist_mbid_lower = ?",
                    (artist_mbid_lower,),
                ).fetchone()
                is not None
            )

        return await self._read(operation)

    async def seed_baseline(self, artist_mbid_lower: str, rg_mbids_lower: list[str]) -> None:
        # DD2 first-poll baseline. the ONLY method that creates the cursor row
        # (update_cursor only updates) so a transient error before the first
        # successful baseline never leaves an empty known-set behind a live cursor.
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            if rg_mbids_lower:
                conn.executemany(
                    "INSERT OR IGNORE INTO artist_known_releases (artist_mbid_lower, rg_mbid_lower) "
                    "VALUES (?, ?)",
                    [(artist_mbid_lower, rg) for rg in rg_mbids_lower],
                )
            conn.execute(
                """
                INSERT INTO artist_release_check (artist_mbid_lower, last_checked_at, last_status, last_error)
                VALUES (?, ?, 'ok', NULL)
                ON CONFLICT(artist_mbid_lower) DO UPDATE SET
                    last_checked_at = excluded.last_checked_at,
                    last_status = excluded.last_status,
                    last_error = excluded.last_error
                """,
                (artist_mbid_lower, now),
            )

        await self._write(operation)

    async def known_release_set(self, artist_mbid_lower: str) -> set[str]:
        def operation(conn: sqlite3.Connection) -> set[str]:
            rows = conn.execute(
                "SELECT rg_mbid_lower FROM artist_known_releases WHERE artist_mbid_lower = ?",
                (artist_mbid_lower,),
            ).fetchall()
            return {row["rg_mbid_lower"] for row in rows}

        return await self._read(operation)

    async def record_new_releases(
        self,
        artist_mbid_lower: str,
        feed_rows: list[NewReleaseInput],
        known_rg_lowers: list[str],
    ) -> None:
        # known_rg_lowers is a subset of feed_rows: a future-dated release is
        # added to the feed but left OUT of the known set so a later poll on/after
        # its release date can still detect and auto-enqueue it (DD4).
        # INSERT OR IGNORE on the feed PK makes overlapping poll runs idempotent.
        if not feed_rows and not known_rg_lowers:
            return
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            if known_rg_lowers:
                conn.executemany(
                    "INSERT OR IGNORE INTO artist_known_releases (artist_mbid_lower, rg_mbid_lower) "
                    "VALUES (?, ?)",
                    [(artist_mbid_lower, rg) for rg in known_rg_lowers],
                )
            if feed_rows:
                conn.executemany(
                    """
                    INSERT OR IGNORE INTO new_release_feed (
                        release_group_mbid_lower, release_group_mbid, artist_mbid_lower,
                        artist_name, title, primary_type, secondary_types, first_release_date, discovered_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    [
                        (
                            r.release_group_mbid_lower,
                            r.release_group_mbid,
                            r.artist_mbid_lower,
                            r.artist_name,
                            r.title,
                            r.primary_type,
                            r.secondary_types,
                            r.first_release_date,
                            now,
                        )
                        for r in feed_rows
                    ],
                )

        await self._write(operation)

    async def update_cursor(
        self, artist_mbid_lower: str, status: str, error: str | None = None
    ) -> None:
        # updates an EXISTING cursor row only; does nothing if no baseline exists
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE artist_release_check SET last_checked_at = ?, last_status = ?, last_error = ? "
                "WHERE artist_mbid_lower = ?",
                (now, status, error, artist_mbid_lower),
            )

        await self._write(operation)

    async def list_followers(self, artist_mbid_lower: str) -> list[str]:
        """Every user following the artist (the events watcher's SSE fan-out
        audience) - unlike list_auto_download_followers, no approval gating."""

        def operation(conn: sqlite3.Connection) -> list[str]:
            rows = conn.execute(
                "SELECT user_id FROM user_followed_artists WHERE artist_mbid_lower = ?"
                " ORDER BY user_id",
                (artist_mbid_lower,),
            ).fetchall()
            return [row["user_id"] for row in rows]

        return await self._read(operation)

    async def list_auto_download_followers(self, artist_mbid_lower: str) -> list[str]:
        # auto_download intent on AND (admin role OR approved standing grant).
        # admins are granted by role with no approval row (DD3) so a later
        # demotion correctly drops them. ordered by user_id so the poller's
        # owner pick is deterministic.

        def operation(conn: sqlite3.Connection) -> list[str]:
            rows = conn.execute(
                """
                SELECT ufa.user_id AS user_id
                FROM user_followed_artists ufa
                JOIN auth_users au ON au.id = ufa.user_id
                LEFT JOIN auto_download_approvals ada
                    ON ada.user_id = ufa.user_id AND ada.artist_mbid_lower = ufa.artist_mbid_lower
                WHERE ufa.artist_mbid_lower = ?
                  AND ufa.auto_download = 1
                  AND (au.role = 'admin' OR ada.state = 'approved')
                ORDER BY ufa.user_id
                """,
                (artist_mbid_lower,),
            ).fetchall()
            return [row["user_id"] for row in rows]

        return await self._read(operation)

    async def list_new_releases_for_user(
        self, user_id: str, limit: int, offset: int
    ) -> tuple[list[NewRelease], int]:
        safe_limit = max(1, limit)
        safe_offset = max(0, offset)

        def operation(conn: sqlite3.Connection) -> tuple[list[sqlite3.Row], int]:
            where = """
                FROM new_release_feed nrf
                JOIN user_followed_artists ufa
                    ON ufa.artist_mbid_lower = nrf.artist_mbid_lower AND ufa.user_id = ?
                WHERE nrf.release_group_mbid_lower NOT IN (
                    SELECT lower(release_group_mbid) FROM library_files
                    WHERE release_group_mbid IS NOT NULL AND deleted_at IS NULL
                )
            """
            total = conn.execute("SELECT COUNT(*) AS c " + where, (user_id,)).fetchone()["c"]
            rows = conn.execute(
                "SELECT nrf.release_group_mbid AS release_group_mbid, "
                "ufa.artist_mbid AS artist_mbid, nrf.artist_name AS artist_name, "
                "nrf.title AS title, nrf.primary_type AS primary_type, "
                "nrf.secondary_types AS secondary_types, "
                "nrf.first_release_date AS first_release_date, "
                "nrf.discovered_at AS discovered_at "
                + where
                + " ORDER BY nrf.first_release_date DESC, nrf.discovered_at DESC LIMIT ? OFFSET ?",
                (user_id, safe_limit, safe_offset),
            ).fetchall()
            return rows, total

        rows, total = await self._read(operation)
        items = [
            NewRelease(
                release_group_mbid=row["release_group_mbid"],
                artist_mbid=row["artist_mbid"],
                artist_name=row["artist_name"],
                title=row["title"],
                primary_type=row["primary_type"],
                secondary_types=row["secondary_types"],
                first_release_date=row["first_release_date"],
                discovered_at=row["discovered_at"],
            )
            for row in rows
        ]
        return items, total

    async def list_recent_releases_for_user(
        self, user_id: str, days: int, limit: int, include_owned: bool = True
    ) -> tuple[list[NewRelease], int]:
        """The LOG view: everything the user's artists released in the last
        ``days`` days, INCLUDING albums already in the library (flagged via
        ``in_library``) unless ``include_owned`` is False (the page's
        'hide owned' filter). Dateless rows fall back to their discovery time.
        Unlike list_new_releases_for_user this is a record of what happened,
        not a to-do list."""
        safe_limit = max(1, limit)
        cutoff_date = (date.today() - timedelta(days=max(1, days))).isoformat()
        cutoff_ts = time.time() - max(1, days) * 86400

        def operation(conn: sqlite3.Connection) -> tuple[list[sqlite3.Row], int]:
            where = """
                FROM new_release_feed nrf
                JOIN user_followed_artists ufa
                    ON ufa.artist_mbid_lower = nrf.artist_mbid_lower AND ufa.user_id = ?
                WHERE (
                    nrf.first_release_date >= ?
                    OR (nrf.first_release_date IS NULL AND nrf.discovered_at >= ?)
                )
            """
            if not include_owned:
                where += """
                AND nrf.release_group_mbid_lower NOT IN (
                    SELECT lower(release_group_mbid) FROM library_files
                    WHERE release_group_mbid IS NOT NULL AND deleted_at IS NULL
                )
                """
            params = (user_id, cutoff_date, cutoff_ts)
            total = conn.execute("SELECT COUNT(*) AS c " + where, params).fetchone()["c"]
            rows = conn.execute(
                "SELECT nrf.release_group_mbid AS release_group_mbid, "
                "ufa.artist_mbid AS artist_mbid, nrf.artist_name AS artist_name, "
                "nrf.title AS title, nrf.primary_type AS primary_type, "
                "nrf.secondary_types AS secondary_types, "
                "nrf.first_release_date AS first_release_date, "
                "nrf.discovered_at AS discovered_at, "
                "EXISTS (SELECT 1 FROM library_files lf "
                "        WHERE lower(lf.release_group_mbid) = nrf.release_group_mbid_lower "
                "          AND lf.deleted_at IS NULL) AS in_library "
                + where
                + " ORDER BY nrf.first_release_date DESC, nrf.discovered_at DESC LIMIT ?",
                (*params, safe_limit),
            ).fetchall()
            return rows, total

        rows, total = await self._read(operation)
        items = [
            NewRelease(
                release_group_mbid=row["release_group_mbid"],
                artist_mbid=row["artist_mbid"],
                artist_name=row["artist_name"],
                title=row["title"],
                primary_type=row["primary_type"],
                secondary_types=row["secondary_types"],
                first_release_date=row["first_release_date"],
                discovered_at=row["discovered_at"],
                in_library=bool(row["in_library"]),
            )
            for row in rows
        ]
        return items, total

    async def count_unseen_new_releases_for_user(self, user_id: str) -> int:
        # same visibility filters as list_new_releases_for_user, narrowed to
        # rows discovered after the seen marker; no marker row counts everything

        def operation(conn: sqlite3.Connection) -> int:
            return conn.execute(
                """
                SELECT COUNT(*) AS c
                FROM new_release_feed nrf
                JOIN user_followed_artists ufa
                    ON ufa.artist_mbid_lower = nrf.artist_mbid_lower AND ufa.user_id = ?
                WHERE nrf.release_group_mbid_lower NOT IN (
                    SELECT lower(release_group_mbid) FROM library_files
                    WHERE release_group_mbid IS NOT NULL AND deleted_at IS NULL
                )
                AND nrf.discovered_at > COALESCE(
                    (SELECT seen_at FROM user_new_release_seen WHERE user_id = ?), 0
                )
                """,
                (user_id, user_id),
            ).fetchone()["c"]

        return await self._read(operation)

    async def mark_new_releases_seen(self, user_id: str) -> None:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO user_new_release_seen (user_id, seen_at) VALUES (?, ?)
                ON CONFLICT(user_id) DO UPDATE SET seen_at = excluded.seen_at
                """,
                (user_id, now),
            )

        await self._write(operation)
