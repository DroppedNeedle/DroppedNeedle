"""Admin standing-approval routes for Weekly Mix auto-request (mirrors the
follow auto-download approval flow)."""

import asyncio
import sqlite3
import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI
from starlette.middleware.base import BaseHTTPMiddleware

from api.v1.routes.requests_page import router as requests_router
from core.dependencies import (
    get_follow_service,
    get_personal_mix_service,
    get_requests_page_service,
)
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore
from services.personal_mix_service import PersonalMixService
from tests.helpers import build_test_client, mock_user, override_admin_auth


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
    store = UserListeningPrefsStore(db_path=db, write_lock=threading.Lock())
    _seed_auth_users(db)
    # user-a opts in -> intent on + a pending standing approval.
    _run(store.upsert("user-a", auto_request_personal_mix=True))
    _run(store.upsert_approval("user-a", "pending"))

    service = PersonalMixService(
        client_factory=AsyncMock(),
        mb_repo=AsyncMock(),
        library_repo=AsyncMock(),
        playlist_service=AsyncMock(),
        download_service=AsyncMock(),
        listening_prefs_store=store,
        connections_store=AsyncMock(),
        auth_store=AsyncMock(),
    )

    follow_service = AsyncMock()
    follow_service.list_pending_approvals = AsyncMock(return_value=[])

    app = FastAPI()
    app.include_router(requests_router)
    app.dependency_overrides[get_personal_mix_service] = lambda: service
    app.dependency_overrides[get_follow_service] = lambda: follow_service
    override_admin_auth(app)
    return SimpleNamespace(client=build_test_client(app), store=store, app=app)


def test_list_pending_shows_user_name(ctx):
    resp = ctx.client.get("/requests/personal-mix-approvals")
    assert resp.status_code == 200
    body = resp.json()
    assert body["count"] == 1
    item = body["items"][0]
    assert item["user_id"] == "user-a"
    assert item["user_name"] == "Alice"


def test_approve_sets_state_and_clears_queue(ctx):
    resp = ctx.client.post("/requests/personal-mix-approvals/user-a/approve")
    assert resp.status_code == 200
    assert resp.json()["success"] is True
    assert ctx.client.get("/requests/personal-mix-approvals").json()["count"] == 0
    assert _run(ctx.store.get_approval_state("user-a")) == "approved"
    # intent stays on: the user is now granted
    assert _run(ctx.store.get("user-a")).auto_request_personal_mix is True


def test_reject_flips_intent_off(ctx):
    resp = ctx.client.post("/requests/personal-mix-approvals/user-a/reject")
    assert resp.json()["success"] is True
    assert _run(ctx.store.get_approval_state("user-a")) == "rejected"
    assert _run(ctx.store.get("user-a")).auto_request_personal_mix is False


def test_revoke_flips_intent_off(ctx):
    ctx.client.post("/requests/personal-mix-approvals/user-a/approve")
    resp = ctx.client.post("/requests/personal-mix-approvals/user-a/revoke")
    assert resp.json()["success"] is True
    assert _run(ctx.store.get_approval_state("user-a")) == "revoked"
    assert _run(ctx.store.get("user-a")).auto_request_personal_mix is False


def test_approve_missing_row_reports_failure(ctx):
    resp = ctx.client.post("/requests/personal-mix-approvals/user-x/approve")
    assert resp.status_code == 200
    assert resp.json()["success"] is False


def test_pending_count_includes_personal_mix(ctx):
    stub = SimpleNamespace(get_pending_approval_count=AsyncMock(return_value=2))
    ctx.app.dependency_overrides[get_requests_page_service] = lambda: stub
    resp = ctx.client.get("/requests/pending-approvals/count")
    assert resp.status_code == 200
    assert resp.json()["count"] == 3  # 2 album approvals + 1 personal-mix grant


class _InjectUser(BaseHTTPMiddleware):
    """Simulates AuthMiddleware resolving a plain (non-admin) user."""

    def __init__(self, app, user):
        super().__init__(app)
        self._user = user

    async def dispatch(self, request, call_next):
        request.state.user = self._user
        request.state.token = None
        return await call_next(request)


def test_non_admin_gets_403_on_every_approval_route():
    # the real _get_current_admin gate must reject a plain user - a user
    # approving their own standing grant is the escalation this flow prevents
    app = FastAPI()
    app.include_router(requests_router)
    app.dependency_overrides[get_personal_mix_service] = lambda: AsyncMock()
    app.add_middleware(_InjectUser, user=mock_user())
    client = build_test_client(app)

    assert client.get("/requests/personal-mix-approvals").status_code == 403
    assert client.post("/requests/personal-mix-approvals/user-a/approve").status_code == 403
    assert client.post("/requests/personal-mix-approvals/user-a/reject").status_code == 403
    assert client.post("/requests/personal-mix-approvals/user-a/revoke").status_code == 403
