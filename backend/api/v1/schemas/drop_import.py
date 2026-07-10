"""Wire schemas for the drop importer (Store Sync phase 01c).

Deliberately narrower than the domain models: staging directories and staged
file paths are on-disk paths and never cross the HTTP boundary (the same rule
that admin-gates the unmatched list).
"""

from infrastructure.msgspec_fastapi import AppStruct
from models.drop_import import DropImportItem, DropImportJob


class DropImportItemResponse(AppStruct):
    id: int
    folder_name: str
    status: str
    updated_at: float
    release_group_mbid: str | None = None
    album_title: str | None = None
    artist_name: str | None = None
    files_total: int = 0
    files_imported: int = 0
    detail: str | None = None


class DropImportJobResponse(AppStruct):
    id: str
    status: str
    created_at: float
    upload_name: str
    user_id: str
    user_name: str
    error: str | None = None
    items: list[DropImportItemResponse] = []


class DropImportJobsResponse(AppStruct):
    jobs: list[DropImportJobResponse]


class DropImportMatchRequest(AppStruct):
    release_group_mbid: str


def item_to_response(item: DropImportItem) -> DropImportItemResponse:
    return DropImportItemResponse(
        id=item.id,
        folder_name=item.folder_name,
        status=item.status,
        updated_at=item.updated_at,
        release_group_mbid=item.release_group_mbid,
        album_title=item.album_title,
        artist_name=item.artist_name,
        files_total=item.files_total,
        files_imported=item.files_imported,
        detail=item.detail,
    )


def job_to_response(job: DropImportJob) -> DropImportJobResponse:
    return DropImportJobResponse(
        id=job.id,
        status=job.status,
        created_at=job.created_at,
        upload_name=job.upload_name,
        user_id=job.user_id,
        user_name=job.user_name,
        error=job.error,
        items=[item_to_response(i) for i in job.items],
    )
