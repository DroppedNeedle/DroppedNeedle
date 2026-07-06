"""User-scoped download search routes (Phase 6): start a search, view the job,
pick a candidate, cancel, and stream live progress via SSE.

Search jobs are owned by the initiating user; pick/cancel/view/stream verify
``job.user_id == current_user.id`` in the service layer (-> 403 via the
registered ``PermissionDeniedError`` handler).
"""

import asyncio
import logging

import msgspec
from fastapi import APIRouter, Depends, Query
from fastapi.responses import StreamingResponse

from api.v1.schemas.common import StatusMessageResponse
from api.v1.schemas.download import (
    DismissReviewResponse,
    PickRequest,
    PickResponse,
    SearchAlbumRequest,
    SearchAlbumResponse,
    SearchJobResponse,
)
from core.dependencies import (
    get_download_service,
    get_sse_publisher,
    get_wanted_watcher_service,
)
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentUserDep
from services.native.download_service import ALREADY_IN_LIBRARY

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/downloads", tags=["downloads"])

_SSE_HEADERS = {
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "X-Accel-Buffering": "no",
}


@router.post("/search/album", response_model=SearchAlbumResponse)
async def search_album(
    current_user: CurrentUserDep,
    body: SearchAlbumRequest = MsgSpecBody(SearchAlbumRequest),
    service=Depends(get_download_service),
):
    job_id = await service.search_album(
        current_user.id,
        body.artist_name,
        body.album_title,
        body.year,
        body.track_count,
        body.release_group_mbid,
    )
    if job_id == ALREADY_IN_LIBRARY:
        return SearchAlbumResponse(status="already_in_library", job_id=None)
    return SearchAlbumResponse(status="searching", job_id=job_id)


@router.get("/search/stream")
async def search_stream(
    current_user: CurrentUserDep,
    job_id: str = Query(...),
    service=Depends(get_download_service),
    publisher=Depends(get_sse_publisher),
):
    # this literal /search/stream route MUST be declared before /search/{job_id}
    # below, or Starlette captures "stream" as job_id and SSE 404s
    # ownership guard before streaming (raises 404/403 via registered handlers)
    await service.get_search_job(current_user.id, job_id)

    async def event_generator():
        try:
            async for message in publisher.subscribe(f"search:{job_id}"):
                if not message["event"]:
                    yield ": keepalive\n\n"
                    continue
                payload = msgspec.json.encode(message["data"]).decode("utf-8")
                yield f"event: {message['event']}\ndata: {payload}\n\n"
        except asyncio.CancelledError:
            pass

    return StreamingResponse(
        event_generator(), media_type="text/event-stream", headers=_SSE_HEADERS
    )


@router.get("/search/{job_id}", response_model=SearchJobResponse)
async def get_search_job(
    job_id: str, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    job, candidates = await service.get_search_job(current_user.id, job_id)
    return SearchJobResponse(
        job_id=job.id,
        status=job.status,
        artist_name=job.artist_name,
        album_title=job.album_title,
        candidate_count=len(candidates),
        top_score=candidates[0].final_score if candidates else None,
        candidates=candidates,
    )


@router.post("/search/{job_id}/pick", response_model=PickResponse)
async def pick_candidate(
    job_id: str,
    current_user: CurrentUserDep,
    body: PickRequest = MsgSpecBody(PickRequest),
    service=Depends(get_download_service),
):
    task_id = await service.pick_candidate(current_user.id, job_id, body.candidate_index)
    return PickResponse(task_id=task_id)


@router.post("/search/{job_id}/dismiss", response_model=DismissReviewResponse)
async def dismiss_review(
    job_id: str,
    current_user: CurrentUserDep,
    watcher=Depends(get_wanted_watcher_service),
):
    """'None of these - keep watching': reject every parked candidate, cancel the
    attempt, and put the album on the wanted watchlist (candidates recorded as
    seen so they never badge again)."""
    watch = await watcher.dismiss_review(job_id, current_user.id, current_user.role)
    return DismissReviewResponse(success=True, state=watch.state)


@router.post("/search/{job_id}/cancel", response_model=StatusMessageResponse)
async def cancel_search(
    job_id: str, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    await service.cancel_search(current_user.id, job_id)
    return StatusMessageResponse(status="ok", message="Search cancelled")
