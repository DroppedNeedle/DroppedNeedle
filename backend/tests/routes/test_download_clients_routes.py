"""Download-clients (SABnzbd) + policy route tests: admin auth, masked key, the
SABnzbd Test reporting version/categories."""

from unittest.mock import AsyncMock, MagicMock

from fastapi import FastAPI, HTTPException

from api.v1.routes import download_clients
from api.v1.schemas.settings import (
    SABNZBD_API_KEY_MASK,
    DownloadPolicySettings,
    SabnzbdConnectionSettings,
    WantedWatcherSettings,
)
from core.dependencies import get_preferences_service
from middleware import _get_current_admin, _get_current_user
from models.acquisition_quality import QualityRecipeEntry
from models.common import ServiceStatus
from tests.helpers import build_test_client, mock_admin_user, mock_user


def _prefs():
    prefs = MagicMock()
    prefs.get_sabnzbd_connection.return_value = SabnzbdConnectionSettings(
        enabled=True, url="http://sab:8080", api_key=SABNZBD_API_KEY_MASK
    )
    prefs.get_sabnzbd_connection_raw.return_value = SabnzbdConnectionSettings(
        enabled=True, url="http://sab:8080", api_key="real-key"
    )
    prefs.get_download_policy.return_value = DownloadPolicySettings()
    prefs.save_sabnzbd_connection.return_value = None
    prefs.save_download_policy.return_value = None
    prefs.get_wanted_settings.return_value = WantedWatcherSettings()
    prefs.save_wanted_settings.return_value = None
    return prefs


def _app(prefs=None) -> FastAPI:
    app = FastAPI()
    app.include_router(download_clients.router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs or _prefs()
    return app


def _deny_admin():
    raise HTTPException(status_code=403, detail="admin only")


def test_get_sabnzbd_admin_masked():
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).get("/download-clients/sabnzbd")
    assert resp.status_code == 200
    assert resp.json()["api_key"] == SABNZBD_API_KEY_MASK


def test_get_sabnzbd_non_admin_forbidden():
    app = _app()
    app.dependency_overrides[_get_current_admin] = _deny_admin
    assert build_test_client(app).get("/download-clients/sabnzbd").status_code == 403


def test_get_policy_admin():
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).get("/download-clients/policy")
    assert resp.status_code == 200
    assert resp.json()["preflight_score_auto_accept"] == 0.70
    assert resp.json()["preferred_quality_wait_minutes"] == 15


def test_put_policy_rebuilds_target_download_singletons(monkeypatch):
    from core.dependencies import (
        get_target_download_orchestrator,
        get_target_download_service,
        get_target_file_processor,
    )

    providers = (
        get_target_file_processor,
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
        "/download-clients/policy",
        json={"preflight_score_auto_accept": 0.8},
    )

    assert response.status_code == 200
    for spy in spies:
        spy.assert_called_once()


def test_get_wanted_settings_admin():
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).get("/download-clients/wanted")
    assert resp.status_code == 200
    body = resp.json()
    assert body["enabled"] is True
    assert body["auto_download_on_find"] is True
    assert body["max_checks_per_sweep"] == 3


def test_get_wanted_settings_non_admin_forbidden():
    app = _app()
    app.dependency_overrides[_get_current_admin] = _deny_admin
    assert build_test_client(app).get("/download-clients/wanted").status_code == 403


def test_put_wanted_settings_saves_and_echoes():
    prefs = _prefs()
    saved = WantedWatcherSettings(enabled=False, max_checks_per_sweep=5)
    prefs.get_wanted_settings.return_value = saved
    app = _app(prefs)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).put(
        "/download-clients/wanted",
        json={"enabled": False, "max_checks_per_sweep": 5},
    )
    assert resp.status_code == 200
    prefs.save_wanted_settings.assert_called_once()
    assert prefs.save_wanted_settings.call_args.args[0].enabled is False
    assert resp.json()["enabled"] is False


def test_put_wanted_settings_rejects_out_of_range():
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).put(
        "/download-clients/wanted", json={"max_checks_per_sweep": 0}
    )
    assert resp.status_code in (400, 422)


def test_test_sabnzbd_reports_version_and_categories(monkeypatch):
    from repositories.protocols.download_client import MountDiagnosis

    fake_client = MagicMock()
    fake_client.health_check = AsyncMock(
        return_value=ServiceStatus(status="ok", version="5.0.4")
    )
    fake_client.get_categories = AsyncMock(return_value=["*", "audio"])
    fake_client.get_complete_dir = AsyncMock(return_value="/data/Downloads/complete")
    fake_client.diagnose_downloads_mount = AsyncMock(
        return_value=MountDiagnosis(
            supported=True,
            completed_downloads=2,
            mount_has_files=True,
            resolvable_downloads=2,
            sampled_downloads=2,
            client_downloads_dir="/data/Downloads/complete",
        )
    )
    monkeypatch.setattr(
        download_clients, "build_sabnzbd_download_client", lambda url, key, mount="/tmp": fake_client
    )

    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).post(
        "/download-clients/sabnzbd/test",
        json={"url": "http://sab:8080", "api_key": "k"},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is True
    assert body["version"] == "5.0.4"
    assert "audio" in body["categories"]
    assert body["complete_dir"] == "/data/Downloads/complete"
    assert body["resolvable_downloads"] == 2
    assert body["sampled_downloads"] == 2
    assert body["mount_message"] is None  # healthy mount -> no advisory


def test_test_sabnzbd_flags_unresolvable_mount(monkeypatch):
    from repositories.protocols.download_client import MountDiagnosis

    fake_client = MagicMock()
    fake_client.health_check = AsyncMock(
        return_value=ServiceStatus(status="ok", version="5.0.4")
    )
    fake_client.get_categories = AsyncMock(return_value=["*", "audio"])
    fake_client.get_complete_dir = AsyncMock(return_value="/data/Downloads/complete")
    fake_client.diagnose_downloads_mount = AsyncMock(
        return_value=MountDiagnosis(
            supported=True,
            completed_downloads=3,
            mount_has_files=True,
            resolvable_downloads=0,
            sampled_downloads=3,
            client_downloads_dir="/data/Downloads/complete",
        )
    )
    monkeypatch.setattr(
        download_clients, "build_sabnzbd_download_client", lambda url, key, mount="/tmp": fake_client
    )

    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).post(
        "/download-clients/sabnzbd/test",
        json={
            "url": "http://sab:8080",
            "api_key": "k",
            "downloads_mount": "/wrong/mount",
        },
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is True
    assert body["resolvable_downloads"] == 0
    assert body["sampled_downloads"] == 3
    assert "0/3" in (body["mount_message"] or "")
    assert "/wrong/mount" in (body["mount_message"] or "")


# --- Acquisition-quality policy routes


def test_policy_impact_maps_invalid_submission_to_domain_error():
    """Malformed order submissions must become the domain ValidationError
    (handled as 400), never an unhandled ValueError (500)."""
    from types import SimpleNamespace

    from api.v1.routes.download_clients import preview_policy_impact
    from core.exceptions import ValidationError

    _ = None  # CurrentAdminDep value is unused inside the route body

    class _Prefs:
        @staticmethod
        def get_download_policy():
            raise AssertionError("stored policy must not be read for invalid input")

    import anyio

    async def run():
        await preview_policy_impact(
            _,  # noqa: ARG001 - the dependency value is unused in the route body
            preferences=_Prefs(),
            payload={"quality_preference_order": ["lossless"]},
        )

    try:
        anyio.run(run)
    except ValidationError:
        return
    except ValueError as exc:  # pragma: no cover - regression signal
        raise AssertionError(f"unmapped ValueError leaked to routing: {exc}")
    raise AssertionError("expected ValidationError")


def test_put_policy_recipe_returns_v2_status_and_recipe(monkeypatch):
    prefs = _prefs()
    returned = DownloadPolicySettings(
        quality_recipe=[
            QualityRecipeEntry(format="flac", quality="cd"),
            QualityRecipeEntry(format="mp3", quality="320_plus"),
        ],
        quality_recipe_status="v2",
    )
    prefs.get_download_policy.return_value = returned
    monkeypatch.setattr(download_clients, "_clear_download_client_cache", lambda: None)
    app = _app(prefs)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).put(
        "/download-clients/policy",
        json={
            "flac_mp3_only": True,
            "quality_recipe": [
                {"format": "flac", "quality": "cd"},
                {"format": "mp3", "quality": "320_plus"},
            ],
        },
    )

    assert response.status_code == 200
    body = response.json()
    assert body["quality_recipe_status"] == "v2"
    assert body["quality_recipe_error"] is None
    assert [entry["quality"] for entry in body["quality_recipe"]] == ["cd", "320_plus"]
    prefs.save_download_policy.assert_called_once()


def test_put_policy_recipe_rejects_invalid_entry_before_save(monkeypatch):
    prefs = _prefs()
    monkeypatch.setattr(download_clients, "_clear_download_client_cache", lambda: None)
    app = _app(prefs)
    app.dependency_overrides[_get_current_admin] = mock_admin_user

    response = build_test_client(app).put(
        "/download-clients/policy",
        json={
            "flac_mp3_only": True,
            "quality_recipe": [{"format": "mp3", "quality": "not-valid"}],
        },
    )

    assert response.status_code in (400, 422)
    prefs.save_download_policy.assert_not_called()


def test_policy_summary_exposes_recipe_status_and_error():
    prefs = _prefs()
    prefs.get_download_policy.return_value = DownloadPolicySettings(
        quality_recipe_status="non_convertible",
        quality_recipe_error="v2 recipes require FLAC/MP3-only mode",
        flac_mp3_only=False,
    )
    app = _app(prefs)
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role="user")

    response = build_test_client(app).get("/download-clients/policy-summary")

    assert response.status_code == 200
    body = response.json()
    assert body["quality_recipe_status"] == "non_convertible"
    assert body["quality_recipe_error"] == "v2 recipes require FLAC/MP3-only mode"
