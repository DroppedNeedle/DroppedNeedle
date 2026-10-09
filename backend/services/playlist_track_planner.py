"""Choose the release group every playlist track is sourced from (#204).

Pure and deterministic: the resolver gathers each track's candidate homes
(and, for the ``studio`` policy, a studio-album substitute) and this module
decides. The rule is *tier first, then coverage*: every track lands in the
best tier its recording reaches, and within a tier release groups are taken
greedily by how many still-unplaced playlist tracks they carry. A playlist of
Creedence songs therefore lands on their studio albums, and songs only found
on compilations collapse onto the fewest compilations instead of one
compilation per song.
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
    """The same song on one of the artist's studio albums, as a different
    recording (typically the original of a compilation-only remaster)."""

    recording_mbid: str
    home: HomeCandidate


@dataclass(frozen=True, slots=True)
class PlannerTrack:
    track_id: str
    recording_mbid: str | None
    candidates: tuple[HomeCandidate, ...] = ()
    substitute: Substitute | None = None


@dataclass(frozen=True, slots=True)
class HomeOption:
    """One choice offered for a flagged track."""

    kind: str  # "studio" | "compilation"
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


def _assign(tracks: list[PlannerTrack]) -> dict[str, HomeCandidate]:
    """Tier first, then coverage, across every placeable track."""
    by_tier: dict[HomeTier, dict[str, list[HomeCandidate]]] = {}
    for track in tracks:
        tier = _best_tier(track.candidates)
        by_tier.setdefault(tier, {})[track.track_id] = [
            candidate
            for candidate in track.candidates
            if classify_home(candidate) is tier
        ]
    placed: dict[str, HomeCandidate] = {}
    for tier in sorted(by_tier):
        placed.update(_cover_tier(by_tier[tier]))
    return placed


def plan_playlist(
    tracks: Iterable[PlannerTrack],
    policy: CompilationPolicy = CompilationPolicy.MATCH_PLAYLIST,
) -> dict[str, TrackPlan]:
    plans: dict[str, TrackPlan] = {}
    placeable: list[PlannerTrack] = []
    for track in tracks:
        if not track.recording_mbid or not track.candidates:
            plans[track.track_id] = TrackPlan(
                track.track_id, ResolutionStatus.UNRESOLVED, track.recording_mbid
            )
        elif (
            policy is CompilationPolicy.STUDIO
            and track.substitute is not None
            and _best_tier(track.candidates).is_compilation
        ):
            # Swapped out before consolidation so a compilation is never
            # chosen on the strength of tracks that will not come from it.
            substitute = track.substitute
            plans[track.track_id] = TrackPlan(
                track.track_id,
                ResolutionStatus.RESOLVED,
                substitute.recording_mbid,
                substitute.home.release_group_mbid,
                classify_home(substitute.home),
            )
        else:
            placeable.append(track)

    assigned = _assign(placeable)
    for track in placeable:
        home = assigned[track.track_id]
        tier = classify_home(home)
        recording = track.recording_mbid or ""
        if not tier.is_compilation or policy is not CompilationPolicy.FLAG:
            plans[track.track_id] = TrackPlan(
                track.track_id,
                ResolutionStatus.RESOLVED,
                recording,
                home.release_group_mbid,
                tier,
            )
            continue
        options: list[HomeOption] = []
        if track.substitute is not None:
            substitute = track.substitute
            options.append(
                HomeOption(
                    "studio",
                    substitute.recording_mbid,
                    substitute.home.release_group_mbid,
                    substitute.home.title,
                    classify_home(substitute.home),
                )
            )
        options.append(
            HomeOption(
                "compilation", recording, home.release_group_mbid, home.title, tier
            )
        )
        plans[track.track_id] = TrackPlan(
            track.track_id,
            ResolutionStatus.FLAGGED,
            recording,
            options=tuple(options),
        )
    return plans
