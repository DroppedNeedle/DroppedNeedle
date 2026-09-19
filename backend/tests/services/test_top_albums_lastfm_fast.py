import pytest
from unittest.mock import AsyncMock, MagicMock

from infrastructure.queue.priority_queue import RequestPriority
from repositories.lastfm_models import LastFmAlbum
from services.artist_discovery_service import ArtistDiscoveryService


ARTIST_MBID = "f4a31f0a-51dd-4fa7-986d-3095c40c5ed9"
RELEASE_MBID_1 = "aaaaaaaa-0000-0000-0000-000000000001"
RELEASE_MBID_2 = "aaaaaaaa-0000-0000-0000-000000000002"
RELEASE_GROUP_MBID_1 = "bbbbbbbb-0000-0000-0000-000000000001"
RELEASE_GROUP_MBID_2 = "bbbbbbbb-0000-0000-0000-000000000002"


def _make_lastfm_albums() -> list[LastFmAlbum]:
    return [
        LastFmAlbum(
            name="Album A",
            artist_name="Test Artist",
            mbid=RELEASE_MBID_1,
            playcount=5000,
        ),
        LastFmAlbum(
            name="Album B",
            artist_name="Test Artist",
            mbid=RELEASE_MBID_2,
            playcount=3000,
        ),
        LastFmAlbum(
            name="Album C (no mbid)", artist_name="Test Artist", mbid="", playcount=1000
        ),
    ]


def _make_service() -> tuple[ArtistDiscoveryService, AsyncMock]:
    lb_repo = MagicMock()
    lb_repo.is_configured.return_value = False

    lastfm_repo = AsyncMock()
    lastfm_repo.get_artist_top_albums = AsyncMock(return_value=_make_lastfm_albums())

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
            "MusicBrainz resolution should NOT be called for Last.fm top-albums"
        )
    )
    mb_repo.get_release_groups_by_artist = AsyncMock(
        return_value=[
            {"id": RELEASE_GROUP_MBID_1, "title": "Album A"},
            {"id": RELEASE_GROUP_MBID_2, "title": "Album B"},
        ]
    )

    library_repo = AsyncMock()
    library_repo.get_library_mbids = AsyncMock(return_value={RELEASE_GROUP_MBID_1})
    library_repo.get_requested_mbids = AsyncMock(return_value={RELEASE_GROUP_MBID_2})

    svc = ArtistDiscoveryService(
        listenbrainz_repo=lb_repo,
        musicbrainz_repo=mb_repo,
        library_db=library_db,
        library_repo=library_repo,
        memory_cache=memory_cache,
        lastfm_repo=lastfm_repo,
        preferences_service=prefs,
    )
    return svc, mb_repo


class TestLastFmTopAlbumsCanonicalization:
    @pytest.mark.asyncio
    async def test_uses_one_discography_browse_without_per_album_resolution(self):
        svc, mb_repo = _make_service()

        result = await svc.get_top_albums(ARTIST_MBID, count=10, source="lastfm")

        assert len(result.albums) == 3
        mb_repo.get_release_group_id_from_release.assert_not_awaited()
        mb_repo.get_release_groups_by_artist.assert_awaited_once_with(
            ARTIST_MBID, limit=100, priority=RequestPriority.USER_INITIATED
        )
        mb_repo.get_artist_release_groups_with_context.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_uses_canonical_discography_mbid(self):
        svc, _ = _make_service()

        result = await svc.get_top_albums(ARTIST_MBID, count=10, source="lastfm")

        assert result.albums[0].release_group_mbid == RELEASE_GROUP_MBID_1
        assert result.albums[1].release_group_mbid == RELEASE_GROUP_MBID_2
        assert result.albums[2].release_group_mbid is None

    @pytest.mark.asyncio
    async def test_library_flags_use_canonical_mbid(self):
        svc, _ = _make_service()

        result = await svc.get_top_albums(ARTIST_MBID, count=10, source="lastfm")

        assert result.albums[0].in_library is True
        assert result.albums[0].requested is False
        assert result.albums[1].in_library is False
        assert result.albums[1].requested is True
        assert result.albums[2].in_library is False
        assert result.albums[2].requested is False

    @pytest.mark.asyncio
    async def test_ambiguous_title_keeps_lastfm_mbid(self):
        svc, mb_repo = _make_service()
        mb_repo.get_release_groups_by_artist.return_value = [
            {"id": RELEASE_GROUP_MBID_1, "title": "Album A"},
            {"id": RELEASE_GROUP_MBID_2, "title": " album   a "},
        ]

        result = await svc.get_top_albums(ARTIST_MBID, count=10, source="lastfm")

        assert result.albums[0].release_group_mbid == RELEASE_MBID_1

    @pytest.mark.asyncio
    async def test_duplicate_editions_collapse_to_one_release_group(self):
        svc, _ = _make_service()
        svc._lastfm_repo.get_artist_top_albums.return_value = [
            LastFmAlbum(
                name="Album A",
                artist_name="Test Artist",
                mbid=RELEASE_MBID_1,
                playcount=5000,
            ),
            LastFmAlbum(
                name=" album   a ",
                artist_name="Test Artist",
                mbid="edition-two",
                playcount=3000,
            ),
        ]

        result = await svc.get_top_albums(ARTIST_MBID, count=10, source="lastfm")

        assert len(result.albums) == 1
        assert result.albums[0].release_group_mbid == RELEASE_GROUP_MBID_1

    @pytest.mark.asyncio
    async def test_unique_deluxe_title_uses_base_release_group(self):
        svc, _ = _make_service()
        svc._lastfm_repo.get_artist_top_albums.return_value = [
            LastFmAlbum(
                name="Album A (Deluxe Edition)",
                artist_name="Test Artist",
                mbid=None,
                playcount=5000,
            )
        ]

        result = await svc.get_top_albums(ARTIST_MBID, count=10, source="lastfm")

        assert result.albums[0].release_group_mbid == RELEASE_GROUP_MBID_1

    @pytest.mark.asyncio
    async def test_discography_failure_keeps_lastfm_result(self):
        svc, mb_repo = _make_service()
        mb_repo.get_release_groups_by_artist.side_effect = RuntimeError("unavailable")

        result = await svc.get_top_albums(ARTIST_MBID, count=10, source="lastfm")

        assert result.albums[0].release_group_mbid == RELEASE_MBID_1
        assert result.albums[1].release_group_mbid == RELEASE_MBID_2
        assert result.albums[2].release_group_mbid is None

    @pytest.mark.asyncio
    async def test_source_is_lastfm(self):
        svc, _ = _make_service()

        result = await svc.get_top_albums(ARTIST_MBID, count=10, source="lastfm")

        assert result.source == "lastfm"
