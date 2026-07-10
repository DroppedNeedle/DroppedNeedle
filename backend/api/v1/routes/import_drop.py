"""Drop importer routes (Store Sync phase 01c).

Curator-gated (admin + trusted): importing writes files into the shared
library, the same trust tier that may auto-acquire. Plain users' path stays
request -> a curator imports -> they get notified.
"""

import asyncio
import logging
import shutil
import uuid
from pathlib import Path

from fastapi import APIRouter, Depends, File, UploadFile

from api.v1.schemas.drop_import import (
    DropImportItemResponse,
    DropImportJobResponse,
    DropImportJobsResponse,
    DropImportMatchRequest,
    item_to_response,
    job_to_response,
)
from core.dependencies import get_drop_import_service
from core.exceptions import ValidationError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentCuratorDep
from services.native.library_manager import _AUDIO_SUFFIXES

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/import", tags=["import"])

_MAX_FILES_PER_DROP = 25


def _copy_to(src, dest: Path) -> None:  # noqa: ANN001 - SpooledTemporaryFile
    # the multipart parser may leave the spooled file at EOF; without the
    # rewind this would stage zero-byte files
    src.seek(0)
    dest.parent.mkdir(parents=True, exist_ok=True)
    with open(dest, "wb") as out:
        shutil.copyfileobj(src, out)


@router.post("/uploads", response_model=DropImportJobResponse, status_code=202)
async def upload_drop(
    current_user: CurrentCuratorDep,
    files: list[UploadFile] = File(...),
    service=Depends(get_drop_import_service),
):
    """Accept zips and/or audio files as one import job. Files are validated by
    extension up front so nothing is staged for a drop that would be rejected."""
    if not files:
        raise ValidationError("No files were uploaded")
    if len(files) > _MAX_FILES_PER_DROP:
        raise ValidationError(f"Too many files in one drop (max {_MAX_FILES_PER_DROP})")
    for upload in files:
        name = upload.filename or ""
        suffix = Path(name).suffix.lower()
        if suffix != ".zip" and suffix not in _AUDIO_SUFFIXES:
            raise ValidationError(f"Unsupported file type: {name or 'unnamed file'}")

    staged: list[tuple[str, Path]] = []
    try:
        for upload in files:
            tmp = service.incoming_dir() / uuid.uuid4().hex
            await asyncio.to_thread(_copy_to, upload.file, tmp)
            staged.append((upload.filename or "upload", tmp))
        job = await service.create_job(
            user_id=current_user.id,
            user_name=current_user.display_name,
            uploads=staged,
        )
    except Exception:
        for _name, tmp in staged:
            try:
                tmp.unlink(missing_ok=True)
            except OSError:
                logger.warning("Could not remove staged upload %s", tmp)
        raise
    return job_to_response(job)


@router.get("/jobs", response_model=DropImportJobsResponse)
async def list_jobs(
    current_user: CurrentCuratorDep,
    all: bool = False,  # noqa: A002 - query-param name is the API surface
    service=Depends(get_drop_import_service),
):
    """The caller's recent import jobs; admins may pass ``all=true`` for every
    user's jobs (non-admins are always scoped to their own)."""
    include_all = all and current_user.role == "admin"
    jobs = await service.list_jobs(user_id=current_user.id, include_all=include_all)
    return DropImportJobsResponse(jobs=[job_to_response(j) for j in jobs])


@router.get("/jobs/{job_id}", response_model=DropImportJobResponse)
async def get_job(
    job_id: str,
    current_user: CurrentCuratorDep,
    service=Depends(get_drop_import_service),
):
    job = await service.get_job(
        job_id, user_id=current_user.id, is_admin=current_user.role == "admin"
    )
    return job_to_response(job)


@router.post("/items/{item_id}/match", response_model=DropImportItemResponse)
async def match_item(
    item_id: int,
    current_user: CurrentCuratorDep,
    body: DropImportMatchRequest = MsgSpecBody(DropImportMatchRequest),
    service=Depends(get_drop_import_service),
):
    item = await service.match_item(
        item_id,
        body.release_group_mbid,
        user_id=current_user.id,
        is_admin=current_user.role == "admin",
    )
    return item_to_response(item)


@router.post("/items/{item_id}/discard", response_model=DropImportItemResponse)
async def discard_item(
    item_id: int,
    current_user: CurrentCuratorDep,
    service=Depends(get_drop_import_service),
):
    item = await service.discard_item(
        item_id,
        user_id=current_user.id,
        is_admin=current_user.role == "admin",
    )
    return item_to_response(item)
