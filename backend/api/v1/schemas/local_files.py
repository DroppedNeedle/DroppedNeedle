from infrastructure.msgspec_fastapi import AppStruct


class LocalTrackInfo(AppStruct):
    track_file_id: str
    title: str
    track_number: int
    disc_number: int = 1
    duration_seconds: float | None = None
    size_bytes: int = 0
    format: str = ""
    bitrate: int | None = None
    date_added: str | None = None
    # MusicBrainz recording MBID for the indexed file. Enables playlist source
    # resolution to match by recording before falling back to disc/track.
    recording_mbid: str | None = None


class LocalLyricLine(AppStruct):
    text: str = ""
    start_seconds: float | None = None


class LocalLyricsResponse(AppStruct):
    text: str = ""
    is_synced: bool = False
    lines: list[LocalLyricLine] = []


class LocalAlbumMatch(AppStruct):
    found: bool
    musicbrainz_id: str | None = None
    tracks: list[LocalTrackInfo] = []
    total_size_bytes: int = 0
    primary_format: str | None = None
    # Whether the caller may download these files (setting + role, computed
    # per request by match_local_album; default True keeps other producers
    # fail-open).
    download_allowed: bool = True


class LocalAlbumSummary(AppStruct):
    musicbrainz_id: str
    name: str
    artist_name: str
    artist_mbid: str | None = None
    year: int | None = None
    track_count: int = 0
    total_size_bytes: int = 0
    primary_format: str | None = None
    cover_url: str | None = None
    date_added: str | None = None
    # Whether the caller may download this album's files (setting + role,
    # computed per request by the local-library list routes; default True keeps
    # other producers fail-open).
    download_allowed: bool = True


class DownloadAccessResponse(AppStruct):
    """Viewer capability: whether the caller may download library files."""

    allowed: bool


class LocalPaginatedResponse(AppStruct):
    items: list[LocalAlbumSummary] = []
    total: int = 0
    offset: int = 0
    limit: int = 50


class FormatInfo(AppStruct):
    count: int = 0
    size_bytes: int = 0
    size_human: str = "0 B"


class LocalStorageStats(AppStruct):
    total_tracks: int = 0
    total_albums: int = 0
    total_artists: int = 0
    total_size_bytes: int = 0
    total_size_human: str = "0 B"
    disk_free_bytes: int = 0
    disk_free_human: str = "0 B"
    format_breakdown: dict[str, FormatInfo] = {}


class CrateTrack(AppStruct):
    """One playable track suggestion for the Listening Room crate, tagged with
    WHY it's there so the card can show a reason badge."""

    track_file_id: str
    title: str
    album_name: str
    artist_name: str
    album_mbid: str | None = None
    cover_url: str | None = None
    format: str = ""
    year: int | None = None
    duration_seconds: float | None = None
    reason: str = "surprise"  # recent | rediscover | surprise | same_era


class CrateResponse(AppStruct):
    items: list[CrateTrack] = []


class LocalSearchResponse(AppStruct):
    """Library search results: matching albums and matching individual tracks.
    Tracks reuse CrateTrack so the player can queue them with the same wiring."""

    albums: list[LocalAlbumSummary] = []
    tracks: list[CrateTrack] = []


class DecadeShelf(AppStruct):
    decade: int
    label: str
    album_count: int
    albums: list[LocalAlbumSummary] = []


class DecadesResponse(AppStruct):
    items: list[DecadeShelf] = []
