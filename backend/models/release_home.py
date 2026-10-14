"""Classify the release groups a recording appears on into source "homes" (#204).

A track request still lands in the folder of one release group, so a playlist
of an artist's songs should be sourced from that artist's own albums rather
than whichever of dozens of compilations each track happens to resolve to.
Every candidate release group gets a :class:`HomeTier`; lower is preferred.

Version (live, remix, demo, ...) is deliberately *not* part of the tier: every
candidate of one recording carries the same audio, so the version only matters
when a *different* recording is substituted (see :func:`recording_version_flags`).
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from enum import IntEnum, StrEnum
from typing import Any

import msgspec

VARIOUS_ARTISTS_MBID = "89ad4ac3-39f7-470e-963a-56509c546377"
_VA_NAMES = frozenset({"various artists", "various", "va"})

VERSION_CHANGING_TYPES = frozenset(
    {"live", "remix", "dj-mix", "mixtape/street", "demo"}
)
_COMPILATION = "compilation"
_SOUNDTRACK = "soundtrack"


class HomeTier(IntEnum):
    """Where a track can be sourced from, best first."""

    ALBUM = 0
    """The artist's own album (studio, or live/remix matching the recording)."""
    EP = 1
    SINGLE = 2
    SOUNDTRACK = 3
    ARTIST_COMPILATION = 4
    """A compilation credited to the track's artist, e.g. a greatest-hits."""
    OTHER_COMPILATION = 5
    """Various Artists, or a compilation credited to someone else."""
    OTHER = 6
    """Unofficial releases, another artist's album, spoken word, unknown types."""

    @property
    def label(self) -> str:
        return self.name.lower()

    @classmethod
    def from_label(cls, label: str | None) -> HomeTier | None:
        if not label:
            return None
        try:
            return cls[label.strip().upper()]
        except KeyError:
            return None

    @property
    def is_compilation(self) -> bool:
        return self in (HomeTier.ARTIST_COMPILATION, HomeTier.OTHER_COMPILATION)


class ResolutionStatus(StrEnum):
    """Stored per playlist entry in ``playlist_tracks.resolution_status``."""

    RESOLVED = "resolved"
    FLAGGED = "flagged"
    """Only compilations carry the exact recording; waiting on the user."""
    UNRESOLVED = "unresolved"
    """No recording, or a recording on no release group."""
    CHOSEN = "chosen"
    """The user picked the home; the resolver never overrides it."""


class CompilationPolicy(StrEnum):
    """What to do when a track's exact recording only exists on compilations."""

    MATCH_PLAYLIST = "match_playlist"
    """Keep the recording; consolidate onto as few compilations as possible."""
    STUDIO = "studio"
    """Swap to the same song on one of the artist's studio albums, if found."""
    FLAG = "flag"
    """Hold the track and offer the options to the user."""


class HomeCandidate(msgspec.Struct, frozen=True):
    """One release group a recording appears on, reduced to what tiering needs.

    ``official`` is True when any release of the group the recording is on is
    Official. ``credits_track_artist`` is decided by the caller (fuzzy artist
    matching lives in the repository layer); pass True when there is no
    artist to compare against.
    """

    release_group_mbid: str
    title: str
    primary_type: str | None
    secondary_types: tuple[str, ...]
    official: bool
    is_various: bool
    credits_track_artist: bool
    first_release_date: str | None = None


class RecordingHome(msgspec.Struct, frozen=True):
    """A release group a recording appears on, independent of any playlist.

    This is what is persisted per recording; ``credits_track_artist`` depends
    on the track being placed, so it is decided per track via
    :meth:`to_candidate`.
    """

    release_group_mbid: str
    title: str
    primary_type: str | None
    secondary_types: tuple[str, ...]
    official: bool
    is_various: bool
    artist_credit: str
    first_release_date: str | None = None
    artist_mbids: tuple[str, ...] = ()
    release_mbid: str | None = None
    """An edition of this group that carries the recording (official, then
    earliest), so a track request can name it instead of the group's default
    edition, which may not hold this recording at all."""

    def to_candidate(self, *, credits_track_artist: bool) -> HomeCandidate:
        return HomeCandidate(
            release_group_mbid=self.release_group_mbid,
            title=self.title,
            primary_type=self.primary_type,
            secondary_types=self.secondary_types,
            official=self.official,
            is_various=self.is_various,
            credits_track_artist=credits_track_artist,
            first_release_date=self.first_release_date,
        )


def release_group_cover_url(release_group_mbid: str) -> str:
    """The cover URL a playlist row shows for the album it is filed under."""
    return f"/api/v1/covers/release-group/{release_group_mbid}?size=250"


def _fold(value: Any) -> str:
    return str(value).strip().casefold() if value is not None else ""


def is_various_artist_credit(
    artist_credit: Iterable[Mapping[str, Any]] | None,
    artist_name: str | None = None,
) -> bool:
    """True when a MusicBrainz ``artist-credit`` list (or display name) is VA."""
    for credit in artist_credit or ():
        artist = credit.get("artist") or {}
        if _fold(artist.get("id")) == VARIOUS_ARTISTS_MBID:
            return True
    return _fold(artist_name) in _VA_NAMES


def version_flags(secondary_types: Iterable[str]) -> frozenset[str]:
    """The version-changing secondary types of a release group."""
    return frozenset(
        folded
        for item in secondary_types
        if (folded := _fold(item)) in VERSION_CHANGING_TYPES
    )


def classify_home(candidate: HomeCandidate) -> HomeTier:
    secondary = {
        _fold(item) for item in candidate.secondary_types
    } - VERSION_CHANGING_TYPES
    secondary.discard("")
    if not candidate.official:
        return HomeTier.OTHER
    if candidate.is_various:
        return HomeTier.OTHER_COMPILATION
    if _COMPILATION in secondary:
        if candidate.credits_track_artist:
            return HomeTier.ARTIST_COMPILATION
        return HomeTier.OTHER_COMPILATION
    if _SOUNDTRACK in secondary:
        return HomeTier.SOUNDTRACK
    if secondary or not candidate.credits_track_artist:
        return HomeTier.OTHER
    return {
        "album": HomeTier.ALBUM,
        "ep": HomeTier.EP,
        "single": HomeTier.SINGLE,
    }.get(_fold(candidate.primary_type), HomeTier.OTHER)


def home_sort_key(candidate: HomeCandidate) -> tuple[int, str, str]:
    """Deterministic per-track preference: tier, then earliest, then MBID."""
    return (
        classify_home(candidate),
        candidate.first_release_date or "9999-99-99",
        _fold(candidate.release_group_mbid),
    )


def recording_version_flags(candidates: Iterable[HomeCandidate]) -> frozenset[str]:
    """Infer a recording's version from the non-compilation groups it is on.

    Compilations rarely carry a ``Live``/``Remix`` secondary type even when the
    track on them is live, so only the recording's own releases decide. A
    recording on any un-flagged own release is treated as the studio version.
    """
    flag_sets = [
        version_flags(candidate.secondary_types)
        for candidate in candidates
        if not candidate.is_various
        and _COMPILATION not in {_fold(item) for item in candidate.secondary_types}
    ]
    if not flag_sets:
        return frozenset()
    return frozenset.intersection(*flag_sets)


def best_home(candidates: Iterable[HomeCandidate]) -> HomeCandidate | None:
    return min(candidates, key=home_sort_key, default=None)
