from __future__ import annotations

from typing import Literal

from infrastructure.msgspec_fastapi import AppStruct


class NavidromeTrackInfo(AppStruct):
    navidrome_id: str
    title: str
    track_number: int
    duration_seconds: float
    disc_number: int = 1
    album_name: str = ""
    artist_name: str = ""
    codec: str | None = None
    bitrate: int | None = None
    image_url: str | None = None
    recording_mbid: str | None = None


class NavidromeAlbumSummary(AppStruct):
    navidrome_id: str
    name: str
    artist_name: str = ""
    year: int | None = None
    track_count: int = 0
    image_url: str | None = None
    musicbrainz_id: str | None = None
    artist_musicbrainz_id: str | None = None


class NavidromeAlbumDetail(AppStruct):
    navidrome_id: str
    name: str
    artist_name: str = ""
    year: int | None = None
    track_count: int = 0
    image_url: str | None = None
    musicbrainz_id: str | None = None
    artist_musicbrainz_id: str | None = None
    tracks: list[NavidromeTrackInfo] = []


class NavidromeAlbumMatch(AppStruct):
    found: bool
    navidrome_album_id: str | None = None
    tracks: list[NavidromeTrackInfo] = []


class NavidromeArtistSummary(AppStruct):
    navidrome_id: str
    name: str
    image_url: str | None = None
    album_count: int = 0
    musicbrainz_id: str | None = None


class NavidromeLibraryStats(AppStruct):
    total_tracks: int = 0
    total_albums: int = 0
    total_artists: int = 0


class NavidromeSearchResponse(AppStruct):
    albums: list[NavidromeAlbumSummary] = []
    artists: list[NavidromeArtistSummary] = []
    tracks: list[NavidromeTrackInfo] = []


class NavidromePlaylistSummary(AppStruct):
    id: str
    name: str
    track_count: int = 0
    duration_seconds: int = 0
    cover_url: str = ""
    owner: str = ""
    is_public: bool = False
    updated_at: str = ""
    is_imported: bool = False


class NavidromePlaylistCollection(AppStruct):
    account_mode: Literal["linked", "shared"]
    account_label: str
    playlists: list[NavidromePlaylistSummary] = []


class NavidromeHubResponse(AppStruct):
    stats: NavidromeLibraryStats | None = None
    recently_played: list[NavidromeAlbumSummary] = []
    favorites: list[NavidromeAlbumSummary] = []
    favorite_artists: list[NavidromeArtistSummary] = []
    favorite_tracks: list[NavidromeTrackInfo] = []
    all_albums_preview: list[NavidromeAlbumSummary] = []
    genres: list[str] = []


class NavidromeAlbumPage(AppStruct):
    items: list[NavidromeAlbumSummary] = []
    total: int = 0


class NavidromeArtistPage(AppStruct):
    items: list[NavidromeArtistSummary] = []
    total: int = 0
    offset: int = 0
    limit: int = 50


class NavidromeTrackPage(AppStruct):
    items: list[NavidromeTrackInfo] = []
    total: int = 0
    offset: int = 0
    limit: int = 50


class NavidromePlaylistTrack(AppStruct):
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


class NavidromePlaylistDetail(AppStruct):
    id: str
    name: str
    track_count: int = 0
    duration_seconds: int = 0
    cover_url: str = ""
    tracks: list[NavidromePlaylistTrack] = []


class NavidromeImportResult(AppStruct):
    droppedneedle_playlist_id: str = ""
    tracks_imported: int = 0
    tracks_failed: int = 0
    already_imported: bool = False


class NavidromeNowPlayingEntrySchema(AppStruct):
    user_name: str = ""
    minutes_ago: int = 0
    player_name: str = ""
    track_name: str = ""
    artist_name: str = ""
    album_name: str = ""
    album_id: str = ""
    cover_art_id: str = ""
    duration_seconds: int = 0
    estimated_position_seconds: float = 0.0


class NavidromeNowPlayingResponse(AppStruct):
    entries: list[NavidromeNowPlayingEntrySchema] = []


class NavidromeArtistInfoSchema(AppStruct):
    navidrome_id: str = ""
    name: str = ""
    biography: str = ""
    image_url: str = ""
    similar_artists: list[NavidromeArtistSummary] = []


class NavidromeAlbumInfoSchema(AppStruct):
    album_id: str = ""
    notes: str = ""
    musicbrainz_id: str = ""
    lastfm_url: str = ""
    image_url: str = ""


class NavidromeLyricLine(AppStruct):
    text: str = ""
    start_seconds: float | None = None


class NavidromeLyricsResponse(AppStruct):
    text: str = ""
    is_synced: bool = False
    lines: list[NavidromeLyricLine] = []


class NavidromeArtistIndexEntry(AppStruct):
    name: str = ""
    artists: list[NavidromeArtistSummary] = []


class NavidromeArtistIndexResponse(AppStruct):
    index: list[NavidromeArtistIndexEntry] = []


class NavidromeGenreSongsResponse(AppStruct):
    songs: list[NavidromeTrackInfo] = []
    genre: str = ""


class NavidromeMusicFolder(AppStruct):
    id: str = ""
    name: str = ""


class NavidromeFolderPreferenceUpdate(AppStruct):
    mode: str
    selected_folder_ids: list[str] = []


class NavidromeFolderPreferenceResponse(AppStruct):
    mode: str
    selected_folder_ids: list[str] = []
    available_folders: list[NavidromeMusicFolder] = []
    stale_folder_ids: list[str] = []
    source_available: bool = True
    scope_revision: str = "all"
