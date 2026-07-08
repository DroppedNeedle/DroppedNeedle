"""Lidarr importer route tests: admin gating on config/test, user access to
status/artists/import, masked-key passthrough, and the Test success/bad-key body.

Auth gotcha (CLAUDE.md): overriding ``_get_current_user`` does NOT unlock admin routes -
the admin endpoints resolve ``_get_current_admin`` directly, so override that."""

from unittest.mock import AsyncMock, MagicMock

from fastapi import FastAPI, HTTPException

from api.v1.routes import lidarr_import
from api.v1.schemas.lidarr_import import (
    LidarrArtistCandidate,
    LidarrArtistListResponse,
    LidarrImportResponse,
)
from api.v1.schemas.settings import (
    LIDARR_IMPORT_API_KEY_MASK,
    LidarrImportConnectionSettings,
)
from core.dependencies import (
    get_lidarr_import_repository,
    get_lidarr_import_service,
    get_preferences_service,
)
from core.exceptions import LidarrImportError
from infrastructure.resilience.retry import CircuitOpenError
from middleware import _get_current_admin, _get_current_user
from repositories.lidarr_import.lidarr_import_models import LidarrSystemStatus
from tests.helpers import build_test_client, mock_admin_user, mock_user

MBID = "11111111-1111-1111-1111-111111111111"


def _prefs():
    prefs = MagicMock()
    prefs.get_lidarr_import_connection.return_value = LidarrImportConnectionSettings(
        url="http://lidarr.test", api_key=LIDARR_IMPORT_API_KEY_MASK
    )
    prefs.get_lidarr_import_connection_raw.return_value = LidarrImportConnectionSettings(
        url="http://lidarr.test", api_key="real-key"
    )
    prefs.save_lidarr_import_connection.return_value = None
    prefs.is_lidarr_import_configured.return_value = True
    return prefs


def _service():
    service = AsyncMock()
    service.list_import_candidates.return_value = LidarrArtistListResponse(
        artists=[
            LidarrArtistCandidate(
                mbid=MBID, name="Auto Artist", monitor_new_items="all",
                already_following=False, would_auto_download=True,
            )
        ],
        total=1,
    )
    service.import_artists.return_value = LidarrImportResponse(
        imported=1, already_following=0, skipped_invalid=0,
        auto_download_enabled=1, approval_batch_id="batch-1",
    )
    return service


def _repo(status_version="3.1.3.4968", raise_exc=None):
    repo = AsyncMock()
    if raise_exc is not None:
        repo.get_system_status.side_effect = raise_exc
    else:
        repo.get_system_status.return_value = LidarrSystemStatus(version=status_version)
    return repo


def _app(prefs=None, service=None, repo=None) -> FastAPI:
    app = FastAPI()
    app.include_router(lidarr_import.router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs or _prefs()
    app.dependency_overrides[get_lidarr_import_service] = lambda: service or _service()
    app.dependency_overrides[get_lidarr_import_repository] = lambda: repo or _repo()
    return app


def _deny_admin():
    raise HTTPException(status_code=403, detail="admin only")


def _as_admin(app):
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    app.dependency_overrides[_get_current_user] = mock_admin_user
    return app


def _as_user(app):
    app.dependency_overrides[_get_current_user] = lambda: mock_user()
    app.dependency_overrides[_get_current_admin] = _deny_admin
    return app


# --- config (admin) -----------------------------------------------------------


def test_get_config_admin_returns_masked_key():
    resp = build_test_client(_as_admin(_app())).get("/lidarr-import/config")
    assert resp.status_code == 200
    assert resp.json()["api_key"] == LIDARR_IMPORT_API_KEY_MASK


def test_get_config_non_admin_forbidden():
    assert build_test_client(_as_user(_app())).get("/lidarr-import/config").status_code == 403


def test_get_config_unauthenticated():
    assert build_test_client(_app()).get("/lidarr-import/config").status_code == 401


def test_put_config_admin_saves():
    prefs = _prefs()
    resp = build_test_client(_as_admin(_app(prefs=prefs))).put(
        "/lidarr-import/config", json={"url": "http://lidarr.test", "api_key": "k"}
    )
    assert resp.status_code == 200
    prefs.save_lidarr_import_connection.assert_called_once()


# --- test (admin) -------------------------------------------------------------


def test_test_connection_success_reports_version():
    resp = build_test_client(_as_admin(_app())).post(
        "/lidarr-import/test", json={"url": "http://lidarr.test", "api_key": "k"}
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is True
    assert body["version"] == "3.1.3.4968"


def test_test_connection_bad_key_reports_auth():
    repo = _repo(raise_exc=LidarrImportError("bad", auth=True))
    resp = build_test_client(_as_admin(_app(repo=repo))).post(
        "/lidarr-import/test", json={"url": "http://lidarr.test", "api_key": "k"}
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is False
    assert "API key" in body["message"]


def test_test_connection_circuit_open_reports_unavailable():
    repo = _repo(raise_exc=CircuitOpenError("open", breaker_name="lidarr_import"))
    resp = build_test_client(_as_admin(_app(repo=repo))).post(
        "/lidarr-import/test", json={"url": "http://lidarr.test", "api_key": "k"}
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is False
    assert "temporarily unavailable" in body["message"]


def test_test_connection_masked_key_resolves_stored():
    prefs = _prefs()
    repo = _repo()
    build_test_client(_as_admin(_app(prefs=prefs, repo=repo))).post(
        "/lidarr-import/test",
        json={"url": "http://lidarr.test", "api_key": LIDARR_IMPORT_API_KEY_MASK},
    )
    # Masked key resolved to the stored raw key before probing (url, api_key positional).
    called_key = repo.get_system_status.call_args.args[1]
    assert called_key == "real-key"


def test_test_connection_non_admin_forbidden():
    resp = build_test_client(_as_user(_app())).post(
        "/lidarr-import/test", json={"url": "http://lidarr.test", "api_key": "k"}
    )
    assert resp.status_code == 403


# --- status / artists / import (user) -----------------------------------------


def test_status_user_returns_only_configured():
    resp = build_test_client(_as_user(_app())).get("/lidarr-import/status")
    assert resp.status_code == 200
    body = resp.json()
    assert body == {"configured": True}  # never leaks url/api_key


def test_status_unauthenticated():
    assert build_test_client(_app()).get("/lidarr-import/status").status_code == 401


def test_list_artists_user_ok():
    resp = build_test_client(_as_user(_app())).get("/lidarr-import/artists")
    assert resp.status_code == 200
    body = resp.json()
    assert body["total"] == 1
    assert body["artists"][0]["mbid"] == MBID


def test_import_user_returns_summary():
    service = _service()
    resp = build_test_client(_as_user(_app(service=service))).post(
        "/lidarr-import/import", json={"selected_mbids": [MBID]}
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["imported"] == 1
    assert body["approval_batch_id"] == "batch-1"
    service.import_artists.assert_awaited_once()


def test_import_unauthenticated():
    resp = build_test_client(_app()).post(
        "/lidarr-import/import", json={"selected_mbids": [MBID]}
    )
    assert resp.status_code == 401
