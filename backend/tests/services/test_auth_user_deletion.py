import threading
import sqlite3

import pytest

from infrastructure.persistence.auth_store import AuthStore
from infrastructure.persistence.discovery_snapshot_store import DiscoverySnapshotStore
from services.auth_service import AuthService


@pytest.mark.asyncio
async def test_delete_user_removes_discovery_snapshots(tmp_path) -> None:
    db_path = tmp_path / "library.db"
    lock = threading.Lock()
    auth_store = AuthStore(db_path, lock)
    snapshot_store = DiscoverySnapshotStore(db_path, lock)
    with sqlite3.connect(db_path) as conn:
        conn.execute(
            "CREATE TABLE playlists (id TEXT PRIMARY KEY, user_id TEXT NOT NULL)"
        )
    admin = await auth_store.create_user(
        id="admin",
        display_name="Admin",
        role="admin",
        username="admin",
        username_display="Admin",
    )
    user = await auth_store.create_user(
        id="user",
        display_name="User",
        role="user",
        username="user",
        username_display="User",
    )
    await snapshot_store.save("discover_response:user", user.id, b"{}", 123.0)
    await snapshot_store.save("discover_queue:user", user.id, b"{}", 123.0)

    service = AuthService(auth_store, discovery_snapshot_store=snapshot_store)
    await service.delete_user(user.id, requesting_user_id=admin.id)

    assert await auth_store.get_user_by_id(user.id) is None
    assert await snapshot_store.get("discover_response:user") is None
    assert await snapshot_store.get("discover_queue:user") is None
