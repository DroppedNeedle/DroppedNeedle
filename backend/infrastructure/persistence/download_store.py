"""``DownloadStore`` - persistence for download tasks, search jobs, and quarantine.

(AUD-5/6/7) Subclasses ``PersistenceBase``, lives in ``library.db``, takes the
SHARED write lock, and sets ``PRAGMA foreign_keys=ON`` so
``download_tasks.user_id -> auth_users(id) ON DELETE CASCADE`` is enforced.
(AUD-9) ``search_jobs.candidates_blob`` stores ``list[ScoredCandidate]`` via the
house JSON codec (``to_jsonable`` + ``json.dumps``), decoded with
``msgspec.convert`` - never ``msgspec.json``.

There is NO batch-GUID / ``client_task_id`` column (C2): a task is correlated to
its slskd transfers by ``source_username`` + the manifest filenames.
"""

import sqlite3
import threading
import time
import uuid
from pathlib import Path
from typing import Any

import msgspec

from infrastructure.persistence._database import (
    PersistenceBase,
    _decode_json,
    _encode_json,
)
from infrastructure.serialization import to_jsonable
from models.download import DownloadTask, ScoredCandidate, SearchJob

_ACTIVE_STATUSES = ("queued", "downloading", "processing")

# Columns on download_tasks that update_status (and friends) may set directly.
_TASK_UPDATABLE = frozenset(
    {
        "release_mbid",
        "recording_mbid",
        "artist_mbid",
        "source_username",
        "source_directory",
        "search_query",
        "search_job_id",
        "candidate_index",
        "preflight_score",
        "progress_percent",
        "total_size_bytes",
        "downloaded_bytes",
        "files_total",
        "files_completed",
        "files_failed",
        "quality_format",
        "quality_bitrate",
        "quality_sample_rate",
        "quality_bit_depth",
        "staging_path",
        "final_path",
        "error_message",
        "last_polled_at",
        "started_at",
        "completed_at",
        "cancelled_at",
    }
)

# Ordered column list used for INSERT; mirrors the DownloadTask struct fields.
_TASK_COLUMNS = (
    "id",
    "user_id",
    "download_type",
    "release_group_mbid",
    "release_mbid",
    "recording_mbid",
    "artist_mbid",
    "artist_name",
    "album_title",
    "track_title",
    "track_number",
    "disc_number",
    "year",
    "track_count",
    "track_duration_seconds",
    "download_client",
    "source_username",
    "source_directory",
    "search_query",
    "search_job_id",
    "candidate_index",
    "status",
    "preflight_score",
    "progress_percent",
    "total_size_bytes",
    "downloaded_bytes",
    "files_total",
    "files_completed",
    "files_failed",
    "quality_format",
    "quality_bitrate",
    "quality_sample_rate",
    "quality_bit_depth",
    "staging_path",
    "final_path",
    "error_message",
    "retry_count",
    "last_polled_at",
    "created_at",
    "started_at",
    "completed_at",
    "cancelled_at",
    "updated_at",
)


class DownloadStore(PersistenceBase):
    def __init__(self, db_path: Path, write_lock: threading.Lock) -> None:
        super().__init__(db_path, write_lock)

    def _connect(self) -> sqlite3.Connection:
        # (AUD-6) Enforce download_tasks.user_id -> auth_users(id) ON DELETE CASCADE.
        conn = super()._connect()
        conn.execute("PRAGMA foreign_keys=ON")
        return conn

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.executescript(
                """
                CREATE TABLE IF NOT EXISTS download_tasks (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                    request_history_mbid TEXT,
                    download_type TEXT NOT NULL DEFAULT 'album',
                    release_group_mbid TEXT NOT NULL,
                    release_mbid TEXT,
                    recording_mbid TEXT,
                    artist_mbid TEXT,
                    artist_name TEXT NOT NULL,
                    album_title TEXT NOT NULL,
                    track_title TEXT,
                    track_number INTEGER,
                    disc_number INTEGER,
                    year INTEGER,
                    track_count INTEGER,
                    track_duration_seconds REAL,
                    download_client TEXT NOT NULL DEFAULT 'slskd',
                    source_username TEXT,
                    source_directory TEXT,
                    search_query TEXT,
                    search_job_id TEXT,
                    candidate_index INTEGER,
                    status TEXT NOT NULL DEFAULT 'queued'
                        CHECK(status IN ('queued','downloading','processing',
                                         'completed','partial','failed','cancelled')),
                    preflight_score REAL,
                    progress_percent INTEGER NOT NULL DEFAULT 0,
                    total_size_bytes INTEGER,
                    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
                    files_total INTEGER NOT NULL DEFAULT 0,
                    files_completed INTEGER NOT NULL DEFAULT 0,
                    files_failed INTEGER NOT NULL DEFAULT 0,
                    quality_format TEXT,
                    quality_bitrate INTEGER,
                    quality_sample_rate INTEGER,
                    quality_bit_depth INTEGER,
                    staging_path TEXT,
                    final_path TEXT,
                    error_message TEXT,
                    retry_count INTEGER NOT NULL DEFAULT 0,
                    last_polled_at REAL,
                    created_at REAL NOT NULL,
                    started_at REAL,
                    completed_at REAL,
                    cancelled_at REAL,
                    updated_at REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_download_tasks_status ON download_tasks(status);
                CREATE INDEX IF NOT EXISTS idx_download_tasks_user ON download_tasks(user_id);
                CREATE INDEX IF NOT EXISTS idx_download_tasks_rgmbid ON download_tasks(release_group_mbid);
                CREATE INDEX IF NOT EXISTS idx_download_tasks_type ON download_tasks(download_type);
                CREATE INDEX IF NOT EXISTS idx_download_tasks_username ON download_tasks(source_username);
                CREATE INDEX IF NOT EXISTS idx_download_tasks_created ON download_tasks(created_at DESC);

                CREATE TABLE IF NOT EXISTS download_quarantine (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    client_id TEXT NOT NULL DEFAULT 'unknown',
                    username TEXT NOT NULL,
                    filename TEXT NOT NULL,
                    release_group_mbid TEXT,
                    reason TEXT NOT NULL
                        CHECK(reason IN ('verify_failed','corrupt','fingerprint_mismatch','duration_mismatch','manual')),
                    quarantined_at REAL NOT NULL,
                    UNIQUE (client_id, username, filename, release_group_mbid)
                );
                CREATE INDEX IF NOT EXISTS idx_quarantine_lookup ON download_quarantine(client_id, username, filename);
                CREATE INDEX IF NOT EXISTS idx_quarantine_quarantined_at ON download_quarantine(quarantined_at);

                CREATE TABLE IF NOT EXISTS search_jobs (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    artist_name TEXT NOT NULL,
                    album_title TEXT NOT NULL,
                    year INTEGER,
                    track_count INTEGER,
                    release_group_mbid TEXT,
                    search_query TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'searching'
                        CHECK(status IN ('searching','matched','completed','failed','cancelled')),
                    candidates_blob TEXT NOT NULL DEFAULT '[]',
                    error_message TEXT,
                    created_at REAL NOT NULL,
                    completed_at REAL,
                    updated_at REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_search_jobs_user ON search_jobs(user_id);
                CREATE INDEX IF NOT EXISTS idx_search_jobs_status ON search_jobs(status);
                CREATE INDEX IF NOT EXISTS idx_search_jobs_rgmbid ON search_jobs(release_group_mbid);
                """
            )
            # Idempotent column adds for dev DBs created before the column existed
            # (try/except duplicate-column, per the plan's migration convention).
            for column, ddl in (("track_duration_seconds", "REAL"),):
                try:
                    conn.execute(f"ALTER TABLE download_tasks ADD COLUMN {column} {ddl}")
                except sqlite3.OperationalError:
                    pass  # duplicate column - already present
            conn.commit()
        finally:
            conn.close()

    async def create_task(
        self,
        *,
        user_id: str,
        download_type: str = "album",
        release_group_mbid: str = "",
        artist_name: str = "",
        album_title: str = "",
        release_mbid: str | None = None,
        recording_mbid: str | None = None,
        artist_mbid: str | None = None,
        track_title: str | None = None,
        track_number: int | None = None,
        disc_number: int | None = None,
        year: int | None = None,
        track_count: int | None = None,
        track_duration_seconds: float | None = None,
        download_client: str = "slskd",
        search_query: str | None = None,
        search_job_id: str | None = None,
        candidate_index: int | None = None,
        source_username: str | None = None,
        source_directory: str | None = None,
        preflight_score: float | None = None,
        status: str = "queued",
        retry_count: int = 0,
    ) -> DownloadTask:
        now = time.time()
        task = DownloadTask(
            id=uuid.uuid4().hex,
            user_id=user_id,
            download_type=download_type,
            release_group_mbid=release_group_mbid,
            artist_name=artist_name,
            album_title=album_title,
            release_mbid=release_mbid,
            recording_mbid=recording_mbid,
            artist_mbid=artist_mbid,
            track_title=track_title,
            track_number=track_number,
            disc_number=disc_number,
            year=year,
            track_count=track_count,
            track_duration_seconds=track_duration_seconds,
            download_client=download_client,
            search_query=search_query,
            search_job_id=search_job_id,
            candidate_index=candidate_index,
            source_username=source_username,
            source_directory=source_directory,
            preflight_score=preflight_score,
            status=status,
            retry_count=retry_count,
            created_at=now,
            updated_at=now,
        )
        values = tuple(getattr(task, col) for col in _TASK_COLUMNS)
        placeholders = ", ".join("?" for _ in _TASK_COLUMNS)
        columns = ", ".join(_TASK_COLUMNS)

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                f"INSERT INTO download_tasks ({columns}) VALUES ({placeholders})",
                values,
            )

        await self._write(operation)
        return task

    async def get_task(self, task_id: str) -> DownloadTask | None:
        def operation(conn: sqlite3.Connection) -> DownloadTask | None:
            row = conn.execute(
                "SELECT * FROM download_tasks WHERE id = ?", (task_id,)
            ).fetchone()
            return _row_to_task(row)

        return await self._read(operation)

    async def get_task_for_user(
        self, task_id: str, user_id: str, user_role: str
    ) -> DownloadTask | None:
        task = await self.get_task(task_id)
        if task is None:
            return None
        if user_role == "admin" or task.user_id == user_id:
            return task
        return None

    async def get_active_task_for_album(
        self, release_group_mbid: str, user_id: str
    ) -> DownloadTask | None:
        def operation(conn: sqlite3.Connection) -> DownloadTask | None:
            row = conn.execute(
                f"""SELECT * FROM download_tasks
                    WHERE release_group_mbid = ? AND user_id = ?
                      AND download_type = 'album'
                      AND status IN ({_in_placeholders(_ACTIVE_STATUSES)})
                    ORDER BY created_at DESC LIMIT 1""",
                (release_group_mbid, user_id, *_ACTIVE_STATUSES),
            ).fetchone()
            return _row_to_task(row)

        return await self._read(operation)

    async def get_active_task_for_album_any_user(
        self, release_group_mbid: str
    ) -> DownloadTask | None:
        """An active album download for this release-group by ANY user. The follow
        poller uses this so one new album is enqueued at most once across all of
        its followers (DD5). Case-insensitive so a casing mismatch never lets a
        duplicate slip through."""
        def operation(conn: sqlite3.Connection) -> DownloadTask | None:
            row = conn.execute(
                f"""SELECT * FROM download_tasks
                    WHERE lower(release_group_mbid) = lower(?)
                      AND download_type = 'album'
                      AND status IN ({_in_placeholders(_ACTIVE_STATUSES)})
                    ORDER BY created_at DESC LIMIT 1""",
                (release_group_mbid, *_ACTIVE_STATUSES),
            ).fetchone()
            return _row_to_task(row)

        return await self._read(operation)

    async def get_active_task_for_track(
        self, recording_mbid: str, user_id: str
    ) -> DownloadTask | None:
        def operation(conn: sqlite3.Connection) -> DownloadTask | None:
            row = conn.execute(
                f"""SELECT * FROM download_tasks
                    WHERE recording_mbid = ? AND user_id = ?
                      AND download_type = 'track'
                      AND status IN ({_in_placeholders(_ACTIVE_STATUSES)})
                    ORDER BY created_at DESC LIMIT 1""",
                (recording_mbid, user_id, *_ACTIVE_STATUSES),
            ).fetchone()
            return _row_to_task(row)

        return await self._read(operation)

    async def list_tasks(
        self,
        user_id: str | None = None,
        user_role: str | None = None,
        status: str | None = None,
        release_group_mbid: str | None = None,
        page: int = 1,
        page_size: int = 20,
    ) -> list[DownloadTask]:
        clauses: list[str] = []
        params: list[Any] = []
        # Non-admins only see their own tasks - fail closed if no user_id is given.
        if user_role != "admin":
            if user_id is None:
                return []
            clauses.append("user_id = ?")
            params.append(user_id)
        if status is not None:
            clauses.append("status = ?")
            params.append(status)
        if release_group_mbid is not None:
            clauses.append("release_group_mbid = ?")
            params.append(release_group_mbid)
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        offset = max(0, (page - 1) * page_size)
        params.extend([page_size, offset])

        def operation(conn: sqlite3.Connection) -> list[DownloadTask]:
            rows = conn.execute(
                f"SELECT * FROM download_tasks {where} "
                f"ORDER BY created_at DESC LIMIT ? OFFSET ?",
                tuple(params),
            ).fetchall()
            return [t for t in (_row_to_task(r) for r in rows) if t is not None]

        return await self._read(operation)

    async def list_active_tasks(self, statuses: list[str]) -> list[DownloadTask]:
        if not statuses:
            return []

        def operation(conn: sqlite3.Connection) -> list[DownloadTask]:
            rows = conn.execute(
                f"SELECT * FROM download_tasks "
                f"WHERE status IN ({_in_placeholders(statuses)}) "
                f"ORDER BY created_at ASC",
                tuple(statuses),
            ).fetchall()
            return [t for t in (_row_to_task(r) for r in rows) if t is not None]

        return await self._read(operation)

    async def update_status(self, task_id: str, status: str, **fields: Any) -> None:
        sets = ["status = ?", "updated_at = ?"]
        params: list[Any] = [status, time.time()]
        for key, value in fields.items():
            if key not in _TASK_UPDATABLE:
                raise ValueError(f"download_tasks column not updatable: {key}")
            sets.append(f"{key} = ?")
            params.append(value)
        params.append(task_id)

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                f"UPDATE download_tasks SET {', '.join(sets)} WHERE id = ?",
                tuple(params),
            )

        await self._write(operation)

    async def set_source_username(self, task_id: str, username: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE download_tasks SET source_username = ?, updated_at = ? WHERE id = ?",
                (username, time.time(), task_id),
            )

        await self._write(operation)

    async def set_search_job_id_and_candidate(
        self, task_id: str, search_job_id: str, candidate_index: int | None
    ) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE download_tasks SET search_job_id = ?, candidate_index = ?, "
                "updated_at = ? WHERE id = ?",
                (search_job_id, candidate_index, time.time(), task_id),
            )

        await self._write(operation)

    async def link_picked_candidate(
        self,
        task_id: str,
        search_job_id: str,
        candidate_index: int,
        source_username: str,
        source_directory: str,
        preflight_score: float,
    ) -> None:
        """(AUD-8) Link task<->candidate AND move the search job to 'matched' in
        ONE transaction (single commit)."""
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """UPDATE download_tasks
                   SET search_job_id = ?, candidate_index = ?, source_username = ?,
                       source_directory = ?, preflight_score = ?, updated_at = ?
                   WHERE id = ?""",
                (
                    search_job_id,
                    candidate_index,
                    source_username,
                    source_directory,
                    preflight_score,
                    now,
                    task_id,
                ),
            )
            conn.execute(
                "UPDATE search_jobs SET status = 'matched', updated_at = ? WHERE id = ?",
                (now, search_job_id),
            )

        await self._write(operation)

    async def update_progress(
        self,
        task_id: str,
        *,
        bytes_downloaded: int,
        files_completed: int,
        progress_percent: int,
    ) -> None:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """UPDATE download_tasks
                   SET downloaded_bytes = ?, files_completed = ?, progress_percent = ?,
                       last_polled_at = ?, updated_at = ?
                   WHERE id = ?""",
                (
                    bytes_downloaded,
                    files_completed,
                    progress_percent,
                    now,
                    now,
                    task_id,
                ),
            )

        await self._write(operation)

    async def set_final_path(self, task_id: str, final_path: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE download_tasks SET final_path = ?, updated_at = ? WHERE id = ?",
                (final_path, time.time(), task_id),
            )

        await self._write(operation)

    async def increment_retry_count(self, task_id: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE download_tasks SET retry_count = retry_count + 1, updated_at = ? WHERE id = ?",
                (time.time(), task_id),
            )

        await self._write(operation)

    async def record_quarantine(
        self,
        client_id: str,
        username: str,
        filename: str,
        reason: str,
        release_group_mbid: str | None = None,
    ) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """INSERT OR IGNORE INTO download_quarantine
                   (client_id, username, filename, release_group_mbid, reason, quarantined_at)
                   VALUES (?, ?, ?, ?, ?, ?)""",
                (client_id, username, filename, release_group_mbid, reason, time.time()),
            )

        await self._write(operation)

    async def load_quarantine_set(self) -> set[tuple[str, str]]:
        """Return ``{(username, filename), ...}`` for fast O(1) scorer lookup.

        Intentionally keyed on global ``(username, filename)`` (M9): a source
        that failed once is excluded from all future scoring for any album."""

        def operation(conn: sqlite3.Connection) -> set[tuple[str, str]]:
            rows = conn.execute(
                "SELECT username, filename FROM download_quarantine"
            ).fetchall()
            return {(row["username"], row["filename"]) for row in rows}

        return await self._read(operation)

    async def delete_quarantine(self, quarantine_id: int) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute("DELETE FROM download_quarantine WHERE id = ?", (quarantine_id,))

        await self._write(operation)

    async def list_quarantine(self, page: int = 1, page_size: int = 50) -> list[dict[str, Any]]:
        offset = max(0, (page - 1) * page_size)

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT * FROM download_quarantine ORDER BY quarantined_at DESC LIMIT ? OFFSET ?",
                (page_size, offset),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def create_search_job(
        self,
        user_id: str,
        artist_name: str,
        album_title: str,
        year: int | None,
        track_count: int | None,
        release_group_mbid: str | None,
        search_query: str,
    ) -> SearchJob:
        now = time.time()
        job = SearchJob(
            id=uuid.uuid4().hex,
            user_id=user_id,
            artist_name=artist_name,
            album_title=album_title,
            year=year,
            track_count=track_count,
            release_group_mbid=release_group_mbid,
            search_query=search_query,
            status="searching",
            created_at=now,
            updated_at=now,
        )

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """INSERT INTO search_jobs
                   (id, user_id, artist_name, album_title, year, track_count,
                    release_group_mbid, search_query, status, candidates_blob,
                    error_message, created_at, completed_at, updated_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, '[]', NULL, ?, NULL, ?)""",
                (
                    job.id,
                    job.user_id,
                    job.artist_name,
                    job.album_title,
                    job.year,
                    job.track_count,
                    job.release_group_mbid,
                    job.search_query,
                    job.status,
                    job.created_at,
                    job.updated_at,
                ),
            )

        await self._write(operation)
        return job

    async def update_search_job_status(
        self, job_id: str, status: str, error: str | None = None
    ) -> None:
        now = time.time()
        completed = now if status in ("matched", "completed", "failed", "cancelled") else None

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """UPDATE search_jobs
                   SET status = ?, error_message = ?, completed_at = ?, updated_at = ?
                   WHERE id = ?""",
                (status, error, completed, now, job_id),
            )

        await self._write(operation)

    async def set_search_job_candidates(
        self, job_id: str, candidates: list[ScoredCandidate]
    ) -> None:
        # (AUD-9) house JSON codec, NOT msgspec.json.
        blob = _encode_json(to_jsonable(candidates))

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE search_jobs SET candidates_blob = ?, updated_at = ? WHERE id = ?",
                (blob, time.time(), job_id),
            )

        await self._write(operation)

    async def get_search_job(self, job_id: str) -> SearchJob | None:
        def operation(conn: sqlite3.Connection) -> SearchJob | None:
            row = conn.execute(
                "SELECT * FROM search_jobs WHERE id = ?", (job_id,)
            ).fetchone()
            return _row_to_search_job(row)

        return await self._read(operation)

    async def get_search_job_candidates(self, job_id: str) -> list[ScoredCandidate]:
        def operation(conn: sqlite3.Connection) -> list[ScoredCandidate]:
            row = conn.execute(
                "SELECT candidates_blob FROM search_jobs WHERE id = ?", (job_id,)
            ).fetchone()
            if row is None:
                return []
            decoded = _decode_json(row["candidates_blob"])
            return msgspec.convert(decoded, type=list[ScoredCandidate], strict=False)

        return await self._read(operation)

    async def delete_expired_search_jobs(self, max_age_seconds: float = 604800) -> int:
        """Delete search jobs older than ``max_age_seconds`` (default 7 days).
        Run at startup; returns the number of rows removed."""
        cutoff = time.time() - max_age_seconds

        def operation(conn: sqlite3.Connection) -> int:
            cursor = conn.execute("DELETE FROM search_jobs WHERE created_at < ?", (cutoff,))
            return cursor.rowcount

        return await self._write(operation)

    async def get_search_job_for_task(self, task_id: str) -> SearchJob | None:
        def operation(conn: sqlite3.Connection) -> SearchJob | None:
            row = conn.execute(
                """SELECT sj.* FROM search_jobs sj
                   JOIN download_tasks dt ON dt.search_job_id = sj.id
                   WHERE dt.id = ?""",
                (task_id,),
            ).fetchone()
            return _row_to_search_job(row)

        return await self._read(operation)

    async def list_retryable_tasks(
        self, max_retry_count: int
    ) -> list[DownloadTask]:
        """The newest task per target (album, or track + user) when that newest task
        is a terminal ``failed``/``partial`` under the ``retry_count`` ceiling.

        Restricting to the newest task is what lets auto-retry escalate: each retry
        spawns a fresh task carrying ``retry_count + 1``, so the original failure
        must stop seeding retries - otherwise backoff never grows, the ceiling is
        never reached, and an album whose retry has since succeeded gets downloaded
        again. Does NOT filter by age - the caller applies per-task exponential
        backoff (which depends on each task's own ``retry_count``). Ordered
        oldest-first so the most overdue retry goes first."""
        def operation(conn: sqlite3.Connection) -> list[DownloadTask]:
            rows = conn.execute(
                """SELECT * FROM download_tasks t
                   WHERE t.status IN ('failed', 'partial')
                     AND t.retry_count < ?
                     AND NOT EXISTS (
                         SELECT 1 FROM download_tasks n
                         WHERE n.user_id = t.user_id
                           AND n.download_type = t.download_type
                           AND n.release_group_mbid = t.release_group_mbid
                           AND COALESCE(n.recording_mbid, '') = COALESCE(t.recording_mbid, '')
                           AND (n.created_at > t.created_at
                                OR (n.created_at = t.created_at AND n.rowid > t.rowid))
                     )
                   ORDER BY t.completed_at ASC NULLS LAST""",
                (max_retry_count,),
            ).fetchall()
            return [t for t in (_row_to_task(r) for r in rows) if t is not None]

        return await self._read(operation)


def _in_placeholders(items: Any) -> str:
    return ", ".join("?" for _ in items)


def _row_to_task(row: sqlite3.Row | None) -> DownloadTask | None:
    if row is None:
        return None
    return msgspec.convert(dict(row), type=DownloadTask, strict=False)


def _row_to_search_job(row: sqlite3.Row | None) -> SearchJob | None:
    if row is None:
        return None
    return msgspec.convert(dict(row), type=SearchJob, strict=False)
