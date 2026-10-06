"""NZBGet download-client routes: admin auth, the masked password, and Test/status
reporting version, categories and the mount diagnosis.

These exercise the routes through the app rather than calling the repository directly.
The first version of this feature shipped a route that referenced
``build_nzbget_download_client`` without importing it: every call 500'd, the card
reported it as "couldn't reach NZBGet", and nothing caught it, because the repository
suite never went through the route and ruff only enables BLE001. Hence a request-level
test per endpoint.
"""

from unittest.mock import AsyncMock, MagicMock

from fastapi import FastAPI, HTTPException

from api.v1.routes import download_clients
from api.v1.schemas.settings import NZBGET_PASSWORD_MASK, NzbgetConnectionSettings
from core.dependencies import get_preferences_service
from middleware import _get_current_admin
from models.common import ServiceStatus
from repositories.protocols.download_client import MountDiagnosis
from tests.helpers import build_test_client, mock_admin_user


def _prefs():
    prefs = MagicMock()
    prefs.get_nzbget_connection.return_value = NzbgetConnectionSettings(
        enabled=True, url="http://nzbget:6789", username="nzbget", password=NZBGET_PASSWORD_MASK
    )
    prefs.get_nzbget_connection_raw.return_value = NzbgetConnectionSettings(
        enabled=True, url="http://nzbget:6789", username="nzbget", password="real-password"
    )
    prefs.save_nzbget_connection.return_value = None
    return prefs


def _app(prefs=None) -> FastAPI:
    app = FastAPI()
    app.include_router(download_clients.router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs or _prefs()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    return app


def _healthy_client(**overrides):
    client = MagicMock()
    client.health_check = AsyncMock(return_value=ServiceStatus(status="ok", version="24.3"))
    client.get_categories = AsyncMock(return_value=["music", "droppedneedle"])
    client.get_complete_dir = AsyncMock(return_value="/downloads/dst")
    client.diagnose_downloads_mount = AsyncMock(
        return_value=MountDiagnosis(
            supported=True,
            completed_downloads=2,
            mount_has_files=True,
            resolvable_downloads=2,
            sampled_downloads=2,
        )
    )
    for key, value in overrides.items():
        setattr(client, key, value)
    return client


def test_get_nzbget_masks_the_password():
    resp = build_test_client(_app()).get("/download-clients/nzbget")
    assert resp.status_code == 200
    assert resp.json()["password"] == NZBGET_PASSWORD_MASK


def test_get_nzbget_requires_admin():
    app = _app()

    def _deny():
        raise HTTPException(status_code=403, detail="admin only")

    app.dependency_overrides[_get_current_admin] = _deny
    assert build_test_client(app).get("/download-clients/nzbget").status_code == 403


def test_put_nzbget_saves_and_returns_the_masked_settings():
    prefs = _prefs()
    resp = build_test_client(_app(prefs)).put(
        "/download-clients/nzbget",
        json={
            "enabled": True,
            "url": "http://nzbget:6789",
            "username": "nzbget",
            "password": "secret",
            "downloads_mount": "/nzbget-downloads",
        },
    )
    assert resp.status_code == 200
    assert prefs.save_nzbget_connection.called
    assert resp.json()["password"] == NZBGET_PASSWORD_MASK


def test_test_nzbget_reports_version_categories_and_mount(monkeypatch):
    monkeypatch.setattr(
        download_clients,
        "build_nzbget_download_client",
        lambda url, user, password, mount="/tmp": _healthy_client(),
    )
    resp = build_test_client(_app()).post(
        "/download-clients/nzbget/test",
        json={"url": "http://nzbget:6789", "username": "nzbget", "password": "p"},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is True
    assert body["version"] == "24.3"
    assert "droppedneedle" in body["categories"]
    assert body["complete_dir"] == "/downloads/dst"
    assert body["mount_message"] is None  # healthy mount -> no advisory


def test_test_nzbget_flags_an_unresolvable_mount(monkeypatch):
    client = _healthy_client(
        diagnose_downloads_mount=AsyncMock(
            return_value=MountDiagnosis(
                supported=True,
                completed_downloads=3,
                mount_has_files=True,
                resolvable_downloads=0,
                sampled_downloads=3,
            )
        )
    )
    monkeypatch.setattr(
        download_clients,
        "build_nzbget_download_client",
        lambda url, user, password, mount="/tmp": client,
    )
    resp = build_test_client(_app()).post(
        "/download-clients/nzbget/test",
        json={
            "url": "http://nzbget:6789",
            "username": "nzbget",
            "password": "p",
            "downloads_mount": "/wrong",
        },
    )
    body = resp.json()
    assert body["valid"] is True
    assert "/wrong" in body["mount_message"]


def test_test_nzbget_uses_the_stored_password_for_the_mask(monkeypatch):
    seen = {}

    def _build(url, user, password, mount="/tmp"):
        seen["password"] = password
        return _healthy_client()

    monkeypatch.setattr(download_clients, "build_nzbget_download_client", _build)
    build_test_client(_app()).post(
        "/download-clients/nzbget/test",
        json={
            "url": "http://nzbget:6789",
            "username": "nzbget",
            "password": NZBGET_PASSWORD_MASK,
        },
    )
    # The UI round-trips the sentinel, so testing it must resolve to the saved secret.
    assert seen["password"] == "real-password"


def test_test_nzbget_reports_an_unexpected_failure_instead_of_500(monkeypatch):
    def _explode(url, user, password, mount="/tmp"):
        raise RuntimeError("boom")

    monkeypatch.setattr(download_clients, "build_nzbget_download_client", _explode)
    resp = build_test_client(_app()).post(
        "/download-clients/nzbget/test",
        json={"url": "http://nzbget:6789", "username": "nzbget", "password": "p"},
    )
    # A 500 reaches the card as a generic "couldn't reach", which sends people hunting a
    # network fault that isn't there. The cause has to come back as the test result.
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is False
    assert "boom" in body["message"]


def test_nzbget_status_reports_not_configured_without_a_url():
    prefs = _prefs()
    prefs.get_nzbget_connection_raw.return_value = NzbgetConnectionSettings(enabled=False, url="")
    resp = build_test_client(_app(prefs)).get("/download-clients/nzbget/status")
    assert resp.status_code == 200
    assert resp.json() == {
        "valid": False,
        "version": None,
        "message": "Not configured",
        "categories": [],
        "complete_dir": None,
        "mount_has_files": None,
        "resolvable_downloads": None,
        "sampled_downloads": None,
        "mount_message": None,
    }


def test_nzbget_status_reports_the_live_version(monkeypatch):
    monkeypatch.setattr(
        download_clients,
        "build_nzbget_download_client",
        lambda url, user, password, mount="/tmp": _healthy_client(),
    )
    resp = build_test_client(_app()).get("/download-clients/nzbget/status")
    body = resp.json()
    assert body["valid"] is True
    assert body["version"] == "24.3"
