from collections.abc import Callable
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI, HTTPException

from api.v1.routes.library_operations_target import router
from api.v1.schemas.library_operations import (
    IdentityPreparationEstimateResponse,
    OperationListResponse,
    OperationResponse,
    RepairFindingListResponse,
    RepairEstimateResponse,
    ReviewDetailResponse,
    ReviewListItem,
    ReviewListResponse,
)
from core.dependencies import (
    get_target_catalog_correction_service,
    get_target_explicit_reidentification_worker,
    get_target_identity_repair_service,
    get_target_library_diagnostics_service,
    get_target_library_operation_service,
    get_target_library_review_service,
    get_target_reidentification_service,
)
from core.exceptions import ResourceNotFoundError, ValidationError
from middleware import _get_current_admin
from tests.helpers import build_test_client, override_admin_auth


@pytest.fixture
def services() -> dict[str, AsyncMock]:
    review = AsyncMock()
    review.list_reviews.return_value = ReviewListResponse(items=[])
    review.detail.return_value = ReviewDetailResponse(
        review=ReviewListItem(
            id="review-1", state="needs_review", reason_code="NO_SAFE_MATCH"
        ),
        tracks=[],
    )
    operation = AsyncMock()
    operation.get.return_value = OperationResponse(
        id="job-1", kind="repair", state="queued"
    )
    operation.control.return_value = operation.get.return_value
    repair = AsyncMock()
    repair.history.return_value = OperationListResponse(items=[])
    repair.get_for_purpose.return_value = operation.get.return_value
    repair.create_management_preparation.return_value = operation.get.return_value
    repair.begin_management_preparation_apply.return_value = operation.get.return_value
    repair.discard_management_preparation.return_value = operation.get.return_value
    repair.findings.return_value = RepairFindingListResponse(items=[])
    repair.estimate.return_value = RepairEstimateResponse(
        identity_count=12, selected_root_count=1, queued_repair_count=2
    )
    repair.estimate_management_preparation.return_value = (
        IdentityPreparationEstimateResponse(
            album_count=20,
            ready_album_count=4,
            mapping_required_count=12,
            exact_release_required_count=4,
            selected_root_count=1,
            queued_preparation_count=0,
        )
    )
    diagnostics = AsyncMock()
    diagnostics.export.return_value = ("droppedneedle-library-run-safe.json", b"{}")
    reidentification = AsyncMock()
    reidentification.create_or_coalesce.return_value = {
        "id": "job-1",
        "kind": "explicit_reidentification",
        "state": "queued",
    }
    return {
        "review": review,
        "operation": operation,
        "correction": AsyncMock(),
        "repair": repair,
        "diagnostics": diagnostics,
        "reidentification": reidentification,
        "explicit_worker": AsyncMock(),
    }


@pytest.fixture
def app(services: dict[str, AsyncMock]) -> FastAPI:
    application = FastAPI()
    application.include_router(router)

    def provide(service: AsyncMock) -> Callable[[], AsyncMock]:
        def dependency_override() -> AsyncMock:
            return service

        return dependency_override

    overrides = {
        get_target_library_review_service: services["review"],
        get_target_library_operation_service: services["operation"],
        get_target_catalog_correction_service: services["correction"],
        get_target_identity_repair_service: services["repair"],
        get_target_library_diagnostics_service: services["diagnostics"],
        get_target_reidentification_service: services["reidentification"],
        get_target_explicit_reidentification_worker: services["explicit_worker"],
    }
    for provider, service in overrides.items():
        application.dependency_overrides[provider] = provide(service)
    return application


def test_review_and_diagnostic_contracts(
    app: FastAPI, services: dict[str, AsyncMock]
) -> None:
    override_admin_auth(app)
    client = build_test_client(app)
    assert client.get("/library/reviews").status_code == 200
    assert client.get("/library/reviews/review-1").status_code == 200
    response = client.get("/library/scan-runs/run-1/diagnostics")
    assert response.status_code == 200
    assert response.headers["content-type"] == "application/json"
    assert response.headers["content-disposition"] == (
        'attachment; filename="droppedneedle-library-run-safe.json"'
    )
    assert response.headers["x-content-type-options"] == "nosniff"
    assert response.headers["cache-control"] == "no-store"
    assert response.content == b"{}"


def test_target_operation_routes_are_admin_only(app: FastAPI) -> None:
    def reject_admin() -> None:
        raise HTTPException(status_code=403, detail="Admin access required")

    app.dependency_overrides[_get_current_admin] = reject_admin
    client = build_test_client(app)
    assert client.get("/library/reviews").status_code == 403
    assert client.get("/library/operations/job-1").status_code == 403
    assert client.get("/library/identity-repairs/job-1/findings").status_code == 403
    assert client.get("/library/scan-runs/run-1/diagnostics").status_code == 403

    unauthenticated = FastAPI()
    unauthenticated.include_router(router)
    client = build_test_client(unauthenticated)
    assert client.get("/library/reviews").status_code == 401
    assert client.get("/library/operations/job-1").status_code == 401


def test_repair_findings_forwards_category_and_pagination(
    app: FastAPI, services: dict[str, AsyncMock]
) -> None:
    override_admin_auth(app)
    response = build_test_client(app).get(
        "/library/identity-repairs/job-1/findings",
        params={
            "limit": 37,
            "cursor": "12.5:finding-2",
            "finding_category": "unverifiable",
        },
    )
    assert response.status_code == 200
    services["repair"].findings.assert_awaited_once_with(
        "job-1",
        limit=37,
        cursor="12.5:finding-2",
        finding_category="unverifiable",
    )


def test_repair_estimate_forwards_selected_roots(
    app: FastAPI, services: dict[str, AsyncMock]
) -> None:
    override_admin_auth(app)
    response = build_test_client(app).get(
        "/library/identity-repairs/estimate",
        params=[("root_id", "root-2"), ("root_id", "root-1")],
    )
    assert response.status_code == 200
    assert response.json()["identity_count"] == 12
    services["repair"].estimate.assert_awaited_once_with(["root-2", "root-1"])


def test_management_identity_preparation_contracts(
    app: FastAPI, services: dict[str, AsyncMock]
) -> None:
    override_admin_auth(app)
    client = build_test_client(app)
    estimate = client.get(
        "/library/management/identity-preparations/estimate",
        params=[("root_id", "root-2"), ("root_id", "root-1")],
    )
    assert estimate.status_code == 200
    assert estimate.json()["mapping_required_count"] == 12
    services["repair"].estimate_management_preparation.assert_awaited_once_with(
        ["root-2", "root-1"]
    )

    started = client.post(
        "/library/management/identity-preparations",
        json={"idempotency_key": "identity-preparation-1", "root_ids": ["root-1"]},
    )
    assert started.status_code == 200
    services["repair"].create_management_preparation.assert_awaited_once()

    assert client.get("/library/management/identity-preparations").status_code == 200
    assert (
        client.get("/library/management/identity-preparations/job-1").status_code == 200
    )
    findings = client.get(
        "/library/management/identity-preparations/job-1/findings",
        params={"finding_category": "mapping_ready"},
    )
    assert findings.status_code == 200
    services["repair"].findings.assert_awaited_with(
        "job-1",
        limit=100,
        cursor=None,
        finding_category="mapping_ready",
    )
    applied = client.post(
        "/library/management/identity-preparations/job-1/apply",
        json={"expected_row_revision": 2, "confirmation": True},
    )
    assert applied.status_code == 200
    services["repair"].begin_management_preparation_apply.assert_awaited_once_with(
        "job-1", expected_row_revision=2, confirmation=True
    )
    discarded = client.post(
        "/library/management/identity-preparations/job-1/discard",
        json={"expected_row_revision": 3},
    )
    assert discarded.status_code == 200
    services["repair"].discard_management_preparation.assert_awaited_once_with(
        "job-1", expected_row_revision=3
    )


def test_route_errors_use_typed_envelopes(
    app: FastAPI, services: dict[str, AsyncMock]
) -> None:
    override_admin_auth(app)
    services["operation"].get.side_effect = ResourceNotFoundError(
        "Library operation not found."
    )
    response = build_test_client(app).get("/library/operations/missing")
    assert response.status_code == 404
    assert response.json()["error"]["code"] == "NOT_FOUND"

    services["diagnostics"].export.side_effect = ValidationError(
        "The scan run ID is invalid."
    )
    response = build_test_client(app).get("/library/scan-runs/bad/diagnostics")
    assert response.status_code == 400
    assert response.json()["error"]["code"] == "VALIDATION_ERROR"


def test_diagnostic_route_uses_fixed_5xx_copy(
    app: FastAPI, services: dict[str, AsyncMock]
) -> None:
    override_admin_auth(app)
    services["diagnostics"].export.side_effect = RuntimeError(
        "provider failed at /secret/music/private.flac"
    )
    response = build_test_client(app).get("/library/scan-runs/run-1/diagnostics")
    assert response.status_code == 500
    assert response.json()["error"]["message"] == "Internal server error"
    assert "/secret/music" not in response.text
    assert "provider failed" not in response.text


def test_target_operation_route_inventory_is_complete() -> None:
    inventory = {
        (method, route.path)
        for route in router.routes
        for method in getattr(route, "methods", set())
        if method in {"GET", "POST"}
    }
    assert inventory == {
        ("GET", "/library/reviews"),
        ("GET", "/library/reviews/{review_id}"),
        ("POST", "/library/reviews/{review_id}/keep-tagged"),
        ("POST", "/library/reviews/{review_id}/detach-and-keep-tagged"),
        ("POST", "/library/reviews/{review_id}/exclude"),
        ("POST", "/library/reviews/{review_id}/restore"),
        ("POST", "/library/reviews/{review_id}/candidate"),
        ("POST", "/library/reviews/bulk-preview"),
        ("POST", "/library/reviews/bulk-apply"),
        ("POST", "/library/reviews/{review_id}/retry"),
        ("GET", "/library/operations/{job_id}"),
        ("POST", "/library/operations/{job_id}/pause"),
        ("POST", "/library/operations/{job_id}/resume"),
        ("POST", "/library/operations/{job_id}/stop"),
        ("POST", "/library/albums/{album_id}/reidentify"),
        ("POST", "/library/operations/{job_id}/candidate"),
        ("POST", "/library/albums/{album_id}/split-preview"),
        ("POST", "/library/albums/{album_id}/split"),
        ("POST", "/library/albums/merge-preview"),
        ("POST", "/library/albums/merge"),
        ("POST", "/library/tracks/move-preview"),
        ("POST", "/library/tracks/move"),
        ("POST", "/library/albums/{album_id}/reset-grouping-preview"),
        ("POST", "/library/albums/{album_id}/reset-grouping"),
        ("POST", "/library/artists/merge-preview"),
        ("POST", "/library/artists/merge"),
        ("POST", "/library/identity-repairs"),
        ("GET", "/library/identity-repairs"),
        ("GET", "/library/identity-repairs/estimate"),
        ("GET", "/library/identity-repairs/{job_id}"),
        ("GET", "/library/identity-repairs/{job_id}/findings"),
        ("POST", "/library/identity-repairs/{job_id}/apply"),
        ("POST", "/library/identity-repairs/{job_id}/pause"),
        ("POST", "/library/identity-repairs/{job_id}/resume"),
        ("POST", "/library/identity-repairs/{job_id}/stop"),
        ("POST", "/library/management/identity-preparations"),
        ("GET", "/library/management/identity-preparations"),
        ("GET", "/library/management/identity-preparations/estimate"),
        ("GET", "/library/management/identity-preparations/{job_id}"),
        ("GET", "/library/management/identity-preparations/{job_id}/findings"),
        ("POST", "/library/management/identity-preparations/{job_id}/apply"),
        ("POST", "/library/management/identity-preparations/{job_id}/discard"),
        ("GET", "/library/scan-runs/{run_id}/diagnostics"),
    }
