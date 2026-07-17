"""Shared operational startup retained by the target-only application."""

from __future__ import annotations

import asyncio
import logging
import shutil
import time
from pathlib import Path
from typing import Any

from core.task_registry import TaskRegistry

logger = logging.getLogger(__name__)

_ORPHAN_STAGING_MAX_AGE_SECONDS = 7 * 24 * 60 * 60


async def _cleanup_orphan_staging(store: Any, staging_path: Path | None) -> None:
    if staging_path is None or not staging_path.exists():
        return
    cutoff = time.time() - _ORPHAN_STAGING_MAX_AGE_SECONDS
    try:
        entries = await asyncio.to_thread(lambda: list(staging_path.iterdir()))
    except OSError as error:
        logger.warning("startup.orphan_sweep_skipped", extra={"error": str(error)})
        return
    removed = 0
    for entry in entries:
        try:
            if not entry.is_dir() or entry.stat().st_mtime >= cutoff:
                continue
            if await store.get_task(entry.name) is not None:
                continue
            await asyncio.to_thread(shutil.rmtree, entry, ignore_errors=True)
            removed += 1
        except OSError as error:
            logger.warning(
                "startup.orphan_sweep_entry_failed", extra={"error": str(error)}
            )
    if removed:
        logger.info("startup.orphan_staging_swept", extra={"removed": removed})


async def _migrate_shared_avatar(auth_store: Any, cache_dir: Path) -> None:
    try:
        admin = await auth_store.get_first_admin()
        if admin is None or admin.avatar_url:
            return
        legacy_dir = cache_dir / "profile"
        source = next(
            (
                legacy_dir / f"avatar{extension}"
                for extension in (".jpg", ".png", ".webp", ".gif")
                if (legacy_dir / f"avatar{extension}").exists()
            ),
            None,
        )
        if source is None:
            return
        destination_dir = cache_dir / "avatars"
        destination_dir.mkdir(parents=True, exist_ok=True)
        destination = destination_dir / f"{admin.id}{source.suffix}"
        await asyncio.to_thread(shutil.move, str(source), str(destination))
        await auth_store.update_user_profile(
            admin.id, avatar_url=f"/api/v1/profile/avatar/{admin.id}"
        )
    except Exception as error:  # noqa: BLE001 - an old avatar cannot block startup
        logger.warning("Shared-avatar migration skipped: %s", error)


async def _migrate_global_connections(
    auth_store: Any,
    preferences: Any,
    connections: Any,
    listening_preferences: Any,
) -> None:
    try:
        admin = await auth_store.get_first_admin()
        if admin is None:
            return
        existing = {
            record.service for record in await connections.list_for_user(admin.id)
        }
        seeded = False
        listenbrainz = preferences.get_listenbrainz_connection()
        if (
            "listenbrainz" not in existing
            and listenbrainz.enabled
            and listenbrainz.user_token
        ):
            await connections.upsert(
                admin.id,
                "listenbrainz",
                {
                    "user_token": listenbrainz.user_token,
                    "username": listenbrainz.username,
                },
            )
            seeded = True
        lastfm = preferences.get_lastfm_connection()
        if "lastfm" not in existing and lastfm.session_key:
            await connections.upsert(
                admin.id,
                "lastfm",
                {"session_key": lastfm.session_key, "username": lastfm.username},
            )
            seeded = True
        current = await listening_preferences.get(admin.id)
        if not current.updated_at:
            scrobble = preferences.get_scrobble_settings()
            source = preferences.get_primary_music_source().source
            if (
                seeded
                or scrobble.scrobble_to_lastfm
                or scrobble.scrobble_to_listenbrainz
                or source != "listenbrainz"
            ):
                await listening_preferences.upsert(
                    admin.id,
                    scrobble_to_lastfm=scrobble.scrobble_to_lastfm,
                    scrobble_to_listenbrainz=scrobble.scrobble_to_listenbrainz,
                    primary_music_source=source,
                )
    except Exception as error:  # noqa: BLE001 - old global settings cannot block startup
        logger.warning("Global-connection backfill skipped: %s", error)


async def run_target_one_time_migrations(
    *, auth_store: Any, preferences: Any, cache_dir: Path
) -> None:
    """Run target data ratchets without touching the retained legacy catalog."""

    from core.dependencies import (
        get_mbid_store,
        get_request_history_store,
        get_user_connections_store,
        get_user_listening_prefs_store,
    )
    from core.dependencies.cache_providers import get_native_library_store

    store = get_native_library_store()
    get_mbid_store()
    canonicalized_requests = (
        await get_request_history_store().async_canonicalize_known_release_aliases()
    )
    if canonicalized_requests:
        logger.info(
            "Canonicalized %d request-history release aliases",
            canonicalized_requests,
        )
    artwork_count = await store.backfill_identified_provider_artwork(
        updated_at=time.time()
    )
    if artwork_count:
        logger.info(
            "Backfilled %d identified album artwork associations", artwork_count
        )
    await auth_store.backfill_usernames()
    await auth_store.migrate_local_provider_to_username()
    await _migrate_shared_avatar(auth_store, cache_dir)
    admin = await auth_store.get_first_admin()
    if admin is not None:
        changed = await store.assign_unowned_target_playlists(admin.id)
        if changed:
            logger.info("Backfilled %d ownerless target playlists", changed)
        await get_mbid_store().migrate_ignored_releases_to_user(admin.id)
    await _migrate_global_connections(
        auth_store,
        preferences,
        get_user_connections_store(),
        get_user_listening_prefs_store(),
    )


def _register_task(name: str, coroutine: Any) -> None:
    task = asyncio.create_task(coroutine)
    TaskRegistry.get_instance().register(name, task)


async def start_target_operational_runtime(
    *, settings: Any, preferences: Any, auth_store: Any
) -> None:
    """Start non-catalog work with target-aware catalog dependencies."""

    from core.dependencies import (
        get_audiodb_browse_queue,
        get_audiodb_image_service,
        get_download_client_repository,
        get_target_events_watcher_service,
        get_jellyfin_repository,
        get_jellyfin_library_service,
        get_mbid_store,
        get_target_navidrome_library_service,
        get_now_playing_service,
        get_target_plex_library_service,
        get_request_history_store,
        get_target_artist_discovery_service,
        get_target_consumer_composition,
        get_target_discover_service,
        get_target_discover_queue_manager,
        get_target_download_orchestrator,
        get_target_download_service,
        get_target_drop_import_service,
        get_target_free_music_service,
        get_target_home_service,
        get_target_new_release_service,
        get_target_personal_mix_service,
        get_target_requests_page_service,
        get_target_wanted_watcher_service,
        get_wanted_store,
        get_youtube_store,
    )
    from core.dependencies.auth_providers import get_auth_store
    from core.dependencies.repo_providers import get_download_store
    from core.dependencies.service_providers import get_plugin_host
    from core.tasks import (
        start_artist_discovery_cache_warming_task,
        start_audiodb_sweep_task,
        start_background_upgrade_scan_task,
        start_discover_home_warmer_task,
        start_download_auto_retry_task,
        start_download_resume_task,
        start_download_watchdog_task,
        start_events_watcher_task,
        start_orphan_cover_demotion_task,
        start_personal_mix_refresh_task,
        start_poll_new_releases_task,
        start_recycle_bin_prune_task,
        start_request_status_sync_task,
        start_store_prune_task,
        start_wanted_watcher_task,
        warm_jellyfin_mbid_index,
        warm_navidrome_mbid_cache,
        warm_plex_mbid_cache,
    )
    from services.now_playing_poller import run_now_playing_presence_loop

    target = get_target_consumer_composition()
    library = target.repository

    start_download_resume_task(get_target_download_orchestrator())
    start_download_watchdog_task(get_target_download_orchestrator)
    start_download_auto_retry_task(get_target_download_orchestrator)

    try:
        await get_target_drop_import_service().sweep_stale()
    except Exception as error:  # noqa: BLE001 - housekeeping cannot block startup
        logger.warning("startup.drop_import_sweep_failed", extra={"error": str(error)})
    try:
        await get_target_free_music_service().sweep_stale()
    except Exception as error:  # noqa: BLE001 - housekeeping cannot block startup
        logger.warning("startup.free_music_sweep_failed", extra={"error": str(error)})
    try:
        await asyncio.to_thread(get_plugin_host().load_all)
    except Exception as error:  # noqa: BLE001 - one plugin cannot block startup
        logger.warning("startup.plugin_load_failed", extra={"error": str(error)})

    staging = preferences.get_typed_library_settings().staging_path
    _register_task(
        "orphan-staging-cleanup",
        _cleanup_orphan_staging(
            get_download_store(), Path(staging) if staging else None
        ),
    )
    _register_task(
        "now-playing-presence",
        run_now_playing_presence_loop(
            get_now_playing_service(),
            get_target_home_service,
            get_jellyfin_library_service,
            get_target_navidrome_library_service,
            get_target_plex_library_service,
        ),
    )

    try:
        await get_download_store().delete_expired_search_jobs()
    except Exception as error:  # noqa: BLE001 - cleanup cannot block startup
        logger.warning("Download search-job cleanup skipped: %s", error)
    try:
        client = get_download_client_repository()
        if client.is_configured():
            health = await client.health_check()
            logger.info(
                "startup.download_client_health", extra={"status": health.status}
            )
        else:
            logger.warning("startup.download_client_unconfigured")
    except Exception as error:  # noqa: BLE001 - health is advisory at startup
        logger.warning(
            "startup.download_client_check_skipped", extra={"error": str(error)}
        )

    advanced = preferences.get_advanced_settings()
    start_discover_home_warmer_task(
        get_target_discover_service,
        get_target_home_service,
        get_auth_store,
        get_target_discover_queue_manager,
    )
    start_artist_discovery_cache_warming_task(
        get_target_artist_discovery_service,
        library,
        interval=advanced.artist_discovery_warm_interval,
        delay=advanced.artist_discovery_warm_delay,
    )
    start_audiodb_sweep_task(
        get_audiodb_image_service(), library, preferences, precache_service=None
    )
    get_audiodb_browse_queue().start_consumer(get_audiodb_image_service(), preferences)

    jellyfin = preferences.get_jellyfin_connection()
    if jellyfin.enabled:
        _register_task(
            "jellyfin-mbid-warmup", warm_jellyfin_mbid_index(get_jellyfin_repository())
        )
    if preferences.get_navidrome_connection().enabled:
        _register_task(
            "navidrome-mbid-warmup",
            warm_navidrome_mbid_cache(get_target_navidrome_library_service),
        )
    if preferences.get_plex_connection().enabled:
        _register_task(
            "plex-mbid-warmup",
            warm_plex_mbid_cache(get_target_plex_library_service),
        )

    start_request_status_sync_task(get_target_requests_page_service())
    start_poll_new_releases_task(get_target_new_release_service())
    start_personal_mix_refresh_task(get_target_personal_mix_service())
    start_orphan_cover_demotion_task(
        target.covers.disk_cache,
        library,
        interval=advanced.orphan_cover_demote_interval_hours * 3600,
    )
    start_store_prune_task(
        get_request_history_store(),
        get_mbid_store(),
        get_youtube_store(),
        request_retention_days=advanced.request_history_retention_days,
        ignored_retention_days=advanced.ignored_releases_retention_days,
        interval=advanced.store_prune_interval_hours * 3600,
        wanted_store=get_wanted_store(),
    )
    start_recycle_bin_prune_task(preferences)
    start_background_upgrade_scan_task(
        get_target_download_service, auth_store, preferences
    )
    start_wanted_watcher_task(get_target_wanted_watcher_service)

    def events_poll_time() -> str:
        return preferences.get_events_settings().poll_time

    start_events_watcher_task(get_target_events_watcher_service, events_poll_time)
