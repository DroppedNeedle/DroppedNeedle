"""UserListeningPrefsStore tests."""

import sqlite3
import threading
from pathlib import Path

import pytest

from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users "
            "(id TEXT PRIMARY KEY, username TEXT, display_name TEXT, role TEXT)"
        )
        conn.executemany(
            "INSERT OR IGNORE INTO auth_users (id, username, display_name, role) "
            "VALUES (?, ?, ?, ?)",
            [
                ("user-a", "alice", "Alice", "user"),
                ("user-b", "bob", "Bob", "user"),
                ("admin-x", "admin", "Admin X", "admin"),
            ],
        )
        conn.commit()
    finally:
        conn.close()


@pytest.fixture
def store(tmp_path: Path) -> UserListeningPrefsStore:
    db_path = tmp_path / "library.db"
    s = UserListeningPrefsStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    return s


def test_migration_is_idempotent(tmp_path: Path):
    db_path = tmp_path / "library.db"
    lock = threading.Lock()
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            """
            CREATE TABLE user_listening_prefs (
              user_id TEXT PRIMARY KEY,
              scrobble_to_lastfm INTEGER NOT NULL DEFAULT 0,
              scrobble_to_listenbrainz INTEGER NOT NULL DEFAULT 0,
              primary_music_source TEXT NOT NULL DEFAULT 'listenbrainz',
              now_playing_visibility TEXT NOT NULL DEFAULT 'full',
              auto_request_personal_mix INTEGER NOT NULL DEFAULT 0,
              updated_at TEXT NOT NULL
            )
            """
        )
        conn.execute(
            "INSERT INTO user_listening_prefs (user_id, updated_at) VALUES ('legacy', '')"
        )
        conn.commit()
    finally:
        conn.close()

    UserListeningPrefsStore(db_path=db_path, write_lock=lock)
    UserListeningPrefsStore(db_path=db_path, write_lock=lock)
    conn = sqlite3.connect(db_path)
    try:
        columns = {row[1] for row in conn.execute("PRAGMA table_info(user_listening_prefs)")}
        value = conn.execute(
            "SELECT navidrome_handles_external_scrobbles "
            "FROM user_listening_prefs WHERE user_id = 'legacy'"
        ).fetchone()[0]
    finally:
        conn.close()
    assert "navidrome_handles_external_scrobbles" in columns
    assert value == 0


@pytest.mark.asyncio
async def test_missing_row_returns_defaults(store: UserListeningPrefsStore):
    prefs = await store.get("user-a")
    assert prefs.user_id == "user-a"
    assert prefs.scrobble_to_lastfm is False
    assert prefs.scrobble_to_listenbrainz is False
    assert prefs.navidrome_handles_external_scrobbles is True
    assert prefs.primary_music_source == "listenbrainz"


@pytest.mark.asyncio
async def test_new_row_uses_navidrome_forwarding_default(
    store: UserListeningPrefsStore,
):
    await store.upsert("user-a", scrobble_to_lastfm=True)
    prefs = await store.get("user-a")
    assert prefs.navidrome_handles_external_scrobbles is True


@pytest.mark.asyncio
async def test_full_upsert_roundtrip(store: UserListeningPrefsStore):
    await store.upsert(
        "user-a",
        scrobble_to_lastfm=True,
        scrobble_to_listenbrainz=True,
        navidrome_handles_external_scrobbles=False,
        primary_music_source="lastfm",
    )
    prefs = await store.get("user-a")
    assert prefs.scrobble_to_lastfm is True
    assert prefs.scrobble_to_listenbrainz is True
    assert prefs.navidrome_handles_external_scrobbles is False
    assert prefs.primary_music_source == "lastfm"


@pytest.mark.asyncio
async def test_partial_upsert_preserves_other_columns(store: UserListeningPrefsStore):
    await store.upsert(
        "user-a",
        scrobble_to_lastfm=True,
        navidrome_handles_external_scrobbles=False,
    )
    await store.upsert("user-a", primary_music_source="lastfm")
    prefs = await store.get("user-a")
    assert prefs.scrobble_to_lastfm is True
    assert prefs.scrobble_to_listenbrainz is False
    assert prefs.navidrome_handles_external_scrobbles is False
    assert prefs.primary_music_source == "lastfm"


@pytest.mark.asyncio
async def test_upsert_is_user_scoped(store: UserListeningPrefsStore):
    await store.upsert("user-a", scrobble_to_lastfm=True)
    assert (await store.get("user-a")).scrobble_to_lastfm is True
    assert (await store.get("user-b")).scrobble_to_lastfm is False


@pytest.mark.asyncio
async def test_cascade_on_user_delete(store: UserListeningPrefsStore, tmp_path: Path):
    await store.upsert("user-a", scrobble_to_lastfm=True)
    conn = sqlite3.connect(tmp_path / "library.db")
    try:
        conn.execute("PRAGMA foreign_keys=ON")
        conn.execute("DELETE FROM auth_users WHERE id = ?", ("user-a",))
        conn.commit()
        remaining = conn.execute(
            "SELECT COUNT(*) FROM user_listening_prefs WHERE user_id = ?", ("user-a",)
        ).fetchone()[0]
    finally:
        conn.close()
    assert remaining == 0


@pytest.mark.asyncio
async def test_auto_request_roundtrip_and_partial_preserve(store: UserListeningPrefsStore):
    await store.upsert("user-a", auto_request_personal_mix=True)
    assert (await store.get("user-a")).auto_request_personal_mix is True
    # a later partial update of another field keeps it via COALESCE
    await store.upsert("user-a", primary_music_source="lastfm")
    prefs = await store.get("user-a")
    assert prefs.auto_request_personal_mix is True
    assert prefs.primary_music_source == "lastfm"
    await store.upsert("user-a", auto_request_personal_mix=False)
    assert (await store.get("user-a")).auto_request_personal_mix is False


@pytest.mark.asyncio
async def test_approval_upsert_and_state(store: UserListeningPrefsStore):
    assert await store.get_approval_state("user-a") is None
    await store.upsert_approval("user-a", "pending")
    assert await store.get_approval_state("user-a") == "pending"

    ok = await store.set_approval_state("user-a", "approved", ("admin-x", "Admin X"))
    assert ok is True
    assert await store.get_approval_state("user-a") == "approved"

    # review transition on a missing row reports failure
    assert await store.set_approval_state("user-z", "approved", ("admin-x", "Admin X")) is False


@pytest.mark.asyncio
async def test_reenable_requeues_as_fresh_pending(store: UserListeningPrefsStore):
    # matches follows: re-enabling re-queues even over a prior grant
    await store.upsert_approval("user-a", "pending")
    await store.set_approval_state("user-a", "approved", ("admin-x", "Admin X"))
    await store.upsert_approval("user-a", "pending")
    assert await store.get_approval_state("user-a") == "pending"


@pytest.mark.asyncio
async def test_list_pending_approvals_joins_user_name(store: UserListeningPrefsStore):
    await store.upsert_approval("user-a", "pending")
    await store.upsert_approval("user-b", "pending")
    await store.set_approval_state("user-b", "rejected", ("admin-x", "Admin X"))
    pending = await store.list_pending_approvals()
    assert [p.user_id for p in pending] == ["user-a"]
    assert pending[0].user_name == "Alice"


@pytest.mark.asyncio
async def test_backfill_queues_pre_gate_optins_once(tmp_path: Path):
    db_path = tmp_path / "library.db"
    lock = threading.Lock()
    store = UserListeningPrefsStore(db_path=db_path, write_lock=lock)
    _seed_auth_users(db_path)
    # user-a (role user) and admin-x opted in before the approval gate existed
    await store.upsert("user-a", auto_request_personal_mix=True)
    await store.upsert("admin-x", auto_request_personal_mix=True)

    UserListeningPrefsStore(db_path=db_path, write_lock=lock)  # re-run _ensure_tables
    assert await store.get_approval_state("user-a") == "pending"
    assert await store.get_approval_state("admin-x") is None  # admins granted by role

    # idempotent: a rejected row is not resurrected by later restarts
    await store.set_approval_state("user-a", "rejected", ("admin-x", "Admin X"))
    UserListeningPrefsStore(db_path=db_path, write_lock=lock)
    assert await store.get_approval_state("user-a") == "rejected"
