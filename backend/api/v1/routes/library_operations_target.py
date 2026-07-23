"""Staged target review, correction, operation, repair, and diagnostic APIs."""

from __future__ import annotations

from collections.abc import AsyncIterator

from fastapi import APIRouter, Query
from fastapi.responses import StreamingResponse

from api.v1.schemas.library_operations import (
    ArtistMergeApplyRequest,
    ArtistMergePreviewRequest,
    BulkReviewApplyRequest,
    BulkReviewPreviewRequest,
    BulkReviewPreviewResponse,
    CandidateAcceptanceRequest,
    CatalogCorrectionResponse,
    IdentityPreparationCreateRequest,
    IdentityPreparationEstimateResponse,
    MembershipApplyRequest,
    MembershipPreviewRequest,
    MembershipPreviewResponse,
    OperationControlRequest,
    OperationListResponse,
    OperationResponse,
    ReidentificationCandidateRequest,
    ReidentificationRequest,
    RepairApplyRequest,
    RepairCreateRequest,
    RepairEstimateResponse,
    RepairFindingListResponse,
    ReviewActionRequest,
    ReviewActionResponse,
    ReviewDetailResponse,
    ReviewListResponse,
)
from core.dependencies.type_aliases import (
    CatalogCorrectionServiceDep,
    ExplicitReidentificationWorkerDep,
    IdentityRepairServiceDep,
    LibraryDiagnosticsServiceDep,
    LibraryOperationServiceDep,
    LibraryReviewServiceDep,
    TargetReidentificationServiceDep,
)
from core.exceptions import ValidationError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep

router = APIRouter(
    route_class=MsgSpecRoute,
    prefix="/library",
    tags=["library-operations-target"],
)


@router.get("/reviews", response_model=ReviewListResponse)
async def list_reviews(
    _: CurrentAdminDep,
    service: LibraryReviewServiceDep,
    cursor: str | None = None,
    limit: int = Query(50, ge=1, le=200),
    state: str | None = None,
    reason_code: str | None = None,
    root_id: str | None = None,
    policy: str | None = None,
    search: str | None = None,
    metadata_incomplete: bool | None = None,
    candidate_available: bool | None = None,
    job_state: str | None = None,
    sort: str = "newest",
    created_from: float | None = None,
    created_to: float | None = None,
    updated_from: float | None = None,
    updated_to: float | None = None,
) -> ReviewListResponse:
    return await service.list_reviews(
        cursor=cursor,
        limit=limit,
        state=state,
        reason_code=reason_code,
        root_id=root_id,
        policy=policy,
        search=search,
        metadata_incomplete=metadata_incomplete,
        candidate_available=candidate_available,
        job_state=job_state,
        sort=sort,
        created_from=created_from,
        created_to=created_to,
        updated_from=updated_from,
        updated_to=updated_to,
    )


@router.get("/reviews/{review_id}", response_model=ReviewDetailResponse)
async def review_detail(
    _: CurrentAdminDep, review_id: str, service: LibraryReviewServiceDep
) -> ReviewDetailResponse:
    return await service.detail(review_id)


async def _review_action(
    review_id: str,
    action: str,
    body: ReviewActionRequest,
    admin: CurrentAdminDep,
    service: LibraryReviewServiceDep,
) -> ReviewActionResponse:
    return await service.act(review_id, action, body, admin.id)


@router.post("/reviews/{review_id}/keep-tagged", response_model=ReviewActionResponse)
async def keep_tagged(
    admin: CurrentAdminDep,
    review_id: str,
    service: LibraryReviewServiceDep,
    body: ReviewActionRequest = MsgSpecBody(ReviewActionRequest),
) -> ReviewActionResponse:
    return await _review_action(review_id, "keep_tagged", body, admin, service)


@router.post(
    "/reviews/{review_id}/detach-and-keep-tagged",
    response_model=ReviewActionResponse,
)
async def detach_and_keep_tagged(
    admin: CurrentAdminDep,
    review_id: str,
    service: LibraryReviewServiceDep,
    body: ReviewActionRequest = MsgSpecBody(ReviewActionRequest),
) -> ReviewActionResponse:
    return await _review_action(review_id, "detach_keep_tagged", body, admin, service)


@router.post("/reviews/{review_id}/exclude", response_model=ReviewActionResponse)
async def exclude_review(
    admin: CurrentAdminDep,
    review_id: str,
    service: LibraryReviewServiceDep,
    body: ReviewActionRequest = MsgSpecBody(ReviewActionRequest),
) -> ReviewActionResponse:
    return await _review_action(review_id, "exclude", body, admin, service)


@router.post("/reviews/{review_id}/restore", response_model=ReviewActionResponse)
async def restore_review(
    admin: CurrentAdminDep,
    review_id: str,
    service: LibraryReviewServiceDep,
    body: ReviewActionRequest = MsgSpecBody(ReviewActionRequest),
) -> ReviewActionResponse:
    return await _review_action(review_id, "restore", body, admin, service)


@router.post("/reviews/{review_id}/candidate", response_model=ReviewActionResponse)
async def accept_review_candidate(
    admin: CurrentAdminDep,
    review_id: str,
    service: LibraryReviewServiceDep,
    body: CandidateAcceptanceRequest = MsgSpecBody(CandidateAcceptanceRequest),
) -> ReviewActionResponse:
    return await service.accept_candidate(review_id, body, admin.id)


@router.post("/reviews/bulk-preview", response_model=BulkReviewPreviewResponse)
async def preview_bulk_reviews(
    _: CurrentAdminDep,
    service: LibraryReviewServiceDep,
    body: BulkReviewPreviewRequest = MsgSpecBody(BulkReviewPreviewRequest),
) -> BulkReviewPreviewResponse:
    return await service.preview_bulk(body)


@router.post("/reviews/bulk-apply", response_model=OperationResponse)
async def apply_bulk_reviews(
    admin: CurrentAdminDep,
    service: LibraryReviewServiceDep,
    body: BulkReviewApplyRequest = MsgSpecBody(BulkReviewApplyRequest),
) -> OperationResponse:
    return await service.apply_bulk(body, admin.id)


@router.post("/reviews/{review_id}/retry", response_model=OperationResponse)
async def retry_review(
    admin: CurrentAdminDep,
    review_id: str,
    reviews: LibraryReviewServiceDep,
    reidentification: TargetReidentificationServiceDep,
    body: ReviewActionRequest = MsgSpecBody(ReviewActionRequest),
) -> OperationResponse:
    if not body.confirmation:
        raise ValidationError("Confirm Retry identification before starting it.")
    detail = await reviews.detail(review_id)
    if detail.review.local_album_id is None or detail.album_revision is None:
        raise ValidationError("Only album reviews can be retried through this action.")
    row = await reidentification.create_or_coalesce(
        detail.review.local_album_id,
        admin.id,
        expected_album_revision=detail.album_revision,
        expected_input_revision=detail.input_revision,
        one_off_local_metadata=detail.review.effective_policy == "local_metadata",
        idempotency_key=body.idempotency_key,
        review_id=review_id,
        expected_review_revision=body.expected_review_revision,
    )
    return OperationResponse(
        **{key: row[key] for key in OperationResponse.__struct_fields__ if key in row}
    )


@router.get("/operations/{job_id}", response_model=OperationResponse)
async def get_operation(
    _: CurrentAdminDep, job_id: str, service: LibraryOperationServiceDep
) -> OperationResponse:
    return await service.get(job_id)


async def _control_operation(
    _: CurrentAdminDep,
    job_id: str,
    control: str,
    service: LibraryOperationServiceDep,
    body: OperationControlRequest = MsgSpecBody(OperationControlRequest),
) -> OperationResponse:
    return await service.control(
        job_id,
        control,
        body.expected_row_revision,
        idempotency_key=body.idempotency_key,
    )


@router.post("/operations/{job_id}/pause", response_model=OperationResponse)
async def pause_operation(
    admin: CurrentAdminDep,
    job_id: str,
    service: LibraryOperationServiceDep,
    body: OperationControlRequest = MsgSpecBody(OperationControlRequest),
) -> OperationResponse:
    return await _control_operation(admin, job_id, "pause", service, body)


@router.post("/operations/{job_id}/resume", response_model=OperationResponse)
async def resume_operation(
    admin: CurrentAdminDep,
    job_id: str,
    service: LibraryOperationServiceDep,
    body: OperationControlRequest = MsgSpecBody(OperationControlRequest),
) -> OperationResponse:
    return await _control_operation(admin, job_id, "resume", service, body)


@router.post("/operations/{job_id}/stop", response_model=OperationResponse)
async def stop_operation(
    admin: CurrentAdminDep,
    job_id: str,
    service: LibraryOperationServiceDep,
    body: OperationControlRequest = MsgSpecBody(OperationControlRequest),
) -> OperationResponse:
    return await _control_operation(admin, job_id, "stop", service, body)


@router.post("/albums/{album_id}/reidentify", response_model=OperationResponse)
async def reidentify_album(
    admin: CurrentAdminDep,
    album_id: str,
    service: TargetReidentificationServiceDep,
    body: ReidentificationRequest = MsgSpecBody(ReidentificationRequest),
) -> OperationResponse:
    row = await service.create_or_coalesce(
        album_id,
        admin.id,
        expected_album_revision=body.expected_album_revision,
        expected_input_revision=body.expected_input_revision,
        one_off_local_metadata=body.one_off_local_metadata,
        idempotency_key=body.idempotency_key,
    )
    return OperationResponse(
        **{key: row[key] for key in OperationResponse.__struct_fields__ if key in row}
    )


@router.post(
    "/operations/{job_id}/candidate",
    response_model=OperationResponse,
)
async def select_reidentification_candidate(
    admin: CurrentAdminDep,
    job_id: str,
    worker: ExplicitReidentificationWorkerDep,
    body: ReidentificationCandidateRequest = MsgSpecBody(
        ReidentificationCandidateRequest
    ),
) -> OperationResponse:
    row = await worker.select_candidate(
        job_id,
        expected_job_revision=body.expected_row_revision,
        candidate_key=body.candidate_key,
        confirmation=body.confirmation,
        actor_user_id=admin.id,
    )
    return worker.response(row)


async def _preview_membership(
    _: CurrentAdminDep,
    kind: str,
    service: CatalogCorrectionServiceDep,
    body: MembershipPreviewRequest = MsgSpecBody(MembershipPreviewRequest),
) -> MembershipPreviewResponse:
    return await service.preview_membership(kind, body)


async def _apply_membership(
    admin: CurrentAdminDep,
    kind: str,
    service: CatalogCorrectionServiceDep,
    body: MembershipApplyRequest = MsgSpecBody(MembershipApplyRequest),
) -> CatalogCorrectionResponse:
    return CatalogCorrectionResponse(
        **(await service.apply_membership(kind, body, admin.id))
    )


@router.post(
    "/albums/{album_id}/split-preview", response_model=MembershipPreviewResponse
)
async def preview_album_split(
    admin: CurrentAdminDep,
    album_id: str,
    service: CatalogCorrectionServiceDep,
    body: MembershipPreviewRequest = MsgSpecBody(MembershipPreviewRequest),
) -> MembershipPreviewResponse:
    if album_id not in body.expected_album_revisions:
        raise ValidationError(
            "The split preview must include the selected album revision."
        )
    return await _preview_membership(admin, "split", service, body)


@router.post("/albums/{album_id}/split", response_model=CatalogCorrectionResponse)
async def apply_album_split(
    admin: CurrentAdminDep,
    album_id: str,
    service: CatalogCorrectionServiceDep,
    body: MembershipApplyRequest = MsgSpecBody(MembershipApplyRequest),
) -> CatalogCorrectionResponse:
    if album_id not in body.expected_album_revisions:
        raise ValidationError("The split must include the selected album revision.")
    return await _apply_membership(admin, "split", service, body)


@router.post("/albums/merge-preview", response_model=MembershipPreviewResponse)
async def preview_album_merge(
    admin: CurrentAdminDep,
    service: CatalogCorrectionServiceDep,
    body: MembershipPreviewRequest = MsgSpecBody(MembershipPreviewRequest),
) -> MembershipPreviewResponse:
    return await _preview_membership(admin, "merge", service, body)


@router.post("/albums/merge", response_model=CatalogCorrectionResponse)
async def apply_album_merge(
    admin: CurrentAdminDep,
    service: CatalogCorrectionServiceDep,
    body: MembershipApplyRequest = MsgSpecBody(MembershipApplyRequest),
) -> CatalogCorrectionResponse:
    return await _apply_membership(admin, "merge", service, body)


@router.post("/tracks/move-preview", response_model=MembershipPreviewResponse)
async def preview_track_move(
    admin: CurrentAdminDep,
    service: CatalogCorrectionServiceDep,
    body: MembershipPreviewRequest = MsgSpecBody(MembershipPreviewRequest),
) -> MembershipPreviewResponse:
    return await _preview_membership(admin, "move", service, body)


@router.post("/tracks/move", response_model=CatalogCorrectionResponse)
async def apply_track_move(
    admin: CurrentAdminDep,
    service: CatalogCorrectionServiceDep,
    body: MembershipApplyRequest = MsgSpecBody(MembershipApplyRequest),
) -> CatalogCorrectionResponse:
    return await _apply_membership(admin, "move", service, body)


@router.post(
    "/albums/{album_id}/reset-grouping-preview",
    response_model=MembershipPreviewResponse,
)
async def preview_album_grouping_reset(
    admin: CurrentAdminDep,
    album_id: str,
    service: CatalogCorrectionServiceDep,
    body: MembershipPreviewRequest = MsgSpecBody(MembershipPreviewRequest),
) -> MembershipPreviewResponse:
    if album_id not in body.expected_album_revisions:
        raise ValidationError(
            "The reset preview must include the selected album revision."
        )
    return await _preview_membership(admin, "reset", service, body)


@router.post(
    "/albums/{album_id}/reset-grouping", response_model=CatalogCorrectionResponse
)
async def reset_album_grouping(
    admin: CurrentAdminDep,
    album_id: str,
    service: CatalogCorrectionServiceDep,
    body: MembershipApplyRequest = MsgSpecBody(MembershipApplyRequest),
) -> CatalogCorrectionResponse:
    if album_id not in body.expected_album_revisions:
        raise ValidationError("The reset must include the selected album revision.")
    return await _apply_membership(admin, "reset", service, body)


@router.post("/artists/merge-preview", response_model=MembershipPreviewResponse)
async def preview_artist_merge(
    _: CurrentAdminDep,
    service: CatalogCorrectionServiceDep,
    body: ArtistMergePreviewRequest = MsgSpecBody(ArtistMergePreviewRequest),
) -> MembershipPreviewResponse:
    return await service.preview_artist_merge(body)


@router.post("/artists/merge", response_model=CatalogCorrectionResponse)
async def apply_artist_merge(
    admin: CurrentAdminDep,
    service: CatalogCorrectionServiceDep,
    body: ArtistMergeApplyRequest = MsgSpecBody(ArtistMergeApplyRequest),
) -> CatalogCorrectionResponse:
    return CatalogCorrectionResponse(
        **(await service.apply_artist_merge(body, admin.id))
    )


@router.post("/identity-repairs", response_model=OperationResponse)
async def create_repair(
    admin: CurrentAdminDep,
    service: IdentityRepairServiceDep,
    body: RepairCreateRequest = MsgSpecBody(RepairCreateRequest),
) -> OperationResponse:
    return await service.create(body, admin.id)


@router.get("/identity-repairs", response_model=OperationListResponse)
async def list_repairs(
    _: CurrentAdminDep,
    service: IdentityRepairServiceDep,
    limit: int = Query(50, ge=1, le=50),
    cursor: str | None = None,
) -> OperationListResponse:
    return await service.history(purpose="existing_matches", limit=limit, cursor=cursor)


@router.get("/identity-repairs/estimate", response_model=RepairEstimateResponse)
async def estimate_repair(
    _: CurrentAdminDep,
    service: IdentityRepairServiceDep,
    root_id: list[str] = Query(default_factory=list),
) -> RepairEstimateResponse:
    return await service.estimate(root_id)


@router.get("/identity-repairs/{job_id}", response_model=OperationResponse)
async def get_repair(
    _: CurrentAdminDep, job_id: str, service: IdentityRepairServiceDep
) -> OperationResponse:
    return await service.get_for_purpose(job_id, "existing_matches")


@router.get(
    "/identity-repairs/{job_id}/findings",
    response_model=RepairFindingListResponse,
)
async def repair_findings(
    _: CurrentAdminDep,
    job_id: str,
    service: IdentityRepairServiceDep,
    limit: int = Query(100, ge=1, le=200),
    cursor: str | None = None,
    finding_category: str | None = None,
) -> RepairFindingListResponse:
    return await service.findings(
        job_id,
        limit=limit,
        cursor=cursor,
        finding_category=finding_category,
    )


@router.post("/identity-repairs/{job_id}/apply", response_model=OperationResponse)
async def apply_repair(
    _: CurrentAdminDep,
    job_id: str,
    service: IdentityRepairServiceDep,
    body: RepairApplyRequest = MsgSpecBody(RepairApplyRequest),
) -> OperationResponse:
    return await service.begin_apply(
        job_id,
        expected_row_revision=body.expected_row_revision,
        confirmation=body.confirmation,
    )


@router.post("/identity-repairs/{job_id}/pause", response_model=OperationResponse)
async def pause_repair(
    admin: CurrentAdminDep,
    job_id: str,
    service: LibraryOperationServiceDep,
    body: OperationControlRequest = MsgSpecBody(OperationControlRequest),
) -> OperationResponse:
    return await _control_operation(admin, job_id, "pause", service, body)


@router.post("/identity-repairs/{job_id}/resume", response_model=OperationResponse)
async def resume_repair(
    admin: CurrentAdminDep,
    job_id: str,
    service: LibraryOperationServiceDep,
    body: OperationControlRequest = MsgSpecBody(OperationControlRequest),
) -> OperationResponse:
    return await _control_operation(admin, job_id, "resume", service, body)


@router.post("/identity-repairs/{job_id}/stop", response_model=OperationResponse)
async def stop_repair(
    admin: CurrentAdminDep,
    job_id: str,
    service: LibraryOperationServiceDep,
    body: OperationControlRequest = MsgSpecBody(OperationControlRequest),
) -> OperationResponse:
    return await _control_operation(admin, job_id, "stop", service, body)


@router.post("/management/identity-preparations", response_model=OperationResponse)
async def create_management_identity_preparation(
    admin: CurrentAdminDep,
    service: IdentityRepairServiceDep,
    body: IdentityPreparationCreateRequest = MsgSpecBody(
        IdentityPreparationCreateRequest
    ),
) -> OperationResponse:
    return await service.create_management_preparation(body, admin.id)


@router.get("/management/identity-preparations", response_model=OperationListResponse)
async def list_management_identity_preparations(
    _: CurrentAdminDep,
    service: IdentityRepairServiceDep,
    limit: int = Query(20, ge=1, le=50),
    cursor: str | None = None,
) -> OperationListResponse:
    return await service.history(
        purpose="management_readiness", limit=limit, cursor=cursor
    )


@router.get(
    "/management/identity-preparations/estimate",
    response_model=IdentityPreparationEstimateResponse,
)
async def estimate_management_identity_preparation(
    _: CurrentAdminDep,
    service: IdentityRepairServiceDep,
    root_id: list[str] = Query(default_factory=list),
) -> IdentityPreparationEstimateResponse:
    return await service.estimate_management_preparation(root_id)


@router.get(
    "/management/identity-preparations/{job_id}",
    response_model=OperationResponse,
)
async def get_management_identity_preparation(
    _: CurrentAdminDep,
    job_id: str,
    service: IdentityRepairServiceDep,
) -> OperationResponse:
    return await service.get_for_purpose(job_id, "management_readiness")


@router.get(
    "/management/identity-preparations/{job_id}/findings",
    response_model=RepairFindingListResponse,
)
async def management_identity_preparation_findings(
    _: CurrentAdminDep,
    job_id: str,
    service: IdentityRepairServiceDep,
    limit: int = Query(100, ge=1, le=200),
    cursor: str | None = None,
    finding_category: str | None = None,
) -> RepairFindingListResponse:
    await service.get_for_purpose(job_id, "management_readiness")
    return await service.findings(
        job_id,
        limit=limit,
        cursor=cursor,
        finding_category=finding_category,
    )


@router.post(
    "/management/identity-preparations/{job_id}/apply",
    response_model=OperationResponse,
)
async def apply_management_identity_preparation(
    _: CurrentAdminDep,
    job_id: str,
    service: IdentityRepairServiceDep,
    body: RepairApplyRequest = MsgSpecBody(RepairApplyRequest),
) -> OperationResponse:
    return await service.begin_management_preparation_apply(
        job_id,
        expected_row_revision=body.expected_row_revision,
        confirmation=body.confirmation,
    )


@router.post(
    "/management/identity-preparations/{job_id}/discard",
    response_model=OperationResponse,
)
async def discard_management_identity_preparation(
    _: CurrentAdminDep,
    job_id: str,
    service: IdentityRepairServiceDep,
    body: OperationControlRequest = MsgSpecBody(OperationControlRequest),
) -> OperationResponse:
    return await service.discard_management_preparation(
        job_id,
        expected_row_revision=body.expected_row_revision,
    )


@router.get("/scan-runs/{run_id}/diagnostics")
async def scan_diagnostics(
    _: CurrentAdminDep,
    run_id: str,
    service: LibraryDiagnosticsServiceDep,
) -> StreamingResponse:
    filename, content = await service.export(run_id)

    async def chunks() -> AsyncIterator[bytes]:
        for offset in range(0, len(content), 64 * 1024):
            yield content[offset : offset + 64 * 1024]

    return StreamingResponse(
        chunks(),
        media_type="application/json",
        headers={
            "Content-Disposition": f'attachment; filename="{filename}"',
            "X-Content-Type-Options": "nosniff",
            "Cache-Control": "no-store",
        },
    )
