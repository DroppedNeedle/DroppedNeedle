import logging
import asyncio
from contextlib import asynccontextmanager
from fastapi import FastAPI, APIRouter, HTTPException
from fastapi.exceptions import RequestValidationError
from starlette.exceptions import HTTPException as StarletteHTTPException
from fastapi.middleware.gzip import GZipMiddleware
from fastapi.middleware.cors import CORSMiddleware
from core.dependencies import (
    get_cache,
    get_library_service,
    get_preferences_service,
    init_app_state,
    cleanup_app_state,
)
from core.tasks import (
    start_cache_cleanup_task,
    start_library_auto_scan_task,
    start_disk_cache_cleanup_task,
    start_artist_discovery_cache_warming_task,
    start_audiodb_sweep_task,
    start_request_status_sync_task,
    start_memory_maintenance_task,
    start_poll_new_releases_task,
    start_discover_home_warmer_task,
    start_personal_mix_refresh_task,
)
from core.task_registry import TaskRegistry
from core.config import get_settings
from core.dependencies.auth_providers import get_auth_service, get_auth_store
from core.exceptions import (
    ResourceNotFoundError,
    ExternalServiceError,
    SourceResolutionError,
    ValidationError,
    ConfigurationError,
    ClientDisconnectedError,
    PermissionDeniedError,
    ConflictError,
    RevisionOverflowError,
    StaleRevisionError,
)
from core.exception_handlers import (
    resource_not_found_handler,
    external_service_error_handler,
    circuit_open_error_handler,
    source_resolution_error_handler,
    validation_error_handler,
    configuration_error_handler,
    permission_denied_handler,
    conflict_error_handler,
    general_exception_handler,
    http_exception_handler,
    starlette_http_exception_handler,
    request_validation_error_handler,
    client_disconnected_handler,
    revision_overflow_error_handler,
    stale_revision_error_handler,
)
from infrastructure.resilience.retry import CircuitOpenError
from infrastructure.msgspec_fastapi import MsgSpecJSONResponse
from middleware import (
    DegradationMiddleware,
    HSTSMiddleware,
    PerformanceMiddleware,
    RateLimitMiddleware,
    AuthMiddleware,
)
from static_server import mount_frontend
from api.v1.routes import (
    search,
    requests,
    library,
    status,
    covers,
    artists,
    albums,
    settings,
    home,
    discover,
    profile,
    playlists,
    following,
    wrapped,
)
from api.v1.routes import discovery_batches as discovery_batches_routes
from api.v1.routes import library_scan as library_scan_routes
from api.v1.routes import library_policies as library_policy_routes
from api.v1.routes import cache as cache_routes
from api.v1.routes import cache_status as cache_status_routes
from api.v1.routes import youtube as youtube_routes
from api.v1.routes import requests_page as requests_page_routes
from api.v1.routes import stream as stream_routes
from api.v1.routes import jellyfin_library as jellyfin_library_routes
from api.v1.routes import navidrome_library as navidrome_library_routes
from api.v1.routes import navidrome_preferences as navidrome_preferences_routes
from api.v1.routes import local_library as local_library_routes
from api.v1.routes import lastfm as lastfm_routes
from api.v1.routes import scrobble as scrobble_routes
from api.v1.routes import me_connections as me_connections_routes
from api.v1.routes import system as system_routes
from api.v1.routes import spotify as spotify_routes
from api.v1.routes import now_playing as now_playing_routes
from api.v1.routes import plex_library as plex_library_routes
from api.v1.routes import plex_auth as plex_auth_routes
from api.v1.routes import version as version_routes
from api.v1.routes import download as download_routes
from api.v1.routes import auth as auth_routes
from api.v1.routes import download_client as download_client_routes
from api.v1.routes import download_clients as download_clients_routes
from api.v1.routes import indexers as indexers_routes
from api.v1.routes import lidarr_import as lidarr_import_routes
from api.v1.routes import import_drop as import_drop_routes
from api.v1.routes import free_music as free_music_routes
from api.v1.routes import plugins as plugins_routes
from api.v1.routes import downloads_search as downloads_search_routes
from api.v1.routes import downloads as downloads_routes
from api.v1.routes import tracks as tracks_routes
from api.v1.routes import quarantine as quarantine_routes

from core.logging_config import configure_logging  # noqa: E402

configure_logging()
logger = logging.getLogger(__name__)

_ORPHAN_STAGING_MAX_AGE_SECONDS = 7 * 24 * 3600


async def _cleanup_orphan_staging(store, staging_path) -> None:
    """Delete staging/{task_id}/ dirs older than 7 days with no download_tasks row.
    Best-effort; one bad entry never aborts the sweep, a failure never blocks startup."""
    import shutil as _shutil
    import time as _time

    if staging_path is None or not staging_path.exists():
        return
    cutoff = _time.time() - _ORPHAN_STAGING_MAX_AGE_SECONDS
    try:
        entries = await asyncio.to_thread(lambda: list(staging_path.iterdir()))
    except OSError as exc:
        logger.warning("startup.orphan_sweep_skipped", extra={"error": str(exc)})
        return
    removed = 0
    for entry in entries:
        try:
            if not entry.is_dir() or entry.stat().st_mtime >= cutoff:
                continue
            if await store.get_task(entry.name) is not None:
                continue
            await asyncio.to_thread(_shutil.rmtree, entry, ignore_errors=True)
            removed += 1
        except OSError as exc:  # noqa: BLE001 - one bad dir must not abort the sweep
            logger.warning(
                "startup.orphan_sweep_entry_failed", extra={"error": str(exc)}
            )
    if removed:
        logger.info("startup.orphan_staging_swept", extra={"removed": removed})


async def _migrate_shared_avatar_to_first_admin(auth_store, cache_dir) -> None:
    """One-time: move the legacy shared avatar to the first admin's per-user avatar.
    Idempotent and best-effort - never blocks startup."""
    import shutil

    try:
        admin = await auth_store.get_first_admin()
        if admin is None or admin.avatar_url:
            return
        legacy_dir = cache_dir / "profile"
        source = next(
            (
                legacy_dir / f"avatar{ext}"
                for ext in (".jpg", ".png", ".webp", ".gif")
                if (legacy_dir / f"avatar{ext}").exists()
            ),
            None,
        )
        if source is None:
            return
        avatars_dir = cache_dir / "avatars"
        avatars_dir.mkdir(parents=True, exist_ok=True)
        dest = avatars_dir / f"{admin.id}{source.suffix}"
        await asyncio.to_thread(shutil.move, str(source), str(dest))
        await auth_store.update_user_profile(
            admin.id, avatar_url=f"/api/v1/profile/avatar/{admin.id}"
        )
        logger.info("Migrated legacy shared avatar to first admin %s", admin.id[:8])
    except Exception as exc:  # noqa: BLE001 - migration must never block startup
        logger.warning("Shared-avatar migration skipped: %s", exc)


async def _migrate_playlists_owner_to_admin(auth_store, playlist_repo) -> None:
    """One-time: assign ownerless playlists to the first admin as private.
    Idempotent (only touches user_id IS NULL rows), best-effort, never blocks startup."""
    try:
        admin = await auth_store.get_first_admin()
        if admin is None:
            return
        count = await asyncio.to_thread(playlist_repo.assign_unowned_to, admin.id)
        if count:
            logger.info(
                "Backfilled %d ownerless playlist(s) to first admin %s",
                count,
                admin.id[:8],
            )
    except Exception as exc:  # noqa: BLE001 - migration must never block startup
        logger.warning("Playlist owner backfill skipped: %s", exc)


async def _migrate_global_connection_to_first_admin(
    auth_store, preferences_service, user_connections_store, user_listening_prefs_store
) -> None:
    """One-time: seed the first admin's per-user connections + listening prefs from the
    existing global config, so an upgrading operator keeps their personalization/scrobbling
    instead of dropping to the trending fallback. Idempotent, best-effort, never blocks
    startup. The app api_key/shared_secret stay global and are not copied into user_connections."""
    try:
        admin = await auth_store.get_first_admin()
        if admin is None:
            return
        existing = {
            r.service for r in await user_connections_store.list_for_user(admin.id)
        }
        seeded: list[str] = []

        lb = preferences_service.get_listenbrainz_connection()
        if "listenbrainz" not in existing and lb.enabled and lb.user_token:
            await user_connections_store.upsert(
                admin.id,
                "listenbrainz",
                {"user_token": lb.user_token, "username": lb.username},
            )
            seeded.append("listenbrainz")

        lf = preferences_service.get_lastfm_connection()
        if "lastfm" not in existing and lf.session_key:
            await user_connections_store.upsert(
                admin.id,
                "lastfm",
                {"session_key": lf.session_key, "username": lf.username},
            )
            seeded.append("lastfm")

        existing_prefs = await user_listening_prefs_store.get(admin.id)
        if not existing_prefs.updated_at:  # empty => no row yet
            scrobble = preferences_service.get_scrobble_settings()
            source = preferences_service.get_primary_music_source().source
            meaningful = bool(
                seeded
                or scrobble.scrobble_to_lastfm
                or scrobble.scrobble_to_listenbrainz
                or source != "listenbrainz"
            )
            if meaningful:
                await user_listening_prefs_store.upsert(
                    admin.id,
                    scrobble_to_lastfm=scrobble.scrobble_to_lastfm,
                    scrobble_to_listenbrainz=scrobble.scrobble_to_listenbrainz,
                    primary_music_source=source,
                )
        if seeded:
            logger.info(
                "Backfilled global connection(s) %s to first admin %s",
                seeded,
                admin.id[:8],
            )
    except Exception as exc:  # noqa: BLE001 - migration must never block startup
        logger.warning("Global-connection backfill skipped: %s", exc)


@asynccontextmanager
async def lifespan(app: FastAPI):
    logger.info("Starting DroppedNeedle...")

    settings = get_settings()
    configured_level = getattr(logging, settings.log_level, logging.INFO)
    logging.getLogger().setLevel(configured_level)

    from core.config import migrate_legacy_config

    migrate_legacy_config()

    await init_app_state(app)
    await get_auth_service().cleanup_expired_tokens()

    # order matters: backfill_usernames must run before migrate_local_provider_to_username
    # (the latter copies each user's username into the local provider_uid). Idempotent.
    _auth_store = get_auth_store()
    await _auth_store.backfill_usernames()
    await _auth_store.migrate_local_provider_to_username()
    await _migrate_shared_avatar_to_first_admin(_auth_store, settings.cache_dir)
    from core.dependencies.repo_providers import get_playlist_repository

    await _migrate_playlists_owner_to_admin(_auth_store, get_playlist_repository())
    from core.dependencies import (
        get_user_connections_store,
        get_user_listening_prefs_store,
    )

    await _migrate_global_connection_to_first_admin(
        _auth_store,
        get_preferences_service(),
        get_user_connections_store(),
        get_user_listening_prefs_store(),
    )
    # Phase 5: rebuild a legacy global ignored_releases table into the per-user shape,
    # assigning existing rows to the first admin. Idempotent (no-op once per-user).
    _first_admin = await _auth_store.get_first_admin()
    if _first_admin is not None:
        from core.dependencies import get_mbid_store

        _ignored_migrated = await get_mbid_store().migrate_ignored_releases_to_user(
            _first_admin.id
        )
        if _ignored_migrated:
            logger.info(
                "Migrated %d legacy ignored releases to the first admin",
                _ignored_migrated,
            )

    preferences_service = get_preferences_service()
    settings.instance_id = preferences_service.get_instance_id()
    advanced_settings = preferences_service.get_advanced_settings()

    # validate config off-thread; log and continue rather than refuse to start, so a
    # bad path can't lock the owner out of the /settings/library UI
    from pathlib import Path as _Path

    from core.exceptions import ConfigurationError
    from core.startup_validator import StartupValidator

    _library_settings = preferences_service.get_typed_library_settings()
    try:
        await asyncio.to_thread(
            StartupValidator(
                [_Path(root.path) for root in _library_settings.library_roots],
                _Path(_library_settings.staging_path)
                if _library_settings.staging_path
                else None,
                slskd_downloads_path=_Path(get_settings().slskd_downloads_path),
            ).validate
        )
        logger.info(
            "startup.library_validated",
            extra={
                "library_path_count": len(_library_settings.library_roots),
                "staging_configured": bool(_library_settings.staging_path),
                "slskd_downloads_path": str(get_settings().slskd_downloads_path),
            },
        )
    except ConfigurationError as exc:
        # a bad path is operator-fixable at /settings/library, never fatal
        logger.error("startup.config_invalid", extra={"error": str(exc)})

    from core.dependencies import (
        get_acquisition_cleanup_service,
        get_download_orchestrator,
        get_library_scanner,
    )
    from core.tasks import (
        start_acquisition_cleanup_task,
        start_download_auto_retry_task,
        start_download_resume_task,
        start_download_watchdog_task,
        start_library_scan_resume_task,
    )

    start_library_scan_resume_task(
        get_library_scanner(),
        [_Path(root.path) for root in _library_settings.library_roots],
    )

    try:
        await get_acquisition_cleanup_service().recover_startup()
    except Exception:  # noqa: BLE001 - durable worker continues recovery after startup
        logger.exception("Acquisition cleanup startup recovery failed")
    start_acquisition_cleanup_task(get_acquisition_cleanup_service)
    start_download_resume_task(get_download_orchestrator())

    # drop-import housekeeping: jobs whose task died with the process are failed,
    # and staging dirs with nothing left to review are removed. Never blocks startup.
    from core.dependencies import get_drop_import_service

    try:
        await get_drop_import_service().sweep_stale()
    except Exception as exc:  # noqa: BLE001 - housekeeping must not block startup
        logger.warning("startup.drop_import_sweep_failed", extra={"error": str(exc)})

    # Free Music (D24): a task whose coroutine died with the process can never finish.
    from core.dependencies import get_free_music_service

    try:
        await get_free_music_service().sweep_stale()
    except Exception as exc:  # noqa: BLE001 - housekeeping must not block startup
        logger.warning("startup.free_music_sweep_failed", extra={"error": str(exc)})

    # plugin host (01b): discover + load admin-enabled plugins. Module imports are
    # blocking work; a broken plugin is isolated by the host and never blocks startup.
    from core.dependencies import get_plugin_host

    try:
        await asyncio.to_thread(get_plugin_host().load_all)
    except Exception as exc:  # noqa: BLE001 - plugins must never block startup
        logger.warning("startup.plugin_load_failed", extra={"error": str(exc)})
    # pass the provider (not an instance) so the watchdog always sweeps the current
    # orchestrator singleton, which is rebuilt when download-client settings are saved
    start_download_watchdog_task(get_download_orchestrator)
    start_download_auto_retry_task(get_download_orchestrator)

    from core.dependencies import get_download_store as _get_download_store

    _orphan_task = asyncio.create_task(
        _cleanup_orphan_staging(
            _get_download_store(),
            _Path(_library_settings.staging_path)
            if _library_settings.staging_path
            else None,
        )
    )
    TaskRegistry.get_instance().register("orphan-staging-cleanup", _orphan_task)

    # live now-playing presence: TTL-sweep stale native/compat sessions + poll the
    # upstream Jellyfin/Navidrome/Plex servers into the shared SSE feed
    from core.dependencies import (
        get_now_playing_service,
        get_home_service,
        get_jellyfin_library_service,
        get_navidrome_library_service,
        get_plex_library_service,
    )
    from services.now_playing_poller import run_now_playing_presence_loop

    _now_playing_task = asyncio.create_task(
        run_now_playing_presence_loop(
            get_now_playing_service(),
            get_home_service,
            get_jellyfin_library_service,
            get_navidrome_library_service,
            get_plex_library_service,
        )
    )
    TaskRegistry.get_instance().register("now-playing-presence", _now_playing_task)

    cache = get_cache()
    start_cache_cleanup_task(
        cache, interval=advanced_settings.memory_cache_cleanup_interval
    )
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
    from core.dependencies import get_scan_state_store

    start_library_auto_scan_task(
        get_library_scanner(), get_scan_state_store(), preferences_service
    )

    # warn (non-fatal) if the download client is unconfigured/unreachable
    from core.dependencies import get_download_client_repository, get_download_store

    try:
        await get_download_store().delete_expired_search_jobs()
    except Exception as exc:  # noqa: BLE001
        logger.warning("Download search-job cleanup skipped: %s", exc)
    try:
        _dl_client = get_download_client_repository()
        if not _dl_client.is_configured():
            logger.warning("startup.download_client_unconfigured")
        else:
            _dl_health = await _dl_client.health_check()
            logger.info(
                "startup.download_client_health", extra={"status": _dl_health.status}
            )
            if _dl_health.status != "ok":
                logger.warning(
                    "startup.download_client_unhealthy",
                    extra={"status": _dl_health.status, "detail": _dl_health.message},
                )
    except Exception as exc:  # noqa: BLE001
        logger.warning(
            "startup.download_client_check_skipped", extra={"error": str(exc)}
        )

    from core.tasks import warm_library_cache
    from core.dependencies import (
        get_album_service,
        get_background_workload_gate,
        get_library_db,
        get_sync_state_store,
    )

    background_workload_gate = get_background_workload_gate()

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
        warm_library_cache(
            library_service,
            get_album_service(),
            get_library_db(),
            background_workload_gate,
        )
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
                await background_workload_gate.wait_until_available()
                artists = await library_db.get_artists()
                albums = await library_db.get_albums()
                if artists or albums:
                    artists_dicts = [
                        {"mbid": a["mbid"], "name": a["name"]} for a in artists
                    ]
                    await library_service._precache_service.precache_library_resources(
                        artists_dicts, albums, resume=True
                    )
                else:
                    logger.warning(
                        "No cached artists/albums to resume sync with, clearing state"
                    )
                    await sync_state_store.clear_sync_state()
            except Exception as e:  # noqa: BLE001
                logger.error("Failed to resume interrupted sync: %s", e)
                await status_service.complete_sync(str(e))

        resume_task = asyncio.create_task(resume_sync())
        resume_task.add_done_callback(
            lambda t: logger.error("Resume sync failed: %s", t.exception())
            if t.exception()
            else None
        )
        TaskRegistry.get_instance().register("library-sync-resume", resume_task)

    # Phase 5: Home/Discover are per-user. Served on-demand (stale-while-revalidate) on the
    # first request, AND kept warm/converged in the background through the day by the per-user
    # warmer below (one user at a time, yields to active users). This is what lets discovery
    # "update as the day goes along" instead of only when a
    # user opens the page - and gives the rate-limited personalisation time to converge.
    from core.dependencies import (
        get_discover_queue_manager,
        get_discover_service,
    )

    start_discover_home_warmer_task(
        get_discover_service,
        get_home_service,
        get_auth_store,
        get_discover_queue_manager,
        workload_gate=background_workload_gate,
    )

    from core.dependencies import get_artist_discovery_service

    start_artist_discovery_cache_warming_task(
        get_artist_discovery_service,
        get_library_db(),
        interval=advanced_settings.artist_discovery_warm_interval,
        delay=advanced_settings.artist_discovery_warm_delay,
        workload_gate=background_workload_gate,
    )

    from core.dependencies import get_audiodb_image_service

    start_audiodb_sweep_task(
        get_audiodb_image_service(),
        get_library_db(),
        get_preferences_service(),
        precache_service=library_service._precache_service,
        workload_gate=background_workload_gate,
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
        mbid_task = asyncio.create_task(
            warm_jellyfin_mbid_index(get_jellyfin_repository())
        )
        mbid_task.add_done_callback(
            lambda t: None
            if t.cancelled()
            else (
                logger.error("Jellyfin MBID index warming failed: %s", t.exception())
                if t.exception()
                else None
            )
        )
        TaskRegistry.get_instance().register("jellyfin-mbid-warmup", mbid_task)

    navidrome_settings = preferences_service.get_navidrome_connection()
    if navidrome_settings.enabled:
        from core.tasks import warm_navidrome_mbid_cache

        nav_mbid_task = asyncio.create_task(warm_navidrome_mbid_cache())
        nav_mbid_task.add_done_callback(
            lambda t: None
            if t.cancelled()
            else (
                logger.error("Navidrome MBID cache warming failed: %s", t.exception())
                if t.exception()
                else None
            )
        )
        TaskRegistry.get_instance().register("navidrome-mbid-warmup", nav_mbid_task)

    plex_settings = preferences_service.get_plex_connection()
    if plex_settings.enabled:
        from core.tasks import warm_plex_mbid_cache

        plex_mbid_task = asyncio.create_task(warm_plex_mbid_cache())
        plex_mbid_task.add_done_callback(
            lambda t: None
            if t.cancelled()
            else (
                logger.error("Plex MBID cache warming failed: %s", t.exception())
                if t.exception()
                else None
            )
        )
        TaskRegistry.get_instance().register("plex-mbid-warmup", plex_mbid_task)

    from core.dependencies import get_requests_page_service

    requests_page_service = get_requests_page_service()

    start_request_status_sync_task(requests_page_service)

    from core.dependencies import get_new_release_service

    start_poll_new_releases_task(get_new_release_service())

    from core.dependencies import get_personal_mix_service

    start_personal_mix_refresh_task(get_personal_mix_service())

    from core.tasks import start_orphan_cover_demotion_task, start_store_prune_task
    from core.dependencies import (
        get_request_history_store,
        get_mbid_store,
        get_youtube_store,
        get_wanted_store,
    )

    start_orphan_cover_demotion_task(
        cover_disk_cache,
        library_db,
        interval=advanced_settings.orphan_cover_demote_interval_hours * 3600,
    )

    # get_wanted_store() here (before the prune task) also guarantees the
    # wanted_watches table exists before the request-history prune's
    # watch-exclusion subquery ever runs (Wanted plan §5.5)
    start_store_prune_task(
        get_request_history_store(),
        get_mbid_store(),
        get_youtube_store(),
        request_retention_days=advanced_settings.request_history_retention_days,
        ignored_retention_days=advanced_settings.ignored_releases_retention_days,
        interval=advanced_settings.store_prune_interval_hours * 3600,
        wanted_store=get_wanted_store(),
    )

    from core.tasks import (
        start_background_upgrade_scan_task,
        start_recycle_bin_prune_task,
    )

    start_recycle_bin_prune_task(preferences_service)

    # NOTE: get_auth_store is already imported at module level - re-importing it here
    # would shadow it as a function-local for ALL of lifespan and break the earlier
    # use at startup (UnboundLocalError).
    from core.dependencies import get_download_service

    start_background_upgrade_scan_task(
        get_download_service, get_auth_store(), preferences_service
    )

    from core.tasks import start_wanted_watcher_task
    from core.dependencies import get_wanted_watcher_service

    # provider, not an instance: each sweep resolves the current watcher (whose
    # DownloadService is itself resolved fresh - settings saves rebuild it)
    start_wanted_watcher_task(get_wanted_watcher_service)

    from core.tasks import start_events_watcher_task
    from core.dependencies import get_events_watcher_service

    def _events_poll_time() -> str:
        return preferences_service.get_events_settings().poll_time

    # provider + time callable: settings saves rebuild the watcher chain and
    # move the daily slot without a restart (re-read every scheduler tick)
    start_events_watcher_task(get_events_watcher_service, _events_poll_time)

    logger.info("DroppedNeedle started successfully")

    try:
        yield
    finally:
        logger.info("Shutting down DroppedNeedle...")

        registry = TaskRegistry.get_instance()
        settings = get_settings()
        await registry.cancel_all(grace_period=settings.shutdown_grace_period)

        try:
            await cleanup_app_state()
        except Exception as e:  # noqa: BLE001
            logger.error("Error during cleanup: %s", e)

        logger.info("DroppedNeedle shut down successfully")


app = FastAPI(
    title="DroppedNeedle",
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
app.add_exception_handler(PermissionDeniedError, permission_denied_handler)
app.add_exception_handler(ConflictError, conflict_error_handler)
app.add_exception_handler(StaleRevisionError, stale_revision_error_handler)
app.add_exception_handler(RevisionOverflowError, revision_overflow_error_handler)
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
        "/api/v1/auth/password-recovery/reset": (1.0, 5),
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

# Compat hardening, scoped to /subsonic + /jellyfin. Added last so CompatCORS is
# outermost and OPTIONS preflights short-circuit before rate-limit + auth.
from api.compat.common.cors import CompatCORSMiddleware  # noqa: E402

app.add_middleware(CompatCORSMiddleware)


@app.get("/health")
def health_check():
    return {"status": "ok", "message": "DroppedNeedle backend running"}


v1_router = APIRouter(prefix="/api/v1")
v1_router.include_router(search.router)
v1_router.include_router(requests.router)
v1_router.include_router(library.router)
v1_router.include_router(library_scan_routes.router)
v1_router.include_router(status.router)
v1_router.include_router(covers.router)
v1_router.include_router(artists.router)
v1_router.include_router(following.router)
v1_router.include_router(albums.router)
v1_router.include_router(settings.router)
v1_router.include_router(library_policy_routes.router)
v1_router.include_router(home.router)
v1_router.include_router(wrapped.router)
# literal /discover/batches paths registered before the discover router (which owns
# the broader /discover prefix) so they can never be shadowed
v1_router.include_router(discovery_batches_routes.router)
v1_router.include_router(discover.router)
v1_router.include_router(youtube_routes.router)
v1_router.include_router(cache_routes.router)
v1_router.include_router(cache_status_routes.router)
v1_router.include_router(requests_page_routes.router)
v1_router.include_router(stream_routes.router)
v1_router.include_router(jellyfin_library_routes.router)
v1_router.include_router(navidrome_library_routes.router)
v1_router.include_router(navidrome_preferences_routes.router)
v1_router.include_router(plex_library_routes.router)
v1_router.include_router(plex_auth_routes.router)
v1_router.include_router(local_library_routes.router)
v1_router.include_router(lastfm_routes.router)
v1_router.include_router(scrobble_routes.router)
v1_router.include_router(me_connections_routes.router)
v1_router.include_router(system_routes.router)
v1_router.include_router(spotify_routes.router)
v1_router.include_router(now_playing_routes.router)
v1_router.include_router(profile.router)
v1_router.include_router(playlists.router)
v1_router.include_router(version_routes.router)
v1_router.include_router(download_routes.router)
v1_router.include_router(auth_routes.router)
v1_router.include_router(download_client_routes.router)
v1_router.include_router(download_clients_routes.router)
v1_router.include_router(indexers_routes.router)
v1_router.include_router(lidarr_import_routes.router)
v1_router.include_router(import_drop_routes.router)
v1_router.include_router(free_music_routes.router)
v1_router.include_router(plugins_routes.router)
v1_router.include_router(downloads_search_routes.router)
# quarantine + search routers declare literal /downloads/{quarantine,search}/* paths;
# they MUST be registered before downloads_routes, whose catch-all GET /downloads/{task_id}
# would otherwise capture the literal segment (e.g. /downloads/quarantine).
v1_router.include_router(quarantine_routes.router)
v1_router.include_router(downloads_routes.router)
v1_router.include_router(tracks_routes.router)
from api.v1.routes import connect_apps_routes  # noqa: E402

v1_router.include_router(connect_apps_routes.router)
app.include_router(v1_router)

# Compat shims mount at the app root (not /api/v1) so clients append native paths;
# must register before mount_frontend's SPA catch-all.
from api.compat.subsonic.router import router as subsonic_router  # noqa: E402
from api.compat.jellyfin.router import router as jellyfin_router  # noqa: E402

app.include_router(subsonic_router)
app.include_router(jellyfin_router)

# Canonicalise compat path casing before routing (some clients lowercase the path).
# After the shims (needs their routes) and outside the rate-limiter so its exact-path
# check sees the canonical form.
from api.compat.common.path_case import CompatPathCaseMiddleware  # noqa: E402

app.add_middleware(
    CompatPathCaseMiddleware,
    routes=[*subsonic_router.routes, *jellyfin_router.routes],
)

# Trust X-Forwarded-Proto / X-Forwarded-Host from the reverse proxy so that
# request.base_url reflects https:// when TLS is terminated upstream.
# Must be outermost (added last) so it rewrites the scope before any other
# middleware or route handler sees the request.
from uvicorn.middleware.proxy_headers import ProxyHeadersMiddleware  # noqa: E402

app.add_middleware(
    ProxyHeadersMiddleware, trusted_hosts=get_settings().trusted_proxy_ips
)

mount_frontend(app)
