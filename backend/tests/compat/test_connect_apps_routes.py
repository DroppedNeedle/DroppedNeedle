"""T6.1a - /api/v1/connect-apps management routes (admin gating, app-passwords)."""

import json
from pathlib import Path

import pytest
from fastapi import FastAPI, HTTPException

from api.v1.routes import connect_apps_routes
from core.config import Settings
from core.dependencies import get_app_password_service, get_preferences_service
from middleware import _get_current_admin, _get_current_user
from services.preferences_service import PreferencesService
from tests.helpers import build_test_client, mock_admin_user, mock_user

pytestmark = pytest.mark.asyncio


def _deny_admin():
    raise HTTPException(status_code=403, detail="admin only")


def _prefs(tmp_path: Path) -> PreferencesService:
    settings = Settings()
    settings.config_file_path = tmp_path / "cfg.json"
    return PreferencesService(settings)


def _app(app_password_service, prefs) -> FastAPI:
    app = FastAPI()
    app.include_router(connect_apps_routes.router)
    app.dependency_overrides[get_app_password_service] = lambda: app_password_service
    app.dependency_overrides[get_preferences_service] = lambda: prefs
    return app


def _as_user(app, user_id="user-alice"):
    app.dependency_overrides[_get_current_user] = lambda: mock_user(user_id=user_id)


def _as_admin(app):
    app.dependency_overrides[_get_current_admin] = mock_admin_user


# ----- settings -----

async def test_get_settings_any_user(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    _as_user(app)
    r = build_test_client(app).get("/connect-apps/settings")
    assert r.status_code == 200
    assert r.json()["subsonic_enabled"] is False
    assert r.json()["exact_track_approval_supported"] is True


async def test_get_settings_unauthenticated_401(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    r = build_test_client(app).get("/connect-apps/settings")
    assert r.status_code == 401


async def test_put_settings_admin_persists(app_password_service, tmp_path):
    prefs = _prefs(tmp_path)
    app = _app(app_password_service, prefs)
    _as_admin(app)
    r = build_test_client(app).put("/connect-apps/settings", json={
        "subsonic_enabled": True, "jellyfin_enabled": True,
        "transcoding_enabled": True, "transcode_default_format": "opus",
        "transcode_max_bitrate_kbps": 192, "advertise_server_name": "DroppedNeedle",
        "advertise_server_version": "10.10.6", "discover_mode": "lazy-mb",
    })
    assert r.status_code == 200
    assert r.json()["discover_mode"] == "lazy-mb"
    assert prefs.get_connect_apps_settings().subsonic_enabled is True


async def test_put_settings_non_admin_403(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    app.dependency_overrides[_get_current_admin] = _deny_admin
    r = build_test_client(app).put("/connect-apps/settings", json={"subsonic_enabled": True})
    assert r.status_code == 403


async def test_put_settings_bad_bitrate_422(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    _as_admin(app)
    r = build_test_client(app).put(
        "/connect-apps/settings", json={"transcode_max_bitrate_kbps": 8}
    )
    assert r.status_code == 422


# ----- app-passwords -----

async def test_create_returns_secret_once_no_secret_in_list(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    _as_user(app)
    client = build_test_client(app)
    created = client.post("/connect-apps/app-passwords", json={"name": "Symfonium (phone)"})
    assert created.status_code == 200
    body = created.json()
    assert body["secret"]  # plaintext shown once
    view = body["app_password"]
    assert set(view.keys()) == {"id", "name", "created_at", "last_used_at", "last_client"}
    assert "secret" not in json.dumps(view)

    listed = client.get("/connect-apps/app-passwords")
    assert listed.status_code == 200
    lb = listed.json()
    assert lb["cap"] == 25 and lb["active_count"] == 1
    assert all("secret" not in k for row in lb["items"] for k in row)


async def test_create_requires_name(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    _as_user(app)
    r = build_test_client(app).post("/connect-apps/app-passwords", json={"name": "  "})
    assert r.status_code == 400


async def test_revoke_owner_204_then_gone(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    _as_user(app)
    client = build_test_client(app)
    pid = client.post("/connect-apps/app-passwords", json={"name": "X"}).json()["app_password"]["id"]
    assert client.delete(f"/connect-apps/app-passwords/{pid}").status_code == 204
    assert client.get("/connect-apps/app-passwords").json()["active_count"] == 0


async def test_revoke_non_owner_404(app_password_service, tmp_path):
    # alice creates; bob tries to revoke -> 404 (ownership enforced, no id leak)
    app = _app(app_password_service, _prefs(tmp_path))
    _as_user(app, "user-alice")
    client = build_test_client(app)
    pid = client.post("/connect-apps/app-passwords", json={"name": "X"}).json()["app_password"]["id"]
    _as_user(app, "user-bob")
    assert build_test_client(app).delete(f"/connect-apps/app-passwords/{pid}").status_code == 404


async def test_create_at_cap_409(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    _as_user(app)
    client = build_test_client(app)
    for i in range(25):
        assert client.post("/connect-apps/app-passwords", json={"name": f"c{i}"}).status_code == 200
    r = client.post("/connect-apps/app-passwords", json={"name": "too-many"})
    assert r.status_code == 409


# ----- admin oversight roster -----

async def test_admin_roster_lists_all_users_with_owner_no_secret(
    app_password_service, tmp_path
):
    await app_password_service.create("user-alice", "Alice phone")
    await app_password_service.create("user-bob", "Bob tablet")
    app = _app(app_password_service, _prefs(tmp_path))
    _as_admin(app)
    r = build_test_client(app).get("/connect-apps/admin/app-passwords")
    assert r.status_code == 200
    body = r.json()
    assert body["active_count"] == 2
    by_user = {row["user_id"]: row for row in body["items"]}
    assert set(by_user) == {"user-alice", "user-bob"}
    assert by_user["user-alice"]["owner_username"] == "alice"
    assert by_user["user-alice"]["owner_display_name"] == "Alice"
    # never a secret, and only the expected metadata columns
    assert "secret" not in json.dumps(body)
    for row in body["items"]:
        assert set(row.keys()) == {
            "id", "user_id", "owner_username", "owner_display_name",
            "name", "created_at", "last_used_at", "last_client",
        }


async def test_admin_roster_forbidden_to_non_admin(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    app.dependency_overrides[_get_current_admin] = _deny_admin
    r = build_test_client(app).get("/connect-apps/admin/app-passwords")
    assert r.status_code == 403


async def test_admin_revoke_any_users_password_204_then_gone(
    app_password_service, tmp_path
):
    record, _ = await app_password_service.create("user-alice", "Alice phone")
    app = _app(app_password_service, _prefs(tmp_path))
    _as_admin(app)
    client = build_test_client(app)
    r = client.delete(f"/connect-apps/admin/app-passwords/{record.id}")
    assert r.status_code == 204
    assert client.get("/connect-apps/admin/app-passwords").json()["active_count"] == 0


async def test_admin_revoke_unknown_id_404(app_password_service, tmp_path):
    app = _app(app_password_service, _prefs(tmp_path))
    _as_admin(app)
    r = build_test_client(app).delete("/connect-apps/admin/app-passwords/nope")
    assert r.status_code == 404


async def test_admin_revoke_forbidden_to_non_admin(app_password_service, tmp_path):
    record, _ = await app_password_service.create("user-alice", "Alice phone")
    app = _app(app_password_service, _prefs(tmp_path))
    app.dependency_overrides[_get_current_admin] = _deny_admin
    r = build_test_client(app).delete(f"/connect-apps/admin/app-passwords/{record.id}")
    assert r.status_code == 403
