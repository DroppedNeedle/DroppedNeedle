import asyncio
import logging
from fastapi import APIRouter, Depends, HTTPException, Query
from api.v1.schemas.common import StatusMessageResponse
from api.v1.schemas.library import (
    LibraryResponse,
    RecentlyAddedResponse,
    AlbumRemoveResponse,
    AlbumRemovePreviewResponse,
    SyncLibraryResponse,
    LibraryMbidsResponse,
    LibraryGroupedResponse,
    TrackResolveRequest,
    TrackResolveResponse,
    NativeAlbumsResponse,
    NativeArtistsResponse,
    NativeTrackPage,
    NativeTracksResponse,
    NativeLibraryStatsResponse,
    LibraryAlbumStatusResponse,
    LibraryLyricsResponse,
    LibraryTrackResponse,
    TrackTagUpdateRequest,
)
from core.dependencies import (
    get_album_service,
    get_download_service,
    get_library_service,
    get_library_manager,
    get_library_scanner,
    get_local_lyrics_service,
    get_preferences_service,
)
from core.exceptions import ExternalServiceError, ResourceNotFoundError
from infrastructure.msgspec_fastapi import MsgSpecRoute, MsgSpecBody
from middleware import CurrentAdminDep, CurrentCuratorDep, CurrentUserDep
from models.audio import AudioTag
from services.album_service import AlbumService
from services.library_service import LibraryService
from services.local_lyrics_service import LocalLyricsService
from services.native.library_manager import LibraryManager
from services.native.library_scanner import LibraryScanner

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/library", tags=["library"])


@router.get("/", response_model=LibraryResponse)
async def get_library(
    library_service: LibraryService = Depends(get_library_service)
):
    library = await library_service.get_library()
    return LibraryResponse(library=library)


@router.get("/artists", response_model=NativeArtistsResponse)
async def get_library_artists(
    current_user: CurrentUserDep,
    limit: int = 50,
    offset: int = 0,
    sort_by: str = "name",
    sort_order: str = "asc",
    q: str | None = None,
    library_manager: LibraryManager = Depends(get_library_manager),
):
    limit = max(1, min(limit, 100))
    offset = max(0, offset)
    if sort_by not in ("name", "album_count", "date_added"):
        sort_by = "name"
    if sort_order not in ("asc", "desc"):
        sort_order = "asc"
    search = (q or "").strip() or None
    items, total = await library_manager.get_artists(
        limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order, q=search
    )
    return NativeArtistsResponse(items=items, total=total)


@router.get("/albums", response_model=NativeAlbumsResponse)
async def get_library_albums(
    current_user: CurrentUserDep,
    page: int = 1,
    page_size: int = 50,
    sort: str = "recent",
    q: str | None = None,
    file_format: str | None = Query(default=None, alias="format"),
    library_manager: LibraryManager = Depends(get_library_manager),
):
    page_size = max(1, min(page_size, 100))
    if sort not in ("recent", "title", "artist"):
        sort = "recent"
    search = (q or "").strip() or None
    fmt = (file_format or "").strip().lower() or None
    items, total = await library_manager.get_albums_page(
        page=page, page_size=page_size, sort=sort, q=search, file_format=fmt
    )
    return NativeAlbumsResponse(items=items, total=total)


@router.get("/tracks", response_model=NativeTrackPage)
async def get_library_tracks(
    current_user: CurrentUserDep,
    limit: int = 48,
    offset: int = 0,
    sort: str = "recent",
    q: str | None = None,
    library_manager: LibraryManager = Depends(get_library_manager),
):
    limit = max(1, min(limit, 200))
    offset = max(0, offset)
    if sort not in ("recent", "title", "artist", "album"):
        sort = "recent"
    search = (q or "").strip() or None
    items, total = await library_manager.get_tracks_page(
        limit=limit, offset=offset, sort=sort, q=search
    )
    return NativeTrackPage(items=items, total=total, offset=offset, limit=limit)


@router.get("/recently-added", response_model=RecentlyAddedResponse)
async def get_recently_added(
    limit: int = 20,
    library_service: LibraryService = Depends(get_library_service)
):
    albums = await library_service.get_recently_added(limit=limit)
    return RecentlyAddedResponse(albums=albums, artists=[])


@router.post("/sync", response_model=SyncLibraryResponse)
async def sync_library(
    force_full: bool = Query(default=False, description="Clear resume checkpoint and start a full sync from scratch"),
    library_service: LibraryService = Depends(get_library_service)
):
    try:
        return await library_service.sync_library(is_manual=True, force_full=force_full)
    except ExternalServiceError as e:
        if "cooldown" in str(e).lower():
            raise HTTPException(status_code=429, detail="Sync is on cooldown, please wait")
        raise


@router.get("/stats", response_model=NativeLibraryStatsResponse)
async def get_library_stats(
    current_user: CurrentUserDep,
    library_manager: LibraryManager = Depends(get_library_manager),
):
    return await library_manager.get_stats()


@router.get("/mbids", response_model=LibraryMbidsResponse)
async def get_library_mbids(
    library_service: LibraryService = Depends(get_library_service)
):
    mbids, requested = await asyncio.gather(
        library_service.get_library_mbids(),
        library_service.get_requested_mbids(),
    )
    return LibraryMbidsResponse(mbids=mbids, requested_mbids=requested)


@router.get("/grouped", response_model=LibraryGroupedResponse)
async def get_library_grouped(
    library_service: LibraryService = Depends(get_library_service)
):
    grouped = await library_service.get_library_grouped()
    return LibraryGroupedResponse(library=grouped)


@router.get("/album/{album_mbid}/removal-preview", response_model=AlbumRemovePreviewResponse)
async def get_album_removal_preview(
    _admin: CurrentAdminDep,
    album_mbid: str,
    library_service: LibraryService = Depends(get_library_service)
):
    try:
        return await library_service.get_album_removal_preview(album_mbid)
    except ExternalServiceError as e:
        logger.error(f"Failed to get album removal preview: {e}")
        raise HTTPException(status_code=500, detail="Failed to load removal preview")


@router.delete("/album/{album_mbid}", response_model=AlbumRemoveResponse)
async def remove_album(
    _admin: CurrentAdminDep,
    album_mbid: str,
    delete_files: bool = False,
    library_service: LibraryService = Depends(get_library_service),
    download_service=Depends(get_download_service),
):
    try:
        result = await library_service.remove_album(album_mbid, delete_files=delete_files)
    except ExternalServiceError as e:
        logger.error(f"Couldn't remove album {album_mbid}: {e}")
        raise HTTPException(status_code=500, detail="Couldn't remove this album")
    # Removing the album must also clear its download-side state - pending auto-retries
    # (or the "retry N/M in ..." loop re-downloads a deleted album), held "Couldn't verify"
    # tracks (rows + files), and blocklist entries. Best-effort: a failure here must not
    # fail the removal the user already confirmed.
    try:
        await download_service.purge_album_downloads(album_mbid)
    except Exception as e:  # noqa: BLE001
        logger.warning(f"Album {album_mbid} removed but download-state cleanup failed: {e}")
    return result


@router.post("/resolve-tracks", response_model=TrackResolveResponse)
async def resolve_tracks(
    body: TrackResolveRequest = MsgSpecBody(TrackResolveRequest),
    library_service: LibraryService = Depends(get_library_service),
):
    return await library_service.resolve_tracks_batch(body.items)


@router.get("/albums/{mbid}/tracks", response_model=NativeTracksResponse)
async def get_native_album_tracks(
    mbid: str,
    current_user: CurrentUserDep,
    library_manager: LibraryManager = Depends(get_library_manager),
):
    tracks = await library_manager.get_tracks(mbid)
    return NativeTracksResponse(items=tracks)


@router.get("/albums/{mbid}/status", response_model=LibraryAlbumStatusResponse)
async def get_native_album_status(
    mbid: str,
    current_user: CurrentUserDep,
    library_manager: LibraryManager = Depends(get_library_manager),
    album_service: AlbumService = Depends(get_album_service),
):
    # live download progress comes from GET /downloads?release_group_mbid=..., not here
    policy = get_preferences_service().get_download_policy()
    status = await library_manager.get_album_status(
        mbid,
        quality_cutoff=policy.quality_cutoff,
        upgrade_allowed=policy.upgrade_allowed,
    )
    # P5 coverage annotation: which held files COVER the release's expected tracks
    # (drives the honest In-Library badge, matched-only Play All, and the orphan
    # review section). Fail-open - a MB hiccup leaves the presence-only reading.
    return await album_service.annotate_album_coverage(mbid, status)


@router.delete("/tracks/{file_id}", response_model=StatusMessageResponse)
async def remove_library_track(
    file_id: str,
    current_user: CurrentCuratorDep,
    library_service: LibraryService = Depends(get_library_service),
):
    """Remove one library file - the album page's orphan-review action (P5): a held
    file matching none of the album's expected tracks. Admin/trusted only, matching
    the album-delete gate."""
    return await library_service.remove_file(file_id)


@router.get("/tracks/{file_id}/lyrics", response_model=LibraryLyricsResponse)
async def get_track_lyrics(
    file_id: str,
    current_user: CurrentUserDep,
    lyrics_service: LocalLyricsService = Depends(get_local_lyrics_service),
) -> LibraryLyricsResponse:
    """Lyrics for a local library track: sidecar .lrc/.txt first, then embedded
    tags (USLT/SYLT, Vorbis LYRICS, MP4 lyr). 404 = no lyrics, matching the
    jellyfin/navidrome lyrics endpoints the frontend already consumes."""
    try:
        lyrics = await lyrics_service.get_lyrics(file_id)
    except ResourceNotFoundError:
        raise HTTPException(status_code=404, detail="Track file not found")
    except PermissionError:
        raise HTTPException(
            status_code=403, detail="Access denied: path is outside the music directory"
        )
    except ExternalServiceError as e:
        logger.error("Lyrics lookup failed for %s: %s", file_id, e)
        raise HTTPException(status_code=502, detail="Failed to read lyrics")
    if lyrics is None:
        raise HTTPException(status_code=404, detail="Lyrics not available")
    return lyrics


@router.get("/tracks/{file_id}/tags", response_model=AudioTag)
async def get_track_tags(
    file_id: str,
    current_user: CurrentAdminDep,
    scanner: LibraryScanner = Depends(get_library_scanner),
):
    return await scanner.read_track_tags(file_id)


@router.post("/tracks/{file_id}", response_model=LibraryTrackResponse)
async def update_track_tags(
    file_id: str,
    current_user: CurrentAdminDep,
    body: TrackTagUpdateRequest = MsgSpecBody(TrackTagUpdateRequest),
    scanner: LibraryScanner = Depends(get_library_scanner),
):
    new_tag = AudioTag(
        title=body.title,
        artist=body.artist,
        album=body.album,
        track_number=body.track_number,
        album_artist=body.album_artist,
        disc_number=body.disc_number,
        year=body.year,
        genre=body.genre,
        musicbrainz_release_group_id=body.musicbrainz_release_group_id,
        musicbrainz_release_id=body.musicbrainz_release_id,
        musicbrainz_recording_id=body.musicbrainz_recording_id,
        musicbrainz_artist_id=body.musicbrainz_artist_id,
        musicbrainz_album_artist_id=body.musicbrainz_album_artist_id,
    )
    return await scanner.update_track_tags(file_id, new_tag)


def _log_rescan_exception(task: asyncio.Task) -> None:
    if not task.cancelled() and task.exception() is not None:
        logger.error("Album rescan task failed: %s", task.exception())


@router.post("/albums/{mbid}/rescan", status_code=202, response_model=StatusMessageResponse)
async def rescan_native_album(
    mbid: str,
    current_user: CurrentAdminDep,
    scanner: LibraryScanner = Depends(get_library_scanner),
):
    from core.task_registry import TaskRegistry

    task = asyncio.create_task(scanner.rescan_album(mbid))
    try:
        TaskRegistry.get_instance().register(f"library-rescan-{mbid}", task)
    except RuntimeError:
        # rescan already running; idempotent no-op
        task.cancel()
        return StatusMessageResponse(status="accepted", message="Album rescan already running")
    task.add_done_callback(_log_rescan_exception)
    return StatusMessageResponse(status="accepted", message="Album rescan started")


@router.post(
    "/albums/{mbid}/reidentify", status_code=202, response_model=StatusMessageResponse
)
async def reidentify_native_album(
    mbid: str,
    current_user: CurrentAdminDep,
    scanner: LibraryScanner = Depends(get_library_scanner),
):
    """Force a fresh whole-folder re-identification of an album, overriding the scan's
    stability guards - the correction path for an album stuck on the wrong release group."""
    from core.task_registry import TaskRegistry

    task = asyncio.create_task(scanner.reidentify_album(mbid))
    try:
        TaskRegistry.get_instance().register(f"library-reidentify-{mbid}", task)
    except RuntimeError:
        task.cancel()
        return StatusMessageResponse(
            status="accepted", message="Album re-identify already running"
        )
    task.add_done_callback(_log_rescan_exception)
    return StatusMessageResponse(status="accepted", message="Album re-identify started")
