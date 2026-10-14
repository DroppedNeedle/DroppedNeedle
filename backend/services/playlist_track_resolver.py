"""Resolve where every track of a playlist should be sourced from (#204).

The I/O half of "Request all tracks"; :mod:`services.playlist_track_planner`
makes the decision. Per playlist run:

1. **Song identity.** The stored recording MBID, else the track's ISRC
   (durable ISRC map, then MusicBrainz ``/isrc``), else its position and
   title on the release group it was imported against.
2. **Homes.** Every release group the recording appears on
   (``get_recording_homes``), bound to the track's artist.
3. **Studio substitutes** (``studio``/``flag`` policies, compilation-only
   tracks): the same song, same length, on one of the artist's studio albums.
4. **Plan and persist.** The verdict goes to ``playlist_tracks`` via
   ``batch_update_resolutions``.

Owned tracks and user-chosen homes are left untouched, and a track whose
lookups failed keeps whatever verdict it had: a transient MusicBrainz outage
must not erase earlier resolutions.
"""

from __future__ import annotations

import asyncio
import logging
from collections import Counter
from dataclasses import dataclass, field
from typing import Any, Protocol


from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.validators import is_valid_isrc
from models.album import Track
from models.release_home import (
    CompilationPolicy,
    HomeCandidate,
    RecordingHome,
    ResolutionStatus,
    best_home,
    classify_home,
    recording_version_flags,
)
from models.release_type_policy import should_include_release
from models.song_identity import same_song_title, song_title_key
from repositories.musicbrainz_album import recording_home_candidates
from repositories.musicbrainz_base import (
    capture_mb_source_context,
    mb_api_get,
    normalize_mb_id,
)
from repositories.playlist_repository import PlaylistTrackRecord, TrackResolution
from services.playlist_track_planner import (
    PlannerTrack,
    Substitute,
    TrackPlan,
    plan_playlist,
)

logger = logging.getLogger(__name__)

# Same fan-out cap as the Spotify import; the MusicBrainz limiter does the
# real throttling.
_MB_CONCURRENCY = 4
# A single ISRC can map to several recordings; homes are fetched for each.
_MAX_RECORDINGS_PER_ISRC = 5
_ARTIST_RG_PAGE = 100
_ARTIST_RG_MAX_PAGES = 10


# Trailing remaster/mono/stereo/version qualifiers that do not change the song:
# "Fortunate Son - Remastered 1985", "Proud Mary (2008 Remaster)".
def same_song(
    title: str,
    duration_seconds: int | None,
    candidate: Track,
) -> bool:
    return same_song_title(
        title,
        duration_seconds,
        candidate.title,
        candidate.length / 1000 if candidate.length else None,
    )


class _MbRepo(Protocol):
    async def get_recording_homes(
        self, recording_mbid: str, *, priority: RequestPriority = ...
    ) -> list[RecordingHome] | None: ...

    async def get_artist_release_groups(
        self,
        artist_mbid: str,
        offset: int = ...,
        limit: int = ...,
        priority: RequestPriority = ...,
    ) -> tuple[list[dict[str, Any]], int]: ...


class _AlbumTracks(Protocol):
    async def get_album_tracks_info(
        self, release_group_id: str, priority: RequestPriority = ...
    ) -> Any: ...


class _PlaylistRepo(Protocol):
    async def get_tracks(self, playlist_id: str) -> list[PlaylistTrackRecord]: ...

    async def batch_update_resolutions(
        self, playlist_id: str, updates: dict[str, TrackResolution]
    ) -> int: ...


@dataclass(slots=True)
class ResolutionSummary:
    """What a run decided; ``albums_by_tier`` counts distinct release groups."""

    tracks: int = 0
    owned: int = 0
    chosen: int = 0
    resolved: int = 0
    flagged: int = 0
    unresolved: int = 0
    failed: int = 0
    albums_by_tier: dict[str, int] = field(default_factory=dict)

    @property
    def albums(self) -> int:
        return sum(self.albums_by_tier.values())

    def to_json(self) -> dict[str, Any]:
        return {
            "tracks": self.tracks,
            "owned": self.owned,
            "chosen": self.chosen,
            "resolved": self.resolved,
            "flagged": self.flagged,
            "unresolved": self.unresolved,
            "failed": self.failed,
            "albums": self.albums,
            "albums_by_tier": dict(self.albums_by_tier),
        }


@dataclass(frozen=True, slots=True)
class _Identity:
    """A track's recording and homes; ``failed`` marks a transient lookup error."""

    recording_mbid: str | None = None
    candidates: tuple[HomeCandidate, ...] = ()
    failed: bool = False
    homes: tuple[RecordingHome, ...] = ()


def is_owned(track: PlaylistTrackRecord) -> bool:
    """Same ownership rule as the album-level request-missing action."""
    return bool(track.library_file_id or track.available_sources)


class PlaylistTrackResolver:
    def __init__(
        self,
        *,
        playlist_repo: _PlaylistRepo,
        mb_repo: _MbRepo,
        album_service: _AlbumTracks,
        canonical_store: Any | None = None,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> None:
        self._playlists = playlist_repo
        self._mb = mb_repo
        self._albums = album_service
        self._canonical_store = canonical_store
        self._priority = priority
        self._semaphore = asyncio.Semaphore(_MB_CONCURRENCY)
        self._tracklists: dict[str, list[Track] | None] = {}
        self._studio_albums: dict[str, list[dict[str, Any]]] = {}

    async def resolve_playlist(
        self,
        playlist_id: str,
        policy: CompilationPolicy = CompilationPolicy.MATCH_PLAYLIST,
    ) -> ResolutionSummary:
        # Entries whose recording is already in the library (under any album,
        # identified or not) are linked first so they count as owned.
        link = getattr(self._playlists, "link_tracks_by_recording", None)
        if link is not None:
            try:
                await link(playlist_id)
            except Exception:  # noqa: BLE001 - ownership falls back to album links
                logger.warning("playlist.resolve could not link by recording")
        tracks = await self._playlists.get_tracks(playlist_id)
        summary = ResolutionSummary(tracks=len(tracks))
        pending: list[PlaylistTrackRecord] = []
        for track in tracks:
            if track.resolution_status == ResolutionStatus.CHOSEN:
                summary.chosen += 1
            elif is_owned(track):
                summary.owned += 1
            else:
                pending.append(track)

        identities = await asyncio.gather(*(self._identify(t) for t in pending))
        planner_tracks: list[PlannerTrack] = []
        failed_ids: set[str] = set()
        for track, identity in zip(pending, identities, strict=True):
            if identity.failed:
                failed_ids.add(track.id)
                continue
            substitute = None
            if policy is not CompilationPolicy.MATCH_PLAYLIST:
                substitute = await self._substitute_for(track, identity)
            planner_tracks.append(
                PlannerTrack(
                    track.id, identity.recording_mbid, identity.candidates, substitute
                )
            )
        summary.failed = len(failed_ids)

        plans = plan_playlist(planner_tracks, policy)
        updates = {track_id: _to_resolution(plan) for track_id, plan in plans.items()}
        if updates:
            await self._playlists.batch_update_resolutions(playlist_id, updates)
        _tally(summary, plans.values())
        if failed_ids:
            logger.info(
                "playlist.resolve playlist=%s failed_lookups=%s (kept prior verdicts)",
                playlist_id,
                len(failed_ids),
            )
        return summary

    # -- song identity -----------------------------------------------------

    async def _identify(self, track: PlaylistTrackRecord) -> _Identity:
        async with self._semaphore:
            try:
                return await self._identify_unbounded(track)
            except Exception:  # noqa: BLE001 - one track never fails the run
                logger.warning("playlist.resolve track lookup failed", exc_info=True)
                return _Identity(failed=True)

    async def _identify_unbounded(self, track: PlaylistTrackRecord) -> _Identity:
        if track.recording_mbid:
            return await self._identity_for([track.recording_mbid], track)
        isrc = (track.isrc or "").strip().upper()
        if is_valid_isrc(isrc):
            recordings = await self._recordings_for_isrc(isrc)
            if recordings:
                return await self._identity_for(recordings, track)
        recording = await self._recording_from_album(track)
        if recording is None:
            return _Identity()
        return await self._identity_for([recording], track)

    async def _identity_for(
        self, recordings: list[str], track: PlaylistTrackRecord
    ) -> _Identity:
        """Pick, among a track's possible recordings, the one with the best home."""
        best: _Identity | None = None
        any_failed = False
        for recording in recordings[:_MAX_RECORDINGS_PER_ISRC]:
            homes = await self._mb.get_recording_homes(
                recording, priority=self._priority
            )
            if homes is None:
                any_failed = True
                continue
            candidates = tuple(recording_home_candidates(homes, track.artist_name))
            identity = _Identity(
                normalize_mb_id(recording), candidates, homes=tuple(homes)
            )
            if best is None or _identity_rank(identity) < _identity_rank(best):
                best = identity
        if best is not None and (best.candidates or not any_failed):
            return best
        return _Identity(failed=any_failed)

    async def _recordings_for_isrc(self, isrc: str) -> list[str]:
        context = capture_mb_source_context()
        store = self._canonical_store
        if store is not None:
            try:
                known = await store.get_recordings_by_isrc(isrc, source_context=context)
            except Exception:  # noqa: BLE001 - durable miss falls through to wire
                known = []
            if known:
                return sorted({str(rec).casefold() for rec in known})
        data = await mb_api_get(
            f"/isrc/{isrc}", priority=self._priority, source_context=context
        )
        # A definitive 404 decodes to {}: no recordings, fall through to the album.
        recordings = (data.get("recordings") or []) if isinstance(data, dict) else []
        if isinstance(recordings, dict):
            recordings = [recordings]
        ids = sorted({str(rec["id"]).casefold() for rec in recordings if rec.get("id")})
        if store is not None and ids:
            try:
                await store.save_isrc_recordings(
                    [(isrc, rec) for rec in ids], source_context=context
                )
            except Exception:  # noqa: BLE001 - banking is best-effort
                logger.warning("playlist.resolve ISRC banking failed")
        return ids

    async def _recording_from_album(self, track: PlaylistTrackRecord) -> str | None:
        """The track on the release group it was imported against, by position
        and title (the same evidence ``resolve_track_sources`` uses)."""
        if not track.album_id:
            return None
        tracklist = await self._tracklist(track.album_id)
        if not tracklist:
            return None
        matches = [
            candidate
            for candidate in tracklist
            if candidate.recording_id and same_song(track.track_name, None, candidate)
        ]
        if not matches:
            matches = [
                candidate
                for candidate in tracklist
                if candidate.recording_id
                and same_song(track.track_name, track.duration, candidate)
            ]
        positioned = [
            candidate
            for candidate in matches
            if track.track_number == candidate.position
            and (track.disc_number or 1) == candidate.disc_number
        ]
        chosen = (positioned or matches)[:1]
        return chosen[0].recording_id if chosen else None

    async def _tracklist(self, release_group_mbid: str) -> list[Track] | None:
        key = normalize_mb_id(release_group_mbid)
        if key not in self._tracklists:
            try:
                info = await self._albums.get_album_tracks_info(key, self._priority)
                self._tracklists[key] = list(getattr(info, "tracks", None) or [])
            except Exception:  # noqa: BLE001 - an unreadable album is just no match
                logger.warning("playlist.resolve tracklist fetch failed")
                self._tracklists[key] = None
        return self._tracklists[key]

    # -- studio substitutes ------------------------------------------------

    async def _substitute_for(
        self, track: PlaylistTrackRecord, identity: _Identity
    ) -> Substitute | None:
        best = best_home(identity.candidates)
        if best is None or not classify_home(best).is_compilation:
            return None
        if recording_version_flags(identity.candidates):
            # Live/remix/demo recordings keep their version (user decision).
            return None
        artist = _track_artist_mbid(identity)
        if artist is None:
            return None
        for album in await self._artist_studio_albums(artist):
            tracklist = await self._tracklist(album["id"])
            for candidate in tracklist or []:
                if candidate.recording_id and same_song(
                    track.track_name, track.duration, candidate
                ):
                    return Substitute(
                        normalize_mb_id(candidate.recording_id),
                        HomeCandidate(
                            release_group_mbid=normalize_mb_id(album["id"]),
                            title=album.get("title") or "",
                            primary_type=album.get("primary-type"),
                            secondary_types=tuple(album.get("secondary-types") or ()),
                            official=True,
                            is_various=False,
                            credits_track_artist=True,
                            first_release_date=album.get("first-release-date"),
                        ),
                    )
        return None

    async def _artist_studio_albums(self, artist_mbid: str) -> list[dict[str, Any]]:
        """The artist's studio albums, earliest first (cached per run)."""
        if artist_mbid not in self._studio_albums:
            groups: list[dict[str, Any]] = []
            for page in range(_ARTIST_RG_MAX_PAGES):
                batch, total = await self._mb.get_artist_release_groups(
                    artist_mbid, page * _ARTIST_RG_PAGE, _ARTIST_RG_PAGE, self._priority
                )
                groups.extend(batch)
                if not batch or len(groups) >= total:
                    break
            studio = [
                group
                for group in groups
                if group.get("id")
                and should_include_release(group, ["studio"], ["album"])
            ]
            studio.sort(
                key=lambda group: (
                    group.get("first-release-date") or "9999-99-99",
                    normalize_mb_id(group["id"]),
                )
            )
            self._studio_albums[artist_mbid] = studio
        return self._studio_albums[artist_mbid]


def _identity_rank(identity: _Identity) -> tuple:
    best = best_home(identity.candidates)
    if best is None:
        return (1,)
    return (0, classify_home(best), best.first_release_date or "9999-99-99")


def _track_artist_mbid(identity: _Identity) -> str | None:
    """The artist credited on most of the recording's homes that credit the
    track's artist (Various Artists homes never count)."""
    counts: Counter[str] = Counter()
    for home, candidate in zip(identity.homes, identity.candidates, strict=True):
        if candidate.credits_track_artist and not home.is_various:
            counts.update(home.artist_mbids)
    if not counts:
        return None
    return min(counts, key=lambda artist: (-counts[artist], artist))


def _to_resolution(plan: TrackPlan) -> TrackResolution:
    return TrackResolution(
        target_release_group_mbid=plan.release_group_mbid,
        home_tier=plan.tier.label if plan.tier is not None else None,
        resolution_status=plan.status.value,
        recording_mbid=plan.recording_mbid,
        flag_options=[option.to_json() for option in plan.options] or None,
    )


def _tally(summary: ResolutionSummary, plans: Any) -> None:
    albums: dict[str, set[str]] = {}
    for plan in plans:
        if plan.status is ResolutionStatus.RESOLVED:
            summary.resolved += 1
            if plan.release_group_mbid and plan.tier is not None:
                albums.setdefault(plan.tier.label, set()).add(plan.release_group_mbid)
        elif plan.status is ResolutionStatus.FLAGGED:
            summary.flagged += 1
        else:
            summary.unresolved += 1
    summary.albums_by_tier = {
        tier: len(groups) for tier, groups in sorted(albums.items())
    }


__all__ = [
    "PlaylistTrackResolver",
    "ResolutionSummary",
    "is_owned",
    "same_song",
    "song_title_key",
]
