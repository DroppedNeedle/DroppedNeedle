"""Download-client route tests: admin auth on config/test, masking passthrough, status."""

from unittest.mock import AsyncMock, MagicMock

from fastapi import FastAPI, HTTPException

from api.v1.routes import download_client
from api.v1.schemas.settings import (
    DOWNLOAD_CLIENT_API_KEY_MASK,
    DownloadClientConnectionSettings,
)
from core.dependencies import (
    get_download_client_repository,
    get_preferences_service,
    get_settings_service,
)
from middleware import _get_current_admin, _get_current_user
from models.common import ServiceStatus
from tests.helpers import build_test_client, mock_admin_user, mock_user


def _prefs(
    url="http://slskd:5030", key=DOWNLOAD_CLIENT_API_KEY_MASK, library_paths=("/music",)
):
    prefs = MagicMock()
    prefs.get_download_client_settings.return_value = DownloadClientConnectionSettings(
        url=url, api_key=key
    )
    prefs.get_typed_library_settings.return_value = MagicMock(
        library_roots=[MagicMock(path=path) for path in library_paths]
    )
    return prefs


def _client(status="ok", version="0.25.1.0", configured=True):
    client = AsyncMock()
    client.is_configured = MagicMock(return_value=configured)
    client.health_check.return_value = ServiceStatus(
        status=status, version=version, message=f"slskd {version}"
    )
    return client


def _settings_service(status="ok", version="0.25.1.0"):
    svc = MagicMock()
    svc.verify_download_client = AsyncMock(
        return_value=ServiceStatus(
            status=status, version=version, message=f"slskd {version}"
        )
    )
    return svc


def _app(prefs=None, client=None, settings_service=None) -> FastAPI:
    app = FastAPI()
    app.include_router(download_client.router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs or _prefs()
    app.dependency_overrides[get_download_client_repository] = (
        lambda: client or _client()
    )
    app.dependency_overrides[get_settings_service] = (
        lambda: settings_service or _settings_service()
    )
    return app


def _deny_admin():
    raise HTTPException(status_code=403, detail="admin only")


def test_get_config_admin_returns_masked_key():
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).get("/download-client/config")
    assert response.status_code == 200
    assert response.json()["api_key"] == DOWNLOAD_CLIENT_API_KEY_MASK


def test_get_config_non_admin_forbidden():
    app = _app()
    app.dependency_overrides[_get_current_admin] = _deny_admin
    assert build_test_client(app).get("/download-client/config").status_code == 403


def test_get_config_unauthenticated():
    app = _app()
    assert build_test_client(app).get("/download-client/config").status_code == 401


def test_put_config_forwards_masked_sentinel():
    prefs = _prefs()
    app = _app(prefs=prefs)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).put(
        "/download-client/config",
        json={
            "enabled": True,
            "client_type": "slskd",
            "url": "http://new:5030",
            "api_key": DOWNLOAD_CLIENT_API_KEY_MASK,
            "verify_downloads": True,
            "min_bitrate_kbps": 128,
            "preflight_score_auto_accept": 0.7,
            "preflight_score_manual_min": 0.5,
        },
    )
    assert response.status_code == 200
    saved = prefs.save_download_client_settings.call_args[0][0]
    assert saved.api_key == DOWNLOAD_CLIENT_API_KEY_MASK
    assert saved.url == "http://new:5030"


def test_put_config_busts_source_and_target_singleton_chains(monkeypatch):
    """Regression: the orchestrator is built eagerly at startup and holds its
    slskd client, so saving settings must clear its singleton too - otherwise
    downloads keep running against the old/empty URL."""
    from core.dependencies import (
        get_download_orchestrator,
        get_target_download_orchestrator,
        get_target_download_service,
        get_target_file_processor,
        get_target_status_service,
    )

    providers = (
        get_download_orchestrator,
        get_target_file_processor,
        get_target_status_service,
        get_target_download_orchestrator,
        get_target_download_service,
    )
    spies = []
    for provider in providers:
        spy = MagicMock()
        monkeypatch.setattr(provider, "cache_clear", spy)
        spies.append(spy)

    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).put(
        "/download-client/config",
        json={
            "enabled": True,
            "client_type": "slskd",
            "url": "http://new:5030",
            "api_key": DOWNLOAD_CLIENT_API_KEY_MASK,
            "verify_downloads": True,
            "min_bitrate_kbps": 128,
            "preflight_score_auto_accept": 0.7,
            "preflight_score_manual_min": 0.5,
        },
    )
    assert response.status_code == 200
    for spy in spies:
        spy.assert_called_once()


def test_test_connection_admin_verifies_submitted_form_values():
    # The fix: /test verifies the credentials in the request body (what the admin
    # typed), NOT the stored config - so it works before the first save.
    svc = _settings_service(status="ok", version="0.25.1.0")
    app = _app(settings_service=svc)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/download-client/test",
        json={"url": "https://slskd.example.com", "api_key": "typed-key"},
    )
    assert response.status_code == 200
    body = response.json()
    assert body["valid"] is True
    assert body["version"] == "0.25.1.0"
    submitted = svc.verify_download_client.call_args[0][0]
    assert submitted.url == "https://slskd.example.com"
    assert submitted.api_key == "typed-key"


def test_test_connection_normalises_schemeless_url():
    # Regression for the original bug: a bare host must be normalised to a full
    # URL before it reaches httpx (which rejects a schemeless URL).
    svc = _settings_service()
    app = _app(settings_service=svc)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/download-client/test", json={"url": "slskd.example.com", "api_key": "k"}
    )
    assert response.status_code == 200
    submitted = svc.verify_download_client.call_args[0][0]
    assert submitted.url == "https://slskd.example.com"


def test_test_connection_non_admin_forbidden():
    app = _app()
    app.dependency_overrides[_get_current_admin] = _deny_admin
    response = build_test_client(app).post(
        "/download-client/test",
        json={"url": "https://slskd.example.com", "api_key": "k"},
    )
    assert response.status_code == 403


def test_status_authenticated_includes_mount(tmp_path):
    app = _app(
        prefs=_prefs(library_paths=(str(tmp_path),)), client=_client(configured=True)
    )
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role="user")
    response = build_test_client(app).get("/download-client/status")
    assert response.status_code == 200
    body = response.json()
    assert body["configured"] is True
    assert "mount" in body and "reason" in body["mount"]


def test_status_mount_path_comes_from_settings():
    # The downloads-mount path is read from config.py Settings, not DownloadClientSettings.
    from core.config import Settings

    app = _app(client=_client(configured=True))
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role="user")
    body = build_test_client(app).get("/download-client/status").json()
    assert body["mount"]["path"] == str(Settings().slskd_downloads_path)


def test_status_surfaces_mount_advisory_when_downloads_invisible(tmp_path, monkeypatch):
    # The mount passes the basic checks (exists/writable/same-fs) but slskd's completed
    # downloads aren't visible on it -> the advisory flags the silent misconfig.
    from api.v1.routes import download_client as dc_mod
    from repositories.protocols.download_client import MountDiagnosis

    dl = tmp_path / "dl"
    dl.mkdir()
    monkeypatch.setattr(
        dc_mod, "get_settings", lambda: MagicMock(slskd_downloads_path=dl)
    )
    client = _client(configured=True)
    client.diagnose_downloads_mount = AsyncMock(
        return_value=MountDiagnosis(
            supported=True, completed_downloads=5, mount_has_files=False
        )
    )
    app = _app(prefs=_prefs(library_paths=(str(tmp_path),)), client=client)
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role="user")

    body = build_test_client(app).get("/download-client/status").json()
    assert body["mount"]["ok"] is True
    assert body["mount_advisory"] and "finished download" in body["mount_advisory"]


def test_status_no_advisory_when_downloads_visible(tmp_path, monkeypatch):
    # files ARE visible on the mount -> no false alarm.
    from api.v1.routes import download_client as dc_mod
    from repositories.protocols.download_client import MountDiagnosis

    dl = tmp_path / "dl"
    dl.mkdir()
    monkeypatch.setattr(
        dc_mod, "get_settings", lambda: MagicMock(slskd_downloads_path=dl)
    )
    client = _client(configured=True)
    client.diagnose_downloads_mount = AsyncMock(
        return_value=MountDiagnosis(
            supported=True, completed_downloads=5, mount_has_files=True
        )
    )
    app = _app(prefs=_prefs(library_paths=(str(tmp_path),)), client=client)
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role="user")

    body = build_test_client(app).get("/download-client/status").json()
    assert body["mount_advisory"] is None


def test_test_connection_surfaces_auth_message():
    # Issue #193: a rejected key reaches the admin as the fixed auth message.
    message = (
        "Authentication rejected (401) — check the API key and slskd's CIDR allowlist"
    )
    svc = MagicMock()
    svc.verify_download_client = AsyncMock(
        return_value=ServiceStatus(status="error", version=None, message=message)
    )
    app = _app(settings_service=svc)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/download-client/test",
        json={"url": "https://slskd.example.com", "api_key": "wrong-key"},
    )
    assert response.status_code == 200
    body = response.json()
    assert body["valid"] is False
    assert body["message"] == message
