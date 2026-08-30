import asyncio
import logging
import time

from fastapi import APIRouter, Depends, Query, Request, Response
from fastapi.responses import RedirectResponse
from starlette.datastructures import URL

from api.v1.schemas.library_target import (
    TargetNativeAlbumDetail,
    TargetNativeAlbumsResponse,
    TargetNativeAlbumStatusResponse,
    ReleaseEditionResult,
    ReleaseEditionSearchResponse,
    TargetNativeArtist,
    TargetNativeArtistAppearancesResponse,
    TargetNativeArtistsResponse,
    TargetNativeStatsResponse,
    TargetNativeTrack,
    TargetNativeTracksResponse,
    TargetCatalogRemovalResponse,
    ManagementReenableRequest,
    ManagementReenableResponse,
)
from api.v1.schemas.edition_conversion import (
    EditionConversionCancelRequest,
    EditionConversionPreflightRequest,
    EditionConversionPreviewRequest,
    EditionConversionPreviewResponse,
    EditionConversionRecheckRequest,
    EditionConversionRetryRequest,
    EditionConversionStartRequest,
    EditionConversionStatusResponse,
)
from api.v1.schemas.library import (
    LibraryMbidsResponse,
    LibraryMembershipRequest,
    LibraryMembershipResponse,
    TrackResolveRequest,
    TrackResolveResponse,
)
from api.v1.schemas.library_scan_target import ScanRunRequestedResponse
from core.exceptions import ResourceNotFoundError, ValidationError
from core.dependencies.type_aliases import (
    LibraryPolicyResolverDep,
    PreferencesServiceDep,
    RequestHistoryStoreDep,
    TargetCatalogWriterServiceDep,
    TargetLibraryScanCoordinatorDep,
    TargetLibraryOwnershipServiceDep,
    TargetNativeLibraryServiceDep,
    EditionConversionServiceDep,
    TargetAlbumEditionFinderServiceDep,
    CachedLocalArtworkServiceDep,
    WantedWatcherServiceDep,
)
from core.dependencies import get_download_service
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep, CurrentCuratorDep, CurrentUserDep
from models.audio import AudioTag
from models.library_work import ScanRequest


logger = logging.getLogger(__name__)

router = APIRouter(
    route_class=MsgSpecRoute,
    prefix="/library",
    tags=["library-target"],
)


def _path_relative_location(target: URL) -> str:
    """Keep redirects same-origin without trusting request URL metadata.

    ``request.url_for`` includes the ASGI root path; retain only its path and
    query to preserve deployment prefixes without reflecting Host or scheme.
    """
    path = "/" + target.path.lstrip("/")
    return f"{path}?{target.query}" if target.query else path


@router.get("/albums/{album_id}/artwork/cached")
async def get_cached_local_album_artwork(
    album_id: str,
    _user: CurrentUserDep,
    service: CachedLocalArtworkServiceDep,
    v: int = Query(ge=1),
) -> Response:
    artwork = await service.get(album_id, v)
    if artwork is None:
        return Response(
            status_code=404,
            headers={
                "Cache-Control": "private, max-age=30",
                "X-Cover-State": "missing",
            },
        )
    content, content_type, source, content_hash = artwork
    return Response(
        content=content,
        media_type=content_type,
        headers={
            "Cache-Control": "private, max-age=31536000, immutable",
            "ETag": f'"{content_hash}"',
            "X-Cover-Source": source,
        },
    )


@router.get("/artists", response_model=TargetNativeArtistsResponse)
async def get_target_artists(
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
    limit: int = 50,
    offset: int = 0,
    sort_by: str = "name",
    sort_order: str = "asc",
    q: str | None = None,
    scope: str = "album",
) -> TargetNativeArtistsResponse:
    normalized_sort = (
        sort_by
        if sort_by in {"name", "album_count", "appearance_count", "date_added"}
        else "name"
    )
    normalized_scope = scope if scope in {"album", "contributors"} else "album"
    items, total = await service.artists(
        limit=max(1, min(limit, 100)),
        offset=max(0, offset),
        search=(q or "").strip() or None,
        sort_by=normalized_sort,
        sort_order="desc" if sort_order == "desc" else "asc",
        scope=normalized_scope,
    )
    album_artist_total, contributor_total = await service.artist_scope_counts()
    return TargetNativeArtistsResponse(
        items=items,
        total=total,
        album_artist_total=album_artist_total,
        contributor_total=contributor_total,
    )


@router.get("/albums", response_model=TargetNativeAlbumsResponse)
async def get_target_albums(
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
    page: int = 1,
    page_size: int = 50,
    sort: str = "recent",
    q: str | None = None,
    file_format: str | None = Query(default=None, alias="format"),
) -> TargetNativeAlbumsResponse:
    allowed = {"recent", "newest", "oldest", "name", "artist", "random"}
    normalized_sort = "name" if sort == "title" else sort
    if normalized_sort not in allowed:
        normalized_sort = "recent"
    size = max(1, min(page_size, 100))
    items, total = await service.albums(
        limit=size,
        offset=max(0, page - 1) * size,
        sort=normalized_sort,
        search=(q or "").strip() or None,
        file_format=(file_format or "").strip().casefold() or None,
    )
    return TargetNativeAlbumsResponse(items=items, total=total)


@router.get("/tracks", response_model=TargetNativeTracksResponse)
async def get_target_tracks(
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
    limit: int = 48,
    offset: int = 0,
    sort: str = "recent",
    q: str | None = None,
) -> TargetNativeTracksResponse:
    allowed = {"recent", "title", "artist", "album", "random"}
    normalized_sort = sort if sort in allowed else "recent"
    size = max(1, min(limit, 200))
    normalized_offset = max(0, offset)
    items, total = await service.tracks(
        limit=size,
        offset=normalized_offset,
        sort=normalized_sort,
        search=(q or "").strip() or None,
    )
    return TargetNativeTracksResponse(
        items=items, total=total, offset=normalized_offset, limit=size
    )


@router.get("/stats", response_model=TargetNativeStatsResponse)
async def get_target_stats(
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
) -> TargetNativeStatsResponse:
    return await service.stats()


@router.get("/mbids", response_model=LibraryMbidsResponse)
async def get_target_provider_ids(
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
    request_history: RequestHistoryStoreDep,
) -> LibraryMbidsResponse:
    provider_ids, requested_ids = await asyncio.gather(
        service.provider_ids(),
        request_history.async_get_requested_mbids(request_kind="album"),
    )
    return LibraryMbidsResponse(
        mbids=provider_ids.musicbrainz_release_group_ids,
        requested_mbids=sorted(requested_ids),
    )


@router.post("/membership", response_model=LibraryMembershipResponse)
async def get_target_membership(
    _user: CurrentUserDep,
    ownership: TargetLibraryOwnershipServiceDep,
    request_history: RequestHistoryStoreDep,
    body: LibraryMembershipRequest = MsgSpecBody(LibraryMembershipRequest),
) -> LibraryMembershipResponse:
    album_ids = list(
        dict.fromkeys(
            value.strip().casefold() for value in body.album_ids if value.strip()
        )
    )
    if len(album_ids) > 500:
        raise ValidationError("Library membership accepts at most 500 album IDs.")
    owned, requested = await asyncio.gather(
        ownership.existing_provider_album_ids(album_ids),
        request_history.async_existing_requested_mbids(album_ids),
    )
    return LibraryMembershipResponse(
        owned_ids=sorted(owned), requested_ids=sorted(requested)
    )


@router.get("/recently-added", response_model=TargetNativeAlbumsResponse)
async def get_target_recently_added(
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
    limit: int = Query(default=20, ge=1, le=100),
) -> TargetNativeAlbumsResponse:
    items = await service.recently_added(limit)
    return TargetNativeAlbumsResponse(items=items, total=len(items))


@router.get(
    "/artists/{artist_id}",
    response_model=TargetNativeArtist,
    name="target_artist_detail",
)
async def get_target_artist(
    request: Request,
    artist_id: str,
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
) -> TargetNativeArtist | RedirectResponse:
    canonical = await service.canonical_id("artist", artist_id)
    if canonical is not None and canonical != artist_id:
        return RedirectResponse(
            _path_relative_location(
                request.url_for("target_artist_detail", artist_id=canonical)
            ),
            status_code=308,
        )
    artist = await service.artist(artist_id)
    if artist is None:
        raise ResourceNotFoundError("Library artist not found.")
    return artist


@router.get(
    "/artists/{artist_id}/albums",
    response_model=TargetNativeAlbumsResponse,
    name="target_artist_albums",
)
async def get_target_artist_albums(
    request: Request,
    artist_id: str,
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
) -> TargetNativeAlbumsResponse | RedirectResponse:
    canonical = await service.canonical_id("artist", artist_id)
    if canonical is not None and canonical != artist_id:
        return RedirectResponse(
            _path_relative_location(
                request.url_for("target_artist_albums", artist_id=canonical)
            ),
            status_code=308,
        )
    items = await service.artist_albums(artist_id)
    return TargetNativeAlbumsResponse(items=items, total=len(items))


@router.get(
    "/artists/{artist_id}/appearances",
    response_model=TargetNativeArtistAppearancesResponse,
    name="target_artist_appearances",
)
async def get_target_artist_appearances(
    request: Request,
    artist_id: str,
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
    limit: int = Query(default=20, ge=1, le=100),
    offset: int = Query(default=0, ge=0),
) -> TargetNativeArtistAppearancesResponse | RedirectResponse:
    canonical = await service.canonical_id("artist", artist_id)
    if canonical is not None and canonical != artist_id:
        target = request.url_for(
            "target_artist_appearances", artist_id=canonical
        ).include_query_params(limit=limit, offset=offset)
        return RedirectResponse(
            _path_relative_location(target),
            status_code=308,
        )
    items, total, total_tracks = await service.artist_appearances(
        artist_id, limit=limit, offset=offset
    )
    return TargetNativeArtistAppearancesResponse(
        items=items,
        total=total,
        total_tracks=total_tracks,
        offset=offset,
        limit=limit,
    )


@router.get(
    "/albums/{album_id}",
    response_model=TargetNativeAlbumDetail,
    name="target_album_detail",
)
async def get_target_album(
    request: Request,
    album_id: str,
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
) -> TargetNativeAlbumDetail | RedirectResponse:
    canonical = await service.canonical_id("album", album_id)
    if canonical is not None and canonical != album_id:
        return RedirectResponse(
            _path_relative_location(
                request.url_for("target_album_detail", album_id=canonical)
            ),
            status_code=308,
        )
    album = await service.album_detail(album_id)
    if album is None:
        raise ResourceNotFoundError("Library album not found.")
    return album


@router.post(
    "/albums/{album_id}/management/re-enable",
    response_model=ManagementReenableResponse,
)
async def reenable_target_album_management(
    album_id: str,
    admin: CurrentAdminDep,
    service: TargetNativeLibraryServiceDep,
    body: ManagementReenableRequest = MsgSpecBody(ManagementReenableRequest),
) -> ManagementReenableResponse:
    reenabled = await service.reenable_album_management(
        album_id,
        expected_exclusion_revision=body.expected_exclusion_revision,
        actor_user_id=admin.id,
        now=time.time(),
    )
    return ManagementReenableResponse(reenabled=reenabled)


@router.post(
    "/albums/{album_id}/edition-conversions/preflight",
    response_model=EditionConversionStatusResponse,
)
async def create_edition_conversion_preflight(
    album_id: str,
    admin: CurrentAdminDep,
    service: EditionConversionServiceDep,
    body: EditionConversionPreflightRequest = MsgSpecBody(
        EditionConversionPreflightRequest
    ),
) -> EditionConversionStatusResponse:
    return await service.create_preflight(
        local_album_id=album_id,
        release_group_mbid=body.release_group_mbid,
        release_mbid=body.release_mbid,
        actor_user_id=admin.id,
    )


@router.post(
    "/edition-conversions/{job_id}/start",
    response_model=EditionConversionStatusResponse,
)
async def start_edition_conversion(
    job_id: str,
    _admin: CurrentAdminDep,
    service: EditionConversionServiceDep,
    body: EditionConversionStartRequest = MsgSpecBody(EditionConversionStartRequest),
) -> EditionConversionStatusResponse:
    return await service.start(
        job_id,
        preflight_token=body.preflight_token,
        expected_row_revision=body.expected_row_revision,
        confirmation=body.confirmation,
    )


@router.get(
    "/edition-conversions/{job_id}",
    response_model=EditionConversionStatusResponse,
)
async def get_edition_conversion(
    job_id: str,
    _admin: CurrentAdminDep,
    service: EditionConversionServiceDep,
) -> EditionConversionStatusResponse:
    return await service.status(job_id)


@router.post(
    "/edition-conversions/{job_id}/preview",
    response_model=EditionConversionPreviewResponse,
)
async def create_edition_conversion_preview(
    job_id: str,
    _admin: CurrentAdminDep,
    service: EditionConversionServiceDep,
    body: EditionConversionPreviewRequest = MsgSpecBody(
        EditionConversionPreviewRequest
    ),
) -> EditionConversionPreviewResponse:
    return await service.create_final_preview(
        job_id, expected_row_revision=body.expected_row_revision
    )


@router.post(
    "/edition-conversions/{job_id}/retry",
    response_model=EditionConversionStatusResponse,
)
async def retry_edition_conversion(
    job_id: str,
    _admin: CurrentAdminDep,
    service: EditionConversionServiceDep,
    body: EditionConversionRetryRequest = MsgSpecBody(EditionConversionRetryRequest),
) -> EditionConversionStatusResponse:
    return await service.retry(
        job_id,
        target_ordinals=body.target_ordinals,
        expected_row_revision=body.expected_row_revision,
    )


@router.post(
    "/edition-conversions/{job_id}/recheck",
    response_model=EditionConversionStatusResponse,
)
async def recheck_edition_conversion(
    job_id: str,
    _admin: CurrentAdminDep,
    service: EditionConversionServiceDep,
    body: EditionConversionRecheckRequest = MsgSpecBody(
        EditionConversionRecheckRequest
    ),
) -> EditionConversionStatusResponse:
    return await service.recheck(
        job_id, expected_row_revision=body.expected_row_revision
    )


@router.post(
    "/edition-conversions/{job_id}/cancel",
    response_model=EditionConversionStatusResponse,
)
async def cancel_edition_conversion(
    job_id: str,
    _admin: CurrentAdminDep,
    service: EditionConversionServiceDep,
    body: EditionConversionCancelRequest = MsgSpecBody(EditionConversionCancelRequest),
) -> EditionConversionStatusResponse:
    return await service.cancel(
        job_id,
        expected_row_revision=body.expected_row_revision,
        confirmation=body.confirmation,
    )


@router.get(
    "/albums/{album_id}/reidentification/releases",
    response_model=ReleaseEditionSearchResponse,
)
async def search_reidentification_releases(
    album_id: str,
    _admin: CurrentAdminDep,
    service: TargetAlbumEditionFinderServiceDep,
    title: str = Query(min_length=1, max_length=250),
    artist: str = Query(default="", max_length=250),
    limit: int = Query(default=12, ge=1, le=12),
    offset: int = Query(default=0, ge=0),
) -> ReleaseEditionSearchResponse:
    (
        title_query,
        artist_query,
        current_release_group_mbid,
        current_release_mbid,
        page,
    ) = await service.search(
        album_id, title=title, artist=artist, limit=limit, offset=offset
    )
    return ReleaseEditionSearchResponse(
        title_query=title_query,
        artist_query=artist_query,
        items=[
            ReleaseEditionResult(
                **{field: getattr(item, field) for field in item.__struct_fields__},
                belongs_to_current_release_group=(
                    current_release_group_mbid is not None
                    and item.release_group_mbid.casefold()
                    == current_release_group_mbid.casefold()
                ),
                is_current_release=(
                    current_release_mbid is not None
                    and item.release_mbid.casefold() == current_release_mbid.casefold()
                ),
            )
            for item in page.items
        ],
        total=page.total,
        offset=page.offset,
        limit=page.limit,
    )


@router.get(
    "/albums/{album_id}/copies",
    response_model=TargetNativeAlbumsResponse,
)
async def get_target_album_copies(
    album_id: str,
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
) -> TargetNativeAlbumsResponse:
    items = await service.album_copies(album_id)
    return TargetNativeAlbumsResponse(items=items, total=len(items))


@router.post("/resolve-tracks", response_model=TrackResolveResponse)
async def resolve_target_tracks(
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
    body: TrackResolveRequest = MsgSpecBody(TrackResolveRequest),
) -> TrackResolveResponse:
    return await service.resolve_tracks(body.items)


@router.get(
    "/albums/{album_id}/tracks",
    response_model=TargetNativeTracksResponse,
    name="target_album_tracks",
)
async def get_target_album_tracks(
    request: Request,
    album_id: str,
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
) -> TargetNativeTracksResponse | RedirectResponse:
    canonical = await service.canonical_id("album", album_id)
    if canonical is not None and canonical != album_id:
        return RedirectResponse(
            _path_relative_location(
                request.url_for("target_album_tracks", album_id=canonical)
            ),
            status_code=308,
        )
    items = await service.album_tracks(album_id)
    return TargetNativeTracksResponse(
        items=items, total=len(items), offset=0, limit=len(items)
    )


@router.get(
    "/albums/{album_id}/status",
    response_model=TargetNativeAlbumStatusResponse,
    name="target_album_status",
)
async def get_target_album_status(
    request: Request,
    album_id: str,
    _user: CurrentUserDep,
    service: TargetNativeLibraryServiceDep,
    preferences: PreferencesServiceDep,
) -> TargetNativeAlbumStatusResponse | RedirectResponse:
    canonical = await service.canonical_id("album", album_id)
    if canonical is not None and canonical != album_id:
        return RedirectResponse(
            _path_relative_location(
                request.url_for("target_album_status", album_id=canonical)
            ),
            status_code=308,
        )
    policy = preferences.get_download_policy()
    return await service.album_status(
        album_id,
        quality_cutoff=policy.quality_cutoff,
        upgrade_allowed=policy.upgrade_allowed,
    )


@router.delete("/album/{album_id}", response_model=TargetCatalogRemovalResponse)
async def remove_target_album(
    album_id: str,
    admin: CurrentAdminDep,
    writer: TargetCatalogWriterServiceDep,
    wanted: WantedWatcherServiceDep,
    delete_files: bool = False,
    stop_wanted: bool = True,
    download_service=Depends(get_download_service),
) -> TargetCatalogRemovalResponse:
    release_group_mbid = await writer.provider_release_group_id(album_id)
    removed = await writer.remove_album(
        album_id, actor_user_id=admin.id, delete_files=delete_files
    )
    cleanup_id = release_group_mbid or album_id
    try:
        await download_service.purge_album_downloads(cleanup_id)
    except Exception:  # noqa: BLE001 - removal already succeeded
        logger.warning("Target album removal download cleanup failed")
    if release_group_mbid:
        try:
            if stop_wanted:
                await wanted.stop_after_library_removal(release_group_mbid)
            else:
                await wanted.continue_after_library_removal(release_group_mbid)
        except Exception:  # noqa: BLE001 - removal already succeeded
            logger.warning("Target album removal wanted-state cleanup failed")
    return TargetCatalogRemovalResponse(
        success=True, id=album_id, removed_track_ids=removed
    )


@router.delete("/tracks/{track_id}", response_model=TargetCatalogRemovalResponse)
async def remove_target_track(
    track_id: str,
    curator: CurrentCuratorDep,
    writer: TargetCatalogWriterServiceDep,
) -> TargetCatalogRemovalResponse:
    removed = await writer.remove_track(track_id, actor_user_id=curator.id)
    return TargetCatalogRemovalResponse(
        success=True, id=track_id, removed_track_ids=removed
    )


@router.get("/tracks/{track_id}/tags", response_model=AudioTag)
async def get_target_track_tags(
    track_id: str,
    _admin: CurrentAdminDep,
    writer: TargetCatalogWriterServiceDep,
) -> AudioTag:
    return await writer.read_tags(track_id)


@router.post(
    "/albums/{album_id}/rescan",
    response_model=ScanRunRequestedResponse,
    status_code=202,
)
async def rescan_target_album(
    album_id: str,
    admin: CurrentAdminDep,
    service: TargetNativeLibraryServiceDep,
    coordinator: TargetLibraryScanCoordinatorDep,
    resolver: LibraryPolicyResolverDep,
) -> ScanRunRequestedResponse:
    scopes = await service.album_rescan_scopes(album_id, resolver)
    if not scopes:
        raise ResourceNotFoundError("Library album not found.")
    result = await coordinator.request_run(
        ScanRequest(
            kind="rescan_files",
            trigger="manual",
            scopes=scopes,
            requested_by_user_id=admin.id,
            policy_revision=resolver.policy_revision,
        )
    )
    return ScanRunRequestedResponse(
        run_id=result.run_id,
        disposition=result.disposition,
        state=result.state,
        row_revision=result.row_revision,
        queued_reason=result.queued_reason,
        conflicting_kind=result.conflicting_kind,
    )
