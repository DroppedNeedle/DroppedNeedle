import logging

from fastapi import APIRouter, Depends, Header, HTTPException

from api.v1.schemas.wrapped import (
    ServerWrappedResponse,
    UserWrappedResponse,
    WrappedUsersResponse,
)
from core.dependencies import get_preferences_service, get_wrapped_service
from infrastructure.msgspec_fastapi import MsgSpecRoute
from services.preferences_service import PreferencesService
from services.wrapped_service import WrappedService

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/wrapped", tags=["wrapped"])


async def verify_wrapped_api_key(
    x_wrapped_api_key: str | None = Header(default=None),
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> None:
    expected = preferences_service.get_wrapped_settings().api_key
    if not expected or x_wrapped_api_key != expected:
        raise HTTPException(status_code=401, detail="Invalid or missing wrapped API key")


@router.get(
    "/users",
    response_model=WrappedUsersResponse,
    dependencies=[Depends(verify_wrapped_api_key)],
    description=(
        "List every DroppedNeedle user with their ListenBrainz link status. "
        "Callers should match users by email and only request /user/{user_id} "
        "for those with has_listenbrainz=true."
    ),
)
async def get_wrapped_users(
    wrapped_service: WrappedService = Depends(get_wrapped_service),
):
    return await wrapped_service.list_eligible_users()


@router.get(
    "/user/{user_id}",
    response_model=UserWrappedResponse,
    dependencies=[Depends(verify_wrapped_api_key)],
    description=(
        "Per-user 'year in review' summary: top artists/tracks/albums/genres "
        "for the current year, loved-track count, and an estimated total "
        "listen count. has_data is false (with empty lists) when the user has "
        "no linked ListenBrainz account or no listens for the year."
    ),
)
async def get_wrapped_user(
    user_id: str,
    wrapped_service: WrappedService = Depends(get_wrapped_service),
):
    return await wrapped_service.get_user_wrapped(user_id)


@router.get(
    "/server",
    response_model=ServerWrappedResponse,
    dependencies=[Depends(verify_wrapped_api_key)],
    description=(
        "Server-wide 'year in review' summary: total tracked users/listens, "
        "the sitewide top artist/album, and a per-user listener leaderboard."
    ),
)
async def get_wrapped_server(
    wrapped_service: WrappedService = Depends(get_wrapped_service),
):
    return await wrapped_service.get_server_wrapped()
