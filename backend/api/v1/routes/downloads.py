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
import mimetypes
from pathlib import Path

from fastapi import APIRouter, Depends, Query
from fastapi.responses import FileResponse, StreamingResponse

from core.exceptions import ResourceNotFoundError

from api.v1.schemas.download import (
    CancelDownloadResponse,
    ClearDownloadsResponse,
    CutoffUnmetItem,
    CutoffUnmetResponse,
    DownloadFileItem,
    DownloadFilesResponse,
    DownloadListResponse,
    DownloadTaskResponse,
    HeldActionResponse,
    HeldImportResponse,
    HeldListResponse,
    ReimportDownloadResponse,
    RetryAllResponse,
    RetryDownloadResponse,
    StopRetriesResponse,
    UpgradeAlbumRequestBody,
    UpgradeRequestResponse,
    UpgradeTrackRequestBody,
)
from core.dependencies import get_download_service, get_sse_publisher
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentCuratorDep, CurrentUserDep
from services.native.download_service import ALREADY_IN_LIBRARY

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/downloads", tags=["downloads"])

_SSE_HEADERS = {
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "X-Accel-Buffering": "no",
}


def _to_response(  # noqa: ANN001 - DownloadTask
    task,
    *,
    next_retry_at: float | None = None,
    retry_max: int = 0,
    retry_ladder_minutes: list[int] | None = None,
) -> DownloadTaskResponse:
    return DownloadTaskResponse(
        id=task.id,
        user_id=task.user_id,
        download_type=task.download_type,
        source=task.source,
        release_group_mbid=task.release_group_mbid,
        recording_mbid=task.recording_mbid,
        artist_mbid=task.artist_mbid,
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
        completed_at=task.completed_at,
        next_retry_at=next_retry_at,
        retry_max=retry_max,
        retry_ladder_minutes=retry_ladder_minutes or [],
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
    retry_ladder = service.retry_ladder_minutes()
    # Tasks waiting on a held-track review are paused, not scheduled - don't show a
    # countdown that will never fire.
    held_ids = await service.held_task_ids(current_user.id, current_user.role)
    return DownloadListResponse(
        items=[
            _to_response(
                t,
                next_retry_at=None if t.id in held_ids else service.next_retry_at(t),
                retry_max=retry_max,
                retry_ladder_minutes=retry_ladder,
            )
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


@router.post("/clear", response_model=ClearDownloadsResponse)
async def clear_downloads(
    current_user: CurrentUserDep, service=Depends(get_download_service)
):
    """Permanently remove the user's terminal (completed + cancelled) tasks."""
    cleared = await service.clear_finished(current_user.id, current_user.role)
    return ClearDownloadsResponse(cleared=cleared)


@router.post("/stop-all-retries", response_model=StopRetriesResponse)
async def stop_all_retries(
    current_user: CurrentUserDep, service=Depends(get_download_service)
):
    """Stop the user's still-scheduled auto-retries (the "wanted/retrying" set)."""
    stopped = await service.stop_all_retries(current_user.id, current_user.role)
    return StopRetriesResponse(stopped=stopped)


@router.post("/retry-all-failed", response_model=RetryAllResponse)
async def retry_all_failed(
    current_user: CurrentUserDep, service=Depends(get_download_service)
):
    """Re-dispatch the user's exhausted/non-auto-retrying failures."""
    retried = await service.retry_all_failed(current_user.id, current_user.role)
    return RetryAllResponse(retried=retried)


def _held_to_response(held) -> HeldImportResponse:  # noqa: ANN001 - HeldImport
    return HeldImportResponse(
        id=held.id,
        release_group_mbid=held.release_group_mbid,
        recording_mbid=held.recording_mbid,
        track_number=held.track_number,
        disc_number=held.disc_number,
        track_title=held.track_title,
        artist_name=held.artist_name,
        album_title=held.album_title,
        year=held.year,
        original_filename=held.original_filename,
        file_format=held.file_format,
        duration_seconds=held.duration_seconds,
        reason=held.reason,
        source=held.source,
        source_task_id=held.source_task_id,
        created_at=held.created_at,
        evidence_title=held.evidence_title,
        evidence_artist=held.evidence_artist,
        evidence_score=held.evidence_score,
    )


# --- Quality upgrades (CollectionManagement, admin/trusted D18) ----------------
# NOTE: declared before "/{task_id}" so the static paths win over the param route.


@router.get("/cutoff-unmet", response_model=CutoffUnmetResponse)
async def list_cutoff_unmet(
    current_user: CurrentCuratorDep, service=Depends(get_download_service)
):
    """The upgrade worklist: albums whose worst held tier is below the cutoff.
    Empty while upgrades are off."""
    rows = await service.list_cutoff_unmet()
    return CutoffUnmetResponse(
        items=[
            CutoffUnmetItem(
                release_group_mbid=row["release_group_mbid"],
                current_tier=row["current_tier"],
                track_count=row["track_count"],
                artist_name=row.get("artist_name"),
                artist_mbid=row.get("artist_mbid"),
                album_title=row.get("album_title"),
                year=row.get("year"),
            )
            for row in rows
        ],
        cutoff=service.quality_cutoff,
        upgrade_allowed=service.upgrade_allowed,
    )


@router.post("/upgrade/album", response_model=UpgradeRequestResponse)
async def request_upgrade_album(
    current_user: CurrentCuratorDep,
    body: UpgradeAlbumRequestBody = MsgSpecBody(UpgradeAlbumRequestBody),
    service=Depends(get_download_service),
):
    """Fetch a better copy of a below-cutoff album (origin='upgrade'; strictly-better
    replace happens at import)."""
    task_id = await service.request_upgrade_album(
        user_id=current_user.id,
        release_group_mbid=body.release_group_mbid,
        artist_name=body.artist_name,
        album_title=body.album_title,
        year=body.year,
        artist_mbid=body.artist_mbid,
    )
    if task_id == ALREADY_IN_LIBRARY:
        return UpgradeRequestResponse(status="satisfied")
    return UpgradeRequestResponse(status="queued", task_id=task_id)


@router.post("/upgrade/track", response_model=UpgradeRequestResponse)
async def request_upgrade_track(
    current_user: CurrentCuratorDep,
    body: UpgradeTrackRequestBody = MsgSpecBody(UpgradeTrackRequestBody),
    service=Depends(get_download_service),
):
    """Fetch a better copy of one below-cutoff track (per-recording floor, D12)."""
    task_id = await service.request_upgrade_track(
        user_id=current_user.id,
        recording_mbid=body.recording_mbid,
        artist_name=body.artist_name,
        track_title=body.track_title,
        album_title=body.album_title,
        duration_seconds=body.duration_seconds,
        release_group_mbid=body.release_group_mbid,
        artist_mbid=body.artist_mbid,
    )
    if task_id == ALREADY_IN_LIBRARY:
        return UpgradeRequestResponse(status="satisfied")
    return UpgradeRequestResponse(status="queued", task_id=task_id)


@router.get("/held", response_model=HeldListResponse)
async def list_held(
    current_user: CurrentUserDep,
    release_group_mbid: str | None = Query(default=None),
    service=Depends(get_download_service),
):
    """Tracks held for an 'import anyway' review - all of them, or scoped to one album."""
    held = await service.list_held(current_user.id, current_user.role, release_group_mbid)
    return HeldListResponse(items=[_held_to_response(h) for h in held])


@router.post("/held/{held_id}/import", response_model=HeldActionResponse)
async def import_held(
    held_id: int, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    """Import a held track as-is, overriding the AcoustID identity check (admin/owner)."""
    final_path = await service.import_held(held_id, current_user.id, current_user.role)
    return HeldActionResponse(status="imported", final_path=final_path)


@router.post("/held/{held_id}/discard", response_model=HeldActionResponse)
async def discard_held(
    held_id: int, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    """Delete a held track's file and let the album's auto-retry resume."""
    await service.discard_held(held_id, current_user.id, current_user.role)
    return HeldActionResponse(status="discarded")


_AUDIO_MEDIA_TYPES = {
    ".flac": "audio/flac", ".mp3": "audio/mpeg", ".m4a": "audio/mp4",
    ".ogg": "audio/ogg", ".opus": "audio/opus", ".wav": "audio/wav",
}


def _audio_media_type(path: Path) -> str:
    return (
        _AUDIO_MEDIA_TYPES.get(path.suffix.lower())
        or mimetypes.guess_type(str(path))[0]
        or "application/octet-stream"
    )


@router.get("/held/{held_id}/audio")
async def stream_held_audio(
    held_id: int, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    """Stream a held file's audio for the in-review preview. FileResponse honours Range
    requests (206 + Content-Range), so the player can scrub. Ownership-checked."""
    held = await service.get_held(held_id, current_user.id, current_user.role)
    if held is None:
        raise ResourceNotFoundError("Held track not found")
    path = Path(held.held_path)
    if not path.exists():
        raise ResourceNotFoundError("The held file is no longer available")
    return FileResponse(path, media_type=_audio_media_type(path), filename=path.name)


@router.post("/{task_id}/reimport", response_model=ReimportDownloadResponse)
async def reimport_download(
    task_id: str, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    task = await service.reimport_task(task_id, current_user.id, current_user.role)
    return ReimportDownloadResponse(
        success=task.status in ("completed", "partial"),
        status=task.status,
        files_imported=task.files_completed,
        files_failed=task.files_failed,
        error_message=task.error_message,
    )


@router.get("/{task_id}", response_model=DownloadTaskResponse)
async def get_download(
    task_id: str, current_user: CurrentUserDep, service=Depends(get_download_service)
):
    task = await service.get_task(task_id, current_user.id, current_user.role)
    return _to_response(
        task,
        next_retry_at=service.next_retry_at(task),
        retry_max=service.auto_retry_max,
        retry_ladder_minutes=service.retry_ladder_minutes(),
    )
