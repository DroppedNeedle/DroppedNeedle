"""Download-task route tests (Phase 7): list/get/files/cancel/retry with user scope
and domain-exception mapping (403/404/400)."""

from unittest.mock import AsyncMock

from fastapi import FastAPI

from api.v1.routes import downloads
from core.dependencies import get_download_service
from core.exceptions import (
    PermissionDeniedError,
    ResourceNotFoundError,
    ValidationError,
)
from middleware import _get_current_user
from models.download import DownloadTask
from repositories.protocols.download_client import DownloadSearchResult
from tests.helpers import build_test_client, mock_user


def _task(task_id: str = "t1", user_id: str = "u1", **overrides) -> DownloadTask:
    return DownloadTask(id=task_id, user_id=user_id, **overrides)


def _app(service) -> FastAPI:
    # The queue route asks the (sync) service for auto-retry hints; give the AsyncMock
    # real return values so responses serialise (a bare AsyncMock hands back coroutines).
    service.next_retry_at = lambda task: None
    service.auto_retry_max = 0
    app = FastAPI()
    app.include_router(downloads.router)
    app.dependency_overrides[get_download_service] = lambda: service
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role="user", user_id="u1")
    return app


def test_quarantine_route_not_shadowed_by_task_catchall():
    """Regression: GET /downloads/quarantine must resolve to the quarantine handler,
    not be captured by the GET /downloads/{task_id} catch-all. Mounts both routers in
    the production registration order (quarantine before downloads)."""
    from api.v1.routes import quarantine
    from core.dependencies import get_download_store
    from middleware import _get_current_admin
    from tests.helpers import mock_admin_user

    service = AsyncMock()
    service.get_task.side_effect = AssertionError(
        "the task catch-all must not capture /downloads/quarantine"
    )
    store = AsyncMock()
    store.list_quarantine.return_value = []

    app = FastAPI()
    app.include_router(quarantine.router)   # production order: literal /quarantine first
    app.include_router(downloads.router)    # catch-all /{task_id} second
    app.dependency_overrides[get_download_service] = lambda: service
    app.dependency_overrides[get_download_store] = lambda: store
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role="admin", user_id="u1")

    response = build_test_client(app).get("/downloads/quarantine")
    assert response.status_code == 200
    assert "items" in response.json()
    store.list_quarantine.assert_awaited_once()


def test_list_downloads_returns_items_for_user():
    service = AsyncMock()
    service.list_tasks.return_value = [_task("t1"), _task("t2")]
    response = build_test_client(_app(service)).get("/downloads")
    assert response.status_code == 200
    body = response.json()
    assert [item["id"] for item in body["items"]] == ["t1", "t2"]
    assert body["page"] == 1
    service.list_tasks.assert_awaited_once_with(
        "u1", "user", status=None, release_group_mbid=None, page=1, page_size=20
    )


def test_list_downloads_passes_status_and_pagination():
    service = AsyncMock()
    service.list_tasks.return_value = []
    build_test_client(_app(service)).get("/downloads?status=failed&page=2&page_size=5")
    service.list_tasks.assert_awaited_once_with(
        "u1", "user", status="failed", release_group_mbid=None, page=2, page_size=5
    )


def test_list_downloads_passes_release_group_filter():
    service = AsyncMock()
    service.list_tasks.return_value = []
    build_test_client(_app(service)).get("/downloads?release_group_mbid=rg-9")
    service.list_tasks.assert_awaited_once_with(
        "u1", "user", status=None, release_group_mbid="rg-9", page=1, page_size=20
    )


def test_get_download_returns_task():
    service = AsyncMock()
    service.get_task.return_value = _task("t1", album_title="OK Computer")
    response = build_test_client(_app(service)).get("/downloads/t1")
    assert response.status_code == 200
    assert response.json()["album_title"] == "OK Computer"


def test_get_download_unauthenticated_401():
    service = AsyncMock()
    app = FastAPI()
    app.include_router(downloads.router)
    app.dependency_overrides[get_download_service] = lambda: service
    response = build_test_client(app).get("/downloads/t1")
    assert response.status_code == 401


def test_get_download_not_found_404():
    service = AsyncMock()
    service.get_task.side_effect = ResourceNotFoundError("Download task not found")
    response = build_test_client(_app(service)).get("/downloads/missing")
    assert response.status_code == 404


def test_cancel_non_owner_forbidden():
    service = AsyncMock()
    service.cancel_task.side_effect = PermissionDeniedError("not yours")
    response = build_test_client(_app(service)).post("/downloads/t1/cancel")
    assert response.status_code == 403


def test_cancel_success():
    service = AsyncMock()
    response = build_test_client(_app(service)).post("/downloads/t1/cancel")
    assert response.status_code == 200
    assert response.json()["success"] is True
    service.cancel_task.assert_awaited_once_with("t1", "u1", "user")


def test_retry_success_returns_new_task_id():
    service = AsyncMock()
    service.retry_task.return_value = "t2"
    response = build_test_client(_app(service)).post("/downloads/t1/retry")
    assert response.status_code == 200
    body = response.json()
    assert body["success"] is True
    assert body["task_id"] == "t2"


def test_retry_non_owner_forbidden():
    service = AsyncMock()
    service.retry_task.side_effect = PermissionDeniedError("not yours")
    response = build_test_client(_app(service)).post("/downloads/t1/retry")
    assert response.status_code == 403


def test_retry_wrong_state_400():
    service = AsyncMock()
    service.retry_task.side_effect = ValidationError("Only failed, cancelled or partial downloads can be retried")
    response = build_test_client(_app(service)).post("/downloads/t1/retry")
    assert response.status_code == 400


def test_retry_not_found_404():
    service = AsyncMock()
    service.retry_task.side_effect = ResourceNotFoundError("Download task not found")
    response = build_test_client(_app(service)).post("/downloads/missing/retry")
    assert response.status_code == 404


def test_get_files_returns_file_list():
    service = AsyncMock()
    files = [
        DownloadSearchResult(
            username="peer", filename="A - B/01.flac", parent_directory="A - B",
            size=123, extension="flac", duration=200.0,
        )
    ]
    service.get_task_files.return_value = (_task("t1", files_total=1), files)
    response = build_test_client(_app(service)).get("/downloads/t1/files")
    assert response.status_code == 200
    body = response.json()
    assert body["task_id"] == "t1"
    assert body["files_total"] == 1
    assert body["files"][0]["filename"] == "A - B/01.flac"
    assert body["files"][0]["size"] == 123
