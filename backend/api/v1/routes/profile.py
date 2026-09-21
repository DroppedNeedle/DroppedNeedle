import asyncio
import logging
import time
from pathlib import Path
from fastapi import APIRouter, Depends, HTTPException, UploadFile, File
from fastapi.responses import FileResponse

from api.v1.schemas.auth import UserResponse, user_to_response
from api.v1.schemas.profile import (
    ChangePasswordRequest,
    EmailUpdateRequest,
    LibraryStats,
    ProfileResponse,
    ProfileUpdateRequest,
    ServiceConnection,
    SetPasswordRequest,
    UsernameUpdateRequest,
)
from core.dependencies import (
    get_preferences_service,
    get_jellyfin_library_service,
    get_local_files_service,
    get_navidrome_library_service,
    get_compat_avatar_service,
)
from core.dependencies.auth_providers import get_auth_service
from core.config import get_settings
from core.password_policy import require_password_login_enabled
from core.exceptions import AuthenticationError, RegistrationError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentUserDep
from services.auth_service import AuthService
from services.preferences_service import PreferencesService
from services.jellyfin_library_service import JellyfinLibraryService
from services.local_files_service import LocalFilesService
from services.navidrome_library_service import NavidromeLibraryService
from services.compat.avatar_service import CompatAvatarService

logger = logging.getLogger(__name__)

AVATAR_DIR_NAME = "avatars"
ALLOWED_IMAGE_TYPES = {"image/jpeg", "image/png", "image/webp", "image/gif"}
MAX_AVATAR_SIZE = 5 * 1024 * 1024  # 5 MB
_EXT_BY_TYPE = {
    "image/jpeg": ".jpg",
    "image/png": ".png",
    "image/webp": ".webp",
    "image/gif": ".gif",
}
_MEDIA_TYPE_BY_EXT = {
    ".jpg": "image/jpeg",
    ".png": "image/png",
    ".webp": "image/webp",
    ".gif": "image/gif",
}

router = APIRouter(route_class=MsgSpecRoute, prefix="/profile", tags=["profile"])


def _raise_http(exc: Exception) -> HTTPException:
    """Map a self-service domain error to its HTTP status (mirrors auth.py).

    The messages here are user-facing on purpose (shown inline by the profile UI)
    and never contain secrets, unlike the deliberately-vague admin-create errors.
    """
    if isinstance(exc, AuthenticationError):
        return HTTPException(status_code=401, detail=str(exc))
    return HTTPException(status_code=400, detail=str(exc))


async def _user_response(auth: AuthService, user) -> UserResponse:
    names = await auth.get_provider_names_for_users([user.id])
    return user_to_response(user, names.get(user.id))


def _get_avatar_dir() -> Path:
    avatar_dir = get_settings().cache_dir / AVATAR_DIR_NAME
    avatar_dir.mkdir(parents=True, exist_ok=True)
    return avatar_dir


@router.get("", response_model=ProfileResponse)
async def get_profile(
    current_user: CurrentUserDep,
    auth: AuthService = Depends(get_auth_service),
    preferences: PreferencesService = Depends(get_preferences_service),
    jellyfin_service: JellyfinLibraryService = Depends(get_jellyfin_library_service),
    local_service: LocalFilesService = Depends(get_local_files_service),
    navidrome_service: NavidromeLibraryService = Depends(get_navidrome_library_service),
) -> ProfileResponse:
    services: list[ServiceConnection] = []

    jellyfin_conn = preferences.get_jellyfin_connection()
    services.append(ServiceConnection(
        name="Jellyfin",
        enabled=jellyfin_conn.enabled,
        username=jellyfin_conn.user_id,
        url=jellyfin_conn.jellyfin_url,
    ))

    lb_conn = preferences.get_listenbrainz_connection()
    services.append(ServiceConnection(
        name="ListenBrainz",
        enabled=lb_conn.enabled,
        username=lb_conn.username,
        url="https://listenbrainz.org",
    ))

    lastfm_conn = preferences.get_lastfm_connection()
    services.append(ServiceConnection(
        name="Last.fm",
        enabled=lastfm_conn.enabled,
        username=lastfm_conn.username,
        url="https://www.last.fm",
    ))

    navidrome_conn = preferences.get_navidrome_connection()
    services.append(ServiceConnection(
        name="Navidrome",
        enabled=navidrome_conn.enabled,
        username=navidrome_conn.username,
        url=navidrome_conn.navidrome_url,
    ))

    plex_conn = preferences.get_plex_connection()
    services.append(ServiceConnection(
        name="Plex",
        enabled=plex_conn.enabled,
        # the app-level Plex connection is token-based; there is no username to show
        username="",
        url=plex_conn.plex_url,
    ))

    async def _fetch_jellyfin_stats() -> LibraryStats | None:
        if not jellyfin_conn.enabled:
            return None
        try:
            s = await jellyfin_service.get_stats()
            return LibraryStats(source="Jellyfin", total_tracks=s.total_tracks, total_albums=s.total_albums, total_artists=s.total_artists)
        except Exception as e:  # noqa: BLE001
            logger.warning("Failed to fetch Jellyfin stats for profile: %s", e)
            return None

    async def _fetch_local_stats() -> LibraryStats | None:
        try:
            s = await local_service.get_storage_stats()
            if s.total_tracks == 0:
                return None
            return LibraryStats(source="Local Files", total_tracks=s.total_tracks, total_albums=s.total_albums, total_artists=s.total_artists, total_size_bytes=s.total_size_bytes, total_size_human=s.total_size_human)
        except Exception as e:  # noqa: BLE001
            logger.warning("Failed to fetch Local Files stats for profile: %s", e)
            return None

    async def _fetch_navidrome_stats() -> LibraryStats | None:
        if not navidrome_conn.enabled:
            return None
        try:
            s = await navidrome_service.get_stats()
            return LibraryStats(source="Navidrome", total_tracks=s.total_tracks, total_albums=s.total_albums, total_artists=s.total_artists)
        except Exception as e:  # noqa: BLE001
            logger.warning("Failed to fetch Navidrome stats for profile: %s", e)
            return None

    results = await asyncio.gather(_fetch_jellyfin_stats(), _fetch_local_stats(), _fetch_navidrome_stats())
    library_stats_list = [r for r in results if r is not None]

    provider_names = await auth.get_provider_names_for_users([current_user.id])

    return ProfileResponse(
        display_name=current_user.display_name,
        avatar_url=current_user.avatar_url or "",
        username=current_user.username,
        username_display=current_user.username_display,
        email=current_user.email,
        providers=provider_names.get(current_user.id) or [],
        services=services,
        library_stats=library_stats_list,
    )


@router.put("", response_model=UserResponse)
async def update_profile(
    current_user: CurrentUserDep,
    body: ProfileUpdateRequest = MsgSpecBody(ProfileUpdateRequest),
    auth: AuthService = Depends(get_auth_service),
) -> UserResponse:
    try:
        user = await auth.update_display_name(current_user.id, body.display_name or "")
    except (RegistrationError, AuthenticationError) as e:
        raise _raise_http(e)
    return await _user_response(auth, user)


@router.put("/username", response_model=UserResponse)
async def update_username(
    current_user: CurrentUserDep,
    body: UsernameUpdateRequest = MsgSpecBody(UsernameUpdateRequest),
    auth: AuthService = Depends(get_auth_service),
) -> UserResponse:
    try:
        user = await auth.update_username(current_user.id, body.username)
    except (RegistrationError, AuthenticationError) as e:
        raise _raise_http(e)
    return await _user_response(auth, user)


@router.put("/email", response_model=UserResponse)
async def update_email(
    current_user: CurrentUserDep,
    body: EmailUpdateRequest = MsgSpecBody(EmailUpdateRequest),
    auth: AuthService = Depends(get_auth_service),
) -> UserResponse:
    try:
        user = await auth.update_email(current_user.id, body.email)
    except (RegistrationError, AuthenticationError) as e:
        raise _raise_http(e)
    return await _user_response(auth, user)


@router.post(
    "/password",
    response_model=UserResponse,
    dependencies=[Depends(require_password_login_enabled)],
)
async def change_password(
    current_user: CurrentUserDep,
    body: ChangePasswordRequest = MsgSpecBody(ChangePasswordRequest),
    auth: AuthService = Depends(get_auth_service),
) -> UserResponse:
    try:
        user = await auth.change_password(current_user.id, body.current_password, body.new_password)
    except (RegistrationError, AuthenticationError) as e:
        raise _raise_http(e)
    return await _user_response(auth, user)


@router.post(
    "/set-password",
    response_model=UserResponse,
    dependencies=[Depends(require_password_login_enabled)],
)
async def set_local_password(
    current_user: CurrentUserDep,
    body: SetPasswordRequest = MsgSpecBody(SetPasswordRequest),
    auth: AuthService = Depends(get_auth_service),
) -> UserResponse:
    try:
        user = await auth.set_local_password(current_user.id, body.new_password)
    except (RegistrationError, AuthenticationError) as e:
        raise _raise_http(e)
    return await _user_response(auth, user)


@router.post("/avatar", response_model=UserResponse)
async def upload_avatar(
    current_user: CurrentUserDep,
    file: UploadFile = File(...),
    auth: AuthService = Depends(get_auth_service),
) -> UserResponse:
    if file.content_type not in ALLOWED_IMAGE_TYPES:
        raise HTTPException(status_code=400, detail="Invalid image type. Allowed: JPEG, PNG, WebP, GIF")

    data = await file.read()
    if len(data) > MAX_AVATAR_SIZE:
        raise HTTPException(status_code=400, detail="Image too large. Maximum size is 5 MB")

    ext = _EXT_BY_TYPE.get(file.content_type, ".jpg")
    avatar_dir = _get_avatar_dir()

    # Replace any previous avatar this user had (a different extension may linger).
    for old_file in avatar_dir.glob(f"{current_user.id}.*"):
        try:
            old_file.unlink()
        except OSError:
            pass

    (avatar_dir / f"{current_user.id}{ext}").write_bytes(data)

    # The served URL is a stable per-user path with a 1h cache; append a version so a
    # re-upload changes the URL and busts the browser image cache everywhere it renders
    # (topbar, hero, admin list) - the route ignores the query param when serving.
    version = int(time.time() * 1000)
    user = await auth.update_avatar(
        current_user.id, f"/api/v1/profile/avatar/{current_user.id}?v={version}"
    )
    return await _user_response(auth, user)


@router.get("/avatar/{user_id}")
async def get_avatar(
    user_id: str,
    current_user: CurrentUserDep,
    avatars: CompatAvatarService = Depends(get_compat_avatar_service),
):
    # Self-or-admin (D9): admins read any user's avatar so the admin user list renders.
    if current_user.id != user_id and current_user.role != "admin":
        raise HTTPException(status_code=403, detail="Forbidden")

    resolved = await asyncio.to_thread(avatars.resolve, user_id)
    if resolved is not None:
        file_path, media_type = resolved
        return FileResponse(
            file_path,
            media_type=media_type,
            headers={"Cache-Control": "private, max-age=3600"},
        )
    raise HTTPException(status_code=404, detail="No avatar found")
