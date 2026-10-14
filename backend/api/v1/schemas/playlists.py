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
    library_file_id: str | None = None
    # "Request all tracks" resolution (#204); all optional for older clients.
    recording_mbid: str | None = None
    target_release_group_mbid: str | None = None
    home_tier: str | None = None
    resolution_status: str | None = None
    flag_options: list[dict] | None = None


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


CompilationPolicyName = Literal["match_playlist", "studio", "flag"]


class ResolveTracksRequest(AppStruct):
    """Body of POST /playlists/{id}/resolve-tracks; omitted fields use the
    configured defaults."""

    compilation_policy: CompilationPolicyName | None = None


class RequestTracksRequest(AppStruct):
    """Body of POST /playlists/{id}/request-tracks. ``auto`` resolves then
    requests; ``review`` requests what an earlier resolve stored."""

    mode: Literal["auto", "review"] | None = None
    compilation_policy: CompilationPolicyName | None = None


class TrackRequestJobResponse(AppStruct):
    """Accepted background job; the outcome arrives as an SSE event."""

    playlist_id: str
    status: Literal["started", "already_running"]
    job: Literal["resolve", "request", "resolve_and_request"]
    # Echoed in the job's SSE payload so a client matches the outcome of the
    # job it started, not a replayed earlier one. None when already running.
    job_id: str | None = None


class ChooseTrackHomeRequest(AppStruct):
    recording_mbid: Annotated[str, msgspec.Meta(min_length=1, max_length=64)]
    release_group_mbid: Annotated[str, msgspec.Meta(min_length=1, max_length=64)]
