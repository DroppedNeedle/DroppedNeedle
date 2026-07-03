"""Spotify playlist browsing and import endpoints."""

import logging

from fastapi import APIRouter, Depends, HTTPException

from core.dependencies import (
    JellyfinLibraryServiceDep,
    LocalFilesServiceDep,
    NavidromeLibraryServiceDep,
    PlexLibraryServiceDep,
    PlaylistServiceDep,
    get_spotify_import_service,
)
from infrastructure.msgspec_fastapi import AppStruct, MsgSpecRoute
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


class SpotifyImportResponse(AppStruct):
    playlist_id: str


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
    current_user: CurrentUserDep,
    playlist_service: PlaylistServiceDep,
    jf_service: JellyfinLibraryServiceDep,
    local_service: LocalFilesServiceDep,
    nd_service: NavidromeLibraryServiceDep,
    plex_service: PlexLibraryServiceDep,
    svc: SpotifyImportService = Depends(get_spotify_import_service),
) -> SpotifyImportResponse:
    try:
        playlist_id = await svc.import_playlist(current_user.id, spotify_playlist_id)
    except SpotifyNotLinkedError:
        raise HTTPException(status_code=400, detail="Spotify account not linked")
    except Exception as exc:  # noqa: BLE001
        logger.error(
            f"Spotify import failed for user {current_user.id} playlist {spotify_playlist_id}: {exc}"
        )
        raise HTTPException(status_code=502, detail="Failed to import playlist from Spotify")

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

    return SpotifyImportResponse(playlist_id=playlist_id)
