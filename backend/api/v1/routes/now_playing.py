"""Live now-playing presence endpoints.

- ``GET /api/v1/now-playing``         - projected snapshot (hydrate)
- ``POST /api/v1/now-playing``        - native web-player heartbeat (upsert presence)
- ``DELETE /api/v1/now-playing``      - native web-player stop (clear presence)

Live snapshots ride the multiplexed ``GET /api/v1/events/stream`` feed.

Privacy is applied in ``NowPlayingService`` keyed on the owner's setting, so the song
of a user on ``track_hidden`` is never serialized here.
"""

from fastapi import APIRouter, Depends, Query
from fastapi.responses import Response

from api.v1.schemas.now_playing import NowPlayingReport, NowPlayingSnapshot
from core.dependencies import get_now_playing_service
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentUserDep
from services.now_playing_service import NowPlayingService

router = APIRouter(route_class=MsgSpecRoute, prefix="/now-playing", tags=["now-playing"])


@router.get("", response_model=NowPlayingSnapshot)
async def get_now_playing(
    current_user: CurrentUserDep,
    service: NowPlayingService = Depends(get_now_playing_service),
) -> NowPlayingSnapshot:
    return NowPlayingSnapshot(sessions=service.snapshot())


@router.post("", status_code=204)
async def report_now_playing(
    current_user: CurrentUserDep,
    body: NowPlayingReport = MsgSpecBody(NowPlayingReport),
    service: NowPlayingService = Depends(get_now_playing_service),
) -> Response:
    await service.update(
        key=f"{current_user.id}:{body.device}",
        user_id=current_user.id,
        user_name=current_user.display_name,
        source=body.source,
        device_name="Web",
        track_name=body.track_name,
        artist_name=body.artist_name,
        album_name=body.album_name,
        cover_url=body.cover_url,
        is_paused=body.is_paused,
        progress_ms=body.progress_ms,
        duration_ms=body.duration_ms,
    )
    return Response(status_code=204)


@router.delete("", status_code=204)
async def clear_now_playing(
    current_user: CurrentUserDep,
    device: str = Query("web"),
    service: NowPlayingService = Depends(get_now_playing_service),
) -> Response:
    await service.remove(f"{current_user.id}:{device}")
    return Response(status_code=204)
