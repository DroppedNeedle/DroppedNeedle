import asyncio
import logging
from datetime import datetime, timedelta
from time import time, monotonic
from typing import TYPE_CHECKING, Optional
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.disk_cache import DiskMetadataCache
from infrastructure.memory import get_rss_bytes, trim_malloc
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.serialization import clone_with_updates
from infrastructure.validators import is_unknown_mbid, is_valid_mbid
from services.library_service import LibraryService
from services.preferences_service import PreferencesService
from core.task_registry import TaskRegistry
from repositories.listenbrainz_repository import listenbrainz_rate_limit_cooldown_active

if TYPE_CHECKING:
    from services.album_service import AlbumService
    from services.native.download_orchestrator import DownloadOrchestrator
    from services.audiodb_image_service import AudioDBImageService
    from services.library_precache_service import LibraryPrecacheService
    from infrastructure.persistence import LibraryDB
    from infrastructure.persistence.request_history import RequestHistoryStore
    from infrastructure.persistence.mbid_store import MBIDStore
    from infrastructure.persistence.youtube_store import YouTubeStore
    from infrastructure.persistence.native_library_store import NativeLibraryStore
    from infrastructure.persistence.wanted_store import WantedStore
    from services.requests_page_service import RequestsPageService
    from services.native.new_release_service import NewReleaseService
    from services.native.background_workload_gate import BackgroundWorkloadGate
    from services.personal_mix_service import PersonalMixService
    from repositories.coverart_disk_cache import CoverDiskCache

logger = logging.getLogger(__name__)


async def cleanup_cache_periodically(
    cache: CacheInterface, interval: int = 300
) -> None:
    while True:
        try:
            await asyncio.sleep(interval)
            await cache.cleanup_expired()
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Cache cleanup task failed: %s", e, exc_info=True)


def start_cache_cleanup_task(
    cache: CacheInterface, interval: int = 300
) -> asyncio.Task:
    task = asyncio.create_task(cleanup_cache_periodically(cache, interval=interval))
    TaskRegistry.get_instance().register("cache-cleanup", task)
    return task


_MEMORY_MAINTENANCE_INTERVAL = 600


def _log_memory_usage(
    cache: CacheInterface,
    rss_before: int | None,
    rss_after: int | None,
    trimmed: bool,
) -> None:
    parts: list[str] = []
    if rss_after is not None:
        parts.append(f"rss={rss_after / 1048576:.0f}MB")
        if trimmed and rss_before is not None:
            freed = (rss_before - rss_after) / 1048576
            if freed >= 1:
                parts.append(f"trim=-{freed:.0f}MB")
    parts.append(f"cache={cache.size()} entries")
    logger.info("memory: %s", " ".join(parts))


async def memory_maintenance_periodically(
    cache: CacheInterface,
    interval: int = _MEMORY_MAINTENANCE_INTERVAL,
) -> None:
    while True:
        try:
            await asyncio.sleep(interval)
            rss_before = get_rss_bytes()
            trimmed = await asyncio.to_thread(trim_malloc)
            rss_after = get_rss_bytes()
            _log_memory_usage(cache, rss_before, rss_after, trimmed)
        except asyncio.CancelledError:
            break
        except Exception as e:  # noqa: BLE001
            logger.error("Memory maintenance task failed: %s", e, exc_info=True)


def start_memory_maintenance_task(
    cache: CacheInterface,
    interval: int = _MEMORY_MAINTENANCE_INTERVAL,
) -> asyncio.Task:
    task = asyncio.create_task(
        memory_maintenance_periodically(cache, interval=interval)
    )
    TaskRegistry.get_instance().register("memory-maintenance", task)
    return task


async def cleanup_disk_cache_periodically(
    disk_cache: DiskMetadataCache,
    interval: int = 600,
    cover_disk_cache: Optional["CoverDiskCache"] = None,
) -> None:
    while True:
        try:
            await asyncio.sleep(interval)
            await disk_cache.cleanup_expired_recent()
            await disk_cache.enforce_recent_size_limits()
            await disk_cache.cleanup_expired_covers()
            await disk_cache.enforce_cover_size_limits()
            if cover_disk_cache:
                await cover_disk_cache.enforce_size_limit(force=True)
                await asyncio.to_thread(cover_disk_cache.cleanup_expired)
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Disk cache cleanup task failed: %s", e, exc_info=True)


def start_disk_cache_cleanup_task(
    disk_cache: DiskMetadataCache,
    interval: int = 600,
    cover_disk_cache: Optional["CoverDiskCache"] = None,
) -> asyncio.Task:
    task = asyncio.create_task(
        cleanup_disk_cache_periodically(
            disk_cache, interval=interval, cover_disk_cache=cover_disk_cache
        )
    )
    TaskRegistry.get_instance().register("disk-cache-cleanup", task)
    return task


async def export_navidrome_playlists_periodically(
    interval: int = 300,
) -> None:
    """Keep Navidrome's imported playlists in step with DroppedNeedle's.

    Polled rather than hooked into every playlist mutation, so no edit path
    can be missed. Unchanged files are not rewritten.
    """
    from core.dependencies import (
        get_navidrome_playlist_export_service,
        get_preferences_service,
    )

    while True:
        try:
            await asyncio.sleep(interval)
            settings = get_preferences_service().get_navidrome_connection()
            if not settings.playlist_sync_enabled or not settings.playlist_sync_path:
                continue
            result = await get_navidrome_playlist_export_service().sync(
                target_dir=settings.playlist_sync_path,
                scope=settings.playlist_sync_scope,
                remove_deleted=settings.playlist_sync_remove_deleted,
            )
            if result.removal_failures:
                logger.error(
                    "Navidrome playlist export could not remove %d superseded "
                    "playlist file(s); they remain visible in Navidrome and will "
                    "be retried: %s",
                    result.removal_failures,
                    result.message,
                )
            elif not result.success:
                logger.warning("Navidrome playlist export: %s", result.message)
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Navidrome playlist export task failed: %s", e, exc_info=True)


def start_navidrome_playlist_export_task(interval: int = 300) -> asyncio.Task:
    task = asyncio.create_task(
        export_navidrome_playlists_periodically(interval=interval)
    )
    TaskRegistry.get_instance().register("navidrome-playlist-export", task)
    return task


def start_download_resume_task(orchestrator: "DownloadOrchestrator") -> asyncio.Task:
    """(AUD-3) Resume in-progress / queued downloads on startup without blocking it;
    the orchestrator dispatches each resumed task in the background."""

    async def _resume() -> None:
        try:
            await orchestrator.startup_resume()
        except Exception as e:  # noqa: BLE001
            logger.error("Download resume failed: %s", e, exc_info=True)

    task = asyncio.create_task(_resume())
    TaskRegistry.get_instance().register("download-resume", task)
    task.add_done_callback(
        lambda t: logger.error("Download resume task error: %s", t.exception())
        if not t.cancelled() and t.exception()
        else None
    )
    return task


async def run_acquisition_cleanup_periodically(
    get_cleanup_service, interval: float = 30.0
) -> None:
    """Drain cleanup debt with freshly resolved clients."""

    worker_id = "acquisition-cleanup-worker"
    while True:
        try:
            service = get_cleanup_service()
            await service.reconcile_legacy_mount()
            await service.run_once(worker_id)
        except asyncio.CancelledError:
            break
        except Exception:  # noqa: BLE001 - durable cleanup survives one failed sweep
            logger.exception("Acquisition cleanup sweep failed")
        try:
            await asyncio.sleep(interval)
        except asyncio.CancelledError:
            break


def start_acquisition_cleanup_task(get_cleanup_service) -> asyncio.Task:
    task = asyncio.create_task(
        run_acquisition_cleanup_periodically(get_cleanup_service)
    )
    TaskRegistry.get_instance().register("acquisition-cleanup", task)
    task.add_done_callback(
        lambda value: logger.error(
            "Acquisition cleanup task error: %s", value.exception()
        )
        if not value.cancelled() and value.exception()
        else None
    )
    return task


_ACQUISITION_ORPHAN_RECONCILE_INTERVAL = 3600.0
_ACQUISITION_ORPHAN_RECONCILE_INITIAL_DELAY = 600.0


async def run_acquisition_orphan_reconcile_periodically(
    get_cleanup_service,
    interval: float = _ACQUISITION_ORPHAN_RECONCILE_INTERVAL,
    delay: float = _ACQUISITION_ORPHAN_RECONCILE_INITIAL_DELAY,
) -> None:
    """Close the pre-journal crash gap (#131): sweep complete-dir folders named for
    DroppedNeedle jobs that no attempt journal owns. Sleep-first so startup
    recovery journals legacy workspaces before the first destructive pass."""
    await asyncio.sleep(delay)
    while True:
        try:
            service = get_cleanup_service()
            removed = await service.reconcile_orphan_folders()
            if removed:
                logger.info(
                    "Acquisition orphan reconcile removed %d folder(s)", removed
                )
        except asyncio.CancelledError:
            break
        except Exception:  # noqa: BLE001 - durable worker survives one failed sweep
            logger.exception("Acquisition orphan reconcile sweep failed")
        try:
            await asyncio.sleep(interval)
        except asyncio.CancelledError:
            break


def start_acquisition_orphan_reconcile_task(get_cleanup_service) -> asyncio.Task:
    task = asyncio.create_task(
        run_acquisition_orphan_reconcile_periodically(get_cleanup_service)
    )
    TaskRegistry.get_instance().register("acquisition-orphan-reconcile", task)
    task.add_done_callback(
        lambda value: logger.error(
            "Acquisition orphan reconcile task error: %s", value.exception()
        )
        if not value.cancelled() and value.exception()
        else None
    )
    return task


async def warm_library_cache(
    library_service: LibraryService,
    album_service: "AlbumService",
    library_db: "LibraryDB",
    workload_gate: "BackgroundWorkloadGate | None" = None,
) -> None:
    try:
        await asyncio.sleep(5)

        if workload_gate is not None:
            await workload_gate.wait_until_available()
        albums_data = await library_db.get_recent_albums(limit=30)

        if not albums_data:
            return

        max_warm = 30
        albums_to_warm = albums_data[:max_warm]

        warmed = 0
        for i, album_data in enumerate(albums_to_warm):
            if workload_gate is not None:
                await workload_gate.wait_until_available()
            mbid = album_data.get("mbid")
            if is_valid_mbid(mbid):
                try:
                    if not await album_service.is_album_cached(mbid):
                        await album_service.get_album_info(
                            mbid,
                            priority=RequestPriority.BACKGROUND_SYNC,
                        )
                        warmed += 1

                    if i % 5 == 0:
                        await asyncio.sleep(1)

                except Exception as e:
                    logger.error(
                        "Library cache warm item failed album=%s mbid=%s error=%s",
                        album_data.get("title"),
                        mbid,
                        e,
                        exc_info=True,
                    )
                    continue

    except Exception as e:
        logger.error("Library cache warming failed: %s", e, exc_info=True)


async def warm_jellyfin_mbid_index(jellyfin_repo: "JellyfinRepository") -> None:
    await asyncio.sleep(8)
    try:
        await jellyfin_repo.build_mbid_index()
    except Exception as e:
        logger.error("Jellyfin MBID index warming failed: %s", e, exc_info=True)


async def warm_navidrome_mbid_cache(service_getter=None) -> None:
    if service_getter is None:
        from core.dependencies import get_navidrome_library_service

        service_getter = get_navidrome_library_service

    await asyncio.sleep(12)
    while True:
        try:
            service = service_getter()
            await service.warm_mbid_cache()
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Navidrome MBID cache warming failed: %s", e, exc_info=True)
        try:
            await asyncio.sleep(14400)
        except asyncio.CancelledError:
            break


async def warm_plex_mbid_cache(service_getter=None) -> None:
    if service_getter is None:
        from core.dependencies import get_plex_library_service

        service_getter = get_plex_library_service

    await asyncio.sleep(15)
    while True:
        try:
            service = service_getter()
            await service.warm_mbid_cache()
            await service.persist_if_dirty()
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Plex MBID cache warming failed: %s", e, exc_info=True)
        try:
            await asyncio.sleep(14400)
        except asyncio.CancelledError:
            break


async def warm_artist_discovery_cache_periodically(
    artist_discovery_service_getter,
    library_db: "LibraryDB",
    interval: int = 14400,
    delay: float = 0.5,
    workload_gate: "BackgroundWorkloadGate | None" = None,
) -> None:
    await asyncio.sleep(
        300
    )  # Allow initial library sync to complete before warming caches

    while True:
        try:
            artist_cursor = ""
            while True:
                if workload_gate is not None:
                    await workload_gate.wait_until_available()
                page = await library_db.get_artist_mbid_page(
                    after_mbid=artist_cursor, limit=500
                )
                if not page:
                    break

                artist_cursor = page[-1]
                mbids = [mbid for mbid in page if is_valid_mbid(mbid)]
                for mbid in mbids:
                    if workload_gate is not None:
                        await workload_gate.run_warmer_unit(
                            lambda mbid=mbid: artist_discovery_service_getter().precache_artist_discovery(
                                [mbid], delay=delay
                            )
                        )
                    else:
                        await (
                            artist_discovery_service_getter().precache_artist_discovery(
                                [mbid], delay=delay
                            )
                        )

                if len(page) < 500:
                    break
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Artist discovery cache warming failed: %s", e, exc_info=True)

        try:
            await asyncio.sleep(interval)
        except asyncio.CancelledError:
            break


def start_artist_discovery_cache_warming_task(
    artist_discovery_service_getter,
    library_db: "LibraryDB",
    interval: int = 14400,
    delay: float = 0.5,
    workload_gate: "BackgroundWorkloadGate | None" = None,
) -> asyncio.Task:
    task = asyncio.create_task(
        warm_artist_discovery_cache_periodically(
            artist_discovery_service_getter,
            library_db,
            interval=interval,
            delay=delay,
            workload_gate=workload_gate,
        )
    )
    TaskRegistry.get_instance().register("artist-discovery-warming", task)
    return task


# Proactive per-user Discover/Home warmer.
# Keeps each user's Discover/Home caches warm and CONVERGED through the
# day, not just while they're looking at the page. During the ListenBrainz-popularity outage
# personalisation is reconstructed from Last.fm via MusicBrainz at a hard 1 req/s, which an
# on-visit build can't finish; a background prewarm (uncancellable) drains those resolutions
# and banks them to mbid_store so the following normal-budget build finds them cached. One
# loop, ONE user at a time (the single global MB 1/s queue makes concurrency pointless), and
# it yields whenever a user is actively browsing.
DISCOVER_WARMER_STARTUP_DELAY = 5  # after core startup, before a normal first visit
DISCOVER_WARMER_INTERVAL = 90  # floor between per-user warm ticks
DISCOVER_WARMER_ENUM_TTL = 600  # re-enumerate eligible users at most this often
DISCOVER_WARMER_REFRESH_INTERVAL = 6 * 3600  # re-warm a converged user this often
DISCOVER_WARMER_PERSONALIZING_RETRY = 600  # re-warm a still-converging user this often
DISCOVER_WARMER_MAX_ATTEMPTS = 4  # stop fast-retrying a user that won't converge
DISCOVER_WARMER_HARD_CAP = 300  # per-user wall-clock ceiling vs a wedged build


async def _enumerate_warmer_users(auth_store) -> list[str]:
    eligible: list[str] = []
    offset = 0
    while True:
        users = await auth_store.list_users(limit=100, offset=offset)
        if not users:
            break
        eligible.extend(u.id for u in users)
        if len(users) < 100:
            break
        offset += 100
    return eligible


async def _pick_due_warmer_user(
    eligible: list[str], last_warmed: dict, attempts: dict, now: float, discover
) -> Optional[str]:
    """The neediest not-currently-building user: never-warmed > still-personalising > stale.
    Skips any user whose live on-visit build is already registered (they own it)."""
    registry = TaskRegistry.get_instance()
    stale_fallback: Optional[str] = None
    for uid in eligible:
        if registry.is_running(f"discover-homepage-warm-{uid}"):
            continue
        last = last_warmed.get(uid)
        if last is None:
            return uid  # never warmed - highest priority
        age = now - last
        if age < DISCOVER_WARMER_PERSONALIZING_RETRY:
            continue  # warmed very recently - not due yet, skip the freshness probe
        has_cache, still_converging = await discover.peek_freshness(uid)
        # A warmed user with NO cached response means the last build was cut at the hard cap
        # (a heavy user mid-outage) - keep them in the fast-retry tier, not the 6h one.
        if (not has_cache or still_converging) and attempts.get(
            uid, 0
        ) < DISCOVER_WARMER_MAX_ATTEMPTS:
            return uid  # still converging - retry soon
        if stale_fallback is None and age > DISCOVER_WARMER_REFRESH_INTERVAL:
            stale_fallback = uid
    return stale_fallback


async def _run_registered_warmer_build(name: str, coro) -> None:
    """Run a warm build under the SAME registry name the on-visit SWR path uses, so the
    process-global registry is the cross-instance mutex (no double build after a settings-
    save singleton rebuild). Bail if a live GET registered first; hard-cap a wedged build."""
    registry = TaskRegistry.get_instance()
    task = asyncio.create_task(coro)
    try:
        registry.register(name, task)
    except RuntimeError:
        task.cancel()  # a live on-visit build won the race - let it own it
        return
    try:
        await asyncio.wait_for(task, timeout=DISCOVER_WARMER_HARD_CAP)
    except asyncio.TimeoutError:
        logger.warning("Discover warmer build '%s' exceeded hard cap", name)
    except Exception as e:  # noqa: BLE001 - a build failure must not kill the loop
        logger.debug("Discover warmer build '%s' failed: %s", name, e)


async def _warm_one_user(
    uid: str,
    discover,
    home,
    last_warmed: dict,
    attempts: dict,
    queue_manager=None,
    workload_gate: "BackgroundWorkloadGate | None" = None,
) -> None:
    registry = TaskRegistry.get_instance()
    if registry.is_running(f"discover-homepage-warm-{uid}"):
        return  # a live user's build owns it
    logger.info("Discover warmer: warming %s", uid[:8])
    # Thorough discover build (relaxed section budgets during the LB outage so the rate-limited
    # MusicBrainz resolution actually completes and banks), registered under the SAME name the
    # on-visit SWR path uses so the two never double-run; then home (reads the discover cache).
    # _run_registered_warmer_build hard-caps it at DISCOVER_WARMER_HARD_CAP.
    if workload_gate is not None:
        await workload_gate.wait_until_available()
        if listenbrainz_rate_limit_cooldown_active():
            return
    await _run_registered_warmer_build(
        f"discover-homepage-warm-{uid}", discover.warm_cache_thorough(uid)
    )
    if workload_gate is not None:
        await workload_gate.wait_until_available()
        if listenbrainz_rate_limit_cooldown_active():
            return
    await _run_registered_warmer_build(f"home-warm-{uid}", home.warm_cache(uid))
    if queue_manager is not None:
        if workload_gate is not None:
            await workload_gate.wait_until_available()
            if listenbrainz_rate_limit_cooldown_active():
                return
        await queue_manager.start_build(uid)
        try:
            await asyncio.wait_for(
                queue_manager.wait_for_build(uid), timeout=DISCOVER_WARMER_HARD_CAP
            )
        except asyncio.TimeoutError:
            logger.warning("Discover queue warmer for %s exceeded hard cap", uid[:8])
    last_warmed[uid] = monotonic()
    has_cache, still_converging = await discover.peek_freshness(uid)
    # converged only when a real response is cached AND it isn't trending-only; a cut-at-cap
    # build (no cache) counts as an attempt so MAX_ATTEMPTS still bounds a hopeless user.
    converged = has_cache and not still_converging
    attempts[uid] = 0 if converged else attempts.get(uid, 0) + 1
    logger.info("Discover warmer: %s warmed (converged=%s)", uid[:8], converged)


async def warm_discover_home_periodically(
    get_discover_service,
    get_home_service,
    get_auth_store,
    get_queue_manager=None,
    interval: int = DISCOVER_WARMER_INTERVAL,
    workload_gate: "BackgroundWorkloadGate | None" = None,
) -> None:
    from core.config import get_settings

    logger.info(
        "Discover/Home warmer starting (delay %ss)", DISCOVER_WARMER_STARTUP_DELAY
    )
    await asyncio.sleep(DISCOVER_WARMER_STARTUP_DELAY)

    eligible: list[str] = []
    enumerated_at = 0.0
    last_warmed: dict[str, float] = {}
    attempts: dict[str, int] = {}

    while True:
        try:
            if workload_gate is not None:
                await workload_gate.wait_until_available()
            if not get_settings().discover_warmer_enabled:
                await asyncio.sleep(interval)
                continue
            now = monotonic()
            if not eligible or (now - enumerated_at) > DISCOVER_WARMER_ENUM_TTL:
                eligible = await _enumerate_warmer_users(get_auth_store())
                enumerated_at = now
                logger.info(
                    "Discover warmer: %d user(s) eligible",
                    len(eligible),
                )
            # We do NOT hard-skip when a user is "active" - the MusicBrainz priority queue
            # already yields background resolution to live USER_INITIATED requests, and the
            # per-user is_running check below avoids fighting a live build. A loop-level skip
            # here just stalled convergence for the very user watching the page.
            if eligible:
                uid = await _pick_due_warmer_user(
                    eligible, last_warmed, attempts, now, get_discover_service()
                )
                if uid is not None and not listenbrainz_rate_limit_cooldown_active():

                    async def warm_if_not_cooling_down() -> None:
                        if listenbrainz_rate_limit_cooldown_active():
                            return
                        await _warm_one_user(
                            uid,
                            get_discover_service(),
                            get_home_service(),
                            last_warmed,
                            attempts,
                            get_queue_manager() if get_queue_manager else None,
                            workload_gate,
                        )

                    if workload_gate is not None:
                        await workload_gate.run_warmer_unit(warm_if_not_cooling_down)
                    else:
                        await warm_if_not_cooling_down()
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Discover/Home warmer failed: %s", e, exc_info=True)

        await asyncio.sleep(interval)


def start_discover_home_warmer_task(
    get_discover_service,
    get_home_service,
    get_auth_store,
    get_queue_manager=None,
    workload_gate: "BackgroundWorkloadGate | None" = None,
) -> asyncio.Task:
    task = asyncio.create_task(
        warm_discover_home_periodically(
            get_discover_service,
            get_home_service,
            get_auth_store,
            get_queue_manager,
            workload_gate=workload_gate,
        )
    )
    task.add_done_callback(
        lambda t: logger.error("Discover/Home warmer task died: %s", t.exception())
        if not t.cancelled() and t.exception()
        else None
    )
    TaskRegistry.get_instance().register("discover-home-warmer", task)
    return task


_AUDIODB_SWEEP_INTERVAL = 86400
_AUDIODB_SWEEP_INITIAL_DELAY = 120
_AUDIODB_SWEEP_MAX_ITEMS = 5000
_AUDIODB_SWEEP_INTER_ITEM_DELAY = 2.0
_AUDIODB_SWEEP_CURSOR_PERSIST_INTERVAL = 50
_AUDIODB_SWEEP_LOG_INTERVAL = 100


async def warm_audiodb_cache_periodically(
    audiodb_image_service: "AudioDBImageService",
    library_db: "LibraryDB",
    preferences_service: "PreferencesService",
    precache_service: "LibraryPrecacheService | None" = None,
    workload_gate: "BackgroundWorkloadGate | None" = None,
) -> None:
    if precache_service is None:
        logger.warning(
            "AudioDB sweep: precache_service not available, byte downloads disabled"
        )
    await asyncio.sleep(_AUDIODB_SWEEP_INITIAL_DELAY)

    while True:
        try:
            await _run_audiodb_sweep_cycle(
                audiodb_image_service,
                library_db,
                preferences_service,
                precache_service,
                workload_gate,
            )
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("AudioDB sweep cycle failed: %s", e, exc_info=True)
        try:
            await asyncio.sleep(_AUDIODB_SWEEP_INTERVAL)
        except asyncio.CancelledError:
            break


async def _run_audiodb_sweep_cycle(
    audiodb_image_service: "AudioDBImageService",
    library_db: "LibraryDB",
    preferences_service: "PreferencesService",
    precache_service: "LibraryPrecacheService | None",
    workload_gate: "BackgroundWorkloadGate | None",
) -> None:
    if workload_gate is not None:
        await workload_gate.wait_until_available()

    settings = preferences_service.get_advanced_settings()
    if not settings.audiodb_enabled:
        return

    cursor = preferences_service.get_setting("audiodb_sweep_cursor")
    all_items = await library_db.get_enrichment_candidates(
        after_mbid=cursor,
        limit=_AUDIODB_SWEEP_MAX_ITEMS,
    )
    if not all_items:
        preferences_service.save_setting("audiodb_sweep_cursor", None)
        preferences_service.save_setting("audiodb_sweep_last_completed", time())
        return

    items_needing_refresh: list[tuple[str, str, dict]] = []
    inspected_cursor = cursor
    inspection_complete = True
    for entity_type, mbid, data in all_items:
        if workload_gate is not None and workload_gate.scan_active:
            inspection_complete = False
            break
        inspected_cursor = f"{entity_type}:{mbid}"
        if len(items_needing_refresh) >= _AUDIODB_SWEEP_MAX_ITEMS:
            break
        if entity_type == "artist":
            cached = await audiodb_image_service.get_cached_artist_images(mbid)
        else:
            cached = await audiodb_image_service.get_cached_album_images(mbid)
        if cached is None:
            items_needing_refresh.append((entity_type, mbid, data))

    if not items_needing_refresh:
        page_complete = inspection_complete and (
            len(all_items) < _AUDIODB_SWEEP_MAX_ITEMS
        )
        preferences_service.save_setting(
            "audiodb_sweep_cursor", None if page_complete else inspected_cursor
        )
        if page_complete:
            preferences_service.save_setting("audiodb_sweep_last_completed", time())
        return

    processed = 0
    processed_cursor = cursor
    bytes_ok = 0
    bytes_fail = 0
    for entity_type, mbid, data in items_needing_refresh:
        if workload_gate is not None and workload_gate.scan_active:
            break
        if not preferences_service.get_advanced_settings().audiodb_enabled:
            break

        try:
            if entity_type == "artist":
                name = data.get("name") if isinstance(data, dict) else None
                result = await audiodb_image_service.fetch_and_cache_artist_images(
                    mbid,
                    name,
                    is_monitored=True,
                )
                if (
                    result
                    and not result.is_negative
                    and result.thumb_url
                    and precache_service
                ):
                    if await precache_service._download_audiodb_bytes(
                        result.thumb_url, "artist", mbid
                    ):
                        bytes_ok += 1
                    else:
                        bytes_fail += 1
            else:
                artist_name = (
                    data.get("artist_name")
                    if isinstance(data, dict)
                    else getattr(data, "artist_name", None)
                )
                album_name = (
                    data.get("title")
                    if isinstance(data, dict)
                    else getattr(data, "title", None)
                )
                result = await audiodb_image_service.fetch_and_cache_album_images(
                    mbid,
                    artist_name=artist_name,
                    album_name=album_name,
                    is_monitored=True,
                )
                if (
                    result
                    and not result.is_negative
                    and result.album_thumb_url
                    and precache_service
                ):
                    if await precache_service._download_audiodb_bytes(
                        result.album_thumb_url, "album", mbid
                    ):
                        bytes_ok += 1
                    else:
                        bytes_fail += 1
        except Exception as e:
            logger.error(
                "audiodb.sweep action=item_error entity_type=%s mbid=%s error=%s",
                entity_type,
                mbid[:8],
                e,
                exc_info=True,
            )

        processed += 1
        processed_cursor = f"{entity_type}:{mbid}"
        if processed % _AUDIODB_SWEEP_CURSOR_PERSIST_INTERVAL == 0:
            preferences_service.save_setting("audiodb_sweep_cursor", processed_cursor)

        await asyncio.sleep(_AUDIODB_SWEEP_INTER_ITEM_DELAY)

    if processed >= len(items_needing_refresh) and inspection_complete:
        page_complete = len(all_items) < _AUDIODB_SWEEP_MAX_ITEMS
        preferences_service.save_setting(
            "audiodb_sweep_cursor", None if page_complete else inspected_cursor
        )
        if page_complete:
            preferences_service.save_setting("audiodb_sweep_last_completed", time())
    else:
        preferences_service.save_setting("audiodb_sweep_cursor", processed_cursor)


def start_audiodb_sweep_task(
    audiodb_image_service: "AudioDBImageService",
    library_db: "LibraryDB",
    preferences_service: "PreferencesService",
    precache_service: "LibraryPrecacheService | None" = None,
    workload_gate: "BackgroundWorkloadGate | None" = None,
) -> asyncio.Task:
    task = asyncio.create_task(
        warm_audiodb_cache_periodically(
            audiodb_image_service,
            library_db,
            preferences_service,
            precache_service=precache_service,
            workload_gate=workload_gate,
        )
    )
    TaskRegistry.get_instance().register("audiodb-sweep", task)
    return task


_REQUEST_SYNC_INTERVAL = 60
_REQUEST_SYNC_INITIAL_DELAY = 15


async def sync_request_statuses_periodically(
    requests_page_service: "RequestsPageService",
    interval: int = _REQUEST_SYNC_INTERVAL,
) -> None:
    await asyncio.sleep(_REQUEST_SYNC_INITIAL_DELAY)

    while True:
        try:
            await requests_page_service.sync_request_statuses()
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Periodic request status sync failed: %s", e, exc_info=True)

        await asyncio.sleep(interval)


def start_request_status_sync_task(
    requests_page_service: "RequestsPageService",
) -> asyncio.Task:
    task = asyncio.create_task(
        sync_request_statuses_periodically(requests_page_service)
    )
    TaskRegistry.get_instance().register("request-status-sync", task)
    return task


_DOWNLOAD_WATCHDOG_INTERVAL = 300
_DOWNLOAD_WATCHDOG_INITIAL_DELAY = 120


async def reap_stale_downloads_periodically(
    get_orchestrator, interval: int = _DOWNLOAD_WATCHDOG_INTERVAL
) -> None:
    """Age out download tasks whose poll loop died (crash/restart) so a request never
    sticks on 'downloading' forever. Complements the in-loop stall watchdog.

    Resolves the orchestrator fresh each sweep: saving download-client settings
    rebuilds the singleton, and we must sweep the CURRENT instance (whose
    ``_active_tasks`` owns the live downloads) so the 'skip a live loop' guard holds."""
    await asyncio.sleep(_DOWNLOAD_WATCHDOG_INITIAL_DELAY)
    while True:
        try:
            await get_orchestrator().reap_stale_tasks()
        except asyncio.CancelledError:
            break
        except Exception as e:  # noqa: BLE001
            logger.error("Download watchdog sweep failed: %s", e, exc_info=True)
        await asyncio.sleep(interval)


def start_download_watchdog_task(get_orchestrator) -> asyncio.Task:
    task = asyncio.create_task(reap_stale_downloads_periodically(get_orchestrator))
    TaskRegistry.get_instance().register("download-watchdog", task)
    return task


_DOWNLOAD_AUTO_RETRY_INTERVAL = 300
_DOWNLOAD_AUTO_RETRY_INITIAL_DELAY = 180


async def auto_retry_failed_downloads_periodically(
    get_orchestrator, interval: int = _DOWNLOAD_AUTO_RETRY_INTERVAL
) -> None:
    """Re-dispatch failed/partial downloads whose backoff has elapsed, giving the
    Soulseek network time to surface new sources. Mirrors the lidarr QueueCleaner
    pattern. Resolves the orchestrator fresh each sweep (same reason as the
    watchdog - a settings save rebuilds the singleton)."""
    await asyncio.sleep(_DOWNLOAD_AUTO_RETRY_INITIAL_DELAY)
    while True:
        try:
            await get_orchestrator().retry_failed_tasks()
        except asyncio.CancelledError:
            break
        except Exception as e:  # noqa: BLE001
            logger.error("Download auto-retry sweep failed: %s", e, exc_info=True)
        await asyncio.sleep(interval)


def start_download_auto_retry_task(get_orchestrator) -> asyncio.Task:
    task = asyncio.create_task(
        auto_retry_failed_downloads_periodically(get_orchestrator)
    )
    TaskRegistry.get_instance().register("download-auto-retry", task)
    return task


_MANAGEMENT_HOLD_AUTO_RETRY_INTERVAL = 60
_MANAGEMENT_HOLD_AUTO_RETRY_INITIAL_DELAY = 60


async def auto_retry_management_holds_periodically(
    get_download_service, interval: int = _MANAGEMENT_HOLD_AUTO_RETRY_INTERVAL
) -> None:
    await asyncio.sleep(_MANAGEMENT_HOLD_AUTO_RETRY_INITIAL_DELAY)
    while True:
        try:
            await get_download_service().retry_due_management_holds()
        except asyncio.CancelledError:
            break
        except Exception as error:  # noqa: BLE001 - durable retry loop survives one sweep
            logger.error("Organizer auto-retry sweep failed: %s", error, exc_info=True)
        try:
            await asyncio.sleep(interval)
        except asyncio.CancelledError:
            break


def start_management_hold_auto_retry_task(get_download_service) -> asyncio.Task:
    task = asyncio.create_task(
        auto_retry_management_holds_periodically(get_download_service)
    )
    TaskRegistry.get_instance().register("management-hold-auto-retry", task)
    return task


_WANTED_WATCHER_INTERVAL = 900
_WANTED_WATCHER_INITIAL_DELAY = 240


async def run_wanted_watcher_periodically(
    get_wanted_watcher, interval: int = _WANTED_WATCHER_INTERVAL
) -> None:
    """Sweep the wanted-watch registry: enrol availability-dead requests and
    re-search due watches (Wanted plan §5.3). Resolves the watcher fresh each
    sweep - the enabled toggle and the DownloadService it dispatches through
    are re-read/rebuilt without a restart."""
    await asyncio.sleep(_WANTED_WATCHER_INITIAL_DELAY)
    while True:
        try:
            await get_wanted_watcher().run_sweep()
        except asyncio.CancelledError:
            break
        except Exception as e:  # noqa: BLE001
            logger.error("Wanted watcher sweep failed: %s", e, exc_info=True)
        await asyncio.sleep(interval)


def start_wanted_watcher_task(get_wanted_watcher) -> asyncio.Task:
    task = asyncio.create_task(run_wanted_watcher_periodically(get_wanted_watcher))
    TaskRegistry.get_instance().register("wanted-watcher", task)
    return task


_FOLLOW_POLL_INTERVAL = 86400  # 24h, hardcoded for v1 (L2)
_FOLLOW_POLL_INITIAL_DELAY = 300


async def poll_followed_artists_new_releases(
    new_release_service: "NewReleaseService",
    interval: int = _FOLLOW_POLL_INTERVAL,
) -> None:
    """Detect new releases for followed artists and auto-enqueue for approved
    followers. Sleep-at-end so a slow run never overlaps the next tick; a failure
    backs off and the loop only exits on shutdown (DD6)."""
    await asyncio.sleep(_FOLLOW_POLL_INITIAL_DELAY)

    while True:
        try:
            await new_release_service.run_poll()
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Follow new-release poll failed: %s", e, exc_info=True)

        await asyncio.sleep(interval)


def start_poll_new_releases_task(
    new_release_service: "NewReleaseService",
) -> asyncio.Task:
    task = asyncio.create_task(poll_followed_artists_new_releases(new_release_service))
    TaskRegistry.get_instance().register("follow-new-release-poll", task)
    return task


_EVENTS_WATCHER_INITIAL_DELAY = 420
_EVENTS_SCHEDULER_TICK = 60
# the startup catch-up skips artists checked within this window so restarts
# (every deploy on this host) don't re-spend the day's provider quota
_EVENTS_CATCHUP_SKIP_RECENT_HOURS = 20.0


def _next_daily_occurrence(hhmm: str, after: datetime) -> datetime:
    """First server-local datetime with wall time ``hhmm`` strictly after
    ``after``. Tolerates garbage (schema validates at the API boundary) by
    falling back to 06:00."""
    try:
        hour, minute = (int(part) for part in hhmm.split(":"))
        if not (0 <= hour <= 23 and 0 <= minute <= 59):
            raise ValueError(hhmm)
    except (ValueError, AttributeError):
        hour, minute = 6, 0
    candidate = after.replace(hour=hour, minute=minute, second=0, microsecond=0)
    if candidate <= after:
        candidate += timedelta(days=1)
    return candidate


async def run_events_watcher_periodically(get_events_watcher, get_poll_time) -> None:
    """Sweep live-event sources for every distinct followed artist
    (.dev-notes/Events) daily at the admin-configured server-local time.

    Scheduler-tick model (the auto-scan pattern): one catch-up sweep shortly
    after startup, then a sweep whenever the next occurrence of ``poll_time``
    after the previous sweep has passed. The watcher AND the time are
    re-resolved every tick, so a settings save takes effect within a minute
    without a restart. Exactly one sleep per iteration incl. the error path;
    the loop only exits on shutdown."""
    await asyncio.sleep(_EVENTS_WATCHER_INITIAL_DELAY)
    last_sweep: datetime | None = None
    while True:
        try:
            now = datetime.now()
            if last_sweep is None:
                # catch-up after restart: only artists not swept recently
                await get_events_watcher().run_sweep(
                    skip_recent_hours=_EVENTS_CATCHUP_SKIP_RECENT_HOURS
                )
                last_sweep = now
            elif now >= _next_daily_occurrence(get_poll_time(), last_sweep):
                await get_events_watcher().run_sweep()
                last_sweep = now
        except asyncio.CancelledError:
            break
        except Exception as e:  # noqa: BLE001
            logger.error("Events watcher sweep failed: %s", e, exc_info=True)
            last_sweep = datetime.now()  # a failing sweep still waits for the next slot
        await asyncio.sleep(_EVENTS_SCHEDULER_TICK)


def start_events_watcher_task(get_events_watcher, get_poll_time) -> asyncio.Task:
    task = asyncio.create_task(
        run_events_watcher_periodically(get_events_watcher, get_poll_time)
    )
    TaskRegistry.get_instance().register("events-watcher", task)
    return task


_events_kick_task: asyncio.Task | None = None


def _log_events_kick_error(task: asyncio.Task) -> None:
    if not task.cancelled() and task.exception() is not None:
        logger.error(
            "Kicked events sweep failed: %s",
            task.exception(),
            exc_info=task.exception(),
        )


def kick_events_sweep(get_events_watcher) -> asyncio.Task | None:
    """One-off sweep right after an events settings save, so enabling the
    feature takes effect immediately instead of waiting out the periodic
    loop's sleep (which may be a day away). The sweep itself no-ops when no
    source is ready, so kicking on every save is safe; a kick while a prior
    kicked sweep is still running is skipped (returns None). A rare overlap
    with the periodic loop is harmless - store writes are serialized and the
    diff/upsert is idempotent."""
    global _events_kick_task
    if _events_kick_task is not None and not _events_kick_task.done():
        return None
    task = asyncio.create_task(get_events_watcher().run_sweep())
    task.add_done_callback(_log_events_kick_error)
    _events_kick_task = task
    return task


_PERSONAL_MIX_REFRESH_INTERVAL = 86400
_PERSONAL_MIX_INITIAL_DELAY = 300


async def refresh_personal_mixes_periodically(
    personal_mix_service: "PersonalMixService",
    interval: int = _PERSONAL_MIX_REFRESH_INTERVAL,
) -> None:
    await asyncio.sleep(_PERSONAL_MIX_INITIAL_DELAY)

    while True:
        try:
            await personal_mix_service.run_for_all_users()
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Personal mix refresh failed: %s", e, exc_info=True)

        await asyncio.sleep(interval)


def start_personal_mix_refresh_task(
    personal_mix_service: "PersonalMixService",
) -> asyncio.Task:
    task = asyncio.create_task(
        refresh_personal_mixes_periodically(personal_mix_service)
    )
    TaskRegistry.get_instance().register("personal-mix-refresh", task)
    return task


async def demote_orphaned_covers_periodically(
    cover_disk_cache: "CoverDiskCache",
    library_db: "LibraryDB",
    interval: int = 86400,
) -> None:
    from repositories.coverart_disk_cache import get_cache_filename

    await asyncio.sleep(300)
    while True:
        try:
            album_mbids = await library_db.get_all_album_mbids()
            artist_mbids = await library_db.get_all_artist_mbids()

            valid_hashes: set[str] = set()
            for mbid in album_mbids:
                for suffix in ("500", "250", "1200", "orig"):
                    valid_hashes.add(get_cache_filename(f"rg_{mbid}", suffix))
            for mbid in artist_mbids:
                for size in ("250", "500"):
                    valid_hashes.add(get_cache_filename(f"artist_{mbid}_{size}", "img"))
                valid_hashes.add(get_cache_filename(f"artist_{mbid}", "img"))

            await asyncio.to_thread(cover_disk_cache.demote_orphaned, valid_hashes)
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Orphan cover demotion failed: %s", e, exc_info=True)

        await asyncio.sleep(interval)


def start_orphan_cover_demotion_task(
    cover_disk_cache: "CoverDiskCache",
    library_db: "LibraryDB",
    interval: int = 86400,
) -> asyncio.Task:
    task = asyncio.create_task(
        demote_orphaned_covers_periodically(
            cover_disk_cache, library_db, interval=interval
        )
    )
    TaskRegistry.get_instance().register("orphan-cover-demotion", task)
    return task


async def prune_stores_periodically(
    request_history: "RequestHistoryStore",
    mbid_store: "MBIDStore",
    youtube_store: "YouTubeStore",
    request_retention_days: int = 180,
    ignored_retention_days: int = 365,
    interval: int = 21600,
    wanted_store: "WantedStore | None" = None,
    native_store: "NativeLibraryStore | None" = None,
) -> None:
    await asyncio.sleep(600)
    while True:
        try:
            await request_history.prune_old_terminal_requests(request_retention_days)
            await mbid_store.prune_old_ignored_releases(ignored_retention_days)
            await youtube_store.delete_orphaned_track_links()
            if wanted_store is not None:
                # terminal (stopped/fulfilled) watches age out on the same window
                # as requests; orphaned seen-candidate rows go with them (§5.1)
                await wanted_store.prune(request_retention_days)
            if native_store is not None:
                # F-PERF-04: bounded 30-day retention for terminal automatic
                # identification jobs (signed LibraryAudit decision); one batch
                # per pass, continuation handled by the next interval.
                await native_store.prune_old_terminal_identification_jobs(
                    now=time()
                )
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Store prune task failed: %s", e, exc_info=True)

        await asyncio.sleep(interval)


def start_store_prune_task(
    request_history: "RequestHistoryStore",
    mbid_store: "MBIDStore",
    youtube_store: "YouTubeStore",
    request_retention_days: int = 180,
    ignored_retention_days: int = 365,
    interval: int = 21600,
    wanted_store: "WantedStore | None" = None,
    native_store: "NativeLibraryStore | None" = None,
) -> asyncio.Task:
    task = asyncio.create_task(
        prune_stores_periodically(
            request_history,
            mbid_store,
            youtube_store,
            request_retention_days=request_retention_days,
            ignored_retention_days=ignored_retention_days,
            interval=interval,
            wanted_store=wanted_store,
            native_store=native_store,
        )
    )
    TaskRegistry.get_instance().register("store-prune", task)
    return task


async def prune_recycle_bin_periodically(
    preferences_service: PreferencesService,
    interval: int = 21600,
) -> None:
    """Remove upgrade-recycled files past ``recycle_retention_days`` (D19). Policy
    and bin path are re-read every pass so settings changes apply without restart."""
    from services.native.recycle_bin import prune, resolve_bin_path

    await asyncio.sleep(600)
    while True:
        try:
            library = preferences_service.get_typed_library_settings()
            if library.enabled:
                policy = preferences_service.get_download_policy()
                bin_path = resolve_bin_path(
                    policy.recycle_bin_path,
                    [root.path for root in library.library_roots],
                )
                if bin_path is not None:
                    removed = await asyncio.to_thread(
                        prune, bin_path, policy.recycle_retention_days
                    )
                    if removed:
                        logger.info("Recycle bin prune removed %d entries", removed)
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Recycle bin prune failed: %s", e, exc_info=True)

        await asyncio.sleep(interval)


def start_recycle_bin_prune_task(
    preferences_service: PreferencesService,
    interval: int = 21600,
) -> asyncio.Task:
    task = asyncio.create_task(
        prune_recycle_bin_periodically(preferences_service, interval=interval)
    )
    TaskRegistry.get_instance().register("recycle-bin-prune", task)
    return task


async def run_background_upgrade_sweep(
    download_service,
    auth_store,
    policy,  # noqa: ANN001
) -> int:
    """One background-upgrade pass (CollectionManagement Phase 5): walk the
    cutoff-unmet worklist and enqueue at most ``background_upgrade_max_per_run``
    origin='upgrade' grabs, owned by the oldest admin (upgrades are a curator
    action, D18 - the scan acts as that curator). Returns how many were enqueued.
    Dedup/gating live in ``request_upgrade_album`` (active-task dedup + the
    origin-aware gate), so a sweep can never double-grab."""
    admins = [u for u in await auth_store.list_users(limit=500) if u.role == "admin"]
    if not admins:
        logger.info("Background upgrade sweep skipped: no admin user to own the tasks")
        return 0
    owner = admins[0]
    items = await download_service.list_cutoff_unmet()
    enqueued = 0
    for item in items:
        if enqueued >= policy.background_upgrade_max_per_run:
            break
        task_id = await download_service.request_upgrade_album(
            user_id=owner.id,
            release_group_mbid=item["release_group_mbid"],
            artist_name=item.get("artist_name") or "Unknown",
            album_title=item.get("album_title") or "Unknown",
            year=item.get("year"),
            artist_mbid=item.get("artist_mbid"),
        )
        if task_id != "already_in_library":
            enqueued += 1
    if enqueued:
        logger.info("Background upgrade sweep enqueued %d upgrade(s)", enqueued)
    return enqueued


async def scan_for_upgrades_periodically(
    get_download_service,  # noqa: ANN001 - resolved fresh (settings saves rebuild it)
    auth_store,  # noqa: ANN001 - AuthStore
    preferences_service: PreferencesService,
) -> None:
    """Opt-in, throttled background upgrade scan (default OFF). Both toggles are
    re-read every pass so enabling it needs no restart; it runs only while
    ``upgrade_allowed`` AND ``background_upgrade_scan_enabled`` are on."""
    await asyncio.sleep(900)
    while True:
        interval_hours = 12
        try:
            policy = preferences_service.get_download_policy()
            interval_hours = policy.background_upgrade_scan_interval_hours
            if policy.upgrade_allowed and policy.background_upgrade_scan_enabled:
                await run_background_upgrade_sweep(
                    get_download_service(), auth_store, policy
                )
        except asyncio.CancelledError:
            break
        except Exception as e:  # noqa: BLE001
            logger.error("Background upgrade sweep failed: %s", e, exc_info=True)
        await asyncio.sleep(interval_hours * 3600)


def start_background_upgrade_scan_task(
    get_download_service,  # noqa: ANN001
    auth_store,  # noqa: ANN001
    preferences_service: PreferencesService,
) -> asyncio.Task:
    task = asyncio.create_task(
        scan_for_upgrades_periodically(
            get_download_service, auth_store, preferences_service
        )
    )
    TaskRegistry.get_instance().register("background-upgrade-scan", task)
    return task
