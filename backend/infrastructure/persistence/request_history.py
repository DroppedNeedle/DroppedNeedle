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
    reviewed_by_id: str | None = None
    reviewed_by_name: str | None = None
    reviewed_at: str | None = None


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
            ]:
                try:
                    conn.execute(f"ALTER TABLE request_history ADD COLUMN {col} {definition}")
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
            requested_at=row["requested_at"],
            completed_at=row["completed_at"],
            status=row["status"],
            download_task_id=row["download_task_id"] if "download_task_id" in keys else None,
            monitor_artist=bool(row["monitor_artist"]) if row["monitor_artist"] is not None else False,
            auto_download_artist=bool(row["auto_download_artist"]) if row["auto_download_artist"] is not None else False,
            user_id=row["user_id"] if "user_id" in keys else None,
            requested_by_name=row["requested_by_name"] if "requested_by_name" in keys else None,
            reviewed_by_id=row["reviewed_by_id"] if "reviewed_by_id" in keys else None,
            reviewed_by_name=row["reviewed_by_name"] if "reviewed_by_name" in keys else None,
            reviewed_at=row["reviewed_at"] if "reviewed_at" in keys else None,
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
        initial_status: str = "pending",
    ) -> None:
        requested_at = datetime.now(timezone.utc).isoformat()
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO request_history (
                    musicbrainz_id_lower, musicbrainz_id, artist_name, album_title,
                    artist_mbid, year, cover_url, requested_at, completed_at, status,
                    monitor_artist, auto_download_artist, user_id, requested_by_name
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?)
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
                    requested_by_name = COALESCE(excluded.requested_by_name, request_history.requested_by_name)
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
                ),
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
            )
            for item in items
        ]

        def operation(conn: sqlite3.Connection) -> int:
            conn.executemany(
                """
                INSERT INTO request_history (
                    musicbrainz_id_lower, musicbrainz_id, artist_name, album_title,
                    artist_mbid, year, cover_url, requested_at, completed_at, status,
                    monitor_artist, auto_download_artist, user_id, requested_by_name
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?)
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
                    requested_by_name = COALESCE(excluded.requested_by_name, request_history.requested_by_name)
                """,
                rows,
            )
            return len(rows)

        return await self._write(operation)

    async def async_get_record(self, musicbrainz_id: str) -> RequestHistoryRecord | None:
        normalized_mbid = musicbrainz_id.lower()

        def operation(conn: sqlite3.Connection) -> RequestHistoryRecord | None:
            row = conn.execute(
                "SELECT * FROM request_history WHERE musicbrainz_id_lower = ?",
                (normalized_mbid,),
            ).fetchone()
            return self._row_to_record(row)

        return await self._read(operation)

    async def async_update_monitoring_flags(
        self, musicbrainz_id: str, *, monitor_artist: bool, auto_download_artist: bool,
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

    async def async_get_active_requests(self) -> list[RequestHistoryRecord]:
        def operation(conn: sqlite3.Connection) -> list[RequestHistoryRecord]:
            rows = conn.execute(
                "SELECT * FROM request_history WHERE status IN (?, ?) ORDER BY requested_at DESC",
                self._ACTIVE_STATUSES,
            ).fetchall()
            return [record for row in rows if (record := self._row_to_record(row)) is not None]

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
                f"SELECT COUNT(*) AS count FROM request_history WHERE user_id = ? AND status IN ({placeholders})",
                (user_id, *self._USER_ACTIVE_STATUSES),
            ).fetchone()
            return int(row["count"] if row is not None else 0)

        return await self._read(operation)

    async def async_get_active_requests_for_user(self, user_id: str) -> list[RequestHistoryRecord]:
        """Active requests for a specific user, includes awaiting_approval items."""
        placeholders = ",".join("?" for _ in self._USER_ACTIVE_STATUSES)

        def operation(conn: sqlite3.Connection) -> list[RequestHistoryRecord]:
            rows = conn.execute(
                f"SELECT * FROM request_history WHERE user_id = ? AND status IN ({placeholders}) ORDER BY requested_at DESC",
                (user_id, *self._USER_ACTIVE_STATUSES),
            ).fetchall()
            return [record for row in rows if (record := self._row_to_record(row)) is not None]

        return await self._read(operation)

    async def async_get_pending_approvals(self) -> list[RequestHistoryRecord]:
        """All requests awaiting admin approval."""
        def operation(conn: sqlite3.Connection) -> list[RequestHistoryRecord]:
            rows = conn.execute(
                "SELECT * FROM request_history WHERE status = 'awaiting_approval' ORDER BY requested_at ASC",
            ).fetchall()
            return [record for row in rows if (record := self._row_to_record(row)) is not None]

        return await self._read(operation)

    async def async_get_pending_approval_count(self) -> int:
        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                "SELECT COUNT(*) AS count FROM request_history WHERE status = 'awaiting_approval'",
            ).fetchone()
            return int(row["count"] if row is not None else 0)

        return await self._read(operation)

    async def async_count_user_requests_since(self, user_id: str, since_iso: str) -> int:
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
                (status, completed_at, reviewed_by_id, reviewed_by_name, reviewed_at, normalized_mbid),
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
            "newest": "requested_at DESC",
            "oldest": "requested_at ASC",
            "status": "status ASC, requested_at DESC",
        }
        order_clause = _SORT_MAP.get(sort or "", "requested_at DESC")

        def operation(conn: sqlite3.Connection) -> tuple[list[RequestHistoryRecord], int]:
            dismiss_clause = (
                "AND musicbrainz_id_lower NOT IN "
                "(SELECT musicbrainz_id_lower FROM request_history_dismissals WHERE user_id = ?)"
            )
            if status_filter == 'reimportable':
                where = f"WHERE user_id = ? AND {_REIMPORTABLE_CONDITION} {dismiss_clause}"
                params: tuple = (user_id, user_id)
            elif status_filter:
                where = f"WHERE user_id = ? AND status = ? {dismiss_clause}"
                params = (user_id, status_filter, user_id)
            else:
                where = f"WHERE user_id = ? {dismiss_clause}"
                params = (user_id, user_id)

            total_row = conn.execute(
                f"SELECT COUNT(*) AS count FROM request_history {where}", params
            ).fetchone()
            rows = conn.execute(
                f"SELECT * FROM request_history {where} ORDER BY {order_clause} LIMIT ? OFFSET ?",
                params + (safe_page_size, offset),
            ).fetchall()
            records = [record for row in rows if (record := self._row_to_record(row)) is not None]
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

        def operation(conn: sqlite3.Connection) -> tuple[list[RequestHistoryRecord], int]:
            params: tuple[object, ...]
            if status_filter == 'reimportable':
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
            records = [record for row in rows if (record := self._row_to_record(row)) is not None]
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

    async def async_update_artist_mbid(self, musicbrainz_id: str, artist_mbid: str) -> None:
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
        """Delete terminal requests older than `days` days. Active requests are never touched."""
        import time as _time
        from datetime import timezone
        cutoff_iso = datetime.fromtimestamp(_time.time() - days * 86400, tz=timezone.utc).isoformat()
        terminal_statuses = ("imported", "failed", "cancelled", "incomplete", "rejected")

        def operation(conn: sqlite3.Connection) -> int:
            cursor = conn.execute(
                f"DELETE FROM request_history WHERE status IN ({','.join('?' for _ in terminal_statuses)}) "
                "AND COALESCE(completed_at, requested_at) < ?",
                (*terminal_statuses, cutoff_iso),
            )
            return cursor.rowcount

        return await self._write(operation)