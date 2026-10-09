"""MusicBrainzAlbumMixin.get_recording_homes: paginated release browse,
collapse to release groups, and durable read-through (#204)."""

import threading
from unittest.mock import AsyncMock

import pytest

import repositories.musicbrainz_album as mod
from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.persistence.mb_canonical_store import MbCanonicalStore
from models.release_home import VARIOUS_ARTISTS_MBID, HomeTier, best_home, classify_home
from repositories.musicbrainz_album import (
    MusicBrainzAlbumMixin,
    _recording_homes_from_releases,
    _RecordingReleaseBrowsePayload,
    recording_home_candidates,
)

REC = "aaaaaaaa-1111-4222-8333-444444444444"
_CCR = [
    {
        "name": "Creedence Clearwater Revival",
        "joinphrase": "",
        "artist": {"id": "ccr", "name": "Creedence Clearwater Revival"},
    }
]
_VA = [{"name": "Various Artists", "artist": {"id": VARIOUS_ARTISTS_MBID}}]


def _release(
    rg_id: str,
    *,
    status="Official",
    secondary=(),
    primary="Album",
    credit=_CCR,
    date="1969-11-02",
):
    return {
        "id": f"rel-{rg_id}-{status}",
        "status": status,
        "artist-credit": credit,
        "release-group": {
            "id": rg_id,
            "title": rg_id,
            "primary-type": primary,
            "secondary-types": list(secondary),
            "first-release-date": date,
            "artist-credit": credit,
        },
    }


class _Repo(MusicBrainzAlbumMixin):
    def __init__(self, store=None) -> None:
        self._cache = InMemoryCache()
        self._mb_canonical_store = store


def test_collapse_to_release_groups_any_official_wins():
    homes = _recording_homes_from_releases(
        [
            _release("RG-Willy", status="Bootleg"),
            _release("rg-willy", status="Official"),
            _release("rg-chronicle", secondary=("Compilation",), date="1976-01-01"),
            _release("rg-va", credit=_VA, secondary=("Compilation",)),
            {"id": "rel-orphan", "release-group": {}},
        ]
    )
    assert [home.release_group_mbid for home in homes] == [
        "rg-chronicle",
        "rg-va",
        "rg-willy",
    ]
    by_id = {home.release_group_mbid: home for home in homes}
    assert by_id["rg-willy"].official
    assert by_id["rg-va"].is_various
    assert by_id["rg-chronicle"].secondary_types == ("Compilation",)
    assert by_id["rg-willy"].artist_credit == "Creedence Clearwater Revival"


def test_candidates_bind_track_artist():
    homes = _recording_homes_from_releases(
        [
            _release("rg-willy"),
            _release(
                "rg-fogerty-comp",
                secondary=("Compilation",),
                credit=[{"name": "John Fogerty", "artist": {"id": "jf"}}],
            ),
        ]
    )
    bound = {
        c.release_group_mbid: c
        for c in recording_home_candidates(homes, "Creedence Clearwater Revival")
    }
    assert bound["rg-willy"].credits_track_artist
    assert not bound["rg-fogerty-comp"].credits_track_artist
    # No usable artist: everything credits the track, tiering stays type-based.
    assert all(
        c.credits_track_artist
        for c in recording_home_candidates(homes, "Various Artists")
    )


@pytest.mark.asyncio
async def test_browse_paginates_past_lookup_cap(monkeypatch):
    """250 releases need three pages; the studio album is on the last one."""
    comps = [
        _release(f"rg-comp-{i:03d}", secondary=("Compilation",), date="1980-01-01")
        for i in range(249)
    ]
    releases = [*comps, _release("rg-willy")]
    calls: list[dict] = []

    async def fake_get(path, params=None, **kwargs):
        calls.append(dict(params))
        offset, limit = params["offset"], params["limit"]
        return _RecordingReleaseBrowsePayload(
            releases=releases[offset : offset + limit], release_count=len(releases)
        )

    monkeypatch.setattr(mod, "mb_api_get", fake_get)
    homes = await _Repo().get_recording_homes(REC)

    assert [call["offset"] for call in calls] == [0, 100, 200]
    assert all(call["recording"] == REC for call in calls)
    assert len(homes) == 250
    best = best_home(recording_home_candidates(homes, "Creedence Clearwater Revival"))
    assert best.release_group_mbid == "rg-willy"


@pytest.mark.asyncio
async def test_browse_is_capped(monkeypatch):
    api = AsyncMock(
        return_value=_RecordingReleaseBrowsePayload(
            releases=[_release("rg-same")] * 100, release_count=5000
        )
    )
    monkeypatch.setattr(mod, "mb_api_get", api)
    homes = await _Repo().get_recording_homes(REC)
    assert api.await_count == mod._RECORDING_HOMES_MAX_PAGES
    assert [home.release_group_mbid for home in homes] == ["rg-same"]


@pytest.mark.asyncio
async def test_store_read_through_and_failure_not_banked(monkeypatch, tmp_path):
    store = MbCanonicalStore(
        db_path=tmp_path / "library.db", write_lock=threading.Lock()
    )
    repo = _Repo(store)

    failing = AsyncMock(side_effect=RuntimeError("boom"))
    monkeypatch.setattr(mod, "mb_api_get", failing)
    assert await repo.get_recording_homes(REC) is None

    api = AsyncMock(
        return_value=_RecordingReleaseBrowsePayload(
            releases=[_release("rg-willy")], release_count=1
        )
    )
    monkeypatch.setattr(mod, "mb_api_get", api)
    first = await repo.get_recording_homes(REC)
    second = await _Repo(store).get_recording_homes(REC.upper())

    assert api.await_count == 1
    assert first == second
    assert (
        best_home(
            recording_home_candidates(second, "Creedence Clearwater Revival")
        ).release_group_mbid
        == "rg-willy"
    )


@pytest.mark.asyncio
async def test_empty_browse_is_banked_as_authoritative(monkeypatch, tmp_path):
    store = MbCanonicalStore(
        db_path=tmp_path / "library.db", write_lock=threading.Lock()
    )
    api = AsyncMock(return_value=_RecordingReleaseBrowsePayload())
    monkeypatch.setattr(mod, "mb_api_get", api)

    assert await _Repo(store).get_recording_homes(REC) == []
    assert await _Repo(store).get_recording_homes(REC) == []
    assert api.await_count == 1


@pytest.mark.asyncio
async def test_invalid_mbid_short_circuits(monkeypatch):
    api = AsyncMock()
    monkeypatch.setattr(mod, "mb_api_get", api)
    assert await _Repo().get_recording_homes("not-an-mbid") is None
    api.assert_not_awaited()


def test_tiering_of_browsed_homes():
    homes = _recording_homes_from_releases(
        [
            _release("rg-single", primary="Single"),
            _release("rg-chronicle", secondary=("Compilation",)),
            _release("rg-va", credit=_VA),
        ]
    )
    tiers = {
        c.release_group_mbid: c
        for c in recording_home_candidates(homes, "Creedence Clearwater Revival")
    }
    assert classify_home(tiers["rg-single"]) is HomeTier.SINGLE
    assert classify_home(tiers["rg-chronicle"]) is HomeTier.ARTIST_COMPILATION
    assert classify_home(tiers["rg-va"]) is HomeTier.OTHER_COMPILATION
