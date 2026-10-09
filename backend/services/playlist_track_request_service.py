"""Playlist "Request all tracks" orchestration (#204).

Glues :class:`~services.playlist_track_resolver.PlaylistTrackResolver` (which
album each track comes from) to ``RequestService.request_track_batch`` (the
existing per-track request path), and handles a user's choice for a flagged
track. Ownership is enforced by the routes before any of this runs.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from typing import Any, Protocol

from api.v1.schemas.request import BatchRequestResponse
from api.v1.schemas.settings import PlaylistTrackRequestSettings
from core.exceptions import ValidationError
from models.release_home import CompilationPolicy, ResolutionStatus
from repositories.playlist_repository import PlaylistTrackRecord, TrackResolution
from services.playlist_track_resolver import (
    PlaylistTrackResolver,
    ResolutionSummary,
    is_owned,
)

logger = logging.getLogger(__name__)

_REQUESTABLE = frozenset({ResolutionStatus.RESOLVED, ResolutionStatus.CHOSEN})


class _PlaylistRepo(Protocol):
    async def get_tracks(self, playlist_id: str) -> list[PlaylistTrackRecord]: ...

    async def get_track(
        self, playlist_id: str, track_id: str
    ) -> PlaylistTrackRecord | None: ...

    async def batch_update_resolutions(
        self, playlist_id: str, updates: dict[str, TrackResolution]
    ) -> int: ...


class _RequestService(Protocol):
    async def request_track_batch(
        self,
        items: list[dict],
        *,
        user_id: str,
        user_role: str,
        requested_by_name: str | None = None,
    ) -> BatchRequestResponse: ...


class _Preferences(Protocol):
    def get_playlist_track_request_settings(self) -> PlaylistTrackRequestSettings: ...


def requestable_items(tracks: list[PlaylistTrackRecord]) -> list[dict[str, Any]]:
    """Request items for every resolved (or user-chosen) track not yet owned."""
    items: list[dict[str, Any]] = []
    for track in tracks:
        if (
            track.resolution_status not in _REQUESTABLE
            or is_owned(track)
            or not track.recording_mbid
            or not track.target_release_group_mbid
        ):
            continue
        items.append(
            {
                "recording_mbid": track.recording_mbid,
                "artist_name": track.artist_name or "Unknown",
                "track_title": track.track_name,
                "duration_seconds": track.duration,
                "release_group_mbid": track.target_release_group_mbid,
            }
        )
    return items


class PlaylistTrackRequestService:
    def __init__(
        self,
        *,
        playlist_repo: _PlaylistRepo,
        resolver_factory: Callable[[], PlaylistTrackResolver],
        request_service: _RequestService,
        preferences_service: _Preferences,
    ) -> None:
        self._playlists = playlist_repo
        self._resolver_factory = resolver_factory
        self._requests = request_service
        self._preferences = preferences_service

    def defaults(self) -> PlaylistTrackRequestSettings:
        return self._preferences.get_playlist_track_request_settings()

    def policy(self, override: str | None) -> CompilationPolicy:
        return CompilationPolicy(override or self.defaults().compilation_policy)

    async def resolve(
        self, playlist_id: str, policy: CompilationPolicy
    ) -> ResolutionSummary:
        # A fresh resolver per run: it caches tracklists for that run only.
        return await self._resolver_factory().resolve_playlist(playlist_id, policy)

    async def request_resolved(
        self,
        playlist_id: str,
        *,
        user_id: str,
        user_role: str,
        requested_by_name: str | None = None,
    ) -> BatchRequestResponse:
        items = requestable_items(await self._playlists.get_tracks(playlist_id))
        if not items:
            return BatchRequestResponse(
                success=True,
                message="No resolved tracks left to request",
                status="nothing_to_request",
            )
        return await self._requests.request_track_batch(
            items,
            user_id=user_id,
            user_role=user_role,
            requested_by_name=requested_by_name,
        )

    async def choose_home(
        self,
        playlist_id: str,
        track_id: str,
        *,
        recording_mbid: str,
        release_group_mbid: str,
    ) -> PlaylistTrackRecord:
        """Pin a flagged track to one of the options it was offered.

        Only offered options are accepted, so the stored choice always names
        a recording known to be on that release group.
        """
        track = await self._playlists.get_track(playlist_id, track_id)
        if track is None:
            raise ValidationError("Track not found in this playlist")
        wanted = (recording_mbid.strip().casefold(), release_group_mbid.strip().casefold())
        option = next(
            (
                option
                for option in track.flag_options or []
                if (
                    str(option.get("recording_mbid", "")).casefold(),
                    str(option.get("release_group_mbid", "")).casefold(),
                )
                == wanted
            ),
            None,
        )
        if option is None:
            raise ValidationError("That album was not offered for this track")
        await self._playlists.batch_update_resolutions(
            playlist_id,
            {
                track_id: TrackResolution(
                    target_release_group_mbid=wanted[1],
                    home_tier=option.get("tier"),
                    resolution_status=ResolutionStatus.CHOSEN.value,
                    recording_mbid=wanted[0],
                    # Kept so the user can change their mind later.
                    flag_options=track.flag_options,
                )
            },
        )
        updated = await self._playlists.get_track(playlist_id, track_id)
        assert updated is not None
        return updated
