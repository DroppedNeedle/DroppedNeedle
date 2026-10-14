"""Choose the album every playlist track is filed under (#204).

Pure and deterministic: the resolver gathers each track's candidate homes,
the album its playlist lists (``playlist_album``) and, when the recording has
no official home, the same song on the artist's own album (``substitute``).
The compilation policy is the strategy for every track:

- ``match_playlist`` mirrors the source: the album the playlist lists,
  compilations included; otherwise the official choice.
- ``studio`` keeps a clean library: the artist's official release (album,
  then EP, single, soundtrack; earliest), never a compilation unless nothing
  official exists.
- ``flag`` files tracks where both agree and holds the rest with both options.

Albums are created as needed, one per release a track belongs to. Within a
tier, the greedy "most playlist tracks" cover only breaks ties, so a second
song from an album already chosen joins it.
"""

from __future__ import annotations

import heapq
from collections.abc import Iterable
from dataclasses import dataclass, field
from typing import Any

from models.release_home import (
    CompilationPolicy,
    HomeCandidate,
    HomeTier,
    ResolutionStatus,
    classify_home,
    home_sort_key,
)


@dataclass(frozen=True, slots=True)
class Substitute:
    """A specific album for the song, possibly as a different recording: the
    artist's own album for a compilation-only recording, or the album the
    playlist lists when it was found by search rather than among the
    recording's homes."""

    recording_mbid: str
    home: HomeCandidate


@dataclass(frozen=True, slots=True)
class PlannerTrack:
    track_id: str
    recording_mbid: str | None
    candidates: tuple[HomeCandidate, ...] = ()
    substitute: Substitute | None = None
    playlist_album: Substitute | None = None


@dataclass(frozen=True, slots=True)
class HomeOption:
    """One choice offered for a flagged track."""

    kind: str  # "studio" (official) | "playlist" (the playlist's album)
    recording_mbid: str
    release_group_mbid: str
    title: str
    tier: HomeTier

    def to_json(self) -> dict[str, Any]:
        return {
            "kind": self.kind,
            "recording_mbid": self.recording_mbid,
            "release_group_mbid": self.release_group_mbid,
            "title": self.title,
            "tier": self.tier.label,
        }


@dataclass(frozen=True, slots=True)
class TrackPlan:
    track_id: str
    status: ResolutionStatus
    recording_mbid: str | None = None
    release_group_mbid: str | None = None
    tier: HomeTier | None = None
    options: tuple[HomeOption, ...] = field(default=())


def _best_tier(candidates: Iterable[HomeCandidate]) -> HomeTier:
    return min(classify_home(candidate) for candidate in candidates)


_OFFICIAL_TIERS = frozenset(
    {HomeTier.ALBUM, HomeTier.EP, HomeTier.SINGLE, HomeTier.SOUNDTRACK}
)


def _is_official(home: HomeCandidate) -> bool:
    return classify_home(home) in _OFFICIAL_TIERS


def _cover_tier(
    pending: dict[str, list[HomeCandidate]],
) -> dict[str, HomeCandidate]:
    """Greedy set cover of ``pending`` tracks (already filtered to one tier).

    Repeatedly takes the release group carrying the most unplaced tracks;
    ties go to the earliest release, then the lowest MBID. Coverage only
    shrinks, so a heap with lazily refreshed counts gives the same picks as
    recounting every round, in O(candidates * log) instead of O(groups * candidates).
    """
    members: dict[str, set[str]] = {}
    representative: dict[str, HomeCandidate] = {}
    for track_id, candidates in pending.items():
        for candidate in candidates:
            key = candidate.release_group_mbid.casefold()
            members.setdefault(key, set()).add(track_id)
            current = representative.get(key)
            if current is None or home_sort_key(candidate) < home_sort_key(current):
                representative[key] = candidate
    coverage = {key: len(tracks) for key, tracks in members.items()}
    heap = [
        (-coverage[key], home_sort_key(representative[key]), key) for key in members
    ]
    heapq.heapify(heap)

    placed: dict[str, HomeCandidate] = {}
    while heap and len(placed) < len(pending):
        negative, _sort_key, key = heapq.heappop(heap)
        if -negative != coverage[key]:
            if coverage[key]:
                heapq.heappush(heap, (-coverage[key], _sort_key, key))
            continue
        for track_id in members[key]:
            if track_id in placed:
                continue
            placed[track_id] = representative[key]
            for candidate in pending[track_id]:
                coverage[candidate.release_group_mbid.casefold()] -= 1
    return placed


def _assign(pool: dict[str, list[HomeCandidate]]) -> dict[str, HomeCandidate]:
    """Tier first, then coverage, over each track's candidates in ``pool``."""
    by_tier: dict[HomeTier, dict[str, list[HomeCandidate]]] = {}
    for track_id, candidates in pool.items():
        if not candidates:
            continue
        tier = _best_tier(candidates)
        by_tier.setdefault(tier, {})[track_id] = [
            candidate for candidate in candidates if classify_home(candidate) is tier
        ]
    placed: dict[str, HomeCandidate] = {}
    for tier in sorted(by_tier):
        placed.update(_cover_tier(by_tier[tier]))
    return placed


@dataclass(frozen=True, slots=True)
class _Choice:
    recording_mbid: str
    home: HomeCandidate

    @property
    def release_group_mbid(self) -> str:
        return self.home.release_group_mbid.casefold()


def _official_choices(tracks: list[PlannerTrack]) -> dict[str, _Choice]:
    """Each track's official release: the recording's own best official home
    (coverage breaks ties), else the playlist's album when that is official,
    else the same song on the artist's own album."""
    pool = {
        track.track_id: [c for c in track.candidates if _is_official(c)]
        for track in tracks
    }
    assigned = _assign(pool)
    choices: dict[str, _Choice] = {}
    for track in tracks:
        if track.track_id in assigned:
            choices[track.track_id] = _Choice(
                track.recording_mbid or "", assigned[track.track_id]
            )
            continue
        for fallback in (track.playlist_album, track.substitute):
            if fallback is not None and _is_official(fallback.home):
                choices[track.track_id] = _Choice(
                    fallback.recording_mbid, fallback.home
                )
                break
    return choices


def _playlist_choice(track: PlannerTrack) -> _Choice | None:
    album = track.playlist_album
    return _Choice(album.recording_mbid, album.home) if album is not None else None


def _resolved(track_id: str, choice: _Choice) -> TrackPlan:
    return TrackPlan(
        track_id,
        ResolutionStatus.RESOLVED,
        choice.recording_mbid,
        choice.home.release_group_mbid,
        classify_home(choice.home),
    )


def _option(kind: str, choice: _Choice) -> HomeOption:
    return HomeOption(
        kind,
        choice.recording_mbid,
        choice.home.release_group_mbid,
        choice.home.title,
        classify_home(choice.home),
    )


def plan_playlist(
    tracks: Iterable[PlannerTrack],
    policy: CompilationPolicy = CompilationPolicy.MATCH_PLAYLIST,
) -> dict[str, TrackPlan]:
    plans: dict[str, TrackPlan] = {}
    placeable: list[PlannerTrack] = []
    for track in tracks:
        if not track.recording_mbid or not (
            track.candidates or track.playlist_album or track.substitute
        ):
            plans[track.track_id] = TrackPlan(
                track.track_id, ResolutionStatus.UNRESOLVED, track.recording_mbid
            )
        else:
            placeable.append(track)

    official = _official_choices(placeable)
    needs_fallback: list[PlannerTrack] = []
    for track in placeable:
        studio = official.get(track.track_id)
        playlist = _playlist_choice(track)
        if policy is CompilationPolicy.MATCH_PLAYLIST:
            choice = playlist or studio
        elif policy is CompilationPolicy.STUDIO:
            choice = studio
        elif (
            studio is not None
            and playlist is not None
            and studio.release_group_mbid != playlist.release_group_mbid
        ):
            plans[track.track_id] = TrackPlan(
                track.track_id,
                ResolutionStatus.FLAGGED,
                track.recording_mbid,
                options=(_option("studio", studio), _option("playlist", playlist)),
            )
            continue
        else:
            choice = studio or playlist
        if choice is None:
            needs_fallback.append(track)
        else:
            plans[track.track_id] = _resolved(track.track_id, choice)

    # Nothing official and no playlist album: the recording's remaining homes
    # (compilations, then anything else), as few of them as possible.
    leftover = _assign(
        {track.track_id: list(track.candidates) for track in needs_fallback}
    )
    for track in needs_fallback:
        home = leftover.get(track.track_id)
        if home is None:
            plans[track.track_id] = TrackPlan(
                track.track_id, ResolutionStatus.UNRESOLVED, track.recording_mbid
            )
        else:
            plans[track.track_id] = _resolved(
                track.track_id, _Choice(track.recording_mbid or "", home)
            )
    return plans
