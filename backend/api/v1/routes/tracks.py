"""Per-track download route (Phase 7): request a single track.

Orphan-track mode (album not in the library) is handled by the service, which
resolves the release group via MusicBrainz, auto-creates the album folder, and
downloads the single track (Q8-D). Authenticated + user-scoped.
"""

import logging

from fastapi import APIRouter, Depends

from api.v1.schemas.download import TrackRequestBody, TrackRequestResponse
from core.dependencies import get_download_service, get_quota_service
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentUserDep
from services.native.download_service import ALREADY_IN_LIBRARY

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/tracks", tags=["tracks"])


@router.post("/{recording_mbid}/request", response_model=TrackRequestResponse)
async def request_track(
    recording_mbid: str,
    current_user: CurrentUserDep,
    body: TrackRequestBody = MsgSpecBody(TrackRequestBody),
    service=Depends(get_download_service),
    quota=Depends(get_quota_service),
):
    # Track asks bypass the approval queue (existing behaviour) but still count
    # toward the rolling request quota (Feature C layer 1, D20) - their download
    # task IS the ask, so the gate runs at this submit point.
    await quota.check_request_quota(current_user.id, current_user.role)
    task_id = await service.request_track(
        user_id=current_user.id,
        recording_mbid=recording_mbid,
        artist_name=body.artist_name,
        track_title=body.track_title,
        album_title=body.album_title,
        duration_seconds=body.duration_seconds,
        release_group_mbid=body.release_group_mbid,
        artist_mbid=body.artist_mbid,
        release_mbid=body.release_id,
    )
    if task_id == ALREADY_IN_LIBRARY:
        return TrackRequestResponse(status="already_in_library")
    return TrackRequestResponse(status="queued", task_id=task_id)
