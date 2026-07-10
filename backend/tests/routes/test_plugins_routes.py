"""Plugin routes: admin list/update (with the secret mask-sentinel), and the
curator source surfaces."""

from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import APIRouter, FastAPI

from api.v1.routes import plugins as plugins_routes
from api.v1.schemas.plugins import PLUGIN_SECRET_MASK
from api.v1.schemas.settings import PluginConfig
from core.dependencies import (
    get_plugin_host,
    get_plugin_source_service,
    get_preferences_service,
)
from infrastructure.plugins.host import LoadedPlugin
from infrastructure.plugins.manifest import PluginManifest, PluginSettingField
from infrastructure.plugins.protocols import SourceItem
from middleware import _get_current_admin, _get_current_curator
from tests.helpers import build_test_client, mock_admin_user, mock_user


def _plugin(name: str = "demo", capabilities: list[str] | None = None) -> LoadedPlugin:
    return LoadedPlugin(
        manifest=PluginManifest(
            name=name,
            version="1.0.0",
            api_version=0,
            entrypoint="plugin:Demo",
            capabilities=capabilities or ["scrobbler"],
            display_name="Demo",
            settings=[
                PluginSettingField(key="url", label="URL"),
                PluginSettingField(key="token", label="Token", secret=True),
            ],
        ),
        enabled=True,
        active_capabilities=capabilities or ["scrobbler"],
    )


@pytest.fixture
def harness(tmp_path):
    host = MagicMock()
    plugin = _plugin()
    host.list_plugins = MagicMock(return_value=[plugin])
    host.get = MagicMock(return_value=plugin)
    host.load_all = MagicMock()
    host.sources = MagicMock(
        return_value=[_plugin("lma-source", capabilities=["audio_source"])]
    )
    prefs = MagicMock()
    prefs.get_plugin_config = MagicMock(
        return_value=PluginConfig(enabled=True, settings={"url": "https://x", "token": "s3cret"})
    )
    prefs.save_plugin_config = MagicMock()
    source_service = MagicMock()
    source_service.search = AsyncMock(
        return_value=[SourceItem(id="i1", title="Show", artist="Band", detail="1977")]
    )
    source_service.start_fetch = MagicMock()

    app = FastAPI()
    v1 = APIRouter(prefix="/api/v1")
    v1.include_router(plugins_routes.router)
    app.include_router(v1)
    app.dependency_overrides[get_plugin_host] = lambda: host
    app.dependency_overrides[get_preferences_service] = lambda: prefs
    app.dependency_overrides[get_plugin_source_service] = lambda: source_service
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    app.dependency_overrides[_get_current_curator] = lambda: mock_user(role="trusted")
    return build_test_client(app), host, prefs, source_service


def test_list_masks_secret_settings(harness):
    client, _, _, _ = harness
    response = client.get("/api/v1/plugins")
    assert response.status_code == 200
    plugin = response.json()["plugins"][0]
    assert plugin["settings_values"]["url"] == "https://x"
    assert plugin["settings_values"]["token"] == PLUGIN_SECRET_MASK
    assert "s3cret" not in response.text


def test_update_keeps_secret_when_mask_returned(harness):
    client, host, prefs, _ = harness
    response = client.put(
        "/api/v1/plugins/demo",
        json={"enabled": True, "settings": {"url": "https://new", "token": PLUGIN_SECRET_MASK}},
    )
    assert response.status_code == 200
    saved = prefs.save_plugin_config.call_args.args[1]
    assert saved.settings["url"] == "https://new"
    assert saved.settings["token"] == "s3cret"  # mask means keep-existing
    host.load_all.assert_called_once()


def test_update_unknown_plugin_404s(harness):
    client, host, _, _ = harness
    host.get = MagicMock(return_value=None)
    response = client.put("/api/v1/plugins/nope", json={"enabled": False, "settings": {}})
    assert response.status_code == 404


def test_sources_list_and_search(harness):
    client, _, _, source_service = harness
    response = client.get("/api/v1/plugins/sources")
    assert response.status_code == 200
    assert response.json()["sources"][0]["name"] == "lma-source"

    response = client.post(
        "/api/v1/plugins/sources/lma-source/search", json={"query": "band 1977"}
    )
    assert response.status_code == 200
    assert response.json()["items"][0]["id"] == "i1"
    source_service.search.assert_awaited_once_with("lma-source", "band 1977")


def test_fetch_starts_background_import(harness):
    client, _, _, source_service = harness
    response = client.post(
        "/api/v1/plugins/sources/lma-source/fetch", json={"item_id": "i1"}
    )
    assert response.status_code == 200
    assert response.json() == {"started": True}
    source_service.start_fetch.assert_called_once()
    assert source_service.start_fetch.call_args.kwargs["user_id"] == "test-user-id"


def test_install_forwards_to_host_and_returns_disabled_plugin(harness, monkeypatch):
    client, host, _, _ = harness
    installed = _plugin("fresh")
    installed.enabled = False
    host.install_from_github = AsyncMock(return_value="fresh")
    host.get = MagicMock(return_value=installed)

    response = client.post(
        "/api/v1/plugins/install", json={"repository_url": "https://github.com/o/r"}
    )
    assert response.status_code == 201
    assert response.json()["enabled"] is False
    assert host.install_from_github.await_args.args[0] == "https://github.com/o/r"


def test_install_surfaces_a_bad_url_as_400(harness):
    from infrastructure.plugins.host import PluginInstallError

    client, host, _, _ = harness
    host.install_from_github = AsyncMock(side_effect=PluginInstallError("Enter a public GitHub URL"))

    response = client.post("/api/v1/plugins/install", json={"repository_url": "nope"})
    assert response.status_code == 400
    assert "GitHub" in response.json()["error"]["message"]


def test_uninstall_calls_the_host(harness):
    client, host, _, _ = harness
    host.uninstall = MagicMock()

    response = client.delete("/api/v1/plugins/demo")
    assert response.status_code == 200
    host.uninstall.assert_called_once_with("demo")
