"""Auth routes: setup, local login/registration, sessions, admin user management."""

from __future__ import annotations

import asyncio, logging

from fastapi import APIRouter, Depends, HTTPException, Query, Request, Response, responses, status

from api.v1.schemas.auth import (
    AuthProvidersResponse,
    AuthResponse,
    CreateUserRequest,
    ImportCandidateListResponse,
    ImportUsersRequest,
    ImportUsersResponse,
    JellyfinLoginRequest,
    LoginRequest,
    OIDCAuthorizeResponse,
    OIDCExchangeRequest,
    PasswordRecoveryCodeResponse,
    PasswordRecoveryResetRequest,
    PlexPinResponse,
    PlexPollResponse,
    SessionListResponse,
    SetRoleRequest,
    SetupRequest,
    SetupStatusResponse,
    UserListResponse,
    UserResponse,
    import_candidate_to_response,
    UserQuotaOverrideBody,
    UserQuotaResponse,
    session_to_response,
    user_to_response,
)
from core.dependencies import get_quota_service
from core.dependencies.auth_providers import get_auth_service, get_plex_user_auth_service, get_jellyfin_user_auth_service, get_oidc_user_auth_service, get_user_import_service
from core.exceptions import AuthenticationError, ConfigurationError, ExternalServiceError, RegistrationError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep, CurrentTokenDep, CurrentUserDep
from services.oidc_user_auth_service import OIDCUserAuthService
from services.auth_service import AuthService
from services.jellyfin_user_auth_service import JellyfinUserAuthService
from services.plex_user_auth_service import PlexUserAuthService
from services.user_import_service import UserImportService

logger = logging.getLogger(__name__)

router = APIRouter(route_class = MsgSpecRoute, prefix = "/auth", tags = ["auth"])

_COOKIE_NAME = "droppedneedle_session"
_COOKIE_MAX_AGE = 30 * 24 * 60 * 60  # 30 days


def _set_session_cookie(response: Response, request: Request, token: str) -> None:
    """Attach an httpOnly session cookie. Marks Secure automatically when the
    request arrived over HTTPS (direct or via X-Forwarded-Proto)."""
    secure = (
        request.url.scheme == "https"
        or request.headers.get("x-forwarded-proto", "").lower() == "https"
    )
    response.set_cookie(
        key = _COOKIE_NAME,
        value = token,
        httponly = True,
        samesite = "lax",
        secure = secure,
        max_age = _COOKIE_MAX_AGE,
        path = "/api",
    )


def _clear_session_cookie(response: Response) -> None:
    response.delete_cookie(key = _COOKIE_NAME, path = "/api")


def _bearer_token(request: Request) -> str | None:
    auth = request.headers.get("Authorization", "")
    if auth.lower().startswith("bearer "):
        return auth[7:].strip()
    return None


@router.get("/setup/status", response_model = SetupStatusResponse)
async def setup_status(auth: AuthService = Depends(get_auth_service)) -> SetupStatusResponse:
    required = await auth.is_setup_required()
    return SetupStatusResponse(required = required)


@router.get("/providers", response_model = AuthProvidersResponse)
async def list_auth_providers(
    oidc_auth: OIDCUserAuthService = Depends(get_oidc_user_auth_service),
) -> AuthProvidersResponse:
    """Return which login methods are currently configured."""
    return oidc_auth.get_enabled_providers()


@router.post("/setup", response_model = AuthResponse, status_code = status.HTTP_201_CREATED)
async def setup(
    request: Request,
    response: Response,
    body: SetupRequest = MsgSpecBody(SetupRequest),
    auth: AuthService = Depends(get_auth_service),
) -> AuthResponse:
    if not await auth.is_setup_required():
        raise HTTPException(
            status_code = status.HTTP_409_CONFLICT,
            detail = "Setup has already been completed",
        )
    try:
        user, token = await auth.create_first_admin(
            display_name = body.display_name,
            username = body.username,
            email = body.email,
            password = body.password,
            user_agent = request.headers.get("User-Agent"),
        )
    except RegistrationError as e:
        # First-admin setup runs only against an empty user table, so the specific
        # reason (breached password, bad username/email) carries no enumeration risk
        # and is surfaced to the user - unlike the generic messages elsewhere here.
        logger.info(f"Setup rejected: {e}")
        raise HTTPException(status_code = status.HTTP_400_BAD_REQUEST, detail = str(e))

    _set_session_cookie(response, request, token)
    return AuthResponse(token = token, user = user_to_response(user))


@router.post("/login", response_model = AuthResponse)
async def login(
    request: Request,
    response: Response,
    body: LoginRequest = MsgSpecBody(LoginRequest),
    auth: AuthService = Depends(get_auth_service),
) -> AuthResponse:
    try:
        user, token = await auth.login_local(
            username = body.username,
            password = body.password,
            user_agent = request.headers.get("User-Agent"),
        )
    except AuthenticationError:
        raise HTTPException(
            status_code = status.HTTP_401_UNAUTHORIZED,
            detail = "Invalid username or password",
            headers = {"WWW-Authenticate": "Bearer"},
        )

    _set_session_cookie(response, request, token)
    return AuthResponse(token = token, user = user_to_response(user))


@router.post("/password-recovery/reset", status_code=status.HTTP_204_NO_CONTENT)
async def reset_password_with_recovery_code(
    body: PasswordRecoveryResetRequest = MsgSpecBody(PasswordRecoveryResetRequest),
    auth: AuthService = Depends(get_auth_service),
) -> None:
    try:
        await auth.reset_password_with_recovery_code(
            username=body.username,
            recovery_code=body.recovery_code,
            new_password=body.new_password,
        )
    except AuthenticationError:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid or expired recovery code",
        )
    except RegistrationError as exc:
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail=str(exc))


@router.post("/logout", status_code = status.HTTP_204_NO_CONTENT)
async def logout(
    request: Request,
    response: Response,
    auth: AuthService = Depends(get_auth_service),
) -> None:
    token = _bearer_token(request) or request.cookies.get(_COOKIE_NAME)
    if token:
        await auth.logout(token)
    _clear_session_cookie(response)


@router.post("/logout-all", status_code = status.HTTP_204_NO_CONTENT)
async def logout_all(
    current_user: CurrentUserDep,
    current_token: CurrentTokenDep,
    auth: AuthService = Depends(get_auth_service),
) -> None:
    await auth.logout_all(current_user.id, except_token_id = current_token.id)


@router.get("/me", response_model = UserResponse)
async def me(
    current_user: CurrentUserDep,
    auth: AuthService = Depends(get_auth_service),
) -> UserResponse:
    # Carry the linked providers so the client can offer change- vs set-password (D8).
    providers = await auth.get_provider_names_for_users([current_user.id])
    return user_to_response(current_user, providers.get(current_user.id))


@router.get("/sessions", response_model = SessionListResponse)
async def list_sessions(
    current_user: CurrentUserDep,
    auth: AuthService = Depends(get_auth_service),
) -> SessionListResponse:
    tokens = await auth.list_sessions(current_user.id)
    return SessionListResponse(sessions = [session_to_response(token) for token in tokens])


@router.delete("/sessions/{session_id}", status_code = status.HTTP_204_NO_CONTENT)
async def revoke_session(
    session_id: str,
    current_user: CurrentUserDep,
    auth: AuthService = Depends(get_auth_service),
) -> None:
    try:
        await auth.revoke_session(session_id, requesting_user_id = current_user.id)
    except AuthenticationError as e:
        logger.debug(f"Session revocation denied for user {current_user.id[:8]}: {e}")
        raise HTTPException(status_code = status.HTTP_403_FORBIDDEN, detail = "Forbidden")


@router.get("/admin/users", response_model = UserListResponse)
async def admin_list_users(
    _admin: CurrentAdminDep,
    auth: AuthService = Depends(get_auth_service),
    limit: int = Query(100, ge=1, le=500),
    offset: int = Query(0, ge=0),
) -> UserListResponse:
    users, total = await asyncio.gather(
        auth.list_users(limit=limit, offset=offset),
        auth.count_users(),
    )
    providers_by_user = await auth.get_provider_names_for_users([user.id for user in users])
    return UserListResponse(
        users = [user_to_response(user, providers_by_user.get(user.id)) for user in users],
        total = total,
    )


@router.post("/admin/users", response_model = UserResponse, status_code = status.HTTP_201_CREATED)
async def admin_create_user(
    _admin: CurrentAdminDep,
    body: CreateUserRequest = MsgSpecBody(CreateUserRequest),
    auth: AuthService = Depends(get_auth_service),
) -> UserResponse:
    try:
        user = await auth.admin_create_user(
            display_name = body.display_name,
            username = body.username,
            email = body.email,
            password = body.password,
            role = body.role,
        )
    except RegistrationError as e:
        logger.debug(f"Admin user creation failed: {e}")
        raise HTTPException(status_code = status.HTTP_409_CONFLICT, detail = "Could not create user")
    return user_to_response(user)


@router.post(
    "/admin/users/{user_id}/password-recovery",
    response_model=PasswordRecoveryCodeResponse,
)
async def admin_create_password_recovery_code(
    user_id: str,
    response: Response,
    current_admin: CurrentAdminDep,
    auth: AuthService = Depends(get_auth_service),
) -> PasswordRecoveryCodeResponse:
    try:
        recovery_code, expires_at = await auth.create_password_recovery_code(user_id)
    except AuthenticationError as exc:
        raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail=str(exc))
    logger.info(
        "Admin %s created a password recovery code for user %s",
        current_admin.id[:8],
        user_id[:8],
    )
    response.headers["Cache-Control"] = "no-store"
    return PasswordRecoveryCodeResponse(
        recovery_code=recovery_code,
        expires_at=expires_at,
    )


@router.get("/admin/import/jellyfin", response_model = ImportCandidateListResponse)
async def admin_import_list_jellyfin(
    _admin: CurrentAdminDep,
    importer: UserImportService = Depends(get_user_import_service),
) -> ImportCandidateListResponse:
    candidates = await importer.list_jellyfin_users()
    return ImportCandidateListResponse(
        users = [import_candidate_to_response(c) for c in candidates],
    )


@router.get("/admin/import/plex", response_model = ImportCandidateListResponse)
async def admin_import_list_plex(
    _admin: CurrentAdminDep,
    importer: UserImportService = Depends(get_user_import_service),
) -> ImportCandidateListResponse:
    candidates = await importer.list_plex_users()
    return ImportCandidateListResponse(
        users = [import_candidate_to_response(c) for c in candidates],
    )


@router.post("/admin/import", response_model = ImportUsersResponse)
async def admin_import_users(
    _admin: CurrentAdminDep,
    body: ImportUsersRequest = MsgSpecBody(ImportUsersRequest),
    importer: UserImportService = Depends(get_user_import_service),
) -> ImportUsersResponse:
    if body.provider not in ("jellyfin", "plex"):
        raise HTTPException(status_code = status.HTTP_400_BAD_REQUEST, detail = "Unsupported import provider")
    try:
        result = await importer.import_users(body.provider, body.provider_uids)
    except RegistrationError as e:
        logger.debug(f"User import failed: {e}")
        raise HTTPException(status_code = status.HTTP_409_CONFLICT, detail = "Could not import users")
    return ImportUsersResponse(
        imported = [user_to_response(u) for u in result.imported],
        linked = [user_to_response(u) for u in result.linked],
        skipped = result.skipped,
        total_imported = len(result.imported),
    )


@router.patch("/admin/users/{user_id}/role", status_code = status.HTTP_204_NO_CONTENT)
async def admin_set_role(
    user_id: str,
    current_admin: CurrentAdminDep,
    body: SetRoleRequest = MsgSpecBody(SetRoleRequest),
    auth: AuthService = Depends(get_auth_service),
) -> None:
    try:
        await auth.set_role(user_id, body.role, requesting_user_id = current_admin.id)
    except AuthenticationError as e:
        logger.debug(f"Role update failed for user {user_id[:8]}: {e}")
        raise HTTPException(status_code = status.HTTP_400_BAD_REQUEST, detail = "Could not update role")


@router.get("/admin/users/{user_id}/quota", response_model = UserQuotaResponse)
async def admin_get_user_quota(
    user_id: str,
    _admin: CurrentAdminDep,
    quota_service = Depends(get_quota_service),
) -> UserQuotaResponse:
    """One user's quota override, effective values and current usage
    (CollectionManagement Feature C; drives the SettingsUsers editor + bars)."""
    override = await quota_service.get_override(user_id)
    usage = await quota_service.usage_for(user_id)
    return UserQuotaResponse(
        user_id = user_id,
        override = UserQuotaOverrideBody(
            request_quota_count = override.request_quota_count if override else None,
            request_quota_days = override.request_quota_days if override else None,
            storage_quota_gb = override.storage_quota_gb if override else None,
        ),
        effective_request_quota_count = usage.quota.request_quota_count,
        effective_request_quota_days = usage.quota.request_quota_days,
        effective_storage_quota_gb = usage.quota.storage_quota_gb,
        requests_in_window = usage.requests_in_window,
        storage_bytes = usage.storage_bytes,
        exempt = usage.exempt,
    )


@router.put("/admin/users/{user_id}/quota", response_model = UserQuotaResponse)
async def admin_set_user_quota(
    user_id: str,
    _admin: CurrentAdminDep,
    body: UserQuotaOverrideBody = MsgSpecBody(UserQuotaOverrideBody),
    quota_service = Depends(get_quota_service),
) -> UserQuotaResponse:
    """Set (or clear, with all-null) a user's quota overrides; returns fresh state."""
    await quota_service.set_override(
        user_id,
        request_quota_count = body.request_quota_count,
        request_quota_days = body.request_quota_days,
        storage_quota_gb = body.storage_quota_gb,
    )
    return await admin_get_user_quota(user_id, _admin, quota_service)


@router.delete("/admin/users/{user_id}/sessions", status_code = status.HTTP_204_NO_CONTENT)
async def admin_revoke_user_sessions(
    user_id: str,
    _admin: CurrentAdminDep,
    auth: AuthService = Depends(get_auth_service),
) -> None:
    await auth.revoke_user_sessions(user_id)


@router.delete("/admin/users/{user_id}", status_code = status.HTTP_204_NO_CONTENT)
async def admin_delete_user(
    user_id: str,
    current_admin: CurrentAdminDep,
    auth: AuthService = Depends(get_auth_service),
) -> None:
    try:
        await auth.delete_user(user_id, requesting_user_id = current_admin.id)
    except AuthenticationError as e:
        logger.debug(f"User deletion failed for user {user_id[:8]}: {e}")
        raise HTTPException(status_code = status.HTTP_400_BAD_REQUEST, detail = "Could not delete user")


@router.post("/plex/pin", response_model = PlexPinResponse)
async def plex_login_pin(plex_auth: PlexUserAuthService = Depends(get_plex_user_auth_service)) -> PlexPinResponse:
    pin_id, auth_url = await plex_auth.create_login_pin()
    return PlexPinResponse(pin_id = pin_id, auth_url = auth_url)


@router.get("/plex/poll", response_model = PlexPollResponse)
async def plex_login_poll(
    pin_id: int,
    request: Request,
    response: Response,
    plex_auth: PlexUserAuthService = Depends(get_plex_user_auth_service),
) -> PlexPollResponse:
    try:
        result = await plex_auth.poll_and_login(
            pin_id, user_agent = request.headers.get("User-Agent")
        )
    except AuthenticationError as e:
        logger.debug(f"Plex login rejected: {e}")
        raise HTTPException(status_code = status.HTTP_403_FORBIDDEN, detail = "Access denied")
    if result is None:
        return PlexPollResponse(completed = False)
    user, token = result
    _set_session_cookie(response, request, token)
    return PlexPollResponse(completed = True, token = token, user = user_to_response(user))


@router.post("/jellyfin/login", response_model = AuthResponse)
async def jellyfin_login(
    request: Request,
    response: Response,
    body: JellyfinLoginRequest = MsgSpecBody(JellyfinLoginRequest),
    jellyfin_auth: JellyfinUserAuthService = Depends(get_jellyfin_user_auth_service),
) -> AuthResponse:
    try:
        user, token = await jellyfin_auth.login(
            username = body.username,
            password = body.password,
            user_agent = request.headers.get("User-Agent"),
        )
    except AuthenticationError:
        raise HTTPException(
            status_code = status.HTTP_401_UNAUTHORIZED,
            detail = "Invalid credentials",
            headers = {"WWW-Authenticate": "Bearer"},
        )
    except ExternalServiceError:
        raise HTTPException(
            status_code = status.HTTP_503_SERVICE_UNAVAILABLE,
            detail = "Jellyfin unavailable",
        )
    _set_session_cookie(response, request, token)
    return AuthResponse(token = token, user = user_to_response(user))


@router.post("/oidc/authorize", response_model = OIDCAuthorizeResponse)
async def oidc_authorize(oidc_auth: OIDCUserAuthService = Depends(get_oidc_user_auth_service)) -> OIDCAuthorizeResponse:
    try:
        url = await oidc_auth.build_authorize_url()
    except ConfigurationError:
        raise HTTPException(status_code = status.HTTP_503_SERVICE_UNAVAILABLE, detail = "OIDC is not configured")
    return OIDCAuthorizeResponse(redirect_url = url)


@router.get("/oidc/callback")
async def oidc_callback(
    code: str,
    state: str,
    request: Request,
    oidc_auth: OIDCUserAuthService = Depends(get_oidc_user_auth_service),
):
    try:
        exchange_code = await oidc_auth.handle_callback(
            code = code,
            state = state,
            user_agent = request.headers.get("User-Agent"),
        )
    except AuthenticationError:
        raise HTTPException(status_code = status.HTTP_401_UNAUTHORIZED, detail = "OIDC authentication failed")
    except ExternalServiceError:
        raise HTTPException(status_code = status.HTTP_503_SERVICE_UNAVAILABLE, detail = "OIDC provider unavailable")
    return responses.RedirectResponse(url = f"/auth/callback?code={exchange_code}")


@router.post("/oidc/exchange", response_model = AuthResponse)
async def oidc_exchange(
    request: Request,
    response: Response,
    body: OIDCExchangeRequest = MsgSpecBody(OIDCExchangeRequest),
    oidc_auth: OIDCUserAuthService = Depends(get_oidc_user_auth_service),
) -> AuthResponse:
    try:
        user, token = await oidc_auth.exchange_code(body.code)
    except AuthenticationError:
        raise HTTPException(status_code = status.HTTP_401_UNAUTHORIZED, detail = "Invalid or expired code")
    _set_session_cookie(response, request, token)
    return AuthResponse(token = token, user = user_to_response(user))
