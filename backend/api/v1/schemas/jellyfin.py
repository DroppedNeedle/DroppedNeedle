from typing import Literal

from infrastructure.msgspec_fastapi import AppStruct


class JellyfinTrackInfo(AppStruct):
    jellyfin_id: str
    title: str
    track_number: int
    duration_seconds: float
    disc_number: int = 1
    album_name: str = ""
    artist_name: str = ""
    album_id: str = ""
    codec: str | None = None
    bitrate: int | None = None
    image_url: str | None = None
    recording_mbid: str | None = None


class JellyfinAlbumSummary(AppStruct):
    jellyfin_id: str
    name: str
    artist_name: str = ""
    year: int | None = None
    track_count: int = 0
    image_url: str | None = None
    musicbrainz_id: str | None = None
    artist_musicbrainz_id: str | None = None
    play_count: int = 0


class JellyfinAlbumDetail(AppStruct):
    jellyfin_id: str
    name: str
    artist_name: str = ""
    year: int | None = None
    track_count: int = 0
    image_url: str | None = None
    musicbrainz_id: str | None = None
    artist_musicbrainz_id: str | None = None
    tracks: list[JellyfinTrackInfo] = []


class JellyfinAlbumMatch(AppStruct):
    found: bool
    jellyfin_album_id: str | None = None
    tracks: list[JellyfinTrackInfo] = []


class JellyfinArtistSummary(AppStruct):
    jellyfin_id: str
    name: str
    image_url: str | None = None
    album_count: int = 0
    musicbrainz_id: str | None = None
    play_count: int = 0


class JellyfinLibraryStats(AppStruct):
    total_tracks: int = 0
    total_albums: int = 0
    total_artists: int = 0


class JellyfinSearchResponse(AppStruct):
    albums: list[JellyfinAlbumSummary] = []
    artists: list[JellyfinArtistSummary] = []
    tracks: list[JellyfinTrackInfo] = []


class JellyfinPlaylistSummary(AppStruct):
    id: str
    name: str
    track_count: int = 0
    duration_seconds: int = 0
    cover_url: str = ""
    created_at: str = ""
    is_imported: bool = False


class JellyfinPlaylistCollection(AppStruct):
    account_mode: Literal["linked", "shared"]
    account_label: str
    playlists: list[JellyfinPlaylistSummary] = []


class JellyfinHubResponse(AppStruct):
    stats: JellyfinLibraryStats | None = None
    recently_played: list[JellyfinAlbumSummary] = []
    recently_added: list[JellyfinAlbumSummary] = []
    favorites: list[JellyfinAlbumSummary] = []
    most_played_artists: list[JellyfinArtistSummary] = []
    most_played_albums: list[JellyfinAlbumSummary] = []
    all_albums_preview: list[JellyfinAlbumSummary] = []
    genres: list[str] = []


class JellyfinPaginatedResponse(AppStruct):
    items: list[JellyfinAlbumSummary] = []
    total: int = 0
    offset: int = 0
    limit: int = 50


class JellyfinArtistPage(AppStruct):
    items: list[JellyfinArtistSummary] = []
    total: int = 0
    offset: int = 0
    limit: int = 50


class JellyfinArtistIndexEntry(AppStruct):
    name: str = ""
    artists: list[JellyfinArtistSummary] = []


class JellyfinArtistIndexResponse(AppStruct):
    index: list[JellyfinArtistIndexEntry] = []


class JellyfinTrackPage(AppStruct):
    items: list[JellyfinTrackInfo] = []
    total: int = 0
    offset: int = 0
    limit: int = 50


class JellyfinPlaylistTrack(AppStruct):
    id: str
    track_name: str
    artist_name: str = ""
    album_name: str = ""
    album_id: str = ""
    artist_id: str = ""
    duration_seconds: int = 0
    track_number: int = 0
    disc_number: int = 1
    cover_url: str = ""


class JellyfinPlaylistDetail(AppStruct):
    id: str
    name: str
    track_count: int = 0
    duration_seconds: int = 0
    cover_url: str = ""
    created_at: str = ""
    tracks: list[JellyfinPlaylistTrack] = []


class JellyfinImportResult(AppStruct):
    droppedneedle_playlist_id: str = ""
    tracks_imported: int = 0
    tracks_failed: int = 0
    already_imported: bool = False


class JellyfinSessionInfo(AppStruct):
    session_id: str = ""
    user_name: str = ""
    device_name: str = ""
    client_name: str = ""
    track_name: str = ""
    artist_name: str = ""
    album_name: str = ""
    album_id: str = ""
    cover_url: str = ""
    position_seconds: float = 0.0
    duration_seconds: float = 0.0
    is_paused: bool = False
    play_method: str = ""
    audio_codec: str = ""
    bitrate: int = 0


class JellyfinSessionsResponse(AppStruct):
    sessions: list[JellyfinSessionInfo] = []


class JellyfinLyricsLineSchema(AppStruct):
    text: str = ""
    start_seconds: float | None = None


class JellyfinLyricsResponse(AppStruct):
    lines: list[JellyfinLyricsLineSchema] = []
    is_synced: bool = False
    lyrics_text: str = ""


class JellyfinFavoritesExpanded(AppStruct):
    albums: list[JellyfinAlbumSummary] = []
    artists: list[JellyfinArtistSummary] = []


class JellyfinFilterFacets(AppStruct):
    years: list[int] = []
    tags: list[str] = []
    studios: list[str] = []
