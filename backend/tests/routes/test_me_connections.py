"""Per-user /me connection + scrobble-preference routes (AMU-2/AMU-3/AMU-6)."""

import asyncio
import sqlite3
import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import FastAPI

from api.v1.routes.me_connections import router as me_router
from core.dependencies import (
    get_lastfm_auth_service,
    get_per_user_client_factory,
    get_personal_mix_service,
    get_preferences_service,
    get_settings_service,
    get_user_connections_store,
    get_user_listening_prefs_store,
)
from infrastructure.crypto import init_crypto
from infrastructure.persistence.user_connections_store import UserConnectionsStore
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore
from services.personal_mix_service import PersonalMixResult, PersonalMixService
from tests.helpers import build_test_client, override_user_auth


@pytest.fixture(autouse=True)
def _crypto(tmp_path: Path) -> None:
    init_crypto(tmp_path / "config")


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)"
        )
        conn.executemany(
            "INSERT OR IGNORE INTO auth_users (id, username, role) VALUES (?, ?, ?)",
            [("user-a", "alice", "user"), ("user-b", "bob", "user")],
        )
        conn.commit()
    finally:
        conn.close()


@pytest.fixture
def ctx(tmp_path: Path):
    db = tmp_path / "library.db"
    conn_store = UserConnectionsStore(db_path=db, write_lock=threading.Lock())
    prefs_store = UserListeningPrefsStore(db_path=db, write_lock=threading.Lock())
    _seed_auth_users(db)

    lastfm_auth = AsyncMock()
    lastfm_auth.request_token.return_value = ("tok-1", "https://www.last.fm/api/auth/?token=tok-1")
    lastfm_auth.exchange_session.return_value = ("alice_lfm", "sk-secret", "")

    settings_service = AsyncMock()
    settings_service.verify_listenbrainz.return_value = SimpleNamespace(valid=True, message="ok")

    prefs_service = MagicMock()
    prefs_service.get_lastfm_connection.return_value = SimpleNamespace(api_key="appkey", shared_secret="appsecret")

    # real grant logic against the real prefs store; everything else mocked
    personal_mix_service = PersonalMixService(
        client_factory=AsyncMock(),
        mb_repo=AsyncMock(),
        library_repo=AsyncMock(),
        playlist_service=AsyncMock(),
        download_service=AsyncMock(),
        listening_prefs_store=prefs_store,
        connections_store=conn_store,
        auth_store=AsyncMock(),
    )

    client_factory = AsyncMock()
    client_factory.is_listenbrainz_linked = AsyncMock(return_value=True)

    app = FastAPI()
    app.include_router(me_router)
    app.dependency_overrides[get_user_connections_store] = lambda: conn_store
    app.dependency_overrides[get_user_listening_prefs_store] = lambda: prefs_store
    app.dependency_overrides[get_lastfm_auth_service] = lambda: lastfm_auth
    app.dependency_overrides[get_settings_service] = lambda: settings_service
    app.dependency_overrides[get_preferences_service] = lambda: prefs_service
    app.dependency_overrides[get_personal_mix_service] = lambda: personal_mix_service
    app.dependency_overrides[get_per_user_client_factory] = lambda: client_factory
    override_user_auth(app, user_id="user-a")
    client = build_test_client(app)
    return SimpleNamespace(
        client=client, app=app, conn_store=conn_store, prefs_store=prefs_store,
        settings_service=settings_service, personal_mix_service=personal_mix_service,
        client_factory=client_factory,
    )


def test_list_connections_empty(ctx):
    resp = ctx.client.get("/me/connections")
    assert resp.status_code == 200
    assert resp.json()["connections"] == []


def test_lastfm_session_persists_per_user_and_hides_secret(ctx):
    resp = ctx.client.post("/me/connections/lastfm/auth/session", json={"token": "tok-1"})
    assert resp.status_code == 200
    assert resp.json()["username"] == "alice_lfm"

    listing = ctx.client.get("/me/connections").json()["connections"]
    assert len(listing) == 1
    assert listing[0]["service"] == "lastfm"
    assert listing[0]["username"] == "alice_lfm"
    body = ctx.client.get("/me/connections").text
    assert "sk-secret" not in body
    assert "session_key" not in body


def test_lastfm_token_endpoint(ctx):
    resp = ctx.client.post("/me/connections/lastfm/auth/token")
    assert resp.status_code == 200
    data = resp.json()
    assert data["token"] == "tok-1"
    assert "auth_url" in data


def test_connect_listenbrainz(ctx):
    resp = ctx.client.put(
        "/me/connections/listenbrainz", json={"user_token": "lb-tok", "username": "alice_lb"}
    )
    assert resp.status_code == 200
    assert resp.json()["username"] == "alice_lb"
    listing = ctx.client.get("/me/connections").json()["connections"]
    assert [c["service"] for c in listing] == ["listenbrainz"]


def test_connect_listenbrainz_empty_username_400(ctx):
    # username required server-side, drives Phase-5 per-user reads
    resp = ctx.client.put(
        "/me/connections/listenbrainz", json={"user_token": "lb-tok", "username": "  "}
    )
    assert resp.status_code == 400


def test_connect_listenbrainz_invalid_token_400(ctx):
    ctx.settings_service.verify_listenbrainz.return_value = SimpleNamespace(valid=False, message="bad token")
    resp = ctx.client.put(
        "/me/connections/listenbrainz", json={"user_token": "bad", "username": "x"}
    )
    assert resp.status_code == 400


def test_scrobble_preferences_default(ctx):
    resp = ctx.client.get("/me/scrobble-preferences")
    assert resp.status_code == 200
    data = resp.json()
    assert data["scrobble_to_lastfm"] is False
    assert data["scrobble_to_listenbrainz"] is False
    assert data["primary_music_source"] == "listenbrainz"


def test_scrobble_preferences_update(ctx):
    resp = ctx.client.put(
        "/me/scrobble-preferences",
        json={"scrobble_to_lastfm": True, "primary_music_source": "lastfm"},
    )
    assert resp.status_code == 200
    data = ctx.client.get("/me/scrobble-preferences").json()
    assert data["scrobble_to_lastfm"] is True
    assert data["scrobble_to_listenbrainz"] is False
    assert data["primary_music_source"] == "lastfm"


def test_scrobble_preferences_rejects_bad_source(ctx):
    resp = ctx.client.put("/me/scrobble-preferences", json={"primary_music_source": "spotify"})
    assert resp.status_code == 422


def test_now_playing_visibility_defaults_to_full(ctx):
    data = ctx.client.get("/me/scrobble-preferences").json()
    assert data["now_playing_visibility"] == "full"


def test_now_playing_visibility_update_roundtrips_and_preserves_others(ctx):
    resp = ctx.client.put(
        "/me/scrobble-preferences", json={"now_playing_visibility": "track_hidden"}
    )
    assert resp.status_code == 200
    assert resp.json()["now_playing_visibility"] == "track_hidden"
    data = ctx.client.get("/me/scrobble-preferences").json()
    assert data["now_playing_visibility"] == "track_hidden"
    # partial upsert leaves the other prefs untouched
    assert data["primary_music_source"] == "listenbrainz"


def test_now_playing_visibility_rejects_bad_value(ctx):
    resp = ctx.client.put(
        "/me/scrobble-preferences", json={"now_playing_visibility": "invisible"}
    )
    assert resp.status_code == 422


def test_disconnect(ctx):
    ctx.client.post("/me/connections/lastfm/auth/session", json={"token": "tok-1"})
    resp = ctx.client.delete("/me/connections/lastfm")
    assert resp.status_code == 200
    assert resp.json()["deleted"] is True
    assert ctx.client.get("/me/connections").json()["connections"] == []


def test_disconnect_unknown_service_404(ctx):
    resp = ctx.client.delete("/me/connections/unknown_service")
    assert resp.status_code == 404


def test_connections_do_not_leak_across_users(ctx):
    ctx.client.post("/me/connections/lastfm/auth/session", json={"token": "tok-1"})
    override_user_auth(ctx.app, user_id="user-b")
    assert ctx.client.get("/me/connections").json()["connections"] == []


def test_auto_request_defaults_to_none_state(ctx):
    data = ctx.client.get("/me/scrobble-preferences").json()
    assert data["auto_request_personal_mix"] is False
    assert data["auto_request_state"] == "none"


def test_auto_request_toggle_as_user_enters_pending(ctx):
    resp = ctx.client.put(
        "/me/scrobble-preferences", json={"auto_request_personal_mix": True}
    )
    assert resp.status_code == 200
    data = resp.json()
    assert data["auto_request_personal_mix"] is True
    assert data["auto_request_state"] == "pending"


def test_auto_request_toggle_as_admin_reads_approved(ctx):
    override_user_auth(ctx.app, role="admin", user_id="user-a")
    resp = ctx.client.put(
        "/me/scrobble-preferences", json={"auto_request_personal_mix": True}
    )
    assert resp.status_code == 200
    assert resp.json()["auto_request_state"] == "approved"


def test_toggle_off_keeps_grant_row_for_reenable(ctx):
    ctx.client.put("/me/scrobble-preferences", json={"auto_request_personal_mix": True})
    resp = ctx.client.put(
        "/me/scrobble-preferences", json={"auto_request_personal_mix": False}
    )
    assert resp.json()["auto_request_state"] == "none"
    # the pending row survives the disable (mirrors follows)
    assert asyncio.run(ctx.prefs_store.get_approval_state("user-a")) == "pending"


def test_resending_true_does_not_requeue_an_approved_grant(ctx):
    # a full-object PUT that resends the unchanged toggle must not suspend the grant
    ctx.client.put("/me/scrobble-preferences", json={"auto_request_personal_mix": True})
    asyncio.run(
        ctx.prefs_store.set_approval_state("user-a", "approved", ("admin-x", "Admin X"))
    )
    resp = ctx.client.put(
        "/me/scrobble-preferences",
        json={"auto_request_personal_mix": True, "scrobble_to_lastfm": True},
    )
    assert resp.json()["auto_request_state"] == "approved"


def test_personal_mix_refresh_requires_listenbrainz(ctx):
    ctx.client_factory.is_listenbrainz_linked.return_value = False
    resp = ctx.client.post("/me/personal-mix/refresh")
    assert resp.status_code == 400


def test_personal_mix_refresh_starts_background_build(ctx, monkeypatch):
    build = AsyncMock(return_value=PersonalMixResult(user_id="user-a", track_count=3))
    monkeypatch.setattr(ctx.personal_mix_service, "build_for_user", build)
    resp = ctx.client.post("/me/personal-mix/refresh")
    assert resp.status_code == 200
    assert resp.json()["status"] == "started"


def test_personal_mix_refresh_reports_already_running(ctx, monkeypatch):
    from core.task_registry import TaskRegistry

    registry = TaskRegistry.get_instance()
    monkeypatch.setattr(registry, "is_running", lambda key: True)
    resp = ctx.client.post("/me/personal-mix/refresh")
    assert resp.status_code == 200
    assert resp.json()["status"] == "already_running"
