from fastapi import APIRouter, Depends, HTTPException, Query
from api.v1.schemas.discover import (
    DiscoverResponse,
    DiscoverQueueResponse,
    DiscoverQueueEnrichment,
    DiscoverIgnoredRelease,
    DiscoverQueueIgnoreRequest,
    DiscoverQueueValidateRequest,
    DiscoverQueueValidateResponse,
    DiscoverQueueStatusResponse,
    AlbumPreviewResponse,
    PreviewTrackItem,
    QueueGenerateRequest,
    RadioPlanRequest,
    RadioPlanResponse,
    QueueGenerateResponse,
    RadioRequest,
    PlaylistSuggestionsRequest,
    PlaylistSuggestionsResponse,
    TrackPreviewResponse,
    YouTubeSearchResponse,
    YouTubeQuotaResponse,
    TrackCacheCheckRequest,
    TrackCacheCheckResponse,
    TrackCacheCheckResponseItem,
)
from api.v1.schemas.common import StatusMessageResponse
from api.v1.schemas.home import HomeSection
from core.dependencies import (
    get_discover_service,
    get_discover_queue_manager,
    get_preview_repository,
    get_user_section_prefs_store,
    get_youtube_repo,
)
from core.dependencies.type_aliases import CurrentUserDep
from infrastructure.degradation import try_get_degradation_context
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute

import msgspec.structs
from infrastructure.persistence.user_section_prefs_store import UserSectionPrefsStore
from repositories.preview_repository import PreviewRepository
from repositories.youtube import YouTubeRepository
from services.discover_service import DiscoverService
from services.discover_queue_manager import DiscoverQueueManager
from services.section_catalog import apply_section_prefs

router = APIRouter(route_class=MsgSpecRoute, prefix="/discover", tags=["discover"])


@router.get("", response_model=DiscoverResponse)
async def get_discover_data(
    current_user: CurrentUserDep,
    discover_service: DiscoverService = Depends(get_discover_service),
    section_prefs: UserSectionPrefsStore = Depends(get_user_section_prefs_store),
):
    result = await discover_service.get_discover_data(user_id=current_user.id)
    disabled = await section_prefs.get_disabled(current_user.id, "discover")
    result = apply_section_prefs(result, "discover", disabled)
    ctx = try_get_degradation_context()
    if ctx is not None and ctx.has_degradation():
        # merge over the build-time summary the cached copy carries (in-request wins)
        merged = {**(result.service_status or {}), **ctx.degraded_summary()}
        result = msgspec.structs.replace(result, service_status=merged)
    return result


@router.post("/refresh", response_model=StatusMessageResponse)
async def refresh_discover_data(
    current_user: CurrentUserDep,
    discover_service: DiscoverService = Depends(get_discover_service),
):
    await discover_service.refresh_discover_data(current_user.id)
    return StatusMessageResponse(status="ok", message="Discover refresh triggered")


@router.post("/radio", response_model=HomeSection)
async def discover_radio(
    body: RadioRequest = MsgSpecBody(RadioRequest),
    service: DiscoverService = Depends(get_discover_service),
) -> HomeSection:
    return await service.generate_radio(body)


@router.post("/radio/plan", response_model=RadioPlanResponse)
async def discover_radio_plan(
    current_user: CurrentUserDep,
    body: RadioPlanRequest = MsgSpecBody(RadioPlanRequest),
    service: DiscoverService = Depends(get_discover_service),
) -> RadioPlanResponse:
    """Track-level radio plan: the frontend resolves playback per track
    (library -> native stream, un-owned -> YouTube / 30s previews)."""
    return await service.build_radio_plan(current_user.id, body)


@router.post("/playlist-suggestions", response_model=PlaylistSuggestionsResponse)
async def playlist_suggestions(
    current_user: CurrentUserDep,
    body: PlaylistSuggestionsRequest = MsgSpecBody(PlaylistSuggestionsRequest),
    service: DiscoverService = Depends(get_discover_service),
) -> PlaylistSuggestionsResponse:
    return await service.get_playlist_suggestions(body, current_user)


@router.get("/queue", response_model=DiscoverQueueResponse)
async def get_discover_queue(
    current_user: CurrentUserDep,
    count: int | None = Query(default=None, description="Number of items (default from settings, max 20)"),
    queue_manager: DiscoverQueueManager = Depends(get_discover_queue_manager),
):
    cached = await queue_manager.consume_queue(current_user.id)
    if cached:
        return cached
    effective_count = min(count, 20) if count is not None else None
    return await queue_manager.build_hydrated_queue(current_user.id, effective_count)


@router.get("/queue/status", response_model=DiscoverQueueStatusResponse)
async def get_queue_status(
    current_user: CurrentUserDep,
    queue_manager: DiscoverQueueManager = Depends(get_discover_queue_manager),
):
    await queue_manager.ensure_loaded(current_user.id)
    return queue_manager.get_status(current_user.id)


@router.post("/queue/generate", response_model=QueueGenerateResponse)
async def generate_queue(
    current_user: CurrentUserDep,
    body: QueueGenerateRequest = MsgSpecBody(QueueGenerateRequest),
    queue_manager: DiscoverQueueManager = Depends(get_discover_queue_manager),
):
    return await queue_manager.start_build(current_user.id, force=body.force)


@router.get("/queue/enrich/{release_group_mbid}", response_model=DiscoverQueueEnrichment)
async def enrich_queue_item(
    release_group_mbid: str,
    discover_service: DiscoverService = Depends(get_discover_service),
):
    return await discover_service.enrich_queue_item(release_group_mbid)


@router.post("/queue/ignore", status_code=204)
async def ignore_queue_item(
    current_user: CurrentUserDep,
    body: DiscoverQueueIgnoreRequest = MsgSpecBody(DiscoverQueueIgnoreRequest),
    discover_service: DiscoverService = Depends(get_discover_service),
    queue_manager: DiscoverQueueManager = Depends(get_discover_queue_manager),
):
    await discover_service.ignore_release(
        current_user.id, body.release_group_mbid, body.artist_mbid, body.release_name, body.artist_name
    )
    await queue_manager.start_build(current_user.id, force=True)
    await discover_service.refresh_discover_data(current_user.id)


@router.get("/queue/ignored", response_model=list[DiscoverIgnoredRelease])
async def get_ignored_items(
    current_user: CurrentUserDep,
    discover_service: DiscoverService = Depends(get_discover_service),
):
    return await discover_service.get_ignored_releases(current_user.id)


@router.post("/queue/validate", response_model=DiscoverQueueValidateResponse)
async def validate_queue(
    body: DiscoverQueueValidateRequest = MsgSpecBody(DiscoverQueueValidateRequest),
    discover_service: DiscoverService = Depends(get_discover_service),
):
    in_library = await discover_service.validate_queue_mbids(body.release_group_mbids)
    return DiscoverQueueValidateResponse(in_library=in_library)


@router.get("/queue/youtube-search", response_model=YouTubeSearchResponse)
async def youtube_search(
    artist: str = Query(..., description="Artist name"),
    album: str = Query(..., description="Album name"),
    yt_repo: YouTubeRepository = Depends(get_youtube_repo),
):
    if not yt_repo or not yt_repo.is_configured:
        return YouTubeSearchResponse(error="not_configured")

    if yt_repo.quota_remaining <= 0 and not yt_repo.is_cached(artist, album):
        return YouTubeSearchResponse(error="quota_exceeded")

    was_cached = yt_repo.is_cached(artist, album)
    video_id = await yt_repo.search_video(artist, album)
    if video_id:
        return YouTubeSearchResponse(
            video_id=video_id,
            embed_url=f"https://www.youtube.com/embed/{video_id}",
            cached=was_cached,
        )
    return YouTubeSearchResponse(error="not_found")


@router.get("/queue/youtube-track-search", response_model=YouTubeSearchResponse)
async def youtube_track_search(
    artist: str = Query(..., description="Artist name"),
    track: str = Query(..., description="Track name"),
    yt_repo: YouTubeRepository = Depends(get_youtube_repo),
):
    if not yt_repo or not yt_repo.is_configured:
        return YouTubeSearchResponse(error="not_configured")

    if yt_repo.quota_remaining <= 0 and not yt_repo.is_cached(artist, track):
        return YouTubeSearchResponse(error="quota_exceeded")

    was_cached = yt_repo.is_cached(artist, track)
    video_id = await yt_repo.search_track(artist, track)
    if video_id:
        return YouTubeSearchResponse(
            video_id=video_id,
            embed_url=f"https://www.youtube.com/embed/{video_id}",
            cached=was_cached,
        )
    return YouTubeSearchResponse(error="not_found")


@router.get("/queue/youtube-quota", response_model=YouTubeQuotaResponse)
async def youtube_quota(
    yt_repo: YouTubeRepository = Depends(get_youtube_repo),
):
    if not yt_repo or not yt_repo.is_configured:
        raise HTTPException(status_code=404, detail="YouTube not configured")
    return yt_repo.get_quota_status()


CACHE_CHECK_MAX_ITEMS = 100
CACHE_CHECK_MAX_STR_LEN = 200


@router.post("/queue/youtube-cache-check", response_model=TrackCacheCheckResponse)
async def youtube_cache_check(
    body: TrackCacheCheckRequest = MsgSpecBody(TrackCacheCheckRequest),
    yt_repo: YouTubeRepository = Depends(get_youtube_repo),
):
    if not yt_repo or not yt_repo.is_configured:
        return TrackCacheCheckResponse()

    seen: set[str] = set()
    deduped: list[tuple[str, str]] = []
    for item in body.items[:CACHE_CHECK_MAX_ITEMS]:
        artist = item.artist[:CACHE_CHECK_MAX_STR_LEN]
        track = item.track[:CACHE_CHECK_MAX_STR_LEN]
        key = f"{artist.lower()}|{track.lower()}"
        if key not in seen:
            seen.add(key)
            deduped.append((artist, track))

    cache_results = yt_repo.are_cached(deduped)
    return TrackCacheCheckResponse(
        items=[
            TrackCacheCheckResponseItem(
                artist=artist,
                track=track,
                cached=cache_results.get(f"{artist.lower()}|{track.lower()}", False),
            )
            for artist, track in deduped
        ]
    )


_PREVIEW_PARAM_MAX_LEN = 200


@router.get("/track-preview", response_model=TrackPreviewResponse)
async def track_preview(
    current_user: CurrentUserDep,
    artist: str = Query(..., min_length=1, description="Artist name"),
    track: str = Query(..., min_length=1, description="Track name"),
    preview_repo: PreviewRepository = Depends(get_preview_repository),
):
    """A keyless 30s audio preview (Deezer -> iTunes). Preview URLs expire, so
    responses are resolved just-in-time and must not be long-cached."""
    found, provider = await preview_repo.get_track_preview(
        artist[:_PREVIEW_PARAM_MAX_LEN], track[:_PREVIEW_PARAM_MAX_LEN]
    )
    if not found:
        return TrackPreviewResponse()
    return TrackPreviewResponse(
        preview_url=found.preview_url,
        title=found.title,
        duration_s=found.duration_s,
        provider=provider,
    )


@router.get("/album-preview", response_model=AlbumPreviewResponse)
async def album_preview(
    current_user: CurrentUserDep,
    artist: str = Query(..., min_length=1, description="Artist name"),
    album: str = Query(..., min_length=1, description="Album name"),
    count: int = Query(default=4, ge=1, le=8),
    preview_repo: PreviewRepository = Depends(get_preview_repository),
):
    """Ordered 30s samples of an album's first tracks (the deck's "Sample album")."""
    tracks, provider = await preview_repo.get_album_preview_tracks(
        artist[:_PREVIEW_PARAM_MAX_LEN], album[:_PREVIEW_PARAM_MAX_LEN], limit=count
    )
    return AlbumPreviewResponse(
        tracks=[
            PreviewTrackItem(
                title=t.title,
                artist_name=t.artist_name,
                preview_url=t.preview_url,
                duration_s=t.duration_s,
                position=t.position,
            )
            for t in tracks
        ],
        provider=provider,
    )
