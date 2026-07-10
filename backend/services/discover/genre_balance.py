"""Genre-balance helpers shared by Discover and the Taste Graph.

A library skewed toward one genre (say 40% K-pop) used to dominate every
recommendation zone because seeds were picked by raw play/album counts.
These pure helpers fix that in three ways:

- ``genre_family``       - normalises genre spellings ("k-pop", "kpop",
                           "korean pop") into one family so caps and prefs
                           apply to the family, not each spelling.
- ``balanced_seed_selection`` - picks seeds round-robin across genre
                           families with sqrt-damped family weights instead
                           of raw frequency order.
- ``cap_genre_share``    - enforces a per-zone share cap (~35%) for any one
                           family, and excludes muted families entirely.
- ``diverse_genre_selection`` - reorders (genre, count) rows for genre-seeded
                           builders (daily mixes) with the same damping.

User control arrives as :class:`GenrePrefs` levels per family:
"reduce" halves a family's weight/allowance, "mute" excludes it.
Items whose genres are unknown are never penalised - they pass through.
"""

from __future__ import annotations

import math
from typing import Any, Callable, Iterable, Sequence, TypeVar

T = TypeVar("T")

# Default per-zone share cap for a single genre family (~a third of a zone).
GENRE_SHARE_CAP = 0.35

GENRE_PREF_LEVELS = ("normal", "reduce", "mute")

# Alias map applied AFTER _canon() (lowercase, separators collapsed to single
# spaces), so one entry covers "k-pop"/"K Pop"/"k_pop". Keep it small and
# focused: the token rules below catch the systematic "k-"/"j-" prefixes.
_FAMILY_ALIASES: dict[str, str] = {
    "kpop": "k-pop",
    "k pop": "k-pop",
    "korean pop": "k-pop",
    "korean idol": "k-pop",
    "jpop": "j-pop",
    "j pop": "j-pop",
    "japanese pop": "j-pop",
    "hiphop": "hip hop",
    "hip hop": "hip hop",
    "rap": "hip hop",
    "rnb": "r&b",
    "r&b": "r&b",
    "r and b": "r&b",
    "contemporary r&b": "r&b",
    "electronica": "electronic",
    "edm": "electronic",
}


def _canon(genre: str) -> str:
    lowered = genre.strip().lower()
    for sep in ("-", "_", "/"):
        lowered = lowered.replace(sep, " ")
    return " ".join(lowered.split())


def genre_family(genre: str | None) -> str:
    """Canonical family name for a genre string; "" when unknown/blank."""
    if not isinstance(genre, str) or not genre.strip():
        return ""
    canon = _canon(genre)
    alias = _FAMILY_ALIASES.get(canon)
    if alias:
        return alias
    tokens = canon.split()
    # "korean ballad", "k rock" style spellings group under the Korean family;
    # same for Japanese. This is deliberately broad: the point of the family is
    # the balance cap, not taxonomy.
    if "korean" in tokens or tokens[0] == "kpop":
        return "k-pop"
    if "japanese" in tokens or tokens[0] == "jpop":
        return "j-pop"
    if len(tokens) > 1 and tokens[0] == "k":
        return "k-pop"
    if len(tokens) > 1 and tokens[0] == "j":
        return "j-pop"
    return canon


class GenrePrefs:
    """Per-user genre-family preference levels ("reduce" / "mute").

    Families not present are "normal". Keys are normalised through
    :func:`genre_family` so any spelling stored still matches.
    """

    __slots__ = ("_levels",)

    def __init__(self, levels: dict[str, str] | None = None) -> None:
        self._levels: dict[str, str] = {}
        for raw_family, level in (levels or {}).items():
            family = genre_family(raw_family)
            if family and level in ("reduce", "mute"):
                self._levels[family] = level

    def is_empty(self) -> bool:
        return not self._levels

    def level(self, family: str) -> str:
        return self._levels.get(family, "normal")

    def is_muted(self, family: str) -> bool:
        return self._levels.get(family) == "mute"

    def multiplier(self, family: str) -> float:
        level = self._levels.get(family)
        if level == "mute":
            return 0.0
        if level == "reduce":
            return 0.5
        return 1.0

    def levels(self) -> dict[str, str]:
        return dict(self._levels)


EMPTY_GENRE_PREFS = GenrePrefs()


def primary_family(genres: Iterable[str] | None) -> str:
    """First known family among an item's genres; "" for unknown."""
    for g in genres or []:
        family = genre_family(g)
        if family:
            return family
    return ""


def balanced_seed_selection(
    candidates: Sequence[T],
    genres_of: Callable[[T], Iterable[str] | None],
    limit: int,
    prefs: GenrePrefs = EMPTY_GENRE_PREFS,
) -> list[T]:
    """Pick up to ``limit`` seeds spread across genre families.

    ``candidates`` arrive in preference order (most-played first). Candidates
    are grouped by primary family; families are ordered by sqrt-damped size
    times the user's pref multiplier (so a 40%-of-library family no longer
    swamps the seed set, and "reduce" halves its priority); seeds are then
    taken round-robin across families. Muted families are excluded. Unknown-
    genre candidates share one "" bucket, so with no genre data at all this
    degrades to the original take-the-first-``limit`` behaviour.
    """
    if limit <= 0 or not candidates:
        return []

    buckets: dict[str, list[T]] = {}
    order: dict[str, int] = {}
    for index, candidate in enumerate(candidates):
        family = primary_family(genres_of(candidate))
        if family and prefs.is_muted(family):
            continue
        buckets.setdefault(family, []).append(candidate)
        order.setdefault(family, index)

    if not buckets:
        return []

    def family_weight(item: tuple[str, list[T]]) -> tuple[float, int]:
        family, members = item
        multiplier = prefs.multiplier(family) if family else 1.0
        return (-(multiplier * math.sqrt(len(members))), order[family])

    ordered_families = [members for _, members in sorted(buckets.items(), key=family_weight)]

    selected: list[T] = []
    round_index = 0
    while len(selected) < limit:
        took_any = False
        for members in ordered_families:
            if round_index < len(members):
                selected.append(members[round_index])
                took_any = True
                if len(selected) >= limit:
                    break
        if not took_any:
            break
        round_index += 1
    return selected


def cap_genre_share(
    items: Sequence[T],
    genres_of: Callable[[T], Iterable[str] | None],
    *,
    cap_ratio: float = GENRE_SHARE_CAP,
    prefs: GenrePrefs = EMPTY_GENRE_PREFS,
    target_size: int | None = None,
    counts: dict[str, int] | None = None,
    total_allowed: int | None = None,
) -> list[T]:
    """Order-preserving filter enforcing mute + a per-family share cap.

    Any single family keeps at most ``max(1, ceil(cap_ratio * size))`` items
    where ``size`` is ``target_size`` (or the input length); a reduced family
    keeps half that. Unknown-genre items always pass. Items skipped for being
    over-cap simply yield their slot to the next (other-genre) items in the
    list - the backfill.

    ``counts``/``total_allowed`` allow a caller to thread page-wide family
    counters through multiple zones so the cap also holds across the whole
    assembled page, not just inside one zone.
    """
    size = target_size if target_size is not None else len(items)
    if size <= 0:
        return list(items)
    base_allowance = max(1, math.ceil(cap_ratio * size))

    local_counts: dict[str, int] = {}
    kept: list[T] = []
    for item in items:
        family = primary_family(genres_of(item))
        if not family:
            kept.append(item)
            continue
        if prefs.is_muted(family):
            continue
        allowance = base_allowance
        if prefs.level(family) == "reduce":
            allowance = max(1, base_allowance // 2)
        if local_counts.get(family, 0) >= allowance:
            continue
        if counts is not None and total_allowed is not None:
            if counts.get(family, 0) >= total_allowed:
                continue
            counts[family] = counts.get(family, 0) + 1
        local_counts[family] = local_counts.get(family, 0) + 1
        kept.append(item)
    return kept


def diverse_genre_selection(
    top_genres: Sequence[tuple[str, int]],
    *,
    limit: int,
    prefs: GenrePrefs = EMPTY_GENRE_PREFS,
    max_per_family: int = 2,
) -> list[tuple[str, int]]:
    """Rebalance (genre, count) rows for genre-seeded builders.

    Counts are sqrt-damped and scaled by the user's pref multiplier, muted
    families drop out, and at most ``max_per_family`` genres of one family
    survive - so five K-pop spellings can't claim five daily-mix clusters.
    Families are interleaved (best genre of each family first) so the result
    leads with breadth.
    """
    if limit <= 0:
        return []

    weighted: list[tuple[float, int, str, int]] = []
    for index, (genre, count) in enumerate(top_genres):
        family = genre_family(genre)
        multiplier = prefs.multiplier(family) if family else 1.0
        if multiplier <= 0.0:
            continue
        weighted.append((multiplier * math.sqrt(max(count, 0) + 1), index, genre, count))
    weighted.sort(key=lambda row: (-row[0], row[1]))

    per_family: dict[str, list[tuple[str, int]]] = {}
    family_order: list[str] = []
    for _, _, genre, count in weighted:
        family = genre_family(genre) or genre
        rows = per_family.setdefault(family, [])
        if family not in family_order:
            family_order.append(family)
        if len(rows) < max_per_family:
            rows.append((genre, count))

    selected: list[tuple[str, int]] = []
    round_index = 0
    while len(selected) < limit:
        took_any = False
        for family in family_order:
            rows = per_family[family]
            if round_index < len(rows):
                selected.append(rows[round_index])
                took_any = True
                if len(selected) >= limit:
                    break
        if not took_any:
            break
        round_index += 1
    return selected


def genres_lookup(
    genres_by_artist: dict[str, list[str]] | None,
) -> Callable[[Any], list[str]]:
    """genres_of adapter for HomeArtist/HomeAlbum/TopPickItem-shaped items,
    resolving through a {artist_mbid_lower: [genres]} map."""
    mapping = genres_by_artist or {}

    def lookup(item: Any) -> list[str]:
        mbid = getattr(item, "artist_mbid", None) or getattr(item, "mbid", None)
        if not mbid:
            album = getattr(item, "album", None)
            if album is not None:
                mbid = getattr(album, "artist_mbid", None)
        if not mbid:
            return []
        return mapping.get(str(mbid).lower(), [])

    return lookup
