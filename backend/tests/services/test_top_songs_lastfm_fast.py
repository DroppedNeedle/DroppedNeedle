import pytest
from unittest.mock import AsyncMock, MagicMock

from infrastructure.queue.priority_queue import RequestPriority
from repositories.lastfm_models import LastFmTrack
from services.artist_discovery_service import ArtistDiscoveryService


ARTIST_MBID = "f4a31f0a-51dd-4fa7-986d-3095c40c5ed9"
ALBUM_RG = "bbbbbbbb-0000-0000-0000-000000000001"
EP_RG = "bbbbbbbb-0000-0000-0000-000000000002"
SINGLE_RG = "bbbbbbbb-0000-0000-0000-000000000003"

DISCOGRAPHY = [
    {"id": EP_RG, "title": "Album A", "primary-type": "EP", "first-release-date": "1999"},
    {
        "id": ALBUM_RG,
        "title": "Album A",
        "primary-type": "Album",
        "secondary-types": [],
        "first-release-date": "2000-10-24",
    },
    {"id": SINGLE_RG, "title": "Hit Single", "primary-type": "Single"},
]


def _make_service(
    tracks: list[LastFmTrack], track_albums: dict[str, object]
) -> tuple[ArtistDiscoveryService, AsyncMock, AsyncMock]:
    lb_repo = MagicMock()
    lb_repo.is_configured.return_value = False

    async def get_track_album(artist: str, track: str) -> str | None:
        album = track_albums.get(track)
        if isinstance(album, Exception):
            raise album
        return album

    lastfm_repo = AsyncMock()
    lastfm_repo.get_artist_top_tracks = AsyncMock(return_value=tracks)
    lastfm_repo.get_track_album = AsyncMock(side_effect=get_track_album)

    prefs = MagicMock()
    prefs.is_lastfm_enabled.return_value = True

    library_db = AsyncMock()
    library_db.get_all_artist_mbids = AsyncMock(return_value=set())

    memory_cache = AsyncMock()
    memory_cache.get = AsyncMock(return_value=None)
    memory_cache.set = AsyncMock()
    memory_cache.get_with_metadata = AsyncMock(return_value=(None, None))
    memory_cache.set_if_token = AsyncMock(return_value=True)
    memory_cache.capture_clear_token = MagicMock(return_value=("test-cache", 0))

    mb_repo = AsyncMock()
    mb_repo.get_release_group_id_from_release = AsyncMock(
        side_effect=AssertionError(
            "MusicBrainz resolution should NOT be called for Last.fm top-songs"
        )
    )
    mb_repo.get_release_groups_by_artist = AsyncMock(return_value=DISCOGRAPHY)

    library_repo = AsyncMock()
    library_repo.existing_artist_mbids = AsyncMock(return_value=set())

    svc = ArtistDiscoveryService(
        listenbrainz_repo=lb_repo,
        musicbrainz_repo=mb_repo,
        library_db=library_db,
        library_repo=library_repo,
        memory_cache=memory_cache,
        lastfm_repo=lastfm_repo,
        preferences_service=prefs,
    )
    return svc, mb_repo, lastfm_repo


def _track(name: str) -> LastFmTrack:
    return LastFmTrack(name=name, artist_name="Test Artist", mbid="stale-mbid", playcount=100)


class TestLastFmTopSongsReleaseGroups:
    @pytest.mark.asyncio
    async def test_resolves_track_album_to_studio_album_release_group(self):
        svc, _, _ = _make_service([_track("Song A")], {"Song A": "Album A (Bonus Track Version)"})

        result = await svc.get_top_songs(ARTIST_MBID, count=10, source="lastfm")

        assert result.songs[0].release_group_mbid == ALBUM_RG
        assert result.songs[0].release_name == "Album A (Bonus Track Version)"

    @pytest.mark.asyncio
    async def test_falls_back_to_same_titled_single_and_drops_foreign_album(self):
        svc, _, _ = _make_service(
            [_track("Hit Single")], {"Hit Single": "Some Various Artists Compilation"}
        )

        result = await svc.get_top_songs(ARTIST_MBID, count=10, source="lastfm")

        assert result.songs[0].release_group_mbid == SINGLE_RG
        assert result.songs[0].release_name is None

    @pytest.mark.asyncio
    async def test_unknown_album_keeps_lastfm_title_without_release_group(self):
        svc, _, _ = _make_service([_track("Deep Cut")], {"Deep Cut": "Unreleased Demos"})

        result = await svc.get_top_songs(ARTIST_MBID, count=10, source="lastfm")

        assert result.songs[0].release_group_mbid is None
        assert result.songs[0].release_name == "Unreleased Demos"

    @pytest.mark.asyncio
    async def test_track_album_lookup_failure_degrades_per_track(self):
        svc, _, _ = _make_service(
            [_track("Song A"), _track("Song B")],
            {"Song A": RuntimeError("Last.fm down"), "Song B": "Album A"},
        )

        result = await svc.get_top_songs(ARTIST_MBID, count=10, source="lastfm")

        assert [s.title for s in result.songs] == ["Song A", "Song B"]
        assert result.songs[0].release_group_mbid is None
        assert result.songs[1].release_group_mbid == ALBUM_RG

    @pytest.mark.asyncio
    async def test_uses_one_discography_browse_without_per_track_resolution(self):
        svc, mb_repo, lastfm_repo = _make_service(
            [_track("Song A"), _track("Hit Single")], {"Song A": "Album A"}
        )

        await svc.get_top_songs(ARTIST_MBID, count=10, source="lastfm")

        mb_repo.get_release_groups_by_artist.assert_awaited_once_with(
            ARTIST_MBID, limit=100, priority=RequestPriority.USER_INITIATED
        )
        mb_repo.get_release_group_id_from_release.assert_not_awaited()
        assert lastfm_repo.get_track_album.await_count == 2

    @pytest.mark.asyncio
    async def test_discography_failure_keeps_songs(self):
        svc, mb_repo, _ = _make_service([_track("Song A")], {"Song A": "Album A"})
        mb_repo.get_release_groups_by_artist.side_effect = RuntimeError("unavailable")

        result = await svc.get_top_songs(ARTIST_MBID, count=10, source="lastfm")

        assert result.songs[0].title == "Song A"
        assert result.songs[0].release_group_mbid is None
        assert result.songs[0].release_name == "Album A"
