"""qBittorrent connection admin routes (fork feature, torrent source): GET/PUT
settings + test. Admin-only; ``api_key`` masked on GET, preserved on PUT when the
masked sentinel comes back. Kept in its own module (same ``/download-clients``
prefix as the SABnzbd routes) to hold the upstream-merge surface down to the
router registration line.
"""

import logging

from fastapi import APIRouter, Depends

from api.v1.schemas.download import QbittorrentTestResponse
from api.v1.schemas.settings import (
    QBITTORRENT_API_KEY_MASK,
    QbittorrentConnectionSettings,
)
from core.dependencies import (
    build_qbittorrent_download_client,
    get_preferences_service,
)
from core.exceptions import ExternalServiceError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep

logger = logging.getLogger(__name__)

router = APIRouter(
    route_class=MsgSpecRoute, prefix="/download-clients", tags=["download-clients"]
)


def _clear_qbittorrent_cache() -> None:
    from core.dependencies import (
        get_download_orchestrator,
        get_download_service,
        get_qbittorrent_client,
        get_qbittorrent_download_client,
        get_target_download_orchestrator,
        get_target_download_service,
    )

    for provider in (
        get_qbittorrent_client,
        get_qbittorrent_download_client,
        get_download_orchestrator,
        get_download_service,
        get_target_download_orchestrator,
        get_target_download_service,
    ):
        provider.cache_clear()


@router.get("/qbittorrent", response_model=QbittorrentConnectionSettings)
async def get_qbittorrent(
    _: CurrentAdminDep, preferences=Depends(get_preferences_service)
):
    return preferences.get_qbittorrent_connection()


@router.put("/qbittorrent", response_model=QbittorrentConnectionSettings)
async def update_qbittorrent(
    _: CurrentAdminDep,
    settings: QbittorrentConnectionSettings = MsgSpecBody(QbittorrentConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    preferences.save_qbittorrent_connection(settings)
    _clear_qbittorrent_cache()
    return preferences.get_qbittorrent_connection()


@router.post("/qbittorrent/test", response_model=QbittorrentTestResponse)
async def test_qbittorrent(
    _: CurrentAdminDep,
    settings: QbittorrentConnectionSettings = MsgSpecBody(QbittorrentConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    """Tests the submitted url/credentials (not stored config). A masked API key
    resolves to the stored one."""
    api_key = settings.api_key
    if api_key == QBITTORRENT_API_KEY_MASK:
        api_key = preferences.get_qbittorrent_connection_raw().api_key

    if not api_key:
        return QbittorrentTestResponse(
            valid=False, message="qBittorrent API key is required"
        )
    client = build_qbittorrent_download_client(settings.url, api_key)
    try:
        status = await client.health_check()
    except ExternalServiceError as exc:
        return QbittorrentTestResponse(valid=False, message=str(exc))
    if status.status != "ok":
        return QbittorrentTestResponse(
            valid=False, message=status.message or "qBittorrent unreachable"
        )
    return QbittorrentTestResponse(
        valid=True, version=status.version, message=status.message or "qBittorrent"
    )
