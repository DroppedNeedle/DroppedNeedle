"""Prowlarr connection admin routes (fork feature): GET/PUT settings + test.

One Prowlarr URL + API key covers every indexer the user manages there (usenet
AND torrent/private trackers): when enabled it replaces the per-indexer Newznab
fan-out for Usenet search and supplies the torrent source's search. Admin-only;
``api_key`` masked on GET, preserved on PUT when the masked sentinel comes back.
Kept in its own module (not ``indexers.py``) to hold the upstream-merge surface
down to the router registration line.
"""

import logging

from fastapi import APIRouter, Depends

from api.v1.schemas.download import ProwlarrTestResponse
from api.v1.schemas.settings import PROWLARR_API_KEY_MASK, ProwlarrConnectionSettings
from core.dependencies import build_prowlarr_client, get_preferences_service
from core.exceptions import ExternalServiceError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/prowlarr", tags=["prowlarr"])


def _clear_prowlarr_cache() -> None:
    # The orchestrator/service capture the indexer at construction; the usenet strategy's
    # indexer selection (Prowlarr vs Newznab fan-out) also happens at build time - clear
    # the whole downstream chain so a save takes effect at once.
    from core.dependencies import (
        get_download_orchestrator,
        get_download_service,
        get_prowlarr_client,
        get_prowlarr_indexer,
        get_target_download_orchestrator,
        get_target_download_service,
        get_torrent_release_scorer,
    )

    for provider in (
        get_prowlarr_client,
        get_prowlarr_indexer,
        get_torrent_release_scorer,
        get_download_orchestrator,
        get_download_service,
        get_target_download_orchestrator,
        get_target_download_service,
    ):
        provider.cache_clear()


@router.get("", response_model=ProwlarrConnectionSettings)
async def get_prowlarr(_: CurrentAdminDep, preferences=Depends(get_preferences_service)):
    return preferences.get_prowlarr_connection()


@router.put("", response_model=ProwlarrConnectionSettings)
async def update_prowlarr(
    _: CurrentAdminDep,
    settings: ProwlarrConnectionSettings = MsgSpecBody(ProwlarrConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    preferences.save_prowlarr_connection(settings)
    _clear_prowlarr_cache()
    return preferences.get_prowlarr_connection()


@router.post("/test", response_model=ProwlarrTestResponse)
async def test_prowlarr(
    _: CurrentAdminDep,
    settings: ProwlarrConnectionSettings = MsgSpecBody(ProwlarrConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    """Tests the submitted url/key (not stored config). A masked key resolves to the
    stored one."""
    api_key = settings.api_key
    if api_key == PROWLARR_API_KEY_MASK:
        api_key = preferences.get_prowlarr_connection_raw().api_key

    client = build_prowlarr_client(settings.url, api_key)
    try:
        status = await client.system_status()
        indexers = await client.indexers()
    except ExternalServiceError as exc:
        return ProwlarrTestResponse(valid=False, message=str(exc))
    enabled = [i for i in indexers if i.enable]
    return ProwlarrTestResponse(
        valid=True,
        version=status.version or None,
        message=f"Prowlarr {status.version}".strip(),
        indexers_total=len(enabled),
        indexers_usenet=sum(1 for i in enabled if i.protocol.lower() == "usenet"),
        indexers_torrent=sum(1 for i in enabled if i.protocol.lower() == "torrent"),
    )
