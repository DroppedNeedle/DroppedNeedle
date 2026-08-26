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
from models.download import (
    DownloadActivitySummary,
    DownloadTask,
    ScoredCandidate,
    SearchJob,
)
from models.download_attempt import DownloadAttempt, DownloadCleanupReconciliation
from models.held_import import HeldImport
from repositories.protocols.download_client import TaskHandle

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

# Held imports: verified acquisition bytes copied into app-owned storage when either the
# recording-identity backstop or automatic Library Management blocks publication.
# Identity holds de-duplicate by release position. Management holds are replaced as one
# task-scoped acquisition unit so an interrupted write cannot masquerade as a complete album.
_HELD_IMPORTS_DDL = """
CREATE TABLE IF NOT EXISTS held_imports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL,
    release_group_mbid TEXT,
    release_mbid TEXT,
    release_track_mbid TEXT,
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
    reason_detail TEXT,
    evidence_title TEXT,
    evidence_artist TEXT,
    evidence_score REAL,
    source TEXT NOT NULL DEFAULT 'soulseek',
    source_task_id TEXT,
    -- The owning task's origin, persisted here because the task itself is deletable
    -- (clear_finished): the D10 confirm-replace must survive a cleared queue.
    origin TEXT NOT NULL DEFAULT 'user',
    naming_template TEXT,
    management_retry_count INTEGER NOT NULL DEFAULT 0,
    management_next_retry_at REAL,
    status TEXT NOT NULL DEFAULT 'held'
        CHECK(status IN ('held','imported','discarded')),
    created_at REAL NOT NULL,
    resolved_at REAL,
    file_cleanup_completed_at REAL
);
CREATE INDEX IF NOT EXISTS idx_held_user ON held_imports(user_id, status);
CREATE INDEX IF NOT EXISTS idx_held_rg ON held_imports(release_group_mbid, status);
CREATE INDEX IF NOT EXISTS idx_held_task ON held_imports(source_task_id, status);
CREATE INDEX IF NOT EXISTS idx_held_dedup
    ON held_imports(release_group_mbid, disc_number, track_number, status);
"""

# No task foreign key: queue/history deletion must not erase cleanup debt.
_DOWNLOAD_ATTEMPTS_DDL = """
CREATE TABLE IF NOT EXISTS download_attempts (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('soulseek','usenet')),
    candidate_index INTEGER NOT NULL CHECK(candidate_index >= 0),
    job_name TEXT NOT NULL DEFAULT '',
    handle_json TEXT NOT NULL,
    remote_storage TEXT,
    mount_root TEXT,
    workspace_path TEXT,
    materialized_paths_json TEXT NOT NULL DEFAULT '[]',
    materialized_fingerprints_json TEXT NOT NULL DEFAULT '{}',
    publisher_bundle_ids_json TEXT NOT NULL DEFAULT '[]',
    legacy_reconciled INTEGER NOT NULL DEFAULT 0 CHECK(legacy_reconciled IN (0,1)),
    state TEXT NOT NULL CHECK(state IN (
        'acquiring','in_use','cleanup_pending','workspace_removed','complete',
        'preserved','needs_attention'
    )),
    disposition TEXT NOT NULL DEFAULT 'undecided'
        CHECK(disposition IN ('undecided','discard','preserve')),
    cleanup_failures INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_failures >= 0),
    next_retry_at REAL NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at REAL,
    error_code TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    completed_at REAL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);
CREATE INDEX IF NOT EXISTS idx_download_attempts_task
    ON download_attempts(task_id, candidate_index, created_at);
CREATE INDEX IF NOT EXISTS idx_download_attempts_cleanup
    ON download_attempts(state, next_retry_at, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_download_attempts_job
    ON download_attempts(source, job_name);

CREATE TABLE IF NOT EXISTS download_cleanup_reconciliation (
    mount_key TEXT PRIMARY KEY,
    mount_root TEXT NOT NULL,
    pending_directories_json TEXT NOT NULL,
    current_directory TEXT,
    last_entry TEXT,
    completed INTEGER NOT NULL DEFAULT 0 CHECK(completed IN (0,1)),
    updated_at REAL NOT NULL
);
"""

# Global badges need structural queue changes, not progress writes. SQLite triggers
# keep this revision correct across every producer, including future direct status
# updates and cascaded deletes. User and global counters preserve list-route ownership:
# admins see all tasks while other roles see only their own.
_DOWNLOAD_ACTIVITY_DDL = """
CREATE TABLE IF NOT EXISTS download_activity_global_revision (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    revision INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO download_activity_global_revision (singleton, revision)
VALUES (1, 0);

CREATE TABLE IF NOT EXISTS download_activity_user_revisions (
    user_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL DEFAULT 0
);

CREATE TRIGGER IF NOT EXISTS download_activity_task_insert
AFTER INSERT ON download_tasks
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_task_status
AFTER UPDATE OF status ON download_tasks
WHEN OLD.status IS NOT NEW.status
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_task_search_link
AFTER UPDATE OF search_job_id, candidate_index ON download_tasks
WHEN OLD.search_job_id IS NOT NEW.search_job_id
    OR OLD.candidate_index IS NOT NEW.candidate_index
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_task_owner
AFTER UPDATE OF user_id ON download_tasks
WHEN OLD.user_id IS NOT NEW.user_id
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_task_delete
AFTER DELETE ON download_tasks
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_held_insert
AFTER INSERT ON held_imports
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_held_status
AFTER UPDATE OF status ON held_imports
WHEN OLD.status IS NOT NEW.status
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_held_owner
AFTER UPDATE OF user_id ON held_imports
WHEN OLD.user_id IS NOT NEW.user_id
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_held_delete
AFTER DELETE ON held_imports
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;
"""

_DOWNLOAD_ATTEMPT_ACTIVITY_DDL = """
CREATE TRIGGER IF NOT EXISTS download_activity_attempt_insert
AFTER INSERT ON download_attempts
WHEN EXISTS (SELECT 1 FROM download_tasks WHERE id = NEW.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = NEW.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_attempt_state
AFTER UPDATE OF state ON download_attempts
WHEN OLD.state IS NOT NEW.state
    AND EXISTS (SELECT 1 FROM download_tasks WHERE id = NEW.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = NEW.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_attempt_delete
AFTER DELETE ON download_attempts
WHEN EXISTS (SELECT 1 FROM download_tasks WHERE id = OLD.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = OLD.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;
"""

# ASCII unit separator joining the two halves of a soulseek identity; mirrors
# ``models.download_identity.soulseek_identity`` so a legacy (username, filename)
# quarantine row migrates to the same key the scorer will look up.
_SOULSEEK_ID_SEP = "\x1f"

# Columns on download_tasks that update_status (and friends) may set directly.
_TASK_UPDATABLE = frozenset(
    {
        "release_mbid",
        "release_track_mbid",
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
        "advertised_queue_depth",
        "queue_position_start",
        "queue_position_end",
        "remote_queued",
        "preferred_quality_fallback_at",
        "quality_pool_key",
        "attempt_number",
        "attempt_total",
        "has_next_source",
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
    "release_track_mbid",
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
    "content_variant",
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
    "advertised_queue_depth",
    "queue_position_start",
    "queue_position_end",
    "remote_queued",
    "preferred_quality_fallback_at",
    "quality_pool_key",
    "attempt_number",
    "attempt_total",
    "has_next_source",
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

_ATTEMPT_CAS_UPDATABLE = frozenset(
    {
        "disposition",
        "remote_storage",
        "mount_root",
        "workspace_path",
        "materialized_paths_json",
        "materialized_fingerprints_json",
        "publisher_bundle_ids_json",
        "cleanup_failures",
        "next_retry_at",
        "lease_owner",
        "lease_expires_at",
        "error_code",
        "completed_at",
        "handle_json",
    }
)


def _safe_alter(conn: sqlite3.Connection, sql: str) -> None:
    try:
        conn.execute(sql)
    except sqlite3.OperationalError as error:
        if "duplicate column" not in str(error).lower():
            raise


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
                    release_track_mbid TEXT,
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
                    content_variant TEXT NOT NULL DEFAULT 'original',
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
                    advertised_queue_depth INTEGER,
                    queue_position_start INTEGER,
                    queue_position_end INTEGER,
                    remote_queued INTEGER NOT NULL DEFAULT 0,
                    preferred_quality_fallback_at REAL,
                    quality_pool_key TEXT,
                    attempt_number INTEGER NOT NULL DEFAULT 0,
                    attempt_total INTEGER NOT NULL DEFAULT 0,
                    has_next_source INTEGER NOT NULL DEFAULT 0,
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
                ("release_track_mbid", "TEXT"),
                ("source", "TEXT NOT NULL DEFAULT 'soulseek'"),
                ("origin", "TEXT NOT NULL DEFAULT 'user'"),
                ("content_variant", "TEXT NOT NULL DEFAULT 'original'"),
                ("advertised_queue_depth", "INTEGER"),
                ("queue_position_start", "INTEGER"),
                ("queue_position_end", "INTEGER"),
                ("remote_queued", "INTEGER NOT NULL DEFAULT 0"),
                ("preferred_quality_fallback_at", "REAL"),
                ("quality_pool_key", "TEXT"),
                ("attempt_number", "INTEGER NOT NULL DEFAULT 0"),
                ("attempt_total", "INTEGER NOT NULL DEFAULT 0"),
                ("has_next_source", "INTEGER NOT NULL DEFAULT 0"),
            ):
                try:
                    conn.execute(
                        f"ALTER TABLE download_tasks ADD COLUMN {column} {ddl}"
                    )
                except sqlite3.OperationalError:
                    pass  # duplicate column - already present
            try:
                conn.execute("ALTER TABLE search_jobs ADD COLUMN artist_mbid TEXT")
            except sqlite3.OperationalError:
                pass  # duplicate column - already present
            self._migrate_quarantine(conn)
            conn.executescript(_HELD_IMPORTS_DDL)
            conn.executescript(_DOWNLOAD_ACTIVITY_DDL)
            conn.executescript(_DOWNLOAD_ATTEMPTS_DDL)
            conn.executescript(_DOWNLOAD_ATTEMPT_ACTIVITY_DDL)
            _safe_alter(
                conn,
                "ALTER TABLE download_attempts ADD COLUMN legacy_reconciled "
                "INTEGER NOT NULL DEFAULT 0 CHECK(legacy_reconciled IN (0,1))",
            )
            _safe_alter(
                conn,
                "ALTER TABLE download_attempts ADD COLUMN materialized_fingerprints_json "
                "TEXT NOT NULL DEFAULT '{}'",
            )
            for column, ddl in (
                ("artist_mbid", "TEXT"),
                ("origin", "TEXT NOT NULL DEFAULT 'user'"),
                ("reason_detail", "TEXT"),
                ("release_track_mbid", "TEXT"),
                ("management_retry_count", "INTEGER NOT NULL DEFAULT 0"),
                ("management_next_retry_at", "REAL"),
                ("file_cleanup_completed_at", "REAL"),
            ):
                try:
                    conn.execute(f"ALTER TABLE held_imports ADD COLUMN {column} {ddl}")
                except sqlite3.OperationalError:
                    pass  # duplicate column - already present
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_held_management_retry "
                "ON held_imports(management_next_retry_at, status)"
            )
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
            conn.execute(
                "ALTER TABLE download_quarantine RENAME TO download_quarantine_legacy"
            )
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
        release_track_mbid: str | None = None,
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
        content_variant: str = "original",
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
            release_track_mbid=release_track_mbid,
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
            content_variant=content_variant,
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

    async def pin_task_release_mbid(
        self, task_id: str, release_group_mbid: str, release_mbid: str
    ) -> str:
        """Pin a legacy task's first verified exact edition without allowing drift."""

        def operation(conn: sqlite3.Connection) -> str:
            row = conn.execute(
                "SELECT release_group_mbid, release_mbid FROM download_tasks WHERE id = ?",
                (task_id,),
            ).fetchone()
            if row is None:
                raise ValueError("The acquisition task no longer exists")
            if (
                not row["release_group_mbid"]
                or str(row["release_group_mbid"]).casefold()
                != release_group_mbid.casefold()
            ):
                raise ValueError("The acquisition task changed before edition pinning")
            existing = row["release_mbid"]
            if existing:
                if str(existing).casefold() != release_mbid.casefold():
                    raise ValueError(
                        "The acquisition task already pins another edition"
                    )
                return str(existing)
            conn.execute(
                "UPDATE download_tasks SET release_mbid = ?, updated_at = ? WHERE id = ?",
                (release_mbid, time.time(), task_id),
            )
            return release_mbid

        return await self._write(operation)

    async def get_parked_task_for_search_job(
        self, search_job_id: str
    ) -> DownloadTask | None:
        """The orchestrator task PARKED on this search job awaiting review: linked to
        the job, no candidate picked, still queued. ``pick_candidate`` RESUMES it - a
        fresh task would drop the threaded single-track identity (search_jobs carries
        none) and the request linkage (terminal sync matches on the task id). See
        .dev-notes/Bugs/2026-07-05-wrong-single-remediation-plan.md, P1.4."""

        def operation(conn: sqlite3.Connection) -> DownloadTask | None:
            row = conn.execute(
                "SELECT * FROM download_tasks WHERE search_job_id = ?"
                " AND candidate_index IS NULL AND status = 'queued'"
                " ORDER BY created_at DESC LIMIT 1",
                (search_job_id,),
            ).fetchone()
            return _row_to_task(row)

        return await self._read(operation)

    async def get_reimportable_task_ids(self, task_ids: list[str]) -> set[str]:
        if not task_ids:
            return set()

        def operation(conn: sqlite3.Connection) -> set[str]:
            placeholders = ",".join("?" * len(task_ids))
            rows = conn.execute(
                f"SELECT id FROM download_tasks WHERE id IN ({placeholders})"
                " AND status IN ('failed', 'partial')"
                " AND source_username IS NOT NULL"
                " AND search_job_id IS NOT NULL"
                " AND candidate_index IS NOT NULL",
                task_ids,
            ).fetchall()
            return {row["id"] for row in rows}

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

    async def get_activity_summary(
        self, user_id: str, user_role: str
    ) -> DownloadActivitySummary:
        """Return one compact ownership-scoped activity projection.

        The four SQL statements share one read connection. The response stays
        bounded regardless of queue history: only counts, a structural revision,
        and the 20 most recently landed release groups cross the HTTP boundary.
        """

        is_admin = user_role == "admin"

        def operation(conn: sqlite3.Connection) -> DownloadActivitySummary:
            if is_admin:
                revision_row = conn.execute(
                    "SELECT revision FROM download_activity_global_revision "
                    "WHERE singleton = 1"
                ).fetchone()
                task_where = ""
                task_params: tuple[str, ...] = ()
                held_where = "status = 'held'"
                held_params: tuple[str, ...] = ()
            else:
                revision_row = conn.execute(
                    "SELECT revision FROM download_activity_user_revisions "
                    "WHERE user_id = ?",
                    (user_id,),
                ).fetchone()
                task_where = "WHERE user_id = ?"
                task_params = (user_id,)
                held_where = "status = 'held' AND user_id = ?"
                held_params = (user_id,)

            counts = conn.execute(
                "SELECT "
                "COALESCE(SUM(CASE WHEN status IN ('queued','downloading','processing') "
                "THEN 1 ELSE 0 END), 0) AS active_count, "
                "COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) "
                f"AS failed_count FROM download_tasks {task_where}",
                task_params,
            ).fetchone()
            held = conn.execute(
                f"SELECT COUNT(*) AS count FROM held_imports WHERE {held_where}",
                held_params,
            ).fetchone()

            landed_scope = "" if is_admin else "AND user_id = ?"
            landed = conn.execute(
                "SELECT release_group_mbid FROM download_tasks "
                "WHERE status IN ('completed','partial') "
                "AND release_group_mbid != '' "
                f"{landed_scope} "
                "GROUP BY release_group_mbid "
                "ORDER BY MAX(COALESCE(completed_at, updated_at)) DESC LIMIT 20",
                task_params,
            ).fetchall()

            return DownloadActivitySummary(
                revision=int(revision_row["revision"] if revision_row else 0),
                active_count=int(counts["active_count"] if counts else 0),
                held_count=int(held["count"] if held else 0),
                failed_count=int(counts["failed_count"] if counts else 0),
                landed_release_group_mbids=[
                    str(row["release_group_mbid"]) for row in landed
                ],
            )

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
        queue_position_start: int | None = None,
        queue_position_end: int | None = None,
        remote_queued: bool = False,
    ) -> None:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """UPDATE download_tasks
                   SET downloaded_bytes = ?, files_completed = ?, progress_percent = ?,
                       queue_position_start = ?, queue_position_end = ?,
                       remote_queued = ?,
                       preferred_quality_fallback_at = CASE
                           WHEN ? > 0 THEN NULL ELSE preferred_quality_fallback_at END,
                       last_polled_at = ?, updated_at = ?
                   WHERE id = ?""",
                (
                    bytes_downloaded,
                    files_completed,
                    progress_percent,
                    queue_position_start,
                    queue_position_end,
                    int(remote_queued),
                    bytes_downloaded,
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

    async def create_download_attempt(
        self,
        *,
        task_id: str,
        source: str,
        candidate_index: int,
        job_name: str,
        handle: TaskHandle,
        attempt_id: str | None = None,
        now: float | None = None,
    ) -> DownloadAttempt:
        timestamp = time.time() if now is None else now
        attempt = DownloadAttempt(
            id=attempt_id or uuid.uuid4().hex,
            task_id=task_id,
            source=source,
            candidate_index=candidate_index,
            job_name=job_name,
            handle=handle,
            state="acquiring",
            disposition="undecided",
            created_at=timestamp,
            updated_at=timestamp,
        )

        def operation(conn: sqlite3.Connection) -> DownloadAttempt:
            conn.execute(
                """INSERT INTO download_attempts
                   (id,task_id,source,candidate_index,job_name,handle_json,state,
                    disposition,created_at,updated_at)
                   VALUES (?,?,?,?,?,?,?, ?,?,?)""",
                (
                    attempt.id,
                    attempt.task_id,
                    attempt.source,
                    attempt.candidate_index,
                    attempt.job_name,
                    _encode_json(to_jsonable(attempt.handle)),
                    attempt.state,
                    attempt.disposition,
                    attempt.created_at,
                    attempt.updated_at,
                ),
            )
            return attempt

        return await self._write(operation)

    async def get_download_attempt(self, attempt_id: str) -> DownloadAttempt | None:
        def operation(conn: sqlite3.Connection) -> DownloadAttempt | None:
            row = conn.execute(
                "SELECT * FROM download_attempts WHERE id=?", (attempt_id,)
            ).fetchone()
            return _row_to_attempt(row)

        return await self._read(operation)

    async def list_download_attempts(self, task_id: str) -> list[DownloadAttempt]:
        def operation(conn: sqlite3.Connection) -> list[DownloadAttempt]:
            rows = conn.execute(
                "SELECT * FROM download_attempts WHERE task_id=? "
                "ORDER BY candidate_index,created_at,id",
                (task_id,),
            ).fetchall()
            return [
                value for row in rows if (value := _row_to_attempt(row)) is not None
            ]

        return await self._read(operation)

    async def get_download_attempt_for_job(
        self, source: str, job_name: str
    ) -> DownloadAttempt | None:
        def operation(conn: sqlite3.Connection) -> DownloadAttempt | None:
            row = conn.execute(
                "SELECT * FROM download_attempts WHERE source=? AND job_name=? "
                "ORDER BY created_at DESC,id DESC LIMIT 1",
                (source, job_name),
            ).fetchone()
            return _row_to_attempt(row)

        return await self._read(operation)

    async def get_download_attempt_for_candidate(
        self, task_id: str, source: str, candidate_index: int
    ) -> DownloadAttempt | None:
        def operation(conn: sqlite3.Connection) -> DownloadAttempt | None:
            row = conn.execute(
                "SELECT * FROM download_attempts "
                "WHERE task_id=? AND source=? AND candidate_index=? "
                "ORDER BY created_at DESC,id DESC LIMIT 1",
                (task_id, source, candidate_index),
            ).fetchone()
            return _row_to_attempt(row)

        return await self._read(operation)

    async def update_download_attempt_handle(
        self,
        attempt_id: str,
        handle: TaskHandle,
        *,
        now: float | None = None,
    ) -> DownloadAttempt:
        timestamp = time.time() if now is None else now

        def operation(conn: sqlite3.Connection) -> DownloadAttempt:
            row = conn.execute(
                """UPDATE download_attempts
                   SET handle_json=?,state='in_use',updated_at=?,row_revision=row_revision+1
                   WHERE id=? AND state IN ('acquiring','in_use') RETURNING *""",
                (_encode_json(to_jsonable(handle)), timestamp, attempt_id),
            ).fetchone()
            if row is None:
                raise ValueError("download attempt is no longer acquiring")
            value = _row_to_attempt(row)
            if value is None:
                raise ValueError("download attempt disappeared")
            return value

        return await self._write(operation)

    async def transition_download_attempt(
        self,
        attempt_id: str,
        *,
        expected_row_revision: int,
        new_state: str,
        now: float | None = None,
        **fields: Any,
    ) -> DownloadAttempt | None:
        """CAS one cleanup transition. ``None`` means another worker won the lease."""

        timestamp = time.time() if now is None else now
        sets = ["state=?", "updated_at=?", "row_revision=row_revision+1"]
        params: list[Any] = [new_state, timestamp]
        for key, value in fields.items():
            if key not in _ATTEMPT_CAS_UPDATABLE:
                raise ValueError(f"download_attempts column not updatable: {key}")
            sets.append(f"{key}=?")
            params.append(value)
        params.extend((attempt_id, expected_row_revision))

        def operation(conn: sqlite3.Connection) -> DownloadAttempt | None:
            row = conn.execute(
                f"UPDATE download_attempts SET {', '.join(sets)} "
                "WHERE id=? AND row_revision=? RETURNING *",
                tuple(params),
            ).fetchone()
            return _row_to_attempt(row)

        return await self._write(operation)

    async def schedule_download_attempt_cleanup(
        self,
        attempt_id: str,
        *,
        disposition: str,
        publisher_bundle_ids: list[str] | None = None,
        now: float | None = None,
    ) -> DownloadAttempt:
        timestamp = time.time() if now is None else now
        state = "cleanup_pending" if disposition == "discard" else "preserved"
        bundles = _encode_json(publisher_bundle_ids or [])

        def operation(conn: sqlite3.Connection) -> DownloadAttempt:
            row = conn.execute(
                """UPDATE download_attempts
                   SET state=?,disposition=?,publisher_bundle_ids_json=?,next_retry_at=?,
                       lease_owner=NULL,lease_expires_at=NULL,error_code=NULL,updated_at=?,
                       row_revision=row_revision+1
                   WHERE id=? AND state NOT IN ('complete','needs_attention') RETURNING *""",
                (state, disposition, bundles, timestamp, timestamp, attempt_id),
            ).fetchone()
            if row is None:
                row = conn.execute(
                    "SELECT * FROM download_attempts WHERE id=?", (attempt_id,)
                ).fetchone()
            value = _row_to_attempt(row)
            if value is None:
                raise ValueError("download attempt not found")
            return value

        return await self._write(operation)

    async def finalize_task_and_attempt(
        self,
        task_id: str,
        status: str,
        *,
        task_fields: dict[str, Any],
        attempt_id: str | None,
        disposition: str | None,
        publisher_bundle_ids: list[str] | None = None,
        now: float | None = None,
    ) -> None:
        """Persist the user-visible result and final cleanup obligation atomically."""

        timestamp = time.time() if now is None else now
        sets = ["status=?", "updated_at=?"]
        params: list[Any] = [status, timestamp]
        for key, value in task_fields.items():
            if key not in _TASK_UPDATABLE:
                raise ValueError(f"download_tasks column not updatable: {key}")
            sets.append(f"{key}=?")
            params.append(value)
        params.append(task_id)

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                f"UPDATE download_tasks SET {', '.join(sets)} WHERE id=?",
                tuple(params),
            )
            if attempt_id is None or disposition is None:
                return
            attempt_state = (
                "cleanup_pending" if disposition == "discard" else "preserved"
            )
            conn.execute(
                """UPDATE download_attempts
                   SET state=?,disposition=?,publisher_bundle_ids_json=?,next_retry_at=?,
                       lease_owner=NULL,lease_expires_at=NULL,error_code=NULL,updated_at=?,
                       row_revision=row_revision+1
                   WHERE id=? AND state NOT IN ('complete','needs_attention')""",
                (
                    attempt_state,
                    disposition,
                    _encode_json(publisher_bundle_ids or []),
                    timestamp,
                    timestamp,
                    attempt_id,
                ),
            )

        await self._write(operation)

    async def cancel_task_and_schedule_attempts(
        self,
        task_id: str,
        *,
        publisher_bundle_ids: list[str] | None = None,
        cleanup_disposition: str = "discard",
        cancelled_at: float | None = None,
    ) -> list[str]:
        if cleanup_disposition not in {"discard", "preserve"}:
            raise ValueError("invalid cancellation cleanup disposition")
        now = time.time() if cancelled_at is None else cancelled_at
        attempt_state = (
            "cleanup_pending" if cleanup_disposition == "discard" else "preserved"
        )

        def operation(conn: sqlite3.Connection) -> list[str]:
            conn.execute(
                "UPDATE download_tasks SET status='cancelled',cancelled_at=?,updated_at=?,"
                "queue_position_start=NULL,queue_position_end=NULL,remote_queued=0,"
                "preferred_quality_fallback_at=NULL,has_next_source=0 "
                "WHERE id=?",
                (now, now, task_id),
            )
            rows = conn.execute(
                "SELECT id FROM download_attempts WHERE task_id=? "
                "AND state NOT IN ('complete','needs_attention')",
                (task_id,),
            ).fetchall()
            ids = [str(row["id"]) for row in rows]
            if ids:
                conn.execute(
                    f"UPDATE download_attempts SET state=?,disposition=?,"
                    f"publisher_bundle_ids_json=?,"
                    f"next_retry_at=?,lease_owner=NULL,"
                    f"lease_expires_at=NULL,error_code=NULL,updated_at=?,"
                    f"row_revision=row_revision+1 WHERE id IN ({_in_placeholders(ids)})",
                    (
                        attempt_state,
                        cleanup_disposition,
                        _encode_json(publisher_bundle_ids or []),
                        now,
                        now,
                        *ids,
                    ),
                )
            return ids

        return await self._write(operation)

    async def claim_download_cleanup_attempts(
        self,
        worker_id: str,
        *,
        now: float | None = None,
        limit: int = 25,
        lease_seconds: float = 300.0,
    ) -> list[DownloadAttempt]:
        timestamp = time.time() if now is None else now

        def operation(conn: sqlite3.Connection) -> list[DownloadAttempt]:
            rows = conn.execute(
                """SELECT id FROM download_attempts
                   WHERE state IN ('cleanup_pending','workspace_removed','needs_attention')
                     AND next_retry_at<=?
                     AND (lease_expires_at IS NULL OR lease_expires_at<=?)
                   ORDER BY next_retry_at,created_at,id LIMIT ?""",
                (timestamp, timestamp, min(25, max(1, limit))),
            ).fetchall()
            ids = [str(row["id"]) for row in rows]
            if not ids:
                return []
            conn.execute(
                f"UPDATE download_attempts SET lease_owner=?,lease_expires_at=?,"
                f"updated_at=?,row_revision=row_revision+1 "
                f"WHERE id IN ({_in_placeholders(ids)})",
                (worker_id, timestamp + lease_seconds, timestamp, *ids),
            )
            claimed = conn.execute(
                f"SELECT * FROM download_attempts WHERE id IN ({_in_placeholders(ids)}) "
                "ORDER BY next_retry_at,created_at,id",
                tuple(ids),
            ).fetchall()
            return [
                value for row in claimed if (value := _row_to_attempt(row)) is not None
            ]

        return await self._write(operation)

    async def claim_download_cleanup_attempt(
        self,
        attempt_id: str,
        worker_id: str,
        *,
        now: float | None = None,
        lease_seconds: float = 300.0,
    ) -> DownloadAttempt | None:
        timestamp = time.time() if now is None else now

        def operation(conn: sqlite3.Connection) -> DownloadAttempt | None:
            row = conn.execute(
                """UPDATE download_attempts
                   SET lease_owner=?,lease_expires_at=?,updated_at=?,
                       row_revision=row_revision+1
                   WHERE id=? AND state IN (
                       'cleanup_pending','workspace_removed','needs_attention'
                   )
                     AND next_retry_at<=?
                     AND (lease_expires_at IS NULL OR lease_expires_at<=?)
                   RETURNING *""",
                (
                    worker_id,
                    timestamp + lease_seconds,
                    timestamp,
                    attempt_id,
                    timestamp,
                    timestamp,
                ),
            ).fetchone()
            return _row_to_attempt(row)

        return await self._write(operation)

    async def acquire_download_attempt_for_reimport(
        self, attempt_id: str, *, now: float | None = None
    ) -> DownloadAttempt | None:
        timestamp = time.time() if now is None else now

        def operation(conn: sqlite3.Connection) -> DownloadAttempt | None:
            row = conn.execute(
                """UPDATE download_attempts
                   SET state='in_use',disposition='undecided',next_retry_at=0,
                       lease_owner=NULL,lease_expires_at=NULL,error_code=NULL,
                       completed_at=NULL,updated_at=?,row_revision=row_revision+1
                   WHERE id=?
                     AND state IN ('cleanup_pending','preserved','needs_attention')
                     AND (lease_expires_at IS NULL OR lease_expires_at<=?)
                   RETURNING *""",
                (timestamp, attempt_id, timestamp),
            ).fetchone()
            return _row_to_attempt(row)

        return await self._write(operation)

    async def record_download_cleanup_failure(
        self,
        attempt_id: str,
        *,
        expected_row_revision: int,
        error_code: str,
        now: float | None = None,
    ) -> DownloadAttempt | None:
        timestamp = time.time() if now is None else now
        delays = (60.0, 300.0, 900.0, 3600.0)

        def operation(conn: sqlite3.Connection) -> DownloadAttempt | None:
            current = conn.execute(
                "SELECT cleanup_failures FROM download_attempts "
                "WHERE id=? AND row_revision=?",
                (attempt_id, expected_row_revision),
            ).fetchone()
            if current is None:
                return None
            failures = int(current["cleanup_failures"]) + 1
            delay = delays[min(failures - 1, len(delays) - 1)]
            row = conn.execute(
                """UPDATE download_attempts
                   SET cleanup_failures=?,next_retry_at=?,lease_owner=NULL,
                       lease_expires_at=NULL,error_code=?,updated_at=?,
                       row_revision=row_revision+1
                   WHERE id=? AND row_revision=? RETURNING *""",
                (
                    failures,
                    timestamp + delay,
                    error_code,
                    timestamp,
                    attempt_id,
                    expected_row_revision,
                ),
            ).fetchone()
            return _row_to_attempt(row)

        return await self._write(operation)

    async def cleanup_states_for_tasks(self, task_ids: list[str]) -> dict[str, str]:
        if not task_ids:
            return {}

        def operation(conn: sqlite3.Connection) -> dict[str, str]:
            rows = conn.execute(
                f"""SELECT task_id,
                    CASE
                      WHEN MAX(state='needs_attention') THEN 'needs_attention'
                      WHEN MAX(state='preserved') THEN 'preserved'
                      WHEN MAX(state IN ('cleanup_pending','workspace_removed')) THEN 'pending'
                      WHEN MAX(state IN ('acquiring','in_use')) THEN 'in_use'
                      WHEN MAX(state='complete') THEN 'complete'
                      ELSE 'not_tracked'
                    END AS cleanup_state
                   FROM download_attempts
                   WHERE task_id IN ({_in_placeholders(task_ids)})
                   GROUP BY task_id""",
                tuple(task_ids),
            ).fetchall()
            return {str(row["task_id"]): str(row["cleanup_state"]) for row in rows}

        return await self._read(operation)

    async def cleanup_warning_count(self) -> int:
        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                """SELECT COUNT(DISTINCT task_id) AS count FROM download_attempts
                   WHERE state='needs_attention'
                      OR (state IN ('cleanup_pending','workspace_removed')
                          AND cleanup_failures>=3)"""
            ).fetchone()
            return int(row["count"] if row is not None else 0)

        return await self._read(operation)

    async def prune_completed_download_attempts(
        self, *, older_than: float, limit: int = 1000
    ) -> int:
        def operation(conn: sqlite3.Connection) -> int:
            rows = conn.execute(
                "SELECT id FROM download_attempts WHERE state='complete' "
                "AND completed_at<? ORDER BY completed_at LIMIT ?",
                (older_than, limit),
            ).fetchall()
            ids = [str(row["id"]) for row in rows]
            if not ids:
                return 0
            conn.execute(
                f"DELETE FROM download_attempts WHERE id IN ({_in_placeholders(ids)})",
                tuple(ids),
            )
            return len(ids)

        return await self._write(operation)

    async def ensure_cleanup_reconciliation(
        self, mount_key: str, mount_root: str, *, now: float | None = None
    ) -> DownloadCleanupReconciliation:
        timestamp = time.time() if now is None else now

        def operation(conn: sqlite3.Connection) -> DownloadCleanupReconciliation:
            conn.execute(
                """INSERT OR IGNORE INTO download_cleanup_reconciliation
                   (mount_key,mount_root,pending_directories_json,completed,updated_at)
                   VALUES (?,?,'[\".\"]',0,?)""",
                (mount_key, mount_root, timestamp),
            )
            row = conn.execute(
                "SELECT * FROM download_cleanup_reconciliation WHERE mount_key=?",
                (mount_key,),
            ).fetchone()
            value = _row_to_reconciliation(row)
            if value is None:
                raise ValueError("cleanup reconciliation disappeared")
            return value

        return await self._write(operation)

    async def save_cleanup_reconciliation(
        self,
        value: DownloadCleanupReconciliation,
        *,
        now: float | None = None,
    ) -> None:
        timestamp = time.time() if now is None else now

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """UPDATE download_cleanup_reconciliation
                   SET pending_directories_json=?,current_directory=?,last_entry=?,
                       completed=?,updated_at=? WHERE mount_key=?""",
                (
                    _encode_json(value.pending_directories),
                    value.current_directory,
                    value.last_entry,
                    int(value.completed),
                    timestamp,
                    value.mount_key,
                ),
            )

        await self._write(operation)

    async def ensure_legacy_download_attempt(
        self,
        *,
        attempt_id: str,
        task_id: str,
        candidate_index: int,
        job_name: str,
        mount_root: str,
        workspace_path: str,
        state: str,
        disposition: str,
        error_code: str | None,
        publisher_bundle_ids: list[str] | None = None,
        now: float | None = None,
    ) -> DownloadAttempt:
        timestamp = time.time() if now is None else now
        handle = TaskHandle(source="usenet", job_name=job_name)

        def operation(conn: sqlite3.Connection) -> DownloadAttempt:
            conn.execute(
                """INSERT OR IGNORE INTO download_attempts
                   (id,task_id,source,candidate_index,job_name,handle_json,mount_root,
                    workspace_path,publisher_bundle_ids_json,legacy_reconciled,state,
                    disposition,error_code,
                    next_retry_at,created_at,updated_at)
                   VALUES (?,?, 'usenet',?,?,?,?,?,?,1,?,?,?,?,?,?)""",
                (
                    attempt_id,
                    task_id,
                    candidate_index,
                    job_name,
                    _encode_json(to_jsonable(handle)),
                    mount_root,
                    workspace_path,
                    _encode_json(publisher_bundle_ids or []),
                    state,
                    disposition,
                    error_code,
                    timestamp,
                    timestamp,
                    timestamp,
                ),
            )
            row = conn.execute(
                "SELECT * FROM download_attempts WHERE id=?", (attempt_id,)
            ).fetchone()
            value = _row_to_attempt(row)
            if value is None:
                raise ValueError("legacy cleanup attempt disappeared")
            return value

        return await self._write(operation)

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
            conn.execute(
                "DELETE FROM download_quarantine WHERE id = ?", (quarantine_id,)
            )

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

    async def list_quarantine(
        self, page: int = 1, page_size: int = 50
    ) -> list[dict[str, Any]]:
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
        release_track_mbid: str | None = None,
        reason_detail: str | None = None,
        origin: str = "user",
        management_retry_count: int = 0,
        management_next_retry_at: float | None = None,
    ) -> int | None:
        """Hold a verify-rejected file for review. De-duped on (album, disc, track): if that
        track is already held, returns None so the caller can drop its extra copy instead of
        piling up one per edition it failed over through. Dedup needs a real track position -
        without one, two different unknown-track holds aren't the same track, so we keep both."""
        now = time.time()

        def operation(conn: sqlite3.Connection) -> int | None:
            if track_number is not None:
                if reason.startswith("management:"):
                    # Management holds are complete acquisition units. An older hold
                    # for the same album must not steal one track from this task and
                    # leave an apparently complete unit that cannot be retried.
                    dupe = conn.execute(
                        """SELECT id FROM held_imports
                           WHERE user_id = ? AND source_task_id IS ? AND disc_number IS ?
                             AND track_number = ? AND status = 'held' LIMIT 1""",
                        (user_id, source_task_id, disc_number, track_number),
                    ).fetchone()
                else:
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
                   (user_id, release_group_mbid, release_mbid, release_track_mbid,
                    recording_mbid, track_number,
                    disc_number, track_title, artist_name, artist_mbid, album_title, year,
                    held_path, original_filename, file_format, duration_seconds, reason,
                    reason_detail,
                    evidence_title, evidence_artist, evidence_score, source, source_task_id,
                    origin, naming_template, management_retry_count,
                    management_next_retry_at, status, created_at)
                   VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'held',?)""",
                (
                    user_id,
                    release_group_mbid,
                    release_mbid,
                    release_track_mbid,
                    recording_mbid,
                    track_number,
                    disc_number,
                    track_title,
                    artist_name,
                    artist_mbid,
                    album_title,
                    year,
                    held_path,
                    original_filename,
                    file_format,
                    duration_seconds,
                    reason,
                    reason_detail,
                    evidence_title,
                    evidence_artist,
                    evidence_score,
                    source,
                    source_task_id,
                    origin,
                    naming_template,
                    management_retry_count,
                    management_next_retry_at,
                    now,
                ),
            )
            return cur.lastrowid

        return await self._write(operation)

    async def replace_management_hold_bundle(
        self, held_files: list[HeldImport]
    ) -> tuple[list[int], list[str]]:
        """Replace one task's management hold in a single SQLite transaction.

        The caller copies every file before entering this transaction and compensates
        those filesystem copies if this write fails. Existing rows are retained in the
        audit trail as discarded; their obsolete paths are returned for best-effort
        cleanup after the new complete unit commits.
        """

        if not held_files:
            raise ValueError("A management hold bundle cannot be empty")
        ownership = {(value.user_id, value.source_task_id) for value in held_files}
        if len(ownership) != 1 or next(iter(ownership))[1] is None:
            raise ValueError("A management hold bundle must belong to one task")
        if any(not value.reason.startswith("management:") for value in held_files):
            raise ValueError("A management hold bundle requires management reasons")
        positions = {(value.disc_number, value.track_number) for value in held_files}
        if None in {position for pair in positions for position in pair}:
            raise ValueError("A management hold bundle requires exact track positions")
        if len(positions) != len(held_files):
            raise ValueError("A management hold bundle contains duplicate positions")

        user_id, source_task_id = next(iter(ownership))
        now = time.time()

        def operation(conn: sqlite3.Connection) -> tuple[list[int], list[str]]:
            existing = conn.execute(
                """SELECT id, held_path FROM held_imports
                   WHERE user_id = ? AND source_task_id = ? AND status = 'held'
                     AND reason LIKE 'management:%'""",
                (user_id, source_task_id),
            ).fetchall()
            if existing:
                existing_ids = [row["id"] for row in existing]
                placeholders = ",".join("?" for _value in existing_ids)
                conn.execute(
                    f"UPDATE held_imports SET status = 'discarded', resolved_at = ? "
                    f"WHERE id IN ({placeholders})",
                    (now, *existing_ids),
                )

            inserted: list[int] = []
            for value in held_files:
                cur = conn.execute(
                    """INSERT INTO held_imports
                       (user_id, release_group_mbid, release_mbid, release_track_mbid,
                        recording_mbid,
                        track_number, disc_number, track_title, artist_name, artist_mbid,
                        album_title, year, held_path, original_filename, file_format,
                        duration_seconds, reason, reason_detail, evidence_title,
                        evidence_artist, evidence_score, source, source_task_id, origin,
                        naming_template, management_retry_count,
                        management_next_retry_at, status, created_at)
                       VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'held',?)""",
                    (
                        value.user_id,
                        value.release_group_mbid,
                        value.release_mbid,
                        value.release_track_mbid,
                        value.recording_mbid,
                        value.track_number,
                        value.disc_number,
                        value.track_title,
                        value.artist_name,
                        value.artist_mbid,
                        value.album_title,
                        value.year,
                        value.held_path,
                        value.original_filename,
                        value.file_format,
                        value.duration_seconds,
                        value.reason,
                        value.reason_detail,
                        value.evidence_title,
                        value.evidence_artist,
                        value.evidence_score,
                        value.source,
                        value.source_task_id,
                        value.origin,
                        value.naming_template,
                        value.management_retry_count,
                        value.management_next_retry_at,
                        now,
                    ),
                )
                inserted.append(cur.lastrowid)
            return inserted, [str(row["held_path"]) for row in existing]

        return await self._write(operation)

    async def list_held_imports(
        self,
        user_id: str,
        user_role: str,
        release_group_mbid: str | None = None,
        source_task_id: str | None = None,
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
            if source_task_id:
                sql += " AND source_task_id = ?"
                params.append(source_task_id)
            sql += " ORDER BY created_at DESC"
            return [_row_to_held(dict(r)) for r in conn.execute(sql, params).fetchall()]

        return await self._read(operation)

    async def get_held_import(
        self, held_id: int, user_id: str, user_role: str
    ) -> HeldImport | None:
        def operation(conn: sqlite3.Connection) -> HeldImport | None:
            row = conn.execute(
                "SELECT * FROM held_imports WHERE id = ?", (held_id,)
            ).fetchone()
            if row is None:
                return None
            held = _row_to_held(dict(row))
            if user_role != "admin" and held.user_id != user_id:
                return None
            return held

        return await self._read(operation)

    async def list_pending_discard_file_cleanups(
        self, *, limit: int = 100
    ) -> list[HeldImport]:
        def operation(conn: sqlite3.Connection) -> list[HeldImport]:
            rows = conn.execute(
                "SELECT * FROM held_imports WHERE status = 'discarded' "
                "AND file_cleanup_completed_at IS NULL "
                "ORDER BY resolved_at, id LIMIT ?",
                (limit,),
            ).fetchall()
            return [_row_to_held(dict(row)) for row in rows]

        return await self._read(operation)

    async def complete_held_file_cleanup(self, held_ids: list[int]) -> None:
        if not held_ids:
            return
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            placeholders = ",".join("?" for _value in held_ids)
            conn.execute(
                f"UPDATE held_imports SET file_cleanup_completed_at = ? "
                f"WHERE id IN ({placeholders}) AND status = 'discarded'",
                (now, *held_ids),
            )

        await self._write(operation)

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

    async def resolve_held_imports(self, held_ids: list[int], status: str) -> None:
        """Resolve one acquisition unit in a single SQLite transaction."""

        if not held_ids:
            return
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            placeholders = ",".join("?" for _value in held_ids)
            conn.execute(
                f"UPDATE held_imports SET status = ?, resolved_at = ? "
                f"WHERE id IN ({placeholders}) AND status = 'held'",
                (status, now, *held_ids),
            )

        await self._write(operation)

    async def update_held_import_reason(
        self, held_ids: list[int], *, reason: str, reason_detail: str | None
    ) -> None:
        """Refresh the actionable reason after an album-level management retry."""

        if not held_ids:
            return

        def operation(conn: sqlite3.Connection) -> None:
            placeholders = ",".join("?" for _value in held_ids)
            conn.execute(
                f"UPDATE held_imports SET reason = ?, reason_detail = ? "
                f"WHERE id IN ({placeholders}) AND status = 'held'",
                (reason, reason_detail, *held_ids),
            )

        await self._write(operation)

    async def schedule_management_hold_retry(
        self,
        held_ids: list[int],
        *,
        retry_count: int,
        next_retry_at: float | None,
    ) -> None:
        if not held_ids:
            return

        def operation(conn: sqlite3.Connection) -> None:
            placeholders = ",".join("?" for _value in held_ids)
            conn.execute(
                f"UPDATE held_imports SET management_retry_count = ?, "
                f"management_next_retry_at = ? WHERE id IN ({placeholders}) "
                "AND status = 'held' AND reason LIKE 'management:%'",
                (retry_count, next_retry_at, *held_ids),
            )

        await self._write(operation)

    async def list_due_management_hold_units(
        self, now: float, *, limit: int = 2
    ) -> list[tuple[str, str]]:
        def operation(conn: sqlite3.Connection) -> list[tuple[str, str]]:
            rows = conn.execute(
                """SELECT source_task_id, user_id
                   FROM held_imports
                   WHERE status = 'held' AND source_task_id IS NOT NULL
                     AND reason LIKE 'management:%'
                     AND management_next_retry_at IS NOT NULL
                     AND management_next_retry_at <= ?
                   GROUP BY source_task_id, user_id
                   ORDER BY MIN(management_next_retry_at), MIN(id)
                   LIMIT ?""",
                (now, limit),
            ).fetchall()
            return [(str(row["source_task_id"]), str(row["user_id"])) for row in rows]

        return await self._read(operation)

    async def repair_management_hold_identity(
        self,
        source_task_id: str,
        release_mbid: str,
        mappings: list[tuple[int, int, int, str, str]],
        *,
        task_release_track_mbid: str | None = None,
    ) -> None:
        """Persist one provider-proven legacy hold repair atomically.

        Each mapping is ``(held_id, disc, position, recording_mbid,
        release_track_mbid)``. The transaction rechecks the complete held unit and
        its task before changing either table, so a stale, partial, duplicate, or
        conflicting projection performs zero updates.
        """

        if not mappings:
            raise ValueError("A held identity repair cannot be empty")
        held_ids = [value[0] for value in mappings]
        positions = {(value[1], value[2]) for value in mappings}
        release_track_mbids = {value[4].casefold() for value in mappings if value[4]}
        if (
            len(set(held_ids)) != len(mappings)
            or len(positions) != len(mappings)
            or len(release_track_mbids) != len(mappings)
            or any(not value[3] or not value[4] for value in mappings)
            or any(value[1] < 1 or value[2] < 1 for value in mappings)
        ):
            raise ValueError(
                "A held identity repair must contain unique complete mappings"
            )

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute("BEGIN IMMEDIATE")
            task = conn.execute(
                """SELECT release_mbid, release_track_mbid, download_type
                   FROM download_tasks WHERE id = ?""",
                (source_task_id,),
            ).fetchone()
            if task is None:
                raise ValueError("The acquisition task changed before identity repair")
            if (task["download_type"] == "track") != bool(task_release_track_mbid):
                raise ValueError(
                    "The acquisition task has an incomplete identity scope"
                )
            if (
                task["release_mbid"]
                and str(task["release_mbid"]).casefold() != release_mbid.casefold()
            ):
                raise ValueError("The acquisition task has a conflicting exact edition")
            if (
                task["release_track_mbid"]
                and task_release_track_mbid
                and str(task["release_track_mbid"]).casefold()
                != task_release_track_mbid.casefold()
            ):
                raise ValueError("The acquisition task has a conflicting release track")

            rows = conn.execute(
                """SELECT id, release_mbid, release_track_mbid, recording_mbid,
                          disc_number, track_number
                   FROM held_imports
                   WHERE source_task_id = ? AND status = 'held'
                     AND reason LIKE 'management:%'""",
                (source_task_id,),
            ).fetchall()
            if len(rows) != len(mappings) or {int(row["id"]) for row in rows} != set(
                held_ids
            ):
                raise ValueError(
                    "The held acquisition unit changed before identity repair"
                )

            expected = {value[0]: value for value in mappings}
            for row in rows:
                _held_id, disc, position, recording_mbid, release_track_mbid = expected[
                    int(row["id"])
                ]
                if (
                    row["disc_number"] != disc
                    or row["track_number"] != position
                    or not row["recording_mbid"]
                    or str(row["recording_mbid"]).casefold()
                    != recording_mbid.casefold()
                    or (
                        row["release_mbid"]
                        and str(row["release_mbid"]).casefold()
                        != release_mbid.casefold()
                    )
                    or (
                        row["release_track_mbid"]
                        and str(row["release_track_mbid"]).casefold()
                        != release_track_mbid.casefold()
                    )
                ):
                    raise ValueError(
                        "The held acquisition identity is stale or conflicting"
                    )

            conn.execute(
                """UPDATE download_tasks
                   SET release_mbid = ?, release_track_mbid = ?, updated_at = ?
                   WHERE id = ?""",
                (release_mbid, task_release_track_mbid, time.time(), source_task_id),
            )
            for (
                held_id,
                _disc,
                _position,
                _recording_mbid,
                release_track_mbid,
            ) in mappings:
                conn.execute(
                    """UPDATE held_imports
                       SET release_mbid = ?, release_track_mbid = ?
                       WHERE id = ? AND status = 'held'""",
                    (release_mbid, release_track_mbid, held_id),
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

    async def task_ids_with_unresolved_held(
        self, user_id: str, user_role: str
    ) -> set[str]:
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
        completed = (
            now if status in ("matched", "completed", "failed", "cancelled") else None
        )

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
            cursor = conn.execute(
                "DELETE FROM search_jobs WHERE created_at < ?", (cutoff,)
            )
            return cursor.rowcount

        return await self._write(operation)

    async def count_user_track_requests_since(
        self, user_id: str, since_epoch: float
    ) -> int:
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

    async def list_retryable_tasks(self, max_retry_count: int) -> list[DownloadTask]:
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
            cur = conn.execute(
                f"DELETE FROM download_tasks WHERE {where}", tuple(params)
            )
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
                "DELETE FROM held_imports WHERE release_group_mbid = ?",
                (release_group_mbid,),
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


def _row_to_attempt(row: sqlite3.Row | None) -> DownloadAttempt | None:
    if row is None:
        return None
    value = dict(row)
    try:
        handle = msgspec.convert(
            _decode_json(str(value.pop("handle_json"))), type=TaskHandle, strict=False
        )
        materialized_paths = [
            str(item)
            for item in _decode_json(str(value.pop("materialized_paths_json")))
        ]
        raw_fingerprints = _decode_json(
            str(value.pop("materialized_fingerprints_json"))
        )
        if not isinstance(raw_fingerprints, dict):
            return None
        materialized_fingerprints = {
            str(path): str(fingerprint) if fingerprint is not None else None
            for path, fingerprint in raw_fingerprints.items()
        }
        publisher_bundle_ids = [
            str(item)
            for item in _decode_json(str(value.pop("publisher_bundle_ids_json")))
        ]
    except (TypeError, ValueError, msgspec.ValidationError):
        return None
    value["legacy_reconciled"] = bool(value["legacy_reconciled"])
    return DownloadAttempt(
        **value,
        handle=handle,
        materialized_paths=materialized_paths,
        materialized_fingerprints=materialized_fingerprints,
        publisher_bundle_ids=publisher_bundle_ids,
    )


def _row_to_reconciliation(
    row: sqlite3.Row | None,
) -> DownloadCleanupReconciliation | None:
    if row is None:
        return None
    value = dict(row)
    try:
        pending = [
            str(item)
            for item in _decode_json(str(value.pop("pending_directories_json")))
        ]
    except (TypeError, ValueError):
        return None
    value["completed"] = bool(value["completed"])
    return DownloadCleanupReconciliation(**value, pending_directories=pending)


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
    row.pop("file_cleanup_completed_at", None)
    return HeldImport(**row)


def _row_to_search_job(row: sqlite3.Row | None) -> SearchJob | None:
    if row is None:
        return None
    return msgspec.convert(dict(row), type=SearchJob, strict=False)
