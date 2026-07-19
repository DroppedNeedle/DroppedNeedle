"""AlbumIdentifier - per-folder track-list identification (Lidarr/beets-style)."""

import logging
import re
from collections import Counter
from typing import TYPE_CHECKING

from rapidfuzz.distance import Levenshtein

from infrastructure.msgspec_fastapi import AppStruct
from infrastructure.queue.priority_queue import RequestPriority
from repositories.musicbrainz_base import extract_artist_name
from services.native.musicbrainz_matcher import MusicBrainzMatcher

if TYPE_CHECKING:
    from repositories.musicbrainz_repository import MusicBrainzRepository

logger = logging.getLogger(__name__)

ALBUM_ACCEPT_THRESHOLD = 0.20
MIN_MAPPED_FRACTION = 0.6
ARTIST_ACCEPT_FLOOR = 0.6

_WEIGHTS = {
    "artist": 3.0,
    "album": 3.0,
    "year": 1.0,
    "tracks": 2.0,
    "missing_tracks": 0.6,
    "unmatched_tracks": 0.9,
    "track_title": 3.0,
    "track_length": 2.0,
    "track_index": 1.0,
    "recording_id": 10.0,
    "release_type": 3.0,
}

# Phase 3 (ScannerAlbumIdentity): prefer a studio Album over a compilation/live/other when
# the same recordings appear on several release groups, so a folder isn't attributed to a
# compilation. A ranking-only signal - excluded from the acceptance gate so a genuine
# compilation folder still matches its compilation.
_STUDIO_ALBUM = "album"
_COMP_OR_LIVE_TYPES = frozenset({"compilation", "live"})

_DURATION_GRACE_S = 10.0
_DURATION_WINDOW_S = 30.0

_PAD_COST = 2.0

_MAX_CANDIDATE_RGS = 10
_ALBUM_SEARCH_LIMIT = 8
_TRACK_SAMPLE = 4

_VA_MBID = "89ad4ac3-39f7-470e-963a-56509c546377"
# Credit names that mean Various Artists. Deliberately no "": an empty credit is
# an unknown identity, not a VA claim - treating it as VA filed 197 single-artist
# albums under /Various Artists/ (2026-07-19 incident).
_VA_NAMES = {"various artists", "various", "va"}

_NON_ALNUM = re.compile(r"[\W_]+", re.UNICODE)


class LocalTrack(AppStruct):
    """One audio file in a folder, projected to what the matcher needs."""

    path: str
    title: str
    artist: str
    album: str
    track_number: int = 0
    disc_number: int = 1
    year: int | None = None
    duration_seconds: float | None = None
    recording_mbid: str | None = None


class MBTrack(AppStruct):
    title: str
    position: int
    disc: int
    absolute_position: int
    length_ms: int | None = None
    recording_mbid: str | None = None


class _ReleaseMeta(AppStruct):
    release_group_mbid: str
    release_mbid: str
    album_title: str
    artist: str
    is_various: bool
    artist_mbid: str | None = None
    year: int | None = None
    primary_type: str | None = None
    secondary_types: frozenset[str] = frozenset()


class AlbumMatch(AppStruct):
    accepted: bool
    distance: float
    release_group_mbid: str
    release_mbid: str
    assignments: dict[str, str]
    artist_mbid: str | None = None
    artist_name: str | None = None


def _clean(text: str) -> str:
    """Lidarr-style fold for the edit-distance metric."""
    folded = MusicBrainzMatcher._fold(text or "").lower()
    return _NON_ALNUM.sub("", folded)


def _string_penalty(a: str, b: str) -> float:
    """1 - Levenshtein coefficient over the cleaned strings."""
    ca, cb = _clean(a), _clean(b)
    if not ca and not cb:
        return 0.0
    if not ca or not cb:
        return 1.0
    return Levenshtein.normalized_distance(ca, cb)


class _Distance:
    """Per-key penalty lists, normalised like Lidarr's Distance."""

    def __init__(self) -> None:
        self._d: dict[str, list[float]] = {}

    def add(self, key: str, penalty: float) -> None:
        self._d.setdefault(key, []).append(penalty)

    def add_string(self, key: str, a: str, b: str) -> None:
        self.add(key, _string_penalty(a, b))

    def add_bool(self, key: str, mismatch: bool) -> None:
        self.add(key, 1.0 if mismatch else 0.0)

    def add_ratio(self, key: str, value: float, target: float) -> None:
        if target <= 0:
            return
        self.add(key, max(0.0, min(value, target)) / target)

    def normalized(self, exclude: tuple[str, ...] = ()) -> float:
        num = den = 0.0
        for key, penalties in self._d.items():
            if key in exclude:
                continue
            weight = _WEIGHTS.get(key, 1.0)
            num += sum(penalties) * weight
            den += len(penalties) * weight
        return num / den if den > 0 else 1.0


def track_distance(local: LocalTrack, mb: MBTrack) -> _Distance:
    """Per-pair cost over title, duration, position, recording MBID."""
    d = _Distance()
    d.add_string("track_title", local.title, mb.title)
    if mb.length_ms and local.duration_seconds:
        diff = abs(local.duration_seconds - mb.length_ms / 1000.0) - _DURATION_GRACE_S
        d.add_ratio("track_length", diff, _DURATION_WINDOW_S)
    if local.track_number > 0 and mb.absolute_position > 0:
        matches = local.track_number == mb.absolute_position or (
            mb.position > 0 and local.track_number == mb.position
        )
        d.add_bool("track_index", not matches)
    if local.recording_mbid and mb.recording_mbid:
        d.add_bool("recording_id", local.recording_mbid != mb.recording_mbid)
    return d


def _hungarian(cost: list[list[float]]) -> list[int]:
    """Minimum-cost perfect assignment on a square matrix (Kuhn–Munkres)."""
    n = len(cost)
    if n == 0:
        return []
    inf = float("inf")
    u = [0.0] * (n + 1)
    v = [0.0] * (n + 1)
    p = [0] * (n + 1)
    way = [0] * (n + 1)
    for i in range(1, n + 1):
        p[0] = i
        j0 = 0
        minv = [inf] * (n + 1)
        used = [False] * (n + 1)
        while True:
            used[j0] = True
            i0 = p[j0]
            delta = inf
            j1 = -1
            for j in range(1, n + 1):
                if used[j]:
                    continue
                cur = cost[i0 - 1][j - 1] - u[i0] - v[j]
                if cur < minv[j]:
                    minv[j] = cur
                    way[j] = j0
                if minv[j] < delta:
                    delta = minv[j]
                    j1 = j
            for j in range(n + 1):
                if used[j]:
                    u[p[j]] += delta
                    v[j] -= delta
                else:
                    minv[j] -= delta
            j0 = j1
            if p[j0] == 0:
                break
        while j0:
            j1 = way[j0]
            p[j0] = p[j1]
            j0 = j1
    assignment = [0] * n
    for j in range(1, n + 1):
        if p[j]:
            assignment[p[j] - 1] = j - 1
    return assignment


def assign_tracks(
    locals_: list[LocalTrack], mb_tracks: list[MBTrack]
) -> tuple[dict[int, int], list[int], list[int], dict[tuple[int, int], _Distance]]:
    """Optimally map local files to release tracks."""
    n_local, n_mb = len(locals_), len(mb_tracks)
    if n_local == 0 or n_mb == 0:
        return {}, list(range(n_local)), list(range(n_mb)), {}
    size = max(n_local, n_mb)
    cost = [[_PAD_COST] * size for _ in range(size)]
    cache: dict[tuple[int, int], _Distance] = {}
    for i in range(n_local):
        for j in range(n_mb):
            d = track_distance(locals_[i], mb_tracks[j])
            cache[(i, j)] = d
            cost[i][j] = d.normalized()
    assignment = _hungarian(cost)
    mapping: dict[int, int] = {}
    local_extra: list[int] = []
    used_mb: set[int] = set()
    for i in range(n_local):
        j = assignment[i]
        if j < n_mb:
            mapping[i] = j
            used_mb.add(j)
        else:
            local_extra.append(i)
    mb_extra = [j for j in range(n_mb) if j not in used_mb]
    return mapping, local_extra, mb_extra, cache


def _most_common(values: list[str]) -> str:
    cleaned = [v.strip() for v in values if v and v.strip()]
    return Counter(cleaned).most_common(1)[0][0] if cleaned else ""


def _most_common_year(values: list[int | None]) -> int | None:
    years = [y for y in values if y]
    return Counter(years).most_common(1)[0][0] if years else None


def _release_type_penalty(meta: "_ReleaseMeta") -> float:
    """Prefer a studio Album over a compilation/live/other for the same recordings."""
    if meta.secondary_types & _COMP_OR_LIVE_TYPES:
        return 1.0  # compilation / live - strongly deprioritised
    primary = (meta.primary_type or "").lower()
    if primary == _STUDIO_ALBUM:
        return 0.0  # studio album - preferred
    if primary == "ep":
        return 0.3
    return 0.5  # single / broadcast / other / unknown


def album_distance(
    locals_: list[LocalTrack],
    mb_tracks: list[MBTrack],
    meta: _ReleaseMeta,
    mapping: dict[int, int],
    local_extra: list[int],
    mb_extra: list[int],
    track_dists: dict[tuple[int, int], _Distance],
) -> _Distance:
    """Weighted album-level distance."""
    d = _Distance()
    if not meta.is_various:
        d.add_string("artist", _most_common([t.artist for t in locals_]), meta.artist)
    d.add_string("album", _most_common([t.album for t in locals_]), meta.album_title)
    local_year = _most_common_year([t.year for t in locals_])
    if local_year and meta.year:
        d.add_ratio("year", abs(local_year - meta.year), 10.0)
    for i, j in mapping.items():
        d.add("tracks", track_dists[(i, j)].normalized())
    cap = len(locals_)
    for _ in range(min(len(mb_extra), cap)):
        d.add("missing_tracks", 1.0)
    for _ in range(min(len(local_extra), cap)):
        d.add("unmatched_tracks", 1.0)
    d.add("release_type", _release_type_penalty(meta))
    return d


def score_release(
    locals_: list[LocalTrack], mb_tracks: list[MBTrack], meta: _ReleaseMeta
) -> AlbumMatch:
    """Assign + score one candidate release."""
    mapping, local_extra, mb_extra, track_dists = assign_tracks(locals_, mb_tracks)
    dist = album_distance(
        locals_, mb_tracks, meta, mapping, local_extra, mb_extra, track_dists
    )
    # Missing tracks (a release we only partly hold) count neither for ranking nor
    # acceptance, so an incomplete studio album isn't out-ranked by a smaller release.
    # release_type ranks (studio Album > compilation/live) but must NOT gate acceptance,
    # or a genuine compilation folder would fail to match its own compilation.
    full = dist.normalized(exclude=("missing_tracks",))
    gate = dist.normalized(exclude=("missing_tracks", "release_type"))
    mapped_fraction = len(mapping) / len(locals_) if locals_ else 0.0
    accepted = (
        gate <= ALBUM_ACCEPT_THRESHOLD
        and mapped_fraction >= MIN_MAPPED_FRACTION
        and _artist_ok(locals_, meta)
    )
    assignments = {
        locals_[i].path: (mb_tracks[j].recording_mbid or "") for i, j in mapping.items()
    }
    return AlbumMatch(
        accepted=accepted,
        distance=round(full, 4),
        release_group_mbid=meta.release_group_mbid,
        release_mbid=meta.release_mbid,
        assignments=assignments,
        artist_mbid=None if meta.is_various else meta.artist_mbid,
        artist_name=None if meta.is_various else (meta.artist or None),
    )


def _artist_ok(locals_: list[LocalTrack], meta: _ReleaseMeta) -> bool:
    """A clearly-different artist blocks acceptance."""
    if meta.is_various or not meta.artist:
        return True
    penalty = _string_penalty(_most_common([t.artist for t in locals_]), meta.artist)
    return penalty <= (1.0 - ARTIST_ACCEPT_FLOOR)


class AlbumIdentifier:
    def __init__(self, mb_repo: "MusicBrainzRepository") -> None:
        self._mb_repo = mb_repo

    async def identify(
        self, locals_: list[LocalTrack], *, seed_release_groups: list[str] | None = None
    ) -> AlbumMatch | None:
        """Identify a folder's release, or None if nothing clears the gate.

        ``seed_release_groups`` are scored first and unconditionally: the scanner passes
        the release groups its AUDIO FINGERPRINTS resolved to, so a folder whose tags are
        junk (wrong album, compilation track numbers) still gets its real album evaluated.
        A tag-derived text search alone would never surface it - which is exactly how one
        folder's files used to scatter across release groups."""
        if len(locals_) < 2:
            return None
        target_count = len(locals_)
        rg_ids = await self._candidate_release_groups(locals_, seed_release_groups)
        best: AlbumMatch | None = None
        for rg_id in rg_ids:
            release = await self._best_release(rg_id, target_count)
            if release is None:
                continue
            meta, mb_tracks = release
            if not mb_tracks:
                continue
            match = score_release(locals_, mb_tracks, meta)
            # Choose among ACCEPTED candidates only, by ranking distance (which now favours
            # a studio Album over a compilation/live for the same recordings). This stops a
            # type-preferred-but-poorly-matching album from shadowing a well-matching one.
            if not match.accepted:
                continue
            if best is None or match.distance < best.distance:
                best = match
            if best.distance == 0.0:
                break
        return best

    async def release_group_type(
        self, release_group_mbid: str
    ) -> tuple[str | None, frozenset[str]]:
        """``(primary_type, {secondary_types})`` for a release group, both lower-cased;
        e.g. ``("album", frozenset())`` is a studio album, ``("album", {"compilation"})``
        a compilation. Reads the release-group detail the matcher already fetches (cached
        1h at the repo). Fails open to ``(None, frozenset())`` - type is advisory."""
        if not release_group_mbid:
            return None, frozenset()
        try:
            detail = await self._mb_repo.get_release_group_by_id(
                release_group_mbid, priority=RequestPriority.BACKGROUND_SYNC
            )
        except Exception as exc:  # noqa: BLE001 - type is advisory; fail open
            logger.warning("RG type fetch failed for %s: %s", release_group_mbid, exc)
            return None, frozenset()
        if not detail:
            return None, frozenset()
        primary = (detail.get("primary-type") or "").lower() or None
        secondary = frozenset(s.lower() for s in (detail.get("secondary-types") or []))
        return primary, secondary

    async def resolve_release_group_artist(
        self, release_group_mbid: str
    ) -> tuple[str | None, str | None]:
        """The canonical primary artist (MBID, name) of a release group."""
        if not release_group_mbid:
            return None, None
        try:
            detail = await self._mb_repo.get_release_group_by_id(
                release_group_mbid,
                includes=["artist-credits"],
                priority=RequestPriority.BACKGROUND_SYNC,
            )
        except Exception as exc:  # noqa: BLE001 - leave the tag-derived artist untouched
            logger.warning("Artist resolve failed for %s: %s", release_group_mbid, exc)
            return None, None
        if not detail:
            return None, None
        credit = detail.get("artist-credit") or []
        if not credit:
            return None, None
        artist = credit[0].get("artist") or {}
        mbid = artist.get("id")
        name = artist.get("name")
        return (mbid or None), (name or None)

    async def _candidate_release_groups(
        self, locals_: list[LocalTrack], seed_release_groups: list[str] | None = None
    ) -> list[str]:
        """Release groups to score: fingerprint-derived seeds first (audio truth), then
        album-title and per-track recording searches ranked by recurrence."""
        seeds = list(dict.fromkeys(m for m in (seed_release_groups or []) if m))
        seed_set = set(seeds)
        artist = _most_common([t.artist for t in locals_])
        album = _most_common([t.album for t in locals_])
        order: list[str] = []
        freq: Counter[str] = Counter()

        def bump(mbid: str | None) -> None:
            if not mbid or mbid in seed_set:
                return
            if mbid not in freq:
                order.append(mbid)
            freq[mbid] += 1

        if artist and album:
            try:
                for result in await self._mb_repo.search_albums(
                    f"{artist} {album}".strip(),
                    limit=_ALBUM_SEARCH_LIMIT,
                    include_all_types=True,
                    priority=RequestPriority.BACKGROUND_SYNC,
                ):
                    bump(result.musicbrainz_id)
            except Exception as exc:  # noqa: BLE001 - fail open to recording search
                logger.warning("Album-candidate search failed: %s", exc)

        for local in _sample(locals_, _TRACK_SAMPLE):
            if not local.title:
                continue
            try:
                recordings = await self._mb_repo.search_recordings(
                    artist or local.artist,
                    local.title,
                    limit=5,
                    priority=RequestPriority.BACKGROUND_SYNC,
                )
            except Exception as exc:  # noqa: BLE001 - one bad track must not abort
                logger.warning("Recording-candidate search failed: %s", exc)
                continue
            for rec in recordings:
                for group in rec.release_groups:
                    bump(group.release_group_mbid)

        text_ranked = sorted(set(order), key=lambda mbid: -freq[mbid])
        return (seeds + text_ranked)[:_MAX_CANDIDATE_RGS]

    async def release_tracks(
        self, rg_id: str, target_count: int
    ) -> tuple[_ReleaseMeta, list[MBTrack]] | None:
        """Public wrapper over ``_best_release``: the release-group's best-fitting
        release and its tracklist, for callers (the drop importer) that need the
        per-track metadata behind an identified or manually chosen release group."""
        return await self._best_release(rg_id, target_count)

    async def _best_release(
        self, rg_id: str, target_count: int
    ) -> tuple[_ReleaseMeta, list[MBTrack]] | None:
        """Pick the release whose track count is closest to the folder's, then fetch its tracklist."""
        try:
            # "artist-credits" is load-bearing: without it MB omits the credit and
            # every album's artist read as "" (see the _VA_NAMES note).
            detail = await self._mb_repo.get_release_group_by_id(
                rg_id,
                includes=["artist-credits", "releases", "media"],
                priority=RequestPriority.BACKGROUND_SYNC,
            )
        except Exception as exc:  # noqa: BLE001
            logger.warning("Release-group detail fetch failed for %s: %s", rg_id, exc)
            return None
        if not detail:
            return None
        rg_title = detail.get("title", "") or ""
        rg_artist = extract_artist_name(detail) or ""
        primary_type = (detail.get("primary-type") or "").lower() or None
        secondary_types = frozenset(
            s.lower() for s in (detail.get("secondary-types") or [])
        )
        credit = detail.get("artist-credit") or []
        rg_artist_mbid = (credit[0].get("artist") or {}).get("id") if credit else None
        if not rg_artist:
            # The credit can still be missing (genuinely uncredited MB data, a
            # transient hiccup); it is knowable, so resolve it rather than proceed
            # with an empty identity.
            resolved_mbid, resolved_name = await self.resolve_release_group_artist(
                rg_id
            )
            rg_artist = resolved_name or ""
            rg_artist_mbid = rg_artist_mbid or resolved_mbid
        is_various = self._is_various(detail, rg_artist_mbid, rg_artist)

        scored: list[tuple[int, int, str, str]] = []
        fallback_id: str | None = None
        for rel in detail.get("releases") or []:
            rel_id = rel.get("id")
            if not rel_id:
                continue
            if fallback_id is None:
                fallback_id = rel_id
            count = sum(int(m.get("track-count") or 0) for m in (rel.get("media") or []))
            if count <= 0:
                continue
            official = 0 if rel.get("status") == "Official" else 1
            scored.append((abs(count - target_count), official, rel.get("date") or "9999", rel_id))
        if scored:
            scored.sort()
            release_id = scored[0][3]
        elif fallback_id is not None:
            release_id = fallback_id
        else:
            return None

        try:
            release = await self._mb_repo.get_release_by_id(
                release_id, includes=["recordings"], priority=RequestPriority.BACKGROUND_SYNC
            )
        except Exception as exc:  # noqa: BLE001
            logger.warning("Release fetch failed for %s: %s", release_id, exc)
            return None
        if not release:
            return None

        meta = _ReleaseMeta(
            release_group_mbid=rg_id,
            release_mbid=release_id,
            album_title=rg_title,
            artist=rg_artist,
            is_various=is_various,
            artist_mbid=rg_artist_mbid or None,
            year=_parse_year(release.get("date")),
            primary_type=primary_type,
            secondary_types=secondary_types,
        )
        return meta, _build_mb_tracks(release)

    @staticmethod
    def _is_various(rg_detail: dict, artist_mbid: str | None, artist_name: str) -> bool:
        """Whether the release group is ACTUALLY credited to Various Artists (by
        VA MBID or credit name) - never merely uncredited (see ``_VA_NAMES``)."""
        for credit in rg_detail.get("artist-credit") or []:
            artist = credit.get("artist") or {}
            if artist.get("id") == _VA_MBID:
                return True
        if artist_mbid == _VA_MBID:
            return True
        return artist_name.strip().lower() in _VA_NAMES


def _build_mb_tracks(release: dict) -> list[MBTrack]:
    tracks: list[MBTrack] = []
    absolute = 0
    for medium in release.get("media") or []:
        disc = int(medium.get("position") or 1)
        for track in medium.get("tracks") or []:
            absolute += 1
            recording = track.get("recording") or {}
            tracks.append(
                MBTrack(
                    title=track.get("title") or recording.get("title") or "",
                    position=int(track.get("position") or 0),
                    disc=disc,
                    absolute_position=absolute,
                    length_ms=track.get("length") or recording.get("length"),
                    recording_mbid=recording.get("id"),
                )
            )
    return tracks


def _parse_year(date_str: str | None) -> int | None:
    if not date_str:
        return None
    try:
        return int(date_str[:4])
    except ValueError:
        return None


def _sample(items: list[LocalTrack], k: int) -> list[LocalTrack]:
    """Up to ``k`` tracks spread evenly across the folder."""
    if len(items) <= k:
        return items
    step = len(items) / k
    return [items[int(i * step)] for i in range(k)]
