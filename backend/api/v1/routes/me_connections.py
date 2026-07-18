"""Per-user external connection + scrobble-preference endpoints.

Endpoints are self-scoped to ``CurrentUserDep``; the encrypted ``connection_data``
ciphertext is never serialized out - only service/enabled/username.
"""

import asyncio
import base64
import logging
import secrets
import uuid
from datetime import datetime, timedelta, timezone
from urllib.parse import urlencode

import httpx
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi import responses as fastapi_responses

from api.v1.schemas.me_connections import (
    ConnectionActionResponse,
    ConnectionsResponse,
    ConnectionStatus,
    ListenBrainzConnectRequest,
    MediaServerConnectRequest,
    PersonalMixRefreshResponse,
    PlexLinkPinResponse,
    PlexLinkPollResponse,
    ScrobblePreferences,
    ScrobblePreferencesUpdate,
    SpotifyAuthUrlResponse,
)
from api.v1.schemas.section_prefs import (
    SectionPrefItem,
    SectionPrefsResponse,
    SectionPrefsUpdate,
)
from api.v1.schemas.settings import (
    LastFmAuthSessionRequest,
    LastFmAuthSessionResponse,
    LastFmAuthTokenResponse,
    ListenBrainzConnectionSettings,
)
from core.dependencies import (
    get_auth_store,
    get_jellyfin_user_auth_service,
    get_lastfm_auth_service,
    get_now_playing_service,
    get_per_user_client_factory,
    get_personal_mix_service,
    get_plex_user_auth_service,
    get_preferences_service,
    get_settings_service,
    get_sse_publisher,
    get_user_connections_store,
    get_user_listening_prefs_store,
    get_user_section_prefs_store,
)
from core.exceptions import (
    AuthenticationError,
    ConfigurationError,
    ExternalServiceError,
    TokenNotAuthorizedError,
)
from core.task_registry import TaskRegistry
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from infrastructure.persistence.auth_store import AuthStore
from infrastructure.persistence.user_connections_store import UserConnectionsStore
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore
from infrastructure.persistence.user_section_prefs_store import UserSectionPrefsStore
from middleware import CurrentUserDep
from services.jellyfin_user_auth_service import JellyfinUserAuthService
from services.lastfm_auth_service import LastFmAuthService
from services.per_user_client_factory import PerUserClientFactory
from services.personal_mix_service import PersonalMixService
from services.plex_user_auth_service import PlexUserAuthService
from services.preferences_service import PreferencesService
from services.section_catalog import Page, sections_for, valid_keys
from services.settings_service import SettingsService

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/me", tags=["me"])

_SUPPORTED_SERVICES = ("lastfm", "listenbrainz", "spotify", "navidrome", "jellyfin", "plex")
_SPOTIFY_SCOPES = "playlist-read-private playlist-read-collaborative user-read-private"


@router.get("/connections", response_model=ConnectionsResponse)
async def list_connections(
    current_user: CurrentUserDep,
    store: UserConnectionsStore = Depends(get_user_connections_store),
) -> ConnectionsResponse:
    records = await store.list_for_user(current_user.id)
    return ConnectionsResponse(
        connections=[
            ConnectionStatus(service=r.service, enabled=r.enabled, username=r.username)
            for r in records
        ]
    )


@router.get("/scrobble-preferences", response_model=ScrobblePreferences)
async def get_scrobble_preferences(
    current_user: CurrentUserDep,
    prefs_store: UserListeningPrefsStore = Depends(get_user_listening_prefs_store),
    personal_mix_service: PersonalMixService = Depends(get_personal_mix_service),
) -> ScrobblePreferences:
    prefs = await prefs_store.get(current_user.id)
    return ScrobblePreferences(
        scrobble_to_lastfm=prefs.scrobble_to_lastfm,
        scrobble_to_listenbrainz=prefs.scrobble_to_listenbrainz,
        navidrome_handles_external_scrobbles=prefs.navidrome_handles_external_scrobbles,
        primary_music_source=prefs.primary_music_source,
        now_playing_visibility=prefs.now_playing_visibility,
        auto_request_personal_mix=prefs.auto_request_personal_mix,
        auto_request_state=await personal_mix_service.get_auto_request_state(
            current_user.id, current_user.role
        ),
    )


@router.put("/scrobble-preferences", response_model=ScrobblePreferences)
async def update_scrobble_preferences(
    current_user: CurrentUserDep,
    body: ScrobblePreferencesUpdate = MsgSpecBody(ScrobblePreferencesUpdate),
    prefs_store: UserListeningPrefsStore = Depends(get_user_listening_prefs_store),
    now_playing_service=Depends(get_now_playing_service),
    personal_mix_service: PersonalMixService = Depends(get_personal_mix_service),
) -> ScrobblePreferences:
    before = await prefs_store.get(current_user.id)
    await prefs_store.upsert(
        current_user.id,
        scrobble_to_lastfm=body.scrobble_to_lastfm,
        scrobble_to_listenbrainz=body.scrobble_to_listenbrainz,
        navidrome_handles_external_scrobbles=body.navidrome_handles_external_scrobbles,
        primary_music_source=body.primary_music_source,
        now_playing_visibility=body.now_playing_visibility,
        auto_request_personal_mix=body.auto_request_personal_mix,
    )
    # apply the privacy change to the live presence feed immediately
    if body.now_playing_visibility is not None:
        await now_playing_service.set_visibility(
            current_user.id, body.now_playing_visibility
        )
    # standing-grant bookkeeping: a non-admin enabling auto-request enters the
    # admin approval queue (mirrors follow auto-download). Only a real value
    # change counts - a full-object PUT resending true must not re-queue (and
    # thereby silently suspend) an already-approved grant.
    if (
        body.auto_request_personal_mix is not None
        and body.auto_request_personal_mix != before.auto_request_personal_mix
    ):
        await personal_mix_service.on_auto_request_toggled(
            current_user.id, current_user.role, body.auto_request_personal_mix
        )
    prefs = await prefs_store.get(current_user.id)
    return ScrobblePreferences(
        scrobble_to_lastfm=prefs.scrobble_to_lastfm,
        scrobble_to_listenbrainz=prefs.scrobble_to_listenbrainz,
        navidrome_handles_external_scrobbles=prefs.navidrome_handles_external_scrobbles,
        primary_music_source=prefs.primary_music_source,
        now_playing_visibility=prefs.now_playing_visibility,
        auto_request_personal_mix=prefs.auto_request_personal_mix,
        auto_request_state=await personal_mix_service.get_auto_request_state(
            current_user.id, current_user.role
        ),
    )


async def _background_mix_refresh(svc: PersonalMixService, user_id: str) -> None:
    try:
        result = await svc.build_for_user(user_id, force=True)
    except Exception:  # noqa: BLE001 - background task; log, never raise
        logger.error(
            "Background personal-mix refresh failed for user %s", user_id, exc_info=True
        )
        return
    # tell the profile card + playlist views the build finished; the event_id lets
    # the client de-dupe the SSEPublisher's replay-to-new-subscribers
    try:
        await get_sse_publisher().publish(
            f"user:{user_id}",
            "personal_mix_refreshed",
            {
                "playlist_id": result.playlist_id,
                "track_count": result.track_count,
                "requested_albums": result.requested_albums,
                "skipped": result.skipped,
                "reason": result.reason,
                "event_id": uuid.uuid4().hex,
            },
        )
    except Exception:  # noqa: BLE001
        logger.warning(
            "Failed to signal personal-mix refresh completion for %s", user_id, exc_info=True
        )


@router.post("/personal-mix/refresh", response_model=PersonalMixRefreshResponse)
async def refresh_personal_mix(
    current_user: CurrentUserDep,
    personal_mix_service: PersonalMixService = Depends(get_personal_mix_service),
    client_factory: PerUserClientFactory = Depends(get_per_user_client_factory),
) -> PersonalMixRefreshResponse:
    # a cold build sits behind the MusicBrainz 1/s limiter for minutes, far past
    # proxy timeouts - answer fast and build in the background (spotify.py pattern)
    if not await client_factory.is_listenbrainz_linked(current_user.id):
        raise HTTPException(
            status_code=400, detail="Connect ListenBrainz first to build Your Weekly Mix"
        )
    task_key = f"personal-mix:refresh:{current_user.id}"
    registry = TaskRegistry.get_instance()
    if registry.is_running(task_key):
        return PersonalMixRefreshResponse(status="already_running")
    task = asyncio.create_task(
        _background_mix_refresh(personal_mix_service, current_user.id)
    )
    try:
        registry.register(task_key, task)
    except RuntimeError:
        # lost a concurrent-POST race: an incumbent build already holds the key,
        # so drop this duplicate instead of running it untracked
        task.cancel()
        return PersonalMixRefreshResponse(status="already_running")
    return PersonalMixRefreshResponse(status="started")


@router.post("/connections/lastfm/auth/token", response_model=LastFmAuthTokenResponse)
async def lastfm_request_token(
    current_user: CurrentUserDep,
    auth_service: LastFmAuthService = Depends(get_lastfm_auth_service),
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> LastFmAuthTokenResponse:
    # app api_key/shared_secret are global; only the resulting session_key is per-user
    settings = preferences_service.get_lastfm_connection()
    if not settings.api_key or not settings.shared_secret:
        raise HTTPException(status_code=400, detail="Last.fm is not configured by the administrator yet")
    try:
        token, auth_url = await auth_service.request_token(settings.api_key)
        return LastFmAuthTokenResponse(token=token, auth_url=auth_url)
    except ConfigurationError as e:
        logger.warning("Last.fm auth token request failed (config): %s", e)
        raise HTTPException(status_code=400, detail="Last.fm settings are incomplete or invalid")
    except ExternalServiceError as e:
        logger.warning("Last.fm auth token request failed (external): %s", e)
        raise HTTPException(status_code=502, detail="Couldn't reach Last.fm for a sign-in token")


@router.post("/connections/lastfm/auth/session", response_model=LastFmAuthSessionResponse)
async def lastfm_exchange_session(
    current_user: CurrentUserDep,
    request: LastFmAuthSessionRequest = MsgSpecBody(LastFmAuthSessionRequest),
    auth_service: LastFmAuthService = Depends(get_lastfm_auth_service),
    store: UserConnectionsStore = Depends(get_user_connections_store),
) -> LastFmAuthSessionResponse:
    try:
        username, session_key, _ = await auth_service.exchange_session(request.token)
        # per-user persistence, not preferences_service.save_lastfm_connection
        await store.upsert(
            current_user.id, "lastfm", {"session_key": session_key, "username": username}
        )
        return LastFmAuthSessionResponse(
            username=username, success=True, message=f"Connected as {username}"
        )
    except TokenNotAuthorizedError:
        raise HTTPException(
            status_code=502,
            detail="Last.fm access hasn't been approved yet. Authorize it, then try again.",
        )
    except ExternalServiceError as e:
        logger.warning("Last.fm session exchange failed (external): %s", e)
        raise HTTPException(status_code=502, detail="Couldn't finish the Last.fm sign-in. Please try again.")
    except ConfigurationError as e:
        logger.warning("Last.fm session exchange rejected: %s", e)
        raise HTTPException(status_code=422, detail="Last.fm configuration error. Check your settings and try again.")


@router.put("/connections/listenbrainz", response_model=ConnectionStatus)
async def connect_listenbrainz(
    current_user: CurrentUserDep,
    body: ListenBrainzConnectRequest = MsgSpecBody(ListenBrainzConnectRequest),
    settings_service: SettingsService = Depends(get_settings_service),
    store: UserConnectionsStore = Depends(get_user_connections_store),
) -> ConnectionStatus:
    # username drives every per-user ListenBrainz read; without it a "connected"
    # account yields silently-empty discovery
    if not body.username.strip():
        raise HTTPException(status_code=400, detail="A ListenBrainz username is required")
    result = await settings_service.verify_listenbrainz(
        ListenBrainzConnectionSettings(
            username=body.username, user_token=body.user_token, enabled=True
        )
    )
    if not result.valid:
        raise HTTPException(status_code=400, detail=result.message)
    await store.upsert(
        current_user.id, "listenbrainz", {"user_token": body.user_token, "username": body.username}
    )
    return ConnectionStatus(service="listenbrainz", enabled=True, username=body.username)


@router.get("/connections/spotify/auth/url", response_model=SpotifyAuthUrlResponse)
async def spotify_auth_url(
    request: Request,
    current_user: CurrentUserDep,
    auth_store: AuthStore = Depends(get_auth_store),
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> SpotifyAuthUrlResponse:
    settings = preferences_service.get_spotify_settings_raw()
    if not settings.enabled or not settings.client_id or not settings.client_secret:
        raise HTTPException(status_code=400, detail="Spotify is not configured by the administrator")
    state = secrets.token_urlsafe(32)
    await auth_store.store_spotify_state(state, current_user.id)
    redirect_uri = str(request.base_url).rstrip("/") + "/api/v1/me/connections/spotify/auth/callback"
    auth_url = "https://accounts.spotify.com/authorize?" + urlencode({
        "client_id": settings.client_id,
        "response_type": "code",
        "redirect_uri": redirect_uri,
        "scope": _SPOTIFY_SCOPES,
        "state": state,
    })
    return SpotifyAuthUrlResponse(auth_url=auth_url)


@router.get("/connections/spotify/auth/callback")
async def spotify_auth_callback(
    request: Request,
    auth_store: AuthStore = Depends(get_auth_store),
    store: UserConnectionsStore = Depends(get_user_connections_store),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    code: str | None = None,
    state: str | None = None,
    error: str | None = None,
) -> fastapi_responses.RedirectResponse:
    if error or not code or not state:
        return fastapi_responses.RedirectResponse("/profile?spotify=error")

    user_id = await auth_store.consume_spotify_state(state)
    if not user_id:
        return fastapi_responses.RedirectResponse("/profile?spotify=error&reason=state")

    settings = preferences_service.get_spotify_settings_raw()
    redirect_uri = str(request.base_url).rstrip("/") + "/api/v1/me/connections/spotify/auth/callback"
    basic = base64.b64encode(f"{settings.client_id}:{settings.client_secret}".encode()).decode()

    try:
        async with httpx.AsyncClient(timeout=15) as client:
            token_resp = await client.post(
                "https://accounts.spotify.com/api/token",
                data={"grant_type": "authorization_code", "code": code, "redirect_uri": redirect_uri},
                headers={"Authorization": f"Basic {basic}"},
            )
            if token_resp.status_code != 200:
                logger.warning(f"Spotify token exchange failed: status={token_resp.status_code}")
                return fastapi_responses.RedirectResponse("/profile?spotify=error&reason=token")
            token_data = token_resp.json()

            me_resp = await client.get(
                "https://api.spotify.com/v1/me",
                headers={"Authorization": f"Bearer {token_data['access_token']}"},
            )
    except Exception:  # noqa: BLE001
        logger.exception("Spotify OAuth callback failed")
        return fastapi_responses.RedirectResponse("/profile?spotify=error&reason=network")

    spotify_user = me_resp.json() if me_resp.status_code == 200 else {}
    expires_at = (
        datetime.now(timezone.utc) + timedelta(seconds=token_data.get("expires_in", 3600))
    ).isoformat()

    await store.upsert(user_id, "spotify", {
        "access_token": token_data["access_token"],
        "refresh_token": token_data.get("refresh_token", ""),
        "expires_at": expires_at,
        "username": spotify_user.get("display_name") or spotify_user.get("id") or "Spotify",
        "spotify_user_id": spotify_user.get("id", ""),
    })
    return fastapi_responses.RedirectResponse("/profile?spotify=connected")


@router.put("/connections/navidrome", response_model=ConnectionStatus)
async def connect_navidrome(
    current_user: CurrentUserDep,
    body: MediaServerConnectRequest = MsgSpecBody(MediaServerConnectRequest),
    client_factory: PerUserClientFactory = Depends(get_per_user_client_factory),
    store: UserConnectionsStore = Depends(get_user_connections_store),
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> ConnectionStatus:
    nd = preferences_service.get_navidrome_connection_raw()
    if not (nd.enabled and nd.navidrome_url):
        raise HTTPException(status_code=400, detail="Navidrome is not configured by the administrator")
    if not body.username.strip() or not body.password:
        raise HTTPException(status_code=400, detail="A Navidrome username and password are required")
    ok, message = await client_factory.validate_navidrome_credentials(body.username, body.password)
    if not ok:
        raise HTTPException(status_code=400, detail=message)
    # Subsonic token auth needs the raw password per request; the store Fernet-encrypts it
    await store.upsert(
        current_user.id, "navidrome", {"username": body.username, "password": body.password}
    )
    await client_factory.invalidate_playlist_cache(current_user.id, "navidrome")
    return ConnectionStatus(service="navidrome", enabled=True, username=body.username)


@router.put("/connections/jellyfin", response_model=ConnectionStatus)
async def connect_jellyfin(
    current_user: CurrentUserDep,
    body: MediaServerConnectRequest = MsgSpecBody(MediaServerConnectRequest),
    auth_service: JellyfinUserAuthService = Depends(get_jellyfin_user_auth_service),
    client_factory: PerUserClientFactory = Depends(get_per_user_client_factory),
    store: UserConnectionsStore = Depends(get_user_connections_store),
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> ConnectionStatus:
    jf = preferences_service.get_jellyfin_connection()
    if not (jf.enabled and jf.jellyfin_url):
        raise HTTPException(status_code=400, detail="Jellyfin is not configured by the administrator")
    if not body.username.strip() or not body.password:
        raise HTTPException(status_code=400, detail="A Jellyfin username and password are required")
    try:
        profile = await auth_service.authenticate_credentials(body.username, body.password)
    except AuthenticationError as e:
        raise HTTPException(status_code=400, detail=str(e))
    except ExternalServiceError:
        raise HTTPException(status_code=502, detail="Couldn't reach Jellyfin to verify the account")
    # only the exchanged access token is persisted - never the password
    await store.upsert(
        current_user.id,
        "jellyfin",
        {
            "access_token": profile["access_token"],
            "jellyfin_user_id": profile["jellyfin_user_id"],
            "username": profile["username"],
        },
    )
    await client_factory.invalidate_playlist_cache(current_user.id, "jellyfin")
    return ConnectionStatus(service="jellyfin", enabled=True, username=profile["username"])


@router.post("/connections/plex/auth/pin", response_model=PlexLinkPinResponse)
async def plex_link_pin(
    current_user: CurrentUserDep,
    auth_service: PlexUserAuthService = Depends(get_plex_user_auth_service),
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> PlexLinkPinResponse:
    plex = preferences_service.get_plex_connection_raw()
    if not (plex.enabled and plex.plex_url):
        raise HTTPException(status_code=400, detail="Plex is not configured by the administrator")
    try:
        pin_id, auth_url = await auth_service.create_login_pin()
    except AuthenticationError:
        raise HTTPException(status_code=502, detail="Couldn't reach Plex to start account linking")
    return PlexLinkPinResponse(pin_id=pin_id, auth_url=auth_url)


@router.get("/connections/plex/auth/poll", response_model=PlexLinkPollResponse)
async def plex_link_poll(
    pin_id: int,
    current_user: CurrentUserDep,
    auth_service: PlexUserAuthService = Depends(get_plex_user_auth_service),
    store: UserConnectionsStore = Depends(get_user_connections_store),
    client_factory: PerUserClientFactory = Depends(get_per_user_client_factory),
) -> PlexLinkPollResponse:
    try:
        profile = await auth_service.poll_for_link(pin_id)
    except AuthenticationError as e:
        # covers both "not on this server" (membership gate) and profile-fetch failures
        raise HTTPException(status_code=403, detail=str(e))
    if profile is None:
        return PlexLinkPollResponse(completed=False)
    await store.upsert(
        current_user.id,
        "plex",
        {
            "auth_token": profile["auth_token"],
            "server_access_token": profile["server_access_token"],
            "plex_user_id": profile["uuid"],
            "username": profile["display_name"],
        },
    )
    await client_factory.invalidate_playlist_cache(current_user.id, "plex")
    return PlexLinkPollResponse(completed=True, username=profile["display_name"])


@router.delete("/connections/{service}", response_model=ConnectionActionResponse)
async def disconnect(
    current_user: CurrentUserDep,
    service: str,
    store: UserConnectionsStore = Depends(get_user_connections_store),
    client_factory: PerUserClientFactory = Depends(get_per_user_client_factory),
) -> ConnectionActionResponse:
    if service not in _SUPPORTED_SERVICES:
        raise HTTPException(status_code=404, detail="Unknown service")
    deleted = await store.delete(current_user.id, service)
    if deleted:
        await client_factory.invalidate_playlist_cache(current_user.id, service)
    return ConnectionActionResponse(service=service, deleted=deleted)


async def _build_section_page(
    page: Page,
    user_id: str,
    section_prefs: UserSectionPrefsStore,
    client_factory: PerUserClientFactory,
) -> list[SectionPrefItem]:
    disabled = await section_prefs.get_disabled(user_id, page)
    lb_linked = await client_factory.is_listenbrainz_linked(user_id)
    lfm_linked = await client_factory.is_lastfm_linked(user_id)
    availability = {
        "listenbrainz": lb_linked,
        "lastfm": lfm_linked,
        # the native library engine is always present
        "library": True,
    }
    return [
        SectionPrefItem(
            key=s.key,
            title=s.title,
            description=s.description,
            zone=s.zone,
            enabled=s.key not in disabled,
            available=availability.get(s.requires, True) if s.requires else True,
            requires=s.requires,
        )
        for s in sections_for(page)
    ]


@router.get("/section-prefs", response_model=SectionPrefsResponse)
async def get_section_prefs(
    current_user: CurrentUserDep,
    section_prefs: UserSectionPrefsStore = Depends(get_user_section_prefs_store),
    client_factory: PerUserClientFactory = Depends(get_per_user_client_factory),
) -> SectionPrefsResponse:
    pages: dict[str, list[SectionPrefItem]] = {}
    for page in ("home", "discover", "sidebar"):
        pages[page] = await _build_section_page(
            page, current_user.id, section_prefs, client_factory
        )
    return SectionPrefsResponse(pages=pages)


@router.put("/section-prefs", response_model=SectionPrefsResponse)
async def update_section_prefs(
    current_user: CurrentUserDep,
    body: SectionPrefsUpdate = MsgSpecBody(SectionPrefsUpdate),
    section_prefs: UserSectionPrefsStore = Depends(get_user_section_prefs_store),
    client_factory: PerUserClientFactory = Depends(get_per_user_client_factory),
) -> SectionPrefsResponse:
    known = valid_keys(body.page)
    unknown = [s.key for s in body.sections if s.key not in known]
    if unknown:
        raise HTTPException(status_code=422, detail=f"Unknown section keys: {', '.join(sorted(unknown))}")
    disabled = {s.key for s in body.sections if not s.enabled}
    await section_prefs.set_disabled(current_user.id, body.page, disabled)
    return SectionPrefsResponse(
        pages={
            body.page: await _build_section_page(
                body.page, current_user.id, section_prefs, client_factory
            )
        }
    )
