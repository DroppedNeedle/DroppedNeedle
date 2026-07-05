"""Spotify playlist browsing and import endpoints."""

import asyncio
import logging

from fastapi import APIRouter, Depends, HTTPException

from core.dependencies import (
    get_jellyfin_library_service,
    get_local_files_service,
    get_navidrome_library_service,
    get_plex_library_service,
    get_playlist_service,
    get_spotify_import_service,
)
from core.task_registry import TaskRegistry
from infrastructure.msgspec_fastapi import AppStruct, MsgSpecBody, MsgSpecRoute
from middleware import CurrentUserDep
from services.spotify_import_service import SpotifyImportService, SpotifyNotLinkedError

_LINK_SOURCE_PRIORITY = ["local", "jellyfin", "navidrome", "plex"]

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/me/spotify", tags=["spotify"])


class SpotifyPlaylistItem(AppStruct):
    id: str
    name: str
    description: str
    track_count: int
    cover_url: str | None
    owner: str
    imported_playlist_id: str | None


class SpotifyPlaylistListResponse(AppStruct):
    playlists: list[SpotifyPlaylistItem]


class SpotifyImportRequest(AppStruct):
    name: str


class SpotifyImportResponse(AppStruct):
    playlist_id: str


async def _background_import(
    svc: SpotifyImportService,
    user_id: str,
    spotify_playlist_id: str,
    playlist_id: str,
    current_user: object,
) -> None:
    playlist_service = get_playlist_service()
    jf_service = get_jellyfin_library_service()
    local_service = get_local_files_service()
    nd_service = get_navidrome_library_service()
    plex_service = get_plex_library_service()
    try:
        await svc.populate_playlist(user_id, spotify_playlist_id, playlist_id)
    except Exception as exc:  # noqa: BLE001
        logger.error(
            f"Background Spotify import failed for playlist {playlist_id}: {exc}"
        )
        return
    try:
        sources_map = await playlist_service.resolve_track_sources(
            playlist_id,
            requesting=None,
            jf_service=jf_service,
            local_service=local_service,
            nd_service=nd_service,
            plex_service=plex_service,
        )
        for track_id, sources in sources_map.items():
            if not sources:
                continue
            best = next((s for s in _LINK_SOURCE_PRIORITY if s in sources), None)
            if best:
                try:
                    await playlist_service.update_track_source(
                        playlist_id,
                        current_user,
                        track_id,
                        source_type=best,
                        jf_service=jf_service,
                        local_service=local_service,
                        nd_service=nd_service,
                        plex_service=plex_service,
                    )
                except Exception:  # noqa: BLE001
                    pass
    except Exception as exc:  # noqa: BLE001
        logger.warning(f"Auto-link failed for playlist {playlist_id}: {exc}")


@router.get("/playlists", response_model=SpotifyPlaylistListResponse)
async def list_spotify_playlists(
    current_user: CurrentUserDep,
    svc: SpotifyImportService = Depends(get_spotify_import_service),
) -> SpotifyPlaylistListResponse:
    try:
        playlists = await svc.list_playlists(current_user.id)
    except SpotifyNotLinkedError:
        raise HTTPException(status_code=400, detail="Spotify account not linked")
    except Exception as exc:  # noqa: BLE001
        logger.error(f"Failed to list Spotify playlists for {current_user.id}: {exc}")
        raise HTTPException(status_code=502, detail="Failed to fetch playlists from Spotify")
    return SpotifyPlaylistListResponse(
        playlists=[
            SpotifyPlaylistItem(
                id=p["id"],
                name=p["name"],
                description=p["description"],
                track_count=p["track_count"],
                cover_url=p["cover_url"],
                owner=p["owner"],
                imported_playlist_id=p["imported_playlist_id"],
            )
            for p in playlists
        ]
    )


@router.post(
    "/playlists/{spotify_playlist_id}/import",
    response_model=SpotifyImportResponse,
)
async def import_spotify_playlist(
    spotify_playlist_id: str,
    body: SpotifyImportRequest = MsgSpecBody(SpotifyImportRequest),
    current_user: CurrentUserDep = None,
    svc: SpotifyImportService = Depends(get_spotify_import_service),
) -> SpotifyImportResponse:
    try:
        playlist_id = await svc.ensure_playlist_record(
            current_user.id, spotify_playlist_id, body.name
        )
    except SpotifyNotLinkedError:
        raise HTTPException(status_code=400, detail="Spotify account not linked")
    except Exception as exc:  # noqa: BLE001
        logger.error(
            f"Spotify import setup failed for user {current_user.id} playlist {spotify_playlist_id}: {exc}"
        )
        raise HTTPException(status_code=502, detail="Failed to start playlist import")

    task_key = f"spotify:import:{current_user.id}:{spotify_playlist_id}"
    registry = TaskRegistry.get_instance()
    if not registry.is_running(task_key):
        task = asyncio.create_task(
            _background_import(
                svc, current_user.id, spotify_playlist_id, playlist_id, current_user
            )
        )
        try:
            registry.register(task_key, task)
        except RuntimeError:
            pass

    return SpotifyImportResponse(playlist_id=playlist_id)
