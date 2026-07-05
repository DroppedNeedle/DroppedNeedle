from fastapi import APIRouter, Depends, File, HTTPException, UploadFile
from fastapi.responses import FileResponse

from api.v1.schemas.common import StatusMessageResponse
from api.v1.schemas.playlists import (
    AddTracksRequest,
    AddTracksResponse,
    CheckTrackMembershipRequest,
    CheckTrackMembershipResponse,
    CoverUploadResponse,
    CreatePlaylistRequest,
    PlaylistDetailResponse,
    PlaylistListResponse,
    PlaylistSummaryResponse,
    PlaylistTrackResponse,
    RedactedPlaylist,
    RemoveTracksRequest,
    ReorderTrackRequest,
    ReorderTrackResponse,
    ResolveSourcesResponse,
    SetPlaylistPublicRequest,
    UpdatePlaylistRequest,
    UpdateTrackRequest,
)
from api.v1.schemas.request import BatchRequestResponse
from core.dependencies import JellyfinLibraryServiceDep, LocalFilesServiceDep, NavidromeLibraryServiceDep, PlexLibraryServiceDep, PlaylistServiceDep, get_request_service
from core.dependencies.type_aliases import CurrentUserDep
from core.exceptions import PlaylistNotFoundError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from services.playlist_service import (
    PlaylistSummaryView,
    RedactedDetailView,
    RedactedSummaryView,
)

router = APIRouter(
    route_class=MsgSpecRoute,
    prefix="/playlists",
    tags=["playlists"],
)


def _normalize_cover_url(url: str | None) -> str | None:
    if url and url.startswith("/api/covers/"):
        return "/api/v1/covers/" + url[len("/api/covers/"):]
    return url


def _normalize_source_type(source_type: str) -> str:
    return source_type


def _normalize_available_sources(sources: list[str] | None) -> list[str] | None:
    if sources is None:
        return None
    return sources


def _custom_cover_url(playlist_id: str, cover_image_path: str | None) -> str | None:
    if cover_image_path:
        return f"/api/v1/playlists/{playlist_id}/cover"
    return None


def _track_to_response(t) -> PlaylistTrackResponse:
    return PlaylistTrackResponse(
        id=t.id,
        position=t.position,
        track_name=t.track_name,
        artist_name=t.artist_name,
        album_name=t.album_name,
        album_id=t.album_id,
        artist_id=t.artist_id,
        track_source_id=t.track_source_id,
        cover_url=_normalize_cover_url(t.cover_url),
        source_type=_normalize_source_type(t.source_type),
        available_sources=_normalize_available_sources(t.available_sources),
        format=t.format,
        track_number=t.track_number,
        disc_number=t.disc_number,
        duration=t.duration,
        created_at=t.created_at,
        plex_rating_key=getattr(t, "plex_rating_key", None),
    )


def _summary_view_to_response(
    view: PlaylistSummaryView | RedactedSummaryView,
) -> PlaylistSummaryResponse | RedactedPlaylist:
    if isinstance(view, RedactedSummaryView):
        return RedactedPlaylist(
            id=view.id, track_count=view.track_count, owner_name=view.owner_name,
        )
    s = view.record
    return PlaylistSummaryResponse(
        id=s.id,
        name=s.name,
        track_count=s.track_count,
        total_duration=s.total_duration,
        cover_urls=[_normalize_cover_url(u) for u in s.cover_urls] if s.cover_urls else [],
        custom_cover_url=_custom_cover_url(s.id, s.cover_image_path),
        source_ref=s.source_ref,
        created_at=s.created_at,
        updated_at=s.updated_at,
        is_public=s.is_public,
        is_owner=view.is_owner,
        owner_name=view.owner_name,
    )


def _detail_to_response(
    playlist, tracks, *, is_owner: bool, owner_name: str | None,
) -> PlaylistDetailResponse:
    track_responses = [_track_to_response(t) for t in tracks]
    cover_urls = list(dict.fromkeys(_normalize_cover_url(t.cover_url) for t in tracks if t.cover_url))[:4]
    total_duration = sum(t.duration for t in tracks if t.duration)
    return PlaylistDetailResponse(
        id=playlist.id,
        name=playlist.name,
        cover_urls=cover_urls,
        custom_cover_url=_custom_cover_url(playlist.id, playlist.cover_image_path),
        source_ref=playlist.source_ref,
        tracks=track_responses,
        track_count=len(tracks),
        total_duration=total_duration or None,
        created_at=playlist.created_at,
        updated_at=playlist.updated_at,
        is_public=playlist.is_public,
        is_owner=is_owner,
        owner_name=owner_name,
    )


@router.get("", response_model=PlaylistListResponse)
async def list_playlists(
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
) -> PlaylistListResponse:
    views = await service.get_all_playlists(current_user)
    return PlaylistListResponse(
        playlists=[_summary_view_to_response(v) for v in views]
    )


@router.post("/check-tracks", response_model=CheckTrackMembershipResponse)
async def check_track_membership(
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    body: CheckTrackMembershipRequest = MsgSpecBody(CheckTrackMembershipRequest),
) -> CheckTrackMembershipResponse:
    tracks = [(t.track_name, t.artist_name, t.album_name) for t in body.tracks]
    membership = await service.check_track_membership(tracks, user_id=current_user.id)
    return CheckTrackMembershipResponse(membership=membership)


@router.post("", response_model=PlaylistDetailResponse, status_code=201)
async def create_playlist(
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    body: CreatePlaylistRequest = MsgSpecBody(CreatePlaylistRequest),
) -> PlaylistDetailResponse:
    playlist = await service.create_playlist(body.name, user_id=current_user.id)
    return _detail_to_response(playlist, [], is_owner=True, owner_name=None)


@router.get("/{playlist_id}", response_model=PlaylistDetailResponse | RedactedPlaylist)
async def get_playlist(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
) -> PlaylistDetailResponse | RedactedPlaylist:
    view = await service.get_playlist_with_tracks(playlist_id, current_user)
    if isinstance(view, RedactedDetailView):
        return RedactedPlaylist(
            id=view.id, track_count=view.track_count, owner_name=view.owner_name,
        )
    return _detail_to_response(
        view.record, view.tracks, is_owner=view.is_owner, owner_name=view.owner_name,
    )


@router.put("/{playlist_id}", response_model=PlaylistDetailResponse)
async def update_playlist(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    body: UpdatePlaylistRequest = MsgSpecBody(UpdatePlaylistRequest),
) -> PlaylistDetailResponse:
    playlist, tracks = await service.update_playlist_with_detail(
        playlist_id, current_user, name=body.name,
    )
    return _detail_to_response(playlist, tracks, is_owner=True, owner_name=None)


@router.delete("/{playlist_id}", response_model=StatusMessageResponse)
async def delete_playlist(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
) -> StatusMessageResponse:
    await service.delete_playlist(playlist_id, current_user)
    return StatusMessageResponse(status="ok", message="Playlist deleted")


@router.patch("/{playlist_id}/share", response_model=PlaylistSummaryResponse)
async def set_playlist_visibility(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    body: SetPlaylistPublicRequest = MsgSpecBody(SetPlaylistPublicRequest),
) -> PlaylistSummaryResponse:
    view = await service.set_public(playlist_id, current_user, body.is_public)
    return _summary_view_to_response(view)


@router.post(
    "/{playlist_id}/tracks",
    response_model=AddTracksResponse,
    status_code=201,
)
async def add_tracks(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    body: AddTracksRequest = MsgSpecBody(AddTracksRequest),
) -> AddTracksResponse:
    track_dicts = [
        {
            "track_name": t.track_name,
            "artist_name": t.artist_name,
            "album_name": t.album_name,
            "album_id": t.album_id,
            "artist_id": t.artist_id,
            "track_source_id": t.track_source_id,
            "cover_url": t.cover_url,
            "source_type": t.source_type,
            "available_sources": t.available_sources,
            "format": t.format,
            "track_number": t.track_number,
            "disc_number": t.disc_number,
            "duration": int(t.duration) if t.duration is not None else None,
            "plex_rating_key": t.plex_rating_key,
        }
        for t in body.tracks
    ]
    created = await service.add_tracks(playlist_id, current_user, track_dicts, body.position)
    return AddTracksResponse(tracks=[_track_to_response(t) for t in created])


@router.post(
    "/{playlist_id}/tracks/remove",
    response_model=StatusMessageResponse,
)
async def remove_tracks(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    body: RemoveTracksRequest = MsgSpecBody(RemoveTracksRequest),
) -> StatusMessageResponse:
    removed = await service.remove_tracks(playlist_id, current_user, body.track_ids)
    return StatusMessageResponse(status="ok", message=f"{removed} track(s) removed")


@router.delete(
    "/{playlist_id}/tracks/{track_id}",
    response_model=StatusMessageResponse,
)
async def remove_track(
    playlist_id: str,
    track_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
) -> StatusMessageResponse:
    await service.remove_track(playlist_id, current_user, track_id)
    return StatusMessageResponse(status="ok", message="Track removed")


# Reorder must be registered before the {track_id} PATCH to avoid
# "reorder" being captured as a track_id path parameter.
@router.patch(
    "/{playlist_id}/tracks/reorder",
    response_model=ReorderTrackResponse,
)
async def reorder_track(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    body: ReorderTrackRequest = MsgSpecBody(ReorderTrackRequest),
) -> ReorderTrackResponse:
    actual_position = await service.reorder_track(
        playlist_id, current_user, body.track_id, body.new_position,
    )
    return ReorderTrackResponse(
        status="ok",
        message="Track reordered",
        actual_position=actual_position,
    )


@router.patch(
    "/{playlist_id}/tracks/{track_id}",
    response_model=PlaylistTrackResponse,
)
async def update_track(
    playlist_id: str,
    track_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    jf_service: JellyfinLibraryServiceDep,
    local_service: LocalFilesServiceDep,
    nd_service: NavidromeLibraryServiceDep,
    plex_service: PlexLibraryServiceDep,
    body: UpdateTrackRequest = MsgSpecBody(UpdateTrackRequest),
) -> PlaylistTrackResponse:
    result = await service.update_track_source(
        playlist_id, current_user, track_id,
        source_type=body.source_type,
        available_sources=body.available_sources,
        jf_service=jf_service,
        local_service=local_service,
        nd_service=nd_service,
        plex_service=plex_service,
    )
    return _track_to_response(result)


@router.post(
    "/{playlist_id}/resolve-sources",
    response_model=ResolveSourcesResponse,
)
async def resolve_sources(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    jf_service: JellyfinLibraryServiceDep,
    local_service: LocalFilesServiceDep,
    nd_service: NavidromeLibraryServiceDep,
    plex_service: PlexLibraryServiceDep,
) -> ResolveSourcesResponse:
    sources = await service.resolve_track_sources(
        playlist_id, requesting=current_user, jf_service=jf_service, local_service=local_service,
        nd_service=nd_service, plex_service=plex_service,
    )
    return ResolveSourcesResponse(sources=sources)


@router.post("/{playlist_id}/cover", response_model=CoverUploadResponse)
async def upload_cover(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    cover_image: UploadFile = File(...),
) -> CoverUploadResponse:
    max_size = 2 * 1024 * 1024
    chunk_size = 8192
    chunks: list[bytes] = []
    total = 0
    while True:
        chunk = await cover_image.read(chunk_size)
        if not chunk:
            break
        total += len(chunk)
        if total > max_size:
            from core.exceptions import InvalidPlaylistDataError
            raise InvalidPlaylistDataError("Image too large. Maximum size is 2 MB")
        chunks.append(chunk)
    data = b"".join(chunks)
    cover_url = await service.upload_cover(
        playlist_id, current_user, data, cover_image.content_type or "",
    )
    return CoverUploadResponse(cover_url=cover_url)


@router.get("/{playlist_id}/cover")
async def get_cover(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
):
    path = await service.get_cover_path(playlist_id, current_user)
    if path is None:
        raise PlaylistNotFoundError("No cover found")

    media_type = {
        ".jpg": "image/jpeg",
        ".jpeg": "image/jpeg",
        ".png": "image/png",
        ".webp": "image/webp",
    }.get(path.suffix.lower(), "application/octet-stream")

    return FileResponse(
        path,
        media_type=media_type,
        headers={"Cache-Control": "public, max-age=3600"},
    )


@router.delete(
    "/{playlist_id}/cover",
    response_model=StatusMessageResponse,
)
async def remove_cover(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
) -> StatusMessageResponse:
    await service.remove_cover(playlist_id, current_user)
    return StatusMessageResponse(status="ok", message="Cover removed")


@router.post(
    "/{playlist_id}/request-missing",
    response_model=BatchRequestResponse,
    status_code=202,
)
async def request_missing_tracks(
    playlist_id: str,
    service: PlaylistServiceDep,
    current_user: CurrentUserDep,
    request_service=Depends(get_request_service),
) -> BatchRequestResponse:
    result = await service.get_playlist_with_tracks(playlist_id, current_user)
    if isinstance(result, RedactedDetailView):
        raise HTTPException(status_code=403, detail="Access denied")

    seen: set[str] = set()
    items: list[dict] = []
    for track in result.tracks:
        mbid = track.album_id
        if not mbid or mbid in seen:
            continue
        if track.available_sources and len(track.available_sources) > 0:
            continue
        seen.add(mbid)
        items.append(
            {
                "musicbrainz_id": mbid,
                "artist_name": track.artist_name or "Unknown",
                "album_title": track.album_name or "Unknown",
            }
        )

    if not items:
        return BatchRequestResponse(
            success=True,
            message="No missing albums found, all tracks already have a source",
        )

    return await request_service.request_batch(
        items=items,
        user_id=current_user.id,
        user_role=current_user.role,
        requested_by_name=current_user.display_name,
    )
