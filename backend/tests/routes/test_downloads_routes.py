"""Download-task route tests (Phase 7): list/get/files/cancel/retry with user scope
and domain-exception mapping (403/404/400)."""

from unittest.mock import AsyncMock

from fastapi import FastAPI
import msgspec

from api.v1.routes import downloads
from core.dependencies import get_download_service
from core.exceptions import (
    ConflictError,
    AutomaticManagementHoldError,
    ConfigurationError,
    PermissionDeniedError,
    ResourceNotFoundError,
    ValidationError,
)
from middleware import _get_current_admin, _get_current_user
from models.download import DownloadActivitySummary, DownloadTask
from repositories.protocols.download_client import DownloadSearchResult
from tests.helpers import build_test_client, mock_user


def _task(task_id: str = "t1", user_id: str = "u1", **overrides) -> DownloadTask:
    return DownloadTask(id=task_id, user_id=user_id, **overrides)


def _app(service) -> FastAPI:
    # The queue route asks the (sync) service for auto-retry hints; give the AsyncMock
    # real return values so responses serialise (a bare AsyncMock hands back coroutines).
    service.next_retry_at = lambda task: None
    service.auto_retry_max = 0
    service.retry_ladder_minutes = lambda: []
    service.cleanup_states.return_value = {}
    service.held_task_ids.return_value = set()
    app = FastAPI()
    app.include_router(downloads.router)
    app.dependency_overrides[get_download_service] = lambda: service
    app.dependency_overrides[_get_current_user] = lambda: mock_user(
        role="user", user_id="u1"
    )
    app.dependency_overrides[_get_current_admin] = lambda: mock_user(
        role="admin", user_id="admin-1"
    )
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
    app.include_router(quarantine.router)  # production order: literal /quarantine first
    app.include_router(downloads.router)  # catch-all /{task_id} second
    app.dependency_overrides[get_download_service] = lambda: service
    app.dependency_overrides[get_download_store] = lambda: store
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    app.dependency_overrides[_get_current_user] = lambda: mock_user(
        role="admin", user_id="u1"
    )

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
    assert body["items"][0]["release_mbid"] is None
    assert body["items"][0]["release_track_mbid"] is None
    assert body["page"] == 1


def test_activity_summary_is_compact_and_user_scoped():
    service = AsyncMock()
    service.get_activity_summary.return_value = DownloadActivitySummary(
        revision=7,
        active_count=2,
        held_count=1,
        failed_count=3,
        landed_release_group_mbids=["rg-1"],
    )

    response = build_test_client(_app(service)).get("/downloads/activity-summary")

    assert response.status_code == 200
    assert response.json() == {
        "revision": 7,
        "active_count": 2,
        "held_count": 1,
        "failed_count": 3,
        "landed_release_group_mbids": ["rg-1"],
    }
    service.get_activity_summary.assert_awaited_once_with("u1", "user")


def test_list_downloads_marks_tasks_with_secured_files_for_review():
    service = AsyncMock()
    service.list_tasks.return_value = [_task("held"), _task("ordinary")]
    app = _app(service)
    service.held_task_ids.return_value = {"held"}

    response = build_test_client(app).get("/downloads")

    assert response.status_code == 200
    assert [item["held_for_review"] for item in response.json()["items"]] == [
        True,
        False,
    ]


def test_list_downloads_batches_cleanup_state_without_exposing_paths():
    service = AsyncMock()
    service.list_tasks.return_value = [_task("t1"), _task("t2")]
    app = _app(service)
    service.cleanup_states.return_value = {"t1": "pending", "t2": "needs_attention"}

    response = build_test_client(app).get("/downloads")

    assert response.status_code == 200
    assert [item["acquisition_cleanup_state"] for item in response.json()["items"]] == [
        "pending",
        "needs_attention",
    ]
    service.cleanup_states.assert_awaited_once_with(["t1", "t2"])
    assert "workspace" not in response.text
    assert "/sab" not in response.text
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


def test_try_next_source_success_for_owner():
    service = AsyncMock()
    service.try_next_source.return_value = _task(
        "t1", status="queued", candidate_index=1
    )

    response = build_test_client(_app(service)).post(
        "/downloads/t1/next-source", json={"expected_candidate_index": 0}
    )

    assert response.status_code == 200
    assert response.json() == {
        "success": True,
        "status": "queued",
        "candidate_index": 1,
    }
    service.try_next_source.assert_awaited_once_with("t1", "u1", "user", 0)


def test_try_next_source_allows_admin_role():
    service = AsyncMock()
    service.try_next_source.return_value = _task(
        "t1", status="queued", candidate_index=1
    )
    app = _app(service)
    app.dependency_overrides[_get_current_user] = lambda: mock_user(
        role="admin", user_id="admin-1"
    )

    response = build_test_client(app).post(
        "/downloads/t1/next-source", json={"expected_candidate_index": 0}
    )

    assert response.status_code == 200
    service.try_next_source.assert_awaited_once_with("t1", "admin-1", "admin", 0)


def test_try_next_source_non_owner_forbidden():
    service = AsyncMock()
    service.try_next_source.side_effect = PermissionDeniedError("not yours")

    response = build_test_client(_app(service)).post(
        "/downloads/t1/next-source", json={"expected_candidate_index": 0}
    )

    assert response.status_code == 403


def test_try_next_source_started_race_is_conflict():
    service = AsyncMock()
    service.try_next_source.side_effect = ConflictError(
        "The transfer has already started"
    )

    response = build_test_client(_app(service)).post(
        "/downloads/t1/next-source", json={"expected_candidate_index": 0}
    )

    assert response.status_code == 409
    assert response.json()["error"]["message"] == "The transfer has already started"


def test_try_next_source_requires_authentication():
    service = AsyncMock()
    app = FastAPI()
    app.include_router(downloads.router)
    app.dependency_overrides[get_download_service] = lambda: service

    response = build_test_client(app).post(
        "/downloads/t1/next-source", json={"expected_candidate_index": 0}
    )

    assert response.status_code == 401


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
    service.retry_task.side_effect = ValidationError(
        "Only failed, cancelled or partial downloads can be retried"
    )
    response = build_test_client(_app(service)).post("/downloads/t1/retry")
    assert response.status_code == 400


def test_retry_not_found_404():
    service = AsyncMock()
    service.retry_task.side_effect = ResourceNotFoundError("Download task not found")
    response = build_test_client(_app(service)).post("/downloads/missing/retry")
    assert response.status_code == 404


def _admin_app(service) -> FastAPI:
    """Like _app but wired for the admin-gated reimport route (CurrentAdminDep)."""
    from middleware import _get_current_admin
    from tests.helpers import mock_admin_user

    app = _app(service)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    return app


def test_reimport_success_reports_completed():
    service = AsyncMock()
    service.reimport_task.return_value = _task(
        "t1", status="completed", files_completed=3, files_failed=0
    )
    response = build_test_client(_admin_app(service)).post("/downloads/t1/reimport")
    assert response.status_code == 200
    body = response.json()
    assert body["success"] is True
    assert body["status"] == "completed"
    assert body["files_imported"] == 3
    service.reimport_task.assert_awaited_once_with("t1")


def test_reimport_partial_is_success():
    service = AsyncMock()
    service.reimport_task.return_value = _task("t1", status="partial", files_failed=1)
    response = build_test_client(_admin_app(service)).post("/downloads/t1/reimport")
    assert response.status_code == 200
    assert response.json()["success"] is True


def test_reimport_not_found_404():
    service = AsyncMock()
    service.reimport_task.side_effect = ResourceNotFoundError("Download task not found")
    response = build_test_client(_admin_app(service)).post(
        "/downloads/missing/reimport"
    )
    assert response.status_code == 404


def test_reimport_wrong_state_400():
    service = AsyncMock()
    service.reimport_task.side_effect = ValidationError(
        "Only failed or partial downloads can be reimported"
    )
    response = build_test_client(_admin_app(service)).post("/downloads/t1/reimport")
    assert response.status_code == 400


def test_response_exposes_completed_at_and_retry_ladder():
    """The queue contract: each task carries its last-attempt timestamp + the full
    backoff ladder (constant across the list, computed once by the route)."""
    service = AsyncMock()
    service.list_tasks.return_value = [_task("t1", completed_at=123.5)]
    app = _app(service)  # _app sets default hint stubs; override the ladder after
    service.retry_ladder_minutes = lambda: [15, 30, 60, 120, 240, 480]
    response = build_test_client(app).get("/downloads")
    assert response.status_code == 200
    item = response.json()["items"][0]
    assert item["completed_at"] == 123.5
    assert item["retry_ladder_minutes"] == [15, 30, 60, 120, 240, 480]


def test_response_completed_at_null_and_ladder_empty_by_default():
    service = AsyncMock()
    service.get_task.return_value = _task("t1")  # completed_at defaults to None
    response = build_test_client(_app(service)).get("/downloads/t1")
    assert response.status_code == 200
    body = response.json()
    assert body["completed_at"] is None
    assert body["retry_ladder_minutes"] == []


def test_response_exposes_durable_quality_queue_and_attempt_details():
    service = AsyncMock()
    service.get_task.return_value = _task(
        "t1",
        quality_format="flac",
        quality_bit_depth=24,
        quality_sample_rate=48_000,
        advertised_queue_depth=2710,
        queue_position_start=91,
        queue_position_end=100,
        remote_queued=True,
        preferred_quality_fallback_at=1234.5,
        attempt_number=1,
        attempt_total=3,
        has_next_source=True,
    )

    response = build_test_client(_app(service)).get("/downloads/t1")

    assert response.status_code == 200
    body = response.json()
    assert body["quality_format"] == "flac"
    assert body["quality_bit_depth"] == 24
    assert body["quality_sample_rate"] == 48_000
    assert body["advertised_queue_depth"] == 2710
    assert [body["queue_position_start"], body["queue_position_end"]] == [91, 100]
    assert body["remote_queued"] is True
    assert body["preferred_quality_fallback_at"] == 1234.5
    assert [body["attempt_number"], body["attempt_total"]] == [1, 3]
    assert body["has_next_source"] is True


def test_clear_downloads_returns_count():
    service = AsyncMock()
    service.clear_finished.return_value = 3
    response = build_test_client(_app(service)).post("/downloads/clear")
    assert response.status_code == 200
    assert response.json() == {"cleared": 3}
    service.clear_finished.assert_awaited_once_with("u1", "user")


def test_clear_downloads_unauthenticated_401():
    service = AsyncMock()
    app = FastAPI()
    app.include_router(downloads.router)
    app.dependency_overrides[get_download_service] = lambda: service
    response = build_test_client(app).post("/downloads/clear")
    assert response.status_code == 401


def test_stop_all_retries_returns_count():
    service = AsyncMock()
    service.stop_all_retries.return_value = 2
    response = build_test_client(_app(service)).post("/downloads/stop-all-retries")
    assert response.status_code == 200
    assert response.json() == {"stopped": 2}
    service.stop_all_retries.assert_awaited_once_with("u1", "user")


def test_retry_all_failed_returns_count():
    service = AsyncMock()
    service.retry_all_failed.return_value = 4
    response = build_test_client(_app(service)).post("/downloads/retry-all-failed")
    assert response.status_code == 200
    assert response.json() == {"retried": 4}
    service.retry_all_failed.assert_awaited_once_with("u1", "user")


def test_get_files_returns_file_list():
    service = AsyncMock()
    files = [
        DownloadSearchResult(
            username="peer",
            filename="A - B/01.flac",
            parent_directory="A - B",
            size=123,
            extension="flac",
            duration=200.0,
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


def _held(held_path: str):
    from models.held_import import HeldImport

    return HeldImport(
        id=1,
        user_id="u1",
        held_path=held_path,
        reason="fingerprint_mismatch",
        source="usenet",
        status="held",
        created_at=0.0,
        track_title="You Shook Me",
    )


def test_list_held_returns_durable_management_retry_schedule():
    service = AsyncMock()
    item = _held("/held/track.flac")
    item = msgspec.structs.replace(
        item,
        reason="management:ROOT_UNAVAILABLE",
        management_retry_count=2,
        management_next_retry_at=1234.0,
    )
    service.list_held.return_value = [item]

    response = build_test_client(_app(service)).get("/downloads/held")

    assert response.status_code == 200
    assert response.json()["items"][0]["management_retry_count"] == 2
    assert response.json()["items"][0]["management_next_retry_at"] == 1234.0


def test_retry_management_hold_returns_album_level_result():
    service = AsyncMock()
    service.retry_management_hold.return_value = ["/music/a.flac", "/music/b.flac"]

    response = build_test_client(_app(service)).post(
        "/downloads/held/management/t1/retry"
    )

    assert response.status_code == 200
    assert response.json() == {"status": "imported", "files": 2}
    service.retry_management_hold.assert_awaited_once_with("t1", "admin-1", "admin")


def test_discard_management_hold_returns_album_level_result():
    service = AsyncMock()
    service.discard_management_hold.return_value = 10

    response = build_test_client(_app(service)).post(
        "/downloads/held/management/t1/discard"
    )

    assert response.status_code == 200
    assert response.json() == {"status": "discarded", "files": 10}
    service.discard_management_hold.assert_awaited_once_with("t1", "admin-1", "admin")


def test_import_held_without_library_root_is_400_configuration_error():
    """Empty roots at 'Import anyway' time surfaces as an actionable 400, not a 500."""
    service = AsyncMock()
    service.import_held.side_effect = ConfigurationError(
        "No library root is configured - restore one in Settings → Library, then try again."
    )

    response = build_test_client(_app(service)).post("/downloads/held/1/import")

    assert response.status_code == 400
    body = response.json()["error"]
    assert body["code"] == "CONFIGURATION_ERROR"
    assert "library root" in body["message"]


def test_automatic_management_hold_returns_safe_conflict_envelope():
    service = AsyncMock()
    dynamic_message = (
        "Provider script /srv/private/music/profile-secret.py rejected "
        "destination /library/hidden-track.flac"
    )
    service.import_held.side_effect = AutomaticManagementHoldError(
        "TRACK_NOT_MAPPED", dynamic_message
    )

    response = build_test_client(_app(service)).post("/downloads/held/1/import")

    assert response.status_code == 409
    assert response.json()["error"] == {
        "code": "AUTOMATIC_MANAGEMENT_HOLD",
        "message": "Import blocked by a Library Management safety check.",
        "details": {"reason_code": "TRACK_NOT_MAPPED"},
    }
    assert dynamic_message not in response.text
    service.import_held.assert_awaited_once_with(1, "u1", "user")


def test_held_audio_streams_file_and_supports_range(tmp_path):
    f = tmp_path / "held.flac"
    f.write_bytes(b"FLACDATA-0123456789")
    service = AsyncMock()
    service.get_held = AsyncMock(return_value=_held(str(f)))
    client = build_test_client(_app(service))

    full = client.get("/downloads/held/1/audio")
    assert full.status_code == 200
    assert full.content == b"FLACDATA-0123456789"
    assert full.headers.get("accept-ranges") == "bytes"

    # a Range request (what the scrubber sends) yields 206 partial content
    part = client.get("/downloads/held/1/audio", headers={"Range": "bytes=0-3"})
    assert part.status_code == 206
    assert part.content == b"FLAC"
    assert "content-range" in {k.lower() for k in part.headers}


def test_held_audio_missing_file_is_404(tmp_path):
    service = AsyncMock()
    service.get_held = AsyncMock(return_value=_held(str(tmp_path / "gone.flac")))
    resp = build_test_client(_app(service)).get("/downloads/held/1/audio")
    assert resp.status_code == 404


def test_held_audio_not_owned_is_404():
    service = AsyncMock()
    service.get_held = AsyncMock(
        return_value=None
    )  # ownership check failed / unknown id
    resp = build_test_client(_app(service)).get("/downloads/held/1/audio")
    assert resp.status_code == 404


# collection-management feature B quality-upgrade routes: admin/trusted curator gate (D18)


def _curator_app(service, *, role: str = "admin") -> FastAPI:
    from middleware import _get_current_curator

    app = _app(service)
    app.dependency_overrides[_get_current_curator] = lambda: mock_user(
        role=role, user_id="u1"
    )
    return app


def test_curator_dep_rejects_plain_user_with_403():
    from types import SimpleNamespace

    from fastapi import HTTPException
    import pytest as _pytest

    from middleware import _get_current_curator

    request = SimpleNamespace(state=SimpleNamespace(user=mock_user(role="user")))
    with _pytest.raises(HTTPException) as exc:
        _get_current_curator(request)
    assert exc.value.status_code == 403

    for role in ("admin", "trusted"):
        request = SimpleNamespace(state=SimpleNamespace(user=mock_user(role=role)))
        assert _get_current_curator(request).role == role


def test_upgrade_routes_require_authentication():
    # no auth state at all -> the curator dependency 401s before the service runs
    service = AsyncMock()
    client = build_test_client(_app(service))
    assert client.get("/downloads/cutoff-unmet").status_code == 401
    assert (
        client.post(
            "/downloads/upgrade/album",
            json={"release_group_mbid": "rg", "artist_name": "A", "album_title": "B"},
        ).status_code
        == 401
    )
    assert (
        client.post(
            "/downloads/upgrade/track",
            json={"recording_mbid": "rec", "artist_name": "A", "track_title": "T"},
        ).status_code
        == 401
    )
    service.list_cutoff_unmet.assert_not_awaited()
    service.request_upgrade_album.assert_not_awaited()


def test_cutoff_unmet_returns_worklist_for_curator():
    service = AsyncMock()
    service.list_cutoff_unmet.return_value = [
        {
            "release_group_mbid": "rg-1",
            "current_tier": "mp3_192",
            "track_count": 10,
            "artist_name": "Radiohead",
            "artist_mbid": "am-1",
            "album_title": "OK Computer",
            "year": 1997,
        }
    ]
    service.quality_cutoff = "lossless"
    service.upgrade_allowed = True

    resp = build_test_client(_curator_app(service, role="trusted")).get(
        "/downloads/cutoff-unmet"
    )

    assert resp.status_code == 200
    body = resp.json()
    assert body["cutoff"] == "lossless"
    assert body["upgrade_allowed"] is True
    assert body["items"][0]["release_group_mbid"] == "rg-1"
    assert body["items"][0]["current_tier"] == "mp3_192"


def test_cutoff_unmet_serializes_normalized_artist_fields_for_target_rows():
    """NEW-TARGET-01: target-shaped store rows reach the wire with the shared
    artist_name/artist_mbid keys populated from the normalized repository
    contract."""
    service = AsyncMock()
    service.list_cutoff_unmet.return_value = [
        {
            "release_group_mbid": "target-rg",
            "current_tier": "mp3_320",
            "track_count": 7,
            # Target-shaped row: legacy keys absent; normalized keys present.
            "album_artist_name": "Target Artist",
            "artist_name": "Target Artist",
            "provider_artist_mbid": "provider-artist-1",
            "artist_mbid": "provider-artist-1",
            "album_title": "Target Album",
            "year": 2024,
        }
    ]
    service.quality_cutoff = "lossless"
    service.upgrade_allowed = True

    resp = build_test_client(_curator_app(service, role="trusted")).get(
        "/downloads/cutoff-unmet"
    )

    assert resp.status_code == 200
    item = resp.json()["items"][0]
    assert item["artist_name"] == "Target Artist"
    assert item["artist_mbid"] == "provider-artist-1"
    assert item["current_tier"] == "mp3_320"


def test_upgrade_album_route_queues_and_maps_sentinel():
    from services.native.download_service import ALREADY_IN_LIBRARY

    service = AsyncMock()
    service.request_upgrade_album.return_value = "task-9"
    client = build_test_client(_curator_app(service))
    body = {"release_group_mbid": "rg-1", "artist_name": "A", "album_title": "B"}

    resp = client.post("/downloads/upgrade/album", json=body)
    assert resp.status_code == 200
    assert resp.json() == {"status": "queued", "task_id": "task-9"}

    service.request_upgrade_album.return_value = ALREADY_IN_LIBRARY
    resp = client.post("/downloads/upgrade/album", json=body)
    assert resp.json()["status"] == "satisfied"


def test_upgrade_track_route_queues():
    service = AsyncMock()
    service.request_upgrade_track.return_value = "task-t"
    resp = build_test_client(_curator_app(service)).post(
        "/downloads/upgrade/track",
        json={"recording_mbid": "rec-1", "artist_name": "A", "track_title": "T"},
    )
    assert resp.status_code == 200
    assert resp.json() == {"status": "queued", "task_id": "task-t"}
    assert service.request_upgrade_track.call_args.kwargs["recording_mbid"] == "rec-1"
