"""Target-only API composition for tests, rehearsal, and the offline replacement."""

from __future__ import annotations

from collections.abc import Awaitable, Callable, Mapping
from contextlib import asynccontextmanager
from datetime import datetime

import logging
import os
from pathlib import Path
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from fastapi import APIRouter, Depends, FastAPI, HTTPException
from fastapi.exceptions import RequestValidationError
from fastapi.middleware.cors import CORSMiddleware
from fastapi.middleware.gzip import GZipMiddleware
from fastapi.routing import APIRoute
from starlette.exceptions import HTTPException as StarletteHTTPException
from uvicorn.middleware.proxy_headers import ProxyHeadersMiddleware

from api.compat.common.deps import get_compat_services
from api.compat.common.cors import CompatCORSMiddleware
from api.compat.common.path_case import CompatPathCaseMiddleware
from api.compat.jellyfin.router import router as jellyfin_router
from api.compat.subsonic.router import router as subsonic_router
from api.v1.routes import (
    albums,
    artists,
    auth,
    cache,
    cache_status,
    covers,
    discovery_batches,
    download,
    download_client,
    download_clients,
    discover,
    downloads,
    downloads_search,
    following,
    free_music,
    home,
    import_drop,
    indexers,
    jellyfin_library,
    lastfm,
    lidarr_import,
    library_operations_target,
    library_policies_target,
    library_scan_target,
    library_target,
    local_library,
    me_connections,
    navidrome_library,
    navidrome_preferences,
    now_playing,
    playlists,
    plex_auth,
    plex_library,
    plugins,
    profile,
    quarantine,
    requests,
    requests_page,
    scrobble,
    search,
    settings,
    spotify,
    status,
    stream,
    system,
    tracks,
    version,
    wrapped,
    youtube,
)
from api.v1.routes import connect_apps_routes
from core.dependencies import (
    get_coverart_repository,
    get_acquisition_dispatcher,
    get_album_service,
    get_album_discovery_service,
    get_artist_service,
    get_artist_discovery_service,
    get_discover_service,
    get_discover_queue_manager,
    get_discovery_batch_service,
    get_home_service,
    get_home_charts_service,
    get_local_files_service,
    get_navidrome_library_service,
    get_plex_library_service,
    get_download_service,
    get_drop_import_service,
    get_events_watcher_getter,
    get_free_music_service,
    get_playlist_service,
    get_personal_mix_service,
    get_quota_service,
    get_request_service,
    get_requests_page_service,
    get_scrobble_service,
    get_search_service,
    get_status_service,
    get_settings_service,
    get_spotify_import_service,
    get_cache_service,
    get_wrapped_service,
    get_target_compat_services,
    get_target_album_service,
    get_target_album_discovery_service,
    get_target_artist_service,
    get_target_artist_discovery_service,
    get_target_acquisition_dispatcher,
    get_target_consumer_composition,
    get_target_discover_service,
    get_target_discover_queue_manager,
    get_target_discovery_batch_service,
    get_target_download_service,
    get_target_drop_import_service,
    get_target_events_watcher_service,
    get_target_free_music_service,
    get_target_home_service,
    get_target_home_charts_service,
    get_target_genre_cover_prewarm_service,
    get_target_navidrome_library_service,
    get_target_plex_library_service,
    get_target_personal_mix_service,
    get_target_quota_service,
    get_target_library_ownership_service,
    get_target_library_operation_supervisor,
    get_target_library_scan_coordinator,
    get_target_library_scan_scheduler,
    get_target_library_policy_service,
    get_target_request_service,
    get_target_requests_page_service,
    get_target_search_service,
    get_target_status_service,
    get_target_settings_service,
    get_target_spotify_import_service,
    get_target_cache_service,
    get_target_wrapped_service,
    get_target_wanted_watcher_service,
    get_wanted_watcher_service,
    cleanup_app_state,
    get_cache,
    get_disk_cache,
    get_native_library_store,
    get_preferences_service,
    init_app_state,
    get_target_album_identification_service,
    get_target_identification_queue,
    get_library_policy_resolver,
)
from core.config import get_settings
from core.exception_handlers import (
    circuit_open_error_handler,
    client_disconnected_handler,
    configuration_error_handler,
    conflict_error_handler,
    external_service_error_handler,
    general_exception_handler,
    http_exception_handler,
    permission_denied_handler,
    request_validation_error_handler,
    resource_not_found_handler,
    revision_overflow_error_handler,
    source_resolution_error_handler,
    stale_revision_error_handler,
    starlette_http_exception_handler,
    validation_error_handler,
)
from core.exceptions import (
    ClientDisconnectedError,
    ConfigurationError,
    ConflictError,
    ExternalServiceError,
    PermissionDeniedError,
    ResourceNotFoundError,
    RevisionOverflowError,
    SourceResolutionError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.resilience.retry import CircuitOpenError
from core.task_registry import TaskRegistry
from core.tasks import (
    start_cache_cleanup_task,
    start_disk_cache_cleanup_task,
    start_memory_maintenance_task,
)
from infrastructure.msgspec_fastapi import MsgSpecJSONResponse
from middleware import (
    AuthMiddleware,
    DegradationMiddleware,
    HSTSMiddleware,
    PerformanceMiddleware,
    RateLimitMiddleware,
)
from services.native.library_scan_supervisor import start_target_scan_supervisor
from services.native.target_application_runtime import (
    start_target_identification_worker,
    start_target_operation_worker,
)
from services.native.target_application_lifecycle import (
    run_target_one_time_migrations,
    start_target_operational_runtime,
)
from services.native.target_startup_validator import TargetStartupValidator
from static_server import mount_frontend

logger = logging.getLogger(__name__)


def _include_settings_without_legacy_library(router: APIRouter) -> None:
    """Keep settings routes except legacy root reads and mutations."""

    filtered = APIRouter()
    filtered.routes.extend(
        route
        for route in settings.router.routes
        if not (
            isinstance(route, APIRoute)
            and (
                (
                    route.path == "/settings/library"
                    and bool(route.methods & {"GET", "PUT"})
                )
                or (
                    route.path == "/settings/library/paths"
                    and bool(route.methods & {"POST", "DELETE"})
                )
            )
        )
    )
    router.include_router(filtered)


async def _require_provider_album_id(
    album_id: str,
    ownership=Depends(get_target_library_ownership_service),
) -> None:
    provider_id = await ownership.provider_album_id(album_id)
    if provider_id != album_id:
        from core.exceptions import ProviderIdentityRequiredError

        raise ProviderIdentityRequiredError(
            "Use the local library album route for a DroppedNeedle album ID."
        )


async def _require_provider_artist_id(
    artist_id: str,
    ownership=Depends(get_target_library_ownership_service),
) -> None:
    provider_id = await ownership.provider_artist_id(artist_id)
    if provider_id != artist_id:
        from core.exceptions import ProviderIdentityRequiredError

        raise ProviderIdentityRequiredError(
            "Use the local library artist route for a DroppedNeedle artist ID."
        )


def create_isolated_target_application(
    *,
    target_composition=None,
    dependency_overrides: Mapping[Callable, Callable] | None = None,
    startup_validator: Callable[[], Awaitable[object]] | None = None,
) -> FastAPI:
    """Build the target-only surface without startup tasks or production mounting."""

    @asynccontextmanager
    async def lifespan(_app: FastAPI):
        if startup_validator is not None:
            await startup_validator()
        yield

    app = FastAPI(default_response_class=MsgSpecJSONResponse, lifespan=lifespan)
    v1 = APIRouter(prefix="/api/v1")
    for router in (
        search.router,
        requests.router,
        requests_page.router,
        library_target.router,
        library_scan_target.router,
        status.router,
        covers.router,
        library_policies_target.router,
        home.router,
        discover.router,
        library_operations_target.router,
        stream.router,
        local_library.router,
        scrobble.router,
        now_playing.router,
        profile.router,
        playlists.router,
        download.router,
        import_drop.router,
        free_music.router,
        tracks.router,
        connect_apps_routes.router,
    ):
        v1.include_router(router)
    _include_settings_without_legacy_library(v1)
    v1.include_router(downloads_search.router)
    v1.include_router(quarantine.router)
    v1.include_router(downloads.router)
    v1.include_router(albums.router, dependencies=[Depends(_require_provider_album_id)])
    v1.include_router(
        artists.router, dependencies=[Depends(_require_provider_artist_id)]
    )
    app.include_router(v1)
    app.include_router(subsonic_router)
    app.include_router(jellyfin_router)
    app.add_middleware(
        CompatPathCaseMiddleware,
        routes=[*subsonic_router.routes, *jellyfin_router.routes],
    )

    target = (
        (lambda: target_composition)
        if target_composition is not None
        else get_target_consumer_composition
    )
    app.dependency_overrides.update(
        {
            get_search_service: get_target_search_service,
            get_request_service: get_target_request_service,
            get_requests_page_service: get_target_requests_page_service,
            get_status_service: get_target_status_service,
            get_settings_service: get_target_settings_service,
            get_coverart_repository: lambda: target().covers,
            get_album_service: get_target_album_service,
            get_album_discovery_service: get_target_album_discovery_service,
            get_artist_service: get_target_artist_service,
            get_artist_discovery_service: get_target_artist_discovery_service,
            get_home_service: get_target_home_service,
            get_home_charts_service: get_target_home_charts_service,
            get_discover_service: get_target_discover_service,
            get_discover_queue_manager: get_target_discover_queue_manager,
            get_discovery_batch_service: get_target_discovery_batch_service,
            get_download_service: get_target_download_service,
            get_acquisition_dispatcher: get_target_acquisition_dispatcher,
            get_drop_import_service: get_target_drop_import_service,
            get_events_watcher_getter: lambda: get_target_events_watcher_service,
            get_free_music_service: get_target_free_music_service,
            get_wanted_watcher_service: get_target_wanted_watcher_service,
            get_scrobble_service: lambda: target().scrobble_service,
            get_playlist_service: lambda: target().playlists,
            get_personal_mix_service: get_target_personal_mix_service,
            get_quota_service: get_target_quota_service,
            get_spotify_import_service: get_target_spotify_import_service,
            get_cache_service: get_target_cache_service,
            get_wrapped_service: get_target_wrapped_service,
            get_local_files_service: lambda: target().local_files,
            get_navidrome_library_service: get_target_navidrome_library_service,
            get_plex_library_service: get_target_plex_library_service,
            get_compat_services: get_target_compat_services,
        }
    )
    if dependency_overrides:
        app.dependency_overrides.update(dependency_overrides)
    return app


def _include_complete_target_routes(app: FastAPI) -> None:
    v1 = APIRouter(prefix="/api/v1")
    for router in (
        search.router,
        requests.router,
        library_target.router,
        library_scan_target.router,
        status.router,
        covers.router,
    ):
        v1.include_router(router)
    v1.include_router(
        artists.router, dependencies=[Depends(_require_provider_artist_id)]
    )
    v1.include_router(following.router)
    v1.include_router(albums.router, dependencies=[Depends(_require_provider_album_id)])
    for router in (
        library_policies_target.router,
        home.router,
        wrapped.router,
        discovery_batches.router,
        discover.router,
        library_operations_target.router,
        youtube.router,
        cache.router,
        cache_status.router,
        requests_page.router,
        stream.router,
        jellyfin_library.router,
        navidrome_library.router,
        navidrome_preferences.router,
        plex_library.router,
        plex_auth.router,
        local_library.router,
        lastfm.router,
        scrobble.router,
        me_connections.router,
        system.router,
        spotify.router,
        now_playing.router,
        profile.router,
        playlists.router,
        version.router,
        download.router,
        auth.router,
        download_client.router,
        download_clients.router,
        indexers.router,
        lidarr_import.router,
        import_drop.router,
        free_music.router,
        plugins.router,
        downloads_search.router,
        quarantine.router,
        downloads.router,
        tracks.router,
        connect_apps_routes.router,
    ):
        v1.include_router(router)
    _include_settings_without_legacy_library(v1)
    app.include_router(v1)
    app.include_router(subsonic_router)
    app.include_router(jellyfin_router)


def _install_target_overrides(app: FastAPI) -> None:
    target = get_target_consumer_composition
    app.dependency_overrides.update(
        {
            get_search_service: get_target_search_service,
            get_request_service: get_target_request_service,
            get_requests_page_service: get_target_requests_page_service,
            get_status_service: get_target_status_service,
            get_settings_service: get_target_settings_service,
            get_coverart_repository: lambda: target().covers,
            get_album_service: get_target_album_service,
            get_album_discovery_service: get_target_album_discovery_service,
            get_artist_service: get_target_artist_service,
            get_artist_discovery_service: get_target_artist_discovery_service,
            get_home_service: get_target_home_service,
            get_home_charts_service: get_target_home_charts_service,
            get_discover_service: get_target_discover_service,
            get_discover_queue_manager: get_target_discover_queue_manager,
            get_discovery_batch_service: get_target_discovery_batch_service,
            get_download_service: get_target_download_service,
            get_acquisition_dispatcher: get_target_acquisition_dispatcher,
            get_drop_import_service: get_target_drop_import_service,
            get_events_watcher_getter: lambda: get_target_events_watcher_service,
            get_free_music_service: get_target_free_music_service,
            get_wanted_watcher_service: get_target_wanted_watcher_service,
            get_scrobble_service: lambda: target().scrobble_service,
            get_playlist_service: lambda: target().playlists,
            get_personal_mix_service: get_target_personal_mix_service,
            get_quota_service: get_target_quota_service,
            get_spotify_import_service: get_target_spotify_import_service,
            get_cache_service: get_target_cache_service,
            get_wrapped_service: get_target_wrapped_service,
            get_local_files_service: lambda: target().local_files,
            get_navidrome_library_service: get_target_navidrome_library_service,
            get_plex_library_service: get_target_plex_library_service,
            get_compat_services: get_target_compat_services,
        }
    )


def _server_timezone_name() -> str:
    configured = os.environ.get("TZ", "").strip()
    if configured:
        try:
            ZoneInfo(configured)
        except (ValueError, ZoneInfoNotFoundError):
            logger.warning(
                "Configured TZ is not an IANA timezone; scheduled scans will use UTC"
            )
            return "UTC"
        else:
            return configured
    local = datetime.now().astimezone().tzinfo
    local_key = getattr(local, "key", None)
    if isinstance(local_key, str) and local_key:
        try:
            ZoneInfo(local_key)
        except (ValueError, ZoneInfoNotFoundError):
            pass
        else:
            return local_key
    return "UTC"


@asynccontextmanager
async def production_target_lifespan(app: FastAPI):
    settings = get_settings()
    logging.getLogger().setLevel(getattr(logging, settings.log_level, logging.INFO))
    from core.config import migrate_legacy_config

    logger.info("target_startup.configuration_started")
    migrate_legacy_config()
    await init_app_state(app)
    logger.info("target_startup.configuration_completed")
    await get_target_library_policy_service().recover_pending_transition()

    from maintenance.automatic_upgrade import (
        await_target_startup_admission,
        target_startup_admission_pending,
    )

    logger.info("target_startup.catalog_validation_started")
    validator = TargetStartupValidator(
        get_native_library_store(),
        lambda: {
            root.id
            for root in get_preferences_service()
            .get_typed_library_settings()
            .library_roots
        },
    )
    validation_phase = (
        "cutover" if target_startup_admission_pending() else "steady_state"
    )
    await validator.validate(validation_phase)
    logger.info("target_startup.catalog_validation_completed")

    await await_target_startup_admission(settings)
    logger.info("target_startup.admission_completed")

    from core.dependencies.auth_providers import get_auth_service, get_auth_store

    await get_auth_service().cleanup_expired_tokens()
    auth_store = get_auth_store()
    preferences = get_preferences_service()
    await run_target_one_time_migrations(
        auth_store=auth_store,
        preferences=preferences,
        cache_dir=settings.cache_dir,
    )
    logger.info("target_startup.data_ratchets_completed")
    settings.instance_id = preferences.get_instance_id()
    cache_instance = get_cache()
    await cache_instance.clear()
    advanced = preferences.get_advanced_settings()
    start_cache_cleanup_task(
        cache_instance, interval=advanced.memory_cache_cleanup_interval
    )
    start_memory_maintenance_task(cache_instance)
    start_disk_cache_cleanup_task(
        get_disk_cache(),
        interval=advanced.disk_cache_cleanup_interval,
        cover_disk_cache=get_target_consumer_composition().covers.disk_cache,
    )

    def root_paths() -> dict[str, Path]:
        return {
            root.id: Path(root.path)
            for root in get_preferences_service()
            .get_typed_library_settings()
            .library_roots
        }

    timezone_name = _server_timezone_name()

    def schedule_settings() -> dict[str, str]:
        schedule = get_preferences_service().get_library_scan_schedule()
        return {
            "frequency": schedule.scan_frequency,
            "daily_time": schedule.daily_scan_time,
            "timezone_name": timezone_name,
        }

    start_target_scan_supervisor(
        get_target_library_scan_coordinator,
        root_paths,
        scheduler_getter=get_target_library_scan_scheduler,
        resolver_getter=get_library_policy_resolver,
        schedule_settings_getter=schedule_settings,
    )
    start_target_identification_worker(
        get_target_identification_queue, get_target_album_identification_service
    )
    start_target_operation_worker(get_target_library_operation_supervisor)
    await start_target_operational_runtime(
        settings=settings,
        preferences=preferences,
        auth_store=auth_store,
    )
    logger.info("target_startup.operational_runtime_started")
    logger.info("DroppedNeedle target application started")

    try:
        yield
    finally:
        await TaskRegistry.get_instance().cancel_all(
            grace_period=settings.shutdown_grace_period
        )
        await cleanup_app_state(
            queue_manager_getter=get_target_discover_queue_manager,
            genre_prewarm_getter=get_target_genre_cover_prewarm_service,
        )


def create_production_target_application() -> FastAPI:
    """Build the process that is selected only by the authorized offline replacement."""

    app = FastAPI(
        title="DroppedNeedle",
        description="Music request and management system",
        version="1.0.0",
        docs_url="/api/v1/docs",
        redoc_url="/api/v1/redoc",
        openapi_url="/api/v1/openapi.json",
        lifespan=production_target_lifespan,
        default_response_class=MsgSpecJSONResponse,
    )
    for exception, handler in (
        (ClientDisconnectedError, client_disconnected_handler),
        (ResourceNotFoundError, resource_not_found_handler),
        (ExternalServiceError, external_service_error_handler),
        (SourceResolutionError, source_resolution_error_handler),
        (ValidationError, validation_error_handler),
        (ConfigurationError, configuration_error_handler),
        (PermissionDeniedError, permission_denied_handler),
        (ConflictError, conflict_error_handler),
        (StaleRevisionError, stale_revision_error_handler),
        (RevisionOverflowError, revision_overflow_error_handler),
        (CircuitOpenError, circuit_open_error_handler),
        (HTTPException, http_exception_handler),
        (StarletteHTTPException, starlette_http_exception_handler),
        (RequestValidationError, request_validation_error_handler),
        (Exception, general_exception_handler),
    ):
        app.add_exception_handler(exception, handler)

    app.add_middleware(HSTSMiddleware)
    app.add_middleware(DegradationMiddleware)
    app.add_middleware(PerformanceMiddleware)
    app.add_middleware(GZipMiddleware, minimum_size=1000, compresslevel=6)
    app.add_middleware(AuthMiddleware)
    app.add_middleware(
        RateLimitMiddleware,
        default_rate=30.0,
        default_capacity=60,
        overrides={
            "/api/v1/search": (10.0, 20),
            "/api/v1/discover": (10.0, 20),
            "/api/v1/covers": (15.0, 30),
            "/api/v1/auth/login": (2.0, 5),
            "/api/v1/auth/setup": (1.0, 3),
            "/api/v1/auth/plex/poll": (5.0, 10),
            "/api/v1/auth/jellyfin/login": (2.0, 5),
        },
    )
    if get_settings().debug:
        app.add_middleware(
            CORSMiddleware,
            allow_origins=[
                "http://localhost:5173",
                "http://127.0.0.1:5173",
                "http://localhost:4173",
                "http://127.0.0.1:4173",
                "http://localhost:3000",
                "http://127.0.0.1:3000",
            ],
            allow_credentials=True,
            allow_methods=["*"],
            allow_headers=["*"],
        )
    app.add_middleware(CompatCORSMiddleware)

    @app.get("/health")
    def health_check():
        return {"status": "ok", "message": "DroppedNeedle backend running"}

    _include_complete_target_routes(app)
    _install_target_overrides(app)
    app.add_middleware(
        CompatPathCaseMiddleware,
        routes=[*subsonic_router.routes, *jellyfin_router.routes],
    )
    app.add_middleware(
        ProxyHeadersMiddleware, trusted_hosts=get_settings().trusted_proxy_ips
    )
    mount_frontend(app)
    return app
