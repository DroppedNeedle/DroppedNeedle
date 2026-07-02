"""CollectionManagement Phase 0: the ``origin`` column on ``download_tasks``.

Invisible-phase guarantees: new tasks default to ``origin='user'``, an explicit
origin round-trips, and a pre-origin database migrates in place with existing
rows back-filled to ``'user'``.
"""

import sqlite3
import threading
import time
from pathlib import Path

import pytest

from infrastructure.persistence.download_store import DownloadStore


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)"
        )
        conn.execute(
            "INSERT OR IGNORE INTO auth_users (id, username, role) VALUES (?, ?, ?)",
            ("user-a", "alice", "user"),
        )
        conn.commit()
    finally:
        conn.close()


@pytest.fixture
def store(tmp_path: Path) -> DownloadStore:
    db_path = tmp_path / "library.db"
    s = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    return s


@pytest.mark.asyncio
async def test_create_task_defaults_origin_user(store: DownloadStore):
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="A", album_title="B"
    )
    assert task.origin == "user"
    assert (await store.get_task(task.id)).origin == "user"


@pytest.mark.asyncio
@pytest.mark.parametrize("origin", ["retry", "upgrade"])
async def test_create_task_explicit_origin_round_trips(store: DownloadStore, origin: str):
    task = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-1",
        artist_name="A",
        album_title="B",
        origin=origin,
    )
    assert (await store.get_task(task.id)).origin == origin


@pytest.mark.asyncio
async def test_failed_upgrade_task_never_auto_retries(store: DownloadStore):
    await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="A", album_title="B",
        origin="upgrade", status="failed",
    )
    assert await store.list_retryable_tasks(6) == []


@pytest.mark.asyncio
async def test_newer_upgrade_task_does_not_suppress_user_retry(store: DownloadStore):
    """The NOT EXISTS newest-per-target subquery must ignore upgrade tasks (D18):
    a curator upgrading an album must not swallow the retry of a user's earlier
    failed download of the same album."""
    failed_user = await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="A", album_title="B",
        origin="user", status="failed",
    )
    await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="A", album_title="B",
        origin="upgrade", status="queued",
    )

    retryable = await store.list_retryable_tasks(6)

    assert [t.id for t in retryable] == [failed_user.id]


@pytest.mark.asyncio
async def test_newer_user_task_still_suppresses_older_retry(store: DownloadStore):
    # sanity: the exclusion must not break the normal newest-per-target rule
    await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="A", album_title="B",
        origin="user", status="failed",
    )
    await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="A", album_title="B",
        origin="retry", status="queued",
    )

    assert await store.list_retryable_tasks(6) == []


@pytest.mark.asyncio
async def test_pre_origin_db_migrates_and_backfills_user(tmp_path: Path):
    """A database created before the origin column existed gains it via the
    idempotent ALTER, and its existing rows read back as origin='user'."""
    db_path = tmp_path / "library.db"
    _seed_auth_users(db_path)
    conn = sqlite3.connect(db_path)
    try:
        # Minimal pre-origin shape: the columns the INSERT, defaults and the
        # CREATE INDEX statements in _ensure_tables need - everything but origin.
        conn.execute(
            """
            CREATE TABLE download_tasks (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                download_type TEXT NOT NULL DEFAULT 'album',
                release_group_mbid TEXT NOT NULL,
                artist_name TEXT NOT NULL,
                album_title TEXT NOT NULL,
                source_username TEXT,
                status TEXT NOT NULL DEFAULT 'queued',
                created_at REAL NOT NULL,
                updated_at REAL NOT NULL
            )
            """
        )
        now = time.time()
        conn.execute(
            "INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name,"
            " album_title, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            ("legacy-1", "user-a", "rg-legacy", "A", "B", now, now),
        )
        conn.commit()
    finally:
        conn.close()

    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    legacy = await store.get_task("legacy-1")
    assert legacy is not None
    assert legacy.origin == "user"
