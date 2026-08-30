import threading
import sqlite3

import pytest

from infrastructure.persistence._database import _safe_alter
from infrastructure.persistence.discovery_snapshot_store import DiscoverySnapshotStore


def test_schema_initialization_is_idempotent(tmp_path) -> None:
    db_path = tmp_path / "library.db"
    lock = threading.Lock()

    DiscoverySnapshotStore(db_path, lock)
    DiscoverySnapshotStore(db_path, lock)


def test_safe_alter_reraises_unrelated_schema_errors(tmp_path) -> None:
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    conn = sqlite3.connect(store.db_path)
    try:
        with pytest.raises(sqlite3.OperationalError):
            _safe_alter(conn, "ALTER TABLE missing_table ADD COLUMN value TEXT")
    finally:
        conn.close()


@pytest.mark.asyncio
async def test_snapshot_round_trip_and_user_deletion(tmp_path) -> None:
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())

    await store.save("discover:u1", "u1", b'{"ok":true}', 123.0)
    await store.save("discover:u2", "u2", b'{"ok":false}', 124.0)

    assert await store.get("discover:u1") == b'{"ok":true}'
    await store.delete_user("u1")
    assert await store.get("discover:u1") is None
    assert await store.get("discover:u2") == b'{"ok":false}'


@pytest.mark.asyncio
async def test_single_snapshot_deletion(tmp_path) -> None:
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    await store.save("queue:u1", "u1", b"queue", 123.0)

    await store.delete("queue:u1")

    assert await store.get("queue:u1") is None


@pytest.mark.asyncio
async def test_library_invalidation_marks_page_and_queue_snapshots_stale(
    tmp_path,
) -> None:
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    await store.save("discover_response:u1", "u1", b"home", 123.0)
    await store.save("discover_queue:u1", "u1", b"queue", 123.0)

    await store.mark_discover_stale()

    assert await store.get_with_stale("discover_response:u1") == (b"home", True)
    assert await store.get_with_stale("discover_queue:u1") == (b"queue", True)


@pytest.mark.asyncio
async def test_source_dependent_snapshots_delete_only_discover_rows(tmp_path) -> None:
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    await store.save("discover_response:u1", "u1", b"response", 123.0)
    await store.save("discover_queue:u1", "u1", b"queue", 124.0)
    await store.save("discover:u1", "u1", b"unrelated", 125.0)
    await store.save("queue:u1", "u1", b"also-unrelated", 126.0)

    assert await store.delete_source_dependent_snapshots() == 2
    assert await store.get("discover_response:u1") is None
    assert await store.get("discover_queue:u1") is None
    assert await store.get("discover:u1") == b"unrelated"
    assert await store.get("queue:u1") == b"also-unrelated"

    connection = sqlite3.connect(store.db_path)
    try:
        source_rows = connection.execute(
            "SELECT COUNT(*) FROM discovery_snapshots "
            "WHERE snapshot_key LIKE 'discover_response:%' "
            "OR snapshot_key LIKE 'discover_queue:%'"
        ).fetchone()[0]
        catalog_revision = connection.execute(
            "SELECT value FROM library_catalog_revision WHERE singleton = 1"
        ).fetchone()[0]
    finally:
        connection.close()

    assert source_rows == 0
    assert catalog_revision == 0


@pytest.mark.asyncio
async def test_snapshot_is_rejected_after_catalog_revision_changes(tmp_path) -> None:
    store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    await store.save("discover_response:u1", "u1", b"home", 123.0)

    connection = sqlite3.connect(store.db_path)
    try:
        connection.execute(
            "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1"
        )
        connection.commit()
    finally:
        connection.close()

    assert await store.get_with_stale("discover_response:u1") is None
