import msgspec.structs
from fastapi import APIRouter, Depends, Request
from api.v1.schemas.request import (
    AlbumRequest,
    BatchAlbumRequest,
    BatchCancelRequest,
    BatchCancelResponse,
    BatchRequestResponse,
    QueueStatusResponse,
    RequestAcceptedResponse,
)
from core.dependencies import get_request_service
from core.dependencies.type_aliases import CurrentUserDep
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from services.request_service import RequestService

router = APIRouter(route_class=MsgSpecRoute, prefix="/requests", tags=["requests"])


@router.post("/new", response_model=RequestAcceptedResponse, status_code=202)
async def request_album(
    current_user: CurrentUserDep,
    album_request: AlbumRequest = MsgSpecBody(AlbumRequest),
    request_service: RequestService = Depends(get_request_service),
):
    return await request_service.request_album(
        album_request.musicbrainz_id,
        artist=album_request.artist,
        album=album_request.album,
        year=album_request.year,
        artist_mbid=album_request.artist_mbid,
        monitor_artist=album_request.monitor_artist,
        auto_download_artist=album_request.auto_download_artist,
        user_id=current_user.id,
        user_role=current_user.role,
        requested_by_name=current_user.display_name,
    )


@router.post("/batch", response_model=BatchRequestResponse, status_code=202)
async def request_batch(
    current_user: CurrentUserDep,
    batch: BatchAlbumRequest = MsgSpecBody(BatchAlbumRequest),
    request_service: RequestService = Depends(get_request_service),
):
    return await request_service.request_batch(
        items=[msgspec.structs.asdict(item) for item in batch.items],
        monitor_artist=batch.monitor_artist,
        auto_download_artist=batch.auto_download_artist,
        user_id=current_user.id,
        user_role=current_user.role,
        requested_by_name=current_user.display_name,
    )


@router.post("/batch/cancel", response_model=BatchCancelResponse)
async def cancel_batch(
    current_user: CurrentUserDep,
    body: BatchCancelRequest = MsgSpecBody(BatchCancelRequest),
    request_service: RequestService = Depends(get_request_service),
):
    user_id = None if current_user.role == "admin" else current_user.id
    return await request_service.cancel_batch(body.musicbrainz_ids, user_id=user_id)


@router.get("/new/queue-status", response_model=QueueStatusResponse)
async def get_queue_status(
    request_service: RequestService = Depends(get_request_service)
):
    return request_service.get_queue_status()
