"""Tier 2 - Cache layer, persistence stores, and foundation providers."""

from __future__ import annotations

import asyncio

from core.config import get_settings
from infrastructure.cache.cache_metrics import InstrumentedCache
from infrastructure.cache.memory_cache import InMemoryCache, CacheInterface
from infrastructure.cache.disk_cache import DiskMetadataCache
from infrastructure.cache.cache_keys import (
    catalog_list_prefixes,
    home_prefixes,
    library_identification_prefixes,
    ARTIST_DISCOVERY_PREFIX,
    DISCOVER_QUEUE_ENRICH_PREFIX,
)
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence import (
    LibraryDB,
    GenreIndex,
    DiscoverySnapshotStore,
    YouTubeStore,
    MBIDStore,
    NativeLibraryStore,
    SyncStateStore,
)
from infrastructure.persistence.mb_canonical_store import MbCanonicalStore
from infrastructure.persistence._database import PriorityWriteLock

from ._registry import singleton


@singleton
def get_cache() -> CacheInterface:
    preferences_service = get_preferences_service()
    advanced = preferences_service.get_advanced_settings()
    max_entries = advanced.memory_cache_max_entries
    # QW9 Part 2: single injection point - every CacheInterface consumer gets
    # the instrumented singleton, so hit/miss/set/delete recording is fleet-wide.
    return InstrumentedCache(InMemoryCache(max_entries=max_entries))


@singleton
def get_disk_cache() -> DiskMetadataCache:
    settings = get_settings()
    preferences_service = get_preferences_service()
    advanced = preferences_service.get_advanced_settings()
    cache_dir = settings.cache_dir / "metadata"
    return DiskMetadataCache(
        base_path=cache_dir,
        recent_metadata_max_size_mb=advanced.recent_metadata_max_size_mb,
        recent_covers_max_size_mb=advanced.recent_covers_max_size_mb,
        persistent_metadata_ttl_hours=advanced.persistent_metadata_ttl_hours,
    )


@singleton
def get_persistence_write_lock() -> PriorityWriteLock:
    return PriorityWriteLock(foreground_burst=8)


@singleton
def get_library_db() -> LibraryDB:
    settings = get_settings()
    lock = get_persistence_write_lock()
    return LibraryDB(db_path=settings.library_db_path, write_lock=lock)


@singleton
def get_native_library_store() -> NativeLibraryStore:
    from core.dependencies.auth_providers import get_auth_store

    settings = get_settings()
    get_auth_store()

    async def invalidate() -> None:
        from services.search_service import SearchService

        SearchService.clear_cached_results()
        cache = get_cache()
        # ST1 phase 1: context-free commits (favorites, play history, ...) no
        # longer destroy MB-derived entity caches - only cheap locally-rebuilt
        # lists sweep here. Phase 2 may thread affected entity ids.
        await asyncio.gather(
            *(cache.clear_prefix(prefix) for prefix in catalog_list_prefixes())
        )
        await get_discovery_snapshot_store().mark_discover_stale()

    async def invalidate_scan_batch() -> None:
        from services.search_service import SearchService

        SearchService.clear_cached_results()
        # ST1 phase 1: scan batches keep their debounce/flush semantics but
        # only sweep the list partition; MB-derived entity caches survive
        # unrelated scan writes. Deferred set unchanged.
        deferred = set(home_prefixes()) | {
            ARTIST_DISCOVERY_PREFIX,
            DISCOVER_QUEUE_ENRICH_PREFIX,
        }
        cache = get_cache()
        await asyncio.gather(
            *(
                cache.clear_prefix(prefix)
                for prefix in catalog_list_prefixes()
                if prefix not in deferred
            )
        )

    return NativeLibraryStore(
        db_path=settings.library_db_path,
        write_lock=get_persistence_write_lock(),
        invalidator=invalidate,
        scan_invalidator=invalidate_scan_batch,
    )


@singleton
def get_library_management_blob_store() -> LibraryManagementBlobStore:
    settings = get_settings()
    return LibraryManagementBlobStore(
        settings.cache_dir / "library-management" / "blobs",
        get_native_library_store(),
    )


@singleton
def get_genre_index() -> GenreIndex:
    settings = get_settings()
    lock = get_persistence_write_lock()
    return GenreIndex(db_path=settings.library_db_path, write_lock=lock)


@singleton
def get_youtube_store() -> YouTubeStore:
    settings = get_settings()
    lock = get_persistence_write_lock()
    return YouTubeStore(db_path=settings.library_db_path, write_lock=lock)


@singleton
def get_mbid_store() -> MBIDStore:
    settings = get_settings()
    lock = get_persistence_write_lock()
    return MBIDStore(db_path=settings.library_db_path, write_lock=lock)

@singleton
def get_mb_canonical_store() -> MbCanonicalStore:
    """ST2 P1: durable canonical maps (release->rg, recording redirects,
    ISRC index). Shares the library DB file and write lock; survives
    musicbrainz_prefixes() sweeps by design (no prefix-list entry)."""
    settings = get_settings()
    lock = get_persistence_write_lock()
    return MbCanonicalStore(db_path=settings.library_db_path, write_lock=lock)


@singleton
def get_sync_state_store() -> SyncStateStore:
    settings = get_settings()
    lock = get_persistence_write_lock()
    return SyncStateStore(db_path=settings.library_db_path, write_lock=lock)


@singleton
def get_discovery_snapshot_store() -> DiscoverySnapshotStore:
    settings = get_settings()
    return DiscoverySnapshotStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_preferences_service() -> "PreferencesService":
    from services.preferences_service import PreferencesService

    settings = get_settings()
    return PreferencesService(settings)


@singleton
def get_mb_response_store():
    from infrastructure.persistence.mb_response_store import MbResponseStore

    return MbResponseStore(
        db_path=get_settings().library_db_path,
        write_lock=get_persistence_write_lock(),
    )


@singleton
def get_cache_service() -> "CacheService":
    from services.cache_service import CacheService

    cache = get_cache()
    library_db = get_library_db()
    disk_cache = get_disk_cache()
    return CacheService(cache, library_db, disk_cache, get_mb_response_store())


@singleton
def get_target_cache_service() -> "CacheService":
    from services.native.target_cache_service import TargetCacheService
    from .service_providers import get_target_library_repository

    return TargetCacheService(
        get_cache(),
        get_target_library_repository(),
        get_disk_cache(),
        get_mb_response_store(),
    )


def get_cache_status_service() -> "CacheStatusService":
    from services.cache_status_service import CacheStatusService

    return CacheStatusService()
