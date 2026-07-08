"""Read-only Lidarr importer routes (LidarrImport).

Admin-only connection config + Test; any authenticated user can read the monitored-artist
candidates and import them into *their own* follows (D4). The old Lidarr *management*
integration stays deleted (D8) - these are the only sanctioned ``/lidarr-import`` paths.
"""

import logging

from fastapi import APIRouter, Depends

from api.v1.schemas.lidarr_import import (
    LidarrArtistListResponse,
    LidarrImportRequest,
    LidarrImportResponse,
    LidarrImportStatusResponse,
    LidarrTestResponse,
)
from api.v1.schemas.settings import (
    LIDARR_IMPORT_API_KEY_MASK,
    LidarrImportConnectionSettings,
)
from core.dependencies import (
    get_lidarr_import_repository,
    get_lidarr_import_service,
    get_preferences_service,
)
from core.exceptions import LidarrImportError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from infrastructure.resilience.retry import CircuitOpenError
from middleware import CurrentAdminDep, CurrentUserDep

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/lidarr-import", tags=["lidarr-import"])


@router.get("/config", response_model=LidarrImportConnectionSettings)
async def get_config(_: CurrentAdminDep, preferences=Depends(get_preferences_service)):
    return preferences.get_lidarr_import_connection()


@router.put("/config", response_model=LidarrImportConnectionSettings)
async def update_config(
    _: CurrentAdminDep,
    settings: LidarrImportConnectionSettings = MsgSpecBody(LidarrImportConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    preferences.save_lidarr_import_connection(settings)
    return preferences.get_lidarr_import_connection()


@router.post("/test", response_model=LidarrTestResponse)
async def test_connection(
    _: CurrentAdminDep,
    settings: LidarrImportConnectionSettings = MsgSpecBody(LidarrImportConnectionSettings),
    preferences=Depends(get_preferences_service),
    repo=Depends(get_lidarr_import_repository),
):
    """Probe ``system/status`` with the SUBMITTED url/key (so Test works before the first
    save); a masked key resolves to the stored one. Reachable/bad-key/version distinctions
    are carried in the body - never a leaked 5xx (the URL/host is never echoed)."""
    api_key = settings.api_key
    if api_key == LIDARR_IMPORT_API_KEY_MASK:
        api_key = preferences.get_lidarr_import_connection_raw().api_key
    try:
        status = await repo.get_system_status(settings.url, api_key)
    except LidarrImportError as exc:
        if exc.auth:
            return LidarrTestResponse(
                valid=False,
                message="Lidarr rejected the API key. Check Settings → General → "
                "Security → API Key in Lidarr.",
            )
        return LidarrTestResponse(
            valid=False,
            message="Couldn't reach Lidarr. Check the URL and that Lidarr is running.",
        )
    except CircuitOpenError:
        return LidarrTestResponse(
            valid=False,
            message="Lidarr is temporarily unavailable after repeated failures. "
            "Try again shortly.",
        )
    return LidarrTestResponse(
        valid=True,
        version=status.version or None,
        message=f"Connected - Lidarr v{status.version}" if status.version else "Connected",
    )


@router.get("/status", response_model=LidarrImportStatusResponse)
async def get_status(_: CurrentUserDep, preferences=Depends(get_preferences_service)):
    """The non-admin gate for the import button. Returns ONLY ``{configured}`` - never the
    url or api_key (config-leak guard)."""
    return LidarrImportStatusResponse(configured=preferences.is_lidarr_import_configured())


@router.get("/artists", response_model=LidarrArtistListResponse)
async def list_candidates(
    current_user: CurrentUserDep,
    service=Depends(get_lidarr_import_service),
):
    return await service.list_import_candidates(current_user.id)


@router.post("/import", response_model=LidarrImportResponse)
async def import_artists(
    current_user: CurrentUserDep,
    body: LidarrImportRequest = MsgSpecBody(LidarrImportRequest),
    service=Depends(get_lidarr_import_service),
):
    return await service.import_artists(
        current_user.id, current_user.role, body.selected_mbids
    )
