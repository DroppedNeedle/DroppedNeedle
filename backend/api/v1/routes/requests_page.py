from fastapi import APIRouter, Depends, HTTPException, Query
from typing import Optional

from api.v1.schemas.requests_page import (
    ActiveCountResponse,
    ActiveRequestsResponse,
    ApprovalActionResponse,
    ApprovalBatchItem,
    ApprovalBatchListResponse,
    AutoDownloadApprovalItem,
    AutoDownloadApprovalsResponse,
    CancelRequestResponse,
    ClearHistoryResponse,
    PersonalMixApprovalItem,
    PersonalMixApprovalsResponse,
    RequestHistoryResponse,
    RetryRequestResponse,
    WantedActionResponse,
    WantedRetryingItem,
    WantedWatchesResponse,
    WantedWatchItem,
)
from core.dependencies import (
    get_auth_store,
    get_follow_service,
    get_personal_mix_service,
    get_requests_page_service,
    get_wanted_watcher_service,
)
from core.dependencies.type_aliases import CurrentUserDep, CurrentAdminDep
from infrastructure.cover_urls import prefer_release_group_cover_url
from infrastructure.validators import validate_mbid
from infrastructure.msgspec_fastapi import MsgSpecRoute
from models.wanted import WantedRetrying, WantedWatch
from services.follow_service import FollowService
from services.native.wanted_watcher_service import WantedWatcherService
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


def _wanted_item(watch: WantedWatch, user_name: str | None = None) -> WantedWatchItem:
    return WantedWatchItem(
        release_group_mbid=watch.release_group_mbid,
        artist_name=watch.artist_name,
        album_title=watch.album_title,
        kind=watch.kind,
        state=watch.state,
        check_count=watch.check_count,
        next_check_at=watch.next_check_at,
        new_candidate_count=watch.new_candidate_count,
        created_at=watch.created_at,
        artist_mbid=watch.artist_mbid,
        year=watch.year,
        cover_url=prefer_release_group_cover_url(
            watch.release_group_mbid, watch.cover_url, size=500
        ),
        first_release_date=watch.first_release_date,
        last_checked_at=watch.last_checked_at,
        last_outcome=watch.last_outcome,
        user_id=watch.user_id,
        user_name=user_name,
    )


def _validated_album_mbid(musicbrainz_id: str) -> str:
    try:
        return validate_mbid(musicbrainz_id, "album")
    except ValueError:
        raise HTTPException(status_code=400, detail="Invalid MBID format")


def _retrying_item(entry: WantedRetrying, user_name: str | None = None) -> WantedRetryingItem:
    return WantedRetryingItem(
        release_group_mbid=entry.release_group_mbid,
        artist_name=entry.artist_name,
        album_title=entry.album_title,
        retry_count=entry.retry_count,
        max_attempts=entry.max_attempts,
        next_retry_at=entry.next_retry_at,
        artist_mbid=entry.artist_mbid,
        year=entry.year,
        cover_url=prefer_release_group_cover_url(
            entry.release_group_mbid, entry.cover_url, size=500
        ),
        user_id=entry.user_id,
        user_name=user_name,
    )


@router.get("/wanted", response_model=WantedWatchesResponse)
async def list_wanted_watches(
    current_user: CurrentUserDep,
    watcher: WantedWatcherService = Depends(get_wanted_watcher_service),
    auth_store=Depends(get_auth_store),
):
    watches = await watcher.list_watches_for(current_user.id, current_user.role)
    retrying = await watcher.list_retrying_for(current_user.id, current_user.role)
    # admins see every user's watches - resolve the "watched by" display names
    names: dict[str, str] = {}
    if current_user.role == "admin" and (watches or retrying):
        try:
            users = await auth_store.list_users(limit=500)
            names = {u.id: u.display_name for u in users}
        except Exception:  # noqa: BLE001 - the chip is cosmetic, never fail the list
            names = {}
    return WantedWatchesResponse(
        items=[_wanted_item(w, names.get(w.user_id)) for w in watches],
        count=len(watches),
        retrying=[_retrying_item(r, names.get(r.user_id)) for r in retrying],
    )


@router.post("/wanted/{musicbrainz_id}/stop", response_model=WantedActionResponse)
async def stop_wanted_watch(
    current_user: CurrentUserDep,
    musicbrainz_id: str,
    watcher: WantedWatcherService = Depends(get_wanted_watcher_service),
):
    musicbrainz_id = _validated_album_mbid(musicbrainz_id)
    watch = await watcher.stop(musicbrainz_id, current_user.id, current_user.role)
    return WantedActionResponse(success=True, state=watch.state)


@router.post("/wanted/{musicbrainz_id}/resume", response_model=WantedActionResponse)
async def resume_wanted_watch(
    current_user: CurrentUserDep,
    musicbrainz_id: str,
    watcher: WantedWatcherService = Depends(get_wanted_watcher_service),
):
    musicbrainz_id = _validated_album_mbid(musicbrainz_id)
    watch = await watcher.resume(musicbrainz_id, current_user.id, current_user.role)
    return WantedActionResponse(success=True, state=watch.state)


@router.post("/wanted/{musicbrainz_id}/seen", response_model=WantedActionResponse)
async def mark_wanted_candidates_seen(
    current_user: CurrentUserDep,
    musicbrainz_id: str,
    watcher: WantedWatcherService = Depends(get_wanted_watcher_service),
):
    musicbrainz_id = _validated_album_mbid(musicbrainz_id)
    watch = await watcher.mark_seen(musicbrainz_id, current_user.id, current_user.role)
    return WantedActionResponse(success=True, state=watch.state)


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
    batches = await follow_service.list_pending_batches()
    personal_mix = await personal_mix_service.list_pending_approvals()
    # Each batch counts as one unit; individual approvals already exclude batched rows
    # (the batch_id IS NULL filter in list_pending_approvals), so nothing is double-counted.
    return ActiveCountResponse(
        count=count + len(auto_download) + len(batches) + len(personal_mix)
    )


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


@router.get(
    "/auto-download-approval-batches", response_model=ApprovalBatchListResponse
)
async def list_auto_download_approval_batches(
    _admin: CurrentAdminDep,
    follow_service: FollowService = Depends(get_follow_service),
):
    batches = await follow_service.list_pending_batches()
    items = [
        ApprovalBatchItem(
            batch_id=b.batch_id,
            user_id=b.user_id,
            user_name=b.user_name,
            artist_count=b.artist_count,
            sample_names=b.sample_names,
            requested_at=b.requested_at,
            source=b.source,
        )
        for b in batches
    ]
    return ApprovalBatchListResponse(batches=items, count=len(items))


@router.post(
    "/auto-download-approval-batches/{batch_id}/approve",
    response_model=ApprovalActionResponse,
)
async def approve_auto_download_batch(
    admin: CurrentAdminDep,
    batch_id: str,
    follow_service: FollowService = Depends(get_follow_service),
):
    affected = await follow_service.approve_batch(
        batch_id, (admin.id, admin.display_name)
    )
    if not affected:
        return ApprovalActionResponse(success=False, message="No matching batch found")
    return ApprovalActionResponse(
        success=True, message=f"Auto-download approved for {affected} artists"
    )


@router.post(
    "/auto-download-approval-batches/{batch_id}/reject",
    response_model=ApprovalActionResponse,
)
async def reject_auto_download_batch(
    admin: CurrentAdminDep,
    batch_id: str,
    follow_service: FollowService = Depends(get_follow_service),
):
    affected = await follow_service.reject_batch(
        batch_id, (admin.id, admin.display_name)
    )
    if not affected:
        return ApprovalActionResponse(success=False, message="No matching batch found")
    return ApprovalActionResponse(
        success=True, message=f"Auto-download rejected for {affected} artists"
    )


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
