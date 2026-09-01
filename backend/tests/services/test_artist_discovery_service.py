import asyncio
from types import SimpleNamespace

import pytest
from unittest.mock import AsyncMock, MagicMock

from api.v1.schemas.discovery import SimilarArtist
from repositories.lastfm_models import LastFmAlbum, LastFmSimilarArtist, LastFmTrack
from repositories.listenbrainz_models import (
    ListenBrainzRecording,
    ListenBrainzReleaseGroup,
)
from services.artist_discovery_service import (
    ArtistDiscoveryService,
    _dedupe_similar_artists,
)


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
    mb_repo.get_release_groups_by_artist = AsyncMock(return_value=[])
    library_repo = AsyncMock()

    svc = ArtistDiscoveryService(
        listenbrainz_repo=lb_repo,
        musicbrainz_repo=mb_repo,
        library_db=library_db,
        library_repo=library_repo,
        memory_cache=memory_cache,
        lastfm_repo=lastfm_repo,
        preferences_service=prefs,
    )
    return svc, lb_repo, lastfm_repo, prefs


class TestGetSimilarArtistsSource:
    @pytest.mark.asyncio
    async def test_default_source_uses_listenbrainz(self):
        svc, lb_repo, lastfm_repo, _ = _make_service()
        # LB returns data -> Last.fm is never consulted
        lb_repo.get_similar_artists = AsyncMock(
            return_value=[
                SimpleNamespace(
                    artist_mbid="mbid-a", artist_name="Artist A", listen_count=10
                )
            ]
        )

        result = await svc.get_similar_artists("mbid-123", count=5)

        assert result.source == "listenbrainz"
        assert len(result.similar_artists) == 1
        lb_repo.get_similar_artists.assert_called_once()
        lastfm_repo.get_similar_artists.assert_not_called()

    @pytest.mark.asyncio
    async def test_empty_listenbrainz_falls_back_to_lastfm(self):
        # LB popularity/similar is disabled or breaker-tripped upstream (2026-07);
        # an empty LB result must fall back to Last.fm so the section still fills
        svc, lb_repo, lastfm_repo, _ = _make_service()
        lb_repo.get_similar_artists = AsyncMock(return_value=[])
        lastfm_repo.get_similar_artists = AsyncMock(
            return_value=[
                LastFmSimilarArtist(
                    name="Fallback Artist", mbid="mbid-fb", match=0.9, url=""
                )
            ]
        )

        result = await svc.get_similar_artists("mbid-123", count=5)

        lastfm_repo.get_similar_artists.assert_called_once()
        assert any(a.name == "Fallback Artist" for a in result.similar_artists)

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

        assert svc._cache.set.await_count == 2

    @pytest.mark.asyncio
    async def test_lb_not_configured_returns_not_configured(self):
        svc, _, _, _ = _make_service(lb_configured=False)

        result = await svc.get_similar_artists("mbid-123", count=5)

        assert result.configured is False

    @pytest.mark.asyncio
    async def test_source_lastfm_marks_in_library(self):
        lastfm_similar = [
            LastFmSimilarArtist(name="In Lib", mbid="lib-mbid", match=0.9, url=""),
            LastFmSimilarArtist(
                name="Not In Lib", mbid="other-mbid", match=0.8, url=""
            ),
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
            LastFmTrack(
                name="Song A", artist_name="Artist", mbid="rec-a", playcount=5000
            ),
            LastFmTrack(
                name="Song B", artist_name="Artist", mbid="rec-b", playcount=3000
            ),
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

        assert svc._cache.set.await_count == 2


class TestGetTopAlbumsSource:
    @pytest.mark.asyncio
    async def test_source_lastfm_returns_albums(self):
        lastfm_albums = [
            LastFmAlbum(
                name="Album X", artist_name="Artist", mbid="alb-x", playcount=8000
            ),
            LastFmAlbum(
                name="Album Y", artist_name="Artist", mbid="alb-y", playcount=4000
            ),
        ]
        svc, lb_repo, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.return_value = lastfm_albums
        svc._library_repo.get_library_mbids = AsyncMock(return_value={"alb-x"})
        svc._library_repo.get_requested_mbids = AsyncMock(return_value=set())

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

        assert svc._cache.set.await_count == 2

    @pytest.mark.asyncio
    async def test_lb_empty_result_is_cached(self):
        svc, lb_repo, _, _ = _make_service()
        lb_repo.get_artist_top_release_groups.return_value = []

        await svc.get_top_albums("mbid-123", count=10)

        assert svc._cache.set.await_count == 2

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

        svc._library_repo.get_library_mbids = AsyncMock(return_value={"rg-a"})
        svc._library_repo.get_requested_mbids = AsyncMock(return_value={"rg-b"})

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
    async def test_lb_top_albums_survive_library_lookup_failure(self):
        svc, lb_repo, _, _ = _make_service()
        lb_repo.get_artist_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="Album 1",
                artist_name="Artist",
                listen_count=42,
                release_group_mbid="rg-1",
            )
        ]
        svc._library_repo.get_library_mbids = AsyncMock(
            side_effect=Exception("library down")
        )
        svc._library_repo.get_requested_mbids = AsyncMock(return_value=set())

        result = await svc.get_top_albums("mbid-123", count=10)

        assert len(result.albums) == 1
        assert result.albums[0].title == "Album 1"
        assert result.albums[0].in_library is False
        assert result.albums[0].requested is False

    @pytest.mark.asyncio
    async def test_source_lastfm_normalizes_mbids(self):
        lastfm_albums = [
            LastFmAlbum(
                name="Upper", artist_name="Artist", mbid="ALB-UPPER", playcount=100
            ),
            LastFmAlbum(
                name="Spaced", artist_name="Artist", mbid=" alb-spaced ", playcount=50
            ),
            LastFmAlbum(name="No MBID", artist_name="Artist", mbid=None, playcount=10),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.return_value = lastfm_albums
        svc._library_repo.get_library_mbids = AsyncMock(return_value={"alb-upper"})
        svc._library_repo.get_requested_mbids = AsyncMock(return_value={"alb-spaced"})

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
            LastFmAlbum(
                name="Album A",
                artist_name="Artist",
                mbid="release-mbid-a",
                playcount=100,
            ),
            LastFmAlbum(
                name="Album B",
                artist_name="Artist",
                mbid="release-mbid-b",
                playcount=50,
            ),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.return_value = lastfm_albums
        svc._library_repo.get_library_mbids = AsyncMock(return_value={"release-mbid-a"})
        svc._library_repo.get_requested_mbids = AsyncMock(return_value=set())

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
            LastFmAlbum(
                name="Album A",
                artist_name="Artist",
                mbid="already-rg-mbid",
                playcount=100,
            ),
        ]
        svc, _, lastfm_repo, _ = _make_service()
        lastfm_repo.get_artist_top_albums.return_value = lastfm_albums
        svc._library_repo.get_library_mbids = AsyncMock(return_value=set())
        svc._library_repo.get_requested_mbids = AsyncMock(return_value=set())

        result = await svc.get_top_albums("mbid-123", count=10, source="lastfm")

        assert result.albums[0].release_group_mbid == "already-rg-mbid"


class TestGetTopSongsLastFmNoAlbumResolution:
    @pytest.mark.asyncio
    async def test_source_lastfm_returns_null_album_fields(self):
        lastfm_tracks = [
            LastFmTrack(
                name="Song A", artist_name="Artist", mbid="rec-a", playcount=5000
            ),
            LastFmTrack(
                name="Song B", artist_name="Artist", mbid="rec-b", playcount=3000
            ),
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
            LastFmTrack(
                name="Song A", artist_name="Artist", mbid="rec-a", playcount=5000
            ),
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


def _make_factory(lb_repo=None, lastfm_repo=None) -> MagicMock:
    factory = MagicMock()
    factory.resolve_listenbrainz = AsyncMock(return_value=lb_repo)
    factory.resolve_lastfm = AsyncMock(return_value=lastfm_repo)
    return factory


def _make_peruser_service(*, lb_repo=None, lastfm_repo=None) -> ArtistDiscoveryService:
    """A service wired with a per-user client_factory (the production path).

    The injected global lb_repo is is_configured()==False to prove the gate goes
    through the factory, not the dead global repo.
    """
    return ArtistDiscoveryService(
        listenbrainz_repo=_make_lb_repo(configured=False),
        musicbrainz_repo=AsyncMock(),
        library_db=_make_library_db(),
        library_repo=AsyncMock(),
        memory_cache=_make_memory_cache(),
        lastfm_repo=lastfm_repo,
        preferences_service=_make_prefs(),
        client_factory=_make_factory(lb_repo=lb_repo, lastfm_repo=lastfm_repo),
    )


class TestPerUserResolution:
    """With a client_factory present, the configured gate follows the requesting
    user's own ListenBrainz/Last.fm link - not the (dead) global repo."""

    @pytest.mark.asyncio
    async def test_linked_listenbrainz_admits_and_resolves_user(self):
        lb_repo = _make_lb_repo()
        svc = _make_peruser_service(lb_repo=lb_repo)

        result = await svc.get_similar_artists("mbid-123", count=5, user_id="user-1")

        assert result.configured is True
        svc._client_factory.resolve_listenbrainz.assert_awaited_once_with("user-1")
        lb_repo.get_similar_artists.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_unlinked_listenbrainz_returns_not_configured(self):
        svc = _make_peruser_service(lb_repo=None)

        result = await svc.get_similar_artists("mbid-123", count=5, user_id="user-1")

        assert result.configured is False

    @pytest.mark.asyncio
    async def test_missing_user_id_returns_not_configured(self):
        lb_repo = _make_lb_repo()
        svc = _make_peruser_service(lb_repo=lb_repo)

        result = await svc.get_similar_artists("mbid-123", count=5)

        assert result.configured is False
        lb_repo.get_similar_artists.assert_not_called()
        svc._client_factory.resolve_listenbrainz.assert_not_called()

    @pytest.mark.asyncio
    async def test_top_songs_follow_user_link(self):
        linked = _make_peruser_service(lb_repo=_make_lb_repo())
        unlinked = _make_peruser_service(lb_repo=None)

        assert (
            await linked.get_top_songs("mbid-123", count=5, user_id="user-1")
        ).configured is True
        assert (
            await unlinked.get_top_songs("mbid-123", count=5, user_id="user-1")
        ).configured is False

    @pytest.mark.asyncio
    async def test_top_albums_follow_user_link(self):
        linked = _make_peruser_service(lb_repo=_make_lb_repo())
        unlinked = _make_peruser_service(lb_repo=None)

        assert (
            await linked.get_top_albums("mbid-123", count=5, user_id="user-1")
        ).configured is True
        assert (
            await unlinked.get_top_albums("mbid-123", count=5, user_id="user-1")
        ).configured is False

    @pytest.mark.asyncio
    async def test_linked_lastfm_admits(self):
        lastfm_repo = _make_lastfm_repo()
        svc = _make_peruser_service(lastfm_repo=lastfm_repo)

        result = await svc.get_similar_artists(
            "mbid-123", count=5, source="lastfm", user_id="user-1"
        )

        assert result.source == "lastfm"
        assert result.configured is True
        svc._client_factory.resolve_lastfm.assert_awaited_once_with("user-1")

    @pytest.mark.asyncio
    async def test_unlinked_lastfm_returns_not_configured(self):
        svc = _make_peruser_service(lastfm_repo=None)

        result = await svc.get_similar_artists(
            "mbid-123", count=5, source="lastfm", user_id="user-1"
        )

        assert result.source == "lastfm"
        assert result.configured is False


@pytest.mark.asyncio
async def test_top_songs_mb_fallback_fences_old_cache_write_and_new_read():
    import repositories.musicbrainz_base as mb_base

    svc, lb_repo, _lastfm_repo, _prefs = _make_service()
    recording = ListenBrainzRecording(
        track_name="Track",
        artist_name="Artist",
        listen_count=10,
        release_name="Album",
        release_mbid="release-id",
        recording_mbid="recording-id",
    )
    lb_repo.get_artist_top_recordings = AsyncMock(return_value=[recording])
    lb_repo.get_recording_release_groups_batch = AsyncMock(return_value={})
    svc._mb_repo.get_recording_position_on_release = AsyncMock(return_value=None)
    old_gate = asyncio.Event()
    old_started = asyncio.Event()
    new_started = asyncio.Event()
    cache_entries: dict[str, object] = {}
    svc._cache.get = AsyncMock(side_effect=lambda key: cache_entries.get(key))

    async def cache_set(key, value, **_kwargs):
        cache_entries[key] = value

    svc._cache.set = AsyncMock(side_effect=cache_set)
    original_source = mb_base.capture_mb_source_context()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="artist-top-songs-old",
        generation=original_source.generation + 1,
    )
    old_generation = mb_base.get_mb_source_generation()

    async def resolve(_release_id):
        generation = mb_base.get_mb_source_generation()
        if generation == old_generation:
            old_started.set()
            await old_gate.wait()
            return "rg-old"
        new_started.set()
        return "rg-new"

    svc._mb_repo.get_release_group_id_from_release = resolve
    try:
        old_task = asyncio.create_task(svc.get_top_songs("artist-id", count=1))
        await old_started.wait()
        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="artist-top-songs-new",
            generation=old_generation + 1,
        )
        new_task = asyncio.create_task(svc.get_top_songs("artist-id", count=1))
        await new_started.wait()
        new_result = await new_task
        old_gate.set()
        old_result = await old_task
    finally:
        old_gate.set()
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source.source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )

    cache_key = svc._build_cache_key("top_songs", "artist-id", 1, "listenbrainz")
    assert old_result.songs[0].release_group_mbid == "rg-old"
    assert new_result.songs[0].release_group_mbid == "rg-new"
    assert cache_entries[cache_key] is new_result


@pytest.mark.asyncio
async def test_top_albums_mb_fallback_separates_generations_and_fences_cache():
    import repositories.musicbrainz_base as mb_base

    svc, lb_repo, _lastfm_repo, _prefs = _make_service()
    recording = ListenBrainzRecording(
        track_name="Track",
        artist_name="Artist",
        listen_count=10,
        release_name="Album",
        release_mbid="release-id",
        recording_mbid="recording-id",
    )
    lb_repo.get_artist_top_release_groups = AsyncMock(return_value=[])
    lb_repo.get_artist_top_recordings = AsyncMock(return_value=[recording])
    lb_repo.get_recording_release_groups_batch = AsyncMock(return_value={})
    svc._library_repo.get_library_mbids = AsyncMock(return_value=set())
    svc._library_repo.get_requested_mbids = AsyncMock(return_value=set())
    old_gate = asyncio.Event()
    old_started = asyncio.Event()
    new_started = asyncio.Event()
    cache_entries: dict[str, object] = {}
    svc._cache.get = AsyncMock(side_effect=lambda key: cache_entries.get(key))

    async def cache_set(key, value, **_kwargs):
        cache_entries[key] = value

    svc._cache.set = AsyncMock(side_effect=cache_set)
    original_source = mb_base.capture_mb_source_context()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="artist-top-albums-old",
        generation=original_source.generation + 1,
    )
    old_generation = mb_base.get_mb_source_generation()

    async def resolve(_release_id):
        generation = mb_base.get_mb_source_generation()
        if generation == old_generation:
            old_started.set()
            await old_gate.wait()
            return "rg-old"
        new_started.set()
        return "rg-new"

    svc._mb_repo.get_release_group_id_from_release = resolve
    try:
        old_task = asyncio.create_task(svc.get_top_albums("artist-id", count=1))
        await old_started.wait()
        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="artist-top-albums-new",
            generation=old_generation + 1,
        )
        new_task = asyncio.create_task(svc.get_top_albums("artist-id", count=1))
        await new_started.wait()
        new_result = await new_task
        old_gate.set()
        old_result = await old_task
    finally:
        old_gate.set()
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source.source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )

    cache_key = svc._build_cache_key("top_albums", "artist-id", 1, "listenbrainz")
    assert old_result.albums[0].release_group_mbid == "rg-old"
    assert new_result.albums[0].release_group_mbid == "rg-new"
    assert cache_entries[cache_key] is new_result
    assert svc._top_albums_in_flight == {}
