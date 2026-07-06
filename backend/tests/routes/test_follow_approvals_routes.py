"""Admin standing-approval routes for auto-download (Phase 3)."""

import asyncio
import sqlite3
import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI

from api.v1.routes.requests_page import router as requests_router
from core.dependencies import (
    get_follow_service,
    get_personal_mix_service,
    get_requests_page_service,
)
from infrastructure.persistence.follow_store import FollowStore
from services.follow_service import FollowService
from tests.helpers import build_test_client, override_admin_auth

MBID = "11111111-2222-3333-4444-555555555555"


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
    # user-a follows + requests auto-download -> a pending standing approval.
    _run(store.follow_artist("user-a", MBID, "Radiohead"))
    _run(store.set_auto_download_intent("user-a", MBID, True))
    _run(store.upsert_approval("user-a", MBID, "Radiohead", "pending"))

    mb_repo = AsyncMock()
    mb_repo.get_artist_by_id.return_value = {"name": "Radiohead"}
    service = FollowService(store, mb_repo)

    # the pending count also sums Weekly Mix grants; keep them empty here
    personal_mix_service = AsyncMock()
    personal_mix_service.list_pending_approvals = AsyncMock(return_value=[])

    app = FastAPI()
    app.include_router(requests_router)
    app.dependency_overrides[get_follow_service] = lambda: service
    app.dependency_overrides[get_personal_mix_service] = lambda: personal_mix_service
    override_admin_auth(app)
    return SimpleNamespace(client=build_test_client(app), store=store, app=app)


def test_list_pending_shows_user_name(ctx):
    resp = ctx.client.get("/requests/auto-download-approvals")
    assert resp.status_code == 200
    body = resp.json()
    assert body["count"] == 1
    item = body["items"][0]
    assert item["user_id"] == "user-a"
    assert item["user_name"] == "Alice"
    assert item["artist_name"] == "Radiohead"


def test_approve_sets_state_and_clears_queue(ctx):
    resp = ctx.client.post(f"/requests/auto-download-approvals/user-a/{MBID}/approve")
    assert resp.status_code == 200
    assert resp.json()["success"] is True
    assert ctx.client.get("/requests/auto-download-approvals").json()["count"] == 0
    approval = _run(ctx.store.get_approval("user-a", MBID))
    assert approval.state == "approved"
    assert approval.reviewed_by_id == "test-admin-id"
    assert approval.reviewed_by_name == "Test Admin"


def test_reject_flips_intent_keeps_follow(ctx):
    resp = ctx.client.post(f"/requests/auto-download-approvals/user-a/{MBID}/reject")
    assert resp.json()["success"] is True
    state = _run(ctx.store.get_follow_state("user-a", MBID))
    assert state.followed is True  # follow retained (L4)
    assert state.auto_download is False  # intent flipped off
    assert state.auto_download_state == "rejected"


def test_revoke_flips_intent_keeps_follow(ctx):
    # approve first, then revoke
    ctx.client.post(f"/requests/auto-download-approvals/user-a/{MBID}/approve")
    resp = ctx.client.post(f"/requests/auto-download-approvals/user-a/{MBID}/revoke")
    assert resp.json()["success"] is True
    state = _run(ctx.store.get_follow_state("user-a", MBID))
    assert state.followed is True
    assert state.auto_download is False
    assert state.auto_download_state == "revoked"


def test_approve_missing_row_reports_failure(ctx):
    resp = ctx.client.post(f"/requests/auto-download-approvals/user-x/{MBID}/approve")
    assert resp.status_code == 200
    assert resp.json()["success"] is False


def test_invalid_mbid_returns_400(ctx):
    resp = ctx.client.post("/requests/auto-download-approvals/user-a/not-an-mbid/approve")
    assert resp.status_code == 400


def _stub_album_approval_count(ctx, count: int) -> None:
    stub = SimpleNamespace(get_pending_approval_count=AsyncMock(return_value=count))
    ctx.app.dependency_overrides[get_requests_page_service] = lambda: stub


def test_pending_count_includes_auto_download_when_no_album_requests(ctx):
    # the reported bug: a lone pending auto-download must still light the sidebar badge
    _stub_album_approval_count(ctx, 0)
    resp = ctx.client.get("/requests/pending-approvals/count")
    assert resp.status_code == 200
    assert resp.json()["count"] == 1


def test_pending_count_sums_album_and_auto_download_approvals(ctx):
    _stub_album_approval_count(ctx, 2)
    resp = ctx.client.get("/requests/pending-approvals/count")
    assert resp.status_code == 200
    assert resp.json()["count"] == 3
