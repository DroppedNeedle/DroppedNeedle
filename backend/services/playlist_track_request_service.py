"""Playlist "Request all tracks" orchestration (#204).

Glues :class:`~services.playlist_track_resolver.PlaylistTrackResolver` (which
album each track comes from) to ``RequestService.request_track_batch`` (the
existing per-track request path), and handles a user's choice for a flagged
track. Ownership is enforced by the routes before any of this runs.
"""

from __future__ import annotations

import logging
from collections.abc import Awaitable, Callable
from typing import Any, Protocol

from api.v1.schemas.request import BatchRequestResponse
from api.v1.schemas.settings import PlaylistTrackRequestSettings
from core.exceptions import ValidationError
from models.release_home import CompilationPolicy, RecordingHome, ResolutionStatus
from models.song_identity import lead_artist
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
        homes_lookup: Callable[[str], Awaitable[list[RecordingHome] | None]]
        | None = None,
    ) -> None:
        self._playlists = playlist_repo
        self._resolver_factory = resolver_factory
        self._requests = request_service
        self._preferences = preferences_service
        # Recording -> homes (durable-store first); names the edition to fetch.
        self._homes_lookup = homes_lookup

    def defaults(self) -> PlaylistTrackRequestSettings:
        return self._preferences.get_playlist_track_request_settings()

    def policy(self, override: str | None) -> CompilationPolicy:
        return CompilationPolicy(override or self.defaults().compilation_policy)

    async def resolve(
        self, playlist_id: str, policy: CompilationPolicy
    ) -> ResolutionSummary:
        # A fresh resolver per run: it caches tracklists for that run only.
        return await self._resolver_factory().resolve_playlist(playlist_id, policy)

    async def link_by_recording(self, playlist_id: str) -> int:
        """Link entries to library tracks holding their recording; 0 when the
        backend can't, or on error (linking never fails a job)."""
        link = getattr(self._playlists, "link_tracks_by_recording", None)
        if link is None:
            return 0
        try:
            linked = await link(playlist_id)
        except Exception:  # noqa: BLE001
            logger.warning("playlist.link_by_recording failed playlist=%s", playlist_id)
            return 0
        return linked if isinstance(linked, int) else 0

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
        for item in items:
            home = await self._home_for(
                item["recording_mbid"], item["release_group_mbid"]
            )
            item["release_mbid"] = home.release_mbid if home else None
            # Real names, as the album page sends: they name the folder the
            # track is filed under. Without a home the downloader backfills
            # the album title from the release group.
            item["album_title"] = home.title if home and home.title else None
            if home and home.artist_credit and not home.is_various:
                item["artist_name"] = home.artist_credit
            else:
                item["artist_name"] = lead_artist(item["artist_name"])
        return await self._requests.request_track_batch(
            items,
            user_id=user_id,
            user_role=user_role,
            requested_by_name=requested_by_name,
        )

    async def _home_for(
        self, recording_mbid: str, release_group_mbid: str
    ) -> RecordingHome | None:
        """The chosen album as the recording's home: its title and credit, and
        the edition that carries the recording.

        Without that edition the downloader uses the album's default one, which
        may not hold the recording, and refuses the request. None keeps that
        default (lookup unavailable or failed).
        """
        if self._homes_lookup is None:
            return None
        try:
            homes = await self._homes_lookup(recording_mbid)
        except Exception:  # noqa: BLE001 - the default edition is the fallback
            logger.warning("playlist.request edition lookup failed")
            return None
        wanted = release_group_mbid.casefold()
        return next(
            (
                home
                for home in homes or []
                if home.release_group_mbid.casefold() == wanted
            ),
            None,
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
                    album_id=wanted[1],
                    cover_url=f"/api/v1/covers/release-group/{wanted[1]}?size=250",
                    # Kept so the user can change their mind later.
                    flag_options=track.flag_options,
                )
            },
        )
        updated = await self._playlists.get_track(playlist_id, track_id)
        assert updated is not None
        return updated
