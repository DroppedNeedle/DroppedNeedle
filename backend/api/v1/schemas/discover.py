from typing import Literal

from api.v1.schemas.home import GenreArtwork, HomeAlbum, HomeSection, ServicePrompt
from api.v1.schemas.common import IntegrationStatus
from api.v1.schemas.weekly_exploration import WeeklyExplorationSection
from models.youtube import YouTubeQuotaResponse as YouTubeQuotaResponse
from infrastructure.msgspec_fastapi import AppStruct


class BecauseYouListenTo(AppStruct):
    seed_artist: str
    seed_artist_mbid: str
    section: HomeSection
    listen_count: int = 0
    banner_url: str | None = None
    wide_thumb_url: str | None = None
    fanart_url: str | None = None


class DiscoverQueueItemLight(AppStruct):
    release_group_mbid: str
    album_name: str
    artist_name: str
    artist_mbid: str
    recommendation_reason: str
    cover_url: str | None = None
    is_wildcard: bool = False
    in_library: bool = False


class DiscoverQueueEnrichment(AppStruct):
    artist_mbid: str | None = None
    release_date: str | None = None
    country: str | None = None
    tags: list[str] = []
    youtube_url: str | None = None
    youtube_search_url: str = ""
    youtube_search_available: bool = False
    artist_description: str | None = None
    listen_count: int | None = None


class DiscoverQueueItemFull(DiscoverQueueItemLight):
    enrichment: DiscoverQueueEnrichment | None = None


class YouTubeSearchResponse(AppStruct):
    video_id: str | None = None
    embed_url: str | None = None
    error: str | None = None
    cached: bool = False


class TrackCacheCheckItem(AppStruct):
    artist: str
    track: str


class TrackCacheCheckRequest(AppStruct):
    items: list[TrackCacheCheckItem] = []


class TrackCacheCheckResponseItem(AppStruct):
    artist: str
    track: str
    cached: bool = False


class TrackCacheCheckResponse(AppStruct):
    items: list[TrackCacheCheckResponseItem] = []


class DiscoverQueueResponse(AppStruct):
    items: list[DiscoverQueueItemLight | DiscoverQueueItemFull] = []
    queue_id: str = ""


class DiscoverQueueIgnoreRequest(AppStruct):
    release_group_mbid: str
    artist_mbid: str
    release_name: str
    artist_name: str


class DiscoverQueueValidateRequest(AppStruct):
    release_group_mbids: list[str]


class DiscoverQueueValidateResponse(AppStruct):
    in_library: list[str] = []


class QueueSettings(AppStruct):
    queue_size: int
    queue_ttl: int
    seed_artists: int
    wildcard_slots: int
    similar_artists_limit: int
    albums_per_similar: int
    enrich_ttl: int
    lastfm_mbid_max_lookups: int


class DiscoverQueueStatusResponse(AppStruct):
    status: str
    queue_id: str | None = None
    item_count: int | None = None
    built_at: float | None = None
    stale: bool | None = None
    error: str | None = None


class QueueGenerateRequest(AppStruct):
    force: bool = False


class QueueGenerateResponse(AppStruct):
    action: str
    status: str
    queue_id: str | None = None
    item_count: int | None = None
    built_at: float | None = None
    stale: bool | None = None
    error: str | None = None


class DiscoverIgnoredRelease(AppStruct):
    release_group_mbid: str
    artist_mbid: str
    release_name: str
    artist_name: str
    ignored_at: float


class RadioSeedItem(AppStruct):
    artist_mbid: str
    artist_name: str = ""
    album_mbid: str | None = None
    album_name: str = ""


class RadioPlanRequest(AppStruct):
    seed_type: Literal["artist", "album", "genre", "items"]
    seed_id: str | None = None
    items: list[RadioSeedItem] = []
    mode: Literal["library", "hybrid"] = "hybrid"
    count: int = 30
    exclude_recording_mbids: list[str] = []
    # Accepted for compatibility with the former two-stage client. Plans are complete.
    fast: bool = False


class RadioPlanTrack(AppStruct):
    track_name: str
    artist_name: str
    artist_mbid: str = ""
    recording_mbid: str | None = None
    album_mbid: str | None = None
    album_name: str | None = None
    in_library: bool = False
    # library tracks resolve straight to the native stream endpoint
    local_file_id: str | None = None
    file_format: str | None = None
    duration_s: float | None = None


class RadioPlanResponse(AppStruct):
    title: str
    tracks: list[RadioPlanTrack] = []


class RadioRequest(AppStruct):
    seed_type: Literal["artist", "album", "genre"]
    seed_id: str
    count: int = 10
    source: Literal["listenbrainz", "lastfm"] | None = None


class PlaylistProfile(AppStruct):
    artist_mbids: list[str] = []
    genre_distribution: dict[str, list[str]] = {}
    track_count: int = 0


class PlaylistSuggestionsRequest(AppStruct):
    playlist_id: str
    count: int = 10
    source: Literal["listenbrainz", "lastfm"] | None = None


class PlaylistSuggestionsResponse(AppStruct):
    suggestions: HomeSection
    playlist_id: str
    profile: PlaylistProfile


class TopPickItem(AppStruct):
    album: HomeAlbum
    match_pct: int
    reasons: list[str] = []
    seed_artist: str | None = None


class TopPicksSection(AppStruct):
    title: str = "Top Picks for You"
    items: list[TopPickItem] = []
    source: str | None = None
    # True while the picks are still trending-only because the personalisation
    # (Last.fm -> MusicBrainz) hasn't resolved yet during a ListenBrainz outage; the UI
    # shows a subtle "still personalising" hint. Clears once personalised picks land.
    personalizing: bool = False


class PreviewTrackItem(AppStruct):
    title: str
    artist_name: str = ""
    preview_url: str = ""
    duration_s: int | None = None
    position: int | None = None


class TrackPreviewResponse(AppStruct):
    preview_url: str | None = None
    title: str | None = None
    duration_s: int | None = None
    provider: str | None = None


class AlbumPreviewResponse(AppStruct):
    tracks: list[PreviewTrackItem] = []
    provider: str | None = None


class DiscoverIntegrationStatus(IntegrationStatus):
    pass


class DiscoverResponse(AppStruct):
    because_you_listen_to: list[BecauseYouListenTo] = []
    discover_queue_enabled: bool = True
    fresh_releases: HomeSection | None = None
    missing_essentials: HomeSection | None = None
    rediscover: HomeSection | None = None
    artists_you_might_like: HomeSection | None = None
    popular_in_your_genres: HomeSection | None = None
    genre_list: HomeSection | None = None
    globally_trending: HomeSection | None = None
    weekly_exploration: WeeklyExplorationSection | None = None
    integration_status: DiscoverIntegrationStatus | None = None
    service_prompts: list[ServicePrompt] = []
    genre_artwork: dict[str, GenreArtwork] = {}
    genre_artwork_schema_version: str = "v2"
    lastfm_weekly_artist_chart: HomeSection | None = None
    lastfm_weekly_album_chart: HomeSection | None = None
    lastfm_recent_scrobbles: HomeSection | None = None
    daily_mixes: list[HomeSection] = []
    radio_sections: list[HomeSection] = []
    top_picks: TopPicksSection | None = None
    listeners_like_you: HomeSection | None = None
    anniversaries: HomeSection | None = None
    new_from_followed: HomeSection | None = None
    unexplored_genres: HomeSection | None = None
    generated_at: float | None = None
    refresh_started_at: float | None = None
    section_status: dict[str, str] = {}
    refreshing: bool = False
    service_status: dict[str, str] | None = None
