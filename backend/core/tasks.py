import asyncio
import logging
from datetime import datetime, timedelta
from time import time
from typing import TYPE_CHECKING, Optional
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.disk_cache import DiskMetadataCache
from infrastructure.memory import get_rss_bytes, trim_malloc
from infrastructure.serialization import clone_with_updates
from infrastructure.validators import is_unknown_mbid
from services.library_service import LibraryService
from services.preferences_service import PreferencesService
from core.task_registry import TaskRegistry

if TYPE_CHECKING:
    from pathlib import Path
    from services.album_service import AlbumService
    from services.native.library_scanner import LibraryScanner
    from services.native.download_orchestrator import DownloadOrchestrator
    from infrastructure.persistence.scan_state_store import ScanStateStore
    from services.audiodb_image_service import AudioDBImageService
    from services.home_service import HomeService
    from services.artist_discovery_service import ArtistDiscoveryService
    from services.library_precache_service import LibraryPrecacheService
    from infrastructure.persistence import LibraryDB
    from infrastructure.persistence.request_history import RequestHistoryStore
    from infrastructure.persistence.mbid_store import MBIDStore
    from infrastructure.persistence.youtube_store import YouTubeStore
    from services.requests_page_service import RequestsPageService
    from services.native.new_release_service import NewReleaseService
    from repositories.coverart_disk_cache import CoverDiskCache

logger = logging.getLogger(__name__)


async def cleanup_cache_periodically(cache: CacheInterface, interval: int = 300) -> None:
    while True:
        try:
            await asyncio.sleep(interval)
            await cache.cleanup_expired()
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Cache cleanup task failed: %s", e, exc_info=True)


def start_cache_cleanup_task(cache: CacheInterface, interval: int = 300) -> asyncio.Task:
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
    task = asyncio.create_task(memory_maintenance_periodically(cache, interval=interval))
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
        cleanup_disk_cache_periodically(disk_cache, interval=interval, cover_disk_cache=cover_disk_cache)
    )
    TaskRegistry.get_instance().register("disk-cache-cleanup", task)
    return task


_SCAN_FREQ_TO_SECONDS = {
    "5min": 300,
    "10min": 600,
    "30min": 1800,
    "1hr": 3600,
    "6hr": 21600,
    "12hr": 43200,
    "24hr": 86400,
    "3d": 259200,
    "7d": 604800,
}

# Longest the scheduler sleeps in one go. Long waits are chopped into ticks so a
# changed schedule (or a finished in-progress scan) is noticed within this window;
# the final sub-tick sleep still lands exactly on the due moment.
_SCHEDULER_TICK = 300


def _parse_daily_time(value: str) -> tuple[int, int]:
    """(hour, minute) from an "HH:MM" string, defaulting to 03:00 on anything odd."""
    try:
        hh, mm = value.split(":")
        hour, minute = int(hh), int(mm)
        if 0 <= hour <= 23 and 0 <= minute <= 59:
            return hour, minute
    except (ValueError, AttributeError):
        pass
    return 3, 0


def _seconds_until_next_scan(
    freq: str,
    daily_scan_time: str,
    last_scan_ts: float | None,
    now: datetime,
) -> float:
    """Seconds to wait before the next scan is due. 0 means "overdue, run now" - which
    is what lets a restart catch up an overdue scan instead of resetting the clock.

    "daily" fires once at daily_scan_time each day (catching up if that moment already
    passed today without a scan); the interval values fire on a rolling gap measured
    from the last actual scan."""
    if freq == "daily":
        hour, minute = _parse_daily_time(daily_scan_time)
        today_at = now.replace(hour=hour, minute=minute, second=0, microsecond=0)
        if now < today_at:
            return (today_at - now).total_seconds()
        already_scanned_today = (
            last_scan_ts is not None and last_scan_ts >= today_at.timestamp()
        )
        if already_scanned_today:
            return (today_at + timedelta(days=1) - now).total_seconds()
        return 0.0

    interval = _SCAN_FREQ_TO_SECONDS.get(freq, 86400)
    if last_scan_ts is None:
        return 0.0
    return max(0.0, (last_scan_ts + interval) - now.timestamp())


async def auto_scan_library_periodically(
    scanner: "LibraryScanner",
    scan_state: "ScanStateStore",
    preferences_service: PreferencesService,
) -> None:
    """Incremental native scan on the configured schedule. The next run is computed
    from the last actual scan (so a restart catches up an overdue scan instead of
    restarting the interval); a tick is skipped while a scan is already running, and a
    failure never kills the loop."""
    from pathlib import Path as _Path

    logger.info("Auto-scan scheduler started")
    while True:
        try:
            schedule = preferences_service.get_library_scan_schedule()
            freq = schedule.scan_frequency
            if freq == "manual":
                await asyncio.sleep(_SCHEDULER_TICK)
                continue

            state = await scan_state.get_state()
            if state.get("status") == "scanning":
                await asyncio.sleep(_SCHEDULER_TICK)
                continue

            delay = _seconds_until_next_scan(
                freq, schedule.daily_scan_time, state.get("started_at"), datetime.now()
            )
            if delay > 0:
                await asyncio.sleep(min(delay, _SCHEDULER_TICK))
                continue

            paths = [
                _Path(p)
                for p in preferences_service.get_library_settings_raw().library_paths
            ]
            if not paths:
                await asyncio.sleep(_SCHEDULER_TICK)
                continue

            logger.info("Auto-scan starting (schedule=%s)", freq)
            success = True
            try:
                await scanner.scan(paths)
                final = await scan_state.get_state()
                success = final.get("status") != "error"
            except Exception as e:
                logger.error("Auto-scan failed: %s", e, exc_info=True)
                success = False

            schedule = preferences_service.get_library_scan_schedule()
            preferences_service.save_library_scan_schedule(
                clone_with_updates(
                    schedule, {"last_scan": int(time()), "last_scan_success": success}
                )
            )
            logger.info("Auto-scan finished (success=%s)", success)
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Auto-scan task failed: %s", e, exc_info=True)
            await asyncio.sleep(60)


def start_library_auto_scan_task(
    scanner: "LibraryScanner",
    scan_state: "ScanStateStore",
    preferences_service: PreferencesService,
) -> asyncio.Task:
    task = asyncio.create_task(
        auto_scan_library_periodically(scanner, scan_state, preferences_service)
    )
    TaskRegistry.get_instance().register("library-auto-scan", task)
    return task


def start_library_scan_resume_task(
    scanner: "LibraryScanner",
    library_paths: "list[Path]",
) -> asyncio.Task:
    """(AUD-3) Resume an interrupted native library scan on startup. Registered so
    it is cancelled at shutdown like every other long-lived task."""

    async def _resume() -> None:
        try:
            await scanner.startup_check(library_paths)
        except Exception as e:  # noqa: BLE001
            logger.error("Library scan resume failed: %s", e, exc_info=True)

    task = asyncio.create_task(_resume())
    TaskRegistry.get_instance().register("library-scan-resume", task)
    task.add_done_callback(
        lambda t: logger.error("Library scan resume task error: %s", t.exception())
        if not t.cancelled() and t.exception()
        else None
    )
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


async def warm_library_cache(
    library_service: LibraryService,
    album_service: 'AlbumService',
    library_db: 'LibraryDB'
) -> None:
    try:
        await asyncio.sleep(5)
        
        albums_data = await library_db.get_albums()
        
        if not albums_data:
            return

        max_warm = 30
        albums_to_warm = albums_data[:max_warm]
        
        warmed = 0
        for i, album_data in enumerate(albums_to_warm):
            mbid = album_data.get('mbid')
            if mbid and not is_unknown_mbid(mbid):
                try:
                    if not await album_service.is_album_cached(mbid):
                        await album_service.get_album_info(mbid)
                        warmed += 1

                    if i % 5 == 0:
                        await asyncio.sleep(1)

                except Exception as e:
                    logger.error(
                        "Library cache warm item failed album=%s mbid=%s error=%s",
                        album_data.get('title'),
                        mbid,
                        e,
                        exc_info=True,
                    )
                    continue
        
    except Exception as e:
        logger.error("Library cache warming failed: %s", e, exc_info=True)


async def warm_genre_cache_periodically(
    home_service: 'HomeService',
    interval: int = 21600,
) -> None:
    # Phase 5: Home/Discover are per-user, so there's no global home cache to read
    # genre names from; derive top genres account-less from the shared library (D2).
    RETRY_INTERVAL = 60

    await asyncio.sleep(30)

    while True:
        warmed = 0
        try:
            genre_names = await home_service.get_library_genre_names()
            if genre_names:
                for src in ("listenbrainz", "lastfm"):
                    try:
                        await home_service._genre.build_and_cache_genre_section(src, genre_names)
                        warmed += 1
                    except Exception as e:
                        logger.error(
                            "Genre cache warming failed (source=%s): %s",
                            src,
                            e,
                            exc_info=True,
                        )
        except asyncio.CancelledError:
            break
        except Exception as e:  # noqa: BLE001
            logger.error("Genre cache warming failed: %s", e, exc_info=True)

        if warmed == 0:
            await asyncio.sleep(RETRY_INTERVAL)
        else:
            try:
                ttl = home_service._genre._get_genre_section_ttl()
            except Exception:  # noqa: BLE001
                ttl = interval
            await asyncio.sleep(ttl)


def start_genre_cache_warming_task(home_service: 'HomeService') -> asyncio.Task:
    task = asyncio.create_task(warm_genre_cache_periodically(home_service))
    TaskRegistry.get_instance().register("genre-cache-warming", task)
    return task


async def warm_jellyfin_mbid_index(jellyfin_repo: 'JellyfinRepository') -> None:

    await asyncio.sleep(8)
    try:
        await jellyfin_repo.build_mbid_index()
    except Exception as e:
        logger.error("Jellyfin MBID index warming failed: %s", e, exc_info=True)


async def warm_navidrome_mbid_cache() -> None:
    from core.dependencies import get_navidrome_library_service

    await asyncio.sleep(12)
    while True:
        try:
            service = get_navidrome_library_service()
            await service.warm_mbid_cache()
        except Exception as e:
            logger.error("Navidrome MBID cache warming failed: %s", e, exc_info=True)
        await asyncio.sleep(14400)


async def warm_plex_mbid_cache() -> None:
    from core.dependencies import get_plex_library_service

    await asyncio.sleep(15)
    while True:
        try:
            service = get_plex_library_service()
            await service.warm_mbid_cache()
            await service.persist_if_dirty()
        except Exception as e:
            logger.error("Plex MBID cache warming failed: %s", e, exc_info=True)
        await asyncio.sleep(14400)


async def warm_artist_discovery_cache_periodically(
    artist_discovery_service: 'ArtistDiscoveryService',
    library_db: 'LibraryDB',
    interval: int = 14400,
    delay: float = 0.5,
) -> None:
    await asyncio.sleep(300)  # Allow initial library sync to complete before warming caches

    while True:
        try:
            artists = await library_db.get_artists()
            if not artists:
                await asyncio.sleep(interval)
                continue

            mbids = [
                a['mbid'] for a in artists
                if a.get('mbid') and not is_unknown_mbid(a['mbid'])
            ]
            if not mbids:
                await asyncio.sleep(interval)
                continue

            await artist_discovery_service.precache_artist_discovery(
                mbids, delay=delay
            )
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Artist discovery cache warming failed: %s", e, exc_info=True)

        await asyncio.sleep(interval)


def start_artist_discovery_cache_warming_task(
    artist_discovery_service: 'ArtistDiscoveryService',
    library_db: 'LibraryDB',
    interval: int = 14400,
    delay: float = 0.5,
) -> asyncio.Task:
    task = asyncio.create_task(
        warm_artist_discovery_cache_periodically(
            artist_discovery_service,
            library_db,
            interval=interval,
            delay=delay,
        )
    )
    TaskRegistry.get_instance().register("artist-discovery-warming", task)
    return task


_AUDIODB_SWEEP_INTERVAL = 86400
_AUDIODB_SWEEP_INITIAL_DELAY = 120
_AUDIODB_SWEEP_MAX_ITEMS = 5000
_AUDIODB_SWEEP_INTER_ITEM_DELAY = 2.0
_AUDIODB_SWEEP_CURSOR_PERSIST_INTERVAL = 50
_AUDIODB_SWEEP_LOG_INTERVAL = 100


async def warm_audiodb_cache_periodically(
    audiodb_image_service: 'AudioDBImageService',
    library_db: 'LibraryDB',
    preferences_service: 'PreferencesService',
    precache_service: 'LibraryPrecacheService | None' = None,
) -> None:
    if precache_service is None:
        logger.warning("AudioDB sweep: precache_service not available, byte downloads disabled")
    await asyncio.sleep(_AUDIODB_SWEEP_INITIAL_DELAY)

    while True:
        try:
            await asyncio.sleep(_AUDIODB_SWEEP_INTERVAL)

            settings = preferences_service.get_advanced_settings()
            if not settings.audiodb_enabled:
                continue

            artists = await library_db.get_artists()
            albums = await library_db.get_albums()
            if not artists and not albums:
                continue

            cursor = preferences_service.get_setting('audiodb_sweep_cursor')
            all_items: list[tuple[str, str, dict]] = []

            for a in (artists or []):
                mbid = a.get('mbid')
                if mbid and not is_unknown_mbid(mbid):
                    all_items.append(("artist", mbid, a))
            for a in (albums or []):
                mbid = a.get('mbid') if isinstance(a, dict) else getattr(a, 'musicbrainz_id', None)
                if mbid and not is_unknown_mbid(mbid):
                    all_items.append(("album", mbid, a))

            all_items.sort(key=lambda x: x[1])

            if cursor:
                start_idx = 0
                for i, (_, mbid, _) in enumerate(all_items):
                    if mbid > cursor:
                        start_idx = i
                        break
                else:
                    start_idx = 0
                    cursor = None
                all_items = all_items[start_idx:]

            items_needing_refresh: list[tuple[str, str, dict]] = []
            for entity_type, mbid, data in all_items:
                if len(items_needing_refresh) >= _AUDIODB_SWEEP_MAX_ITEMS:
                    break
                if entity_type == "artist":
                    cached = await audiodb_image_service.get_cached_artist_images(mbid)
                else:
                    cached = await audiodb_image_service.get_cached_album_images(mbid)
                if cached is None:
                    items_needing_refresh.append((entity_type, mbid, data))

            if not items_needing_refresh:
                preferences_service.save_setting('audiodb_sweep_cursor', None)
                preferences_service.save_setting('audiodb_sweep_last_completed', time())
                continue

            processed = 0
            bytes_ok = 0
            bytes_fail = 0
            for entity_type, mbid, data in items_needing_refresh:
                if not preferences_service.get_advanced_settings().audiodb_enabled:
                    break

                try:
                    if entity_type == "artist":
                        name = data.get('name') if isinstance(data, dict) else None
                        result = await audiodb_image_service.fetch_and_cache_artist_images(
                            mbid, name, is_monitored=True,
                        )
                        if result and not result.is_negative and result.thumb_url and precache_service:
                            if await precache_service._download_audiodb_bytes(result.thumb_url, "artist", mbid):
                                bytes_ok += 1
                            else:
                                bytes_fail += 1
                    else:
                        artist_name = data.get('artist_name') if isinstance(data, dict) else getattr(data, 'artist_name', None)
                        album_name = data.get('title') if isinstance(data, dict) else getattr(data, 'title', None)
                        result = await audiodb_image_service.fetch_and_cache_album_images(
                            mbid, artist_name=artist_name,
                            album_name=album_name, is_monitored=True,
                        )
                        if result and not result.is_negative and result.album_thumb_url and precache_service:
                            if await precache_service._download_audiodb_bytes(result.album_thumb_url, "album", mbid):
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
                if processed % _AUDIODB_SWEEP_CURSOR_PERSIST_INTERVAL == 0:
                    preferences_service.save_setting('audiodb_sweep_cursor', mbid)

                await asyncio.sleep(_AUDIODB_SWEEP_INTER_ITEM_DELAY)

            if processed >= len(items_needing_refresh):
                preferences_service.save_setting('audiodb_sweep_cursor', None)
                preferences_service.save_setting('audiodb_sweep_last_completed', time())
            else:
                preferences_service.save_setting('audiodb_sweep_cursor', mbid)

        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("AudioDB sweep cycle failed: %s", e, exc_info=True)


def start_audiodb_sweep_task(
    audiodb_image_service: 'AudioDBImageService',
    library_db: 'LibraryDB',
    preferences_service: 'PreferencesService',
    precache_service: 'LibraryPrecacheService | None' = None,
) -> asyncio.Task:
    task = asyncio.create_task(
        warm_audiodb_cache_periodically(
            audiodb_image_service, library_db, preferences_service,
            precache_service=precache_service,
        )
    )
    TaskRegistry.get_instance().register("audiodb-sweep", task)
    return task


_REQUEST_SYNC_INTERVAL = 60
_REQUEST_SYNC_INITIAL_DELAY = 15


async def sync_request_statuses_periodically(
    requests_page_service: 'RequestsPageService',
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
    requests_page_service: 'RequestsPageService',
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


_FOLLOW_POLL_INTERVAL = 86400  # 24h, hardcoded for v1 (L2)
_FOLLOW_POLL_INITIAL_DELAY = 300


async def poll_followed_artists_new_releases(
    new_release_service: 'NewReleaseService',
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
    new_release_service: 'NewReleaseService',
) -> asyncio.Task:
    task = asyncio.create_task(
        poll_followed_artists_new_releases(new_release_service)
    )
    TaskRegistry.get_instance().register("follow-new-release-poll", task)
    return task


async def demote_orphaned_covers_periodically(
    cover_disk_cache: 'CoverDiskCache',
    library_db: 'LibraryDB',
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
    cover_disk_cache: 'CoverDiskCache',
    library_db: 'LibraryDB',
    interval: int = 86400,
) -> asyncio.Task:
    task = asyncio.create_task(
        demote_orphaned_covers_periodically(cover_disk_cache, library_db, interval=interval)
    )
    TaskRegistry.get_instance().register("orphan-cover-demotion", task)
    return task


async def prune_stores_periodically(
    request_history: 'RequestHistoryStore',
    mbid_store: 'MBIDStore',
    youtube_store: 'YouTubeStore',
    request_retention_days: int = 180,
    ignored_retention_days: int = 365,
    interval: int = 21600,
) -> None:
    await asyncio.sleep(600)
    while True:
        try:
            await request_history.prune_old_terminal_requests(request_retention_days)
            await mbid_store.prune_old_ignored_releases(ignored_retention_days)
            await youtube_store.delete_orphaned_track_links()
        except asyncio.CancelledError:
            break
        except Exception as e:
            logger.error("Store prune task failed: %s", e, exc_info=True)

        await asyncio.sleep(interval)


def start_store_prune_task(
    request_history: 'RequestHistoryStore',
    mbid_store: 'MBIDStore',
    youtube_store: 'YouTubeStore',
    request_retention_days: int = 180,
    ignored_retention_days: int = 365,
    interval: int = 21600,
) -> asyncio.Task:
    task = asyncio.create_task(
        prune_stores_periodically(
            request_history, mbid_store, youtube_store,
            request_retention_days=request_retention_days,
            ignored_retention_days=ignored_retention_days,
            interval=interval,
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
            policy = preferences_service.get_download_policy()
            library = preferences_service.get_library_settings()
            bin_path = resolve_bin_path(policy.recycle_bin_path, library.library_paths)
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
    download_service, auth_store, policy  # noqa: ANN001
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
        scan_for_upgrades_periodically(get_download_service, auth_store, preferences_service)
    )
    TaskRegistry.get_instance().register("background-upgrade-scan", task)
    return task
