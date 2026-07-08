"""Admin bulk "Lidarr Import" approval-batch routes (LidarrImport D3)."""

import asyncio
import sqlite3
import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI, HTTPException

from api.v1.routes.requests_page import router as requests_router
from core.dependencies import (
    get_follow_service,
    get_personal_mix_service,
    get_requests_page_service,
)
from infrastructure.persistence.follow_store import FollowStore
from middleware import _get_current_admin
from services.follow_service import FollowService
from tests.helpers import build_test_client, override_admin_auth

A = "aaaaaaaa-1111-2222-3333-444444444444"
B = "bbbbbbbb-1111-2222-3333-444444444444"


def _run(coro):
    return asyncio.run(coro)


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users "
            "(id TEXT PRIMARY KEY, display_name TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'user')"
        )
        conn.executemany(
            "INSERT OR IGNORE INTO auth_users (id, display_name, role) VALUES (?, ?, ?)",
            [("user-a", "Alice", "user"), ("test-admin-id", "Test Admin", "admin")],
        )
        conn.commit()
    finally:
        conn.close()


@pytest.fixture
def ctx(tmp_path: Path):
    db = tmp_path / "library.db"
    store = FollowStore(db_path=db, write_lock=threading.Lock())
    _seed_auth_users(db)
    # Alice imported A + B with auto-download; a non-admin -> one pending batch.
    _run(store.follow_artists_bulk("user-a", [(A, "Artist A"), (B, "Artist B")]))
    _run(store.set_auto_download_intent_bulk("user-a", [A, B], True))
    batch_id = _run(FollowService(store, AsyncMock()).create_import_batch(
        "user-a", [(A, "Artist A"), (B, "Artist B")]
    ))

    service = FollowService(store, AsyncMock())
    personal_mix_service = AsyncMock()
    personal_mix_service.list_pending_approvals = AsyncMock(return_value=[])

    app = FastAPI()
    app.include_router(requests_router)
    app.dependency_overrides[get_follow_service] = lambda: service
    app.dependency_overrides[get_personal_mix_service] = lambda: personal_mix_service
    override_admin_auth(app)
    return SimpleNamespace(
        client=build_test_client(app), store=store, app=app, batch_id=batch_id
    )


def test_list_batches_groups_card(ctx):
    resp = ctx.client.get("/requests/auto-download-approval-batches")
    assert resp.status_code == 200
    body = resp.json()
    assert body["count"] == 1
    batch = body["batches"][0]
    assert batch["batch_id"] == ctx.batch_id
    assert batch["user_id"] == "user-a"
    assert batch["user_name"] == "Alice"
    assert batch["artist_count"] == 2
    assert batch["source"] == "lidarr_import"
    assert set(batch["sample_names"]) == {"Artist A", "Artist B"}


def test_approve_batch_unlocks_and_clears(ctx):
    resp = ctx.client.post(
        f"/requests/auto-download-approval-batches/{ctx.batch_id}/approve"
    )
    assert resp.status_code == 200
    assert resp.json()["success"] is True
    assert ctx.client.get("/requests/auto-download-approval-batches").json()["count"] == 0
    assert _run(ctx.store.list_auto_download_followers(A.lower())) == ["user-a"]


def test_reject_batch_flips_intent_off_keeps_follow(ctx):
    resp = ctx.client.post(
        f"/requests/auto-download-approval-batches/{ctx.batch_id}/reject"
    )
    assert resp.json()["success"] is True
    by_mbid = {f.artist_mbid: f for f in _run(ctx.store.list_followed_artists("user-a"))}
    assert by_mbid[A].auto_download is False  # intent off
    assert by_mbid[A].artist_name == "Artist A"  # still followed
    assert _run(ctx.store.list_auto_download_followers(A.lower())) == []


def test_approve_missing_batch_reports_failure(ctx):
    resp = ctx.client.post("/requests/auto-download-approval-batches/nope/approve")
    assert resp.status_code == 200
    assert resp.json()["success"] is False


def test_batched_rows_not_in_individual_approvals(ctx):
    # The batch's rows must not also show as individual per-artist cards.
    resp = ctx.client.get("/requests/auto-download-approvals")
    assert resp.json()["count"] == 0


def _stub_album_approval_count(ctx, count: int) -> None:
    stub = SimpleNamespace(get_pending_approval_count=AsyncMock(return_value=count))
    ctx.app.dependency_overrides[get_requests_page_service] = lambda: stub


def test_pending_count_includes_batch_as_one_unit(ctx):
    _stub_album_approval_count(ctx, 0)
    resp = ctx.client.get("/requests/pending-approvals/count")
    assert resp.status_code == 200
    assert resp.json()["count"] == 1  # one batch = one badge unit, not two artists


def test_batch_routes_non_admin_forbidden(ctx):
    ctx.app.dependency_overrides[_get_current_admin] = _deny_admin
    assert ctx.client.get("/requests/auto-download-approval-batches").status_code == 403
    assert (
        ctx.client.post(
            f"/requests/auto-download-approval-batches/{ctx.batch_id}/approve"
        ).status_code
        == 403
    )


def _deny_admin():
    raise HTTPException(status_code=403, detail="admin only")
