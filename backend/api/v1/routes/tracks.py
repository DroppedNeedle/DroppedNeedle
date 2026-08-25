"""Per-track download route (Phase 7): request a single track.

Orphan-track mode (album not in the library) is handled by the service, which
resolves the release group via MusicBrainz, auto-creates the album folder, and
downloads the single track (Q8-D). Authenticated + user-scoped.
"""

import logging

from fastapi import APIRouter, Depends

from api.v1.schemas.download import TrackRequestBody, TrackRequestResponse
from core.dependencies import get_request_service
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentUserDep

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/tracks", tags=["tracks"])


@router.post("/{recording_mbid}/request", response_model=TrackRequestResponse)
async def request_track(
    recording_mbid: str,
    current_user: CurrentUserDep,
    body: TrackRequestBody = MsgSpecBody(TrackRequestBody),
    service=Depends(get_request_service),
):
    return await service.request_track(
        recording_mbid,
        user_id=current_user.id,
        user_role=current_user.role,
        requested_by_name=current_user.display_name,
        artist_name=body.artist_name,
        track_title=body.track_title,
        album_title=body.album_title,
        duration_seconds=body.duration_seconds,
        release_group_mbid=body.release_group_mbid,
        artist_mbid=body.artist_mbid,
        release_mbid=body.release_id,
    )
