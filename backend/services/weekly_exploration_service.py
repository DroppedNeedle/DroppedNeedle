from __future__ import annotations

import asyncio
import logging

from api.v1.schemas.weekly_exploration import (
    WeeklyExplorationSection,
    WeeklyExplorationTrack,
)
from infrastructure.cover_urls import release_cover_url, release_group_cover_url
from repositories.protocols import (
    ListenBrainzRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
)

logger = logging.getLogger(__name__)

# Bound the release->release-group resolution: during a ListenBrainz-popularity outage
# every discover section falls back to MusicBrainz (1/s) at once and they mutually starve.
# On timeout the section still renders with the release-level cover fallback.
_MB_RESOLVE_BUDGET_SECONDS = 15


class WeeklyExplorationService:
    def __init__(
        self,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
    ) -> None:
        self._lb_repo = listenbrainz_repo
        self._mb_repo = musicbrainz_repo

    async def build_section(
        self, username: str, lb_repo: ListenBrainzRepositoryProtocol | None = None
    ) -> WeeklyExplorationSection | None:
        # Use the requesting user's request-scoped client when provided (Phase 5);
        # never the global singleton's identity.
        repo = lb_repo or self._lb_repo
        try:
            playlists = await repo.get_recommendation_playlists(username)
            if not playlists:
                return None

            newest = next(
                (p for p in playlists if p.get("source_patch") == "weekly-exploration"),
                playlists[0],
            )
            playlist_id = newest.get("playlist_id", "")
            if not playlist_id:
                return None

            playlist = await repo.get_playlist_tracks(playlist_id)
            if not playlist or not playlist.tracks:
                return None

            recording_ids = [
                track.recording_mbid
                for track in playlist.tracks
                if track.recording_mbid
            ]
            try:
                recording_to_rg = await repo.get_recording_release_groups_batch(
                    recording_ids
                )
                if not isinstance(recording_to_rg, dict):
                    recording_to_rg = {}
            except Exception:  # noqa: BLE001 - missing metadata falls back to MB
                recording_to_rg = {}

            unresolved_release_ids = list({
                track.caa_release_mbid
                for track in playlist.tracks
                if track.caa_release_mbid
                and not (
                    track.recording_mbid
                    and recording_to_rg.get(track.recording_mbid)
                )
            })
            try:
                rg_results = await asyncio.wait_for(
                    asyncio.gather(
                        *(
                            self._mb_repo.get_release_group_id_from_release(release_id)
                            for release_id in unresolved_release_ids
                        ),
                        return_exceptions=True,
                    ),
                    timeout=_MB_RESOLVE_BUDGET_SECONDS,
                )
            except Exception:  # noqa: BLE001 - MB starved, use release-level covers
                logger.warning("Weekly exploration RG resolution exceeded its budget; using release covers")
                rg_results = []
            release_to_rg = {
                release_id: release_group_id
                for release_id, release_group_id in zip(unresolved_release_ids, rg_results)
                if isinstance(release_group_id, str) and release_group_id
            }

            tracks: list[WeeklyExplorationTrack] = []
            for track in playlist.tracks:
                artist_mbid = track.artist_mbids[0] if track.artist_mbids else None
                release_group_mbid = (
                    recording_to_rg.get(track.recording_mbid, "")
                    if track.recording_mbid
                    else None
                ) or (
                    release_to_rg.get(track.caa_release_mbid, "")
                    if track.caa_release_mbid
                    else None
                )

                cover_url: str | None = None
                if release_group_mbid:
                    cover_url = release_group_cover_url(release_group_mbid, size=250)
                elif track.caa_release_mbid:
                    cover_url = release_cover_url(track.caa_release_mbid, size=250)

                tracks.append(WeeklyExplorationTrack(
                    title=track.title,
                    artist_name=track.creator,
                    album_name=track.album,
                    recording_mbid=track.recording_mbid,
                    artist_mbid=artist_mbid,
                    release_group_mbid=release_group_mbid or None,
                    cover_url=cover_url,
                    duration_ms=track.duration_ms,
                ))

            return WeeklyExplorationSection(
                title=playlist.title,
                playlist_date=playlist.date,
                tracks=tracks,
                source_url=newest.get("identifier", ""),
            )
        except Exception as exc:  # noqa: BLE001
            logger.warning("Failed to build weekly exploration: %s", exc)
            return None
