"""Tests for AlbumIdentifier - per-folder track-list matching."""

import itertools
import random
from unittest.mock import AsyncMock

import pytest

from infrastructure.queue.priority_queue import RequestPriority
from models.search import SearchResult
from repositories.musicbrainz_album import RecordingMatch, RecordingReleaseGroup
from services.native.album_matcher import (
    AlbumIdentifier,
    LocalTrack,
    MBTrack,
    _ReleaseMeta,
    _clean,
    _hungarian,
    _string_penalty,
    assign_tracks,
    score_release,
    track_distance,
)

_SANTANA = [
    "Waiting", "Evil Ways", "Shades of Time", "Savor", "Jingo",
    "Persuasion", "Treat", "You Just Don't Care", "Soul Sacrifice",
]


def _locals(titles, *, album="Santana", artist="Santana", duration=240.0):
    return [
        LocalTrack(
            path=f"/m/{i:02d}.flac", title=t, artist=artist, album=album,
            track_number=i + 1, duration_seconds=duration,
        )
        for i, t in enumerate(titles)
    ]


def _mb_tracks(titles, *, disc=1, start_abs=0, length_ms=240000, rec_prefix="rec"):
    out = []
    absolute = start_abs
    for pos, title in enumerate(titles, 1):
        absolute += 1
        out.append(
            MBTrack(
                title=title, position=pos, disc=disc, absolute_position=absolute,
                length_ms=length_ms, recording_mbid=f"{rec_prefix}-{absolute}",
            )
        )
    return out


def test_clean_folds_case_accents_and_drops_punctuation():
    assert _clean("Björk!") == "bjork"
    assert _clean("A.S.A.P  -  Rocky") == "asaprocky"


def test_string_penalty_bounds():
    assert _string_penalty("waiting", "waiting") == 0.0
    assert _string_penalty("", "") == 0.0
    assert _string_penalty("anything", "") == 1.0
    assert _string_penalty("abc", "xyz") == 1.0


def test_track_distance_title_and_duration_grace():
    a = MBTrack(title="Waiting", position=1, disc=1, absolute_position=1, length_ms=240000)
    near = LocalTrack(path="x", title="Waiting", artist="", album="", track_number=1,
                      duration_seconds=247.0)
    assert track_distance(near, a).normalized() == 0.0
    far = LocalTrack(path="x", title="Waiting", artist="", album="", track_number=1,
                     duration_seconds=285.0)
    assert track_distance(far, a).normalized() > 0.0


def test_track_index_accepts_absolute_or_per_disc_number():
    mb = MBTrack(title="X", position=1, disc=2, absolute_position=10, length_ms=None)
    per_disc = LocalTrack(path="x", title="X", artist="", album="", track_number=1)
    absolute = LocalTrack(path="x", title="X", artist="", album="", track_number=10)
    wrong = LocalTrack(path="x", title="X", artist="", album="", track_number=4)
    assert track_distance(per_disc, mb).normalized() == 0.0
    assert track_distance(absolute, mb).normalized() == 0.0
    assert track_distance(wrong, mb).normalized() > 0.0


def test_hungarian_matches_brute_force_optimum():
    rng = random.Random(7)
    for _ in range(200):
        n = rng.randint(1, 6)
        cost = [[rng.random() for _ in range(n)] for _ in range(n)]
        got = sum(cost[i][j] for i, j in enumerate(_hungarian(cost)))
        best = min(
            sum(cost[i][p[i]] for i in range(n))
            for p in itertools.permutations(range(n))
        )
        assert abs(got - best) < 1e-9


def test_assign_tracks_avoids_collisions_and_reports_extras():
    locals_ = [
        LocalTrack(path="a", title="Scream", artist="", album="", track_number=1),
        LocalTrack(path="b", title="Screamm", artist="", album="", track_number=2),
    ]
    mb = [
        MBTrack(title="Scream", position=1, disc=1, absolute_position=1, recording_mbid="r1"),
        MBTrack(title="Screamm", position=2, disc=1, absolute_position=2, recording_mbid="r2"),
        MBTrack(title="Screammm", position=3, disc=1, absolute_position=3, recording_mbid="r3"),
    ]
    mapping, local_extra, mb_extra, _ = assign_tracks(locals_, mb)
    assert mapping == {0: 0, 1: 1}
    assert local_extra == []
    assert mb_extra == [2]


def test_score_release_ranks_standalone_album_above_compilation():
    locals_ = _locals(_SANTANA, album="The Woodstock Experience")
    debut = _mb_tracks(_SANTANA)
    comp = _mb_tracks(_SANTANA, disc=1) + _mb_tracks(
        [f"Live {i}" for i in range(8)], disc=2, start_abs=9, rec_prefix="live"
    )
    m_debut = score_release(
        locals_, debut,
        _ReleaseMeta(release_group_mbid="rg-debut", release_mbid="rel-debut",
                     album_title="Santana", artist="Santana", is_various=False, year=1969,
                     primary_type="album"),
    )
    m_comp = score_release(
        locals_, comp,
        _ReleaseMeta(release_group_mbid="rg-comp", release_mbid="rel-comp",
                     album_title="The Woodstock Experience", artist="Santana",
                     is_various=False, year=2009, primary_type="album",
                     secondary_types=frozenset({"compilation"})),
    )
    assert m_debut.distance < m_comp.distance
    assert m_debut.accepted is True
    assert len(m_debut.assignments) == 9


def test_score_release_rejects_wrong_artist_despite_matching_titles():
    locals_ = _locals(_SANTANA, album="Santana", artist="Santana")
    mb = _mb_tracks(_SANTANA)
    match = score_release(
        locals_, mb,
        _ReleaseMeta(release_group_mbid="rg", release_mbid="rel", album_title="Santana",
                     artist="The Karaoke Crew", is_various=False, year=2010),
    )
    assert match.accepted is False


def test_score_release_accepts_various_artists_release():
    locals_ = _locals(_SANTANA, album="Woodstock", artist="Santana")
    mb = _mb_tracks(_SANTANA)
    match = score_release(
        locals_, mb,
        _ReleaseMeta(release_group_mbid="rg", release_mbid="rel", album_title="Woodstock",
                     artist="Various Artists", is_various=True, year=1994),
    )
    assert match.accepted is True


def test_score_release_rejects_when_few_files_map():
    locals_ = _locals(["Waiting"] + [f"Unrelated {i}" for i in range(4)], album="Mystery")
    mb = _mb_tracks(_SANTANA)
    match = score_release(
        locals_, mb,
        _ReleaseMeta(release_group_mbid="rg", release_mbid="rel", album_title="Santana",
                     artist="Santana", is_various=False, year=1969),
    )
    assert match.accepted is False


def _rg_detail(title, artist, releases, *, primary_type="Album", secondary_types=()):
    return {
        "title": title,
        "primary-type": primary_type,
        "secondary-types": list(secondary_types),
        "artist-credit": [{"name": artist, "artist": {"id": "a1", "name": artist}}],
        "releases": releases,
    }


def _release(rel_id, disc_counts, *, status="Official", date="1969"):
    return {
        "id": rel_id, "status": status, "date": date,
        "media": [{"track-count": c} for c in disc_counts],
    }


def _release_tracks(titles_by_disc):
    media = []
    absolute = 0
    for disc_no, titles in enumerate(titles_by_disc, 1):
        tracks = []
        for pos, title in enumerate(titles, 1):
            absolute += 1
            tracks.append({
                "title": title, "position": pos, "length": 240000,
                "recording": {"id": f"rec-{absolute}", "title": title},
            })
        media.append({"position": disc_no, "tracks": tracks})
    return {"date": "1969", "media": media}


def _santana_repo():
    """Repo where the title search finds only the comp; recording search surfaces the debut."""
    repo = AsyncMock()
    repo.search_albums = AsyncMock(return_value=[
        SearchResult(type="album", title="The Woodstock Experience",
                     musicbrainz_id="rg-comp", artist="Santana"),
    ])
    repo.search_recordings = AsyncMock(return_value=[
        RecordingMatch(
            recording_mbid="rec-x", title="Waiting", artist="Santana", score=100,
            release_groups=[
                RecordingReleaseGroup("rg-debut", "Santana", "rel-debut", "Album", ()),
                RecordingReleaseGroup("rg-comp", "The Woodstock Experience", "rel-comp",
                                      "Album", ("Compilation",)),
            ],
        ),
    ])

    async def rg_detail(mbid, includes=None, priority=None):
        if mbid == "rg-debut":
            return _rg_detail("Santana", "Santana", [_release("rel-debut", [9])])
        return _rg_detail("The Woodstock Experience", "Santana",
                          [_release("rel-comp", [9, 8], date="2009")],
                          secondary_types=("Compilation",))

    async def release(rel_id, includes=None, priority=None):
        if rel_id == "rel-debut":
            return _release_tracks([_SANTANA])
        return _release_tracks([_SANTANA, [f"Live {i}" for i in range(8)]])

    repo.get_release_group_by_id = AsyncMock(side_effect=rg_detail)
    repo.get_release_by_id = AsyncMock(side_effect=release)
    return repo


@pytest.mark.asyncio
async def test_identify_picks_standalone_release_over_compilation():
    identifier = AlbumIdentifier(_santana_repo())
    locals_ = _locals(_SANTANA, album="The Woodstock Experience")
    match = await identifier.identify(locals_)
    assert match is not None
    assert match.accepted is True
    assert match.release_group_mbid == "rg-debut"
    assert match.release_mbid == "rel-debut"
    assert len(match.assignments) == 9
    assert match.artist_mbid == "a1"
    assert match.artist_name == "Santana"


def test_score_release_omits_artist_for_various_artists():
    locals_ = _locals(_SANTANA, album="Woodstock", artist="Santana")
    mb = _mb_tracks(_SANTANA)
    match = score_release(
        locals_, mb,
        _ReleaseMeta(release_group_mbid="rg", release_mbid="rel", album_title="Woodstock",
                     artist="Various Artists", is_various=True, artist_mbid="va-id"),
    )
    assert match.artist_mbid is None
    assert match.artist_name is None


@pytest.mark.asyncio
async def test_resolve_release_group_artist_returns_primary_credit():
    repo = AsyncMock()
    repo.get_release_group_by_id = AsyncMock(return_value=_rg_detail(
        "?", "XXXTENTACION", [_release("rel", [18])],
    ))
    mbid, name = await AlbumIdentifier(repo).resolve_release_group_artist("rg-1")
    assert name == "XXXTENTACION"
    assert mbid == "a1"
    repo.get_release_group_by_id.assert_awaited_once_with(
        "rg-1", includes=["artist-credits"], priority=RequestPriority.BACKGROUND_SYNC
    )


@pytest.mark.asyncio
async def test_resolve_release_group_artist_handles_missing_data():
    repo = AsyncMock()
    repo.get_release_group_by_id = AsyncMock(return_value=None)
    assert await AlbumIdentifier(repo).resolve_release_group_artist("rg-1") == (None, None)
    assert await AlbumIdentifier(repo).resolve_release_group_artist("") == (None, None)


@pytest.mark.asyncio
async def test_best_release_fetches_the_artist_credit():
    """The RG detail fetch must include artist-credits: without it MusicBrainz
    omits the credit entirely, the extracted artist was "" for EVERY album, and
    "" used to classify as Various Artists (the 197-album /Various Artists/
    misfiling incident)."""
    repo = AsyncMock()
    repo.get_release_group_by_id = AsyncMock(
        return_value=_rg_detail("Santana", "Santana", [_release("rel-debut", [9])])
    )
    repo.get_release_by_id = AsyncMock(return_value=_release_tracks([_SANTANA]))

    picked = await AlbumIdentifier(repo).release_tracks("rg-debut", 9)

    assert picked is not None
    meta, _tracks = picked
    assert meta.artist == "Santana"
    assert meta.is_various is False
    kwargs = repo.get_release_group_by_id.await_args_list[0].kwargs
    assert "artist-credits" in kwargs["includes"]


@pytest.mark.asyncio
async def test_creditless_release_group_is_not_various_and_resolves_credit():
    """A detail response WITHOUT an artist-credit (pre-fix cache entries, MB
    hiccups) is an UNKNOWN identity: never Various Artists - and the credit is
    knowable, so it is resolved with a dedicated artist-credits lookup."""
    creditless = _rg_detail("Definitely Maybe", "ignored", [_release("rel-dm", [11])])
    del creditless["artist-credit"]

    async def rg_detail(mbid, includes=None, priority=None):
        if includes == ["artist-credits"]:  # the fallback resolve
            return _rg_detail("Definitely Maybe", "Oasis", [])
        return creditless

    repo = AsyncMock()
    repo.get_release_group_by_id = AsyncMock(side_effect=rg_detail)
    repo.get_release_by_id = AsyncMock(
        return_value=_release_tracks([[f"T{i}" for i in range(11)]])
    )

    picked = await AlbumIdentifier(repo).release_tracks("rg-dm", 11)

    assert picked is not None
    meta, _tracks = picked
    assert meta.is_various is False  # empty credit is NOT a VA claim
    assert meta.artist == "Oasis"  # resolved from the RG's own credit
    assert meta.artist_mbid == "a1"


@pytest.mark.asyncio
async def test_release_group_credited_to_va_is_various():
    """'Various Artists' stays reserved for release groups actually credited so."""
    detail = _rg_detail("Now 94", "Various Artists", [_release("rel-now", [20])])
    detail["artist-credit"][0]["artist"]["id"] = "89ad4ac3-39f7-470e-963a-56509c546377"
    repo = AsyncMock()
    repo.get_release_group_by_id = AsyncMock(return_value=detail)
    repo.get_release_by_id = AsyncMock(
        return_value=_release_tracks([[f"T{i}" for i in range(20)]])
    )

    picked = await AlbumIdentifier(repo).release_tracks("rg-now", 20)

    assert picked is not None
    meta, _tracks = picked
    assert meta.is_various is True


@pytest.mark.asyncio
async def test_identify_returns_none_for_single_file():
    identifier = AlbumIdentifier(_santana_repo())
    assert await identifier.identify(_locals(["Waiting"])) is None


@pytest.mark.asyncio
async def test_identify_returns_none_when_no_candidate_accepts():
    repo = AsyncMock()
    repo.search_albums = AsyncMock(return_value=[])
    repo.search_recordings = AsyncMock(return_value=[])
    identifier = AlbumIdentifier(repo)
    assert await identifier.identify(_locals(_SANTANA)) is None


@pytest.mark.asyncio
async def test_identify_falls_back_when_release_track_counts_missing():
    repo = AsyncMock()
    repo.search_albums = AsyncMock(return_value=[
        SearchResult(type="album", title="Santana", musicbrainz_id="rg-debut", artist="Santana"),
    ])
    repo.search_recordings = AsyncMock(return_value=[])
    repo.get_release_group_by_id = AsyncMock(return_value=_rg_detail(
        "Santana", "Santana", [{"id": "rel-debut", "status": "Official", "media": [{}]}],
    ))
    repo.get_release_by_id = AsyncMock(return_value=_release_tracks([_SANTANA]))
    identifier = AlbumIdentifier(repo)
    match = await identifier.identify(_locals(_SANTANA, album="Santana"))
    assert match is not None and match.release_group_mbid == "rg-debut"
    assert len(match.assignments) == 9


# -- fingerprint-seeded identification (prevents one folder scattering across RGs) --


def _comp_release(n_tracks):
    """A compilation release whose recordings are DISTINCT from the debut's."""
    return {
        "date": "2009",
        "media": [{"position": 1, "tracks": [
            {"title": f"Hit {i}", "position": i + 1, "length": 200000,
             "recording": {"id": f"comp-rec-{i}", "title": f"Hit {i}"}}
            for i in range(n_tracks)
        ]}],
    }


def _seed_repo():
    """Text/recording search surface ONLY a compilation; the real debut is reachable
    solely via a fingerprint seed - the Led Zeppelin scatter scenario, distilled."""
    repo = AsyncMock()
    repo.search_albums = AsyncMock(return_value=[
        SearchResult(type="album", title="Greatest Hits",
                     musicbrainz_id="rg-comp", artist="Santana"),
    ])
    repo.search_recordings = AsyncMock(return_value=[])

    async def rg_detail(mbid, includes=None, priority=None):
        if mbid == "rg-debut":
            return _rg_detail("Santana", "Santana", [_release("rel-debut", [9])])
        return _rg_detail("Greatest Hits", "Santana", [_release("rel-comp", [30], date="2009")],
                          secondary_types=("Compilation",))

    async def release(rel_id, includes=None, priority=None):
        return _release_tracks([_SANTANA]) if rel_id == "rel-debut" else _comp_release(30)

    repo.get_release_group_by_id = AsyncMock(side_effect=rg_detail)
    repo.get_release_by_id = AsyncMock(side_effect=release)
    return repo


def _fingerprinted_locals():
    """Files with junk tags (wrong album, compilation track numbers) but audio-derived
    recording MBIDs that match the debut's tracklist."""
    return [
        LocalTrack(
            path=f"/m/{i:02d}.flac", title=t, artist="Santana", album="Greatest Hits",
            track_number=i + 12, duration_seconds=240.0, recording_mbid=f"rec-{i + 1}",
        )
        for i, t in enumerate(_SANTANA)
    ]


@pytest.mark.asyncio
async def test_candidate_release_groups_puts_fingerprint_seeds_first():
    identifier = AlbumIdentifier(_seed_repo())
    cands = await identifier._candidate_release_groups(_locals(_SANTANA), ["rg-debut"])
    assert cands[0] == "rg-debut"  # audio seed ranked ahead of the text-search comp
    assert "rg-comp" in cands  # text candidate still considered
    # a seed the text search also returns is not duplicated
    only_comp = await identifier._candidate_release_groups(_locals(_SANTANA), ["rg-comp"])
    assert only_comp.count("rg-comp") == 1


@pytest.mark.asyncio
async def test_identify_uses_fingerprint_seed_when_tags_find_only_compilation():
    identifier = AlbumIdentifier(_seed_repo())
    locals_ = _fingerprinted_locals()
    # Tags alone surface only the compilation, whose recordings don't match -> no accept.
    assert await identifier.identify(locals_) is None
    # Seeding the fingerprint-derived debut RG lets the recording_id weight (10.0) win.
    seeded = await identifier.identify(locals_, seed_release_groups=["rg-debut"])
    assert seeded is not None and seeded.accepted
    assert seeded.release_group_mbid == "rg-debut"
    assert len(seeded.assignments) == 9


@pytest.mark.asyncio
async def test_seed_release_groups_none_preserves_tag_only_behaviour():
    # With no seed the matcher behaves exactly as before (the Santana recording-search path).
    identifier = AlbumIdentifier(_santana_repo())
    match = await identifier.identify(_locals(_SANTANA, album="The Woodstock Experience"))
    assert match is not None and match.release_group_mbid == "rg-debut"


# -- Phase 3: release-type preference (studio Album > compilation/live/EP) --


def _album_vs_ep_repo():
    """The same songs sit on a 9-track studio Album and a 5-track EP; a partial folder of
    5 must resolve to the Album, not the exact-count EP (guards R4 - count-fit trap)."""
    repo = AsyncMock()
    repo.search_albums = AsyncMock(return_value=[
        SearchResult(type="album", title="Santana", musicbrainz_id="rg-album", artist="Santana"),
        SearchResult(type="album", title="Santana EP", musicbrainz_id="rg-ep", artist="Santana"),
    ])
    repo.search_recordings = AsyncMock(return_value=[])

    async def rg_detail(mbid, includes=None, priority=None):
        if mbid == "rg-album":
            return _rg_detail("Santana", "Santana", [_release("rel-album", [9])],
                              primary_type="Album")
        return _rg_detail("Santana EP", "Santana", [_release("rel-ep", [5])], primary_type="EP")

    async def release(rel_id, includes=None, priority=None):
        return _release_tracks([_SANTANA]) if rel_id == "rel-album" else _release_tracks(
            [_SANTANA[:5]]
        )

    repo.get_release_group_by_id = AsyncMock(side_effect=rg_detail)
    repo.get_release_by_id = AsyncMock(side_effect=release)
    return repo


@pytest.mark.asyncio
async def test_identify_prefers_full_album_over_exact_count_ep():
    identifier = AlbumIdentifier(_album_vs_ep_repo())
    locals_ = _locals(_SANTANA[:5], album="Santana")  # only 5 of the album's 9 tracks
    match = await identifier.identify(locals_)
    assert match is not None
    # the studio album wins on release-type even though the EP is an exact track-count fit
    assert match.release_group_mbid == "rg-album"


@pytest.mark.asyncio
async def test_identify_still_matches_a_genuine_compilation():
    # release-type ranks but never gates: a folder that really IS a compilation still
    # matches its compilation (guards the R4-tension - don't lock comps out).
    identifier = AlbumIdentifier(_seed_repo())  # sole candidate is the 30-track comp
    locals_ = _locals(
        [f"Hit {i}" for i in range(30)], album="Greatest Hits", duration=200.0
    )
    match = await identifier.identify(locals_)
    assert match is not None and match.release_group_mbid == "rg-comp"


@pytest.mark.asyncio
async def test_identify_issues_all_mb_calls_at_background_priority():
    # A library refresh must not compete with live user searches on MusicBrainz's 1/s
    # limiter: every MB call the identifier makes during a scan is BACKGROUND_SYNC.
    repo = _santana_repo()
    await AlbumIdentifier(repo).identify(_locals(_SANTANA))
    awaited = [
        repo.search_albums,
        repo.search_recordings,
        repo.get_release_group_by_id,
        repo.get_release_by_id,
    ]
    assert all(m.await_count >= 1 for m in awaited)  # the path actually exercised each
    for mock in awaited:
        for call in mock.await_args_list:
            assert call.kwargs.get("priority") == RequestPriority.BACKGROUND_SYNC


@pytest.mark.asyncio
async def test_release_group_type_uses_background_priority():
    repo = AsyncMock()
    repo.get_release_group_by_id = AsyncMock(
        return_value={"primary-type": "Album", "secondary-types": []}
    )
    await AlbumIdentifier(repo).release_group_type("rg-1")
    assert (
        repo.get_release_group_by_id.await_args.kwargs["priority"]
        == RequestPriority.BACKGROUND_SYNC
    )
