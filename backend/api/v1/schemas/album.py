from api.v1.schemas.common import LastFmTagSchema
from models.album import AlbumInfo as AlbumInfo
from models.album import Track as Track
from infrastructure.msgspec_fastapi import AppStruct


class AlbumBasicInfo(AppStruct):
    title: str
    musicbrainz_id: str
    artist_name: str
    artist_id: str
    release_date: str | None = None
    year: int | None = None
    type: str | None = None
    disambiguation: str | None = None
    in_library: bool = False
    requested: bool = False
    cover_url: str | None = None
    album_thumb_url: str | None = None


class AlbumTracksInfo(AppStruct):
    tracks: list[Track] = []
    total_tracks: int = 0
    total_length: int | None = None
    label: str | None = None
    barcode: str | None = None
    country: str | None = None


class AlbumEditionItem(AppStruct):
    """One edition (MB release) of a release group, for the Edition picker
    (CollectionManagement Feature E)."""

    release_mbid: str
    track_count: int
    title: str | None = None
    disambiguation: str | None = None
    date: str | None = None
    country: str | None = None
    packaging: str | None = None
    status: str | None = None
    is_owned: bool = False
    is_pinned: bool = False


class AlbumEditionsResponse(AppStruct):
    items: list[AlbumEditionItem] = []
    pinned_release_mbid: str | None = None
    owned_release_mbid: str | None = None


class EditionPinBody(AppStruct):
    release_mbid: str


class EditionPinResponse(AppStruct):
    pinned_release_mbid: str | None = None


class EditionAcquireResponse(AppStruct):
    """'Acquire this edition' outcome: how many tracks were requested (missing),
    queued for upgrade (below cutoff), or needed nothing."""

    release_mbid: str
    total_tracks: int
    requested: int
    upgrades: int
    skipped: int


class LastFmAlbumEnrichment(AppStruct):
    summary: str | None = None
    tags: list[LastFmTagSchema] = []
    listeners: int = 0
    playcount: int = 0
    url: str | None = None
