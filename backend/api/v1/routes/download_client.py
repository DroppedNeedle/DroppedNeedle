"""Download-client admin config/test + status routes (Phase 6).

config GET/PUT and test are admin-only; status is authenticated (it surfaces the
downloads-mount health, which any user's UI may read). slskd ``api_key`` is
masked on GET and preserved on PUT when the masked sentinel comes back.
"""

import asyncio
import logging
from pathlib import Path

from fastapi import APIRouter, Depends

from api.v1.schemas.download import DownloadClientStatusResponse, TestConnectionResponse
from api.v1.schemas.settings import DownloadClientConnectionSettings
from core.config import get_settings
from core.dependencies import (
    get_download_client_repository,
    get_preferences_service,
    get_settings_service,
)
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep, CurrentUserDep
from models.common import ServiceStatus
from services.native.download_service import check_downloads_mount

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/download-client", tags=["download-client"])


@router.get("/config", response_model=DownloadClientConnectionSettings)
async def get_config(_: CurrentAdminDep, preferences=Depends(get_preferences_service)):
    return preferences.get_download_client_settings()


@router.put("/config", response_model=DownloadClientConnectionSettings)
async def update_config(
    _: CurrentAdminDep,
    settings: DownloadClientConnectionSettings = MsgSpecBody(DownloadClientConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    preferences.save_download_client_settings(settings)
    # bust the whole download-client singleton chain so new settings take effect
    # immediately; scorer/matcher/DownloadService/orchestrator capture these at
    # construction and hold the client, so they must be cleared too, not just slskd
    # client/repository. The orchestrator is built eagerly at startup (resume task),
    # so omitting it leaves every download running against the old/empty URL.
    from core.dependencies import (
        get_album_preflight_scorer,
        get_download_client_repository as _dc,
        get_download_orchestrator,
        get_download_service,
        get_slskd_client,
        get_slskd_repository,
        get_track_matcher,
    )

    for provider in (
        get_slskd_client,
        get_slskd_repository,
        _dc,
        get_album_preflight_scorer,
        get_track_matcher,
        get_download_orchestrator,
        get_download_service,
    ):
        provider.cache_clear()
    return preferences.get_download_client_settings()


@router.post("/test", response_model=TestConnectionResponse)
async def test_connection(
    _: CurrentAdminDep,
    settings: DownloadClientConnectionSettings = MsgSpecBody(DownloadClientConnectionSettings),
    settings_service=Depends(get_settings_service),
):
    # tests credentials in the request body, not stored config, so Test works
    # before the first save and reflects edits
    status = await settings_service.verify_download_client(settings)
    return TestConnectionResponse(
        valid=status.status == "ok", version=status.version, message=status.message or ""
    )


@router.get("/status", response_model=DownloadClientStatusResponse)
async def get_status(
    current_user: CurrentUserDep,
    client=Depends(get_download_client_repository),
    preferences=Depends(get_preferences_service),
):
    app_settings = get_settings()
    library_paths = [Path(p) for p in preferences.get_library_settings().library_paths]
    # Offload the blocking stat/access syscalls (slow on network mounts) off the loop.
    mount = await asyncio.to_thread(
        check_downloads_mount, app_settings.slskd_downloads_path, library_paths
    )
    configured = client.is_configured()
    client_status = (
        await client.health_check()
        if configured
        else ServiceStatus(status="error", message="not configured")
    )
    # Catch the silent misconfig: a mount that passes the basic checks but where
    # slskd's finished downloads aren't actually visible (wrong path / unreadable).
    advisory: str | None = None
    slskd_dir: str | None = None
    if configured and mount.ok:
        diag = await client.diagnose_downloads_mount()
        slskd_dir = diag.client_downloads_dir
        # "slskd saves to X" - naming the exact path turns guesswork into a checklist.
        saves_to = f" slskd saves to {slskd_dir}." if slskd_dir else ""
        if diag.supported and diag.completed_downloads > 0:
            if not diag.mount_has_files:
                advisory = (
                    f"slskd has {diag.completed_downloads} finished download(s), but the downloads "
                    f"folder at {mount.path} looks empty.{saves_to} Make sure it points to slskd's "
                    f"downloads directory and that the container can read it (check the PUID/GID)."
                )
            elif diag.sampled_downloads > 0 and diag.resolvable_downloads == 0:
                # Mount has files but NONE of slskd's finished downloads resolve under it -
                # the classic "mounted a parent (whole media share) instead of slskd's
                # completed-downloads dir" footgun. The finder then can't reach them.
                advisory = (
                    f"None of slskd's {diag.completed_downloads} finished download(s) are visible "
                    f"under {mount.path}.{saves_to} The downloads mount usually points at a parent "
                    f"folder (e.g. your whole media share) instead of slskd's completed-downloads "
                    f"folder. Point it at the completed-downloads folder."
                )
    return DownloadClientStatusResponse(
        configured=configured, client=client_status, mount=mount,
        mount_advisory=advisory, slskd_downloads_dir=slskd_dir,
    )
