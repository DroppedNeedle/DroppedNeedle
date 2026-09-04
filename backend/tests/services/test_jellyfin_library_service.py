from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.jellyfin import (
    JellyfinAlbumSummary,
    JellyfinArtistSummary,
    JellyfinFavoritesExpanded,
    JellyfinHubResponse,
)
from repositories.jellyfin_models import JellyfinItem
from services.jellyfin_library_service import JellyfinLibraryService


def _make_service() -> tuple[JellyfinLibraryService, MagicMock]:
    repo = MagicMock()
    repo.is_configured.return_value = True
    repo.get_albums = AsyncMock(return_value=([], 0))
    repo.get_album_tracks = AsyncMock(return_value=[])
    repo.get_album_detail = AsyncMock(return_value=None)
    repo.get_album_by_mbid = AsyncMock(return_value=None)
    repo.get_artists = AsyncMock(return_value=[])
    repo.get_recently_played = AsyncMock(return_value=[])
    repo.get_recently_added = AsyncMock(return_value=[])
    repo.get_favorite_albums = AsyncMock(return_value=[])
    repo.get_favorite_artists = AsyncMock(return_value=[])
    repo.get_most_played_artists = AsyncMock(return_value=[])
    repo.get_most_played_albums = AsyncMock(return_value=[])
    repo.get_genres = AsyncMock(return_value=[])
    repo.get_library_stats = AsyncMock(return_value={"Albums": 0, "Artists": 0, "Songs": 0})
    repo.search_items = AsyncMock(return_value=[])
    repo.get_image_url = MagicMock(return_value=None)
    repo.get_playlists = AsyncMock(return_value=[])

    prefs = MagicMock()
    prefs.get_advanced_settings.return_value = MagicMock()

    service = JellyfinLibraryService(
        jellyfin_repo=repo,
        preferences_service=prefs,
    )
    return service, repo


def _item(
    id: str = "jf-1",
    name: str = "Album",
    type: str = "MusicAlbum",
    artist_name: str = "Artist",
    play_count: int = 0,
    year: int | None = 2024,
    child_count: int | None = 10,
    album_count: int | None = None,
    provider_ids: dict[str, str] | None = None,
    image_tag: str | None = None,
) -> JellyfinItem:
    return JellyfinItem(
        id=id,
        name=name,
        type=type,
        artist_name=artist_name,
        play_count=play_count,
        year=year,
        child_count=child_count,
        album_count=album_count,
        provider_ids=provider_ids,
        image_tag=image_tag,
    )


class TestGetRecentlyAdded:
    @pytest.mark.asyncio
    async def test_returns_album_summaries(self):
        service, repo = _make_service()
        repo.get_recently_added = AsyncMock(return_value=[_item(id="a1", name="New Album")])
        result = await service.get_recently_added(limit=10)
        assert len(result) == 1
        assert isinstance(result[0], JellyfinAlbumSummary)
        assert result[0].jellyfin_id == "a1"
        assert result[0].name == "New Album"

    @pytest.mark.asyncio
    async def test_returns_empty_list_when_none(self):
        service, repo = _make_service()
        repo.get_recently_added = AsyncMock(return_value=[])
        result = await service.get_recently_added()
        assert result == []


class TestGetMostPlayedArtists:
    @pytest.mark.asyncio
    async def test_returns_artist_summaries_with_play_count(self):
        service, repo = _make_service()
        repo.get_most_played_artists = AsyncMock(return_value=[
            _item(id="ar1", name="Top Artist", type="MusicArtist", album_count=5, play_count=42),
        ])
        result = await service.get_most_played_artists(limit=10)
        assert len(result) == 1
        assert isinstance(result[0], JellyfinArtistSummary)
        assert result[0].jellyfin_id == "ar1"
        assert result[0].name == "Top Artist"
        assert result[0].play_count == 42
        assert result[0].album_count == 5

    @pytest.mark.asyncio
    async def test_returns_empty_list_when_none(self):
        service, repo = _make_service()
        repo.get_most_played_artists = AsyncMock(return_value=[])
        result = await service.get_most_played_artists()
        assert result == []


class TestGetMostPlayedAlbums:
    @pytest.mark.asyncio
    async def test_returns_album_summaries_with_play_count(self):
        service, repo = _make_service()
        repo.get_most_played_albums = AsyncMock(return_value=[
            _item(id="a1", name="Hot Album", play_count=99),
        ])
        result = await service.get_most_played_albums(limit=10)
        assert len(result) == 1
        assert isinstance(result[0], JellyfinAlbumSummary)
        assert result[0].play_count == 99

    @pytest.mark.asyncio
    async def test_returns_empty_list_when_none(self):
        service, repo = _make_service()
        repo.get_most_played_albums = AsyncMock(return_value=[])
        result = await service.get_most_played_albums()
        assert result == []


class TestHubDataNewShelves:
    @pytest.mark.asyncio
    async def test_hub_includes_recently_added(self):
        service, repo = _make_service()
        repo.get_recently_added = AsyncMock(return_value=[_item(id="ra1", name="New")])
        hub = await service.get_hub_data()
        assert isinstance(hub, JellyfinHubResponse)
        assert len(hub.recently_added) == 1
        assert hub.recently_added[0].jellyfin_id == "ra1"

    @pytest.mark.asyncio
    async def test_hub_includes_most_played_artists(self):
        service, repo = _make_service()
        repo.get_most_played_artists = AsyncMock(return_value=[
            _item(id="ar1", name="Top", type="MusicArtist", album_count=3, play_count=10),
        ])
        hub = await service.get_hub_data()
        assert len(hub.most_played_artists) == 1
        assert hub.most_played_artists[0].play_count == 10

    @pytest.mark.asyncio
    async def test_hub_includes_most_played_albums(self):
        service, repo = _make_service()
        repo.get_most_played_albums = AsyncMock(return_value=[
            _item(id="a1", name="Hot", play_count=50),
        ])
        hub = await service.get_hub_data()
        assert len(hub.most_played_albums) == 1
        assert hub.most_played_albums[0].play_count == 50

    @pytest.mark.asyncio
    async def test_hub_graceful_on_recently_added_error(self):
        service, repo = _make_service()
        repo.get_recently_added = AsyncMock(side_effect=Exception("timeout"))
        hub = await service.get_hub_data()
        assert hub.recently_added == []

    @pytest.mark.asyncio
    async def test_hub_graceful_on_most_played_errors(self):
        service, repo = _make_service()
        repo.get_most_played_artists = AsyncMock(side_effect=Exception("fail"))
        repo.get_most_played_albums = AsyncMock(side_effect=Exception("fail"))
        hub = await service.get_hub_data()
        assert hub.most_played_artists == []
        assert hub.most_played_albums == []


class TestGetFavoritesExpanded:
    @pytest.mark.asyncio
    async def test_returns_albums_and_artists(self):
        service, repo = _make_service()
        repo.get_favorite_albums = AsyncMock(return_value=[
            _item(id="a1", name="Fav Album", type="MusicAlbum"),
        ])
        repo.get_favorite_artists = AsyncMock(return_value=[
            _item(id="ar1", name="Fav Artist", type="MusicArtist", album_count=3),
        ])
        result = await service.get_favorites_expanded(limit=50)
        assert isinstance(result, JellyfinFavoritesExpanded)
        assert len(result.albums) == 1
        assert result.albums[0].jellyfin_id == "a1"
        assert len(result.artists) == 1
        assert result.artists[0].jellyfin_id == "ar1"
        assert result.artists[0].name == "Fav Artist"
        assert result.artists[0].album_count == 3

    @pytest.mark.asyncio
    async def test_returns_empty_when_no_favorites(self):
        service, repo = _make_service()
        repo.get_favorite_albums = AsyncMock(return_value=[])
        repo.get_favorite_artists = AsyncMock(return_value=[])
        result = await service.get_favorites_expanded(limit=50)
        assert result.albums == []
        assert result.artists == []

    @pytest.mark.asyncio
    async def test_artist_summary_has_play_count(self):
        service, repo = _make_service()
        repo.get_favorite_artists = AsyncMock(return_value=[
            _item(id="ar2", name="Played Artist", type="MusicArtist", play_count=77, album_count=2),
        ])
        result = await service.get_favorites_expanded(limit=10)
        assert len(result.artists) == 1
        assert result.artists[0].play_count == 77


class TestImportPlaylistMapsAlbumMbids:
    """import_playlist stores the MusicBrainz MBID as album_id, not the
    Jellyfin album GUID, so the MBID-keyed local catalog can match (#150)."""

    @staticmethod
    def _requesting():
        return SimpleNamespace(id="u1")

    @staticmethod
    def _playlist_service():
        ps = MagicMock()
        ps.get_by_source_ref = AsyncMock(return_value=None)
        ps.create_playlist = AsyncMock(return_value=SimpleNamespace(id="dn-pl-1"))
        ps.add_tracks = AsyncMock()
        return ps

    @staticmethod
    def _track_item(id: str, album_id: str, track_number: int = 1) -> JellyfinItem:
        return JellyfinItem(
            id=id,
            name=f"Song {id}",
            type="Audio",
            artist_name="Artist",
            album_name="Album",
            album_id=album_id,
            artist_id="artist-1",
            index_number=track_number,
            parent_index_number=1,
            duration_ticks=200 * 10_000_000,
        )

    def _wire_playlist(self, repo, track_items):
        repo.get_playlists = AsyncMock(
            return_value=[_item(id="pl-1", name="Mix", type="Playlist")]
        )
        repo.get_playlist = AsyncMock(
            return_value=_item(id="pl-1", name="Mix", type="Playlist")
        )
        repo.get_playlist_items = AsyncMock(return_value=track_items)

    @pytest.mark.asyncio
    async def test_stores_release_group_mbid_as_album_id(self):
        service, repo = _make_service()
        self._wire_playlist(repo, [self._track_item("t1", "guid-a")])
        repo.get_album_detail = AsyncMock(
            return_value=_item(
                id="guid-a",
                provider_ids={
                    "MusicBrainzReleaseGroup": "rg-123",
                    "MusicBrainzAlbum": "rel-456",
                },
            )
        )
        ps = self._playlist_service()

        await service.import_playlist("pl-1", ps, self._requesting())

        track_dicts = ps.add_tracks.call_args[0][2]
        assert track_dicts[0]["album_id"] == "rg-123"
        assert track_dicts[0]["track_source_id"] == "t1"
        assert track_dicts[0]["source_type"] == "jellyfin"

    @pytest.mark.asyncio
    async def test_falls_back_to_musicbrainz_album_provider_id(self):
        service, repo = _make_service()
        self._wire_playlist(repo, [self._track_item("t1", "guid-a")])
        repo.get_album_detail = AsyncMock(
            return_value=_item(id="guid-a", provider_ids={"MusicBrainzAlbum": "rel-456"})
        )
        ps = self._playlist_service()

        await service.import_playlist("pl-1", ps, self._requesting())

        track_dicts = ps.add_tracks.call_args[0][2]
        assert track_dicts[0]["album_id"] == "rel-456"

    @pytest.mark.asyncio
    async def test_stores_none_when_album_detail_missing(self):
        service, repo = _make_service()
        self._wire_playlist(repo, [self._track_item("t1", "guid-a")])
        repo.get_album_detail = AsyncMock(return_value=None)
        ps = self._playlist_service()

        await service.import_playlist("pl-1", ps, self._requesting())

        track_dicts = ps.add_tracks.call_args[0][2]
        assert track_dicts[0]["album_id"] is None

    @pytest.mark.asyncio
    async def test_stores_none_when_no_provider_ids(self):
        service, repo = _make_service()
        self._wire_playlist(repo, [self._track_item("t1", "guid-a")])
        repo.get_album_detail = AsyncMock(return_value=_item(id="guid-a"))
        ps = self._playlist_service()

        await service.import_playlist("pl-1", ps, self._requesting())

        track_dicts = ps.add_tracks.call_args[0][2]
        assert track_dicts[0]["album_id"] is None

    @pytest.mark.asyncio
    async def test_fetches_each_distinct_album_once(self):
        service, repo = _make_service()
        self._wire_playlist(
            repo,
            [
                self._track_item("t1", "guid-a", track_number=1),
                self._track_item("t2", "guid-a", track_number=2),
                self._track_item("t3", "guid-b", track_number=1),
            ],
        )
        repo.get_album_detail = AsyncMock(
            side_effect=lambda guid: _item(
                id=guid, provider_ids={"MusicBrainzReleaseGroup": f"rg-{guid}"}
            )
        )
        ps = self._playlist_service()

        await service.import_playlist("pl-1", ps, self._requesting())

        assert repo.get_album_detail.await_count == 2
        awaited = {c.args[0] for c in repo.get_album_detail.await_args_list}
        assert awaited == {"guid-a", "guid-b"}
        track_dicts = ps.add_tracks.call_args[0][2]
        assert [t["album_id"] for t in track_dicts] == [
            "rg-guid-a",
            "rg-guid-a",
            "rg-guid-b",
        ]

    @pytest.mark.asyncio
    async def test_already_imported_skips_album_fetches(self):
        service, repo = _make_service()
        ps = self._playlist_service()
        ps.get_by_source_ref = AsyncMock(return_value=SimpleNamespace(id="dn-pl-1"))

        result = await service.import_playlist("pl-1", ps, self._requesting())

        assert result.already_imported is True
        repo.get_album_detail.assert_not_called()
