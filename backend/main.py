import logging
import asyncio
from contextlib import asynccontextmanager
from fastapi import FastAPI, APIRouter, HTTPException
from fastapi.exceptions import RequestValidationError
from starlette.exceptions import HTTPException as StarletteHTTPException
from fastapi.middleware.gzip import GZipMiddleware
from fastapi.middleware.cors import CORSMiddleware
from core.dependencies import (
    get_request_queue, 
    get_cache, 
    get_library_service,
    get_preferences_service,
    init_app_state, 
    cleanup_app_state
)
from core.tasks import start_cache_cleanup_task, start_library_sync_task, start_disk_cache_cleanup_task, start_home_cache_warming_task, start_genre_cache_warming_task, start_discover_cache_warming_task, start_artist_discovery_cache_warming_task, start_audiodb_sweep_task, start_request_status_sync_task, start_memory_maintenance_task
from core.task_registry import TaskRegistry
from core.config import get_settings
from core.dependencies.auth_providers import get_auth_service
from core.exceptions import ResourceNotFoundError, ExternalServiceError, SourceResolutionError, ValidationError, ConfigurationError, ClientDisconnectedError
from core.exception_handlers import (
    resource_not_found_handler,
    external_service_error_handler,
    circuit_open_error_handler,
    source_resolution_error_handler,
    validation_error_handler,
    configuration_error_handler,
    general_exception_handler,
    http_exception_handler,
    starlette_http_exception_handler,
    request_validation_error_handler,
    client_disconnected_handler,
)
from infrastructure.resilience.retry import CircuitOpenError
from infrastructure.msgspec_fastapi import MsgSpecJSONResponse
from middleware import DegradationMiddleware, HSTSMiddleware, PerformanceMiddleware, RateLimitMiddleware, AuthMiddleware
from static_server import mount_frontend
from api.v1.routes import (
    search, requests, library, status, queue, covers, artists, albums, settings, home, discover, profile, playlists
)
from api.v1.routes import cache as cache_routes
from api.v1.routes import cache_status as cache_status_routes
from api.v1.routes import youtube as youtube_routes
from api.v1.routes import requests_page as requests_page_routes
from api.v1.routes import stream as stream_routes
from api.v1.routes import jellyfin_library as jellyfin_library_routes
from api.v1.routes import navidrome_library as navidrome_library_routes
from api.v1.routes import local_library as local_library_routes
from api.v1.routes import lastfm as lastfm_routes
from api.v1.routes import scrobble as scrobble_routes
from api.v1.routes import plex_library as plex_library_routes
from api.v1.routes import plex_auth as plex_auth_routes
from api.v1.routes import version as version_routes
from api.v1.routes import download as download_routes
from api.v1.routes import auth as auth_routes

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(name)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)


@asynccontextmanager
async def lifespan(app: FastAPI):
    logger.info("Starting Musicseerr...")
    
    settings = get_settings()
    configured_level = getattr(logging, settings.log_level, logging.INFO)
    logging.getLogger().setLevel(configured_level)

    await init_app_state(app)
    await get_auth_service().cleanup_expired_tokens()
    
    preferences_service = get_preferences_service()
    settings.instance_id = preferences_service.get_instance_id()
    advanced_settings = preferences_service.get_advanced_settings()

    cache = get_cache()
    start_cache_cleanup_task(cache, interval=advanced_settings.memory_cache_cleanup_interval)
    start_memory_maintenance_task(cache)
    
    from core.dependencies import get_disk_cache
    disk_cache = get_disk_cache()
    from core.dependencies import get_coverart_repository
    cover_disk_cache = get_coverart_repository().disk_cache
    start_disk_cache_cleanup_task(
        disk_cache,
        interval=advanced_settings.disk_cache_cleanup_interval,
        cover_disk_cache=cover_disk_cache,
    )
    
    library_service = get_library_service()
    start_library_sync_task(library_service, preferences_service)

    request_queue = get_request_queue()
    await request_queue.start()
    
    from core.tasks import warm_library_cache
    from core.dependencies import get_album_service, get_library_db, get_sync_state_store
    
    def handle_cache_warming_error(task: asyncio.Task):
        try:
            if task.cancelled():
                return
            
            exc = task.exception()
            if exc:
                logger.error("Cache warming failed: %s", exc, exc_info=exc)
        except asyncio.CancelledError:
            pass
        except Exception as e:  # noqa: BLE001
            logger.error("Error checking cache warming task: %s", e)
    
    cache_task = asyncio.create_task(
        warm_library_cache(library_service, get_album_service(), get_library_db())
    )
    cache_task.add_done_callback(handle_cache_warming_error)
    TaskRegistry.get_instance().register("library-cache-warmup", cache_task)

    from services.cache_status_service import CacheStatusService
    sync_state_store = get_sync_state_store()
    library_db = get_library_db()
    status_service = CacheStatusService(sync_state_store)

    interrupted_state = await status_service.restore_from_persistence()
    if interrupted_state:

        async def resume_sync():
            try:
                await asyncio.sleep(5)
                artists = await library_db.get_artists()
                albums = await library_db.get_albums()
                if artists or albums:
                    artists_dicts = [{'mbid': a['mbid'], 'name': a['name']} for a in artists]
                    await library_service._precache_service.precache_library_resources(
                        artists_dicts, albums, resume=True
                    )
                else:
                    logger.warning("No cached artists/albums to resume sync with, clearing state")
                    await sync_state_store.clear_sync_state()
            except Exception as e:  # noqa: BLE001
                logger.error("Failed to resume interrupted sync: %s", e)
                await status_service.complete_sync(str(e))

        resume_task = asyncio.create_task(resume_sync())
        resume_task.add_done_callback(lambda t: logger.error("Resume sync failed: %s", t.exception()) if t.exception() else None)
        TaskRegistry.get_instance().register("library-sync-resume", resume_task)

    from core.dependencies import get_home_service
    start_home_cache_warming_task(get_home_service())
    start_genre_cache_warming_task(get_home_service())

    from core.dependencies import get_discover_service, get_discover_queue_manager
    start_discover_cache_warming_task(
        get_discover_service(),
        queue_manager=get_discover_queue_manager(),
        preferences_service=get_preferences_service(),
    )

    from core.dependencies import get_artist_discovery_service
    start_artist_discovery_cache_warming_task(
        get_artist_discovery_service(),
        get_library_db(),
        interval=advanced_settings.artist_discovery_warm_interval,
        delay=advanced_settings.artist_discovery_warm_delay,
    )

    from core.dependencies import get_audiodb_image_service
    start_audiodb_sweep_task(
        get_audiodb_image_service(),
        get_library_db(),
        get_preferences_service(),
        precache_service=library_service._precache_service,
    )

    from core.dependencies import get_audiodb_browse_queue
    browse_queue = get_audiodb_browse_queue()
    browse_queue.start_consumer(
        get_audiodb_image_service(),
        get_preferences_service(),
    )

    from core.tasks import warm_jellyfin_mbid_index
    from core.dependencies import get_jellyfin_repository
    jellyfin_settings = preferences_service.get_jellyfin_connection()
    if jellyfin_settings.enabled:
        mbid_task = asyncio.create_task(warm_jellyfin_mbid_index(get_jellyfin_repository()))
        mbid_task.add_done_callback(
            lambda t: None if t.cancelled() else (
                logger.error("Jellyfin MBID index warming failed: %s", t.exception()) if t.exception() else None
            )
        )
        TaskRegistry.get_instance().register("jellyfin-mbid-warmup", mbid_task)

    navidrome_settings = preferences_service.get_navidrome_connection()
    if navidrome_settings.enabled:
        from core.tasks import warm_navidrome_mbid_cache
        nav_mbid_task = asyncio.create_task(warm_navidrome_mbid_cache())
        nav_mbid_task.add_done_callback(
            lambda t: None if t.cancelled() else (
                logger.error("Navidrome MBID cache warming failed: %s", t.exception()) if t.exception() else None
            )
        )
        TaskRegistry.get_instance().register("navidrome-mbid-warmup", nav_mbid_task)

    plex_settings = preferences_service.get_plex_connection()
    if plex_settings.enabled:
        from core.tasks import warm_plex_mbid_cache
        plex_mbid_task = asyncio.create_task(warm_plex_mbid_cache())
        plex_mbid_task.add_done_callback(
            lambda t: None if t.cancelled() else (
                logger.error("Plex MBID cache warming failed: %s", t.exception()) if t.exception() else None
            )
        )
        TaskRegistry.get_instance().register("plex-mbid-warmup", plex_mbid_task)

    from core.dependencies import get_requests_page_service
    requests_page_service = get_requests_page_service()

    start_request_status_sync_task(requests_page_service)

    from core.tasks import start_orphan_cover_demotion_task, start_store_prune_task
    from core.dependencies import get_request_history_store, get_mbid_store, get_youtube_store

    start_orphan_cover_demotion_task(
        cover_disk_cache,
        library_db,
        interval=advanced_settings.orphan_cover_demote_interval_hours * 3600,
    )

    start_store_prune_task(
        get_request_history_store(),
        get_mbid_store(),
        get_youtube_store(),
        request_retention_days=advanced_settings.request_history_retention_days,
        ignored_retention_days=advanced_settings.ignored_releases_retention_days,
        interval=advanced_settings.store_prune_interval_hours * 3600,
    )
    
    logger.info("Musicseerr started successfully")
    
    try:
        yield
    finally:
        logger.info("Shutting down Musicseerr...")

        try:
            await request_queue.stop()
        except Exception as e:  # noqa: BLE001
            logger.error("Error stopping request queue: %s", e)

        registry = TaskRegistry.get_instance()
        settings = get_settings()
        await registry.cancel_all(grace_period=settings.shutdown_grace_period)

        try:
            await cleanup_app_state()
        except Exception as e:  # noqa: BLE001
            logger.error("Error during cleanup: %s", e)
        
        logger.info("Musicseerr shut down successfully")


app = FastAPI(
    title="Musicseerr",
    description="Music request and management system",
    version="1.0.0",
    docs_url="/api/v1/docs",
    redoc_url="/api/v1/redoc",
    openapi_url="/api/v1/openapi.json",
    lifespan=lifespan,
    default_response_class=MsgSpecJSONResponse,
)

app.add_exception_handler(ClientDisconnectedError, client_disconnected_handler)
app.add_exception_handler(ResourceNotFoundError, resource_not_found_handler)
app.add_exception_handler(ExternalServiceError, external_service_error_handler)
app.add_exception_handler(SourceResolutionError, source_resolution_error_handler)
app.add_exception_handler(ValidationError, validation_error_handler)
app.add_exception_handler(ConfigurationError, configuration_error_handler)
app.add_exception_handler(CircuitOpenError, circuit_open_error_handler)
app.add_exception_handler(HTTPException, http_exception_handler)
app.add_exception_handler(StarletteHTTPException, starlette_http_exception_handler)
app.add_exception_handler(RequestValidationError, request_validation_error_handler)
app.add_exception_handler(Exception, general_exception_handler)

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

app_settings = get_settings()
if app_settings.debug:
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


@app.get("/health")
def health_check():
    return {"status": "ok", "message": "Musicseerr backend running"}


v1_router = APIRouter(prefix="/api/v1")
v1_router.include_router(search.router)
v1_router.include_router(requests.router)
v1_router.include_router(library.router)
v1_router.include_router(queue.router)
v1_router.include_router(status.router)
v1_router.include_router(covers.router)
v1_router.include_router(artists.router)
v1_router.include_router(albums.router)
v1_router.include_router(settings.router)
v1_router.include_router(home.router)
v1_router.include_router(discover.router)
v1_router.include_router(youtube_routes.router)
v1_router.include_router(cache_routes.router)
v1_router.include_router(cache_status_routes.router)
v1_router.include_router(requests_page_routes.router)
v1_router.include_router(stream_routes.router)
v1_router.include_router(jellyfin_library_routes.router)
v1_router.include_router(navidrome_library_routes.router)
v1_router.include_router(plex_library_routes.router)
v1_router.include_router(plex_auth_routes.router)
v1_router.include_router(local_library_routes.router)
v1_router.include_router(lastfm_routes.router)
v1_router.include_router(scrobble_routes.router)
v1_router.include_router(profile.router)
v1_router.include_router(playlists.router)
v1_router.include_router(version_routes.router)
v1_router.include_router(download_routes.router)
v1_router.include_router(auth_routes.router)
app.include_router(v1_router)

mount_frontend(app)
