from fastapi import APIRouter, Depends

from api.v1.schemas.cache_status import CacheSyncStatus
from core.dependencies import get_cache_status_service
from infrastructure.msgspec_fastapi import MsgSpecRoute
from middleware import CurrentCuratorDep
from services.cache_status_service import CacheStatusService

router = APIRouter(route_class=MsgSpecRoute, prefix="/cache/sync", tags=["cache"])


@router.get("/status", response_model=CacheSyncStatus)
async def get_sync_status(
    status_service: CacheStatusService = Depends(get_cache_status_service),
):
    progress = status_service.get_progress()

    return CacheSyncStatus(
        is_syncing=progress.is_syncing,
        phase=progress.phase,
        total_items=progress.total_items,
        processed_items=progress.processed_items,
        progress_percent=progress.progress_percent,
        current_item=progress.current_item,
        started_at=progress.started_at,
        error_message=progress.error_message,
        total_artists=progress.total_artists,
        processed_artists=progress.processed_artists,
        total_albums=progress.total_albums,
        processed_albums=progress.processed_albums
    )


@router.post("/cancel")
async def cancel_sync(
    _user: CurrentCuratorDep,
    status_service: CacheStatusService = Depends(get_cache_status_service),
):
    from core.task_registry import TaskRegistry
    await status_service.cancel_current_sync()
    await TaskRegistry.get_instance().cancel("precache-library")
    await status_service.wait_for_completion()
    return {"status": "cancelled"}
