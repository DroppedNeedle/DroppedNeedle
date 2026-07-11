"""Shared DI bundle for the compat shims.

Resolve services through FastAPI Depends so tests can override individual
providers via app.dependency_overrides; bundling keeps route signatures short
while each inner Depends stays independently overridable.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from fastapi import Depends

from core.dependencies import (
    get_app_password_service,
    get_compat_discover_service,
    get_compat_id_map_service,
    get_compat_scrobble_adapter,
    get_coverart_repository,
    get_favorites_service,
    get_library_view_service,
    get_local_files_service,
    get_now_playing_service,
    get_playlist_service,
    get_preferences_service,
    get_transcode_service,
)

if TYPE_CHECKING:
    from repositories.coverart_repository import CoverArtRepository
    from services.compat.app_password_service import AppPasswordService
    from services.compat.compat_scrobble_adapter import CompatScrobbleAdapter
    from services.compat.discover_service import CompatDiscoverService
    from services.compat.favorites_service import FavoritesService
    from services.compat.id_map_service import CompatIdMapService
    from services.compat.library_view_service import LibraryViewService
    from services.compat.transcode_service import TranscodeService
    from services.local_files_service import LocalFilesService
    from services.now_playing_service import NowPlayingService
    from services.playlist_service import PlaylistService
    from services.preferences_service import PreferencesService


@dataclass
class CompatServices:
    app_passwords: "AppPasswordService"
    view: "LibraryViewService"
    favorites: "FavoritesService"
    playlists: "PlaylistService"
    scrobble: "CompatScrobbleAdapter"
    discover: "CompatDiscoverService"
    id_map: "CompatIdMapService"
    local_files: "LocalFilesService"
    coverart: "CoverArtRepository"
    preferences: "PreferencesService"
    transcode: "TranscodeService"
    now_playing: "NowPlayingService"


def get_compat_services(
    app_passwords=Depends(get_app_password_service),
    view=Depends(get_library_view_service),
    favorites=Depends(get_favorites_service),
    playlists=Depends(get_playlist_service),
    scrobble=Depends(get_compat_scrobble_adapter),
    discover=Depends(get_compat_discover_service),
    id_map=Depends(get_compat_id_map_service),
    local_files=Depends(get_local_files_service),
    coverart=Depends(get_coverart_repository),
    preferences=Depends(get_preferences_service),
    transcode=Depends(get_transcode_service),
    now_playing=Depends(get_now_playing_service),
) -> CompatServices:
    return CompatServices(
        app_passwords=app_passwords,
        view=view,
        favorites=favorites,
        playlists=playlists,
        scrobble=scrobble,
        discover=discover,
        id_map=id_map,
        local_files=local_files,
        coverart=coverart,
        preferences=preferences,
        transcode=transcode,
        now_playing=now_playing,
    )
