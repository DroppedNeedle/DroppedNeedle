"""FollowStore tests for the LidarrImport bulk-follow + bulk-approval methods."""

import sqlite3
import threading
from pathlib import Path

import pytest

from infrastructure.persistence.follow_store import FollowStore

A = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
B = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
C = "cccccccc-cccc-cccc-cccc-cccccccccccc"


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users "
            "(id TEXT PRIMARY KEY, display_name TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'user')"
        )
        conn.executemany(
            "INSERT OR IGNORE INTO auth_users (id, display_name, role) VALUES (?, ?, ?)",
            [("admin-1", "Admin", "admin"), ("user-a", "Alice", "user"), ("user-b", "Bob", "user")],
        )
        conn.commit()
    finally:
        conn.close()


@pytest.fixture
def store(tmp_path: Path) -> FollowStore:
    db_path = tmp_path / "library.db"
    s = FollowStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    return s


# --- bulk follow --------------------------------------------------------------


@pytest.mark.asyncio
async def test_follow_artists_bulk_inserts_all(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "Artist A"), (B, "Artist B")])
    followed = await store.list_followed_artists("user-a")
    assert {f.artist_mbid for f in followed} == {A, B}
    assert all(f.auto_download is False for f in followed)


@pytest.mark.asyncio
async def test_follow_artists_bulk_idempotent_preserves_intent_and_time(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "Artist A")])
    await store.set_auto_download_intent("user-a", A, True)
    first = (await store.list_followed_artists("user-a"))[0]

    # Re-import with a refreshed name: intent + followed_at preserved (D6/DR4).
    await store.follow_artists_bulk("user-a", [(A, "Artist A (renamed)")])
    again = (await store.list_followed_artists("user-a"))[0]
    assert again.auto_download is True
    assert again.followed_at == first.followed_at
    assert again.artist_name == "Artist A (renamed)"


@pytest.mark.asyncio
async def test_set_auto_download_intent_bulk_targets_only_given(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "A"), (B, "B"), (C, "C")])
    await store.set_auto_download_intent_bulk("user-a", [A, C], True)
    by_mbid = {f.artist_mbid: f for f in await store.list_followed_artists("user-a")}
    assert by_mbid[A].auto_download is True
    assert by_mbid[C].auto_download is True
    assert by_mbid[B].auto_download is False


@pytest.mark.asyncio
async def test_existing_followed_lower_returns_subset(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "A"), (B, "B")])
    existing = await store.existing_followed_lower("user-a", [A.lower(), B.lower(), C.lower()])
    assert existing == {A.lower(), B.lower()}
    assert await store.existing_followed_lower("user-a", []) == set()


# --- bulk approval batch ------------------------------------------------------


@pytest.mark.asyncio
async def test_create_and_list_pending_batch(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "A"), (B, "B")])
    await store.create_import_approval_batch("user-a", [(A, "A"), (B, "B")], "batch-1")
    batches = await store.list_pending_approval_batches()
    assert len(batches) == 1
    batch = batches[0]
    assert batch.batch_id == "batch-1"
    assert batch.user_id == "user-a"
    assert batch.user_name == "Alice"
    assert batch.artist_count == 2
    assert batch.source == "lidarr_import"
    assert set(batch.sample_names) == {"A", "B"}


@pytest.mark.asyncio
async def test_batched_rows_excluded_from_individual_pending(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "A"), (B, "B")])
    # One individual pending approval + one batched.
    await store.upsert_approval("user-a", A, "A", "pending")
    await store.create_import_approval_batch("user-a", [(B, "B")], "batch-1")
    individual = await store.list_pending_approvals()
    assert [a.artist_mbid for a in individual] == [A]  # B is batched, not shown here


@pytest.mark.asyncio
async def test_approve_batch_unlocks_gate(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "A")])
    await store.set_auto_download_intent_bulk("user-a", [A], True)
    await store.create_import_approval_batch("user-a", [(A, "A")], "batch-1")
    # Before approval: intent on but not approved -> not an eligible follower.
    assert await store.list_auto_download_followers(A.lower()) == []

    affected = await store.set_batch_approval_state("batch-1", "approved", ("admin-1", "Admin"))
    assert affected == 1
    assert await store.list_auto_download_followers(A.lower()) == ["user-a"]
    # Batch no longer pending.
    assert await store.list_pending_approval_batches() == []


@pytest.mark.asyncio
async def test_reject_batch_flips_intent_off(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "A"), (B, "B")])
    await store.set_auto_download_intent_bulk("user-a", [A, B], True)
    await store.create_import_approval_batch("user-a", [(A, "A"), (B, "B")], "batch-1")

    affected = await store.set_batch_approval_state("batch-1", "rejected", ("admin-1", "Admin"))
    assert affected == 2
    # Still followed, but intent flipped off, and not eligible.
    by_mbid = {f.artist_mbid: f for f in await store.list_followed_artists("user-a")}
    assert by_mbid[A].auto_download is False
    assert by_mbid[B].auto_download is False
    assert await store.list_auto_download_followers(A.lower()) == []


@pytest.mark.asyncio
async def test_batch_upsert_never_downgrades_approved(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "A")])
    await store.set_auto_download_intent_bulk("user-a", [A], True)
    # A pre-existing APPROVED grant (e.g. from a prior manual request).
    await store.upsert_approval("user-a", A, "A", "approved")
    # A new batch that (defensively) includes the already-approved artist.
    await store.create_import_approval_batch("user-a", [(A, "A")], "batch-1")

    approval = await store.get_approval("user-a", A)
    assert approval.state == "approved"  # never downgraded to pending
    # Still eligible; and it does not appear as a pending batch row.
    assert await store.list_auto_download_followers(A.lower()) == ["user-a"]
    assert await store.list_pending_approval_batches() == []


@pytest.mark.asyncio
async def test_batches_grouped_per_user(store: FollowStore):
    await store.follow_artists_bulk("user-a", [(A, "A")])
    await store.follow_artists_bulk("user-b", [(B, "B")])
    await store.create_import_approval_batch("user-a", [(A, "A")], "batch-a")
    await store.create_import_approval_batch("user-b", [(B, "B")], "batch-b")
    batches = await store.list_pending_approval_batches()
    assert {b.batch_id for b in batches} == {"batch-a", "batch-b"}
    assert {b.user_id for b in batches} == {"user-a", "user-b"}


@pytest.mark.asyncio
async def test_migrates_preexisting_db_without_batch_columns(tmp_path: Path):
    """An existing DB whose auto_download_approvals predates the batch_id/source columns
    must migrate cleanly: _ensure_tables must not crash (the batch index can only be
    created AFTER _safe_alter adds the column) and batching must then work."""
    db_path = tmp_path / "library.db"
    conn = sqlite3.connect(db_path)
    conn.execute(
        "CREATE TABLE auth_users (id TEXT PRIMARY KEY, display_name TEXT NOT NULL, "
        "role TEXT NOT NULL DEFAULT 'user')"
    )
    conn.execute("INSERT INTO auth_users VALUES ('user-a', 'Alice', 'user')")
    # The pre-migration auto_download_approvals shape (no batch_id / source).
    conn.execute(
        """
        CREATE TABLE auto_download_approvals (
            user_id TEXT NOT NULL, artist_mbid TEXT NOT NULL, artist_mbid_lower TEXT NOT NULL,
            artist_name TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending',
            requested_at REAL NOT NULL, reviewed_by_id TEXT, reviewed_by_name TEXT,
            reviewed_at REAL, PRIMARY KEY (user_id, artist_mbid_lower)
        )
        """
    )
    conn.commit()
    conn.close()

    store = FollowStore(db_path=db_path, write_lock=threading.Lock())
    cols = set()
    check = sqlite3.connect(db_path)
    try:
        cols = {r[1] for r in check.execute("PRAGMA table_info(auto_download_approvals)")}
    finally:
        check.close()
    assert {"batch_id", "source"} <= cols

    await store.follow_artists_bulk("user-a", [(A, "A")])
    await store.create_import_approval_batch("user-a", [(A, "A")], "batch-1")
    batches = await store.list_pending_approval_batches()
    assert len(batches) == 1 and batches[0].artist_count == 1


def test_construct_twice_is_idempotent(tmp_path: Path):
    """The _safe_alter ADD COLUMNs must be no-ops the second time (store idempotency rule)."""
    db_path = tmp_path / "library.db"
    FollowStore(db_path=db_path, write_lock=threading.Lock())
    # Second construction on the same path must not raise.
    FollowStore(db_path=db_path, write_lock=threading.Lock())
    conn = sqlite3.connect(db_path)
    try:
        cols = {r[1] for r in conn.execute("PRAGMA table_info(auto_download_approvals)")}
    finally:
        conn.close()
    assert {"batch_id", "source"} <= cols
