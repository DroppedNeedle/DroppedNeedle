from typing import Annotated, Literal

import msgspec
from infrastructure.msgspec_fastapi import AppStruct


class PlaylistTrackResponse(AppStruct):
    id: str
    position: int
    track_name: str
    artist_name: str
    album_name: str
    album_id: str | None = None
    artist_id: str | None = None
    track_source_id: str | None = None
    cover_url: str | None = None
    source_type: str = ""
    available_sources: list[str] | None = None
    format: str | None = None
    track_number: int | None = None
    disc_number: int | None = None
    duration: int | None = None
    created_at: str = ""
    plex_rating_key: str | None = None


class PlaylistSummaryResponse(AppStruct):
    id: str
    name: str
    track_count: int = 0
    total_duration: int | None = None
    cover_urls: list[str] = msgspec.field(default_factory=list)
    custom_cover_url: str | None = None
    source_ref: str | None = None
    created_at: str = ""
    updated_at: str = ""
    # Ownership / visibility (D4).
    is_public: bool = False
    is_owner: bool = False
    owner_name: str | None = None
    is_redacted: bool = False


class PlaylistDetailResponse(AppStruct):
    # Keep these fields in sync with PlaylistSummaryResponse because the frontend extends PlaylistSummary.
    id: str
    name: str
    cover_urls: list[str] = msgspec.field(default_factory=list)
    custom_cover_url: str | None = None
    source_ref: str | None = None
    tracks: list[PlaylistTrackResponse] = msgspec.field(default_factory=list)
    track_count: int = 0
    total_duration: int | None = None
    created_at: str = ""
    updated_at: str = ""
    # Ownership / visibility (D4).
    is_public: bool = False
    is_owner: bool = False
    owner_name: str | None = None
    is_redacted: bool = False


class RedactedPlaylist(AppStruct):
    """Admin's view of another user's PRIVATE playlist (D4): existence + count + owner
    only, never the name/tracks/covers. Returned in the list and as a detail body (200)."""
    id: str
    track_count: int = 0
    owner_name: str | None = None
    is_redacted: bool = True


class PlaylistListResponse(AppStruct):
    playlists: list[PlaylistSummaryResponse | RedactedPlaylist] = msgspec.field(default_factory=list)


class SetPlaylistPublicRequest(AppStruct):
    is_public: bool


class CreatePlaylistRequest(AppStruct):
    name: str


class UpdatePlaylistRequest(AppStruct):
    name: str | None = None


class SmartMixSeed(AppStruct):
    type: Literal["artist", "genre", "mood"]
    value: str


class GeneratePlaylistRequest(AppStruct):
    """Smart Mix: generate and persist a playlist from a blend of seeds
    (1-10, any mix of artist/genre/mood)."""
    seeds: Annotated[list[SmartMixSeed], msgspec.Meta(min_length=1, max_length=10)]
    count: Annotated[int, msgspec.Meta(ge=1, le=250)] = 25
    name: str | None = None


class TrackDataRequest(AppStruct):
    track_name: str
    artist_name: str
    album_name: str
    album_id: str | None = None
    artist_id: str | None = None
    track_source_id: str | None = None
    cover_url: str | None = None
    source_type: str = ""
    available_sources: list[str] | None = None
    format: str | None = None
    track_number: int | None = None
    disc_number: int | None = None
    duration: float | int | None = None
    plex_rating_key: str | None = None


class AddTracksRequest(AppStruct):
    tracks: list[TrackDataRequest]
    position: int | None = None


class RemoveTracksRequest(AppStruct):
    track_ids: list[str]


class ReorderTrackRequest(AppStruct):
    track_id: str
    new_position: int


class ReorderTrackResponse(AppStruct):
    status: str = "ok"
    message: str = "Track reordered"
    actual_position: int = 0


class UpdateTrackRequest(AppStruct):
    source_type: str | None = None
    available_sources: list[str] | None = None


class AddTracksResponse(AppStruct):
    tracks: list[PlaylistTrackResponse] = msgspec.field(default_factory=list)


class CoverUploadResponse(AppStruct):
    cover_url: str


class TrackIdentifier(AppStruct):
    track_name: str
    artist_name: str
    album_name: str


class CheckTrackMembershipRequest(AppStruct):
    tracks: list[TrackIdentifier]


class CheckTrackMembershipResponse(AppStruct):
    membership: dict[str, list[int]]


class ResolveSourcesResponse(AppStruct):
    sources: dict[str, list[str]]
