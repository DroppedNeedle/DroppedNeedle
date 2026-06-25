"""DI providers for the inbound Connect Apps (compat) services and stores.

All compat stores share the single WAL database (``settings.library_db_path``)
and the shared persistence write lock so their FKs resolve in the same file.
"""

from __future__ import annotations

import asyncio
import os

from core.config import get_settings

from ._registry import singleton
from .auth_providers import get_auth_store
from .cache_providers import get_persistence_write_lock


@singleton
def get_app_password_store() -> "AppPasswordStore":
    from infrastructure.persistence.app_password_store import AppPasswordStore

    settings = get_settings()
    return AppPasswordStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_app_password_service() -> "AppPasswordService":
    from services.compat.app_password_service import AppPasswordService

    return AppPasswordService(get_app_password_store(), get_auth_store())


@singleton
def get_favorites_store() -> "FavoritesStore":
    from infrastructure.persistence.favorites_store import FavoritesStore

    settings = get_settings()
    return FavoritesStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_favorites_service() -> "FavoritesService":
    from services.compat.favorites_service import FavoritesService

    return FavoritesService(get_favorites_store())


@singleton
def get_compat_id_map_store() -> "CompatIdMapStore":
    from infrastructure.persistence.compat_id_map_store import CompatIdMapStore

    settings = get_settings()
    return CompatIdMapStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_compat_id_map_service() -> "CompatIdMapService":
    from services.compat.id_map_service import CompatIdMapService

    return CompatIdMapService(get_compat_id_map_store())


@singleton
def get_library_view_service() -> "LibraryViewService":
    from services.compat.library_view_service import LibraryViewService
    from .cache_providers import get_library_db
    from .repo_providers import get_coverart_repository
    from .service_providers import get_library_manager

    return LibraryViewService(
        library_manager=get_library_manager(),
        library_db=get_library_db(),
        coverart_repository=get_coverart_repository(),
        favorites_service=get_favorites_service(),
    )


@singleton
def get_compat_scrobble_adapter() -> "CompatScrobbleAdapter":
    from services.compat.compat_scrobble_adapter import CompatScrobbleAdapter
    from .service_providers import get_now_playing_service, get_scrobble_service

    return CompatScrobbleAdapter(
        get_scrobble_service(), get_library_view_service(), get_now_playing_service()
    )


@singleton
def get_transcode_semaphore() -> asyncio.Semaphore:
    # one worker has limited CPU; cap concurrent ffmpeg subprocesses
    return asyncio.Semaphore(max(2, (os.cpu_count() or 2) // 2))


@singleton
def get_transcode_service() -> "TranscodeService":
    from services.compat.transcode_service import TranscodeService

    return TranscodeService(get_transcode_semaphore())


@singleton
def get_compat_discover_service() -> "CompatDiscoverService":
    from services.compat.discover_service import CompatDiscoverService
    from .cache_providers import get_library_db, get_preferences_service
    from .repo_providers import get_play_history_store
    from .service_providers import (
        get_artist_discovery_service,
        get_per_user_client_factory,
    )

    return CompatDiscoverService(
        library_db=get_library_db(),
        library_view_service=get_library_view_service(),
        preferences_service=get_preferences_service(),
        play_history_store=get_play_history_store(),
        artist_discovery_service=get_artist_discovery_service(),
        client_factory=get_per_user_client_factory(),
        # no related-artist infra yet, so lazy-mb caches empty and degrades to
        # local-only until wired.
        related_artists_fetcher=None,
    )
