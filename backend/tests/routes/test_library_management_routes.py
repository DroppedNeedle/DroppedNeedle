import json
from pathlib import Path
from unittest.mock import AsyncMock

import msgspec
import pytest
from fastapi import FastAPI, HTTPException

from api.v1.routes.library import router as legacy_library_router
from api.v1.routes.library_management import router
from api.v1.routes.library_target import router as target_library_router
from api.v1.schemas.library_management import (
    PICARD_ORGANIZER_PROFILE_ID,
    LibraryManagementSettings,
)
from api.v1.schemas.library_management_preview import (
    LibraryManagementOperationHistoryResponse,
    LibraryManagementBaselinePurgeImpactResponse,
    LibraryManagementBaselinePurgeResponse,
    LibraryManagementPlanItemPageResponse,
    LibraryManagementPreviewCreatedResponse,
    LibraryManagementPreviewDetailResponse,
    LibraryManagementResultPageResponse,
    LibraryManagementTagEditorContextResponse,
)
from api.v1.schemas.library_operations import OperationResponse
from core.config import Settings
from core.dependencies import (
    get_library_management_duplicate_service,
    get_library_management_preview_service,
    get_library_management_profile_service,
    get_library_management_recovery_service,
    get_library_management_baseline_service,
    get_library_management_undo_service,
)
from middleware import _get_current_admin
from services.native.library_management_preview_service import (
    LibraryManagementPreviewService,
)
from services.native.library_management_profile_service import (
    LibraryManagementProfileService,
)
from services.native.library_management_recovery_service import (
    LibraryManagementRecoveryService,
)
from services.native.library_management_baseline_service import (
    LibraryManagementBaselineService,
)
from services.native.library_management_duplicate_service import (
    LibraryManagementDuplicateService,
)
from services.native.library_management_undo_service import LibraryManagementUndoService
from services.preferences_service import PreferencesService
from tests.helpers import build_test_client, override_admin_auth


def _profile_service(tmp_path: Path) -> LibraryManagementProfileService:
    root = tmp_path / "Music"
    root.mkdir()
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    settings.config_file_path.write_text(
        json.dumps({"library_settings": {"library_paths": [str(root)]}}),
        encoding="utf-8",
    )
    return LibraryManagementProfileService(PreferencesService(settings))


def _preview_detail(
    *, proposed_settings_revision: str | None = None
) -> LibraryManagementPreviewDetailResponse:
    return LibraryManagementPreviewDetailResponse(
        job_id="job-1",
        state="ready",
        phase="ready",
        mode="preview",
        origin="manual",
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        profile_name="Picard-style organizer",
        profile_revision="profile-revision",
        settings_revision="settings-revision",
        policy_revision="policy-revision",
        catalog_revision=1,
        proposed_settings_revision=proposed_settings_revision,
    )


@pytest.fixture
def route_services(
    tmp_path: Path,
) -> tuple[LibraryManagementProfileService, AsyncMock]:
    profile = _profile_service(tmp_path)
    preview = AsyncMock(spec=LibraryManagementPreviewService)
    preview.create_manual.return_value = LibraryManagementPreviewCreatedResponse(
        job_id="job-1", preview_token="opaque", created_at=1.0, expires_at=2.0
    )
    preview.create_activation.return_value = preview.create_manual.return_value
    preview.create_tag_edit.return_value = preview.create_manual.return_value
    preview.tag_editor_context.return_value = LibraryManagementTagEditorContextResponse(
        local_track_id="track-1",
        local_album_id="album-1",
        root_id="root-1",
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        profile_name="Picard-style organizer",
        settings_revision="settings-revision",
        policy_revision="policy-revision",
        track_revision=1,
        album_revision=1,
        accepted_identity=True,
    )
    preview.detail.return_value = _preview_detail()
    preview.items.return_value = LibraryManagementPlanItemPageResponse(items=[])
    preview.artwork_preview.return_value = (b"pinned-artwork", "image/png")
    preview.results.return_value = LibraryManagementResultPageResponse(items=[])
    preview.history.return_value = LibraryManagementOperationHistoryResponse(items=[])
    preview.apply.return_value = OperationResponse(
        id="job-1", kind="library_management", state="queued"
    )
    preview.confirm_activation.return_value = profile.get_settings()
    return profile, preview


@pytest.fixture
def app(
    route_services: tuple[LibraryManagementProfileService, AsyncMock],
) -> FastAPI:
    profile, preview = route_services
    application = FastAPI()
    application.include_router(router)
    application.dependency_overrides[get_library_management_profile_service] = (
        lambda: profile
    )
    application.dependency_overrides[get_library_management_preview_service] = (
        lambda: preview
    )
    recovery = AsyncMock(spec=LibraryManagementRecoveryService)
    recovery.diagnostics.return_value = {
        "recoverable_bundle_count": 2,
        "nonterminal_journal_count": 3,
        "needs_attention_count": 1,
        "cleanup_pending_count": 1,
        "oldest_updated_at": 10.0,
        "state_counts": {"cleanup_pending": 1, "needs_attention": 1},
    }
    application.dependency_overrides[get_library_management_recovery_service] = (
        lambda: recovery
    )
    undo = AsyncMock(spec=LibraryManagementUndoService)
    undo.create_preview.return_value = LibraryManagementPreviewCreatedResponse(
        job_id="undo-job-1",
        preview_token="undo-opaque",
        created_at=3.0,
        expires_at=4.0,
    )
    application.dependency_overrides[get_library_management_undo_service] = lambda: undo
    application.state.library_management_undo = undo
    baseline = AsyncMock(spec=LibraryManagementBaselineService)
    baseline.create_restore_preview.return_value = (
        LibraryManagementPreviewCreatedResponse(
            job_id="restore-job-1",
            preview_token="restore-opaque",
            created_at=5.0,
            expires_at=6.0,
        )
    )
    baseline.purge_impact.return_value = LibraryManagementBaselinePurgeImpactResponse(
        baseline_count=2,
        referenced_blob_count=3,
        referenced_blob_bytes=4096,
        blocked_journal_count=0,
        active_restore_count=0,
        catalog_revision=7,
        impact_token="impact",
    )
    baseline.purge.return_value = LibraryManagementBaselinePurgeResponse(
        purged_baseline_count=2,
        detached_reference_count=3,
        cleaned_blob_count=2,
    )
    application.dependency_overrides[get_library_management_baseline_service] = (
        lambda: baseline
    )
    application.state.library_management_baseline = baseline
    duplicate = AsyncMock(spec=LibraryManagementDuplicateService)
    duplicate.create_preview.return_value = LibraryManagementPreviewCreatedResponse(
        job_id="duplicate-job-1",
        preview_token="duplicate-opaque",
        created_at=7.0,
        expires_at=8.0,
    )
    application.dependency_overrides[get_library_management_duplicate_service] = (
        lambda: duplicate
    )
    application.state.library_management_duplicate = duplicate
    return application


def test_settings_profile_crud_impact_and_validation(
    app: FastAPI,
    route_services: tuple[LibraryManagementProfileService, AsyncMock],
) -> None:
    profile_service, _ = route_services
    override_admin_auth(app)
    client = build_test_client(app)

    current = client.get("/settings/library-management")
    assert current.status_code == 200
    revision = current.json()["settings_revision"]

    created = client.post(
        "/settings/library-management/profiles",
        json={
            "name": "Careful organizer",
            "description": "Preserves local choices",
            "expected_settings_revision": revision,
        },
    )
    assert created.status_code == 200
    profile = created.json()["profile"]
    assert profile["name"] == "Careful organizer"
    revision = created.json()["settings_revision"]

    read = client.get(f"/settings/library-management/profiles/{profile['id']}")
    assert read.status_code == 200
    assert read.json()["id"] == profile["id"]

    profile["description"] = "Updated"
    updated = client.put(
        f"/settings/library-management/profiles/{profile['id']}",
        json={"profile": profile, "expected_settings_revision": revision},
    )
    assert updated.status_code == 200
    assert updated.json()["profile"]["description"] == "Updated"
    revision = updated.json()["settings_revision"]

    proposed = msgspec.to_builtins(
        msgspec.convert(
            msgspec.to_builtins(profile_service.get_settings()),
            type=LibraryManagementSettings,
        )
    )
    impact = client.post(
        "/settings/library-management/impact",
        json={"settings": proposed, "expected_settings_revision": revision},
    )
    validation = client.post(
        "/settings/library-management/validate",
        json={"settings": proposed, "expected_settings_revision": revision},
    )
    assert impact.status_code == 200
    assert impact.json()["classification"] == "no_change"
    assert validation.status_code == 200

    deleted = client.request(
        "DELETE",
        f"/settings/library-management/profiles/{profile['id']}",
        json={"expected_settings_revision": revision},
    )
    assert deleted.status_code == 200
    assert all(value["id"] != profile["id"] for value in deleted.json()["profiles"])


def test_profile_path_mismatch_uses_error_envelope(
    app: FastAPI,
    route_services: tuple[LibraryManagementProfileService, AsyncMock],
) -> None:
    profile_service, _ = route_services
    override_admin_auth(app)
    settings = profile_service.get_settings()
    profile = msgspec.to_builtins(settings.profiles[0])
    response = build_test_client(app).put(
        "/settings/library-management/profiles/different",
        json={
            "profile": profile,
            "expected_settings_revision": settings.settings_revision,
        },
    )
    assert response.status_code == 400
    assert response.json()["error"]["code"] == "VALIDATION_ERROR"


def test_preview_routes_forward_actor_filters_and_hide_source_identity(
    app: FastAPI,
    route_services: tuple[LibraryManagementProfileService, AsyncMock],
) -> None:
    profile_service, preview = route_services
    override_admin_auth(app)
    client = build_test_client(app)
    settings = profile_service.get_settings()
    response = client.post(
        "/library/management/previews",
        json={
            "selection": {"kind": "tracks", "ids": ["track-1"]},
            "profile_id": PICARD_ORGANIZER_PROFILE_ID,
            "expected_settings_revision": settings.settings_revision,
            "expected_policy_revision": "policy-revision",
            "idempotency_key": "preview-request-1",
            "overrides": {"metadata_enabled": False, "move_enabled": False},
        },
    )
    assert response.status_code == 200
    preview.create_manual.assert_awaited_once()
    assert preview.create_manual.await_args.args[1] == "test-admin-id"
    request = preview.create_manual.await_args.args[0]
    assert request.overrides.metadata_enabled is False
    assert request.overrides.move_enabled is False

    detail = client.get("/library/management/previews/job-1")
    page = client.get(
        "/library/management/previews/job-1/items",
        params={
            "after_ordinal": 4,
            "limit": 25,
            "eligibility": "warning",
            "reason_code": "OPTIONAL_ENRICHMENT_DEFERRED",
            "root_id": "root-1",
            "artist_id": "artist-1",
            "album_id": "album-1",
            "audio_format": "flac",
            "collision_class": "normalized_path_collision",
            "has_preserved_value": True,
            "has_representation_loss": True,
            "change_kind": "tags",
        },
    )
    assert detail.status_code == 200
    assert "source_path_identity" not in detail.text
    assert page.status_code == 200
    preview.items.assert_awaited_once_with(
        "job-1",
        after_ordinal=4,
        limit=25,
        eligibility="warning",
        reason_code="OPTIONAL_ENRICHMENT_DEFERRED",
        root_id="root-1",
        artist_id="artist-1",
        album_id="album-1",
        audio_format="flac",
        collision_class="normalized_path_collision",
        has_preserved_value=True,
        has_representation_loss=True,
        change_kind="tags",
    )


def test_activation_status_rejects_manual_preview(
    app: FastAPI,
) -> None:
    override_admin_auth(app)
    response = build_test_client(app).get(
        "/settings/library-management/activation-previews/job-1"
    )
    assert response.status_code == 400
    assert response.json()["error"]["code"] == "VALIDATION_ERROR"


def test_preview_artwork_route_is_private_and_content_typed(
    app: FastAPI,
    route_services: tuple[LibraryManagementProfileService, AsyncMock],
) -> None:
    _profile, preview = route_services
    override_admin_auth(app)

    response = build_test_client(app).get(
        "/library/management/previews/job-1/items/2/artwork/" + "a" * 64
    )

    assert response.status_code == 200
    assert response.content == b"pinned-artwork"
    assert response.headers["content-type"] == "image/png"
    assert response.headers["cache-control"] == "private, max-age=86400, immutable"
    assert response.headers["x-content-type-options"] == "nosniff"
    preview.artwork_preview.assert_awaited_once_with("job-1", 2, "a" * 64)


def test_recovery_diagnostics_are_bounded_admin_state(app: FastAPI) -> None:
    override_admin_auth(app)
    response = build_test_client(app).get("/library/management/recovery/diagnostics")

    assert response.status_code == 200
    assert response.json() == {
        "recoverable_bundle_count": 2,
        "nonterminal_journal_count": 3,
        "needs_attention_count": 1,
        "cleanup_pending_count": 1,
        "oldest_updated_at": 10.0,
        "state_counts": {"cleanup_pending": 1, "needs_attention": 1},
    }


def test_apply_history_and_result_routes_are_admin_bounded(
    app: FastAPI,
    route_services: tuple[LibraryManagementProfileService, AsyncMock],
) -> None:
    _, preview = route_services
    override_admin_auth(app)
    client = build_test_client(app)
    applied = client.post(
        "/library/management/previews/job-1/apply",
        json={
            "preview_token": "opaque",
            "expected_operation_row_revision": 2,
            "idempotency_key": "apply-once",
            "confirmation": True,
        },
    )
    history = client.get(
        "/library/management/operations",
        params={
            "limit": 25,
            "origin": "manual",
            "profile_id": PICARD_ORGANIZER_PROFILE_ID,
            "root_id": "root-1",
            "state": "succeeded",
            "mode": "apply",
            "created_from": 1,
            "created_to": 2,
        },
    )
    detail = client.get("/library/management/operations/job-1")
    results = client.get(
        "/library/management/operations/job-1/results",
        params={"after_ordinal": 4, "limit": 25},
    )

    assert applied.status_code == 200
    assert history.status_code == 200
    assert detail.status_code == 200
    assert results.status_code == 200
    preview.apply.assert_awaited_once()
    preview.history.assert_awaited_once_with(
        limit=25,
        cursor=None,
        origin="manual",
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        root_id="root-1",
        state="succeeded",
        mode="apply",
        created_from=1.0,
        created_to=2.0,
    )
    preview.results.assert_awaited_once_with("job-1", after_ordinal=4, limit=25)


def test_undo_preview_route_forwards_source_revision_and_actor(app: FastAPI) -> None:
    override_admin_auth(app)
    response = build_test_client(app).post(
        "/library/management/operations/job-1/undo-preview",
        json={
            "expected_operation_row_revision": 8,
            "idempotency_key": "undo-once",
        },
    )

    assert response.status_code == 200
    assert response.json()["job_id"] == "undo-job-1"
    undo = app.state.library_management_undo
    undo.create_preview.assert_awaited_once()
    assert undo.create_preview.await_args.args[0] == "job-1"
    assert undo.create_preview.await_args.args[1].expected_operation_row_revision == 8
    assert undo.create_preview.await_args.args[2] == "test-admin-id"


def test_baseline_restore_and_purge_routes_are_explicit_admin_actions(
    app: FastAPI,
) -> None:
    override_admin_auth(app)
    client = build_test_client(app)
    restore = client.post(
        "/library/management/baselines/restore-previews",
        json={
            "selection": {"kind": "tracks", "ids": ["track-1"]},
            "expected_settings_revision": "settings",
            "expected_policy_revision": "policy",
            "idempotency_key": "restore-once",
        },
    )
    impact = client.post("/library/management/baselines/purge-impact")
    purge = client.post(
        "/library/management/baselines/purge",
        json={
            "impact_token": "impact",
            "expected_catalog_revision": 7,
            "typed_confirmation": "PURGE BASELINES",
            "idempotency_key": "purge-once",
        },
    )

    assert restore.status_code == 200
    assert impact.status_code == 200
    assert purge.status_code == 200
    baseline = app.state.library_management_baseline
    baseline.create_restore_preview.assert_awaited_once()
    assert baseline.create_restore_preview.await_args.args[1] == "test-admin-id"
    baseline.purge.assert_awaited_once()
    assert baseline.purge.await_args.args[1] == "test-admin-id"


def test_duplicate_resolution_route_forwards_explicit_choice_and_actor(
    app: FastAPI,
) -> None:
    override_admin_auth(app)
    response = build_test_client(app).post(
        "/library/management/duplicate-resolution-previews",
        json={
            "source_job_id": "source-job-1",
            "source_plan_item_ordinal": 4,
            "expected_source_operation_row_revision": 9,
            "collision_kind": "same_path_different_content",
            "existing_root_id": "root-1",
            "existing_relative_path": "Artist/Album/01 Song.flac",
            "action": "keep_incoming_alternate",
            "alternate_relative_path": "Artist/Album/01 Song (2).flac",
            "expected_settings_revision": "settings",
            "expected_policy_revision": "policy",
            "idempotency_key": "resolve-once",
        },
    )

    assert response.status_code == 200
    assert response.json()["job_id"] == "duplicate-job-1"
    duplicate = app.state.library_management_duplicate
    duplicate.create_preview.assert_awaited_once()
    request, actor = duplicate.create_preview.await_args.args
    assert request.source_plan_item_ordinal == 4
    assert request.action == "keep_incoming_alternate"
    assert request.alternate_relative_path == "Artist/Album/01 Song (2).flac"
    assert actor == "test-admin-id"


def test_management_routes_are_admin_only(app: FastAPI) -> None:
    def reject_admin() -> None:
        raise HTTPException(status_code=403, detail="Admin access required")

    app.dependency_overrides[_get_current_admin] = reject_admin
    client = build_test_client(app)
    assert client.get("/settings/library-management").status_code == 403
    assert client.get("/library/management/previews/job-1").status_code == 403
    assert client.get("/library/management/recovery/diagnostics").status_code == 403

    unauthenticated = FastAPI()
    unauthenticated.include_router(router)
    client = build_test_client(unauthenticated)
    assert client.get("/settings/library-management").status_code == 401
    assert client.get("/library/management/previews/job-1").status_code == 401
    assert client.get("/library/management/recovery/diagnostics").status_code == 401


def test_preview_route_uses_fixed_5xx_copy(
    app: FastAPI,
    route_services: tuple[LibraryManagementProfileService, AsyncMock],
) -> None:
    _, preview = route_services
    override_admin_auth(app)
    preview.detail.side_effect = RuntimeError(
        "failed while reading /secret/library/private.flac"
    )
    response = build_test_client(app).get("/library/management/previews/job-1")
    assert response.status_code == 500
    assert response.json()["error"]["message"] == "Internal server error"
    assert "/secret/library" not in response.text


def test_tag_editor_routes_use_admin_service_contract(
    app: FastAPI,
    route_services: tuple[LibraryManagementProfileService, AsyncMock],
) -> None:
    _, preview = route_services
    override_admin_auth(app)
    client = build_test_client(app)

    response = client.get("/library/management/tracks/track-1/tag-editor")
    assert response.status_code == 200
    assert response.json()["accepted_identity"] is True
    preview.tag_editor_context.assert_awaited_once_with("track-1")

    response = client.post(
        "/library/management/tag-edit-previews",
        json={
            "local_track_id": "track-1",
            "mode": "save_override",
            "expected_settings_revision": "settings-revision",
            "expected_policy_revision": "policy-revision",
            "fields": [{"field_name": "genre", "value": ["Rock", "Art Rock"]}],
        },
    )
    assert response.status_code == 200
    request, actor = preview.create_tag_edit.await_args.args
    assert request.fields[0].value == ["Rock", "Art Rock"]
    assert actor == "test-admin-id"


def test_management_route_inventory_is_complete() -> None:
    inventory = {
        (method, route.path)
        for route in router.routes
        for method in getattr(route, "methods", set())
        if method in {"GET", "POST", "PUT", "DELETE"}
    }
    assert inventory == {
        ("GET", "/settings/library-management"),
        ("PUT", "/settings/library-management"),
        ("POST", "/settings/library-management/impact"),
        ("POST", "/settings/library-management/validate"),
        ("GET", "/settings/library-management/profiles/{profile_id}"),
        ("POST", "/settings/library-management/profiles"),
        ("POST", "/settings/library-management/profiles/{profile_id}/copy"),
        ("PUT", "/settings/library-management/profiles/{profile_id}"),
        ("DELETE", "/settings/library-management/profiles/{profile_id}"),
        (
            "GET",
            "/settings/library-management/profiles/{profile_id}/preset-diff",
        ),
        ("POST", "/settings/library-management/activation-previews"),
        ("GET", "/settings/library-management/activation-previews/{job_id}"),
        ("POST", "/settings/library-management/activation-confirmations"),
        ("POST", "/library/management/previews"),
        ("GET", "/library/management/tracks/{track_id}/tag-editor"),
        ("POST", "/library/management/tag-edit-previews"),
        ("GET", "/library/management/previews/{job_id}"),
        ("GET", "/library/management/previews/{job_id}/items"),
        (
            "GET",
            "/library/management/previews/{job_id}/items/{ordinal}/artwork/{sha256}",
        ),
        ("POST", "/library/management/previews/{job_id}/apply"),
        ("GET", "/library/management/operations"),
        ("GET", "/library/management/operations/{job_id}"),
        ("GET", "/library/management/operations/{job_id}/results"),
        ("POST", "/library/management/operations/{job_id}/undo-preview"),
        ("POST", "/library/management/baselines/restore-previews"),
        ("POST", "/library/management/duplicate-resolution-previews"),
        ("POST", "/library/management/baselines/purge-impact"),
        ("POST", "/library/management/baselines/purge"),
        ("GET", "/library/management/recovery/diagnostics"),
    }


@pytest.mark.parametrize(
    "library_router", [legacy_library_router, target_library_router]
)
def test_direct_track_tag_writer_is_not_exposed(library_router: object) -> None:
    assert not any(
        route.path == "/library/tracks/{track_id}"
        and "POST" in getattr(route, "methods", set())
        for route in library_router.routes
    )
