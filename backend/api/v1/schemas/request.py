from typing import Annotated
import msgspec
from infrastructure.msgspec_fastapi import AppStruct


class AlbumRequest(AppStruct):
    musicbrainz_id: str
    artist: str | None = None
    album: str | None = None
    year: int | None = None
    artist_mbid: str | None = None
    monitor_artist: bool = False
    auto_download_artist: bool = False


class RequestAcceptedResponse(AppStruct):
    success: bool
    message: str
    musicbrainz_id: str
    status: str = "pending"


class BatchAlbumItem(AppStruct):
    musicbrainz_id: str
    artist_name: str = "Unknown"
    album_title: str = "Unknown"
    year: int | None = None
    artist_mbid: str | None = None


class BatchAlbumRequest(AppStruct):
    items: Annotated[list[BatchAlbumItem], msgspec.Meta(max_length=500)]
    monitor_artist: bool = False
    auto_download_artist: bool = False


class BatchRequestResponse(AppStruct):
    success: bool
    message: str
    requested: int = 0
    skipped: int = 0
    overflow: int = 0
    # Native clients must render the decision made at this mutation boundary,
    # not infer it from a role value that may have changed moments earlier.
    status: str = "pending"


class BatchCancelRequest(AppStruct):
    musicbrainz_ids: Annotated[list[str], msgspec.Meta(max_length=500)]


class BatchCancelResponse(AppStruct):
    success: bool
    cancelled: int = 0
    failed: int = 0
    message: str = ""
