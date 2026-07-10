"""``DropImportStore`` - persistence for drop-import jobs and their items.

Tables in the shared ``library.db``:

- ``drop_import_jobs`` - one row per upload gesture; owns a staging directory
  under ``<root_app_dir>/imports/<job_id>``.
- ``drop_import_items`` - one row per album-shaped unit inside a job.
  ``staging_paths`` is a JSON list of the unit's staged audio files, kept so a
  ``needs_review`` item can be re-imported against a manually chosen release
  group after a restart.

``PRAGMA foreign_keys=ON`` on top of ``PersistenceBase._connect`` so the
``ON DELETE CASCADE`` from items to jobs fires (the events-store pattern).
"""

import json
import sqlite3
import threading
import time
from pathlib import Path

from infrastructure.persistence._database import PersistenceBase
from models.drop_import import DropImportItem, DropImportJob, ItemStatus, JobStatus

_JOB_COLUMNS = "id, user_id, user_name, status, created_at, upload_name, staging_dir, error"
_ITEM_COLUMNS = (
    "id, job_id, folder_name, status, release_group_mbid, album_title, artist_name, "
    "files_total, files_imported, detail, staging_paths, updated_at"
)


def _row_to_job(row: sqlite3.Row) -> DropImportJob:
    return DropImportJob(
        id=row["id"],
        user_id=row["user_id"],
        user_name=row["user_name"],
        status=row["status"],
        created_at=row["created_at"],
        upload_name=row["upload_name"],
        staging_dir=row["staging_dir"],
        error=row["error"],
    )


def _row_to_item(row: sqlite3.Row) -> DropImportItem:
    try:
        staging_paths = json.loads(row["staging_paths"] or "[]")
    except ValueError:
        staging_paths = []
    return DropImportItem(
        id=row["id"],
        job_id=row["job_id"],
        folder_name=row["folder_name"],
        status=row["status"],
        release_group_mbid=row["release_group_mbid"],
        album_title=row["album_title"],
        artist_name=row["artist_name"],
        files_total=row["files_total"],
        files_imported=row["files_imported"],
        detail=row["detail"],
        staging_paths=staging_paths,
        updated_at=row["updated_at"],
    )


class DropImportStore(PersistenceBase):
    def __init__(self, db_path: Path, write_lock: threading.Lock) -> None:
        super().__init__(db_path, write_lock)

    def _connect(self) -> sqlite3.Connection:
        conn = super()._connect()
        conn.execute("PRAGMA foreign_keys=ON")
        return conn

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.executescript("""
                CREATE TABLE IF NOT EXISTS drop_import_jobs (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    user_name TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL
                        CHECK(status IN ('processing','completed','failed')),
                    created_at REAL NOT NULL,
                    upload_name TEXT NOT NULL,
                    staging_dir TEXT NOT NULL,
                    error TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_drop_import_jobs_user
                    ON drop_import_jobs(user_id, created_at);

                CREATE TABLE IF NOT EXISTS drop_import_items (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    job_id TEXT NOT NULL
                        REFERENCES drop_import_jobs(id) ON DELETE CASCADE,
                    folder_name TEXT NOT NULL,
                    status TEXT NOT NULL
                        CHECK(status IN ('processing','imported','skipped',
                                         'needs_review','failed','discarded')),
                    release_group_mbid TEXT,
                    album_title TEXT,
                    artist_name TEXT,
                    files_total INTEGER NOT NULL DEFAULT 0,
                    files_imported INTEGER NOT NULL DEFAULT 0,
                    detail TEXT,
                    staging_paths TEXT NOT NULL DEFAULT '[]',
                    updated_at REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_drop_import_items_job
                    ON drop_import_items(job_id);
            """)
            conn.commit()
        finally:
            conn.close()

    # -- jobs --

    async def create_job(
        self, job_id: str, user_id: str, user_name: str, upload_name: str, staging_dir: str
    ) -> None:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                f"INSERT INTO drop_import_jobs ({_JOB_COLUMNS}) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, NULL)",
                (job_id, user_id, user_name, JobStatus.PROCESSING, now, upload_name, staging_dir),
            )

        await self._write(operation)

    async def set_job_status(self, job_id: str, status: str, error: str | None = None) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE drop_import_jobs SET status = ?, error = ? WHERE id = ?",
                (status, error, job_id),
            )

        await self._write(operation)

    async def get_job(self, job_id: str) -> DropImportJob | None:
        def operation(conn: sqlite3.Connection) -> DropImportJob | None:
            row = conn.execute(
                f"SELECT {_JOB_COLUMNS} FROM drop_import_jobs WHERE id = ?", (job_id,)
            ).fetchone()
            if row is None:
                return None
            job = _row_to_job(row)
            items = conn.execute(
                f"SELECT {_ITEM_COLUMNS} FROM drop_import_items WHERE job_id = ? ORDER BY id",
                (job_id,),
            ).fetchall()
            job.items = [_row_to_item(r) for r in items]
            return job

        return await self._read(operation)

    async def list_jobs(
        self, *, user_id: str | None = None, limit: int = 50
    ) -> list[DropImportJob]:
        """Recent jobs, newest first, items attached. ``user_id=None`` lists all
        users' jobs (the admin view)."""

        def operation(conn: sqlite3.Connection) -> list[DropImportJob]:
            if user_id is None:
                rows = conn.execute(
                    f"SELECT {_JOB_COLUMNS} FROM drop_import_jobs "
                    "ORDER BY created_at DESC LIMIT ?",
                    (limit,),
                ).fetchall()
            else:
                rows = conn.execute(
                    f"SELECT {_JOB_COLUMNS} FROM drop_import_jobs WHERE user_id = ? "
                    "ORDER BY created_at DESC LIMIT ?",
                    (user_id, limit),
                ).fetchall()
            jobs = [_row_to_job(r) for r in rows]
            by_id = {job.id: job for job in jobs}
            if by_id:
                placeholders = ",".join("?" for _ in by_id)
                items = conn.execute(
                    f"SELECT {_ITEM_COLUMNS} FROM drop_import_items "
                    f"WHERE job_id IN ({placeholders}) ORDER BY id",
                    tuple(by_id),
                ).fetchall()
                for row in items:
                    by_id[row["job_id"]].items.append(_row_to_item(row))
            return jobs

        return await self._read(operation)

    async def fail_stale_processing(self, detail: str) -> int:
        """Startup sweep: a 'processing' job whose task died with the process can
        never finish - mark it (and its non-terminal items) failed."""
        now = time.time()

        def operation(conn: sqlite3.Connection) -> int:
            stale = [
                row["id"]
                for row in conn.execute(
                    "SELECT id FROM drop_import_jobs WHERE status = 'processing'"
                )
            ]
            if not stale:
                return 0
            placeholders = ",".join("?" for _ in stale)
            conn.execute(
                f"UPDATE drop_import_jobs SET status = 'failed', error = ? "
                f"WHERE id IN ({placeholders})",
                (detail, *stale),
            )
            conn.execute(
                f"UPDATE drop_import_items SET status = 'failed', detail = ?, updated_at = ? "
                f"WHERE status = 'processing' AND job_id IN ({placeholders})",
                (detail, now, *stale),
            )
            return len(stale)

        return await self._write(operation)

    # -- items --

    async def add_item(
        self, job_id: str, folder_name: str, staging_paths: list[str], files_total: int
    ) -> int:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> int:
            cursor = conn.execute(
                "INSERT INTO drop_import_items "
                "(job_id, folder_name, status, files_total, staging_paths, updated_at) "
                "VALUES (?, ?, ?, ?, ?, ?)",
                (
                    job_id,
                    folder_name,
                    ItemStatus.PROCESSING,
                    files_total,
                    json.dumps(staging_paths),
                    now,
                ),
            )
            return int(cursor.lastrowid or 0)

        return await self._write(operation)

    async def get_item(self, item_id: int) -> DropImportItem | None:
        def operation(conn: sqlite3.Connection) -> DropImportItem | None:
            row = conn.execute(
                f"SELECT {_ITEM_COLUMNS} FROM drop_import_items WHERE id = ?", (item_id,)
            ).fetchone()
            return _row_to_item(row) if row else None

        return await self._read(operation)

    async def update_item(
        self,
        item_id: int,
        *,
        status: str,
        release_group_mbid: str | None = None,
        album_title: str | None = None,
        artist_name: str | None = None,
        files_imported: int | None = None,
        detail: str | None = None,
        staging_paths: list[str] | None = None,
    ) -> None:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            sets = ["status = ?", "updated_at = ?"]
            params: list = [status, now]
            if release_group_mbid is not None:
                sets.append("release_group_mbid = ?")
                params.append(release_group_mbid)
            if album_title is not None:
                sets.append("album_title = ?")
                params.append(album_title)
            if artist_name is not None:
                sets.append("artist_name = ?")
                params.append(artist_name)
            if files_imported is not None:
                sets.append("files_imported = ?")
                params.append(files_imported)
            if detail is not None:
                sets.append("detail = ?")
                params.append(detail)
            if staging_paths is not None:
                sets.append("staging_paths = ?")
                params.append(json.dumps(staging_paths))
            params.append(item_id)
            conn.execute(
                f"UPDATE drop_import_items SET {', '.join(sets)} WHERE id = ?",
                tuple(params),
            )

        await self._write(operation)
