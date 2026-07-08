"""Phase 1 boot gate: the app boots with brownout stubs, no Lidarr route paths,
and the download-client settings stub is mounted."""

import inspect
import re

from fastapi.testclient import TestClient

import main
from services import local_files_service as local_files_module
from services.native.stubs import LibraryStub

# TestClient without `with` skips lifespan startup (no background tasks needed for these checks).
client = TestClient(main.app)


def test_library_stub_implements_every_method_local_files_service_calls():
    """LocalFilesService is DI-wired to LibraryStub during the brownout; every
    repo method it calls must exist on the stub or the route 500s at runtime.
    This guards against the same gap recurring as more services are wired."""
    source = inspect.getsource(local_files_module)
    called = set(re.findall(r"_library_repo\.([a-z_]+)\(", source))
    missing = sorted(m for m in called if not hasattr(LibraryStub, m))
    assert missing == [], f"LibraryStub is missing methods LocalFilesService calls: {missing}"


def test_app_boots_and_health_ok():
    resp = client.get("/health")
    assert resp.status_code == 200
    assert resp.json()["status"] == "ok"


def test_only_sanctioned_lidarr_routes_mounted():
    """The old Lidarr *management* integration stays deleted (LidarrImport D8). The only
    permitted Lidarr route paths are the read-only migration importer under
    ``/lidarr-import`` - any other ``lidarr`` path would mean the management surface came back."""
    lidarr_paths = [
        path
        for route in main.app.routes
        if "lidarr" in (path := getattr(route, "path", ""))
    ]
    assert lidarr_paths, "expected the lidarr-import routes to be mounted"
    assert all("/lidarr-import" in path for path in lidarr_paths), lidarr_paths


def test_download_client_settings_route_mounted():
    # Phase 6 relocated the download-client config from the P1 brownout stub at
    # /settings/download-client to its canonical home at /download-client/config.
    paths = [getattr(route, "path", "") for route in main.app.routes]
    assert any(path.endswith("/download-client/config") for path in paths)
