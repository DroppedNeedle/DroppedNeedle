"""``FreeMusicStore`` - persistence for Free Music download tasks (D24).

Deliberately independent of ``DownloadStore``, which phase 02 deletes. One table
in the shared ``library.db``.
"""

import sqlite3
import threading
import time
from pathlib import Path

from infrastructure.persistence._database import PersistenceBase
from models.free_music import FreeMusicStatus, FreeMusicTask

_COLUMNS = (
    "id, user_id, kind, mbid, artist, title, status, identifier, licence_url, "
    "format, files_total, files_completed, bytes_total, bytes_downloaded, "
    "attempts, error, created_at, updated_at, track_count, origin, "
    "release_group_mbid, release_mbid, release_track_mbid, recording_mbid, "
    "duration_seconds, album_title, track_number, disc_number, "
    "quality_snapshot_json, quality_snapshot_hash, "
    "quality_snapshot_summary, tried_candidates_json"
)


# Quality-field writer whitelist (update_free_music_quality_fields): the
# pinned snapshot plus the persisted tried-candidate ladder.
_FREE_MUSIC_QUALITY_UPDATABLE = frozenset(
    {
        "quality_snapshot_json",
        "quality_snapshot_hash",
        "quality_snapshot_summary",
        "tried_candidates_json",
    }
)


def _row_to_task(row: sqlite3.Row) -> FreeMusicTask:
    return FreeMusicTask(
        id=row["id"],
        user_id=row["user_id"],
        kind=row["kind"],
        mbid=row["mbid"],
        artist=row["artist"],
        title=row["title"],
        status=row["status"],
        track_count=row["track_count"],
        identifier=row["identifier"] or "",
        licence_url=row["licence_url"] or "",
        format=row["format"] or "",
        files_total=row["files_total"],
        files_completed=row["files_completed"],
        bytes_total=row["bytes_total"],
        bytes_downloaded=row["bytes_downloaded"],
        attempts=row["attempts"],
        error=row["error"],
        origin=row["origin"] or "user",
        release_group_mbid=row["release_group_mbid"],
        release_mbid=row["release_mbid"],
        release_track_mbid=row["release_track_mbid"],
        recording_mbid=row["recording_mbid"],
        duration_seconds=row["duration_seconds"],
        album_title=row["album_title"],
        track_number=row["track_number"],
        disc_number=row["disc_number"],
        quality_snapshot_json=row["quality_snapshot_json"],
        quality_snapshot_hash=row["quality_snapshot_hash"],
        quality_snapshot_summary=row["quality_snapshot_summary"],
        tried_candidates_json=row["tried_candidates_json"] or "[]",
        created_at=row["created_at"],
        updated_at=row["updated_at"],
    )


class FreeMusicStore(PersistenceBase):
    def __init__(self, db_path: Path, write_lock: threading.Lock) -> None:
        super().__init__(db_path, write_lock)

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.executescript("""
                CREATE TABLE IF NOT EXISTS free_music_tasks (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK(kind IN ('album','track')),
                    mbid TEXT NOT NULL,
                    artist TEXT NOT NULL DEFAULT '',
                    title TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL CHECK(status IN
                        ('searching','downloading','importing','completed','failed','cancelled')),
                    identifier TEXT,
                    licence_url TEXT,
                    format TEXT,
                    files_total INTEGER NOT NULL DEFAULT 0,
                    files_completed INTEGER NOT NULL DEFAULT 0,
                    bytes_total INTEGER NOT NULL DEFAULT 0,
                    bytes_downloaded INTEGER NOT NULL DEFAULT 0,
                    attempts INTEGER NOT NULL DEFAULT 0,
                    error TEXT,
                    created_at REAL NOT NULL,
                    updated_at REAL NOT NULL,
                    track_count INTEGER NOT NULL DEFAULT 0 CHECK(track_count >= 0)
                    ,origin TEXT NOT NULL DEFAULT 'user'
                    ,release_group_mbid TEXT
                    ,release_mbid TEXT
                    ,release_track_mbid TEXT
                    ,recording_mbid TEXT
                    ,duration_seconds REAL
                    ,album_title TEXT
                    ,track_number INTEGER
                    ,disc_number INTEGER
                    ,quality_snapshot_json TEXT
                    ,quality_snapshot_hash TEXT
                    ,quality_snapshot_summary TEXT
                    ,tried_candidates_json TEXT NOT NULL DEFAULT '[]'
                );
                CREATE INDEX IF NOT EXISTS idx_free_music_user
                    ON free_music_tasks(user_id, created_at);
                CREATE INDEX IF NOT EXISTS idx_free_music_mbid
                    ON free_music_tasks(mbid);
            """)
            try:
                conn.execute(
                    "ALTER TABLE free_music_tasks ADD COLUMN track_count "
                    "INTEGER NOT NULL DEFAULT 0 CHECK(track_count >= 0)"
                )
            except sqlite3.OperationalError as error:
                if "duplicate column name" not in str(error).casefold():
                    raise
            for statement in (
                "ALTER TABLE free_music_tasks ADD COLUMN origin TEXT NOT NULL DEFAULT 'user'",
                "ALTER TABLE free_music_tasks ADD COLUMN release_group_mbid TEXT",
                "ALTER TABLE free_music_tasks ADD COLUMN release_mbid TEXT",
                "ALTER TABLE free_music_tasks ADD COLUMN release_track_mbid TEXT",
                "ALTER TABLE free_music_tasks ADD COLUMN recording_mbid TEXT",
                "ALTER TABLE free_music_tasks ADD COLUMN duration_seconds REAL",
                "ALTER TABLE free_music_tasks ADD COLUMN album_title TEXT",
                "ALTER TABLE free_music_tasks ADD COLUMN track_number INTEGER",
                "ALTER TABLE free_music_tasks ADD COLUMN disc_number INTEGER",
                "ALTER TABLE free_music_tasks ADD COLUMN quality_snapshot_json TEXT",
                "ALTER TABLE free_music_tasks ADD COLUMN quality_snapshot_hash TEXT",
                "ALTER TABLE free_music_tasks ADD COLUMN quality_snapshot_summary TEXT",
                "ALTER TABLE free_music_tasks ADD COLUMN tried_candidates_json "
                "TEXT NOT NULL DEFAULT '[]'",
            ):
                try:
                    conn.execute(statement)
                except sqlite3.OperationalError as error:
                    if "duplicate column name" not in str(error).casefold():
                        raise
            conn.commit()
        finally:
            conn.close()

    async def create(
        self,
        task_id: str,
        user_id: str,
        kind: str,
        mbid: str,
        artist: str,
        title: str,
        *,
        track_count: int = 0,
        origin: str = "user",
        release_group_mbid: str | None = None,
        release_mbid: str | None = None,
        release_track_mbid: str | None = None,
        recording_mbid: str | None = None,
        duration_seconds: float | None = None,
        album_title: str | None = None,
        track_number: int | None = None,
        disc_number: int | None = None,
        quality_snapshot_hash: str | None = None,
        quality_snapshot_json: str | None = None,
        quality_snapshot_summary: str | None = None,
        tried_candidates_json: str = "[]",
    ) -> None:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "INSERT INTO free_music_tasks "
                "(id, user_id, kind, mbid, artist, title, status, created_at, updated_at, "
                "track_count, origin, release_group_mbid, release_mbid, "
                "release_track_mbid, recording_mbid, duration_seconds, "
                "album_title, track_number, disc_number, "
                "quality_snapshot_json, quality_snapshot_hash, "
                "quality_snapshot_summary, tried_candidates_json) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, "
                "?, ?, ?, ?)",
                (
                    task_id,
                    user_id,
                    kind,
                    mbid,
                    artist,
                    title,
                    FreeMusicStatus.SEARCHING,
                    now,
                    now,
                    max(0, track_count),
                    origin,
                    release_group_mbid,
                    release_mbid,
                    release_track_mbid,
                    recording_mbid,
                    duration_seconds,
                    album_title,
                    track_number,
                    disc_number,
                    quality_snapshot_json,
                    quality_snapshot_hash,
                    quality_snapshot_summary,
                    tried_candidates_json,
                ),
            )

        await self._write(operation)

    async def get(self, task_id: str) -> FreeMusicTask | None:
        def operation(conn: sqlite3.Connection) -> FreeMusicTask | None:
            row = conn.execute(
                f"SELECT {_COLUMNS} FROM free_music_tasks WHERE id = ?", (task_id,)
            ).fetchone()
            return _row_to_task(row) if row else None

        return await self._read(operation)

    async def list_tasks(
        self, *, user_id: str | None = None, limit: int = 50
    ) -> list[FreeMusicTask]:
        """Newest first. ``user_id=None`` lists everyone's (the admin view)."""

        def operation(conn: sqlite3.Connection) -> list[FreeMusicTask]:
            if user_id is None:
                rows = conn.execute(
                    f"SELECT {_COLUMNS} FROM free_music_tasks "
                    "ORDER BY created_at DESC LIMIT ?",
                    (limit,),
                ).fetchall()
            else:
                rows = conn.execute(
                    f"SELECT {_COLUMNS} FROM free_music_tasks WHERE user_id = ? "
                    "ORDER BY created_at DESC LIMIT ?",
                    (user_id, limit),
                ).fetchall()
            return [_row_to_task(r) for r in rows]

        return await self._read(operation)

    async def list_tasks_missing_snapshot(
        self, *, limit: int = 500
    ) -> list[FreeMusicTask]:
        """Backfill feed: tasks without a pinned snapshot, ALL statuses included
        - terminal history keeps its policy summary for display."""

        def operation(conn: sqlite3.Connection) -> list[FreeMusicTask]:
            rows = conn.execute(
                f"SELECT {_COLUMNS} FROM free_music_tasks "
                "WHERE quality_snapshot_json IS NULL "
                "ORDER BY created_at DESC LIMIT ?",
                (limit,),
            ).fetchall()
            return [_row_to_task(r) for r in rows]

        return await self._read(operation)

    async def update(
        self,
        task_id: str,
        *,
        status: str | None = None,
        identifier: str | None = None,
        licence_url: str | None = None,
        format: str | None = None,  # noqa: A002 - mirrors the column name
        files_total: int | None = None,
        files_completed: int | None = None,
        bytes_total: int | None = None,
        bytes_downloaded: int | None = None,
        attempts: int | None = None,
        error: str | None = None,
        quality_snapshot_hash: str | None = None,
        quality_snapshot_json: str | None = None,
        quality_snapshot_summary: str | None = None,
        tried_candidates_json: str | None = None,
        expected_statuses: tuple[str, ...] | None = None,
    ) -> bool:
        now = time.time()
        fields = {
            "status": status,
            "identifier": identifier,
            "licence_url": licence_url,
            "format": format,
            "files_total": files_total,
            "files_completed": files_completed,
            "bytes_total": bytes_total,
            "bytes_downloaded": bytes_downloaded,
            "attempts": attempts,
            "error": error,
            "quality_snapshot_json": quality_snapshot_json,
            "quality_snapshot_hash": quality_snapshot_hash,
            "quality_snapshot_summary": quality_snapshot_summary,
            "tried_candidates_json": tried_candidates_json,
        }
        sets = ["updated_at = ?"]
        params: list = [now]
        for column, value in fields.items():
            if value is not None:
                sets.append(f"{column} = ?")
                params.append(value)
        where = "id = ?"
        params.append(task_id)
        if expected_statuses:
            placeholders = ",".join("?" for _ in expected_statuses)
            where += f" AND status IN ({placeholders})"
            params.extend(expected_statuses)

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                f"UPDATE free_music_tasks SET {', '.join(sets)} WHERE {where}",
                tuple(params),
            )
            return cursor.rowcount == 1

        return await self._write(operation)

    async def update_free_music_quality_fields(self, updates: list[dict]) -> None:
        """Persist acquisition-quality snapshot/ladder fields onto Free Music
        tasks in ONE transaction. Provided-key-writes-value like
        ``DownloadStore.update_task_quality_fields`` (keys absent from a dict
        stay untouched; a present key writes verbatim, including an explicit
        None). Validation completes before any SQL runs."""
        if not updates:
            return
        now = time.time()
        statements: list[tuple[str, tuple[Any, ...]]] = []
        for change in updates:
            task_id = change.get("id")
            if not isinstance(task_id, str) or not task_id:
                raise ValueError("each quality-field update needs a task id")
            sets = ["updated_at = ?"]
            params: list[Any] = [now]
            for key, value in change.items():
                if key == "id":
                    continue
                if key not in _FREE_MUSIC_QUALITY_UPDATABLE:
                    raise ValueError(f"free_music_tasks column not updatable: {key}")
                sets.append(f"{key} = ?")
                params.append(value)
            params.append(task_id)
            statements.append(
                (
                    f"UPDATE free_music_tasks SET {', '.join(sets)} WHERE id = ?",
                    tuple(params),
                )
            )

        def operation(conn: sqlite3.Connection) -> None:
            for sql, sql_params in statements:
                conn.execute(sql, sql_params)

        return await self._write(operation)

    async def backfill_free_music_quality_fields(self, updates: list[dict]) -> int:
        """Stamp only snapshot-less Free Music tasks during startup migration."""
        if not updates:
            return 0
        now = time.time()
        statements: list[tuple[str, tuple[Any, ...]]] = []
        for change in updates:
            task_id = change.get("id")
            if not isinstance(task_id, str) or not task_id:
                raise ValueError("each quality-field update needs a task id")
            sets = ["updated_at = ?"]
            params: list[Any] = [now]
            for key, value in change.items():
                if key == "id":
                    continue
                if key not in _FREE_MUSIC_QUALITY_UPDATABLE:
                    raise ValueError(f"free_music_tasks column not updatable: {key}")
                sets.append(f"{key} = ?")
                params.append(value)
            params.append(task_id)
            statements.append(
                (
                    "UPDATE free_music_tasks SET "
                    f"{', '.join(sets)} WHERE id = ? AND quality_snapshot_json IS NULL",
                    tuple(params),
                )
            )

        def operation(conn: sqlite3.Connection) -> int:
            changed = 0
            for sql, sql_params in statements:
                changed += conn.execute(sql, sql_params).rowcount
            return changed

        return await self._write(operation)

    async def restart_terminal(self, task_id: str) -> bool:
        """Atomically move a failed or cancelled task back to searching.

        The status guard prevents a concurrent history removal from deleting a task
        after retry has claimed it.
        """
        now = time.time()

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "UPDATE free_music_tasks SET status = 'searching', identifier = NULL, "
                "licence_url = NULL, format = NULL, files_total = 0, files_completed = 0, "
                "bytes_total = 0, bytes_downloaded = 0, attempts = 0, error = NULL, "
                "updated_at = ? WHERE id = ? AND status IN ('failed','cancelled')",
                (now, task_id),
            )
            return cursor.rowcount == 1

        return await self._write(operation)

    async def cancel_active(self, task_id: str) -> FreeMusicTask | None:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> FreeMusicTask | None:
            changed = conn.execute(
                "UPDATE free_music_tasks SET status = 'cancelled', error = 'Cancelled', "
                "updated_at = ? WHERE id = ? AND status IN ('searching','downloading')",
                (now, task_id),
            ).rowcount
            if changed != 1:
                return None
            row = conn.execute(
                f"SELECT {_COLUMNS} FROM free_music_tasks WHERE id = ?", (task_id,)
            ).fetchone()
            return _row_to_task(row)

        return await self._write(operation)

    async def delete_terminal(self, task_id: str) -> FreeMusicTask | None:
        """Delete one terminal history row and return the deleted task."""

        def operation(conn: sqlite3.Connection) -> FreeMusicTask | None:
            row = conn.execute(
                f"SELECT {_COLUMNS} FROM free_music_tasks WHERE id = ? "
                "AND status IN ('completed','failed','cancelled')",
                (task_id,),
            ).fetchone()
            if row is None:
                return None
            task = _row_to_task(row)
            conn.execute("DELETE FROM free_music_tasks WHERE id = ?", (task_id,))
            return task

        return await self._write(operation)

    async def delete_terminal_tasks(
        self, *, user_id: str | None
    ) -> list[tuple[str, str]]:
        """Delete terminal history for one user, or everyone when ``user_id=None``."""

        def operation(conn: sqlite3.Connection) -> list[tuple[str, str]]:
            where = "status IN ('completed','failed','cancelled')"
            params: tuple[str, ...] = ()
            if user_id is not None:
                where += " AND user_id = ?"
                params = (user_id,)
            rows = conn.execute(
                f"SELECT id, user_id FROM free_music_tasks WHERE {where}", params
            ).fetchall()
            if rows:
                conn.execute(f"DELETE FROM free_music_tasks WHERE {where}", params)
            return [(row["id"], row["user_id"]) for row in rows]

        return await self._write(operation)

    async def fail_stale(self, detail: str) -> int:
        """Startup sweep: a non-terminal task whose coroutine died with the
        process can never finish."""

        def operation(conn: sqlite3.Connection) -> int:
            cursor = conn.execute(
                "UPDATE free_music_tasks SET status = 'failed', error = ?, updated_at = ? "
                "WHERE status NOT IN ('completed','failed','cancelled')",
                (detail, time.time()),
            )
            return cursor.rowcount

        return await self._write(operation)
