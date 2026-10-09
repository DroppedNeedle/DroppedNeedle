"""Playlist-wide home planning (#204), on a Creedence-shaped catalog.

CCR have seven studio albums but well over a hundred compilation releases.
A playlist of their songs should land on the studio albums, and songs only
available on compilations should collapse onto as few of them as possible.
"""

import random

import pytest

from models.release_home import (
    CompilationPolicy,
    HomeCandidate,
    HomeTier,
    ResolutionStatus,
)
from services.playlist_track_planner import (
    PlannerTrack,
    Substitute,
    plan_playlist,
)

STUDIO_ALBUMS = {
    "rg-ccr": ("Creedence Clearwater Revival", "1968-07-05"),
    "rg-bayou": ("Bayou Country", "1969-01-05"),
    "rg-green": ("Green River", "1969-08-03"),
    "rg-willy": ("Willy and the Poor Boys", "1969-11-02"),
    "rg-cosmo": ("Cosmo's Factory", "1970-07-16"),
    "rg-pendulum": ("Pendulum", "1970-12-07"),
    "rg-mardi": ("Mardi Gras", "1972-04-11"),
}


def _studio(rg: str) -> HomeCandidate:
    title, date = STUDIO_ALBUMS[rg]
    return HomeCandidate(rg, title, "Album", (), True, False, True, date)


def _artist_comp(rg: str, date: str = "1976-01-01") -> HomeCandidate:
    return HomeCandidate(rg, rg, "Album", ("Compilation",), True, False, True, date)


def _va_comp(rg: str, date: str = "1990-01-01") -> HomeCandidate:
    return HomeCandidate(rg, rg, "Album", ("Compilation",), True, True, True, date)


def _single(rg: str) -> HomeCandidate:
    return HomeCandidate(rg, rg, "Single", (), True, False, True, "1969-01-01")


def _live(rg: str) -> HomeCandidate:
    return HomeCandidate(rg, rg, "Album", ("Live",), True, False, True, "1973-01-01")


def _ccr_playlist() -> list[PlannerTrack]:
    """20 studio recordings, each on its studio album plus ~12 compilations:
    the CCR greatest-hits comp, a rotating set of artist comps, and VA comps."""
    tracks = []
    albums = list(STUDIO_ALBUMS)
    for i in range(20):
        album = albums[i % len(albums)]
        comps = [_artist_comp("rg-chronicle")]
        comps += [_artist_comp(f"rg-best-of-{(i + k) % 30:02d}") for k in range(5)]
        comps += [_va_comp(f"rg-va-{i:02d}-{k}") for k in range(6)]
        candidates = [*comps, _studio(album)]
        random.Random(i).shuffle(candidates)
        tracks.append(PlannerTrack(f"t{i:02d}", f"rec-{i:02d}", tuple(candidates)))
    return tracks


def _remaster_only(i: int, *, substitute: str | None = None) -> PlannerTrack:
    """A remaster recording found only on compilations; ``rg-chronicle-2``
    carries every one of them, each VA comp only one."""
    candidates = (
        _artist_comp("rg-chronicle-2", "1986-01-01"),
        _va_comp(f"rg-va-remaster-{i}"),
    )
    sub = None
    if substitute is not None:
        sub = Substitute(f"rec-studio-{i}", _studio(substitute))
    return PlannerTrack(f"r{i}", f"rec-remaster-{i}", candidates, sub)


def _homes(plans) -> set[str]:
    return {p.release_group_mbid for p in plans.values() if p.release_group_mbid}


def test_studio_recordings_land_on_their_studio_albums():
    tracks = _ccr_playlist()
    plans = plan_playlist(tracks)

    assert _homes(plans) == set(STUDIO_ALBUMS)
    for i, track in enumerate(tracks):
        plan = plans[track.track_id]
        assert plan.status is ResolutionStatus.RESOLVED
        assert plan.tier is HomeTier.ALBUM
        assert plan.recording_mbid == track.recording_mbid
        assert plan.release_group_mbid == list(STUDIO_ALBUMS)[i % 7]


def test_compilation_only_tracks_consolidate_onto_one_compilation():
    """The worst case from the issue: one song per compilation. Every
    remaster shares one artist compilation, so it is the only extra album."""
    tracks = [*_ccr_playlist(), *(_remaster_only(i) for i in range(5))]
    plans = plan_playlist(tracks, CompilationPolicy.MATCH_PLAYLIST)

    assert _homes(plans) == {*STUDIO_ALBUMS, "rg-chronicle-2"}
    for i in range(5):
        plan = plans[f"r{i}"]
        assert plan.status is ResolutionStatus.RESOLVED
        assert plan.recording_mbid == f"rec-remaster-{i}"
        assert plan.tier is HomeTier.ARTIST_COMPILATION


def test_greedy_cover_takes_the_widest_compilation_first():
    a = [_va_comp("rg-wide"), _va_comp("rg-a")]
    b = [_va_comp("rg-wide"), _va_comp("rg-b")]
    c = [_va_comp("rg-narrow-1"), _va_comp("rg-c")]
    d = [_va_comp("rg-narrow-1"), _va_comp("rg-d")]
    e = [_va_comp("rg-wide"), _va_comp("rg-e")]
    tracks = [
        PlannerTrack(tid, f"rec-{tid}", tuple(cands))
        for tid, cands in zip("abcde", (a, b, c, d, e))
    ]
    plans = plan_playlist(tracks)
    assert {tid: plans[tid].release_group_mbid for tid in "abcde"} == {
        "a": "rg-wide",
        "b": "rg-wide",
        "e": "rg-wide",
        "c": "rg-narrow-1",
        "d": "rg-narrow-1",
    }


def test_artist_compilation_beats_wider_various_artists_compilation():
    """Tier first, coverage second."""
    tracks = [
        PlannerTrack(
            f"t{i}",
            f"rec-{i}",
            (_va_comp("rg-va-everything"), _artist_comp(f"rg-artist-{i}")),
        )
        for i in range(3)
    ]
    plans = plan_playlist(tracks)
    assert {plans[f"t{i}"].release_group_mbid for i in range(3)} == {
        "rg-artist-0",
        "rg-artist-1",
        "rg-artist-2",
    }


def test_studio_single_beats_compilation_album():
    track = PlannerTrack(
        "t", "rec", (_artist_comp("rg-chronicle"), _single("rg-single"))
    )
    plan = plan_playlist([track])["t"]
    assert (plan.release_group_mbid, plan.tier) == ("rg-single", HomeTier.SINGLE)


def test_live_recording_stays_live():
    """A live recording's own album is its home; the comps carry the same take."""
    track = PlannerTrack(
        "live",
        "rec-live",
        (_live("rg-live-in-europe"), _artist_comp("rg-chronicle")),
    )
    plan = plan_playlist([track])["live"]
    assert plan.release_group_mbid == "rg-live-in-europe"
    assert plan.recording_mbid == "rec-live"


def test_studio_policy_swaps_to_studio_recording_when_found():
    tracks = [
        *_ccr_playlist(),
        _remaster_only(0, substitute="rg-cosmo"),
        _remaster_only(1, substitute="rg-green"),
        _remaster_only(2),  # no studio match: falls back to consolidation
    ]
    plans = plan_playlist(tracks, CompilationPolicy.STUDIO)

    assert (plans["r0"].recording_mbid, plans["r0"].release_group_mbid) == (
        "rec-studio-0",
        "rg-cosmo",
    )
    assert plans["r0"].tier is HomeTier.ALBUM
    assert plans["r1"].release_group_mbid == "rg-green"
    assert (plans["r2"].recording_mbid, plans["r2"].release_group_mbid) == (
        "rec-remaster-2",
        "rg-chronicle-2",
    )
    assert _homes(plans) == {*STUDIO_ALBUMS, "rg-chronicle-2"}


def test_studio_substitutes_do_not_steer_compilation_choice():
    """Swapped tracks must not make a compilation look wider than it is."""
    swapped = [
        PlannerTrack(
            f"s{i}",
            f"rec-s{i}",
            (_va_comp("rg-bait"),),
            Substitute(f"rec-studio-s{i}", _studio("rg-willy")),
        )
        for i in range(3)
    ]
    stays = PlannerTrack("stay", "rec-stay", (_va_comp("rg-bait"), _va_comp("rg-a")))
    plans = plan_playlist([*swapped, stays], CompilationPolicy.STUDIO)
    # Coverage of rg-bait without the swapped tracks is 1, tied with rg-a;
    # the tie goes to the earlier MBID, not to the bait.
    assert plans["stay"].release_group_mbid == "rg-a"


def test_studio_policy_ignores_substitute_for_non_compilation_tracks():
    track = PlannerTrack(
        "t",
        "rec",
        (_studio("rg-bayou"),),
        Substitute("rec-other", _studio("rg-willy")),
    )
    plan = plan_playlist([track], CompilationPolicy.STUDIO)["t"]
    assert (plan.recording_mbid, plan.release_group_mbid) == ("rec", "rg-bayou")


def test_flag_policy_holds_compilation_only_tracks_with_options():
    tracks = [
        *_ccr_playlist(),
        _remaster_only(0, substitute="rg-cosmo"),
        _remaster_only(1),
    ]
    plans = plan_playlist(tracks, CompilationPolicy.FLAG)

    flagged = plans["r0"]
    assert flagged.status is ResolutionStatus.FLAGGED
    assert flagged.release_group_mbid is None
    assert [option.to_json() for option in flagged.options] == [
        {
            "kind": "studio",
            "recording_mbid": "rec-studio-0",
            "release_group_mbid": "rg-cosmo",
            "title": "Cosmo's Factory",
            "tier": "album",
        },
        {
            "kind": "compilation",
            "recording_mbid": "rec-remaster-0",
            "release_group_mbid": "rg-chronicle-2",
            "title": "rg-chronicle-2",
            "tier": "artist_compilation",
        },
    ]
    assert [option.kind for option in plans["r1"].options] == ["compilation"]
    # Studio-album tracks are unaffected by the policy.
    assert all(
        plans[f"t{i:02d}"].status is ResolutionStatus.RESOLVED for i in range(20)
    )


@pytest.mark.parametrize(
    "track",
    [
        PlannerTrack("t", None, (_studio("rg-bayou"),)),
        PlannerTrack("t", "rec-orphan", ()),
    ],
)
def test_unresolvable_tracks(track):
    plan = plan_playlist([track])["t"]
    assert plan.status is ResolutionStatus.UNRESOLVED
    assert plan.release_group_mbid is None
    assert plan.recording_mbid == track.recording_mbid


def test_plan_is_deterministic_under_input_order():
    tracks = [*_ccr_playlist(), *(_remaster_only(i) for i in range(5))]
    expected = plan_playlist(tracks)
    for seed in range(5):
        shuffled = list(tracks)
        random.Random(seed).shuffle(shuffled)
        assert plan_playlist(shuffled) == expected


def test_empty_playlist():
    assert plan_playlist([]) == {}


def _reference_cover(pending):
    """The obvious recount-every-round greedy the heap version must match."""
    from models.release_home import home_sort_key

    placed, remaining = {}, dict(pending)
    while remaining:
        coverage, rep = {}, {}
        for tid, cands in remaining.items():
            for cand in cands:
                key = cand.release_group_mbid.casefold()
                coverage.setdefault(key, set()).add(tid)
                if key not in rep or home_sort_key(cand) < home_sort_key(rep[key]):
                    rep[key] = cand
        chosen = min(coverage, key=lambda k: (-len(coverage[k]), home_sort_key(rep[k])))
        for tid in coverage[chosen]:
            placed[tid] = rep[chosen]
            del remaining[tid]
    return placed


@pytest.mark.parametrize("seed", range(25))
def test_heap_cover_matches_reference_greedy(seed):
    from services.playlist_track_planner import _cover_tier

    rng = random.Random(seed)
    # A release group has one first-release date, whichever track it is on.
    groups = {
        f"rg-{g:03d}": f"19{rng.randint(60, 99)}-01-01"
        for g in range(rng.randint(2, 40))
    }
    names = sorted(groups)
    pending = {
        f"t{i}": [
            _va_comp(rg, groups[rg])
            for rg in rng.sample(names, rng.randint(1, min(8, len(names))))
        ]
        for i in range(rng.randint(1, 60))
    }
    assert _cover_tier(pending) == _reference_cover(pending)
