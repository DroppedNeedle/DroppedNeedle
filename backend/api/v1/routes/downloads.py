"""User-scoped download-task routes (Phase 7): list the queue, view a task, stream
live progress (SSE), cancel, retry, and list a task's files.

All routes are authenticated. Listing is user-scoped (admins see all tasks);
view/stream/cancel/retry/files verify ``task.user_id == current_user.id`` (or admin)
in the service/orchestrator layer (-> 403/404 via the registered handlers). These
live alongside the Phase-6 search routes under the same ``/downloads`` prefix; the
literal ``/search/*`` routes are registered first so they take precedence over the
``/{task_id}`` patterns here.
"""

import asyncio
import logging

import msgspec
from fastapi import APIRouter, Depends, Query
from fastapi.responses import StreamingResponse

from api.v1.schemas.download import (
    CancelDownloadResponse,
    DownloadFileItem,
    DownloadFilesResponse,
    DownloadListResponse,
    DownloadTaskResponse,
    RetryDownloadResponse,
)
from core.dependencies import get_download_service, get_sse_publisher
from infrastructure.msgspec_fastapi import MsgSpecRoute
from middleware import CurrentUserDep

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/downloads", tags=["downloads"])

_SSE_HEADERS = {
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "X-Accel-Buffering": "no",
}


def _to_response(  # noqa: ANN001 - DownloadTask
    task, *, next_retry_at: float | None = None, retry_max: int = 0
) -> DownloadTaskResponse:
    return DownloadTaskResponse(
        id=task.id,
        user_id=task.user_id,
        download_type=task.download_type,
        release_group_mbid=task.release_group_mbid,
        recording_mbid=task.recording_mbid,
        artist_name=task.artist_name,
        album_title=task.album_title,
        track_title=task.track_title,
        year=task.year,
        status=task.status,
        progress_percent=task.progress_percent,
        total_size_bytes=task.total_size_bytes,
        downloaded_bytes=task.downloaded_bytes,
        files_total=task.files_total,
        files_completed=task.files_completed,
        files_failed=task.files_failed,
        source_username=task.source_username,
        search_job_id=task.search_job_id,
        candidate_index=task.candidate_index,
        preflight_score=task.preflight_score,
        final_path=task.final_path,
        error_message=task.error_message,
        retry_count=task.retry_count,
        created_at=task.created_at,
        updated_at=task.updated_at,
        next_retry_at=next_retry_at,
        retry_max=retry_max,
    )


@router.get("", response_model=DownloadListResponse)
async def list_downloads(
    current_user: CurrentUserDep,
    status: str | None = Query(default=None),
    release_group_mbid: str | None = Query(default=None),
    page: int = Query(default=1, ge=1),
    page_size: int = Query(default=20, ge=1, le=100),
    service=Depends(get_download_service),
):
    tasks = await service.list_tasks(
        current_user.id,
        current_user.role,
        status=status,
        release_group_mbid=release_group_mbid,
        page=page,
        page_size=page_size,
    )
    retry_max = service.auto_retry_max
    return DownloadListResponse(
        items=[
            _to_response(t, next_retry_at=service.next_retry_at(t), retry_max=retry_max)
            for t in tasks
        ],
        page=page,
        page_size=page_size,
    )


@router.get("/{task_id}/stream")
async def stream_download(
    task_id: str,
    current_user: CurrentUserDep,
    service=Depends(get_download_service),
    publisher=Depends(get_sse_publisher),
):
    # Ownership guard before streaming (404/403 via registered handlers).
    await service.get_task(task_id, current_user.id, current_user.role)

    async def event_generator():
        try:
            async for message in publisher.subscribe(f"download:{task_id}"):
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


@router.get("/{task_id}/files", response_model=DownloadFilesResponse)
async def get_download_files(
    task_id: str, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    task, files = await service.get_task_files(task_id, current_user.id, current_user.role)
    return DownloadFilesResponse(
        task_id=task.id,
        status=task.status,
        files_total=task.files_total,
        files_completed=task.files_completed,
        files_failed=task.files_failed,
        progress_percent=task.progress_percent,
        files=[DownloadFileItem(filename=f.filename, size=f.size, duration=f.duration) for f in files],
    )


@router.post("/{task_id}/cancel", response_model=CancelDownloadResponse)
async def cancel_download(
    task_id: str, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    await service.cancel_task(task_id, current_user.id, current_user.role)
    return CancelDownloadResponse(success=True)


@router.post("/{task_id}/retry", response_model=RetryDownloadResponse)
async def retry_download(
    task_id: str, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    new_task_id = await service.retry_task(task_id, current_user.id, current_user.role)
    return RetryDownloadResponse(success=True, task_id=new_task_id)


@router.get("/{task_id}", response_model=DownloadTaskResponse)
async def get_download(
    task_id: str, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    task = await service.get_task(task_id, current_user.id, current_user.role)
    return _to_response(
        task, next_retry_at=service.next_retry_at(task), retry_max=service.auto_retry_max
    )
