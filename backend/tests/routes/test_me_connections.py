"""Per-user /me connection + scrobble-preference routes (AMU-2/AMU-3/AMU-6)."""

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
    get_preferences_service,
    get_settings_service,
    get_user_connections_store,
    get_user_listening_prefs_store,
)
from infrastructure.crypto import init_crypto
from infrastructure.persistence.user_connections_store import UserConnectionsStore
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore
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

    app = FastAPI()
    app.include_router(me_router)
    app.dependency_overrides[get_user_connections_store] = lambda: conn_store
    app.dependency_overrides[get_user_listening_prefs_store] = lambda: prefs_store
    app.dependency_overrides[get_lastfm_auth_service] = lambda: lastfm_auth
    app.dependency_overrides[get_settings_service] = lambda: settings_service
    app.dependency_overrides[get_preferences_service] = lambda: prefs_service
    override_user_auth(app, user_id="user-a")
    client = build_test_client(app)
    return SimpleNamespace(
        client=client, app=app, conn_store=conn_store, prefs_store=prefs_store,
        settings_service=settings_service,
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
    resp = ctx.client.delete("/me/connections/spotify")
    assert resp.status_code == 404


def test_connections_do_not_leak_across_users(ctx):
    ctx.client.post("/me/connections/lastfm/auth/session", json={"token": "tok-1"})
    override_user_auth(ctx.app, user_id="user-b")
    assert ctx.client.get("/me/connections").json()["connections"] == []
