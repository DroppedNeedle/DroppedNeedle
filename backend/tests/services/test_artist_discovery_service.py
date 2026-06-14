import pytest
from unittest.mock import AsyncMock, MagicMock, PropertyMock

from api.v1.schemas.discovery import SimilarArtist
from repositories.lastfm_models import LastFmAlbum, LastFmSimilarArtist, LastFmTrack
from repositories.listenbrainz_models import ListenBrainzRecording, ListenBrainzReleaseGroup
from services.artist_discovery_service import ArtistDiscoveryService, _dedupe_similar_artists


def _make_lb_repo(configured: bool = True) -> MagicMock:
    repo = MagicMock()
    repo.is_configured.return_value = configured
    repo.get_similar_artists = AsyncMock(return_value=[])
    repo.get_artist_top_recordings = AsyncMock(return_value=[])
    repo.get_artist_top_release_groups = AsyncMock(return_value=[])
    return repo


def _make_lastfm_repo(enabled: bool = True) -> AsyncMock:
    repo = AsyncMock()
    repo.get_similar_artists = AsyncMock(return_value=[])
    repo.get_artist_top_tracks = AsyncMock(return_value=[])
    repo.get_artist_top_albums = AsyncMock(return_value=[])
    return repo


def _make_prefs(enabled: bool = True) -> MagicMock:
    prefs = MagicMock()
    prefs.is_lastfm_enabled.return_value = enabled
    return prefs


def _make_library_db() -> AsyncMock:
    cache = AsyncMock()
    cache.get_all_artist_mbids = AsyncMock(return_value=set())
    return cache


def _make_memory_cache() -> AsyncMock:
    cache = AsyncMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    return cache


def _make_service(
    lb_configured: bool = True,
    lastfm_enabled: bool = True,
) -> tuple[ArtistDiscoveryService, MagicMock, AsyncMock, MagicMock]:
    lb_repo = _make_lb_repo(configured=lb_configured)
    lastfm_repo = _make_lastfm_repo(enabled=lastfm_enabled)
    prefs = _make_prefs(enabled=lastfm_enabled)
    library_db = _make_library_db()
    memory_cache = _make_memory_cache()
    mb_repo = AsyncMock()
    lidarr_repo = AsyncMock()

    svc = ArtistDiscoveryService(
        listenbrainz_repo=lb_repo,
        musicbrainz_repo=mb_repo,
        library_db=library_db,
        lidarr_repo=lidarr_repo,
        memory_cache=memory_cache,
        lastfm_repo=lastfm_repo,
        preferences_service=prefs,
    )
    return svc, lb_repo, lastfm_repo, prefs


class TestGetSimilarArtistsSource:
    @pytest.mark.asyncio
    async def test_default_source_uses_listenbrainz(self):
        svc, lb_repo, lastfm_repo, _ = _make_service()

        result = await svc.get_similar_artists("mbid-123", count=5)

        assert result.source == "listenbrainz"
        lb_repo.get_similar_artists.assert_called_once()
        lastfm_repo.get_similar_artists.assert_not_called()

    @pytest.mark.asyncio
    async def test_source_lastfm_calls_lastfm(self):
        lastfm_similar = [
            LastFmSimilarArtist(name="Artist A", mbid="mbid-a", match=0.9, url=""),
            LastFmSimilarArtist(name="Artist B", mbid="mbid-b", match=0.8, url=""),
        ]
        svc, lb_repo, lastfm_repo, _ = _make_service()
        lastfm_repo.get_similar_artists.return_value = lastfm_similar

        result = await svc.get_similar_artists("mbid-123", count=5, source="lastfm")

        assert result.source == "lastfm"
        lastfm_repo.get_similar_artists.assert_called_once()
        lb_repo.get_similar_artists.assert_not_called()
        assert len(result.similar_artists) == 2
        assert result.similar_artists[0].name == "Artist A"
        assert result.similar_artists[0].musicbrainz_id == "mbid-a"

    @pytest.mark.asyncio
    async def test_source_lastfm_filters_artists_without_mbid(self):
        lastfm_similar = [
            LastFmSimilarArtist(name="Has MBID", mbid="mbid-a", match=0.9, url=""),
            LastFmSimilarArtist(name="No MBID", mbid=None, match=0.8, url=""),
            LastFmSimilarArtist(name="Empty MBID", mbid="", match=0.7, url=""),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_similar_artists.return_value = lastfm_similar

        result = await svc.get_similar_artists("mbid-123", count=10, source="lastfm")

        assert len(result.similar_artists) == 1
        assert result.similar_artists[0].name == "Has MBID"

    @pytest.mark.asyncio
    async def test_source_lastfm_disabled_returns_not_configured(self):
        svc, _, _, _ = _make_service(lastfm_enabled=False)

        result = await svc.get_similar_artists("mbid-123", count=5, source="lastfm")

        assert result.source == "lastfm"
        assert result.configured is False
        assert result.similar_artists == []

    @pytest.mark.asyncio
    async def test_source_lastfm_handles_exception(self):
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_similar_artists.side_effect = Exception("API error")

        result = await svc.get_similar_artists("mbid-123", count=5, source="lastfm")

        assert result.source == "lastfm"
        assert result.similar_artists == []

    @pytest.mark.asyncio
    async def test_lastfm_exception_result_is_cached(self):
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_similar_artists.side_effect = Exception("API error")

        await svc.get_similar_artists("mbid-123", count=5, source="lastfm")

        assert svc._cache.set.await_count == 1

    @pytest.mark.asyncio
    async def test_lb_exception_result_is_cached(self):
        svc, lb_repo, _, _ = _make_service()
        lb_repo.get_similar_artists.side_effect = Exception("LB error")

        await svc.get_similar_artists("mbid-123", count=5)

        assert svc._cache.set.await_count == 1

    @pytest.mark.asyncio
    async def test_lb_not_configured_returns_not_configured(self):
        svc, _, _, _ = _make_service(lb_configured=False)

        result = await svc.get_similar_artists("mbid-123", count=5)

        assert result.configured is False

    @pytest.mark.asyncio
    async def test_source_lastfm_marks_in_library(self):
        lastfm_similar = [
            LastFmSimilarArtist(name="In Lib", mbid="lib-mbid", match=0.9, url=""),
            LastFmSimilarArtist(name="Not In Lib", mbid="other-mbid", match=0.8, url=""),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_similar_artists.return_value = lastfm_similar
        svc._library_db.get_all_artist_mbids.return_value = {"lib-mbid"}

        result = await svc.get_similar_artists("mbid-123", count=10, source="lastfm")

        assert result.similar_artists[0].in_library is True
        assert result.similar_artists[1].in_library is False

    @pytest.mark.asyncio
    async def test_cache_key_includes_count_for_similar(self):
        svc, lb_repo, _, _ = _make_service()

        await svc.get_similar_artists("mbid-123", count=5)
        await svc.get_similar_artists("mbid-123", count=10)

        assert lb_repo.get_similar_artists.await_count == 2

    @pytest.mark.asyncio
    async def test_same_count_hits_cache_for_similar(self):
        svc, lb_repo, _, _ = _make_service()
        svc._cache.get.side_effect = [
            None,
            MagicMock(similar_artists=[]),
        ]

        await svc.get_similar_artists("mbid-123", count=5)
        await svc.get_similar_artists("mbid-123", count=5)

        assert lb_repo.get_similar_artists.await_count == 1


class TestGetTopSongsSource:
    @pytest.mark.asyncio
    async def test_source_lastfm_returns_tracks(self):
        lastfm_tracks = [
            LastFmTrack(name="Song A", artist_name="Artist", mbid="rec-a", playcount=5000),
            LastFmTrack(name="Song B", artist_name="Artist", mbid="rec-b", playcount=3000),
        ]
        svc, lb_repo, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_tracks.return_value = lastfm_tracks

        result = await svc.get_top_songs("mbid-123", count=10, source="lastfm")

        assert result.source == "lastfm"
        assert result.configured is True
        assert len(result.songs) == 2
        assert result.songs[0].title == "Song A"
        assert result.songs[0].listen_count == 5000
        assert result.songs[1].title == "Song B"
        lastfm_repo.get_artist_top_tracks.assert_called_once()
        lb_repo.get_artist_top_recordings.assert_not_called()

    @pytest.mark.asyncio
    async def test_source_lastfm_disabled_returns_not_configured(self):
        svc, _, _, _ = _make_service(lastfm_enabled=False)

        result = await svc.get_top_songs("mbid-123", count=10, source="lastfm")

        assert result.source == "lastfm"
        assert result.songs == []
        assert result.configured is False

    @pytest.mark.asyncio
    async def test_source_lastfm_handles_exception(self):
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_tracks.side_effect = Exception("API error")

        result = await svc.get_top_songs("mbid-123", count=10, source="lastfm")

        assert result.source == "lastfm"
        assert result.songs == []

    @pytest.mark.asyncio
    async def test_lastfm_exception_result_is_cached(self):
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_tracks.side_effect = Exception("API error")

        await svc.get_top_songs("mbid-123", count=10, source="lastfm")

        assert svc._cache.set.await_count == 1

    @pytest.mark.asyncio
    async def test_lb_exception_result_is_cached(self):
        svc, lb_repo, _, _ = _make_service()
        lb_repo.get_artist_top_recordings.side_effect = Exception("LB error")

        await svc.get_top_songs("mbid-123", count=10)

        assert svc._cache.set.await_count == 1


class TestGetTopAlbumsSource:
    @pytest.mark.asyncio
    async def test_source_lastfm_returns_albums(self):
        lastfm_albums = [
            LastFmAlbum(name="Album X", artist_name="Artist", mbid="alb-x", playcount=8000),
            LastFmAlbum(name="Album Y", artist_name="Artist", mbid="alb-y", playcount=4000),
        ]
        svc, lb_repo, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.return_value = lastfm_albums
        svc._lidarr_repo.get_library_mbids = AsyncMock(return_value={"alb-x"})
        svc._lidarr_repo.get_requested_mbids = AsyncMock(return_value=set())

        result = await svc.get_top_albums("mbid-123", count=10, source="lastfm")

        assert result.source == "lastfm"
        assert result.configured is True
        assert len(result.albums) == 2
        assert result.albums[0].title == "Album X"
        assert result.albums[0].listen_count == 8000
        assert result.albums[0].in_library is True
        assert result.albums[1].in_library is False
        lastfm_repo.get_artist_top_albums.assert_called_once()
        lb_repo.get_artist_top_release_groups.assert_not_called()

    @pytest.mark.asyncio
    async def test_source_lastfm_disabled_returns_not_configured(self):
        svc, _, _, _ = _make_service(lastfm_enabled=False)

        result = await svc.get_top_albums("mbid-123", count=10, source="lastfm")

        assert result.source == "lastfm"
        assert result.albums == []
        assert result.configured is False

    @pytest.mark.asyncio
    async def test_source_lastfm_handles_exception(self):
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.side_effect = Exception("API error")

        result = await svc.get_top_albums("mbid-123", count=10, source="lastfm")

        assert result.source == "lastfm"
        assert result.albums == []

    @pytest.mark.asyncio
    async def test_lastfm_exception_result_is_cached(self):
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.side_effect = Exception("API error")

        await svc.get_top_albums("mbid-123", count=10, source="lastfm")

        assert svc._cache.set.await_count == 1

    @pytest.mark.asyncio
    async def test_lb_exception_result_is_cached(self):
        svc, lb_repo, _, _ = _make_service()
        lb_repo.get_artist_top_release_groups.side_effect = Exception("LB error")

        await svc.get_top_albums("mbid-123", count=10)

        assert svc._cache.set.await_count == 1

    @pytest.mark.asyncio
    async def test_lb_empty_result_is_cached(self):
        svc, lb_repo, _, _ = _make_service()
        lb_repo.get_artist_top_release_groups.return_value = []

        await svc.get_top_albums("mbid-123", count=10)

        assert svc._cache.set.await_count == 1

    @pytest.mark.asyncio
    async def test_lb_empty_release_groups_falls_back_to_recordings(self):
        svc, lb_repo, _, _ = _make_service()
        lb_repo.get_artist_top_release_groups.return_value = []
        lb_repo.get_artist_top_recordings.return_value = [
            ListenBrainzRecording(
                track_name="Track A1",
                artist_name="Artist",
                listen_count=12,
                release_name="Album A",
                release_mbid="rel-a",
            ),
            ListenBrainzRecording(
                track_name="Track A2",
                artist_name="Artist",
                listen_count=9,
                release_name="Album A",
                release_mbid="rel-a",
            ),
            ListenBrainzRecording(
                track_name="Track B1",
                artist_name="Artist",
                listen_count=7,
                release_name="Album B",
                release_mbid="rel-b",
            ),
        ]

        svc._lidarr_repo.get_library_mbids = AsyncMock(return_value={"rg-a"})
        svc._lidarr_repo.get_requested_mbids = AsyncMock(return_value={"rg-b"})

        async def _resolve_release_group(release_mbid: str):
            return {"rel-a": "rg-a", "rel-b": "rg-b"}.get(release_mbid)

        svc._mb_repo.get_release_group_id_from_release = _resolve_release_group

        result = await svc.get_top_albums("mbid-123", count=10)

        assert len(result.albums) == 2
        assert result.albums[0].title == "Album A"
        assert result.albums[0].listen_count == 21
        assert result.albums[0].release_group_mbid == "rg-a"
        assert result.albums[0].in_library is True
        assert result.albums[1].title == "Album B"
        assert result.albums[1].release_group_mbid == "rg-b"
        assert result.albums[1].requested is True

    @pytest.mark.asyncio
    async def test_lb_top_albums_survive_lidarr_lookup_failure(self):
        svc, lb_repo, _, _ = _make_service()
        lb_repo.get_artist_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="Album 1",
                artist_name="Artist",
                listen_count=42,
                release_group_mbid="rg-1",
            )
        ]
        svc._lidarr_repo.get_library_mbids = AsyncMock(side_effect=Exception("lidarr down"))
        svc._lidarr_repo.get_requested_mbids = AsyncMock(return_value=set())

        result = await svc.get_top_albums("mbid-123", count=10)

        assert len(result.albums) == 1
        assert result.albums[0].title == "Album 1"
        assert result.albums[0].in_library is False
        assert result.albums[0].requested is False

    @pytest.mark.asyncio
    async def test_source_lastfm_normalizes_mbids(self):
        lastfm_albums = [
            LastFmAlbum(name="Upper", artist_name="Artist", mbid="ALB-UPPER", playcount=100),
            LastFmAlbum(name="Spaced", artist_name="Artist", mbid=" alb-spaced ", playcount=50),
            LastFmAlbum(name="No MBID", artist_name="Artist", mbid=None, playcount=10),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.return_value = lastfm_albums
        svc._lidarr_repo.get_library_mbids = AsyncMock(return_value={"alb-upper"})
        svc._lidarr_repo.get_requested_mbids = AsyncMock(return_value={"alb-spaced"})

        result = await svc.get_top_albums("mbid-123", count=10, source="lastfm")

        assert result.albums[0].release_group_mbid == "alb-upper"
        assert result.albums[0].in_library is True
        assert result.albums[1].release_group_mbid == "alb-spaced"
        assert result.albums[1].requested is True
        assert result.albums[2].release_group_mbid is None
        assert result.albums[2].in_library is False
        assert result.albums[2].requested is False

    @pytest.mark.asyncio
    async def test_source_lastfm_uses_raw_mbids_without_resolution(self):
        lastfm_albums = [
            LastFmAlbum(name="Album A", artist_name="Artist", mbid="release-mbid-a", playcount=100),
            LastFmAlbum(name="Album B", artist_name="Artist", mbid="release-mbid-b", playcount=50),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.return_value = lastfm_albums
        svc._lidarr_repo.get_library_mbids = AsyncMock(return_value={"release-mbid-a"})
        svc._lidarr_repo.get_requested_mbids = AsyncMock(return_value=set())

        svc._mb_repo.get_release_group_id_from_release = AsyncMock(
            side_effect=AssertionError("Resolution should not be called")
        )

        result = await svc.get_top_albums("mbid-123", count=10, source="lastfm")

        assert result.albums[0].release_group_mbid == "release-mbid-a"
        assert result.albums[0].in_library is True
        assert result.albums[1].release_group_mbid == "release-mbid-b"
        assert result.albums[1].in_library is False

    @pytest.mark.asyncio
    async def test_source_lastfm_keeps_raw_mbid_directly(self):
        lastfm_albums = [
            LastFmAlbum(name="Album A", artist_name="Artist", mbid="already-rg-mbid", playcount=100),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.return_value = lastfm_albums
        svc._lidarr_repo.get_library_mbids = AsyncMock(return_value=set())
        svc._lidarr_repo.get_requested_mbids = AsyncMock(return_value=set())

        result = await svc.get_top_albums("mbid-123", count=10, source="lastfm")

        assert result.albums[0].release_group_mbid == "already-rg-mbid"


class TestGetTopSongsLastFmNoAlbumResolution:
    @pytest.mark.asyncio
    async def test_source_lastfm_returns_null_album_fields(self):
        lastfm_tracks = [
            LastFmTrack(name="Song A", artist_name="Artist", mbid="rec-a", playcount=5000),
            LastFmTrack(name="Song B", artist_name="Artist", mbid="rec-b", playcount=3000),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_tracks.return_value = lastfm_tracks

        result = await svc.get_top_songs("mbid-123", count=10, source="lastfm")

        assert len(result.songs) == 2
        assert result.source == "lastfm"
        for song in result.songs:
            assert song.release_group_mbid is None
            assert song.release_name is None

    @pytest.mark.asyncio
    async def test_source_lastfm_preserves_track_metadata(self):
        lastfm_tracks = [
            LastFmTrack(name="Song A", artist_name="Artist", mbid="rec-a", playcount=5000),
            LastFmTrack(name="Song B", artist_name="Artist", mbid=None, playcount=3000),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_tracks.return_value = lastfm_tracks

        result = await svc.get_top_songs("mbid-123", count=10, source="lastfm")

        assert result.songs[0].title == "Song A"
        assert result.songs[0].recording_mbid == "rec-a"
        assert result.songs[0].listen_count == 5000
        assert result.songs[1].title == "Song B"
        assert result.songs[1].recording_mbid is None
        assert result.songs[1].listen_count == 3000


class TestDedupeSimilarArtists:
    """Guards the similar-artists carousel against each_key_duplicate."""

    def test_drops_duplicate_mbids_keeping_first(self):
        artists = [
            SimilarArtist(musicbrainz_id="mbid-a", name="Artist A"),
            SimilarArtist(musicbrainz_id="mbid-b", name="Artist B"),
            SimilarArtist(musicbrainz_id="mbid-a", name="Artist A (dupe)"),
        ]
        result = _dedupe_similar_artists(artists)
        assert [a.musicbrainz_id for a in result] == ["mbid-a", "mbid-b"]
        assert result[0].name == "Artist A"

    def test_dedupe_is_case_insensitive(self):
        artists = [
            SimilarArtist(musicbrainz_id="MBID-A", name="Artist A"),
            SimilarArtist(musicbrainz_id="mbid-a", name="Artist A (lower)"),
        ]
        result = _dedupe_similar_artists(artists)
        assert len(result) == 1
        assert result[0].musicbrainz_id == "MBID-A"

    def test_drops_entries_without_mbid(self):
        artists = [
            SimilarArtist(musicbrainz_id="", name="No id"),
            SimilarArtist(musicbrainz_id="mbid-c", name="Has id"),
        ]
        result = _dedupe_similar_artists(artists)
        assert [a.musicbrainz_id for a in result] == ["mbid-c"]

    def test_empty_input(self):
        assert _dedupe_similar_artists([]) == []