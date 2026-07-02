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
from models.held_import import HeldImport

_ACTIVE_STATUSES = ("queued", "downloading", "processing")
_RETRYABLE_STATUSES = ("failed", "partial")

# A blocklisted release self-heals after this long, so a wrongful blocklist (a transient
# failure, a false-positive) doesn't exclude a release forever. A genuinely dead release
# just gets re-tried once past the TTL and re-blocklisted. (A manual re-request clears the
# album's entries immediately, regardless of TTL.)
_QUARANTINE_TTL_SECONDS = 7 * 24 * 3600.0

# Quarantine is the cross-source blocklist, keyed (source, identity, release_group_mbid)
# (D8). ``identity`` is a single opaque string whose encoding is source-specific
# (see ``models.download_identity``): soulseek = username+filename, usenet = title+size.
# ``download_failed`` was added to the reason CHECK for SABnzbd hard-failures (D11).
_QUARANTINE_DDL = """
CREATE TABLE IF NOT EXISTS download_quarantine (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL DEFAULT 'soulseek',
    identity TEXT NOT NULL,
    release_group_mbid TEXT,
    reason TEXT NOT NULL
        CHECK(reason IN ('verify_failed','corrupt','fingerprint_mismatch',
                         'duration_mismatch','download_failed','manual')),
    quarantined_at REAL NOT NULL,
    UNIQUE (source, identity, release_group_mbid)
);
CREATE INDEX IF NOT EXISTS idx_quarantine_lookup ON download_quarantine(source, identity);
CREATE INDEX IF NOT EXISTS idx_quarantine_quarantined_at ON download_quarantine(quarantined_at);
"""

# Held imports (the "import anyway" review queue): a downloaded file that matched a track
# by duration but failed the AcoustID recording-identity backstop, copied into an app-owned
# held area so it survives the download client cleaning its completed folder. One 'held' row
# per (release_group, disc, track) - de-duped so failover across editions doesn't pile up
# copies. Resolving a row (imported/discarded) is what re-enables the album's auto-retry.
_HELD_IMPORTS_DDL = """
CREATE TABLE IF NOT EXISTS held_imports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL,
    release_group_mbid TEXT,
    release_mbid TEXT,
    recording_mbid TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    track_title TEXT,
    artist_name TEXT,
    artist_mbid TEXT,
    album_title TEXT,
    year INTEGER,
    held_path TEXT NOT NULL,
    original_filename TEXT,
    file_format TEXT,
    duration_seconds REAL,
    reason TEXT NOT NULL,
    evidence_title TEXT,
    evidence_artist TEXT,
    evidence_score REAL,
    source TEXT NOT NULL DEFAULT 'soulseek',
    source_task_id TEXT,
    -- The owning task's origin, persisted here because the task itself is deletable
    -- (clear_finished): the D10 confirm-replace must survive a cleared queue.
    origin TEXT NOT NULL DEFAULT 'user',
    naming_template TEXT,
    status TEXT NOT NULL DEFAULT 'held'
        CHECK(status IN ('held','imported','discarded')),
    created_at REAL NOT NULL,
    resolved_at REAL
);
CREATE INDEX IF NOT EXISTS idx_held_user ON held_imports(user_id, status);
CREATE INDEX IF NOT EXISTS idx_held_rg ON held_imports(release_group_mbid, status);
CREATE INDEX IF NOT EXISTS idx_held_task ON held_imports(source_task_id, status);
CREATE INDEX IF NOT EXISTS idx_held_dedup
    ON held_imports(release_group_mbid, disc_number, track_number, status);
"""

# ASCII unit separator joining the two halves of a soulseek identity; mirrors
# ``models.download_identity.soulseek_identity`` so a legacy (username, filename)
# quarantine row migrates to the same key the scorer will look up.
_SOULSEEK_ID_SEP = "\x1f"

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
    "source",
    "origin",
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
                    source TEXT NOT NULL DEFAULT 'soulseek',
                    -- Why the task exists ('user' | 'retry' | 'upgrade'); orthogonal to
                    -- source. Drives the origin-aware album gate, replace-on-import and
                    -- cap/quota exemptions (CollectionManagement D18/D19).
                    origin TEXT NOT NULL DEFAULT 'user',
                    source_username TEXT,
                    source_directory TEXT,
                    search_query TEXT,
                    search_job_id TEXT,
                    candidate_index INTEGER,
                    -- Mirrors services/native/acquisition/status.DownloadStatus.PERSISTED
                    -- (test_download_status asserts the two stay in sync). The transient
                    -- 'retrying'/'awaiting_review' statuses are SSE-only, never persisted.
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

                CREATE TABLE IF NOT EXISTS search_jobs (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    artist_name TEXT NOT NULL,
                    album_title TEXT NOT NULL,
                    year INTEGER,
                    track_count INTEGER,
                    release_group_mbid TEXT,
                    artist_mbid TEXT,
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
            for column, ddl in (
                ("track_duration_seconds", "REAL"),
                ("source", "TEXT NOT NULL DEFAULT 'soulseek'"),
                ("origin", "TEXT NOT NULL DEFAULT 'user'"),
            ):
                try:
                    conn.execute(f"ALTER TABLE download_tasks ADD COLUMN {column} {ddl}")
                except sqlite3.OperationalError:
                    pass  # duplicate column - already present
            try:
                conn.execute("ALTER TABLE search_jobs ADD COLUMN artist_mbid TEXT")
            except sqlite3.OperationalError:
                pass  # duplicate column - already present
            self._migrate_quarantine(conn)
            conn.executescript(_HELD_IMPORTS_DDL)
            for column, ddl in (
                ("artist_mbid", "TEXT"),
                ("origin", "TEXT NOT NULL DEFAULT 'user'"),
            ):
                try:
                    conn.execute(f"ALTER TABLE held_imports ADD COLUMN {column} {ddl}")
                except sqlite3.OperationalError:
                    pass  # duplicate column - already present
            conn.commit()
        finally:
            conn.close()

    def _migrate_quarantine(self, conn: sqlite3.Connection) -> None:
        """Create the quarantine table, rebuilding the old slskd-shaped schema in
        place (D8). SQLite can't ALTER a UNIQUE/CHECK, so a table that still has the
        old ``username``/``filename`` columns is rebuilt: rename aside, create the new
        ``(source, identity, …)`` shape, copy each legacy row encoding its
        ``(username, filename)`` pair as the soulseek identity (so a previously
        blocklisted source stays blocklisted), then drop the old table."""
        cols = {
            row["name"]
            for row in conn.execute("PRAGMA table_info(download_quarantine)").fetchall()
        }
        if "username" in cols:  # legacy slskd-shaped schema -> rebuild
            conn.execute("ALTER TABLE download_quarantine RENAME TO download_quarantine_legacy")
            # The legacy indexes follow the renamed table but keep their names; SQLite index
            # names are schema-global, so the new CREATE INDEX IF NOT EXISTS would no-op
            # against them and the rebuilt table would end up index-less. Drop them first.
            conn.execute("DROP INDEX IF EXISTS idx_quarantine_lookup")
            conn.execute("DROP INDEX IF EXISTS idx_quarantine_quarantined_at")
            conn.executescript(_QUARANTINE_DDL)
            # Stamp migrated rows with the upgrade time, NOT the legacy ``quarantined_at``:
            # the legacy schema had no TTL, so entries were permanent, but ``load_quarantine_set``
            # now self-heals anything older than ``_QUARANTINE_TTL_SECONDS``. Inheriting the old
            # timestamp would silently expire a still-valid blocklist on upgrade (defeating this
            # migration's purpose); a fresh stamp gives each entry one TTL window post-upgrade.
            conn.execute(
                """INSERT OR IGNORE INTO download_quarantine
                   (source, identity, release_group_mbid, reason, quarantined_at)
                   SELECT 'soulseek', username || ? || filename,
                          release_group_mbid, reason, ?
                   FROM download_quarantine_legacy""",
                (_SOULSEEK_ID_SEP, time.time()),
            )
            conn.execute("DROP TABLE download_quarantine_legacy")
        else:
            conn.executescript(_QUARANTINE_DDL)

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
        source: str = "soulseek",
        origin: str = "user",
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
            source=source,
            origin=origin,
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
        *,
        source: str = "soulseek",
        download_client: str = "slskd",
    ) -> None:
        """(AUD-8) Link task<->candidate AND move the search job to 'matched' in
        ONE transaction (single commit). ``source``/``download_client`` route a picked
        Usenet candidate to SABnzbd instead of the slskd default (D2/D3)."""
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """UPDATE download_tasks
                   SET search_job_id = ?, candidate_index = ?, source_username = ?,
                       source_directory = ?, preflight_score = ?, source = ?,
                       download_client = ?, updated_at = ?
                   WHERE id = ?""",
                (
                    search_job_id,
                    candidate_index,
                    source_username,
                    source_directory,
                    preflight_score,
                    source,
                    download_client,
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
        *,
        source: str,
        identity: str,
        reason: str,
        release_group_mbid: str | None = None,
    ) -> None:
        """Blocklist a release by its source identity (D8). ``identity`` encoding is
        source-specific (``models.download_identity``): soulseek = username+filename,
        usenet = title+size."""

        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            # Prune expired blocklist entries on write (cheap, indexed) so the table stays
            # small and the TTL self-heal is reflected on disk, not just filtered on read.
            conn.execute(
                "DELETE FROM download_quarantine WHERE quarantined_at < ?",
                (now - _QUARANTINE_TTL_SECONDS,),
            )
            conn.execute(
                """INSERT OR IGNORE INTO download_quarantine
                   (source, identity, release_group_mbid, reason, quarantined_at)
                   VALUES (?, ?, ?, ?, ?)""",
                (source, identity, release_group_mbid, reason, now),
            )

        await self._write(operation)

    async def load_quarantine_set(self) -> set[tuple[str, str]]:
        """Return ``{(source, identity), ...}`` for fast O(1) scorer lookup.

        Keyed on the global ``(source, identity)`` (M9): a release that failed is excluded
        from future scoring for any album - but only for ``_QUARANTINE_TTL_SECONDS``, so a
        wrongful blocklist self-heals. Expired rows are filtered here and pruned on the next
        ``record_quarantine`` write (keeping this a pure read)."""
        cutoff = time.time() - _QUARANTINE_TTL_SECONDS

        def operation(conn: sqlite3.Connection) -> set[tuple[str, str]]:
            rows = conn.execute(
                "SELECT source, identity FROM download_quarantine WHERE quarantined_at >= ?",
                (cutoff,),
            ).fetchall()
            return {(row["source"], row["identity"]) for row in rows}

        return await self._read(operation)

    async def delete_quarantine(self, quarantine_id: int) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute("DELETE FROM download_quarantine WHERE id = ?", (quarantine_id,))

        await self._write(operation)

    async def delete_quarantine_for_album(self, release_group_mbid: str) -> int:
        """Clear every blocklist entry for an album (all its tried releases). Called on a
        MANUAL re-request so an explicit 'try again' overrides the blocklist. Returns the
        number of rows removed."""

        def operation(conn: sqlite3.Connection) -> int:
            cur = conn.execute(
                "DELETE FROM download_quarantine WHERE release_group_mbid = ?",
                (release_group_mbid,),
            )
            return cur.rowcount

        return await self._write(operation)

    async def list_quarantine(self, page: int = 1, page_size: int = 50) -> list[dict[str, Any]]:
        offset = max(0, (page - 1) * page_size)

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT * FROM download_quarantine ORDER BY quarantined_at DESC LIMIT ? OFFSET ?",
                (page_size, offset),
            ).fetchall()
            return [_quarantine_row_to_admin(dict(row)) for row in rows]

        return await self._read(operation)

    # -- held imports ("import anyway" review queue) --

    async def record_held_import(
        self,
        *,
        user_id: str,
        held_path: str,
        reason: str,
        source: str,
        source_task_id: str | None,
        release_group_mbid: str | None,
        release_mbid: str | None,
        recording_mbid: str | None,
        track_number: int | None,
        disc_number: int | None,
        track_title: str | None,
        artist_name: str | None,
        artist_mbid: str | None,
        album_title: str | None,
        year: int | None,
        original_filename: str | None,
        file_format: str | None,
        duration_seconds: float | None,
        evidence_title: str | None,
        evidence_artist: str | None,
        evidence_score: float | None,
        naming_template: str | None,
        origin: str = "user",
    ) -> int | None:
        """Hold a verify-rejected file for review. De-duped on (album, disc, track): if that
        track is already held, returns None so the caller can drop its extra copy instead of
        piling up one per edition it failed over through. Dedup needs a real track position -
        without one, two different unknown-track holds aren't the same track, so we keep both."""
        now = time.time()

        def operation(conn: sqlite3.Connection) -> int | None:
            if track_number is not None:
                dupe = conn.execute(
                    """SELECT id FROM held_imports
                       WHERE user_id = ? AND release_group_mbid IS ? AND disc_number IS ?
                         AND track_number = ? AND status = 'held' LIMIT 1""",
                    (user_id, release_group_mbid, disc_number, track_number),
                ).fetchone()
                if dupe is not None:
                    return None
            cur = conn.execute(
                """INSERT INTO held_imports
                   (user_id, release_group_mbid, release_mbid, recording_mbid, track_number,
                    disc_number, track_title, artist_name, artist_mbid, album_title, year,
                    held_path, original_filename, file_format, duration_seconds, reason,
                    evidence_title, evidence_artist, evidence_score, source, source_task_id,
                    origin, naming_template, status, created_at)
                   VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'held',?)""",
                (user_id, release_group_mbid, release_mbid, recording_mbid, track_number,
                 disc_number, track_title, artist_name, artist_mbid, album_title, year,
                 held_path, original_filename, file_format, duration_seconds, reason,
                 evidence_title, evidence_artist, evidence_score, source, source_task_id,
                 origin, naming_template, now),
            )
            return cur.lastrowid

        return await self._write(operation)

    async def list_held_imports(
        self, user_id: str, user_role: str, release_group_mbid: str | None = None
    ) -> list[HeldImport]:
        def operation(conn: sqlite3.Connection) -> list[HeldImport]:
            sql = "SELECT * FROM held_imports WHERE status = 'held'"
            params: list[Any] = []
            if user_role != "admin":
                sql += " AND user_id = ?"
                params.append(user_id)
            if release_group_mbid:
                sql += " AND release_group_mbid = ?"
                params.append(release_group_mbid)
            sql += " ORDER BY created_at DESC"
            return [_row_to_held(dict(r)) for r in conn.execute(sql, params).fetchall()]

        return await self._read(operation)

    async def get_held_import(
        self, held_id: int, user_id: str, user_role: str
    ) -> HeldImport | None:
        def operation(conn: sqlite3.Connection) -> HeldImport | None:
            row = conn.execute("SELECT * FROM held_imports WHERE id = ?", (held_id,)).fetchone()
            if row is None:
                return None
            held = _row_to_held(dict(row))
            if user_role != "admin" and held.user_id != user_id:
                return None
            return held

        return await self._read(operation)

    async def resolve_held_import(self, held_id: int, status: str) -> None:
        """Mark a held row imported/discarded (keeps the row for audit; the file itself is
        deleted by the caller on discard, or consumed by the move on import)."""
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE held_imports SET status = ?, resolved_at = ? WHERE id = ?",
                (status, now, held_id),
            )

        await self._write(operation)

    async def has_unresolved_held_for_task(self, source_task_id: str) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            row = conn.execute(
                "SELECT 1 FROM held_imports WHERE source_task_id = ? AND status = 'held' LIMIT 1",
                (source_task_id,),
            ).fetchone()
            return row is not None

        return await self._read(operation)

    async def task_ids_with_unresolved_held(self, user_id: str, user_role: str) -> set[str]:
        """The set of task ids that still have a held track under review - used to pause
        those tasks' auto-retry (they wait for the human, not another download)."""

        def operation(conn: sqlite3.Connection) -> set[str]:
            sql = (
                "SELECT DISTINCT source_task_id FROM held_imports "
                "WHERE status = 'held' AND source_task_id IS NOT NULL"
            )
            params: list[Any] = []
            if user_role != "admin":
                sql += " AND user_id = ?"
                params.append(user_id)
            return {r["source_task_id"] for r in conn.execute(sql, params).fetchall()}

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
        artist_mbid: str | None = None,
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
            artist_mbid=artist_mbid,
            search_query=search_query,
            status="searching",
            created_at=now,
            updated_at=now,
        )

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """INSERT INTO search_jobs
                   (id, user_id, artist_name, album_title, year, track_count,
                    release_group_mbid, artist_mbid, search_query, status, candidates_blob,
                    error_message, created_at, completed_at, updated_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '[]', NULL, ?, NULL, ?)""",
                (
                    job.id,
                    job.user_id,
                    job.artist_name,
                    job.album_title,
                    job.year,
                    job.track_count,
                    job.release_group_mbid,
                    job.artist_mbid,
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

    async def count_user_track_requests_since(self, user_id: str, since_epoch: float) -> int:
        """Track asks in the rolling request-quota window (D20). Tracks bypass the
        approval queue and have no request_history row, so their download task IS
        the ask - counted only for origin='user' (retries/upgrades aren't new asks).
        ``created_at`` is an epoch float (time.time()), so the window compares epoch."""

        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                """SELECT COUNT(*) FROM download_tasks
                   WHERE user_id = ? AND download_type = 'track'
                     AND origin = 'user' AND created_at >= ?""",
                (user_id, since_epoch),
            ).fetchone()
            return int(row[0])

        return await self._read(operation)

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
            # origin='upgrade' is excluded on BOTH sides (D18): outer, so a failed
            # upgrade never auto-retries; inner, so a newer upgrade task can't
            # suppress a user task's legitimate retry (and vice-versa).
            rows = conn.execute(
                """SELECT * FROM download_tasks t
                   WHERE t.status IN ('failed', 'partial')
                     AND t.origin != 'upgrade'
                     AND t.retry_count < ?
                     AND NOT EXISTS (
                         SELECT 1 FROM download_tasks n
                         WHERE n.user_id = t.user_id
                           AND n.download_type = t.download_type
                           AND n.release_group_mbid = t.release_group_mbid
                           AND COALESCE(n.recording_mbid, '') = COALESCE(t.recording_mbid, '')
                           AND n.origin != 'upgrade'
                           AND (n.created_at > t.created_at
                                OR (n.created_at = t.created_at AND n.rowid > t.rowid))
                     )
                   ORDER BY t.completed_at ASC NULLS LAST""",
                (max_retry_count,),
            ).fetchall()
            return [t for t in (_row_to_task(r) for r in rows) if t is not None]

        return await self._read(operation)

    async def list_tasks_by_status(
        self, user_id: str | None, user_role: str | None, statuses: list[str]
    ) -> list[DownloadTask]:
        """Every task in the given statuses (unpaginated), user-scoped exactly like
        ``list_tasks``: non-admins see only their own (fail closed if no user_id),
        admins span all users. Backs the bulk stop-retries / retry-all sweeps, which
        then partition the result by ``next_retry_at``."""
        if not statuses:
            return []
        clauses = [f"status IN ({_in_placeholders(statuses)})"]
        params: list[Any] = list(statuses)
        if user_role != "admin":
            if user_id is None:
                return []
            clauses.append("user_id = ?")
            params.append(user_id)
        where = " AND ".join(clauses)

        def operation(conn: sqlite3.Connection) -> list[DownloadTask]:
            rows = conn.execute(
                f"SELECT * FROM download_tasks WHERE {where} ORDER BY created_at DESC",
                tuple(params),
            ).fetchall()
            return [t for t in (_row_to_task(r) for r in rows) if t is not None]

        return await self._read(operation)

    async def delete_tasks_by_status(
        self, user_id: str | None, user_role: str | None, statuses: list[str]
    ) -> int:
        """Hard-delete the user's tasks in the given (terminal) statuses; user-scoped
        exactly like ``list_tasks`` (non-admins own-only and fail closed without a
        user_id, admins span all users). Returns the number of rows removed. Caller is
        responsible for passing only terminal statuses - this does no status guarding."""
        if not statuses:
            return 0
        clauses = [f"status IN ({_in_placeholders(statuses)})"]
        params: list[Any] = list(statuses)
        if user_role != "admin":
            if user_id is None:
                return 0
            clauses.append("user_id = ?")
            params.append(user_id)
        where = " AND ".join(clauses)

        def operation(conn: sqlite3.Connection) -> int:
            cur = conn.execute(f"DELETE FROM download_tasks WHERE {where}", tuple(params))
            return cur.rowcount

        return await self._write(operation)

    async def cancel_album_auto_retries(self, release_group_mbid: str) -> list[str]:
        """Cancel every ``failed``/``partial`` task for an album so it stops seeding
        auto-retries (a removed-from-library album must not keep re-downloading).
        Returns the cancelled task IDs. Active tasks (queued/downloading/processing)
        are left alone - those are cancelled per-task through ``cancel_task`` so their
        live transfers are torn down."""
        now = time.time()

        def operation(conn: sqlite3.Connection) -> list[str]:
            rows = conn.execute(
                "SELECT id FROM download_tasks "
                "WHERE release_group_mbid = ? AND status IN ('failed', 'partial')",
                (release_group_mbid,),
            ).fetchall()
            ids = [row["id"] for row in rows]
            if ids:
                conn.execute(
                    f"UPDATE download_tasks SET status = 'cancelled', cancelled_at = ?, "
                    f"updated_at = ? WHERE id IN ({_in_placeholders(ids)})",
                    (now, now, *ids),
                )
            return ids

        return await self._write(operation)

    async def purge_album_artifacts(self, release_group_mbid: str) -> list[str]:
        """On library removal, drop an album's held-import rows and blocklist entries, and
        return the held files' on-disk paths so the caller can unlink them. Retries are
        cancelled separately (``cancel_album_auto_retries``); together they ensure a removed
        album leaves no held 'Couldn't verify' tracks or blocklist behind."""

        def operation(conn: sqlite3.Connection) -> list[str]:
            held_paths = [
                row["held_path"]
                for row in conn.execute(
                    "SELECT held_path FROM held_imports WHERE release_group_mbid = ?",
                    (release_group_mbid,),
                ).fetchall()
            ]
            conn.execute(
                "DELETE FROM held_imports WHERE release_group_mbid = ?", (release_group_mbid,)
            )
            conn.execute(
                "DELETE FROM download_quarantine WHERE release_group_mbid = ?",
                (release_group_mbid,),
            )
            return held_paths

        return await self._write(operation)


def _in_placeholders(items: Any) -> str:
    return ", ".join("?" for _ in items)


def _row_to_task(row: sqlite3.Row | None) -> DownloadTask | None:
    if row is None:
        return None
    return msgspec.convert(dict(row), type=DownloadTask, strict=False)


# source -> the download client_type that owns it (fixed v1 map).
_SOURCE_CLIENT_TYPE = {"soulseek": "slskd", "usenet": "sabnzbd"}


def _quarantine_row_to_admin(row: dict[str, Any]) -> dict[str, Any]:
    """Project a ``(source, identity, …)`` quarantine row onto the legacy admin API
    shape (``client_id``/``username``/``filename``) so the existing admin list +
    frontend keep working after the table rebuild (D8). A soulseek identity splits
    back into ``(username, filename)``; a usenet identity (title+size) has no
    username, so it surfaces under ``filename`` with an empty ``username`` until the
    quarantine UI gets a source-aware pass."""
    source = row.get("source", "soulseek")
    identity = row.get("identity", "")
    username, sep, filename = identity.partition(_SOULSEEK_ID_SEP)
    if source == "soulseek" and sep:
        username, filename = username, filename
    else:
        username, filename = "", identity
    return {
        "id": row.get("id"),
        "source": source,
        "client_id": _SOURCE_CLIENT_TYPE.get(source, source),
        "username": username,
        "filename": filename,
        "identity": identity,
        "reason": row.get("reason"),
        "quarantined_at": row.get("quarantined_at"),
        "release_group_mbid": row.get("release_group_mbid"),
    }


def _row_to_held(row: dict[str, Any]) -> HeldImport:
    # column names mirror HeldImport's fields exactly
    return HeldImport(**row)


def _row_to_search_job(row: sqlite3.Row | None) -> SearchJob | None:
    if row is None:
        return None
    return msgspec.convert(dict(row), type=SearchJob, strict=False)
