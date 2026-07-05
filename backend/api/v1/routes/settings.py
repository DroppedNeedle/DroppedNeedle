import asyncio
import logging
import os
import msgspec
from fastapi import APIRouter, Depends, HTTPException, Request
from api.v1.schemas.settings import (
    UserPreferences,
    LibrarySyncSettings,
    LibraryScanScheduleSettings,
    LibraryScanScheduleResponse,
    JellyfinConnectionSettings,
    JellyfinVerifyResponse,
    JellyfinUserInfo,
    NavidromeConnectionSettings,
    ListenBrainzConnectionSettings,
    YouTubeConnectionSettings,
    LastFmConnectionSettings,
    LastFmConnectionSettingsResponse,
    LastFmVerifyResponse,
    ScrobbleSettings,
    PrimaryMusicSourceSettings,
    PlexConnectionSettings,
    PlexVerifyResponse,
    MusicBrainzConnectionSettings,
    SecuritySettings,
    OIDCConnectionSettings,
    LibrarySettings,
    LibraryPathRequest,
    SpotifySettings,
    HomeSettings,
    ACOUSTID_KEY_MASK,
)
from api.v1.schemas.plex import PlexLibrarySectionInfo
from api.v1.schemas.common import VerifyConnectionResponse
from api.v1.schemas.advanced_settings import AdvancedSettingsFrontend, FrontendCacheTTLs, _is_masked_api_key
from core.dependencies import (
    get_preferences_service,
    get_settings_service,
    get_oidc_user_auth_service,
)
from services.oidc_user_auth_service import OIDCUserAuthService
from core.exceptions import ConfigurationError
from infrastructure.msgspec_fastapi import AppStruct, MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep
from services.preferences_service import PreferencesService
from services.settings_service import SettingsService

logger = logging.getLogger(__name__)


async def _admin_guard(_: CurrentAdminDep) -> None: ...


router = APIRouter(
    route_class=MsgSpecRoute,
    prefix="/settings",
    tags=["settings"],
    dependencies=[Depends(_admin_guard)],
)


@router.get("/preferences", response_model=UserPreferences)
async def get_preferences(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_preferences()


@router.put("/preferences", response_model=UserPreferences)
async def update_preferences(
    preferences: UserPreferences = MsgSpecBody(UserPreferences),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_preferences(preferences)
        await settings_service.clear_caches_for_preference_change()
        return preferences
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating preferences: {e}")
        raise HTTPException(status_code=400, detail="Couldn't save these settings")


@router.get("/library/sync", response_model=LibrarySyncSettings)
async def get_library_sync_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_library_sync_settings()


@router.put("/library/sync", response_model=LibrarySyncSettings)
async def update_library_sync_settings(
    library_sync_settings: LibrarySyncSettings = MsgSpecBody(LibrarySyncSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    try:
        preferences_service.save_library_sync_settings(library_sync_settings)
        return library_sync_settings
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating library sync settings: {e}")
        raise HTTPException(status_code=400, detail="Library sync settings are incomplete or invalid")


def _server_timezone_label() -> str:
    """Human label for the server's local timezone, captioning the daily-scan time
    picker. Prefers the IANA name from TZ, else the local abbreviation."""
    tz = os.environ.get("TZ", "").strip()
    if tz:
        return tz
    from datetime import datetime

    return datetime.now().astimezone().tzname() or "server time"


@router.get("/library/schedule", response_model=LibraryScanScheduleResponse)
async def get_library_scan_schedule(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    s = preferences_service.get_library_scan_schedule()
    return LibraryScanScheduleResponse(
        scan_frequency=s.scan_frequency,
        daily_scan_time=s.daily_scan_time,
        last_scan=s.last_scan,
        last_scan_success=s.last_scan_success,
        server_timezone=_server_timezone_label(),
    )


@router.put("/library/schedule", response_model=LibraryScanScheduleSettings)
async def update_library_scan_schedule(
    schedule: LibraryScanScheduleSettings = MsgSpecBody(LibraryScanScheduleSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    try:
        preferences_service.save_library_scan_schedule(schedule)
        return schedule
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating library scan schedule: {e}")
        raise HTTPException(status_code=400, detail="Library scan schedule is incomplete or invalid")


@router.get("/library", response_model=LibrarySettings)
async def get_library_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_library_settings()


@router.put("/library", response_model=LibrarySettings)
async def update_library_settings(
    settings: LibrarySettings = MsgSpecBody(LibrarySettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    try:
        preferences_service.save_library_settings(settings)
        return preferences_service.get_library_settings()
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating library settings: {e}")
        raise HTTPException(status_code=400, detail="Library settings are invalid")


@router.post("/library/paths", response_model=LibrarySettings)
async def add_library_path(
    body: LibraryPathRequest = MsgSpecBody(LibraryPathRequest),
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    candidate = (body.path or "").strip()
    if not candidate:
        raise HTTPException(status_code=400, detail="Library path is required")
    # validate at add-time so a typo'd/unmounted path fails loudly rather than
    # saving silently and yielding an empty scan until restart (StartupValidator
    # only checks paths at boot); isdir runs off the loop, can stall on network fs
    if not await asyncio.to_thread(os.path.isdir, candidate):
        raise HTTPException(
            status_code=400,
            detail=f"Path does not exist or is not a directory: {candidate}",
        )
    current = preferences_service.get_library_settings_raw()
    paths = list(current.library_paths)
    if candidate not in paths:
        paths.append(candidate)
    current.library_paths = paths
    current.acoustid_api_key = ACOUSTID_KEY_MASK if current.acoustid_api_key else ""
    preferences_service.save_library_settings(current)
    return preferences_service.get_library_settings()


@router.delete("/library/paths", response_model=LibrarySettings)
async def remove_library_path(
    path: str,
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    current = preferences_service.get_library_settings_raw()
    current.library_paths = [p for p in current.library_paths if p != path]
    current.acoustid_api_key = ACOUSTID_KEY_MASK if current.acoustid_api_key else ""
    preferences_service.save_library_settings(current)
    return preferences_service.get_library_settings()


@router.get("/cache-ttls", response_model=FrontendCacheTTLs)
async def get_frontend_cache_ttls(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    backend_settings = preferences_service.get_advanced_settings()
    return FrontendCacheTTLs(
        home=backend_settings.frontend_ttl_home,
        discover=backend_settings.frontend_ttl_discover,
        library=backend_settings.frontend_ttl_library,
        recently_added=backend_settings.frontend_ttl_recently_added,
        discover_queue=backend_settings.frontend_ttl_discover_queue,
        search=backend_settings.frontend_ttl_search,
        local_files_sidebar=backend_settings.frontend_ttl_local_files_sidebar,
        jellyfin_sidebar=backend_settings.frontend_ttl_jellyfin_sidebar,
        plex_sidebar=backend_settings.frontend_ttl_plex_sidebar,
        playlist_sources=backend_settings.frontend_ttl_playlist_sources,
        discover_queue_polling_interval=backend_settings.discover_queue_polling_interval,
        discover_queue_auto_generate=backend_settings.discover_queue_auto_generate,
    )


@router.get("/advanced", response_model=AdvancedSettingsFrontend)
async def get_advanced_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    backend_settings = preferences_service.get_advanced_settings()
    return AdvancedSettingsFrontend.from_backend(backend_settings)


@router.put("/advanced", response_model=AdvancedSettingsFrontend)
async def update_advanced_settings(
    settings: AdvancedSettingsFrontend = MsgSpecBody(AdvancedSettingsFrontend),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        backend_settings = settings.to_backend()
        if _is_masked_api_key(backend_settings.audiodb_api_key):
            current = preferences_service.get_advanced_settings()
            backend_settings = msgspec.structs.replace(
                backend_settings, audiodb_api_key=current.audiodb_api_key
            )
        preferences_service.save_advanced_settings(backend_settings)
        await settings_service.on_coverart_settings_changed()
        saved = preferences_service.get_advanced_settings()
        return AdvancedSettingsFrontend.from_backend(saved)
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating advanced settings: {e}")
        raise HTTPException(status_code=400, detail="Couldn't save these settings")
    except ValueError as e:
        logger.warning(f"Validation error updating advanced settings: {e}")
        raise HTTPException(status_code=400, detail="That settings value isn't valid")




@router.get("/jellyfin", response_model=JellyfinConnectionSettings)
async def get_jellyfin_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_jellyfin_connection()


@router.put("/jellyfin", response_model=JellyfinConnectionSettings)
async def update_jellyfin_settings(
    settings: JellyfinConnectionSettings = MsgSpecBody(JellyfinConnectionSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_jellyfin_connection(settings)
        await settings_service.on_jellyfin_settings_changed()
        return settings
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating Jellyfin settings: {e}")
        raise HTTPException(status_code=400, detail="Jellyfin settings are incomplete or invalid")


@router.post("/jellyfin/verify", response_model=JellyfinVerifyResponse)
async def verify_jellyfin_connection(
    settings: JellyfinConnectionSettings = MsgSpecBody(JellyfinConnectionSettings),
    settings_service: SettingsService = Depends(get_settings_service),
):
    result = await settings_service.verify_jellyfin(settings)
    users = [JellyfinUserInfo(id=user.id, name=user.name) for user in (result.users or [])] if result.success else []
    return JellyfinVerifyResponse(success=result.success, message=result.message, users=users)


@router.get("/navidrome", response_model=NavidromeConnectionSettings)
async def get_navidrome_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_navidrome_connection()


@router.put("/navidrome", response_model=NavidromeConnectionSettings)
async def update_navidrome_settings(
    settings: NavidromeConnectionSettings = MsgSpecBody(NavidromeConnectionSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_navidrome_connection(settings)
        await settings_service.on_navidrome_settings_changed(enabled=settings.enabled)
        return preferences_service.get_navidrome_connection()
    except ConfigurationError as e:
        logger.warning("Configuration error updating Navidrome settings: %s", e)
        raise HTTPException(status_code=400, detail="Navidrome settings are incomplete or invalid")


@router.post("/navidrome/verify", response_model=VerifyConnectionResponse)
async def verify_navidrome_connection(
    settings: NavidromeConnectionSettings = MsgSpecBody(NavidromeConnectionSettings),
    settings_service: SettingsService = Depends(get_settings_service),
):
    result = await settings_service.verify_navidrome(settings)
    return VerifyConnectionResponse(valid=result.valid, message=result.message)


@router.get("/plex", response_model=PlexConnectionSettings)
async def get_plex_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_plex_connection()


@router.put("/plex", response_model=PlexConnectionSettings)
async def update_plex_settings(
    settings: PlexConnectionSettings = MsgSpecBody(PlexConnectionSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_plex_connection(settings)
        await settings_service.on_plex_settings_changed(enabled=settings.enabled)
        logger.info("Updated Plex connection settings")
        return preferences_service.get_plex_connection()
    except ConfigurationError as e:
        logger.warning("Configuration error updating Plex settings: %s", e)
        raise HTTPException(status_code=400, detail="Plex settings are incomplete or invalid")


@router.post("/plex/verify", response_model=PlexVerifyResponse)
async def verify_plex_connection(
    settings: PlexConnectionSettings = MsgSpecBody(PlexConnectionSettings),
    settings_service: SettingsService = Depends(get_settings_service),
):
    result = await settings_service.verify_plex(settings)
    libs = [PlexLibrarySectionInfo(key=k, title=t) for k, t in result.libraries]
    return PlexVerifyResponse(valid=result.valid, message=result.message, libraries=libs)


@router.get("/plex/libraries", response_model=list[PlexLibrarySectionInfo])
async def get_plex_libraries(
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        libs = await settings_service.get_plex_libraries()
        return [PlexLibrarySectionInfo(key=k, title=t) for k, t in libs]
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))
    except Exception as e:
        logger.exception("Failed to fetch Plex libraries: %s", e)
        raise HTTPException(status_code=502, detail="Could not fetch libraries from Plex")


@router.get("/listenbrainz", response_model=ListenBrainzConnectionSettings)
async def get_listenbrainz_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_listenbrainz_connection()


@router.put("/listenbrainz", response_model=ListenBrainzConnectionSettings)
async def update_listenbrainz_settings(
    settings: ListenBrainzConnectionSettings = MsgSpecBody(ListenBrainzConnectionSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_listenbrainz_connection(settings)
        await settings_service.on_listenbrainz_settings_changed()
        return settings
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating ListenBrainz settings: {e}")
        raise HTTPException(status_code=400, detail="ListenBrainz settings are incomplete or invalid")


@router.post("/listenbrainz/verify", response_model=VerifyConnectionResponse)
async def verify_listenbrainz_connection(
    settings: ListenBrainzConnectionSettings = MsgSpecBody(ListenBrainzConnectionSettings),
    settings_service: SettingsService = Depends(get_settings_service),
):
    result = await settings_service.verify_listenbrainz(settings)
    return VerifyConnectionResponse(valid=result.valid, message=result.message)


@router.get("/youtube", response_model=YouTubeConnectionSettings)
async def get_youtube_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_youtube_connection()


@router.put("/youtube", response_model=YouTubeConnectionSettings)
async def update_youtube_settings(
    settings: YouTubeConnectionSettings = MsgSpecBody(YouTubeConnectionSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_youtube_connection(settings)
        await settings_service.on_youtube_settings_changed()
        return settings
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating YouTube settings: {e}")
        raise HTTPException(status_code=400, detail="YouTube settings are incomplete or invalid")


@router.post("/youtube/verify", response_model=VerifyConnectionResponse)
async def verify_youtube_connection(
    settings: YouTubeConnectionSettings = MsgSpecBody(YouTubeConnectionSettings),
    settings_service: SettingsService = Depends(get_settings_service),
):
    result = await settings_service.verify_youtube(settings)
    return VerifyConnectionResponse(valid=result.valid, message=result.message)


@router.get("/spotify", response_model=SpotifySettings, dependencies=[Depends(_admin_guard)])
async def get_spotify_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_spotify_settings()


@router.put("/spotify", response_model=SpotifySettings, dependencies=[Depends(_admin_guard)])
async def update_spotify_settings(
    settings: SpotifySettings = MsgSpecBody(SpotifySettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    preferences_service.save_spotify_settings(settings)
    return preferences_service.get_spotify_settings()


class SpotifyRedirectUriResponse(AppStruct):
    redirect_uri: str


@router.get(
    "/spotify/redirect-uri",
    response_model=SpotifyRedirectUriResponse,
    dependencies=[Depends(_admin_guard)],
)
async def get_spotify_redirect_uri(request: Request) -> SpotifyRedirectUriResponse:
    redirect_uri = str(request.base_url).rstrip("/") + "/api/v1/me/connections/spotify/auth/callback"
    return SpotifyRedirectUriResponse(redirect_uri=redirect_uri)


@router.get("/home", response_model=HomeSettings, dependencies=[Depends(_admin_guard)])
async def get_home_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_home_settings()


@router.put("/home", response_model=HomeSettings, dependencies=[Depends(_admin_guard)])
async def update_home_settings(
    settings: HomeSettings = MsgSpecBody(HomeSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_home_settings(settings)
        await settings_service.clear_home_cache()
        return settings
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating home settings: {e}")
        raise HTTPException(status_code=400, detail="Home settings are incomplete or invalid")



@router.get("/lastfm", response_model=LastFmConnectionSettingsResponse)
async def get_lastfm_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    settings = preferences_service.get_lastfm_connection()
    return LastFmConnectionSettingsResponse.from_settings(settings)


@router.put("/lastfm", response_model=LastFmConnectionSettingsResponse)
async def update_lastfm_settings(
    settings: LastFmConnectionSettings = MsgSpecBody(LastFmConnectionSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_lastfm_connection(settings)
        await settings_service.on_lastfm_settings_changed()
        saved = preferences_service.get_lastfm_connection()
        return LastFmConnectionSettingsResponse.from_settings(saved)
    except ConfigurationError as e:
        logger.warning("Configuration error updating Last.fm settings: %s", e)
        raise HTTPException(status_code=400, detail="Last.fm settings are incomplete or invalid")


@router.post("/lastfm/verify", response_model=LastFmVerifyResponse)
async def verify_lastfm_connection(
    settings: LastFmConnectionSettings = MsgSpecBody(LastFmConnectionSettings),
    settings_service: SettingsService = Depends(get_settings_service),
):
    result = await settings_service.verify_lastfm(settings)
    return LastFmVerifyResponse(valid=result.valid, message=result.message)


@router.get("/scrobble", response_model=ScrobbleSettings)
async def get_scrobble_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_scrobble_settings()


@router.put("/scrobble", response_model=ScrobbleSettings)
async def update_scrobble_settings(
    settings: ScrobbleSettings = MsgSpecBody(ScrobbleSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    try:
        preferences_service.save_scrobble_settings(settings)
        return preferences_service.get_scrobble_settings()
    except ConfigurationError as e:
        logger.warning("Configuration error updating scrobble settings: %s", e)
        raise HTTPException(status_code=400, detail="Scrobbling settings are incomplete or invalid")


@router.get("/primary-source", response_model=PrimaryMusicSourceSettings)
async def get_primary_music_source(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_primary_music_source()


@router.put("/primary-source", response_model=PrimaryMusicSourceSettings)
async def update_primary_music_source(
    settings: PrimaryMusicSourceSettings = MsgSpecBody(PrimaryMusicSourceSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_primary_music_source(settings)
        await settings_service.clear_home_cache()
        await settings_service.clear_source_resolution_cache()
        return preferences_service.get_primary_music_source()
    except ConfigurationError as e:
        logger.warning("Configuration error updating primary music source: %s", e)
        raise HTTPException(status_code=400, detail="Invalid primary music source")


@router.get("/musicbrainz", response_model=MusicBrainzConnectionSettings)
async def get_musicbrainz_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
):
    return preferences_service.get_musicbrainz_connection()


@router.put("/musicbrainz", response_model=MusicBrainzConnectionSettings)
async def update_musicbrainz_settings(
    settings: MusicBrainzConnectionSettings = MsgSpecBody(MusicBrainzConnectionSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
    settings_service: SettingsService = Depends(get_settings_service),
):
    try:
        preferences_service.save_musicbrainz_connection(settings)
        await settings_service.on_musicbrainz_settings_changed(settings)
        return settings
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating MusicBrainz settings: {e}")
        raise HTTPException(status_code=400, detail="MusicBrainz settings are incomplete or invalid")


@router.post("/musicbrainz/verify", response_model=VerifyConnectionResponse)
async def verify_musicbrainz_connection(
    settings: MusicBrainzConnectionSettings = MsgSpecBody(MusicBrainzConnectionSettings),
    settings_service: SettingsService = Depends(get_settings_service),
):
    result = await settings_service.verify_musicbrainz(settings)
    return VerifyConnectionResponse(valid=result.valid, message=result.message)


@router.get("/security", response_model=SecuritySettings)
async def get_security_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> SecuritySettings:
    return preferences_service.get_security_settings()


@router.put("/security", response_model=SecuritySettings)
async def update_security_settings(
    settings: SecuritySettings = MsgSpecBody(SecuritySettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> SecuritySettings:
    try:
        preferences_service.save_security_settings(settings)
        return settings
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating security settings: {e}")
        raise HTTPException(status_code=400, detail="Could not save security settings")


@router.post("/security/verify-hibp", response_model=VerifyConnectionResponse)
async def verify_hibp_local_file(
    settings: SecuritySettings = MsgSpecBody(SecuritySettings),
) -> VerifyConnectionResponse:
    path = (settings.hibp_local_path or "").strip()
    if not path:
        return VerifyConnectionResponse(valid=False, message="No path provided.")

    if not os.path.isfile(path):
        return VerifyConnectionResponse(valid=False, message=f"File not found: {path}")

    try:
        size = os.path.getsize(path)
        with open(path, "rb") as fh:
            first_line = fh.readline().decode("ascii", errors="ignore").strip()

        if not first_line or ":" not in first_line:
            return VerifyConnectionResponse(
                valid=False,
                message="File does not appear to be a valid HIBP hash list (unexpected format).",
            )

        parts = first_line.split(":", 1)
        if len(parts[0]) != 40 or not parts[0].isalnum():
            return VerifyConnectionResponse(
                valid=False,
                message="File does not appear to be a valid HIBP hash list (expected 40-char SHA-1 hash).",
            )

        def _fmt_size(b: int) -> str:
            for unit in ("B", "KB", "MB", "GB"):
                if b < 1024:
                    return f"{b:.1f} {unit}"
                b //= 1024
            return f"{b:.1f} TB"

        return VerifyConnectionResponse(
            valid=True,
            message=f"File looks valid. Size: {_fmt_size(size)}.",
        )
    except OSError as e:
        return VerifyConnectionResponse(valid=False, message=f"Could not read file: {e}")


@router.get("/oidc", response_model=OIDCConnectionSettings)
async def get_oidc_settings(
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> OIDCConnectionSettings:
    return preferences_service.get_oidc_connection()


@router.put("/oidc", response_model=OIDCConnectionSettings)
async def update_oidc_settings(
    settings: OIDCConnectionSettings = MsgSpecBody(OIDCConnectionSettings),
    preferences_service: PreferencesService = Depends(get_preferences_service),
) -> OIDCConnectionSettings:
    try:
        preferences_service.save_oidc_connection(settings)
        return preferences_service.get_oidc_connection()
    except ConfigurationError as e:
        logger.warning(f"Configuration error updating OIDC settings: {e}")
        raise HTTPException(status_code=400, detail="OIDC settings are incomplete or invalid")


@router.post("/oidc/verify", response_model=VerifyConnectionResponse)
async def verify_oidc_connection(
    settings: OIDCConnectionSettings = MsgSpecBody(OIDCConnectionSettings),
    oidc_auth: OIDCUserAuthService = Depends(get_oidc_user_auth_service),
) -> VerifyConnectionResponse:
    valid, message = await oidc_auth.verify(settings)
    return VerifyConnectionResponse(valid=valid, message=message)
