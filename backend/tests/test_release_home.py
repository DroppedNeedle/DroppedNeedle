"""Release-group home tiering for playlist track requests (#204)."""

import pytest

from models.release_home import (
    VARIOUS_ARTISTS_MBID,
    HomeCandidate,
    HomeTier,
    best_home,
    classify_home,
    home_sort_key,
    is_various_artist_credit,
    recording_version_flags,
    version_flags,
)


def _candidate(
    mbid: str = "rg-1",
    *,
    primary: str | None = "Album",
    secondary: tuple[str, ...] = (),
    official: bool = True,
    various: bool = False,
    credits_artist: bool = True,
    date: str | None = "1969-01-01",
) -> HomeCandidate:
    return HomeCandidate(
        release_group_mbid=mbid,
        title=mbid,
        primary_type=primary,
        secondary_types=secondary,
        official=official,
        is_various=various,
        credits_track_artist=credits_artist,
        first_release_date=date,
    )


@pytest.mark.parametrize(
    ("candidate", "tier"),
    [
        (_candidate(), HomeTier.ALBUM),
        (_candidate(primary="EP"), HomeTier.EP),
        (_candidate(primary="Single"), HomeTier.SINGLE),
        (_candidate(secondary=("Soundtrack",)), HomeTier.SOUNDTRACK),
        (_candidate(secondary=("Compilation",)), HomeTier.ARTIST_COMPILATION),
        (
            _candidate(secondary=("Compilation",), credits_artist=False),
            HomeTier.OTHER_COMPILATION,
        ),
        (
            _candidate(secondary=("Compilation",), various=True),
            HomeTier.OTHER_COMPILATION,
        ),
        (_candidate(various=True), HomeTier.OTHER_COMPILATION),
        (_candidate(official=False), HomeTier.OTHER),
        (_candidate(credits_artist=False), HomeTier.OTHER),
        (_candidate(secondary=("Spokenword",)), HomeTier.OTHER),
        (_candidate(primary="Broadcast"), HomeTier.OTHER),
        (_candidate(primary=None), HomeTier.OTHER),
        # Version flags are not a tier: a live album is the live recording's album.
        (_candidate(secondary=("Live",)), HomeTier.ALBUM),
        (_candidate(secondary=("Live", "Compilation")), HomeTier.ARTIST_COMPILATION),
    ],
)
def test_classify_home(candidate: HomeCandidate, tier: HomeTier) -> None:
    assert classify_home(candidate) is tier


def test_studio_single_beats_artist_compilation() -> None:
    """The legacy rank prefers a Compilation+Album over a studio Single."""
    single = _candidate("single", primary="Single")
    comp = _candidate("chronicle", secondary=("Compilation",), date="1976-01-01")
    assert best_home([comp, single]) == single


def test_best_home_prefers_album_then_earliest_then_mbid() -> None:
    later = _candidate("b-later", date="1970-07-01")
    earlier = _candidate("c-earlier", date="1969-08-01")
    undated = _candidate("a-undated", date=None)
    va = _candidate("va", various=True, date="1960-01-01")
    assert best_home([va, undated, later, earlier]) == earlier
    assert home_sort_key(undated) < home_sort_key(va)
    tie_a = _candidate("A-tie")
    tie_b = _candidate("b-tie")
    assert best_home([tie_b, tie_a]) == tie_a


def test_best_home_empty() -> None:
    assert best_home([]) is None


def test_is_various_artist_credit() -> None:
    assert is_various_artist_credit([{"artist": {"id": VARIOUS_ARTISTS_MBID}}])
    assert is_various_artist_credit([{"artist": {"id": VARIOUS_ARTISTS_MBID.upper()}}])
    assert is_various_artist_credit(None, " Various Artists ")
    assert not is_various_artist_credit(
        [{"artist": {"id": "ccr"}}], "Creedence Clearwater Revival"
    )
    assert not is_various_artist_credit(None, None)


def test_version_flags() -> None:
    assert version_flags(["Live", "Compilation"]) == frozenset({"live"})
    assert version_flags([]) == frozenset()


def test_recording_version_flags() -> None:
    studio = [_candidate("album"), _candidate("comp", secondary=("Compilation",))]
    assert recording_version_flags(studio) == frozenset()

    live = [
        _candidate("live-album", secondary=("Live",)),
        _candidate("live-single", primary="Single", secondary=("Live",)),
        # Comps holding the live take are usually not tagged Live.
        _candidate("comp", secondary=("Compilation",)),
        _candidate("va", various=True),
    ]
    assert recording_version_flags(live) == frozenset({"live"})

    assert (
        recording_version_flags([_candidate("comp", secondary=("Compilation",))])
        == frozenset()
    )


def test_tier_labels_round_trip() -> None:
    for tier in HomeTier:
        assert HomeTier.from_label(tier.label) is tier
    assert HomeTier.from_label(None) is None
    assert HomeTier.from_label("bogus") is None
    assert HomeTier.ARTIST_COMPILATION.is_compilation
    assert not HomeTier.SOUNDTRACK.is_compilation
