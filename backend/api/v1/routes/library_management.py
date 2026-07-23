"""Administrator-only Library Management configuration and preview APIs."""

from __future__ import annotations

from typing import Literal

from fastapi import APIRouter, Depends, Query, Response

from api.v1.schemas.library_management import (
    LibraryManagementChangeImpact,
    LibraryManagementPresetDiff,
    LibraryManagementProfile,
    LibraryManagementSettingsResponse,
    LibraryManagementSettingsUpdateRequest,
)
from api.v1.schemas.library_management_preview import (
    LibraryManagementActivationConfirmRequest,
    LibraryManagementActivationPreviewRequest,
    LibraryManagementApplyRequest,
    LibraryManagementBaselineRestorePreviewRequest,
    LibraryManagementBaselinePurgeImpactResponse,
    LibraryManagementBaselinePurgeRequest,
    LibraryManagementBaselinePurgeResponse,
    LibraryManagementDiscardRequest,
    LibraryManagementDuplicateResolutionPreviewRequest,
    LibraryManagementOperationHistoryResponse,
    LibraryManagementPlanItemPageResponse,
    LibraryManagementPreviewCreateRequest,
    LibraryManagementPreviewCreatedResponse,
    LibraryManagementPreviewDetailResponse,
    LibraryManagementProfileCopyRequest,
    LibraryManagementProfileCreateRequest,
    LibraryManagementProfileDeleteRequest,
    LibraryManagementProfileMutationResponse,
    LibraryManagementProfileUpdateRequest,
    LibraryManagementRecoveryDiagnosticsResponse,
    LibraryManagementResultPageResponse,
    LibraryManagementSettingsImpactRequest,
    LibraryManagementTagEditPreviewRequest,
    LibraryManagementTagEditorContextResponse,
    LibraryManagementUndoPreviewRequest,
)
from api.v1.schemas.library_operations import OperationResponse
from core.dependencies import (
    LibraryManagementBaselineServiceDep,
    LibraryManagementDuplicateServiceDep,
    LibraryManagementPreviewServiceDep,
    LibraryManagementProfileServiceDep,
    LibraryManagementRecoveryServiceDep,
    LibraryManagementUndoServiceDep,
)
from core.exceptions import ValidationError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep


async def _admin_guard(_: CurrentAdminDep) -> None: ...


router = APIRouter(
    route_class=MsgSpecRoute,
    tags=["library-management"],
    dependencies=[Depends(_admin_guard)],
)


def _profile_mutation_response(
    profile: LibraryManagementProfile,
    service: LibraryManagementProfileServiceDep,
) -> LibraryManagementProfileMutationResponse:
    return LibraryManagementProfileMutationResponse(
        profile=profile,
        settings_revision=service.get_settings().settings_revision,
    )


@router.get(
    "/settings/library-management",
    response_model=LibraryManagementSettingsResponse,
)
async def get_library_management_settings(
    service: LibraryManagementProfileServiceDep,
) -> LibraryManagementSettingsResponse:
    return service.get_settings()


@router.put(
    "/settings/library-management",
    response_model=LibraryManagementSettingsResponse,
)
async def update_library_management_settings(
    service: LibraryManagementProfileServiceDep,
    request: LibraryManagementSettingsUpdateRequest = MsgSpecBody(
        LibraryManagementSettingsUpdateRequest
    ),
) -> LibraryManagementSettingsResponse:
    return service.save_settings(
        request.settings,
        expected_settings_revision=request.expected_settings_revision,
    )


@router.post(
    "/settings/library-management/impact",
    response_model=LibraryManagementChangeImpact,
)
async def preview_library_management_impact(
    service: LibraryManagementProfileServiceDep,
    request: LibraryManagementSettingsImpactRequest = MsgSpecBody(
        LibraryManagementSettingsImpactRequest
    ),
) -> LibraryManagementChangeImpact:
    return service.preview_impact(
        request.settings,
        expected_settings_revision=request.expected_settings_revision,
    )


@router.post(
    "/settings/library-management/validate",
    response_model=LibraryManagementChangeImpact,
)
async def validate_library_management_settings(
    service: LibraryManagementProfileServiceDep,
    request: LibraryManagementSettingsImpactRequest = MsgSpecBody(
        LibraryManagementSettingsImpactRequest
    ),
) -> LibraryManagementChangeImpact:
    return service.preview_impact(
        request.settings,
        expected_settings_revision=request.expected_settings_revision,
    )


@router.get(
    "/settings/library-management/profiles/{profile_id}",
    response_model=LibraryManagementProfile,
)
async def get_library_management_profile(
    profile_id: str,
    service: LibraryManagementProfileServiceDep,
) -> LibraryManagementProfile:
    return service.get_profile(profile_id)


@router.post(
    "/settings/library-management/profiles",
    response_model=LibraryManagementProfileMutationResponse,
)
async def create_library_management_profile(
    service: LibraryManagementProfileServiceDep,
    request: LibraryManagementProfileCreateRequest = MsgSpecBody(
        LibraryManagementProfileCreateRequest
    ),
) -> LibraryManagementProfileMutationResponse:
    profile = service.create_profile(
        name=request.name,
        description=request.description,
        expected_settings_revision=request.expected_settings_revision,
    )
    return _profile_mutation_response(profile, service)


@router.post(
    "/settings/library-management/profiles/{profile_id}/copy",
    response_model=LibraryManagementProfileMutationResponse,
)
async def copy_library_management_profile(
    profile_id: str,
    service: LibraryManagementProfileServiceDep,
    request: LibraryManagementProfileCopyRequest = MsgSpecBody(
        LibraryManagementProfileCopyRequest
    ),
) -> LibraryManagementProfileMutationResponse:
    profile = service.copy_profile(
        profile_id,
        name=request.name,
        expected_settings_revision=request.expected_settings_revision,
    )
    return _profile_mutation_response(profile, service)


@router.put(
    "/settings/library-management/profiles/{profile_id}",
    response_model=LibraryManagementProfileMutationResponse,
)
async def update_library_management_profile(
    profile_id: str,
    service: LibraryManagementProfileServiceDep,
    request: LibraryManagementProfileUpdateRequest = MsgSpecBody(
        LibraryManagementProfileUpdateRequest
    ),
) -> LibraryManagementProfileMutationResponse:
    if request.profile.id != profile_id:
        raise ValidationError("The profile ID does not match the request path.")
    profile = service.update_profile(
        request.profile,
        expected_settings_revision=request.expected_settings_revision,
    )
    return _profile_mutation_response(profile, service)


@router.delete(
    "/settings/library-management/profiles/{profile_id}",
    response_model=LibraryManagementSettingsResponse,
)
async def delete_library_management_profile(
    profile_id: str,
    service: LibraryManagementProfileServiceDep,
    request: LibraryManagementProfileDeleteRequest = MsgSpecBody(
        LibraryManagementProfileDeleteRequest
    ),
) -> LibraryManagementSettingsResponse:
    return service.delete_profile(
        profile_id,
        expected_settings_revision=request.expected_settings_revision,
    )


@router.get(
    "/settings/library-management/profiles/{profile_id}/preset-diff",
    response_model=LibraryManagementPresetDiff,
)
async def get_library_management_profile_preset_diff(
    profile_id: str,
    service: LibraryManagementProfileServiceDep,
) -> LibraryManagementPresetDiff:
    return service.preset_diff(profile_id)


@router.post(
    "/settings/library-management/activation-previews",
    response_model=LibraryManagementPreviewCreatedResponse,
)
async def create_library_management_activation_preview(
    admin: CurrentAdminDep,
    service: LibraryManagementPreviewServiceDep,
    request: LibraryManagementActivationPreviewRequest = MsgSpecBody(
        LibraryManagementActivationPreviewRequest
    ),
) -> LibraryManagementPreviewCreatedResponse:
    return await service.create_activation(request, admin.id)


@router.get(
    "/settings/library-management/activation-previews/{job_id}",
    response_model=LibraryManagementPreviewDetailResponse,
)
async def get_library_management_activation_preview(
    job_id: str,
    service: LibraryManagementPreviewServiceDep,
) -> LibraryManagementPreviewDetailResponse:
    detail = await service.detail(job_id)
    if detail.proposed_settings_revision is None:
        raise ValidationError("This is not an activation preview.")
    return detail


@router.post(
    "/settings/library-management/activation-confirmations",
    response_model=LibraryManagementSettingsResponse,
)
async def confirm_library_management_activation(
    service: LibraryManagementPreviewServiceDep,
    request: LibraryManagementActivationConfirmRequest = MsgSpecBody(
        LibraryManagementActivationConfirmRequest
    ),
) -> LibraryManagementSettingsResponse:
    return await service.confirm_activation(request)


@router.post(
    "/library/management/previews",
    response_model=LibraryManagementPreviewCreatedResponse,
)
async def create_library_management_preview(
    admin: CurrentAdminDep,
    service: LibraryManagementPreviewServiceDep,
    request: LibraryManagementPreviewCreateRequest = MsgSpecBody(
        LibraryManagementPreviewCreateRequest
    ),
) -> LibraryManagementPreviewCreatedResponse:
    return await service.create_manual(request, admin.id)


@router.get(
    "/library/management/tracks/{track_id}/tag-editor",
    response_model=LibraryManagementTagEditorContextResponse,
)
async def get_library_management_tag_editor(
    track_id: str,
    service: LibraryManagementPreviewServiceDep,
) -> LibraryManagementTagEditorContextResponse:
    return await service.tag_editor_context(track_id)


@router.post(
    "/library/management/tag-edit-previews",
    response_model=LibraryManagementPreviewCreatedResponse,
)
async def create_library_management_tag_edit_preview(
    admin: CurrentAdminDep,
    service: LibraryManagementPreviewServiceDep,
    request: LibraryManagementTagEditPreviewRequest = MsgSpecBody(
        LibraryManagementTagEditPreviewRequest
    ),
) -> LibraryManagementPreviewCreatedResponse:
    return await service.create_tag_edit(request, admin.id)


@router.post(
    "/library/management/baselines/restore-previews",
    response_model=LibraryManagementPreviewCreatedResponse,
)
async def create_library_management_baseline_restore_preview(
    admin: CurrentAdminDep,
    service: LibraryManagementBaselineServiceDep,
    request: LibraryManagementBaselineRestorePreviewRequest = MsgSpecBody(
        LibraryManagementBaselineRestorePreviewRequest
    ),
) -> LibraryManagementPreviewCreatedResponse:
    return await service.create_restore_preview(request, admin.id)


@router.post(
    "/library/management/duplicate-resolution-previews",
    response_model=LibraryManagementPreviewCreatedResponse,
)
async def create_library_management_duplicate_resolution_preview(
    admin: CurrentAdminDep,
    service: LibraryManagementDuplicateServiceDep,
    request: LibraryManagementDuplicateResolutionPreviewRequest = MsgSpecBody(
        LibraryManagementDuplicateResolutionPreviewRequest
    ),
) -> LibraryManagementPreviewCreatedResponse:
    return await service.create_preview(request, admin.id)


@router.post(
    "/library/management/baselines/purge-impact",
    response_model=LibraryManagementBaselinePurgeImpactResponse,
)
async def preview_library_management_baseline_purge(
    service: LibraryManagementBaselineServiceDep,
) -> LibraryManagementBaselinePurgeImpactResponse:
    return await service.purge_impact()


@router.post(
    "/library/management/baselines/purge",
    response_model=LibraryManagementBaselinePurgeResponse,
)
async def purge_library_management_baselines(
    admin: CurrentAdminDep,
    service: LibraryManagementBaselineServiceDep,
    request: LibraryManagementBaselinePurgeRequest = MsgSpecBody(
        LibraryManagementBaselinePurgeRequest
    ),
) -> LibraryManagementBaselinePurgeResponse:
    return await service.purge(request, admin.id)


@router.get(
    "/library/management/operations",
    response_model=LibraryManagementOperationHistoryResponse,
)
async def list_library_management_operations(
    service: LibraryManagementPreviewServiceDep,
    limit: int = Query(50, ge=1, le=50),
    cursor: str | None = None,
    origin: str | None = None,
    profile_id: str | None = None,
    root_id: str | None = None,
    state: str | None = None,
    mode: str | None = None,
    created_from: float | None = None,
    created_to: float | None = None,
) -> LibraryManagementOperationHistoryResponse:
    return await service.history(
        limit=limit,
        cursor=cursor,
        origin=origin,
        profile_id=profile_id,
        root_id=root_id,
        state=state,
        mode=mode,
        created_from=created_from,
        created_to=created_to,
    )


@router.get(
    "/library/management/operations/{job_id}",
    response_model=LibraryManagementPreviewDetailResponse,
)
async def get_library_management_operation(
    job_id: str,
    service: LibraryManagementPreviewServiceDep,
) -> LibraryManagementPreviewDetailResponse:
    return await service.detail(job_id)


@router.post(
    "/library/management/operations/{job_id}/undo-preview",
    response_model=LibraryManagementPreviewCreatedResponse,
)
async def create_library_management_undo_preview(
    job_id: str,
    admin: CurrentAdminDep,
    service: LibraryManagementUndoServiceDep,
    request: LibraryManagementUndoPreviewRequest = MsgSpecBody(
        LibraryManagementUndoPreviewRequest
    ),
) -> LibraryManagementPreviewCreatedResponse:
    return await service.create_preview(job_id, request, admin.id)


@router.get(
    "/library/management/operations/{job_id}/results",
    response_model=LibraryManagementResultPageResponse,
)
async def list_library_management_operation_results(
    job_id: str,
    service: LibraryManagementPreviewServiceDep,
    after_ordinal: int = Query(-1, ge=-1),
    limit: int = Query(100, ge=1, le=200),
) -> LibraryManagementResultPageResponse:
    return await service.results(job_id, after_ordinal=after_ordinal, limit=limit)


@router.get(
    "/library/management/previews/{job_id}",
    response_model=LibraryManagementPreviewDetailResponse,
)
async def get_library_management_preview(
    job_id: str,
    service: LibraryManagementPreviewServiceDep,
) -> LibraryManagementPreviewDetailResponse:
    return await service.detail(job_id)


@router.post(
    "/library/management/previews/{job_id}/discard",
    response_model=LibraryManagementPreviewDetailResponse,
)
async def discard_library_management_preview(
    job_id: str,
    service: LibraryManagementPreviewServiceDep,
    request: LibraryManagementDiscardRequest = MsgSpecBody(
        LibraryManagementDiscardRequest
    ),
) -> LibraryManagementPreviewDetailResponse:
    return await service.discard(job_id, request)


@router.post(
    "/library/management/previews/{job_id}/apply",
    response_model=OperationResponse,
)
async def apply_library_management_preview(
    job_id: str,
    service: LibraryManagementPreviewServiceDep,
    request: LibraryManagementApplyRequest = MsgSpecBody(LibraryManagementApplyRequest),
) -> OperationResponse:
    return await service.apply(job_id, request)


@router.get(
    "/library/management/recovery/diagnostics",
    response_model=LibraryManagementRecoveryDiagnosticsResponse,
)
async def get_library_management_recovery_diagnostics(
    service: LibraryManagementRecoveryServiceDep,
) -> LibraryManagementRecoveryDiagnosticsResponse:
    return LibraryManagementRecoveryDiagnosticsResponse(**await service.diagnostics())


@router.get(
    "/library/management/previews/{job_id}/items",
    response_model=LibraryManagementPlanItemPageResponse,
)
async def list_library_management_preview_items(
    job_id: str,
    service: LibraryManagementPreviewServiceDep,
    after_ordinal: int = Query(-1, ge=-1),
    limit: int = Query(100, ge=1, le=200),
    eligibility: Literal["eligible", "warning", "blocked", "stale"] | None = None,
    reason_code: str | None = None,
    root_id: str | None = None,
    artist_id: str | None = None,
    album_id: str | None = None,
    audio_format: str | None = None,
    collision_class: str | None = None,
    has_preserved_value: bool | None = None,
    has_representation_loss: bool | None = None,
    change_kind: Literal["tags", "artwork", "path", "sidecars", "no_change"]
    | None = None,
) -> LibraryManagementPlanItemPageResponse:
    return await service.items(
        job_id,
        after_ordinal=after_ordinal,
        limit=limit,
        eligibility=eligibility,
        reason_code=reason_code,
        root_id=root_id,
        artist_id=artist_id,
        album_id=album_id,
        audio_format=audio_format,
        collision_class=collision_class,
        has_preserved_value=has_preserved_value,
        has_representation_loss=has_representation_loss,
        change_kind=change_kind,
    )


@router.get(
    "/library/management/previews/{job_id}/items/{ordinal}/artwork/{sha256}",
    response_class=Response,
)
async def get_library_management_preview_artwork(
    job_id: str,
    ordinal: int,
    sha256: str,
    service: LibraryManagementPreviewServiceDep,
) -> Response:
    content, mime_type = await service.artwork_preview(job_id, ordinal, sha256)
    return Response(
        content=content,
        media_type=mime_type,
        headers={
            "Cache-Control": "private, max-age=86400, immutable",
            "X-Content-Type-Options": "nosniff",
        },
    )
