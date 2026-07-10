import logging

from fastapi import APIRouter, BackgroundTasks, Depends, HTTPException, Query, Request, status
from core.exceptions import ClientDisconnectedError
from api.v1.schemas.album import (
    AlbumBasicInfo,
    AlbumEditionItem,
    AlbumEditionsResponse,
    AlbumInfo,
    AlbumTracksInfo,
    EditionAcquireResponse,
    EditionPinBody,
    EditionPinResponse,
    LastFmAlbumEnrichment,
)
from api.v1.schemas.discovery import SimilarAlbumsResponse, MoreByArtistResponse
from api.v1.schemas.get_it import PurchaseOptionsResponse
from core.dependencies import get_album_service, get_album_discovery_service, get_album_enrichment_service, get_download_service, get_get_it_service, get_navidrome_library_service
from services.album_service import AlbumService
from services.album_discovery_service import AlbumDiscoveryService
from services.album_enrichment_service import AlbumEnrichmentService
from services.navidrome_library_service import NavidromeLibraryService
from middleware import CurrentCuratorDep, CurrentUserDep
from infrastructure.msgspec_fastapi import MsgSpecBody
from infrastructure.validators import is_unknown_mbid
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.degradation import try_get_degradation_context
from infrastructure.msgspec_fastapi import MsgSpecRoute

import msgspec.structs
import msgspec

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/albums", tags=["album"])


@router.get("/{album_id}", response_model=AlbumInfo)
async def get_album(
    album_id: str,
    album_service: AlbumService = Depends(get_album_service)
):
    if is_unknown_mbid(album_id):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"Invalid or unknown album ID: {album_id}"
        )
    
    try:
        result = await album_service.get_album_info(album_id)
        ctx = try_get_degradation_context()
        if ctx is not None and ctx.has_degradation():
            result = msgspec.structs.replace(result, service_status=ctx.degraded_summary())
        return result
    except ValueError:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid album request"
        )


@router.post("/{album_id}/refresh", response_model=AlbumBasicInfo)
async def refresh_album(
    album_id: str,
    album_service: AlbumService = Depends(get_album_service),
    navidrome_service: NavidromeLibraryService = Depends(get_navidrome_library_service),
):
    if is_unknown_mbid(album_id):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"Invalid or unknown album ID: {album_id}"
        )

    try:
        navidrome_service.invalidate_album_cache(album_id)
        await album_service.refresh_album(album_id)
        return await album_service.get_album_basic_info(album_id)
    except ValueError:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid album request"
        )


@router.get("/{album_id}/purchase-options", response_model=PurchaseOptionsResponse)
async def get_album_purchase_options(
    album_id: str,
    service=Depends(get_get_it_service),
):
    """The "Where to buy" links for an album (Get it, phase 01). Lazy by design:
    the album page's own load path never calls this - the section component
    fetches it with its own skeleton, so cold iTunes/MB lookups cost the album
    page nothing."""
    if is_unknown_mbid(album_id):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"Invalid or unknown album ID: {album_id}",
        )
    return await service.get_purchase_options(album_id)


@router.get("/{album_id}/basic", response_model=AlbumBasicInfo)
async def get_album_basic(
    album_id: str,
    request: Request,
    background_tasks: BackgroundTasks,
    album_service: AlbumService = Depends(get_album_service)
):
    if await request.is_disconnected():
        raise ClientDisconnectedError("Client disconnected")
    
    if is_unknown_mbid(album_id):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"Invalid or unknown album ID: {album_id}"
        )
    
    try:
        result = await album_service.get_album_basic_info(album_id)
        background_tasks.add_task(album_service.warm_full_album_cache, album_id)
        return result
    except ValueError:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid album request"
        )


@router.get("/{album_id}/tracks", response_model=AlbumTracksInfo)
async def get_album_tracks(
    album_id: str,
    request: Request,
    album_service: AlbumService = Depends(get_album_service)
):
    if await request.is_disconnected():
        raise ClientDisconnectedError("Client disconnected")
    
    if is_unknown_mbid(album_id):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"Invalid or unknown album ID: {album_id}"
        )
    
    try:
        return await album_service.get_album_tracks_info(album_id)
    except ValueError:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid album request"
        )


@router.get("/{album_id}/similar", response_model=SimilarAlbumsResponse)
async def get_similar_albums(
    album_id: str,
    current_user: CurrentUserDep,
    artist_id: str = Query(..., description="Artist MBID for similarity lookup"),
    count: int = Query(default=10, ge=1, le=30),
    discovery_service: AlbumDiscoveryService = Depends(get_album_discovery_service)
):
    if is_unknown_mbid(album_id) or is_unknown_mbid(artist_id):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid or unknown album/artist ID"
        )
    return await discovery_service.get_similar_albums(album_id, artist_id, count, user_id=current_user.id)


@router.get("/{album_id}/more-by-artist", response_model=MoreByArtistResponse)
async def get_more_by_artist(
    album_id: str,
    artist_id: str = Query(..., description="Artist MBID"),
    count: int = Query(default=10, ge=1, le=30),
    discovery_service: AlbumDiscoveryService = Depends(get_album_discovery_service)
):
    if is_unknown_mbid(artist_id):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid or unknown artist ID"
        )
    return await discovery_service.get_more_by_artist(
        artist_id, album_id, count, priority=RequestPriority.USER_INITIATED
    )


@router.get("/{album_id}/lastfm", response_model=LastFmAlbumEnrichment)
async def get_album_lastfm_enrichment(
    album_id: str,
    artist_name: str = Query(..., description="Artist name for Last.fm lookup"),
    album_name: str = Query(..., description="Album name for Last.fm lookup"),
    enrichment_service: AlbumEnrichmentService = Depends(get_album_enrichment_service),
):
    if is_unknown_mbid(album_id):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"Invalid or unknown album ID: {album_id}"
        )
    result = await enrichment_service.get_lastfm_enrichment(
        artist_name=artist_name, album_name=album_name, album_mbid=album_id
    )
    if result is None:
        return LastFmAlbumEnrichment()
    return result

# --- Album edition selection (CollectionManagement Feature E) -------------------


@router.get("/{album_id}/editions", response_model=AlbumEditionsResponse)
async def list_album_editions(
    album_id: str,
    current_user: CurrentUserDep,
    album_service: AlbumService = Depends(get_album_service),
):
    """Every edition (MB release) of this album, flagged with the owned and pinned
    ones - the Edition picker's data source. Viewing is open to all roles."""
    try:
        data = await album_service.list_editions(album_id)
    except ValueError:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid album request"
        )
    return AlbumEditionsResponse(
        items=[AlbumEditionItem(**item) for item in data["items"]],
        pinned_release_mbid=data["pinned_release_mbid"],
        owned_release_mbid=data["owned_release_mbid"],
    )


@router.put("/{album_id}/edition", response_model=EditionPinResponse)
async def set_album_edition(
    album_id: str,
    current_user: CurrentCuratorDep,
    body: EditionPinBody = MsgSpecBody(EditionPinBody),
    album_service: AlbumService = Depends(get_album_service),
):
    """Pin an edition (admin/trusted, D16): the album page and acquisition follow it
    until cleared. Busts the album caches."""
    try:
        await album_service.set_edition_pin(album_id, body.release_mbid, current_user.id)
    except ValueError:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid album request"
        )
    return EditionPinResponse(pinned_release_mbid=body.release_mbid)


@router.delete("/{album_id}/edition", response_model=EditionPinResponse)
async def clear_album_edition(
    album_id: str,
    current_user: CurrentCuratorDep,
    album_service: AlbumService = Depends(get_album_service),
):
    """Clear the pin (back to auto: owned edition, else ranked)."""
    try:
        await album_service.clear_edition_pin(album_id)
    except ValueError:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid album request"
        )
    return EditionPinResponse(pinned_release_mbid=None)


@router.post("/{album_id}/edition/acquire", response_model=EditionAcquireResponse)
async def acquire_album_edition(
    album_id: str,
    current_user: CurrentCuratorDep,
    download_service=Depends(get_download_service),
):
    """Fill the pinned/owned edition's missing tracks + upgrade its below-cutoff
    owned tracks (admin/trusted, D13). Never retags existing files (D15)."""
    result = await download_service.acquire_edition(current_user.id, album_id)
    return EditionAcquireResponse(**result)
