import asyncio
import logging
import sqlite3
import threading
from datetime import datetime, timezone
from pathlib import Path

import msgspec

logger = logging.getLogger(__name__)

_REIMPORTABLE_CONDITION = (
    "status = 'failed'"
    " AND download_task_id IS NOT NULL"
    " AND EXISTS ("
    "SELECT 1 FROM download_tasks"
    " WHERE download_tasks.id = request_history.download_task_id"
    " AND download_tasks.status IN ('failed', 'partial')"
    " AND download_tasks.source_username IS NOT NULL"
    " AND download_tasks.search_job_id IS NOT NULL"
    " AND download_tasks.candidate_index IS NOT NULL"
    ")"
)

_REIMPORTABLE_JOIN_CONDITION = (
    "rh.status = 'failed'"
    " AND rh.download_task_id IS NOT NULL"
    " AND EXISTS ("
    "SELECT 1 FROM download_tasks"
    " WHERE download_tasks.id = rh.download_task_id"
    " AND download_tasks.status IN ('failed', 'partial')"
    " AND download_tasks.source_username IS NOT NULL"
    " AND download_tasks.search_job_id IS NOT NULL"
    " AND download_tasks.candidate_index IS NOT NULL"
    ")"
)


class RequestHistoryRecord(msgspec.Struct):
    musicbrainz_id: str
    artist_name: str
    album_title: str
    requested_at: str
    status: str
    artist_mbid: str | None = None
    year: int | None = None
    cover_url: str | None = None
    completed_at: str | None = None
    download_task_id: str | None = None
    monitor_artist: bool = False
    auto_download_artist: bool = False
    user_id: str | None = None
    requested_by_name: str | None = None
    release_mbid: str | None = None
    reviewed_by_id: str | None = None
    reviewed_by_name: str | None = None
    reviewed_at: str | None = None
    request_kind: str = "album"
    track_title: str | None = None
    duration_seconds: int | None = None
    track_release_group_mbid: str | None = None


class RequestHistoryStore:
    _ACTIVE_STATUSES = ("pending", "downloading")
    # Statuses a non-admin user sees in their "active" view (includes awaiting approval)
    _USER_ACTIVE_STATUSES = ("pending", "downloading", "awaiting_approval", "queued")

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
        return conn

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS request_history (
                    musicbrainz_id_lower TEXT PRIMARY KEY,
                    musicbrainz_id TEXT NOT NULL,
                    artist_name TEXT NOT NULL,
                    album_title TEXT NOT NULL,
                    artist_mbid TEXT,
                    year INTEGER,
                    cover_url TEXT,
                    requested_at TEXT NOT NULL,
                    completed_at TEXT,
                    status TEXT NOT NULL,
                    monitor_artist INTEGER NOT NULL DEFAULT 0,
                    auto_download_artist INTEGER NOT NULL DEFAULT 0
                )
                """
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_request_history_status_requested_at ON request_history(status, requested_at DESC)"
            )
            for col, definition in [
                ("monitor_artist", "INTEGER NOT NULL DEFAULT 0"),
                ("auto_download_artist", "INTEGER NOT NULL DEFAULT 0"),
                ("user_id", "TEXT"),
                ("requested_by_name", "TEXT"),
                ("reviewed_by_id", "TEXT"),
                ("reviewed_by_name", "TEXT"),
                ("reviewed_at", "TEXT"),
                ("download_task_id", "TEXT"),
                ("release_mbid", "TEXT"),
                ("request_kind", "TEXT NOT NULL DEFAULT 'album'"),
                ("track_title", "TEXT"),
                ("duration_seconds", "INTEGER"),
                ("track_release_group_mbid", "TEXT"),
            ]:
                try:
                    conn.execute(
                        f"ALTER TABLE request_history ADD COLUMN {col} {definition}"
                    )
                except sqlite3.OperationalError as e:
                    if "duplicate column" not in str(e).lower():
                        logger.warning("Unexpected error adding column %s: %s", col, e)
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS request_history_dismissals (
                    user_id TEXT NOT NULL,
                    musicbrainz_id_lower TEXT NOT NULL,
                    dismissed_at TEXT NOT NULL DEFAULT (datetime('now')),
                    PRIMARY KEY (user_id, musicbrainz_id_lower)
                )
                """
            )
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS request_history_requesters (
                    user_id TEXT NOT NULL,
                    musicbrainz_id_lower TEXT NOT NULL,
                    requested_at TEXT NOT NULL,
                    requested_by_name TEXT,
                    PRIMARY KEY (user_id, musicbrainz_id_lower)
                )
                """
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_request_history_requesters_mbid "
                "ON request_history_requesters(musicbrainz_id_lower)"
            )
            # Preserve ownership of rows created before multi-listener attribution.
            conn.execute(
                """
                INSERT OR IGNORE INTO request_history_requesters (
                    user_id, musicbrainz_id_lower, requested_at, requested_by_name
                )
                SELECT user_id, musicbrainz_id_lower, requested_at, requested_by_name
                FROM request_history WHERE user_id IS NOT NULL
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

    @staticmethod
    def _row_to_record(row: sqlite3.Row | None) -> RequestHistoryRecord | None:
        if row is None:
            return None
        keys = row.keys()
        return RequestHistoryRecord(
            musicbrainz_id=row["musicbrainz_id"],
            artist_name=row["artist_name"],
            album_title=row["album_title"],
            artist_mbid=row["artist_mbid"],
            year=row["year"],
            cover_url=row["cover_url"],
            requested_at=(
                row["requester_requested_at"]
                if "requester_requested_at" in keys
                else row["requested_at"]
            ),
            completed_at=row["completed_at"],
            status=row["status"],
            download_task_id=row["download_task_id"]
            if "download_task_id" in keys
            else None,
            monitor_artist=bool(row["monitor_artist"])
            if row["monitor_artist"] is not None
            else False,
            auto_download_artist=bool(row["auto_download_artist"])
            if row["auto_download_artist"] is not None
            else False,
            user_id=(
                row["requester_user_id"]
                if "requester_user_id" in keys
                else (row["user_id"] if "user_id" in keys else None)
            ),
            requested_by_name=(
                row["requester_name"]
                if "requester_name" in keys
                else (row["requested_by_name"] if "requested_by_name" in keys else None)
            ),
            release_mbid=row["release_mbid"] if "release_mbid" in keys else None,
            reviewed_by_id=row["reviewed_by_id"] if "reviewed_by_id" in keys else None,
            reviewed_by_name=row["reviewed_by_name"]
            if "reviewed_by_name" in keys
            else None,
            reviewed_at=row["reviewed_at"] if "reviewed_at" in keys else None,
            request_kind=(row["request_kind"] if "request_kind" in keys else "album")
            or "album",
            track_title=row["track_title"] if "track_title" in keys else None,
            duration_seconds=(
                row["duration_seconds"] if "duration_seconds" in keys else None
            ),
            track_release_group_mbid=(
                row["track_release_group_mbid"]
                if "track_release_group_mbid" in keys
                else None
            ),
        )

    async def async_record_request(
        self,
        musicbrainz_id: str,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        cover_url: str | None = None,
        artist_mbid: str | None = None,
        monitor_artist: bool = False,
        auto_download_artist: bool = False,
        user_id: str | None = None,
        requested_by_name: str | None = None,
        release_mbid: str | None = None,
        initial_status: str = "pending",
        request_kind: str = "album",
        track_title: str | None = None,
        duration_seconds: int | None = None,
        track_release_group_mbid: str | None = None,
    ) -> None:
        requested_at = datetime.now(timezone.utc).isoformat()
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO request_history (
                    musicbrainz_id_lower, musicbrainz_id, artist_name, album_title,
                    artist_mbid, year, cover_url, requested_at, completed_at, status,
                    monitor_artist, auto_download_artist, user_id, requested_by_name,
                    release_mbid, request_kind, track_title, duration_seconds,
                    track_release_group_mbid
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(musicbrainz_id_lower) DO UPDATE SET
                    musicbrainz_id = excluded.musicbrainz_id,
                    artist_name = excluded.artist_name,
                    album_title = excluded.album_title,
                    artist_mbid = excluded.artist_mbid,
                    year = excluded.year,
                    cover_url = COALESCE(excluded.cover_url, request_history.cover_url),
                    requested_at = excluded.requested_at,
                    completed_at = NULL,
                    status = excluded.status,
                    monitor_artist = excluded.monitor_artist,
                    auto_download_artist = excluded.auto_download_artist,
                    user_id = COALESCE(excluded.user_id, request_history.user_id),
                    requested_by_name = COALESCE(excluded.requested_by_name, request_history.requested_by_name),
                    release_mbid = excluded.release_mbid,
                    request_kind = excluded.request_kind,
                    track_title = excluded.track_title,
                    duration_seconds = excluded.duration_seconds,
                    track_release_group_mbid = excluded.track_release_group_mbid
                """,
                (
                    normalized_mbid,
                    musicbrainz_id,
                    artist_name,
                    album_title,
                    artist_mbid,
                    year,
                    cover_url,
                    requested_at,
                    initial_status,
                    int(monitor_artist),
                    int(auto_download_artist),
                    user_id,
                    requested_by_name,
                    release_mbid,
                    request_kind,
                    track_title,
                    duration_seconds,
                    track_release_group_mbid,
                ),
            )
            if user_id is not None:
                conn.execute(
                    """
                    INSERT INTO request_history_requesters (
                        user_id, musicbrainz_id_lower, requested_at, requested_by_name
                    ) VALUES (?, ?, ?, ?)
                    ON CONFLICT (user_id, musicbrainz_id_lower) DO UPDATE SET
                        requested_at = excluded.requested_at,
                        requested_by_name = COALESCE(
                            excluded.requested_by_name,
                            request_history_requesters.requested_by_name
                        )
                    """,
                    (user_id, normalized_mbid, requested_at, requested_by_name),
                )

        await self._write(operation)

    async def async_bulk_record_requests(
        self,
        items: list[dict],
        monitor_artist: bool = False,
        auto_download_artist: bool = False,
        user_id: str | None = None,
        requested_by_name: str | None = None,
        initial_status: str = "pending",
    ) -> int:
        """Bulk insert/upsert request history records in a single transaction. Returns count inserted."""
        requested_at = datetime.now(timezone.utc).isoformat()
        monitor_int = int(monitor_artist)
        auto_download_int = int(auto_download_artist)

        rows = [
            (
                item["musicbrainz_id"].lower(),
                item["musicbrainz_id"],
                item.get("artist_name", "Unknown"),
                item.get("album_title", "Unknown"),
                item.get("artist_mbid"),
                item.get("year"),
                item.get("cover_url"),
                requested_at,
                initial_status,
                monitor_int,
                auto_download_int,
                user_id,
                requested_by_name,
                item.get("release_mbid"),
            )
            for item in items
        ]

        def operation(conn: sqlite3.Connection) -> int:
            conn.executemany(
                """
                INSERT INTO request_history (
                    musicbrainz_id_lower, musicbrainz_id, artist_name, album_title,
                    artist_mbid, year, cover_url, requested_at, completed_at, status,
                    monitor_artist, auto_download_artist, user_id, requested_by_name,
                    release_mbid
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(musicbrainz_id_lower) DO UPDATE SET
                    musicbrainz_id = excluded.musicbrainz_id,
                    artist_name = excluded.artist_name,
                    album_title = excluded.album_title,
                    artist_mbid = excluded.artist_mbid,
                    year = excluded.year,
                    cover_url = COALESCE(excluded.cover_url, request_history.cover_url),
                    requested_at = excluded.requested_at,
                    completed_at = NULL,
                    status = excluded.status,
                    monitor_artist = excluded.monitor_artist,
                    auto_download_artist = excluded.auto_download_artist,
                    user_id = COALESCE(excluded.user_id, request_history.user_id),
                    requested_by_name = COALESCE(excluded.requested_by_name, request_history.requested_by_name),
                    release_mbid = excluded.release_mbid
                """,
                rows,
            )
            if user_id is not None:
                conn.executemany(
                    """
                    INSERT INTO request_history_requesters (
                        user_id, musicbrainz_id_lower, requested_at, requested_by_name
                    ) VALUES (?, ?, ?, ?)
                    ON CONFLICT (user_id, musicbrainz_id_lower) DO UPDATE SET
                        requested_at = excluded.requested_at,
                        requested_by_name = COALESCE(
                            excluded.requested_by_name,
                            request_history_requesters.requested_by_name
                        )
                    """,
                    [
                        (
                            user_id,
                            item["musicbrainz_id"].lower(),
                            requested_at,
                            requested_by_name,
                        )
                        for item in items
                    ],
                )
            return len(rows)

        return await self._write(operation)

    async def async_add_requester(
        self,
        musicbrainz_id: str,
        user_id: str | None,
        requested_by_name: str | None = None,
    ) -> None:
        if user_id is None:
            return
        await self.async_add_requesters(
            [musicbrainz_id], user_id, requested_by_name=requested_by_name
        )

    async def async_add_requesters(
        self,
        musicbrainz_ids: list[str],
        user_id: str | None,
        requested_by_name: str | None = None,
    ) -> None:
        if user_id is None:
            return
        requested_at = datetime.now(timezone.utc).isoformat()
        normalized = list(
            dict.fromkeys(value.casefold() for value in musicbrainz_ids if value)
        )
        if not normalized:
            return

        def operation(conn: sqlite3.Connection) -> None:
            conn.executemany(
                """
                INSERT INTO request_history_requesters (
                    user_id, musicbrainz_id_lower, requested_at, requested_by_name
                ) VALUES (?, ?, ?, ?)
                ON CONFLICT (user_id, musicbrainz_id_lower) DO UPDATE SET
                    requested_at = excluded.requested_at,
                    requested_by_name = COALESCE(
                        excluded.requested_by_name,
                        request_history_requesters.requested_by_name
                    )
                """,
                [
                    (user_id, musicbrainz_id, requested_at, requested_by_name)
                    for musicbrainz_id in normalized
                ],
            )

        await self._write(operation)

    async def async_is_requester(self, user_id: str, musicbrainz_id: str) -> bool:
        normalized_mbid = musicbrainz_id.casefold()

        def operation(conn: sqlite3.Connection) -> bool:
            row = conn.execute(
                "SELECT 1 FROM request_history_requesters "
                "WHERE user_id = ? AND musicbrainz_id_lower = ?",
                (user_id, normalized_mbid),
            ).fetchone()
            return row is not None

        return await self._read(operation)

    async def async_requester_count(self, musicbrainz_id: str) -> int:
        normalized_mbid = musicbrainz_id.casefold()

        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                "SELECT COUNT(*) AS count FROM request_history_requesters "
                "WHERE musicbrainz_id_lower = ?",
                (normalized_mbid,),
            ).fetchone()
            return int(row["count"] if row is not None else 0)

        return await self._read(operation)

    async def async_remove_requester(self, user_id: str, musicbrainz_id: str) -> bool:
        """Remove only one listener's interest and transfer primary attribution."""
        normalized_mbid = musicbrainz_id.casefold()

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "DELETE FROM request_history_requesters "
                "WHERE user_id = ? AND musicbrainz_id_lower = ?",
                (user_id, normalized_mbid),
            )
            if cursor.rowcount <= 0:
                return False
            owner = conn.execute(
                "SELECT user_id FROM request_history " "WHERE musicbrainz_id_lower = ?",
                (normalized_mbid,),
            ).fetchone()
            if owner is not None and owner["user_id"] == user_id:
                successor = conn.execute(
                    "SELECT user_id, requested_by_name FROM request_history_requesters "
                    "WHERE musicbrainz_id_lower = ? ORDER BY requested_at ASC LIMIT 1",
                    (normalized_mbid,),
                ).fetchone()
                if successor is not None:
                    conn.execute(
                        "UPDATE request_history SET user_id = ?, requested_by_name = ? "
                        "WHERE musicbrainz_id_lower = ?",
                        (
                            successor["user_id"],
                            successor["requested_by_name"],
                            normalized_mbid,
                        ),
                    )
            return True

        return await self._write(operation)

    async def async_get_record(
        self, musicbrainz_id: str
    ) -> RequestHistoryRecord | None:
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> RequestHistoryRecord | None:
            row = conn.execute(
                "SELECT * FROM request_history WHERE musicbrainz_id_lower = ?",
                (normalized_mbid,),
            ).fetchone()
            return self._row_to_record(row)

        return await self._read(operation)

    async def async_canonicalize_known_release_aliases(
        self, source_mbids: list[str] | None = None
    ) -> int:
        status_rank = {
            "downloading": 5,
            "pending": 4,
            "queued": 3,
            "awaiting_approval": 2,
        }

        def operation(conn: sqlite3.Connection) -> int:
            has_map = conn.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' "
                "AND name = 'mbid_resolution_map'"
            ).fetchone()
            if has_map is None:
                return 0
            filters = (
                "release_group_mbid IS NOT NULL AND trim(release_group_mbid) != '' "
                "AND source_mbid_lower != lower(release_group_mbid)"
            )
            parameters: tuple[str, ...] = ()
            if source_mbids is not None:
                normalized = list(
                    dict.fromkeys(value.casefold() for value in source_mbids if value)
                )
                if not normalized:
                    return 0
                filters += (
                    f" AND source_mbid_lower IN ({','.join('?' for _ in normalized)})"
                )
                parameters = tuple(normalized)
            mappings = conn.execute(
                "SELECT source_mbid_lower, source_mbid, release_group_mbid "
                f"FROM mbid_resolution_map WHERE {filters} ORDER BY source_mbid_lower",
                parameters,
            ).fetchall()
            changed = 0
            for mapping in mappings:
                source_key = str(mapping["source_mbid_lower"])
                target_id = str(mapping["release_group_mbid"])
                target_key = target_id.casefold()
                source = conn.execute(
                    "SELECT * FROM request_history WHERE musicbrainz_id_lower = ?",
                    (source_key,),
                ).fetchone()
                if source is None:
                    continue
                target = conn.execute(
                    "SELECT * FROM request_history WHERE musicbrainz_id_lower = ?",
                    (target_key,),
                ).fetchone()
                source_wins = target is None or (
                    status_rank.get(str(source["status"]), 1),
                    source["download_task_id"] is not None,
                    str(source["requested_at"]),
                ) > (
                    status_rank.get(str(target["status"]), 1),
                    target["download_task_id"] is not None,
                    str(target["requested_at"]),
                )
                winner = source if source_wins else target
                loser = target if source_wins else source
                values = dict(winner)
                for column in (
                    "artist_mbid",
                    "year",
                    "cover_url",
                    "download_task_id",
                    "user_id",
                    "requested_by_name",
                    "reviewed_by_id",
                    "reviewed_by_name",
                    "reviewed_at",
                ):
                    if values[column] is None and loser is not None:
                        values[column] = loser[column]
                values["monitor_artist"] = int(
                    bool(source["monitor_artist"])
                    or bool(target and target["monitor_artist"])
                )
                values["auto_download_artist"] = int(
                    bool(source["auto_download_artist"])
                    or bool(target and target["auto_download_artist"])
                )
                if values["release_mbid"] is None:
                    values["release_mbid"] = (
                        source["release_mbid"] or mapping["source_mbid"]
                    )
                values["musicbrainz_id_lower"] = target_key
                values["musicbrainz_id"] = target_id
                conn.execute(
                    "INSERT OR IGNORE INTO request_history_dismissals "
                    "(user_id, musicbrainz_id_lower, dismissed_at) "
                    "SELECT user_id, ?, dismissed_at FROM request_history_dismissals "
                    "WHERE musicbrainz_id_lower = ?",
                    (target_key, source_key),
                )
                conn.execute(
                    "DELETE FROM request_history_dismissals WHERE musicbrainz_id_lower = ?",
                    (source_key,),
                )
                conn.execute(
                    "INSERT OR IGNORE INTO request_history_requesters "
                    "(user_id, musicbrainz_id_lower, requested_at, requested_by_name) "
                    "SELECT user_id, ?, requested_at, requested_by_name "
                    "FROM request_history_requesters WHERE musicbrainz_id_lower = ?",
                    (target_key, source_key),
                )
                conn.execute(
                    "DELETE FROM request_history_requesters "
                    "WHERE musicbrainz_id_lower = ?",
                    (source_key,),
                )
                conn.execute(
                    "DELETE FROM request_history WHERE musicbrainz_id_lower IN (?, ?)",
                    (source_key, target_key),
                )
                columns = list(values)
                conn.execute(
                    f"INSERT INTO request_history ({','.join(columns)}) "
                    f"VALUES ({','.join('?' for _ in columns)})",
                    tuple(values[column] for column in columns),
                )
                changed += 1
            return changed

        return await self._write(operation)

    async def async_update_monitoring_flags(
        self,
        musicbrainz_id: str,
        *,
        monitor_artist: bool,
        auto_download_artist: bool,
    ) -> None:
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE request_history SET monitor_artist = ?, auto_download_artist = ? WHERE musicbrainz_id_lower = ?",
                (int(monitor_artist), int(auto_download_artist), normalized_mbid),
            )

        await self._write(operation)

    async def async_get_active_mbids(self) -> set[str]:
        """Return the set of MBIDs with active (pending/downloading) requests."""

        def operation(conn: sqlite3.Connection) -> set[str]:
            rows = conn.execute(
                "SELECT musicbrainz_id_lower FROM request_history WHERE status IN (?, ?)",
                self._ACTIVE_STATUSES,
            ).fetchall()
            return {row["musicbrainz_id_lower"] for row in rows}

        return await self._read(operation)

    async def async_get_requested_mbids(self) -> set[str]:
        """Return every MBID that should still appear requested in the UI."""
        placeholders = ",".join("?" for _ in self._USER_ACTIVE_STATUSES)

        def operation(conn: sqlite3.Connection) -> set[str]:
            rows = conn.execute(
                "SELECT musicbrainz_id_lower FROM request_history "
                f"WHERE status IN ({placeholders})",
                self._USER_ACTIVE_STATUSES,
            ).fetchall()
            return {row["musicbrainz_id_lower"] for row in rows}

        return await self._read(operation)

    async def async_existing_requested_mbids(self, ids: list[str]) -> set[str]:
        """Return active request state only for the supplied release-group IDs."""
        normalized = list(
            dict.fromkeys(value.strip().casefold() for value in ids if value.strip())
        )
        if not normalized:
            return set()
        status_placeholders = ",".join("?" for _ in self._USER_ACTIVE_STATUSES)

        def operation(conn: sqlite3.Connection) -> set[str]:
            found: set[str] = set()
            for offset in range(0, len(normalized), 500):
                batch = normalized[offset : offset + 500]
                id_placeholders = ",".join("?" for _ in batch)
                rows = conn.execute(
                    "SELECT musicbrainz_id_lower FROM request_history "
                    f"WHERE musicbrainz_id_lower IN ({id_placeholders}) "
                    f"AND status IN ({status_placeholders})",
                    (*batch, *self._USER_ACTIVE_STATUSES),
                ).fetchall()
                found.update(str(row["musicbrainz_id_lower"]) for row in rows)
            return found

        return await self._read(operation)

    async def async_get_active_requests(self) -> list[RequestHistoryRecord]:
        def operation(conn: sqlite3.Connection) -> list[RequestHistoryRecord]:
            rows = conn.execute(
                "SELECT * FROM request_history WHERE status IN (?, ?) ORDER BY requested_at DESC",
                self._ACTIVE_STATUSES,
            ).fetchall()
            return [
                record
                for row in rows
                if (record := self._row_to_record(row)) is not None
            ]

        return await self._read(operation)

    async def async_get_active_count(self) -> int:
        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                "SELECT COUNT(*) AS count FROM request_history WHERE status IN (?, ?)",
                self._ACTIVE_STATUSES,
            ).fetchone()
            return int(row["count"] if row is not None else 0)

        return await self._read(operation)

    async def async_get_active_count_for_user(self, user_id: str) -> int:
        placeholders = ",".join("?" for _ in self._USER_ACTIVE_STATUSES)

        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                "SELECT COUNT(*) AS count FROM request_history AS rh "
                "JOIN request_history_requesters AS rr "
                "ON rr.musicbrainz_id_lower = rh.musicbrainz_id_lower "
                f"WHERE rr.user_id = ? AND rh.status IN ({placeholders})",
                (user_id, *self._USER_ACTIVE_STATUSES),
            ).fetchone()
            return int(row["count"] if row is not None else 0)

        return await self._read(operation)

    async def async_get_active_requests_for_user(
        self, user_id: str
    ) -> list[RequestHistoryRecord]:
        """Active requests for a specific user, includes awaiting_approval items."""
        placeholders = ",".join("?" for _ in self._USER_ACTIVE_STATUSES)

        def operation(conn: sqlite3.Connection) -> list[RequestHistoryRecord]:
            rows = conn.execute(
                "SELECT rh.*, rr.user_id AS requester_user_id, "
                "rr.requested_by_name AS requester_name, "
                "rr.requested_at AS requester_requested_at "
                "FROM request_history AS rh "
                "JOIN request_history_requesters AS rr "
                "ON rr.musicbrainz_id_lower = rh.musicbrainz_id_lower "
                f"WHERE rr.user_id = ? AND rh.status IN ({placeholders}) "
                "ORDER BY rr.requested_at DESC",
                (user_id, *self._USER_ACTIVE_STATUSES),
            ).fetchall()
            return [
                record
                for row in rows
                if (record := self._row_to_record(row)) is not None
            ]

        return await self._read(operation)

    async def async_get_pending_approvals(self) -> list[RequestHistoryRecord]:
        """All requests awaiting admin approval."""

        def operation(conn: sqlite3.Connection) -> list[RequestHistoryRecord]:
            rows = conn.execute(
                "SELECT * FROM request_history WHERE status = 'awaiting_approval' ORDER BY requested_at ASC",
            ).fetchall()
            return [
                record
                for row in rows
                if (record := self._row_to_record(row)) is not None
            ]

        return await self._read(operation)

    async def async_get_pending_approval_count(self) -> int:
        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                "SELECT COUNT(*) AS count FROM request_history WHERE status = 'awaiting_approval'",
            ).fetchone()
            return int(row["count"] if row is not None else 0)

        return await self._read(operation)

    async def async_count_user_requests_since(
        self, user_id: str, since_iso: str
    ) -> int:
        """Album asks by one user inside the rolling request-quota window (D9/D20).
        Any status counts - a pending/awaiting_approval ask is still an ask (that's
        the point of enforcing at submit). ``requested_at`` is ISO-8601 UTC, so a
        same-format string compare is chronological."""

        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                "SELECT COUNT(*) AS count FROM request_history "
                "WHERE user_id = ? AND requested_at >= ?",
                (user_id, since_iso),
            ).fetchone()
            return int(row["count"] if row is not None else 0)

        return await self._read(operation)

    async def async_record_review(
        self,
        musicbrainz_id: str,
        status: str,
        reviewed_by_id: str,
        reviewed_by_name: str | None,
        completed_at: str | None = None,
    ) -> None:
        normalized_mbid = musicbrainz_id.lower()
        reviewed_at = datetime.now(timezone.utc).isoformat()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                UPDATE request_history
                SET status = ?, completed_at = COALESCE(?, completed_at),
                    reviewed_by_id = ?, reviewed_by_name = ?, reviewed_at = ?
                WHERE musicbrainz_id_lower = ?
                """,
                (
                    status,
                    completed_at,
                    reviewed_by_id,
                    reviewed_by_name,
                    reviewed_at,
                    normalized_mbid,
                ),
            )

        await self._write(operation)

    async def async_get_history_for_user(
        self,
        user_id: str,
        page: int = 1,
        page_size: int = 20,
        status_filter: str | None = None,
        sort: str | None = None,
    ) -> tuple[list[RequestHistoryRecord], int]:
        """Paginated history for a specific user."""
        safe_page = max(page, 1)
        safe_page_size = max(page_size, 1)
        offset = (safe_page - 1) * safe_page_size

        _SORT_MAP = {
            "newest": "rr.requested_at DESC",
            "oldest": "rr.requested_at ASC",
            "status": "rh.status ASC, rr.requested_at DESC",
        }
        order_clause = _SORT_MAP.get(sort or "", "rr.requested_at DESC")

        def operation(
            conn: sqlite3.Connection,
        ) -> tuple[list[RequestHistoryRecord], int]:
            dismiss_clause = (
                "AND rh.musicbrainz_id_lower NOT IN "
                "(SELECT musicbrainz_id_lower FROM request_history_dismissals WHERE user_id = ?)"
            )
            if status_filter == "reimportable":
                where = f"WHERE rr.user_id = ? AND {_REIMPORTABLE_JOIN_CONDITION} {dismiss_clause}"
                params: tuple = (user_id, user_id)
            elif status_filter:
                where = f"WHERE rr.user_id = ? AND rh.status = ? {dismiss_clause}"
                params = (user_id, status_filter, user_id)
            else:
                where = f"WHERE rr.user_id = ? {dismiss_clause}"
                params = (user_id, user_id)

            total_row = conn.execute(
                "SELECT COUNT(*) AS count FROM request_history AS rh "
                "JOIN request_history_requesters AS rr "
                "ON rr.musicbrainz_id_lower = rh.musicbrainz_id_lower "
                f"{where}",
                params,
            ).fetchone()
            rows = conn.execute(
                "SELECT rh.*, rr.user_id AS requester_user_id, "
                "rr.requested_by_name AS requester_name, "
                "rr.requested_at AS requester_requested_at "
                "FROM request_history AS rh "
                "JOIN request_history_requesters AS rr "
                "ON rr.musicbrainz_id_lower = rh.musicbrainz_id_lower "
                f"{where} ORDER BY {order_clause} LIMIT ? OFFSET ?",
                params + (safe_page_size, offset),
            ).fetchall()
            records = [
                record
                for row in rows
                if (record := self._row_to_record(row)) is not None
            ]
            total = int(total_row["count"] if total_row is not None else 0)
            return records, total

        return await self._read(operation)

    async def async_get_history(
        self,
        page: int = 1,
        page_size: int = 20,
        status_filter: str | None = None,
        sort: str | None = None,
    ) -> tuple[list[RequestHistoryRecord], int]:
        safe_page = max(page, 1)
        safe_page_size = max(page_size, 1)
        offset = (safe_page - 1) * safe_page_size

        _SORT_MAP = {
            "newest": "requested_at DESC",
            "oldest": "requested_at ASC",
            "status": "status ASC, requested_at DESC",
        }
        order_clause = _SORT_MAP.get(sort or "", "requested_at DESC")

        def operation(
            conn: sqlite3.Connection,
        ) -> tuple[list[RequestHistoryRecord], int]:
            params: tuple[object, ...]
            if status_filter == "reimportable":
                where_clause = f"WHERE {_REIMPORTABLE_CONDITION}"
                params = ()
            elif status_filter:
                where_clause = "WHERE status = ?"
                params = (status_filter,)
            else:
                where_clause = ""
                params = ()

            total_row = conn.execute(
                f"SELECT COUNT(*) AS count FROM request_history {where_clause}",
                params,
            ).fetchone()
            rows = conn.execute(
                f"SELECT * FROM request_history {where_clause} ORDER BY {order_clause} LIMIT ? OFFSET ?",
                params + (safe_page_size, offset),
            ).fetchall()
            records = [
                record
                for row in rows
                if (record := self._row_to_record(row)) is not None
            ]
            total = int(total_row["count"] if total_row is not None else 0)
            return records, total

        return await self._read(operation)

    async def async_update_status(
        self,
        musicbrainz_id: str,
        status: str,
        completed_at: str | None = None,
    ) -> None:
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> None:
            if status in self._ACTIVE_STATUSES and completed_at is None:
                conn.execute(
                    "UPDATE request_history SET status = ?, completed_at = NULL WHERE musicbrainz_id_lower = ?",
                    (status, normalized_mbid),
                )
                return

            conn.execute(
                "UPDATE request_history SET status = ?, completed_at = COALESCE(?, completed_at) WHERE musicbrainz_id_lower = ?",
                (status, completed_at, normalized_mbid),
            )

        await self._write(operation)

    async def async_update_cover_url(self, musicbrainz_id: str, cover_url: str) -> None:
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE request_history SET cover_url = ? WHERE musicbrainz_id_lower = ?",
                (cover_url, normalized_mbid),
            )

        await self._write(operation)

    async def async_update_artist_mbid(
        self, musicbrainz_id: str, artist_mbid: str
    ) -> None:
        """Backfill the artist MBID without resetting other fields."""
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE request_history SET artist_mbid = ? WHERE musicbrainz_id_lower = ? AND (artist_mbid IS NULL OR artist_mbid = '')",
                (artist_mbid, normalized_mbid),
            )

        await self._write(operation)

    async def async_update_download_task_id(
        self, musicbrainz_id: str, download_task_id: str
    ) -> None:
        """Link a request to its native download task (Q5-A), set at task creation.
        Mirrors ``async_update_cover_url`` / ``async_update_artist_mbid``."""
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE request_history SET download_task_id = ? WHERE musicbrainz_id_lower = ?",
                (download_task_id, normalized_mbid),
            )

        await self._write(operation)

    async def async_get_record_by_download_task_id(
        self, download_task_id: str
    ) -> RequestHistoryRecord | None:
        """Resolve the request that owns a native task.

        Album requests historically used the release-group MBID as their key.
        Exact-track requests use the recording MBID, so task ownership is the
        only identifier that is correct for both request kinds.
        """

        def operation(conn: sqlite3.Connection) -> RequestHistoryRecord | None:
            row = conn.execute(
                "SELECT * FROM request_history WHERE download_task_id = ? LIMIT 1",
                (download_task_id,),
            ).fetchone()
            return self._row_to_record(row)

        return await self._read(operation)

    async def async_delete_record(self, musicbrainz_id: str) -> bool:
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "DELETE FROM request_history WHERE musicbrainz_id_lower = ?",
                (normalized_mbid,),
            )
            conn.execute(
                "DELETE FROM request_history_dismissals WHERE musicbrainz_id_lower = ?",
                (normalized_mbid,),
            )
            conn.execute(
                "DELETE FROM request_history_requesters WHERE musicbrainz_id_lower = ?",
                (normalized_mbid,),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    async def async_dismiss_record(self, user_id: str, musicbrainz_id: str) -> bool:
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> bool:
            record = conn.execute(
                "SELECT musicbrainz_id_lower FROM request_history WHERE musicbrainz_id_lower = ?",
                (normalized_mbid,),
            ).fetchone()
            if record is None:
                return False
            conn.execute(
                """
                INSERT INTO request_history_dismissals (user_id, musicbrainz_id_lower)
                VALUES (?, ?)
                ON CONFLICT (user_id, musicbrainz_id_lower) DO NOTHING
                """,
                (user_id, normalized_mbid),
            )
            return True

        return await self._write(operation)

    async def prune_old_terminal_requests(self, days: int) -> int:
        """Delete terminal requests older than `days` days. Active requests are never
        touched. Rows with a live wanted watch (state watching/dormant) are kept even
        past retention - pruning them would orphan the watch and break its status-flip
        linkage (Wanted plan §4.4). ``wanted_watches`` lives in the same database file;
        a DB from before the table existed falls back to the unguarded delete."""
        import time as _time
        from datetime import timezone

        cutoff_iso = datetime.fromtimestamp(
            _time.time() - days * 86400, tz=timezone.utc
        ).isoformat()
        terminal_statuses = (
            "imported",
            "failed",
            "cancelled",
            "incomplete",
            "rejected",
        )
        base = (
            f"DELETE FROM request_history WHERE status IN ({','.join('?' for _ in terminal_statuses)}) "
            "AND COALESCE(completed_at, requested_at) < ?"
        )
        watch_guard = (
            " AND musicbrainz_id_lower NOT IN (SELECT release_group_mbid_lower"
            " FROM wanted_watches WHERE state IN ('watching','dormant'))"
        )

        def operation(conn: sqlite3.Connection) -> int:
            try:
                cursor = conn.execute(
                    base + watch_guard, (*terminal_statuses, cutoff_iso)
                )
            except sqlite3.OperationalError:
                # no wanted_watches table yet (fresh DB before the store's first
                # construction) - nothing can be watched, prune unguarded
                cursor = conn.execute(base, (*terminal_statuses, cutoff_iso))
            conn.execute(
                "DELETE FROM request_history_requesters WHERE musicbrainz_id_lower "
                "NOT IN (SELECT musicbrainz_id_lower FROM request_history)"
            )
            return cursor.rowcount

        return await self._write(operation)
