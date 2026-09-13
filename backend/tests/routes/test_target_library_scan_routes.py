from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import FastAPI, HTTPException

from api.v1.routes.library_scan_target import router
from api.v1.schemas.library_policies import (
    LibraryPathPolicyRule,
    LibraryRootSettings,
    TypedLibrarySettings,
)
from core.dependencies import (
    get_library_administrative_work_service,
    get_library_policy_resolver,
    get_mb_provider_availability,
    get_target_identification_queue,
    get_target_library_policy_reconciliation_service,
    get_target_library_scan_coordinator,
)
from core.exceptions import ResourceNotFoundError, StaleRevisionError, ValidationError
from middleware import _get_current_admin
from models.library_work import (
    LibraryWorkItem,
    ScanControlResult,
    ScanFailureRecord,
    ScanRequestResult,
    ScanScope,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from tests.helpers import build_test_client, override_admin_auth, override_user_auth


@pytest.fixture
def resolver(tmp_path) -> LibraryPolicyResolver:
    root = tmp_path / "music"
    root.mkdir()
    return LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-a",
                    path=str(root),
                    label="Library",
                    rules=[
                        LibraryPathPolicyRule(
                            id="parent", relative_path="Artist", policy="automatic"
                        ),
                        LibraryPathPolicyRule(
                            id="child",
                            relative_path="Artist/Album",
                            policy="automatic",
                        ),
                    ],
                )
            ]
        )
    )


@pytest.fixture
def coordinator() -> AsyncMock:
    service = AsyncMock()
    service.current.return_value = []
    service.history.return_value = []
    service.history_page.return_value = ([], None)
    service.scan_run_failures.return_value = ([], None)
    service.estimate.return_value = (12, 10.0)
    service.request_run.return_value = ScanRequestResult(
        run_id="run-1",
        disposition="started",
        state="queued",
        row_revision=1,
    )
    service.control.return_value = ScanControlResult(
        run_id="run-1",
        state="pausing",
        row_revision=2,
        event_revision=1,
        stream_revision=3,
    )
    return service


@pytest.fixture
def identification_queue() -> AsyncMock:
    service = AsyncMock()
    service.activity_snapshot.return_value = {
        "control_state": "running",
        "control_revision": 1,
        "counts": {},
        "started_at": None,
        "updated_at": None,
        "deferred_count": 0,
        "claimable_count": 0,
        "deferred_reason_counts": {},
        "deferred_jobs": [
            {
                "job_id": "job-d1",
                "local_album_id": "a1",
                "album_title": "Stuck Album",
                "artist_name": "Stuck Artist",
                "last_failure_code": "UNEXPECTED_ERROR",
                "attempt_count": 3,
                "not_before": 100.0,
                "updated_at": 90.0,
            }
        ],
        "attention_count": 0,
        "failure_event_id": None,
        "failure_at": None,
        "foreground_operation_count": 0,
    }
    service.stream_revisions.return_value = {
        "scan": 0,
        "identification": 0,
        "operation": 0,
    }
    service.pause.return_value = 2
    service.resume.return_value = 3
    return service


@pytest.fixture
def administrative_work() -> AsyncMock:
    service = AsyncMock()
    service.active.return_value = []
    return service


@pytest.fixture
def reconciliation() -> AsyncMock:
    service = AsyncMock()
    service.apply.return_value = ScanRequestResult(
        run_id="policy-run-1",
        disposition="started",
        state="queued",
        row_revision=1,
    )
    return service


@pytest.fixture
def mb_availability() -> MagicMock:
    return MagicMock(return_value=True)


@pytest.fixture
def app(
    coordinator: AsyncMock,
    identification_queue: AsyncMock,
    administrative_work: AsyncMock,
    mb_availability: MagicMock,
    resolver: LibraryPolicyResolver,
    reconciliation: AsyncMock,
) -> FastAPI:
    application = FastAPI()
    application.include_router(router)
    application.dependency_overrides[get_target_library_scan_coordinator] = (
        lambda: coordinator
    )
    application.dependency_overrides[get_library_policy_resolver] = lambda: resolver
    application.dependency_overrides[get_target_identification_queue] = (
        lambda: identification_queue
    )
    application.dependency_overrides[get_library_administrative_work_service] = (
        lambda: administrative_work
    )
    application.dependency_overrides[
        get_target_library_policy_reconciliation_service
    ] = lambda: reconciliation
    application.dependency_overrides[get_mb_provider_availability] = (
        lambda: mb_availability
    )
    return application


@pytest.fixture
def admin_client(app: FastAPI):
    override_admin_auth(app)
    override_user_auth(app, role="admin")
    return build_test_client(app)


def test_start_accepts_scope_ids_and_never_accepts_a_path(
    admin_client, coordinator: AsyncMock, resolver: LibraryPolicyResolver
) -> None:
    response = admin_client.post(
        "/library/scan-runs",
        json={
            "kind": "incremental",
            "scope_ids": ["root-a"],
            "expected_policy_revision": resolver.policy_revision,
        },
    )
    assert response.status_code == 202
    assert response.json()["disposition"] == "started"
    request = coordinator.request_run.await_args.args[0]
    assert request.scopes[0].root_id == "root-a"
    assert request.scopes[0].relative_path == "."

    rejected = admin_client.post(
        "/library/scan-runs",
        json={
            "kind": "incremental",
            "scope_ids": ["/untrusted/path"],
            "expected_policy_revision": resolver.policy_revision,
        },
    )
    assert rejected.status_code == 400


def test_start_collapses_nested_selected_scopes_to_one_walk(
    admin_client, coordinator: AsyncMock, resolver: LibraryPolicyResolver
) -> None:
    response = admin_client.post(
        "/library/scan-runs",
        json={
            "kind": "incremental",
            "scope_ids": ["parent", "child"],
            "expected_policy_revision": resolver.policy_revision,
        },
    )

    assert response.status_code == 202
    request = coordinator.request_run.await_args.args[0]
    assert [(scope.scope_id, scope.relative_path) for scope in request.scopes] == [
        ("parent", "Artist")
    ]


def test_mutations_are_admin_only(
    app: FastAPI, resolver: LibraryPolicyResolver
) -> None:
    def reject_admin():
        raise HTTPException(status_code=403, detail="Admin access required")

    app.dependency_overrides[_get_current_admin] = reject_admin
    override_user_auth(app, role="user")
    client = build_test_client(app)
    body = {
        "kind": "incremental",
        "scope_ids": ["root-a"],
        "expected_policy_revision": resolver.policy_revision,
    }
    assert client.post("/library/scan-runs", json=body).status_code == 403
    assert (
        client.post(
            "/library/scan-runs/run-1/pause", json={"expected_revision": 1}
        ).status_code
        == 403
    )

    unauthenticated = FastAPI()
    unauthenticated.include_router(router)
    unauthenticated.dependency_overrides[get_target_library_scan_coordinator] = (
        lambda: coordinator
    )
    unauthenticated.dependency_overrides[get_library_policy_resolver] = lambda: resolver
    client = build_test_client(unauthenticated)
    assert client.post("/library/scan-runs", json=body).status_code == 401
    assert (
        client.post(
            "/library/scan-runs/run-1/stop", json={"expected_revision": 1}
        ).status_code
        == 401
    )


def test_activity_is_authenticated_and_redacted(
    app: FastAPI,
    coordinator: AsyncMock,
    identification_queue: AsyncMock,
    administrative_work: AsyncMock,
) -> None:
    unauthenticated = build_test_client(app)
    assert unauthenticated.get("/library/activity").status_code == 401

    coordinator.current.return_value = [
        SimpleNamespace(
            id="run-1",
            state="indexing",
            requested_by_user_id="admin-secret",
            aggregate_scope="private/path",
            updated_at=10,
            started_at=1,
        )
    ]
    coordinator.snapshot.return_value = SimpleNamespace(
        counters={"inspected_count": 4, "discovered_count": 10}
    )
    override_user_auth(app, role="user")
    response = build_test_client(app).get("/library/activity")
    assert response.status_code == 200
    payload = response.json()
    assert payload["revisions"] == {
        "scan": 0,
        "identification": 0,
        "operation": 0,
    }
    assert payload["items"][0]["label"] == "Updating the local library"
    assert payload["items"][0]["processed"] == 4
    assert payload["work_items"][0]["processed"] == 4
    assert payload["work_items"][0]["scope_label"] is None
    administrative_work.active.assert_not_awaited()
    encoded = response.text
    assert "admin-secret" not in encoded
    assert "private/path" not in encoded

    coordinator.current.return_value[0].state = "discovering"
    coordinator.snapshot.return_value = SimpleNamespace(
        counters={"inspected_count": 0, "discovered_count": 8}
    )
    discovering = build_test_client(app).get("/library/activity").json()["items"][0]
    assert discovering["processed"] == 8
    assert discovering["total"] is None
    assert discovering["indeterminate"] is True

    identification_queue.activity_snapshot.return_value = {
        "control_state": "paused",
        "control_revision": 8,
        "counts": {"queued": 6, "running": 1, "succeeded": 3, "needs_review": 2},
        "started_at": 2.0,
        "updated_at": 11.0,
        "deferred_count": 1,
        "deferred_reason_counts": {"PROVIDER_TEMPORARILY_UNAVAILABLE": 1},
        "deferred_jobs": [
            {
                "job_id": "job-d2",
                "local_album_id": "a2",
                "album_title": "Paused Album",
                "artist_name": "Paused Artist",
                "last_failure_code": "UNEXPECTED_ERROR",
                "attempt_count": 2,
                "not_before": 12.0,
                "updated_at": 11.0,
            }
        ],
        "attention_count": 2,
        "kept_local_count": 4,
        "active_priority": 30,
        "failure_event_id": "failure-opaque",
        "failure_at": 9.0,
        "foreground_operation_count": 1,
    }
    payload = build_test_client(app).get("/library/activity").json()
    item = next(item for item in payload["items"] if item["kind"] == "identification")
    assert item == {
        "kind": "identification",
        "state": "pausing",
        "label": "Identifying albums",
        "processed": 5,
        "total": 12,
        "indeterminate": False,
        "updated_at": 11.0,
        "started_at": 2.0,
        "waiting_count": 7,
        "identified_count": 3,
        "kept_local_count": 4,
        "needs_review_count": 2,
        "failed_count": 0,
        "deferred_count": 1,
        "deferred_reason_counts": {"PROVIDER_TEMPORARILY_UNAVAILABLE": 1},
        "deferred_jobs": [
            {
                "job_id": "job-d2",
                "local_album_id": "a2",
                "album_title": "Paused Album",
                "artist_name": "Paused Artist",
                "last_failure_code": "UNEXPECTED_ERROR",
                "attempt_count": 2,
                "not_before": 12.0,
                "updated_at": 11.0,
            }
        ],
        "attention_count": 2,
        "priority_band": "Administrator retries",
        "oldest_backlog_at": 2.0,
        # Regression: a live breaker read, not bool(deferred_count) - deferrals
        # with a healthy provider must not raise a false provider alert.
        "provider_unavailable": False,
        "control_revision": 8,
        "failure_event_id": "failure-opaque",
        "failure_at": 9.0,
        "foreground_operation_count": 1,
    }
    work = next(
        item
        for item in payload["work_items"]
        if item["kind"] == "identification" and item["state"] != "failed"
    )
    assert work["processed"] == 0
    assert work["total"] is None
    assert work["remaining_count"] == 7
    failure = next(
        item
        for item in payload["work_items"]
        if item["kind"] == "identification" and item["state"] == "failed"
    )
    assert failure["failure_event_id"] == "failure-opaque"


def test_activity_marks_provider_unavailable_from_live_breaker_read(
    app: FastAPI,
    identification_queue: AsyncMock,
    mb_availability: MagicMock,
) -> None:
    identification_queue.activity_snapshot.return_value = {
        "control_state": "running",
        "control_revision": 1,
        "counts": {"queued": 2},
        "started_at": 2.0,
        "updated_at": 11.0,
        "deferred_count": 2,
        "claimable_count": 2,
        "deferred_reason_counts": {"PROVIDER_TEMPORARILY_UNAVAILABLE": 2},
        "attention_count": 0,
        "kept_local_count": 0,
        "active_priority": None,
        "failure_event_id": None,
        "failure_at": None,
        "foreground_operation_count": 0,
    }
    mb_availability.return_value = False
    override_user_auth(app, role="user")

    payload = build_test_client(app).get("/library/activity").json()
    item = next(item for item in payload["items"] if item["kind"] == "identification")
    assert item["provider_unavailable"] is True
    assert item["deferred_count"] == 2
    assert item["deferred_reason_counts"] == {"PROVIDER_TEMPORARILY_UNAVAILABLE": 2}
    assert item["attention_count"] == 0


def test_activity_reports_identification_idle_until_work_is_claimable(
    app: FastAPI, identification_queue: AsyncMock
) -> None:
    # Deferred-not-due jobs are waiting but not claimable: the lane must not
    # pretend the queue is running while nothing can be claimed.
    identification_queue.activity_snapshot.return_value = {
        **identification_queue.activity_snapshot.return_value,
        "control_state": "running",
        "counts": {"queued": 1},
        "claimable_count": 0,
        "attention_count": 1,
        "deferred_count": 1,
        "deferred_reason_counts": {"PROVIDER_TEMPORARILY_UNAVAILABLE": 1},
    }
    override_user_auth(app, role="user")
    items = build_test_client(app).get("/library/activity").json()["items"]
    identification = next(item for item in items if item["kind"] == "identification")
    assert identification["state"] == "idle"
    assert identification["waiting_count"] == 1

    identification_queue.activity_snapshot.return_value = {
        **identification_queue.activity_snapshot.return_value,
        "counts": {"queued": 1},
        "claimable_count": 1,
    }
    items = build_test_client(app).get("/library/activity").json()["items"]
    identification = next(item for item in items if item["kind"] == "identification")
    assert identification["state"] == "running"
    assert identification["attention_count"] == 1


def test_activity_projects_admin_work_and_scan_finalization_truthfully(
    admin_client,
    coordinator: AsyncMock,
    administrative_work: AsyncMock,
) -> None:
    coordinator.current.return_value = [
        SimpleNamespace(
            id="run-1",
            state="reconciling",
            phase="reconciling",
            aggregate_scope="all",
            updated_at=10,
            started_at=1,
        ),
        SimpleNamespace(
            id="run-2",
            state="queued",
            phase="queued",
            aggregate_scope="all",
            updated_at=11,
            started_at=None,
        ),
    ]
    coordinator.snapshot.side_effect = [
        SimpleNamespace(
            counters={"inspected_count": 95, "total_count": 100, "changed_count": 3}
        ),
        SimpleNamespace(counters={}),
    ]
    administrative_work.active.return_value = [
        LibraryWorkItem(
            id="management-1",
            kind="library_management",
            state="running",
            phase="planning",
            effect="catalog_only",
            processed=20,
            total=200,
            unit="files",
            priority=30,
            updated_at=9,
        )
    ]

    payload = admin_client.get("/library/activity").json()

    scans = [item for item in payload["work_items"] if item["kind"] == "scan"]
    scan = next(item for item in scans if item["id"] == "run-1")
    queued_scan = next(item for item in scans if item["id"] == "run-2")
    management = next(
        item for item in payload["work_items"] if item["kind"] == "library_management"
    )
    assert scan["processed"] == 100
    assert scan["total"] == 100
    assert scan["phase"] == "reconciling"
    assert scan["scope_label"] == "Whole library"
    assert queued_scan["state"] == "queued"
    assert queued_scan["priority"] > scan["priority"]
    assert len(payload["items"]) == 1
    assert management["processed"] == 20
    administrative_work.active.assert_awaited_once()


def test_activity_projects_recent_scan_failure_and_foreground_work(
    app: FastAPI, coordinator: AsyncMock, identification_queue: AsyncMock
) -> None:
    import time

    failed = SimpleNamespace(
        id="failed-scan",
        state="failed",
        terminal_at=time.time() - 10,
        updated_at=time.time() - 10,
        started_at=time.time() - 20,
    )
    coordinator.history.return_value = [failed]
    coordinator.snapshot.return_value = SimpleNamespace(
        counters={"inspected_count": 3, "total_count": 5}
    )
    identification_queue.activity_snapshot.return_value = {
        **identification_queue.activity_snapshot.return_value,
        "foreground_operation_count": 1,
    }
    override_user_auth(app, role="user")
    items = build_test_client(app).get("/library/activity").json()["items"]
    scan = next(item for item in items if item["kind"] == "scan")
    identification = next(item for item in items if item["kind"] == "identification")
    assert scan["failure_event_id"] == "failed-scan"
    assert scan["processed"] == 3
    assert identification["foreground_operation_count"] == 1
    assert identification["state"] == "idle"
    assert identification["waiting_count"] == 0


def test_identification_controls_are_revisioned_and_admin_only(
    app: FastAPI, identification_queue: AsyncMock
) -> None:
    paused = dict(identification_queue.activity_snapshot.return_value)
    paused["control_state"] = "paused"
    running = dict(identification_queue.activity_snapshot.return_value)
    running["control_state"] = "running"
    identification_queue.activity_snapshot.side_effect = [paused, running]
    override_admin_auth(app)
    override_user_auth(app, role="admin")
    client = build_test_client(app)
    response = client.post(
        "/library/identification/pause", json={"expected_revision": 1}
    )
    assert response.status_code == 200
    assert response.json() == {"state": "paused", "row_revision": 2}
    identification_queue.pause.assert_awaited_once_with(
        "test-admin-id", expected_revision=1
    )

    response = client.post(
        "/library/identification/resume", json={"expected_revision": 2}
    )
    assert response.status_code == 200
    assert response.json() == {"state": "running", "row_revision": 3}
    identification_queue.resume.assert_awaited_once_with(expected_revision=2)

    def reject_admin():
        raise HTTPException(status_code=403, detail="Admin access required")

    app.dependency_overrides[_get_current_admin] = reject_admin
    assert (
        build_test_client(app)
        .post("/library/identification/pause", json={"expected_revision": 1})
        .status_code
        == 403
    )


def test_control_and_history_contracts(admin_client, coordinator: AsyncMock) -> None:
    response = admin_client.post(
        "/library/scan-runs/run-1/pause", json={"expected_revision": 1}
    )
    assert response.status_code == 200
    assert response.json() == {
        "run_id": "run-1",
        "state": "pausing",
        "row_revision": 2,
        "event_revision": 1,
        "stream_revision": 3,
    }
    assert admin_client.get("/library/scan-runs").json() == {
        "items": [],
        "next_cursor": None,
    }
    assert admin_client.get("/library/scan-runs/estimate").json() == {
        "approximate": True,
        "estimated_file_count": 12,
        "estimated_at": 10.0,
    }


def test_missing_and_stale_run_use_typed_error_envelopes(
    admin_client, coordinator: AsyncMock
) -> None:
    coordinator.snapshot.side_effect = ResourceNotFoundError("missing")
    response = admin_client.get("/library/scan-runs/missing")
    assert response.status_code == 404
    assert response.json()["error"]["code"] == "NOT_FOUND"

    coordinator.control.side_effect = StaleRevisionError("stale")
    response = admin_client.post(
        "/library/scan-runs/run-1/stop", json={"expected_revision": 1}
    )
    assert response.status_code == 409
    assert response.json()["error"]["code"] == "STALE_REVISION"


def test_scan_runs_start_rejects_a_disabled_library_with_the_switch_message(
    admin_client, coordinator: AsyncMock, resolver: LibraryPolicyResolver
) -> None:
    coordinator.request_run.side_effect = ValidationError(
        "The local library is disabled. Enable it in Settings → Library "
        "before starting a scan."
    )
    response = admin_client.post(
        "/library/scan-runs",
        json={
            "kind": "incremental",
            "scope_ids": ["root-a"],
            "expected_policy_revision": resolver.policy_revision,
        },
    )

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "VALIDATION_ERROR"
    assert "disabled" in response.json()["error"]["message"]


def test_scan_run_failures_returns_persisted_detail_for_indexing_rows(
    admin_client, coordinator: AsyncMock
) -> None:
    """NEW-SCAN-04: the failures endpoint serializes the safe detail recorded by
    the indexer (timeout deadline, capacity, exception class) for admin eyes."""
    coordinator.scan_run_failures.return_value = (
        [
            ScanFailureRecord(
                root_id="root-a",
                relative_path="hashed/away.flac",
                failure_code="TAG_READ_TIMEOUT",
                recorded_at=12.5,
                failure_detail=(
                    "The tag read exceeded its 30.0s deadline. A kernel-blocked "
                    "read is bounded by the timeout but the underlying syscall "
                    "may still be running."
                ),
                phase="indexing",
            )
        ],
        None,
    )

    resp = admin_client.get("/library/scan-runs/run-1/failures")

    assert resp.status_code == 200
    item = resp.json()["items"][0]
    assert item["failure_code"] == "TAG_READ_TIMEOUT"
    assert "30.0s deadline" in item["failure_detail"]
    assert item["phase"] == "indexing"


def test_activity_reports_healthy_breaker_with_unmappable_reason(
    admin_client,
    coordinator: AsyncMock,
    identification_queue: AsyncMock,
    administrative_work: AsyncMock,
) -> None:
    """F-IDENT-02: a healthy MusicBrainz breaker keeps provider_unavailable
    false while deferred reason counts carry the deterministic unmappable code."""
    from types import SimpleNamespace as _NS

    coordinator.current.return_value = []
    coordinator.history.return_value = []
    identification_queue.stream_revisions.return_value = {}
    identification_queue.activity_snapshot.return_value = {
        "control_state": "running",
        "control_revision": 1,
        "counts": {"queued": 1},
        "started_at": 1.0,
        "updated_at": 2.0,
        "deferred_count": 1,
        "deferred_reason_counts": {"UNMAPPABLE_PROVIDER_PAYLOAD": 1},
        "claimable_count": 0,
        "attention_count": 0,
        "kept_local_count": 0,
        "active_priority": 20,
        "failure_event_id": None,
        "failure_at": None,
        "foreground_operation_count": 0,
    }
    administrative_work.active.return_value = []

    payload = admin_client.get("/library/activity").json()
    item = next(item for item in payload["items"] if item["kind"] == "identification")
    assert item["provider_unavailable"] is False
    assert item["deferred_count"] == 1
    assert item["deferred_reason_counts"] == {"UNMAPPABLE_PROVIDER_PAYLOAD": 1}


def test_scan_run_failures_returns_snake_case_items(
    admin_client, coordinator: AsyncMock
) -> None:
    coordinator.scan_run_failures.return_value = (
        [
            ScanFailureRecord(
                root_id="root-a",
                relative_path="Artist/Album",
                failure_code="WALK_EACCES",
                recorded_at=1_800_000_001.0,
                failure_detail="[Errno 13] Permission denied",
                phase="discovering",
            )
        ],
        41,
    )

    response = admin_client.get("/library/scan-runs/run-1/failures?cursor=40")

    assert response.status_code == 200
    assert response.json() == {
        "items": [
            {
                "root_id": "root-a",
                "relative_path": "Artist/Album",
                "failure_code": "WALK_EACCES",
                "failure_detail": "[Errno 13] Permission denied",
                "phase": "discovering",
                "recorded_at": 1_800_000_001.0,
            }
        ],
        "next_cursor": 41,
    }
    coordinator.scan_run_failures.assert_awaited_once_with(
        "run-1", limit=50, cursor_rowid=40
    )


def test_scan_run_failures_limit_bounds_are_validated(admin_client) -> None:
    assert admin_client.get("/library/scan-runs/run-1/failures?limit=0").status_code == 422
    assert (
        admin_client.get("/library/scan-runs/run-1/failures?limit=201").status_code
        == 422
    )


def test_scan_run_failures_auth_matrix(
    app: FastAPI, coordinator: AsyncMock
) -> None:
    assert (
        build_test_client(app).get("/library/scan-runs/run-1/failures").status_code
        == 401
    )

    def reject_admin():
        raise HTTPException(status_code=403, detail="Admin access required")

    app.dependency_overrides[_get_current_admin] = reject_admin
    assert (
        build_test_client(app).get("/library/scan-runs/run-1/failures").status_code
        == 403
    )

    override_admin_auth(app)
    override_user_auth(app, role="admin")
    coordinator.scan_run_failures.side_effect = ResourceNotFoundError(
        "Scan run not found: missing"
    )
    missing = build_test_client(app).get("/library/scan-runs/missing/failures")
    assert missing.status_code == 404
    assert missing.json()["error"]["code"] == "NOT_FOUND"


def test_target_route_security_inventory_is_complete() -> None:
    """F-NL-03: the target router exposes exactly the revisioned scan surface;
    every legacy /library/scan/* path is an absence check."""
    paths = {
        route.path
        for route in router.routes
        if getattr(route, "methods", set()) & {"GET", "POST"}
    }
    assert paths == {
        "/library/activity",
        "/library/identification/pause",
        "/library/identification/resume",
        "/library/scan-runs",
        "/library/scan-runs/current",
        "/library/scan-runs/estimate",
        "/library/scan-runs/{run_id}",
        "/library/scan-runs/{run_id}/failures",
        "/library/scan-runs/{run_id}/pause",
        "/library/scan-runs/{run_id}/resume",
        "/library/scan-runs/{run_id}/stop",
    }
    for legacy in ("/library/scan/start", "/library/scan/cancel", "/library/scan/status"):
        assert legacy not in paths


def test_policy_reconcile_apply_uses_frozen_pending_scopes(
    admin_client,
    coordinator: AsyncMock,
    reconciliation: AsyncMock,
    resolver: LibraryPolicyResolver,
) -> None:
    """F-TARGETCATALOG-02: a policy Apply must route through the
    reconciliation service (frozen pending scopes, trigger=policy_apply)
    instead of rebuilding current-settings scopes with trigger=manual."""
    frozen_scope = ScanScope(
        root_id="root-a",
        scope_id="removed-root",
        relative_path="Old Root",
        effective_policy="excluded",
        policy_revision="pending-rev",
    )
    reconciliation.preview_apply.return_value = {
        "policy_revision": "pending-rev",
        "scope_ids": ["removed-root"],
        "estimated_file_count": 3,
        "scopes": [frozen_scope],
    }

    response = admin_client.post(
        "/library/scan-runs",
        json={
            "kind": "policy_reconcile",
            "scope_ids": ["removed-root"],
            "expected_policy_revision": "pending-rev",
        },
    )

    assert response.status_code == 202
    body = response.json()
    assert body["run_id"] == "policy-run-1"
    assert body["disposition"] == "started"
    # The general scan path must not be used for a policy Apply.
    coordinator.request_run.assert_not_awaited()
    reconciliation.apply.assert_awaited_once_with(
        ["removed-root"],
        expected_policy_revision="pending-rev",
        requested_by_user_id="test-admin-id",
    )


def test_manual_scans_still_use_current_scopes_and_manual_trigger(
    admin_client,
    coordinator: AsyncMock,
    reconciliation: AsyncMock,
    resolver: LibraryPolicyResolver,
) -> None:
    """F-TARGETCATALOG-02 non-goal guard: ordinary manual scans keep the
    current-scope selection and trigger=manual."""
    response = admin_client.post(
        "/library/scan-runs",
        json={
            "kind": "incremental",
            "scope_ids": ["parent"],
            "expected_policy_revision": resolver.policy_revision,
        },
    )

    assert response.status_code == 202
    reconciliation.apply.assert_not_awaited()
    request = coordinator.request_run.await_args.args[0]
    assert request.trigger == "manual"
    assert request.kind == "incremental"


def test_stale_policy_apply_returns_error_without_requesting_a_run(
    admin_client,
    coordinator: AsyncMock,
    reconciliation: AsyncMock,
) -> None:
    """F-TARGETCATALOG-02: a stale expected revision fails through the
    service's existing gate and never enqueues a run."""
    from core.exceptions import StaleRevisionError

    reconciliation.apply.side_effect = StaleRevisionError(
        "The library policy changed. Refresh this page and try again."
    )

    response = admin_client.post(
        "/library/scan-runs",
        json={
            "kind": "policy_reconcile",
            "scope_ids": ["removed-root"],
            "expected_policy_revision": "stale-rev",
        },
    )

    assert response.status_code == 409
    assert response.json()["error"]["code"] == "STALE_REVISION"
    coordinator.request_run.assert_not_awaited()


def test_same_id_repath_apply_carries_both_frozen_paths(
    admin_client,
    coordinator: AsyncMock,
    reconciliation: AsyncMock,
) -> None:
    """F-TARGETCATALOG-02: a same-ID re-path's frozen pending scopes include
    both the old and new relative paths; Apply must pass them through so one
    policy run reconciles both. The service (not the route) owns selection."""
    old_scope = ScanScope(
        root_id="root-a",
        scope_id="rule-1",
        relative_path="Artist/Old",
        effective_policy="excluded",
        policy_revision="pending-rev",
    )
    new_scope = ScanScope(
        root_id="root-a",
        scope_id="rule-1",
        relative_path="Artist/New",
        effective_policy="automatic",
        policy_revision="pending-rev",
    )
    reconciliation.preview_apply.return_value = {
        "policy_revision": "pending-rev",
        "scope_ids": ["rule-1"],
        "estimated_file_count": 2,
        "scopes": [old_scope, new_scope],
    }
    reconciliation.apply.return_value = ScanRequestResult(
        run_id="repath-run-1",
        disposition="started",
        state="queued",
        row_revision=1,
    )

    response = admin_client.post(
        "/library/scan-runs",
        json={
            "kind": "policy_reconcile",
            "scope_ids": ["rule-1"],
            "expected_policy_revision": "pending-rev",
        },
    )

    assert response.status_code == 202
    assert response.json()["run_id"] == "repath-run-1"
    # The service received the shared ID once and is responsible for carrying
    # every matching frozen scope into the run.
    reconciliation.apply.assert_awaited_once_with(
        ["rule-1"],
        expected_policy_revision="pending-rev",
        requested_by_user_id="test-admin-id",
    )
