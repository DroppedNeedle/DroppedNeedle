"""Admin plugin API routes: admin gating, list shape, enable/disable, settings
GET/PUT with masked secrets, and the test endpoint."""

import textwrap

from fastapi import FastAPI, HTTPException

from api.v1.routes import plugins as plugins_routes
from core.dependencies import get_plugin_manager
from middleware import _get_current_admin
from plugins.base import API_VERSION, PLUGIN_SECRET_MASK
from plugins.manager import PluginManager
from tests.helpers import build_test_client, mock_admin_user

_EXTERNAL_PLUGIN = textwrap.dedent(
    '''
    from plugins.base import AcquisitionPlugin, SettingsField, TestResult


    class RoutePlugin(AcquisitionPlugin):
        id = "route_plugin"
        name = "Route Plugin"
        version = "1.1.0"

        def settings_schema(self):
            return [
                SettingsField(key="endpoint", type="str", label="Endpoint", default=""),
                SettingsField(key="token", type="secret", label="Token", default=""),
            ]

        def configure(self, settings):
            self.applied = dict(settings)

        async def test_connection(self, settings=None):
            values = settings or getattr(self, "applied", {})
            if not values.get("endpoint"):
                return TestResult(ok=False, message="endpoint required")
            return TestResult(ok=True, message="reachable", version="9.9")

        async def search(self, request):
            return []

        async def enqueue(self, request):
            raise NotImplementedError

        async def get_status(self, handle):
            raise NotImplementedError

        async def cancel(self, handle):
            return True

        async def completed_path(self, handle):
            return []
    '''
)


class _FakePrefs:
    def __init__(self):
        self.store: dict[str, dict] = {}

    def get_plugins_config(self) -> dict:
        return dict(self.store)

    def save_plugin_config_raw(self, plugin_id: str, cfg: dict) -> None:
        self.store[plugin_id] = cfg


def _manager(tmp_path) -> PluginManager:
    ext = tmp_path / "plugins"
    ext.mkdir(parents=True, exist_ok=True)
    (ext / "route_plugin.py").write_text(_EXTERNAL_PLUGIN, encoding="utf-8")
    (ext / "broken.py").write_text("raise RuntimeError('kaput')", encoding="utf-8")
    return PluginManager(preferences=_FakePrefs(), external_dir=ext)


def _app(manager) -> FastAPI:
    app = FastAPI()
    app.include_router(plugins_routes.router)
    app.dependency_overrides[get_plugin_manager] = lambda: manager
    return app


def _deny_admin():
    raise HTTPException(status_code=403, detail="admin only")


def _admin_client(tmp_path):
    manager = _manager(tmp_path)
    app = _app(manager)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    return build_test_client(app), manager


def test_list_requires_admin(tmp_path):
    app = _app(_manager(tmp_path))
    app.dependency_overrides[_get_current_admin] = _deny_admin
    assert build_test_client(app).get("/plugins").status_code == 403


def test_list_shape_builtins_external_and_quarantined(tmp_path):
    client, _ = _admin_client(tmp_path)
    resp = client.get("/plugins")
    assert resp.status_code == 200
    body = resp.json()
    assert body["api_version"] == API_VERSION
    by_id = {p["id"]: p for p in body["plugins"]}

    assert by_id["soulseek"]["builtin"] is True
    assert by_id["soulseek"]["loaded"] is True
    assert by_id["usenet"]["builtin"] is True

    ext = by_id["route_plugin"]
    assert ext["builtin"] is False
    assert ext["loaded"] is True
    assert ext["version"] == "1.1.0"
    assert ext["enabled"] is True

    broken = next(p for p in body["plugins"] if "kaput" in (p["error"] or ""))
    assert broken["loaded"] is False
    assert broken["enabled"] is False


def test_enable_disable_roundtrip(tmp_path):
    client, manager = _admin_client(tmp_path)
    resp = client.post("/plugins/route_plugin/disable")
    assert resp.status_code == 200
    assert resp.json() == {"id": "route_plugin", "enabled": False}
    assert manager.is_enabled("route_plugin") is False
    resp = client.post("/plugins/route_plugin/enable")
    assert resp.json()["enabled"] is True


def test_enable_unknown_404_and_broken_409(tmp_path):
    client, _ = _admin_client(tmp_path)
    assert client.post("/plugins/nope/enable").status_code == 404
    assert client.post("/plugins/broken/enable").status_code == 409


def test_settings_get_put_masks_secret(tmp_path):
    client, manager = _admin_client(tmp_path)

    resp = client.get("/plugins/route_plugin/settings")
    assert resp.status_code == 200
    body = resp.json()
    keys = [f["key"] for f in body["schema"]]
    assert keys == ["endpoint", "token"]
    assert body["values"]["token"] == ""

    resp = client.put(
        "/plugins/route_plugin/settings",
        json={"values": {"endpoint": "http://z", "token": "hush"}},
    )
    assert resp.status_code == 200
    assert resp.json()["values"]["token"] == PLUGIN_SECRET_MASK
    assert resp.json()["values"]["endpoint"] == "http://z"
    assert manager.get_plugin("route_plugin").applied["token"] == "hush"

    # PUTting the mask back preserves the real secret
    resp = client.put(
        "/plugins/route_plugin/settings",
        json={"values": {"endpoint": "http://z2", "token": PLUGIN_SECRET_MASK}},
    )
    assert resp.status_code == 200
    assert manager.get_plugin("route_plugin").applied["token"] == "hush"
    assert manager.get_plugin("route_plugin").applied["endpoint"] == "http://z2"


def test_settings_non_admin_forbidden(tmp_path):
    app = _app(_manager(tmp_path))
    app.dependency_overrides[_get_current_admin] = _deny_admin
    client = build_test_client(app)
    assert client.get("/plugins/route_plugin/settings").status_code == 403
    assert client.put("/plugins/route_plugin/settings", json={"values": {}}).status_code == 403
    assert client.post("/plugins/route_plugin/test", json={}).status_code == 403
    assert client.post("/plugins/route_plugin/enable").status_code == 403


def test_test_endpoint_uses_submitted_values(tmp_path):
    client, _ = _admin_client(tmp_path)
    resp = client.post("/plugins/route_plugin/test", json={"values": {"endpoint": ""}})
    assert resp.status_code == 200
    # empty submitted endpoint -> falls back to stored (also empty) -> invalid
    assert resp.json()["valid"] is False

    resp = client.post("/plugins/route_plugin/test", json={"values": {"endpoint": "http://up"}})
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is True
    assert body["version"] == "9.9"


def test_test_endpoint_unknown_plugin_404(tmp_path):
    client, _ = _admin_client(tmp_path)
    assert client.post("/plugins/nope/test", json={}).status_code == 404
