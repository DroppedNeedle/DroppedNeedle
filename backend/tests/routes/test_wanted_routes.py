"""Wanted watch routes (Wanted plan §6 Phase 2): the ownership auth matrix
(401 unauth / 403 non-owner / owner ok / admin ok) and stop/resume/seen
behaviour against a real WantedStore behind a real WantedWatcherService."""

import asyncio
import threading
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest
from fastapi import FastAPI

from api.v1.routes.requests_page import router
from core.dependencies import get_auth_store, get_wanted_watcher_service
from infrastructure.persistence.wanted_store import WantedStore
from middleware import _get_current_user
from services.native.wanted_watcher_service import WantedWatcherService
from tests.helpers import build_test_client, mock_user

VALID_MBID = "22222222-2222-2222-2222-222222222222"
OTHER_MBID = "33333333-3333-3333-3333-333333333333"
MISSING_MBID = "44444444-4444-4444-4444-444444444444"
OWNER_ID = "owner-user-id"

_ENDPOINTS = [
    ("GET", "/requests/wanted"),
    ("POST", f"/requests/wanted/{VALID_MBID}/stop"),
    ("POST", f"/requests/wanted/{VALID_MBID}/resume"),
    ("POST", f"/requests/wanted/{VALID_MBID}/seen"),
]


def _make_watcher(tmp_path) -> tuple[WantedWatcherService, WantedStore]:
    store = WantedStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())

    async def _seed() -> None:
        await store.create_watch(
            release_group_mbid=VALID_MBID, user_id=OWNER_ID, artist_name="Yan Qing",
            album_title="the arrival", kind="missing", next_check_at=time.time() + 3600,
        )
        await store.create_watch(
            release_group_mbid=OTHER_MBID, user_id="someone-else", artist_name="Other",
            album_title="Album", kind="partial", next_check_at=time.time() + 3600,
        )
        await store.record_cycle(
            VALID_MBID, outcome="new_manual", next_check_at=time.time() + 3600,
            quiet=False, new_candidate_count=3,
        )

    asyncio.run(_seed())
    request_history = AsyncMock()
    request_history.async_get_history.return_value = ([], 0)
    watcher = WantedWatcherService(
        wanted_store=store,
        request_history=request_history,
        download_store=AsyncMock(),
        get_download_service=lambda: AsyncMock(),
        library_manager=AsyncMock(),
        album_service=AsyncMock(),
        mb_repo=AsyncMock(),
        sse_publisher=AsyncMock(),
        preferences=Mock(),
    )
    return watcher, store


def _client(tmp_path, *, role: str | None = "user", user_id: str = OWNER_ID):
    app = FastAPI()
    app.include_router(router)
    watcher, store = _make_watcher(tmp_path)
    app.dependency_overrides[get_wanted_watcher_service] = lambda: watcher
    auth_store = AsyncMock()
    auth_store.list_users.return_value = [
        SimpleNamespace(id=OWNER_ID, display_name="Owner"),
        SimpleNamespace(id="someone-else", display_name="Someone Else"),
    ]
    app.dependency_overrides[get_auth_store] = lambda: auth_store
    if role is not None:
        app.dependency_overrides[_get_current_user] = lambda: mock_user(
            role=role, user_id=user_id
        )
    client = build_test_client(app)
    client.wanted_store = store  # type: ignore[attr-defined] - test-side handle
    return client


# --- auth matrix ---


@pytest.mark.parametrize("method,path", _ENDPOINTS)
def test_unauthenticated_gets_401(tmp_path, method, path):
    client = _client(tmp_path, role=None)
    assert client.request(method, path).status_code == 401


@pytest.mark.parametrize("method,path", _ENDPOINTS[1:])  # the mutating three
def test_non_owner_gets_403(tmp_path, method, path):
    client = _client(tmp_path, user_id="someone-else")
    assert client.request(method, path).status_code == 403


@pytest.mark.parametrize("method,path", _ENDPOINTS[1:])
def test_owner_is_admitted(tmp_path, method, path):
    client = _client(tmp_path)
    assert client.request(method, path).status_code == 200


@pytest.mark.parametrize("method,path", _ENDPOINTS[1:])
def test_admin_is_admitted_on_any_watch(tmp_path, method, path):
    client = _client(tmp_path, role="admin", user_id="admin-id")
    assert client.request(method, path).status_code == 200


def test_unknown_watch_is_404(tmp_path):
    client = _client(tmp_path)
    assert client.post(f"/requests/wanted/{MISSING_MBID}/stop").status_code == 404


def test_invalid_mbid_is_400(tmp_path):
    client = _client(tmp_path)
    assert client.post("/requests/wanted/not-a-mbid/stop").status_code == 400


# --- behaviour ---


def test_list_scopes_to_the_caller(tmp_path):
    client = _client(tmp_path)
    body = client.get("/requests/wanted").json()
    assert body["count"] == 1
    item = body["items"][0]
    assert item["release_group_mbid"] == VALID_MBID
    assert item["kind"] == "missing"
    assert item["state"] == "watching"
    assert item["new_candidate_count"] == 3
    assert item["last_outcome"] == "new_manual"
    assert item["cover_url"]  # cover falls back to the release-group cover route


def test_list_shows_admin_everything_with_owner_names(tmp_path):
    client = _client(tmp_path, role="admin", user_id="admin-id")
    body = client.get("/requests/wanted").json()
    assert body["count"] == 2
    assert {i["user_id"] for i in body["items"]} == {OWNER_ID, "someone-else"}
    assert {i["user_name"] for i in body["items"]} == {"Owner", "Someone Else"}


def test_list_omits_owner_names_for_non_admins(tmp_path):
    client = _client(tmp_path)
    body = client.get("/requests/wanted").json()
    assert body["items"][0]["user_name"] is None


def test_list_includes_read_only_retrying_rows(tmp_path):
    from models.wanted import WantedRetrying

    client = _client(tmp_path)
    watcher = client.app.dependency_overrides[get_wanted_watcher_service]()
    watcher.list_retrying_for = AsyncMock(
        return_value=[
            WantedRetrying(
                release_group_mbid=MISSING_MBID,
                artist_name="Yan Qing",
                album_title="the arrival",
                retry_count=0,
                max_attempts=6,
                next_retry_at=time.time() + 900,
                user_id=OWNER_ID,
            )
        ]
    )
    body = client.get("/requests/wanted").json()
    assert len(body["retrying"]) == 1
    row = body["retrying"][0]
    assert row["release_group_mbid"] == MISSING_MBID
    assert row["retry_count"] == 0
    assert row["max_attempts"] == 6
    assert row["cover_url"]  # release-group cover fallback applied
    # watches themselves are unaffected
    assert body["count"] == 1


def test_stop_then_resume_roundtrip(tmp_path):
    client = _client(tmp_path)
    stopped = client.post(f"/requests/wanted/{VALID_MBID}/stop").json()
    assert stopped == {"success": True, "state": "stopped"}
    resumed = client.post(f"/requests/wanted/{VALID_MBID}/resume").json()
    assert resumed == {"success": True, "state": "watching"}


def test_resume_on_a_watching_want_is_check_now(tmp_path):
    client = _client(tmp_path)
    resp = client.post(f"/requests/wanted/{VALID_MBID}/resume")
    assert resp.json() == {"success": True, "state": "watching"}

    async def _next_check():
        watch = await client.wanted_store.get_watch(VALID_MBID)
        return watch.next_check_at

    assert asyncio.run(_next_check()) <= time.time()  # due immediately


def test_resume_on_a_fulfilled_want_is_400(tmp_path):
    client = _client(tmp_path)

    async def _fulfil():
        await client.wanted_store.mark_fulfilled(VALID_MBID, "satisfied")

    asyncio.run(_fulfil())
    assert client.post(f"/requests/wanted/{VALID_MBID}/resume").status_code == 400


def test_dismiss_review_route_delegates_to_the_watcher(tmp_path):
    """'None of these - keep watching' lives on the downloads-search router but is
    the watcher's action; ownership/404 behaviour is covered by the service tests."""
    from api.v1.routes import downloads_search

    app = FastAPI()
    app.include_router(downloads_search.router)
    watcher = AsyncMock()
    watcher.dismiss_review.return_value = SimpleNamespace(state="watching")
    app.dependency_overrides[get_wanted_watcher_service] = lambda: watcher
    app.dependency_overrides[_get_current_user] = lambda: mock_user(
        role="user", user_id=OWNER_ID
    )
    client = build_test_client(app)

    resp = client.post("/downloads/search/job-1/dismiss")

    assert resp.status_code == 200
    assert resp.json() == {"success": True, "state": "watching"}
    watcher.dismiss_review.assert_awaited_once_with("job-1", OWNER_ID, "user")


def test_dismiss_review_route_rejects_unauthenticated(tmp_path):
    from api.v1.routes import downloads_search

    app = FastAPI()
    app.include_router(downloads_search.router)
    app.dependency_overrides[get_wanted_watcher_service] = lambda: AsyncMock()
    client = build_test_client(app)
    assert client.post("/downloads/search/job-1/dismiss").status_code == 401


def test_seen_clears_the_badge(tmp_path):
    client = _client(tmp_path)
    assert client.post(f"/requests/wanted/{VALID_MBID}/seen").status_code == 200
    body = client.get("/requests/wanted").json()
    assert body["items"][0]["new_candidate_count"] == 0
