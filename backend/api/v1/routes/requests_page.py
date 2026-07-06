from fastapi import APIRouter, Depends, HTTPException, Query
from typing import Optional

from api.v1.schemas.requests_page import (
    ActiveCountResponse,
    ActiveRequestsResponse,
    ApprovalActionResponse,
    AutoDownloadApprovalItem,
    AutoDownloadApprovalsResponse,
    CancelRequestResponse,
    ClearHistoryResponse,
    PersonalMixApprovalItem,
    PersonalMixApprovalsResponse,
    RequestHistoryResponse,
    RetryRequestResponse,
)
from core.dependencies import (
    get_follow_service,
    get_personal_mix_service,
    get_requests_page_service,
)
from core.dependencies.type_aliases import CurrentUserDep, CurrentAdminDep
from infrastructure.validators import validate_mbid
from infrastructure.msgspec_fastapi import MsgSpecRoute
from services.follow_service import FollowService
from services.personal_mix_service import PersonalMixService
from services.requests_page_service import RequestsPageService

router = APIRouter(route_class=MsgSpecRoute, prefix="/requests", tags=["requests-page"])


@router.get("/active", response_model=ActiveRequestsResponse)
async def get_active_requests(
    current_user: CurrentUserDep,
    service: RequestsPageService = Depends(get_requests_page_service),
):
    user_id = None if current_user.role == "admin" else current_user.id
    return await service.get_active_requests(user_id=user_id)


@router.get("/active/count", response_model=ActiveCountResponse)
async def get_active_request_count(
    current_user: CurrentUserDep,
    service: RequestsPageService = Depends(get_requests_page_service),
):
    user_id = None if current_user.role == "admin" else current_user.id
    count = await service.get_active_count(user_id=user_id)
    return ActiveCountResponse(count=count)


@router.get("/history", response_model=RequestHistoryResponse)
async def get_request_history(
    current_user: CurrentUserDep,
    page: int = Query(1, ge=1),
    page_size: int = Query(20, ge=1, le=100),
    status: Optional[str] = Query(None),
    sort: Optional[str] = Query(None, pattern="^(newest|oldest|status)$"),
    service: RequestsPageService = Depends(get_requests_page_service),
):
    user_id = None if current_user.role == "admin" else current_user.id
    return await service.get_request_history(
        page=page, page_size=page_size, status_filter=status, sort=sort, user_id=user_id
    )


@router.delete("/active/{musicbrainz_id}", response_model=CancelRequestResponse)
async def cancel_request(
    current_user: CurrentUserDep,
    musicbrainz_id: str,
    service: RequestsPageService = Depends(get_requests_page_service),
):
    try:
        musicbrainz_id = validate_mbid(musicbrainz_id, "album")
    except ValueError:
        raise HTTPException(status_code=400, detail="Invalid MBID format")
    return await service.cancel_request(
        musicbrainz_id, user_id=current_user.id, user_role=current_user.role
    )


@router.post("/retry/{musicbrainz_id}", response_model=RetryRequestResponse)
async def retry_request(
    current_user: CurrentUserDep,
    musicbrainz_id: str,
    service: RequestsPageService = Depends(get_requests_page_service),
):
    try:
        musicbrainz_id = validate_mbid(musicbrainz_id, "album")
    except ValueError:
        raise HTTPException(status_code=400, detail="Invalid MBID format")
    return await service.retry_request(
        musicbrainz_id, user_id=current_user.id, user_role=current_user.role
    )


@router.delete("/history/{musicbrainz_id}", response_model=ClearHistoryResponse)
async def clear_history_item(
    current_user: CurrentUserDep,
    musicbrainz_id: str,
    service: RequestsPageService = Depends(get_requests_page_service),
):
    try:
        musicbrainz_id = validate_mbid(musicbrainz_id, "album")
    except ValueError:
        raise HTTPException(status_code=400, detail="Invalid MBID format")
    deleted = await service.clear_history_item(
        musicbrainz_id, user_id=current_user.id, user_role=current_user.role
    )
    return ClearHistoryResponse(success=deleted)


@router.get("/pending-approvals", response_model=ActiveRequestsResponse)
async def get_pending_approvals(
    _admin: CurrentAdminDep,
    service: RequestsPageService = Depends(get_requests_page_service),
):
    return await service.get_pending_approvals()


@router.get("/pending-approvals/count", response_model=ActiveCountResponse)
async def get_pending_approval_count(
    _admin: CurrentAdminDep,
    service: RequestsPageService = Depends(get_requests_page_service),
    follow_service: FollowService = Depends(get_follow_service),
    personal_mix_service: PersonalMixService = Depends(get_personal_mix_service),
):
    count = await service.get_pending_approval_count()
    auto_download = await follow_service.list_pending_approvals()
    personal_mix = await personal_mix_service.list_pending_approvals()
    return ActiveCountResponse(count=count + len(auto_download) + len(personal_mix))


@router.post("/approve/{musicbrainz_id}", response_model=ApprovalActionResponse)
async def approve_request(
    admin: CurrentAdminDep,
    musicbrainz_id: str,
    service: RequestsPageService = Depends(get_requests_page_service),
):
    try:
        musicbrainz_id = validate_mbid(musicbrainz_id, "album")
    except ValueError:
        raise HTTPException(status_code=400, detail="Invalid MBID format")
    result = await service.approve_request(musicbrainz_id, admin.id, admin.display_name)
    return ApprovalActionResponse(success=result.success, message=result.message)


@router.post("/reject/{musicbrainz_id}", response_model=ApprovalActionResponse)
async def reject_request(
    admin: CurrentAdminDep,
    musicbrainz_id: str,
    service: RequestsPageService = Depends(get_requests_page_service),
):
    try:
        musicbrainz_id = validate_mbid(musicbrainz_id, "album")
    except ValueError:
        raise HTTPException(status_code=400, detail="Invalid MBID format")
    result = await service.reject_request(musicbrainz_id, admin.id, admin.display_name)
    return ApprovalActionResponse(success=result.success, message=result.message)


@router.get("/auto-download-approvals", response_model=AutoDownloadApprovalsResponse)
async def list_auto_download_approvals(
    _admin: CurrentAdminDep,
    follow_service: FollowService = Depends(get_follow_service),
):
    approvals = await follow_service.list_pending_approvals()
    items = [
        AutoDownloadApprovalItem(
            user_id=a.user_id,
            user_name=a.user_name,
            artist_mbid=a.artist_mbid,
            artist_name=a.artist_name,
            requested_at=a.requested_at,
        )
        for a in approvals
    ]
    return AutoDownloadApprovalsResponse(items=items, count=len(items))


def _validate_artist_mbid(artist_mbid: str) -> None:
    try:
        validate_mbid(artist_mbid)
    except ValueError:
        raise HTTPException(status_code=400, detail="Invalid MBID format")


@router.post(
    "/auto-download-approvals/{user_id}/{artist_mbid}/approve",
    response_model=ApprovalActionResponse,
)
async def approve_auto_download(
    admin: CurrentAdminDep,
    user_id: str,
    artist_mbid: str,
    follow_service: FollowService = Depends(get_follow_service),
):
    _validate_artist_mbid(artist_mbid)
    ok = await follow_service.approve(user_id, artist_mbid, (admin.id, admin.display_name))
    if not ok:
        return ApprovalActionResponse(success=False, message="No matching approval found")
    return ApprovalActionResponse(success=True, message="Auto-download approved")


@router.post(
    "/auto-download-approvals/{user_id}/{artist_mbid}/reject",
    response_model=ApprovalActionResponse,
)
async def reject_auto_download(
    admin: CurrentAdminDep,
    user_id: str,
    artist_mbid: str,
    follow_service: FollowService = Depends(get_follow_service),
):
    _validate_artist_mbid(artist_mbid)
    ok = await follow_service.reject(user_id, artist_mbid, (admin.id, admin.display_name))
    if not ok:
        return ApprovalActionResponse(success=False, message="No matching approval found")
    return ApprovalActionResponse(success=True, message="Auto-download rejected")


@router.post(
    "/auto-download-approvals/{user_id}/{artist_mbid}/revoke",
    response_model=ApprovalActionResponse,
)
async def revoke_auto_download(
    admin: CurrentAdminDep,
    user_id: str,
    artist_mbid: str,
    follow_service: FollowService = Depends(get_follow_service),
):
    _validate_artist_mbid(artist_mbid)
    ok = await follow_service.revoke(user_id, artist_mbid, (admin.id, admin.display_name))
    if not ok:
        return ApprovalActionResponse(success=False, message="No matching approval found")
    return ApprovalActionResponse(success=True, message="Auto-download revoked")


@router.get("/personal-mix-approvals", response_model=PersonalMixApprovalsResponse)
async def list_personal_mix_approvals(
    _admin: CurrentAdminDep,
    personal_mix_service: PersonalMixService = Depends(get_personal_mix_service),
):
    approvals = await personal_mix_service.list_pending_approvals()
    items = [
        PersonalMixApprovalItem(
            user_id=a.user_id,
            user_name=a.user_name,
            requested_at=a.requested_at,
        )
        for a in approvals
    ]
    return PersonalMixApprovalsResponse(items=items, count=len(items))


@router.post(
    "/personal-mix-approvals/{user_id}/approve",
    response_model=ApprovalActionResponse,
)
async def approve_personal_mix_auto_request(
    admin: CurrentAdminDep,
    user_id: str,
    personal_mix_service: PersonalMixService = Depends(get_personal_mix_service),
):
    ok = await personal_mix_service.approve_auto_request(user_id, (admin.id, admin.display_name))
    if not ok:
        return ApprovalActionResponse(success=False, message="No matching approval found")
    return ApprovalActionResponse(success=True, message="Weekly Mix auto-request approved")


@router.post(
    "/personal-mix-approvals/{user_id}/reject",
    response_model=ApprovalActionResponse,
)
async def reject_personal_mix_auto_request(
    admin: CurrentAdminDep,
    user_id: str,
    personal_mix_service: PersonalMixService = Depends(get_personal_mix_service),
):
    ok = await personal_mix_service.reject_auto_request(user_id, (admin.id, admin.display_name))
    if not ok:
        return ApprovalActionResponse(success=False, message="No matching approval found")
    return ApprovalActionResponse(success=True, message="Weekly Mix auto-request rejected")


@router.post(
    "/personal-mix-approvals/{user_id}/revoke",
    response_model=ApprovalActionResponse,
)
async def revoke_personal_mix_auto_request(
    admin: CurrentAdminDep,
    user_id: str,
    personal_mix_service: PersonalMixService = Depends(get_personal_mix_service),
):
    ok = await personal_mix_service.revoke_auto_request(user_id, (admin.id, admin.display_name))
    if not ok:
        return ApprovalActionResponse(success=False, message="No matching approval found")
    return ApprovalActionResponse(success=True, message="Weekly Mix auto-request revoked")
