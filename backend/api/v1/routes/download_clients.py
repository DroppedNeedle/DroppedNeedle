"""Download-clients (SABnzbd) + shared download-policy admin routes.

SABnzbd config GET/PUT/test and the shared ``download_policy`` GET/PUT are admin-only.
The SABnzbd ``api_key`` is the FULL key (the add-only nzbkey can't do queue/history/
delete) - masked on GET, preserved on PUT when the masked sentinel comes back. Test
reports SABnzbd's version + category list + completed dir (the mount hint).
"""

import logging

from fastapi import APIRouter, Depends

from api.v1.schemas.download import SabnzbdTestResponse, SourcePriority
from api.v1.schemas.settings import (
    SABNZBD_API_KEY_MASK,
    DownloadPolicySettings,
    SabnzbdConnectionSettings,
    WantedWatcherSettings,
)
from core.dependencies import build_sabnzbd_download_client, get_preferences_service
from core.exceptions import ExternalServiceError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/download-clients", tags=["download-clients"])


def _clear_download_client_cache() -> None:
    # Both the SABnzbd connection and the shared policy feed the scorers, file processor,
    # orchestrator and service - clear the whole chain so a save takes effect at once
    # (shared with the plugin settings API - see core/dependencies/invalidation.py).
    from core.dependencies.invalidation import clear_usenet_chain

    clear_usenet_chain()


@router.get("/sabnzbd", response_model=SabnzbdConnectionSettings)
async def get_sabnzbd(_: CurrentAdminDep, preferences=Depends(get_preferences_service)):
    return preferences.get_sabnzbd_connection()


@router.put("/sabnzbd", response_model=SabnzbdConnectionSettings)
async def update_sabnzbd(
    _: CurrentAdminDep,
    settings: SabnzbdConnectionSettings = MsgSpecBody(SabnzbdConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    preferences.save_sabnzbd_connection(settings)
    _clear_download_client_cache()
    return preferences.get_sabnzbd_connection()


@router.post("/sabnzbd/test", response_model=SabnzbdTestResponse)
async def test_sabnzbd(
    _: CurrentAdminDep,
    settings: SabnzbdConnectionSettings = MsgSpecBody(SabnzbdConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    """Tests the submitted url/key (not stored config). A masked key resolves to the
    stored one."""
    api_key = settings.api_key
    if api_key == SABNZBD_API_KEY_MASK:
        api_key = preferences.get_sabnzbd_connection_raw().api_key

    client = build_sabnzbd_download_client(settings.url, api_key)
    try:
        status = await client.health_check()
        if status.status != "ok":
            return SabnzbdTestResponse(valid=False, message=status.message or "SABnzbd unreachable")
        cats = await client.get_categories()
        complete_dir = await client.get_complete_dir()
    except ExternalServiceError as exc:
        return SabnzbdTestResponse(valid=False, message=str(exc))
    return SabnzbdTestResponse(
        valid=True,
        version=status.version,
        message=f"SABnzbd {status.version}",
        categories=cats,
        complete_dir=complete_dir or None,
    )


@router.get("/source-priority", response_model=SourcePriority)
async def get_source_priority(_: CurrentAdminDep, preferences=Depends(get_preferences_service)):
    return SourcePriority(order=preferences.get_source_priority())


@router.put("/source-priority", response_model=SourcePriority)
async def update_source_priority(
    _: CurrentAdminDep,
    body: SourcePriority = MsgSpecBody(SourcePriority),
    preferences=Depends(get_preferences_service),
):
    preferences.save_source_priority(body.order)
    _clear_download_client_cache()
    return SourcePriority(order=preferences.get_source_priority())


@router.get("/policy", response_model=DownloadPolicySettings)
async def get_policy(_: CurrentAdminDep, preferences=Depends(get_preferences_service)):
    return preferences.get_download_policy()


@router.put("/policy", response_model=DownloadPolicySettings)
async def update_policy(
    _: CurrentAdminDep,
    policy: DownloadPolicySettings = MsgSpecBody(DownloadPolicySettings),
    preferences=Depends(get_preferences_service),
):
    preferences.save_download_policy(policy)
    _clear_download_client_cache()
    return preferences.get_download_policy()


@router.get("/wanted", response_model=WantedWatcherSettings)
async def get_wanted_watcher_settings(
    _: CurrentAdminDep, preferences=Depends(get_preferences_service)
):
    return preferences.get_wanted_settings()


@router.put("/wanted", response_model=WantedWatcherSettings)
async def update_wanted_watcher_settings(
    _: CurrentAdminDep,
    settings: WantedWatcherSettings = MsgSpecBody(WantedWatcherSettings),
    preferences=Depends(get_preferences_service),
):
    # no singleton clearing needed: the watcher re-reads this section every sweep
    preferences.save_wanted_settings(settings)
    return preferences.get_wanted_settings()
