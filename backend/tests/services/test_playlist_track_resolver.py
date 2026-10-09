"""PlaylistTrackResolver: song identity, homes, studio substitutes and
persistence over a real playlist DB with a fake MusicBrainz (#204)."""

import threading

import pytest

import services.playlist_track_resolver as mod
from models.album import Track
from models.release_home import (
    VARIOUS_ARTISTS_MBID,
    CompilationPolicy,
    RecordingHome,
    ResolutionStatus,
)
from repositories.async_playlist_repository import AsyncPlaylistRepository
from repositories.playlist_repository import PlaylistRepository, TrackResolution
from services.playlist_track_resolver import (
    PlaylistTrackResolver,
    same_song,
    song_title_key,
)

CCR = "Creedence Clearwater Revival"
CCR_ID = "ccr-artist"


def _album(rg, title, date, secondary=(), *, various=False, credit=CCR):
    return RecordingHome(
        release_group_mbid=rg,
        title=title,
        primary_type="Album",
        secondary_types=secondary,
        official=True,
        is_various=various,
        artist_credit="Various Artists" if various else credit,
        first_release_date=date,
        artist_mbids=(VARIOUS_ARTISTS_MBID,) if various else (CCR_ID,),
    )


WILLY = _album("rg-willy", "Willy and the Poor Boys", "1969-11-02")
BAYOU = _album("rg-bayou", "Bayou Country", "1969-01-05")
COSMO = _album("rg-cosmo", "Cosmo's Factory", "1970-07-16")
CHRONICLE = _album("rg-chronicle", "Chronicle", "1976-01-01", ("Compilation",))
VA_60S = _album(
    "rg-va-60s", "Sixties Gold", "1995-01-01", ("Compilation",), various=True
)
LIVE = _album("rg-live", "Live in Europe", "1973-01-01", ("Live",))


class FakeMb:
    def __init__(self, homes, artist_groups=()):
        self.homes = homes
        self.artist_groups = list(artist_groups)
        self.home_calls: list[str] = []
        self.failing: set[str] = set()

    async def get_recording_homes(self, recording_mbid, *, priority=None):
        self.home_calls.append(recording_mbid)
        if recording_mbid in self.failing:
            return None
        return self.homes.get(recording_mbid, [])

    async def get_artist_release_groups(
        self, artist_mbid, offset=0, limit=50, priority=None
    ):
        assert artist_mbid == CCR_ID
        return self.artist_groups[offset : offset + limit], len(self.artist_groups)


class FakeAlbums:
    def __init__(self, tracklists):
        self.tracklists = tracklists
        self.calls: list[str] = []

    async def get_album_tracks_info(self, release_group_id, priority=None):
        self.calls.append(release_group_id)
        return type("Info", (), {"tracks": self.tracklists.get(release_group_id, [])})


class FakeStore:
    def __init__(self, isrc_map=None):
        self.isrc_map = dict(isrc_map or {})
        self.saved: list[tuple[str, str]] = []

    async def get_recordings_by_isrc(self, isrc, *, source_context):
        return self.isrc_map.get(isrc, [])

    async def save_isrc_recordings(self, pairs, *, source_context=None):
        self.saved.extend(pairs)


def _rg(rg_id, title, date, secondary=()):
    return {
        "id": rg_id,
        "title": title,
        "primary-type": "Album",
        "secondary-types": list(secondary),
        "first-release-date": date,
    }


@pytest.fixture
def playlists(tmp_path):
    repo = PlaylistRepository(
        db_path=tmp_path / "library.db", write_lock=threading.Lock()
    )
    return AsyncPlaylistRepository(repo)


async def _playlist(playlists, *tracks):
    record = await playlists.create_playlist("CCR")
    defaults = {"artist_name": CCR, "album_name": "Chronicle", "source_type": ""}
    await playlists.add_tracks(record.id, [{**defaults, **t} for t in tracks])
    return record.id, await playlists.get_tracks(record.id)


def _resolver(playlists, mb, albums=None, store=None):
    return PlaylistTrackResolver(
        playlist_repo=playlists,
        mb_repo=mb,
        album_service=albums or FakeAlbums({}),
        canonical_store=store,
    )


@pytest.mark.asyncio
async def test_isrc_tracks_resolve_to_studio_albums_and_persist(playlists):
    mb = FakeMb(
        {
            "rec-fs": [CHRONICLE, VA_60S, WILLY],
            "rec-pm": [BAYOU, CHRONICLE],
            "rec-dmt": [WILLY, CHRONICLE, VA_60S],
        }
    )
    store = FakeStore({"USFI86900001": ["rec-fs"], "USFI86900002": ["rec-pm"], "USFI86900003": ["rec-dmt"]})
    pid, _ = await _playlist(
        playlists,
        {"track_name": "Fortunate Son", "isrc": "USFI86900001"},
        {"track_name": "Proud Mary", "isrc": "USFI86900002"},
        {"track_name": "Down on the Corner", "isrc": "USFI86900003"},
    )

    summary = await _resolver(playlists, mb, store=store).resolve_playlist(pid)

    tracks = {t.track_name: t for t in await playlists.get_tracks(pid)}
    assert {name: t.target_release_group_mbid for name, t in tracks.items()} == {
        "Fortunate Son": "rg-willy",
        "Proud Mary": "rg-bayou",
        "Down on the Corner": "rg-willy",
    }
    assert all(t.home_tier == "album" for t in tracks.values())
    assert all(t.resolution_status == "resolved" for t in tracks.values())
    assert tracks["Fortunate Son"].recording_mbid == "rec-fs"
    assert summary.to_json() == {
        "tracks": 3,
        "owned": 0,
        "chosen": 0,
        "resolved": 3,
        "flagged": 0,
        "unresolved": 0,
        "failed": 0,
        "albums": 2,
        "albums_by_tier": {"album": 2},
    }


@pytest.mark.asyncio
async def test_isrc_wire_lookup_is_banked(playlists, monkeypatch):
    calls = []

    async def fake_get(path, params=None, **kwargs):
        calls.append(path)
        return {"recordings": [{"id": "REC-FS"}]}

    monkeypatch.setattr(mod, "mb_api_get", fake_get)
    store = FakeStore()
    pid, _ = await _playlist(
        playlists, {"track_name": "Fortunate Son", "isrc": "USFI86900049"}
    )
    await _resolver(
        playlists, FakeMb({"rec-fs": [WILLY]}), store=store
    ).resolve_playlist(pid)

    assert calls == ["/isrc/USFI86900049"]
    assert store.saved == [("USFI86900049", "rec-fs")]
    (track,) = await playlists.get_tracks(pid)
    assert (track.recording_mbid, track.target_release_group_mbid) == (
        "rec-fs",
        "rg-willy",
    )


@pytest.mark.asyncio
async def test_isrc_with_several_recordings_picks_the_best_homed(playlists):
    mb = FakeMb({"rec-comp-cut": [VA_60S], "rec-original": [WILLY, CHRONICLE]})
    store = FakeStore({"USFI86900001": ["rec-comp-cut", "rec-original"]})
    pid, _ = await _playlist(
        playlists, {"track_name": "Fortunate Son", "isrc": "USFI86900001"}
    )

    await _resolver(playlists, mb, store=store).resolve_playlist(pid)

    (track,) = await playlists.get_tracks(pid)
    assert track.recording_mbid == "rec-original"
    assert track.target_release_group_mbid == "rg-willy"


@pytest.mark.asyncio
async def test_recording_found_from_imported_album_position(playlists):
    albums = FakeAlbums(
        {
            "rg-chronicle": [
                Track(position=1, title="Susie Q", recording_id="rec-sq"),
                Track(position=7, title="Proud Mary", recording_id="rec-pm"),
                Track(
                    position=8,
                    title="Proud Mary",
                    disc_number=2,
                    recording_id="rec-pm-live",
                ),
            ]
        }
    )
    mb = FakeMb({"rec-pm": [BAYOU, CHRONICLE]})
    pid, _ = await _playlist(
        playlists,
        {
            "track_name": "Proud Mary",
            "album_id": "rg-chronicle",
            "track_number": 7,
            "disc_number": 1,
        },
    )

    await _resolver(playlists, mb, albums).resolve_playlist(pid)

    (track,) = await playlists.get_tracks(pid)
    assert track.recording_mbid == "rec-pm"
    assert track.target_release_group_mbid == "rg-bayou"


@pytest.mark.asyncio
async def test_owned_chosen_and_unidentifiable_tracks(playlists):
    mb = FakeMb({"rec-fs": [WILLY]})
    pid, tracks = await _playlist(
        playlists,
        {"track_name": "Owned", "recording_mbid": "rec-fs", "library_file_id": "f1"},
        {"track_name": "Chosen", "recording_mbid": "rec-fs"},
        {"track_name": "Nothing to go on"},
        {"track_name": "On nothing", "recording_mbid": "rec-orphan"},
    )
    await playlists.batch_update_resolutions(
        pid,
        {tracks[1].id: TrackResolution("rg-chronicle", "artist_compilation", "chosen")},
    )

    summary = await _resolver(playlists, mb).resolve_playlist(pid)

    by_name = {t.track_name: t for t in await playlists.get_tracks(pid)}
    assert by_name["Owned"].resolution_status is None
    assert by_name["Chosen"].target_release_group_mbid == "rg-chronicle"
    assert by_name["Nothing to go on"].resolution_status == "unresolved"
    assert by_name["On nothing"].resolution_status == "unresolved"
    assert (summary.owned, summary.chosen, summary.unresolved) == (1, 1, 2)
    assert mb.home_calls == ["rec-orphan"]


@pytest.mark.asyncio
async def test_failed_lookup_keeps_prior_verdict(playlists):
    mb = FakeMb({"rec-fs": [WILLY]})
    pid, _ = await _playlist(
        playlists, {"track_name": "Fortunate Son", "recording_mbid": "rec-fs"}
    )
    await _resolver(playlists, mb).resolve_playlist(pid)

    mb.failing.add("rec-fs")
    summary = await _resolver(playlists, mb).resolve_playlist(pid)

    (track,) = await playlists.get_tracks(pid)
    assert (track.resolution_status, track.target_release_group_mbid) == (
        "resolved",
        "rg-willy",
    )
    assert summary.failed == 1


REMASTER_HOMES = {
    "rec-fs-remaster": [CHRONICLE, VA_60S],
    "rec-lodi-remaster": [CHRONICLE],
    "rec-tt-remaster": [CHRONICLE],
}
ARTIST_GROUPS = [
    _rg("rg-chronicle", "Chronicle", "1976-01-01", ("Compilation",)),
    _rg("rg-live", "Live in Europe", "1973-01-01", ("Live",)),
    _rg("rg-willy", "Willy and the Poor Boys", "1969-11-02"),
    _rg("rg-green", "Green River", "1969-08-03"),
]
STUDIO_TRACKLISTS = {
    "rg-green": [
        Track(position=2, title="Commotion", length=164_000, recording_id="rec-c"),
        Track(position=4, title="Lodi", length=191_000, recording_id="rec-lodi"),
    ],
    "rg-willy": [
        Track(position=6, title="Fortunate Son", length=140_000, recording_id="rec-fs"),
    ],
}


async def _remaster_playlist(playlists):
    return await _playlist(
        playlists,
        {
            "track_name": "Fortunate Son - Remastered 1985",
            "recording_mbid": "rec-fs-remaster",
            "duration": 141,
        },
        {"track_name": "Lodi", "recording_mbid": "rec-lodi-remaster", "duration": 192},
        # Same title, but 40s longer: a different take, never substituted.
        {
            "track_name": "Travelin' Band",
            "recording_mbid": "rec-tt-remaster",
            "duration": 168,
        },
    )


@pytest.mark.asyncio
async def test_match_playlist_policy_consolidates_onto_one_compilation(playlists):
    mb = FakeMb(REMASTER_HOMES, ARTIST_GROUPS)
    albums = FakeAlbums(STUDIO_TRACKLISTS)
    pid, _ = await _remaster_playlist(playlists)

    summary = await _resolver(playlists, mb, albums).resolve_playlist(pid)

    tracks = await playlists.get_tracks(pid)
    assert {t.target_release_group_mbid for t in tracks} == {"rg-chronicle"}
    assert {t.home_tier for t in tracks} == {"artist_compilation"}
    assert summary.albums_by_tier == {"artist_compilation": 1}
    assert albums.calls == []  # no substitute search without the studio/flag policy


@pytest.mark.asyncio
async def test_studio_policy_substitutes_matching_studio_recordings(playlists):
    mb = FakeMb(REMASTER_HOMES, ARTIST_GROUPS)
    albums = FakeAlbums(STUDIO_TRACKLISTS)
    pid, _ = await _remaster_playlist(playlists)

    await _resolver(playlists, mb, albums).resolve_playlist(
        pid, CompilationPolicy.STUDIO
    )

    by_name = {t.track_name: t for t in await playlists.get_tracks(pid)}
    fortunate = by_name["Fortunate Son - Remastered 1985"]
    assert (fortunate.recording_mbid, fortunate.target_release_group_mbid) == (
        "rec-fs",
        "rg-willy",
    )
    assert (
        by_name["Lodi"].recording_mbid,
        by_name["Lodi"].target_release_group_mbid,
    ) == (
        "rec-lodi",
        "rg-green",
    )
    travelin = by_name["Travelin' Band"]
    assert (travelin.recording_mbid, travelin.target_release_group_mbid) == (
        "rec-tt-remaster",
        "rg-chronicle",
    )
    # Studio albums only, earliest first; the live album is never searched.
    assert "rg-live" not in albums.calls and "rg-chronicle" not in albums.calls


@pytest.mark.asyncio
async def test_flag_policy_stores_options_and_requests_nothing(playlists):
    mb = FakeMb(REMASTER_HOMES, ARTIST_GROUPS)
    pid, _ = await _remaster_playlist(playlists)

    summary = await _resolver(
        playlists, mb, FakeAlbums(STUDIO_TRACKLISTS)
    ).resolve_playlist(pid, CompilationPolicy.FLAG)

    by_name = {t.track_name: t for t in await playlists.get_tracks(pid)}
    lodi = by_name["Lodi"]
    assert lodi.resolution_status == ResolutionStatus.FLAGGED
    assert lodi.target_release_group_mbid is None
    assert [(o["kind"], o["release_group_mbid"]) for o in lodi.flag_options] == [
        ("studio", "rg-green"),
        ("compilation", "rg-chronicle"),
    ]
    assert [o["kind"] for o in by_name["Travelin' Band"].flag_options] == [
        "compilation"
    ]
    assert (summary.flagged, summary.resolved) == (3, 0)


@pytest.mark.asyncio
async def test_live_recordings_are_never_substituted(playlists):
    mb = FakeMb({"rec-live": [LIVE, CHRONICLE]}, ARTIST_GROUPS)
    albums = FakeAlbums(STUDIO_TRACKLISTS)
    pid, _ = await _playlist(
        playlists,
        {"track_name": "Fortunate Son", "recording_mbid": "rec-live", "duration": 140},
    )
    await _resolver(playlists, mb, albums).resolve_playlist(
        pid, CompilationPolicy.STUDIO
    )

    (track,) = await playlists.get_tracks(pid)
    assert (track.recording_mbid, track.target_release_group_mbid) == (
        "rec-live",
        "rg-live",
    )


@pytest.mark.parametrize(
    ("title", "key"),
    [
        ("Fortunate Son - Remastered 1985", "fortunate son"),
        ("Proud Mary (2008 Remaster)", "proud mary"),
        ("Bad Moon Rising - Mono", "bad moon rising"),
        ("Green River - Single Version", "green river"),
        (
            "Born on the Bayou - Live at Woodstock",
            "born on the bayou live at woodstock",
        ),
        ("Suzie Q (Demo)", "suzie q demo"),
        ("Who'll Stop the Rain", "who ll stop the rain"),
        ("Déjà Vu", "deja vu"),
    ],
)
def test_song_title_key(title, key):
    assert song_title_key(title) == key


def test_same_song_needs_close_length_when_known():
    studio = Track(position=1, title="Fortunate Son", length=140_000, recording_id="r")
    assert same_song("Fortunate Son - Remastered", 142, studio)
    assert not same_song("Fortunate Son", 150, studio)
    assert not same_song("Fortunate Son - Live", 140, studio)
    assert same_song("Fortunate Son", None, studio)
    assert not same_song("Fortunate", None, studio)
