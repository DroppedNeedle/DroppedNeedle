from fastapi import FastAPI
from fastapi.testclient import TestClient

from static_server import FrontendStaticFiles


def test_hashed_frontend_assets_are_immutable(tmp_path):
    immutable = tmp_path / "immutable"
    immutable.mkdir()
    (immutable / "entry.abc123.js").write_text("export {};", encoding="utf-8")
    app = FastAPI()
    app.mount("/_app", FrontendStaticFiles(directory=tmp_path))

    response = TestClient(app).get("/_app/immutable/entry.abc123.js")

    assert response.status_code == 200
    assert response.headers["cache-control"] == "public, max-age=31536000, immutable"


def test_unhashed_frontend_metadata_is_not_marked_immutable(tmp_path):
    (tmp_path / "version.json").write_text('{"version":"1"}', encoding="utf-8")
    app = FastAPI()
    app.mount("/_app", FrontendStaticFiles(directory=tmp_path))

    response = TestClient(app).get("/_app/version.json")

    assert response.status_code == 200
    assert "immutable" not in response.headers.get("cache-control", "")
