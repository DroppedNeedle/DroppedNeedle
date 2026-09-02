"""Wire schemas for Free Music (D24)."""

from infrastructure.msgspec_fastapi import AppStruct
from models.free_music import FreeMusicTask


class FreeMusicTaskResponse(AppStruct):
    id: str
    user_id: str
    kind: str
    mbid: str
    artist: str
    title: str
    status: str
    created_at: float
    updated_at: float
    identifier: str = ""
    licence_url: str = ""
    format: str = ""
    files_total: int = 0
    files_completed: int = 0
    bytes_total: int = 0
    bytes_downloaded: int = 0
    error: str | None = None
    quality_snapshot_hash: str | None = None
    quality_snapshot_summary: str | None = None


class FreeMusicTasksResponse(AppStruct):
    tasks: list[FreeMusicTaskResponse] = []


class FreeMusicHistoryClearResponse(AppStruct):
    cleared: int = 0


def task_to_response(task: FreeMusicTask) -> FreeMusicTaskResponse:
    return FreeMusicTaskResponse(
        id=task.id,
        user_id=task.user_id,
        kind=task.kind,
        mbid=task.mbid,
        artist=task.artist,
        title=task.title,
        status=task.status,
        created_at=task.created_at,
        updated_at=task.updated_at,
        identifier=task.identifier,
        licence_url=task.licence_url,
        format=task.format,
        files_total=task.files_total,
        files_completed=task.files_completed,
        bytes_total=task.bytes_total,
        bytes_downloaded=task.bytes_downloaded,
        error=task.error,
        quality_snapshot_hash=task.quality_snapshot_hash,
        quality_snapshot_summary=task.quality_snapshot_summary,
    )
