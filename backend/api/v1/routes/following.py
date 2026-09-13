from fastapi import APIRouter, Depends, Query

from api.v1.schemas.following import (
    CitySearchResponse,
    CitySearchResult,
    ConcertResponse,
    ConcertsResponse,
    EventCitiesResponse,
    EventCitiesUpdateRequest,
    EventCityPayload,
    FollowedArtistResponse,
    NewReleaseResponse,
    UnseenCountResponse,
    WantedResponse,
)
from core.dependencies import (
    get_events_service,
    get_follow_service,
    get_geocoding_repository,
)
from core.dependencies.type_aliases import CurrentUserDep
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from models.events import EventCity
from repositories.geocoding_repository import GeocodingRepository
from services.events_service import EventsService
from services.follow_service import FollowService

router = APIRouter(route_class=MsgSpecRoute, prefix="/following", tags=["following"])


@router.get("/artists", response_model=list[FollowedArtistResponse])
async def list_followed_artists(
    current_user: CurrentUserDep,
    follow_service: FollowService = Depends(get_follow_service),
):
    artists = await follow_service.list_following(current_user.id, current_user.role)
    return [
        FollowedArtistResponse(
            mbid=a.artist_mbid,
            name=a.artist_name,
            auto_download=a.auto_download,
            auto_download_state=a.auto_download_state,
            followed_at=a.followed_at,
        )
        for a in artists
    ]


@router.get("/new-releases", response_model=WantedResponse)
async def list_new_releases(
    current_user: CurrentUserDep,
    limit: int = Query(50, ge=1, le=100),
    offset: int = Query(0, ge=0),
    follow_service: FollowService = Depends(get_follow_service),
):
    items, total = await follow_service.list_new_releases(current_user.id, limit, offset)
    return WantedResponse(
        items=[
            NewReleaseResponse(
                release_group_mbid=item.release_group_mbid,
                title=item.title,
                artist_name=item.artist_name,
                artist_mbid=item.artist_mbid,
                primary_type=item.primary_type,
                first_release_date=item.first_release_date,
            )
            for item in items
        ],
        total=total,
    )


@router.get("/new-releases/recent", response_model=WantedResponse)
async def list_recent_releases(
    current_user: CurrentUserDep,
    days: int = Query(30, ge=1, le=365),
    # headroom for the page's load-more growth (48 per click, client caps at 480)
    limit: int = Query(8, ge=1, le=500),
    include_owned: bool = Query(True),
    follow_service: FollowService = Depends(get_follow_service),
):
    """The release LOG (hub + New Releases page): everything the user's artists
    put out in the window, albums already in the library flagged in_library=True.
    include_owned=false is the page's 'hide owned' filter - that variant matches
    the plain GET /new-releases to-do view, windowed."""
    items, total = await follow_service.list_recent_releases(
        current_user.id, days, limit, include_owned
    )
    return WantedResponse(
        items=[
            NewReleaseResponse(
                release_group_mbid=item.release_group_mbid,
                title=item.title,
                artist_name=item.artist_name,
                artist_mbid=item.artist_mbid,
                primary_type=item.primary_type,
                first_release_date=item.first_release_date,
                in_library=item.in_library,
            )
            for item in items
        ],
        total=total,
    )


@router.get("/new-releases/unseen-count", response_model=UnseenCountResponse)
async def get_unseen_new_release_count(
    current_user: CurrentUserDep,
    follow_service: FollowService = Depends(get_follow_service),
):
    count = await follow_service.count_unseen_new_releases(current_user.id)
    return UnseenCountResponse(count=count)


@router.post("/new-releases/seen", response_model=UnseenCountResponse)
async def mark_new_releases_seen(
    current_user: CurrentUserDep,
    follow_service: FollowService = Depends(get_follow_service),
):
    """Stamp the user's seen marker; the unseen count is 0 by definition after."""
    await follow_service.mark_new_releases_seen(current_user.id)
    return UnseenCountResponse(count=0)


def _to_concert_response(match) -> ConcertResponse:
    event = match.event
    return ConcertResponse(
        artist_mbid=match.artist_mbid,
        artist_name=event.artist_name,
        event_name=event.event_name,
        local_date=event.local_date,
        status=event.status,
        source=event.source,
        source_event_id=event.source_event_id,
        matched_city=match.matched_city,
        venue_name=event.venue_name,
        city=event.city,
        region=event.region,
        country_code=event.country_code,
        starts_at=event.starts_at,
        ticket_url=event.ticket_url,
        distance_km=match.distance_km,
    )


@router.get("/concerts", response_model=ConcertsResponse)
async def list_concerts(
    current_user: CurrentUserDep,
    events_service: EventsService = Depends(get_events_service),
):
    """Upcoming concerts for the user's followed artists in their saved cities,
    date ascending. ``configured=False`` = no enabled events source."""
    matches = await events_service.list_concerts(current_user.id)
    return ConcertsResponse(
        configured=events_service.is_configured(),
        items=[_to_concert_response(m) for m in matches],
        total=len(matches),
    )


@router.get("/concerts/cities", response_model=EventCitiesResponse)
async def list_event_cities(
    current_user: CurrentUserDep,
    events_service: EventsService = Depends(get_events_service),
):
    cities = await events_service.list_cities(current_user.id)
    return EventCitiesResponse(
        items=[
            EventCityPayload(
                city_name=c.city_name,
                latitude=c.latitude,
                longitude=c.longitude,
                radius_km=c.radius_km,
                country_code=c.country_code,
            )
            for c in cities
        ]
    )


@router.put("/concerts/cities", response_model=EventCitiesResponse)
async def replace_event_cities(
    current_user: CurrentUserDep,
    payload: EventCitiesUpdateRequest = MsgSpecBody(EventCitiesUpdateRequest),
    events_service: EventsService = Depends(get_events_service),
):
    """Replace-all semantics: the picker submits its full city list in order."""
    cities = await events_service.replace_cities(
        current_user.id,
        [
            EventCity(
                city_name=item.city_name.strip(),
                latitude=item.latitude,
                longitude=item.longitude,
                radius_km=item.radius_km,
                position=index,
                country_code=item.country_code,
            )
            for index, item in enumerate(payload.items)
            if item.city_name.strip()
        ],
    )
    return EventCitiesResponse(
        items=[
            EventCityPayload(
                city_name=c.city_name,
                latitude=c.latitude,
                longitude=c.longitude,
                radius_km=c.radius_km,
                country_code=c.country_code,
            )
            for c in cities
        ]
    )


@router.get("/concerts/city-search", response_model=CitySearchResponse)
async def search_cities(
    current_user: CurrentUserDep,
    q: str = Query(..., min_length=2, max_length=100),
    geocoding_repo: GeocodingRepository = Depends(get_geocoding_repository),
):
    """Backend proxy to Open-Meteo. A provider failure surfaces as 503
    (GeocodingApiError) - an empty list strictly means 'no matching city'."""
    results = await geocoding_repo.search_cities(q)
    return CitySearchResponse(
        items=[
            CitySearchResult(
                name=r.name,
                latitude=r.latitude,
                longitude=r.longitude,
                country_code=r.country_code,
                country=r.country,
                region=r.admin1,
            )
            for r in results
            if r.name
        ]
    )


@router.get("/concerts/unseen-count", response_model=UnseenCountResponse)
async def get_unseen_concert_count(
    current_user: CurrentUserDep,
    events_service: EventsService = Depends(get_events_service),
):
    count = await events_service.count_unseen(current_user.id)
    return UnseenCountResponse(count=count)


@router.post("/concerts/seen", response_model=UnseenCountResponse)
async def mark_concerts_seen(
    current_user: CurrentUserDep,
    events_service: EventsService = Depends(get_events_service),
):
    """Stamp the user's seen marker; the unseen count is 0 by definition after."""
    await events_service.mark_seen(current_user.id)
    return UnseenCountResponse(count=0)
