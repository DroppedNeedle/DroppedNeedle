"""Drop importer routes: upload validation/staging, listing scope, match and
discard actions, and the curator auth posture the matrix can't cover for
multipart (401 unauth / 403 plain user / trusted ok)."""

import io
import zipfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import APIRouter, FastAPI

from api.v1.routes import import_drop
from core.dependencies import get_drop_import_service
from middleware import _get_current_curator
from models.drop_import import DropImportItem, DropImportJob
from tests.helpers import build_test_client, mock_user


def _job(job_id: str = "job-1", user_id: str = "test-user-id") -> DropImportJob:
    return DropImportJob(
        id=job_id,
        user_id=user_id,
        user_name="Test User",
        status="completed",
        created_at=1.0,
        upload_name="album.zip",
        staging_dir="/secret/staging/path",
        items=[
            DropImportItem(
                id=1,
                job_id=job_id,
                folder_name="Artist - Album",
                status="imported",
                updated_at=2.0,
                release_group_mbid="rg-1",
                album_title="Album",
                artist_name="Artist",
                files_total=2,
                files_imported=2,
                staging_paths=["/secret/staged/file.flac"],
            )
        ],
    )


def _service(tmp_path) -> AsyncMock:
    service = AsyncMock()
    service.incoming_dir = lambda: tmp_path / "incoming"
    service.create_job = AsyncMock(return_value=_job())
    service.list_jobs = AsyncMock(return_value=[_job()])
    service.get_job = AsyncMock(return_value=_job())
    service.match_item = AsyncMock(return_value=_job().items[0])
    service.discard_item = AsyncMock(return_value=_job().items[0])
    return service


def _client(tmp_path, *, role: str | None = "trusted"):
    app = FastAPI()
    v1 = APIRouter(prefix="/api/v1")
    v1.include_router(import_drop.router)
    app.include_router(v1)
    service = _service(tmp_path)
    app.dependency_overrides[get_drop_import_service] = lambda: service
    if role is not None:
        app.dependency_overrides[_get_current_curator] = lambda: mock_user(role=role)
    return build_test_client(app), service


def _zip_bytes() -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as zf:
        zf.writestr("Album/01.flac", b"x")
    return buffer.getvalue()


def test_upload_accepts_zip_and_stages_it(tmp_path):
    client, service = _client(tmp_path)
    payload = _zip_bytes()
    response = client.post(
        "/api/v1/import/uploads",
        files=[("files", ("album.zip", payload, "application/zip"))],
    )
    assert response.status_code == 202
    assert response.json()["id"] == "job-1"
    service.create_job.assert_awaited_once()
    uploads = service.create_job.await_args.kwargs["uploads"]
    assert uploads[0][0] == "album.zip"
    # byte-for-byte: a multipart parser that leaves the spool at EOF would
    # stage an empty file here
    assert Path(uploads[0][1]).read_bytes() == payload


def test_upload_rejects_unsupported_type(tmp_path):
    client, service = _client(tmp_path)
    response = client.post(
        "/api/v1/import/uploads",
        files=[("files", ("virus.exe", b"MZ", "application/octet-stream"))],
    )
    assert response.status_code == 400
    assert "Unsupported file type" in response.json()["error"]["message"]
    service.create_job.assert_not_awaited()


def test_upload_cleans_staged_files_when_create_fails(tmp_path):
    client, service = _client(tmp_path)
    service.create_job = AsyncMock(side_effect=RuntimeError("boom"))
    response = client.post(
        "/api/v1/import/uploads",
        files=[("files", ("album.zip", _zip_bytes(), "application/zip"))],
    )
    assert response.status_code == 500
    incoming = tmp_path / "incoming"
    assert not incoming.exists() or not any(incoming.iterdir())


def test_response_never_leaks_staging_paths(tmp_path):
    client, _ = _client(tmp_path)
    response = client.get("/api/v1/import/jobs")
    assert response.status_code == 200
    body = response.text
    assert "/secret/" not in body
    job = response.json()["jobs"][0]
    assert "staging_dir" not in job
    assert "staging_paths" not in job["items"][0]


def test_list_jobs_scopes_all_to_admin_only(tmp_path):
    client, service = _client(tmp_path, role="trusted")
    client.get("/api/v1/import/jobs?all=true")
    assert service.list_jobs.await_args.kwargs["include_all"] is False

    admin_client, admin_service = _client(tmp_path, role="admin")
    admin_client.get("/api/v1/import/jobs?all=true")
    assert admin_service.list_jobs.await_args.kwargs["include_all"] is True


def test_match_and_discard_forward_to_service(tmp_path):
    client, service = _client(tmp_path)
    response = client.post(
        "/api/v1/import/items/1/match", json={"release_group_mbid": "rg-9"}
    )
    assert response.status_code == 200
    assert service.match_item.await_args.args == (1, "rg-9")

    response = client.post("/api/v1/import/items/1/discard")
    assert response.status_code == 200
    service.discard_item.assert_awaited_once()


def test_unauthenticated_requests_are_rejected(tmp_path):
    client, _ = _client(tmp_path, role=None)
    assert client.get("/api/v1/import/jobs").status_code == 401
    assert (
        client.post(
            "/api/v1/import/uploads",
            files=[("files", ("a.zip", _zip_bytes(), "application/zip"))],
        ).status_code
        == 401
    )


def test_plain_user_is_forbidden(tmp_path):
    """The curator dependency itself rejects the plain-user role with 403."""
    request = SimpleNamespace(state=SimpleNamespace(user=mock_user(role="user")))
    with pytest.raises(Exception) as excinfo:
        _get_current_curator(request)
    assert getattr(excinfo.value, "status_code", None) == 403
    for role in ("trusted", "admin"):
        request = SimpleNamespace(state=SimpleNamespace(user=mock_user(role=role)))
        assert _get_current_curator(request).role == role
