from __future__ import annotations

from infrastructure.msgspec_fastapi import AppStruct


class PlexTrackInfo(AppStruct):
    plex_id: str
    title: str
    track_number: int
    duration_seconds: float
    disc_number: int = 1
    album_name: str = ""
    artist_name: str = ""
    codec: str | None = None
    bitrate: int | None = None
    audio_channels: int | None = None
    container: str | None = None
    part_key: str | None = None
    image_url: str | None = None


class PlexAlbumSummary(AppStruct):
    plex_id: str
    name: str
    artist_name: str = ""
    year: int | None = None
    track_count: int = 0
    image_url: str | None = None
    musicbrainz_id: str | None = None
    artist_musicbrainz_id: str | None = None
    last_viewed_at: int = 0


class PlexAlbumDetail(AppStruct):
    plex_id: str
    name: str
    artist_name: str = ""
    year: int | None = None
    track_count: int = 0
    image_url: str | None = None
    musicbrainz_id: str | None = None
    artist_musicbrainz_id: str | None = None
    tracks: list[PlexTrackInfo] = []
    genres: list[str] = []


class PlexAlbumMatch(AppStruct):
    found: bool
    plex_album_id: str | None = None
    tracks: list[PlexTrackInfo] = []


class PlexArtistSummary(AppStruct):
    plex_id: str
    name: str
    image_url: str | None = None
    musicbrainz_id: str | None = None


class PlexLibraryStats(AppStruct):
    total_tracks: int = 0
    total_albums: int = 0
    total_artists: int = 0


class PlexSearchResponse(AppStruct):
    albums: list[PlexAlbumSummary] = []
    artists: list[PlexArtistSummary] = []
    tracks: list[PlexTrackInfo] = []


class PlexAlbumPage(AppStruct):
    items: list[PlexAlbumSummary] = []
    total: int = 0


class PlexArtistPage(AppStruct):
    items: list[PlexArtistSummary] = []
    total: int = 0
    offset: int = 0
    limit: int = 50


class PlexArtistIndexEntry(AppStruct):
    name: str = ""
    artists: list[PlexArtistSummary] = []


class PlexArtistIndexResponse(AppStruct):
    index: list[PlexArtistIndexEntry] = []


class PlexTrackPage(AppStruct):
    items: list[PlexTrackInfo] = []
    total: int = 0
    offset: int = 0
    limit: int = 50


class PlexPlaylistSummary(AppStruct):
    id: str
    name: str
    track_count: int = 0
    duration_seconds: int = 0
    is_smart: bool = False
    cover_url: str = ""
    updated_at: str = ""
    is_imported: bool = False


class PlexHubResponse(AppStruct):
    stats: PlexLibraryStats | None = None
    recently_played: list[PlexAlbumSummary] = []
    recently_added: list[PlexAlbumSummary] = []
    all_albums_preview: list[PlexAlbumSummary] = []
    genres: list[str] = []
    playlists: list[PlexPlaylistSummary] = []


class PlexDiscoveryAlbum(AppStruct):
    plex_id: str
    name: str
    artist_name: str = ""
    year: int | None = None
    image_url: str | None = None


class PlexDiscoveryHub(AppStruct):
    title: str
    hub_type: str = ""
    albums: list[PlexDiscoveryAlbum] = []


class PlexDiscoveryResponse(AppStruct):
    hubs: list[PlexDiscoveryHub] = []


class PlexLibrarySectionInfo(AppStruct):
    key: str
    title: str


class PlexPlaylistTrack(AppStruct):
    id: str
    track_name: str
    artist_name: str = ""
    album_name: str = ""
    album_id: str = ""
    plex_rating_key: str = ""
    part_key: str = ""
    duration_seconds: int = 0
    track_number: int = 0
    disc_number: int = 1
    cover_url: str = ""


class PlexPlaylistDetail(AppStruct):
    id: str
    name: str
    track_count: int = 0
    duration_seconds: int = 0
    is_smart: bool = False
    cover_url: str = ""
    updated_at: str = ""
    tracks: list[PlexPlaylistTrack] = []


class PlexImportResult(AppStruct):
    musicseerr_playlist_id: str = ""
    tracks_imported: int = 0
    tracks_failed: int = 0
    already_imported: bool = False


class PlexSessionInfo(AppStruct):
    session_id: str = ""
    user_name: str = ""
    track_title: str = ""
    artist_name: str = ""
    album_name: str = ""
    cover_url: str = ""
    player_device: str = ""
    player_platform: str = ""
    player_state: str = ""
    is_direct_play: bool = True
    progress_ms: int = 0
    duration_ms: int = 0
    audio_codec: str = ""
    audio_channels: int = 0
    bitrate: int = 0


class PlexSessionsResponse(AppStruct):
    sessions: list[PlexSessionInfo] = []
    available: bool = True


class PlexHistoryEntrySchema(AppStruct):
    rating_key: str = ""
    track_title: str = ""
    artist_name: str = ""
    album_name: str = ""
    cover_url: str = ""
    viewed_at: str = ""
    device_name: str = ""


class PlexHistoryResponse(AppStruct):
    entries: list[PlexHistoryEntrySchema] = []
    total: int = 0
    limit: int = 0
    offset: int = 0
    available: bool = True


class PlexAnalyticsItem(AppStruct):
    name: str = ""
    subtitle: str = ""
    play_count: int = 0
    cover_url: str | None = None


class PlexAnalyticsResponse(AppStruct):
    top_artists: list[PlexAnalyticsItem] = []
    top_albums: list[PlexAnalyticsItem] = []
    top_tracks: list[PlexAnalyticsItem] = []
    total_listens: int = 0
    listens_last_7_days: int = 0
    listens_last_30_days: int = 0
    total_hours: float = 0.0
    is_complete: bool = True
    entries_analyzed: int = 0
