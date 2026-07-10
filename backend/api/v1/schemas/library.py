from models.library import LibraryAlbum as LibraryAlbum
from models.library import LibraryGroupedAlbum as LibraryGroupedAlbum
from models.library import LibraryGroupedArtist as LibraryGroupedArtist
from infrastructure.msgspec_fastapi import AppStruct


class LibraryArtist(AppStruct):
    mbid: str
    name: str
    album_count: int = 0
    date_added: int | None = None


class LibraryResponse(AppStruct):
    library: list[LibraryAlbum]


class RecentlyAddedResponse(AppStruct):
    albums: list[LibraryAlbum] = []
    artists: list[LibraryArtist] = []


class LibraryStatsResponse(AppStruct):
    artist_count: int
    album_count: int
    db_size_bytes: int
    db_size_mb: float
    last_sync: int | None = None


class AlbumRemoveResponse(AppStruct):
    success: bool
    artist_removed: bool = False
    artist_name: str | None = None


class AlbumRemovePreviewResponse(AppStruct):
    success: bool
    artist_will_be_removed: bool = False
    artist_name: str | None = None


class SyncLibraryResponse(AppStruct):
    status: str
    artists: int
    albums: int


class LibraryMbidsResponse(AppStruct):
    mbids: list[str] = []
    requested_mbids: list[str] = []


class LibraryGroupedResponse(AppStruct):
    library: list[LibraryGroupedArtist] = []


class TrackResolveItem(AppStruct):
    release_group_mbid: str | None = None
    disc_number: int | None = None
    track_number: int | None = None


class TrackResolveRequest(AppStruct):
    items: list[TrackResolveItem] = []


class ResolvedTrack(AppStruct):
    release_group_mbid: str | None = None
    disc_number: int | None = None
    track_number: int | None = None
    source: str | None = None
    track_source_id: str | None = None
    stream_url: str | None = None
    format: str | None = None
    duration: float | None = None


class TrackResolveResponse(AppStruct):
    items: list[ResolvedTrack] = []


# reused LibraryManager domain structs; no import cycle since it imports only models/stubs
from services.native.library_manager import (  # noqa: E402, F401
    LibraryAlbumStatus as LibraryAlbumStatusResponse,
    LibraryAlbumSummary as LibraryAlbumResponse,
    LibraryArtistSummary as LibraryArtistResponse,
    LibraryStats as NativeLibraryStatsResponse,
    LibraryTrack as LibraryTrackResponse,
    LibraryTrackListItem as LibraryTrackListItemResponse,
    UnmatchedFile as UnmatchedFileResponse,
)


class NativeAlbumsResponse(AppStruct):
    items: list[LibraryAlbumResponse] = []
    total: int = 0


class NativeTrackPage(AppStruct):
    # envelope matches the frontend createLibraryTrackLoader page shape
    items: list[LibraryTrackListItemResponse] = []
    total: int = 0
    offset: int = 0
    limit: int = 48


class LibraryUnmatchedResponse(AppStruct):
    items: list[UnmatchedFileResponse] = []
    total: int = 0


class NativeArtistsResponse(AppStruct):
    items: list[LibraryArtistResponse] = []
    total: int = 0


class NativeTracksResponse(AppStruct):
    items: list[LibraryTrackResponse] = []


class TrackTagUpdateRequest(AppStruct):
    title: str
    artist: str
    album: str
    track_number: int
    album_artist: str | None = None
    disc_number: int = 1
    year: int | None = None
    genre: str | None = None
    musicbrainz_release_group_id: str | None = None
    musicbrainz_release_id: str | None = None
    musicbrainz_recording_id: str | None = None
    musicbrainz_artist_id: str | None = None
    musicbrainz_album_artist_id: str | None = None


class UnmatchedResolveRequest(AppStruct):
    # mbid required for manual_id, optional for accept (falls back to top candidate)
    resolution: str  # 'accept' | 'reject' | 'manual_id'
    mbid: str | None = None


class UnmatchedBatchItem(AppStruct):
    review_id: int
    recording_mbid: str | None = None


class UnmatchedBatchResolveRequest(AppStruct):
    release_group_mbid: str
    items: list[UnmatchedBatchItem] = []


class UnmatchedBatchFailure(AppStruct):
    review_id: int
    error: str


class UnmatchedBatchResolveResponse(AppStruct):
    resolved: int = 0
    failed: list[UnmatchedBatchFailure] = []


class LibraryLyricLine(AppStruct):
    text: str = ""
    start_seconds: float | None = None


class LibraryLyricsResponse(AppStruct):
    # same shape the frontend already consumes from the jellyfin/navidrome lyrics endpoints
    text: str = ""
    is_synced: bool = False
    lines: list[LibraryLyricLine] = []


class LibraryScanStatusResponse(AppStruct):
    status: str = "idle"
    total_files: int = 0
    processed_files: int = 0
    matched_files: int = 0
    failed_files: int = 0
    started_at: float | None = None
    updated_at: float | None = None
