from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.navidrome import NavidromeArtistSummary, NavidromeTrackInfo
from repositories.navidrome_models import (
    SubsonicAlbum,
    SubsonicArtist,
    SubsonicGenre,
    SubsonicPlaylist,
    SubsonicSearchResult,
    SubsonicSong,
)
from services.navidrome_library_service import NavidromeLibraryService, _normalize, _clean_album_name


def _make_service() -> tuple[NavidromeLibraryService, MagicMock]:
    repo = MagicMock()
    repo.get_album_list = AsyncMock(return_value=[])
    repo.get_album = AsyncMock()
    repo.get_artists = AsyncMock(return_value=[])
    repo.get_artist = AsyncMock()
    repo.get_starred = AsyncMock(return_value=SubsonicSearchResult())
    repo.get_genres = AsyncMock(return_value=[])
    repo.search = AsyncMock(return_value=SubsonicSearchResult())
    repo.search_songs = AsyncMock(return_value=[])
    repo.get_playlists = AsyncMock(return_value=[])
    repo.get_playlist = AsyncMock()
    prefs = MagicMock()
    prefs.get_advanced_settings.return_value = MagicMock()
    service = NavidromeLibraryService(navidrome_repo=repo, preferences_service=prefs)
    return service, repo


def _album(id: str = "al1", name: str = "Album", artist: str = "Artist",
           year: int = 2020, song_count: int = 10, mbid: str = "") -> SubsonicAlbum:
    return SubsonicAlbum(
        id=id, name=name, artist=artist, year=year,
        songCount=song_count, musicBrainzId=mbid,
    )


def _artist(id: str = "ar1", name: str = "Artist", album_count: int = 3,
            mbid: str = "") -> SubsonicArtist:
    return SubsonicArtist(
        id=id, name=name, albumCount=album_count, musicBrainzId=mbid,
    )


def _song(id: str = "s1", title: str = "Song", album: str = "Album",
          artist: str = "Artist", track: int = 1, duration: int = 200,
          suffix: str = "mp3", bit_rate: int = 320) -> SubsonicSong:
    return SubsonicSong(
        id=id, title=title, album=album, artist=artist,
        track=track, duration=duration, suffix=suffix, bitRate=bit_rate,
    )


class TestGetAlbums:
    @pytest.mark.asyncio
    async def test_returns_mapped_summaries(self):
        service, repo = _make_service()
        repo.get_album_list = AsyncMock(return_value=[_album(id="a1", name="OK Computer")])
        result = await service.get_albums()
        assert len(result) == 1
        assert result[0].navidrome_id == "a1"
        assert result[0].name == "OK Computer"

    @pytest.mark.asyncio
    async def test_empty_list(self):
        service, repo = _make_service()
        result = await service.get_albums()
        assert result == []


class TestGetAlbumDetail:
    @pytest.mark.asyncio
    async def test_maps_tracks(self):
        service, repo = _make_service()
        album = _album(id="a1", name="The Wall")
        album = SubsonicAlbum(
            id="a1", name="The Wall", artist="Pink Floyd",
            year=1979, songCount=2, musicBrainzId="mb-a1",
            song=[_song(id="s1", title="Comfortably Numb", track=1),
                  _song(id="s2", title="Another Brick", track=2)],
        )
        repo.get_album = AsyncMock(return_value=album)
        result = await service.get_album_detail("a1")
        assert result is not None
        assert result.navidrome_id == "a1"
        assert result.track_count == 2
        assert result.tracks[0].title == "Comfortably Numb"
        assert result.musicbrainz_id is None

    @pytest.mark.asyncio
    async def test_returns_none_on_exception(self):
        service, repo = _make_service()
        repo.get_album = AsyncMock(side_effect=RuntimeError("fail"))
        result = await service.get_album_detail("a1")
        assert result is None

    @pytest.mark.asyncio
    async def test_fix_missing_track_numbers(self):
        service, repo = _make_service()
        album = SubsonicAlbum(
            id="a1", name="Album",
            song=[_song(id="s1", title="A", track=0), _song(id="s2", title="B", track=0)],
        )
        repo.get_album = AsyncMock(return_value=album)
        result = await service.get_album_detail("a1")
        assert result is not None
        assert result.tracks[0].track_number == 1
        assert result.tracks[1].track_number == 2

    @pytest.mark.asyncio
    async def test_selected_scope_rejects_album_outside_folder(self):
        service, repo = _make_service()
        repo.get_album.return_value = _album(id="outside", name="Hidden")
        repo.search.return_value = SubsonicSearchResult(album=[])

        result = await service.get_album_detail("outside", ("folder-a",))

        assert result is None
        repo.search.assert_awaited_once_with(
            "Hidden",
            artist_count=0,
            album_count=500,
            song_count=0,
            music_folder_ids=("folder-a",),
        )


class TestGetArtists:
    @pytest.mark.asyncio
    async def test_maps_artist_summaries(self):
        service, repo = _make_service()
        repo.get_artists = AsyncMock(return_value=[_artist(id="ar1", name="Radiohead", mbid="mb-ar1")])
        result = await service.get_artists()
        assert len(result) == 1
        assert result[0].navidrome_id == "ar1"
        assert result[0].name == "Radiohead"
        assert result[0].musicbrainz_id == "mb-ar1"


class TestGetArtistDetail:
    @pytest.mark.asyncio
    async def test_returns_artist_and_albums(self):
        service, repo = _make_service()
        repo.get_artist = AsyncMock(return_value=_artist(id="ar1", name="Muse"))
        search_result = SubsonicSearchResult(
            album=[SubsonicAlbum(id="al1", name="Absolution", artistId="ar1")],
        )
        repo.search = AsyncMock(return_value=search_result)
        repo.get_album = AsyncMock(return_value=_album(id="al1", name="Absolution"))
        result = await service.get_artist_detail("ar1")
        assert result is not None
        assert result["artist"].name == "Muse"
        assert len(result["albums"]) == 1

    @pytest.mark.asyncio
    async def test_returns_none_on_exception(self):
        service, repo = _make_service()
        repo.get_artist = AsyncMock(side_effect=RuntimeError("fail"))
        result = await service.get_artist_detail("ar1")
        assert result is None

    @pytest.mark.asyncio
    async def test_selected_scope_rejects_artist_outside_folder(self):
        service, repo = _make_service()
        repo.get_artists.return_value = [_artist(id="visible")]

        result = await service.get_artist_detail("outside", ("folder-a",))

        assert result is None
        repo.get_artist.assert_not_awaited()


class TestSearch:
    @pytest.mark.asyncio
    async def test_maps_search_results(self):
        service, repo = _make_service()
        repo.search = AsyncMock(return_value=SubsonicSearchResult(
            artist=[_artist(id="ar1", name="Beatles")],
            album=[_album(id="al1", name="Abbey Road")],
            song=[_song(id="s1", title="Come Together")],
        ))
        result = await service.search("beatles")
        assert len(result.artists) == 1
        assert result.artists[0].name == "Beatles"
        assert len(result.albums) == 1
        assert result.albums[0].name == "Abbey Road"
        assert len(result.tracks) == 1
        assert result.tracks[0].title == "Come Together"


class TestGetRecent:
    @pytest.mark.asyncio
    async def test_delegates_to_repo(self):
        service, repo = _make_service()
        repo.get_album_list = AsyncMock(return_value=[_album(id="al1")])
        result = await service.get_recent(limit=5)
        assert len(result) == 1
        repo.get_album_list.assert_awaited_once_with(
            type="recent", size=5, offset=0, music_folder_ids=None
        )


class TestGetFavorites:
    @pytest.mark.asyncio
    async def test_maps_starred(self):
        service, repo = _make_service()
        repo.get_starred = AsyncMock(return_value=SubsonicSearchResult(
            artist=[_artist()], album=[_album()], song=[_song()],
        ))
        result = await service.get_favorites()
        assert len(result.artists) == 1
        assert len(result.albums) == 1
        assert len(result.tracks) == 1


class TestGetGenres:
    @pytest.mark.asyncio
    async def test_returns_genre_names(self):
        service, repo = _make_service()
        repo.get_genres = AsyncMock(return_value=[
            SubsonicGenre(name="Rock", songCount=100),
            SubsonicGenre(name="Jazz"),
            SubsonicGenre(name=""),
        ])
        result = await service.get_genres()
        assert result == ["Rock", "Jazz"]

    @pytest.mark.asyncio
    async def test_selected_scope_derives_only_scoped_album_genres(self):
        service, repo = _make_service()
        repo.get_album_list.return_value = [
            SubsonicAlbum(id="a", name="A", genre="Ambient"),
            SubsonicAlbum(id="b", name="B", genre="Rock"),
        ]

        result = await service.get_genres(("folder-a",))

        assert result == ["Ambient", "Rock"]
        repo.get_genres.assert_not_awaited()


class TestScopedPlaylists:
    @pytest.mark.asyncio
    async def test_playlist_spanning_folders_is_filtered_for_summary_and_detail(self):
        service, repo = _make_service()
        inside = _song(id="inside", duration=10)
        outside = _song(id="outside", duration=20)
        repo.search_songs.return_value = [inside]
        repo.get_playlists.return_value = [
            SubsonicPlaylist(id="p1", name="Mixed", songCount=2, duration=30)
        ]
        repo.get_playlist.return_value = SubsonicPlaylist(
            id="p1",
            name="Mixed",
            songCount=2,
            duration=30,
            entry=[inside, outside],
        )

        summaries = await service.list_playlists(
            music_folder_ids=("folder-a",)
        )
        detail = await service.get_playlist_detail("p1", ("folder-a",))

        assert summaries[0].track_count == 1
        assert summaries[0].duration_seconds == 10
        assert [track.id for track in detail.tracks] == ["inside"]
        assert detail.track_count == 1
        assert detail.duration_seconds == 10


class TestGetStats:
    @pytest.mark.asyncio
    async def test_aggregates_counts(self):
        service, repo = _make_service()
        repo.get_artists = AsyncMock(return_value=[_artist(), _artist(id="ar2")])
        repo.get_album_list = AsyncMock(return_value=[
            _album(id="al1", song_count=50),
            _album(id="al2", song_count=30),
        ])
        result = await service.get_stats()
        assert result.total_artists == 2
        assert result.total_albums == 2
        assert result.total_tracks == 80


class TestAlbumMatch:
    @pytest.mark.asyncio
    async def test_scoped_reverse_index_match_scans_beyond_first_album_page(self):
        service, repo = _make_service()
        service._mbid_to_navidrome_id = {"mb-target": "nd-target"}
        first_page = [_album(id=f"other-{index}") for index in range(500)]
        repo.get_album_list = AsyncMock(
            side_effect=[first_page, [_album(id="nd-target", mbid="mb-target")]]
        )
        repo.get_album = AsyncMock(
            return_value=SubsonicAlbum(
                id="nd-target",
                name="Album",
                musicBrainzId="mb-target",
                song=[_song(id="song-target")],
            )
        )

        result = await service.get_album_match(
            "mb-target", "", "", music_folder_ids=("folder-a",)
        )

        assert result.found is True
        assert result.navidrome_album_id == "nd-target"
        assert repo.get_album_list.await_count == 2
        repo.search.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_mbid_match(self):
        service, repo = _make_service()
        candidate = SubsonicAlbum(id="nd-1", name="Album", musicBrainzId="mb-target")
        repo.search = AsyncMock(return_value=SubsonicSearchResult(album=[candidate]))
        repo.get_album = AsyncMock(return_value=SubsonicAlbum(
            id="nd-1", name="Album", musicBrainzId="mb-target",
            song=[_song(id="s1", title="Track 1")],
        ))
        result = await service.get_album_match("mb-target", "Album", "Artist")
        assert result.found is True
        assert result.navidrome_album_id == "nd-1"
        assert len(result.tracks) == 1

    @pytest.mark.asyncio
    async def test_fuzzy_name_match(self):
        service, repo = _make_service()
        candidate = SubsonicAlbum(id="nd-2", name="OK Computer", artist="Radiohead")
        repo.search = AsyncMock(return_value=SubsonicSearchResult(album=[candidate]))
        repo.get_album = AsyncMock(return_value=SubsonicAlbum(
            id="nd-2", name="OK Computer", artist="Radiohead",
            song=[_song()],
        ))
        result = await service.get_album_match("", "OK Computer", "Radiohead")
        assert result.found is True
        assert result.navidrome_album_id == "nd-2"

    @pytest.mark.asyncio
    async def test_no_match(self):
        service, repo = _make_service()
        repo.search = AsyncMock(return_value=SubsonicSearchResult(album=[]))
        result = await service.get_album_match("mb-none", "Nonexistent", "Nobody")
        assert result.found is False
        assert result.navidrome_album_id is None


class TestNormalize:
    def test_strips_accents(self):
        assert _normalize("Café") == "cafe"

    def test_lowercases(self):
        assert _normalize("HELLO") == "hello"

    def test_strips_non_alphanumeric(self):
        assert _normalize("OK Computer!") == "okcomputer"


class TestLibraryAlbumMatching:
    @pytest.mark.asyncio
    async def test_exact_match(self):
        service, _ = _make_service()
        service._library_album_index = {
            f"{_normalize('Buzz')}:{_normalize('NIKI')}": ("mbid-buzz", "mbid-niki"),
        }
        result = await service._resolve_album_mbid("Buzz", "NIKI")
        assert result == "mbid-buzz"

    @pytest.mark.asyncio
    async def test_cleaned_name_match(self):
        service, _ = _make_service()
        service._library_album_index = {
            f"{_normalize('OK Computer')}:{_normalize('Radiohead')}": ("mbid-okc", "mbid-rh"),
        }
        result = await service._resolve_album_mbid("OK Computer (Remastered 2017)", "Radiohead")
        assert result == "mbid-okc"

    @pytest.mark.asyncio
    async def test_no_match_returns_none(self):
        service, _ = _make_service()
        service._library_album_index = {}
        result = await service._resolve_album_mbid("Nonexistent", "Nobody")
        assert result is None

    @pytest.mark.asyncio
    async def test_negative_cache_prevents_re_lookup(self):
        service, _ = _make_service()
        service._library_album_index = {}
        result1 = await service._resolve_album_mbid("Missing", "Artist")
        assert result1 is None
        service._library_album_index = {
            f"{_normalize('Missing')}:{_normalize('Artist')}": ("mbid-late", "mbid-a"),
        }
        result2 = await service._resolve_album_mbid("Missing", "Artist")
        assert result2 is None

    @pytest.mark.asyncio
    async def test_empty_name_returns_none(self):
        service, _ = _make_service()
        result = await service._resolve_album_mbid("", "Artist")
        assert result is None


class TestLibraryArtistMatching:
    @pytest.mark.asyncio
    async def test_exact_match(self):
        service, _ = _make_service()
        service._library_artist_index = {
            _normalize("Radiohead"): "mbid-radiohead",
        }
        result = await service._resolve_artist_mbid("Radiohead")
        assert result == "mbid-radiohead"

    @pytest.mark.asyncio
    async def test_no_match_returns_none(self):
        service, _ = _make_service()
        service._library_artist_index = {}
        result = await service._resolve_artist_mbid("Unknown Artist")
        assert result is None

    @pytest.mark.asyncio
    async def test_empty_name_returns_none(self):
        service, _ = _make_service()
        result = await service._resolve_artist_mbid("")
        assert result is None


def _make_service_with_cache() -> tuple[NavidromeLibraryService, MagicMock, MagicMock]:
    repo = MagicMock()
    repo.get_album_list = AsyncMock(return_value=[])
    repo.get_album = AsyncMock()
    repo.get_artists = AsyncMock(return_value=[])
    repo.get_artist = AsyncMock()
    repo.get_starred = AsyncMock(return_value=SubsonicSearchResult())
    repo.get_genres = AsyncMock(return_value=[])
    repo.search = AsyncMock(return_value=SubsonicSearchResult())
    prefs = MagicMock()
    prefs.get_advanced_settings.return_value = MagicMock()
    cache = MagicMock()
    cache.get_all_albums_for_matching = AsyncMock(return_value=[])
    cache.load_navidrome_album_mbid_index = AsyncMock(return_value={})
    cache.load_navidrome_artist_mbid_index = AsyncMock(return_value={})
    cache.save_navidrome_album_mbid_index = AsyncMock()
    cache.save_navidrome_artist_mbid_index = AsyncMock()
    service = NavidromeLibraryService(navidrome_repo=repo, preferences_service=prefs, library_db=cache, mbid_store=cache)
    return service, repo, cache


class TestWarmMbidCacheLifecycle:
    @pytest.mark.asyncio
    async def test_builds_library_index_resolves_albums_and_persists(self):
        service, repo, cache = _make_service_with_cache()
        cache.get_all_albums_for_matching = AsyncMock(return_value=[
            ("OK Computer", "Radiohead", "mbid-okc", "mbid-rh"),
        ])
        repo.get_album_list = AsyncMock(return_value=[
            _album(id="a1", name="OK Computer", artist="Radiohead"),
        ])
        await service.warm_mbid_cache()
        key = f"{_normalize('OK Computer')}:{_normalize('Radiohead')}"
        assert service._album_mbid_cache[key] == "mbid-okc"
        assert service._artist_mbid_cache[_normalize("Radiohead")] == "mbid-rh"
        cache.save_navidrome_album_mbid_index.assert_awaited_once()
        cache.save_navidrome_artist_mbid_index.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_negative_cache_overridden_when_library_match_exists(self):
        service, repo, cache = _make_service_with_cache()
        key = f"{_normalize('Buzz')}:{_normalize('NIKI')}"
        service._album_mbid_cache[key] = (None, 0.0)
        cache.get_all_albums_for_matching = AsyncMock(return_value=[
            ("Buzz", "NIKI", "mbid-buzz", "mbid-niki"),
        ])
        repo.get_album_list = AsyncMock(return_value=[
            _album(id="a1", name="Buzz", artist="NIKI"),
        ])
        await service.warm_mbid_cache()
        assert service._album_mbid_cache[key] == "mbid-buzz"

    @pytest.mark.asyncio
    async def test_persist_if_dirty_round_trip(self):
        service, repo, cache = _make_service_with_cache()
        service._library_album_index = {
            f"{_normalize('Album')}:{_normalize('Artist')}": ("mbid-a", "mbid-ar"),
        }
        service._library_artist_index = {_normalize("Artist"): "mbid-ar"}
        await service._resolve_album_mbid("Album", "Artist")
        await service._resolve_artist_mbid("Artist")
        assert service._dirty is True
        await service.persist_if_dirty()
        assert service._dirty is False
        saved_albums = cache.save_navidrome_album_mbid_index.call_args[0][0]
        saved_artists = cache.save_navidrome_artist_mbid_index.call_args[0][0]
        assert saved_albums[f"{_normalize('Album')}:{_normalize('Artist')}"] == "mbid-a"
        assert saved_artists[_normalize("Artist")] == "mbid-ar"

    @pytest.mark.asyncio
    async def test_disk_cache_loaded_when_library_unavailable(self):
        service, repo, cache = _make_service_with_cache()
        cache.get_all_albums_for_matching = AsyncMock(return_value=[])
        key = f"{_normalize('Album')}:{_normalize('Artist')}"
        cache.load_navidrome_album_mbid_index = AsyncMock(return_value={key: "mbid-disk"})
        cache.load_navidrome_artist_mbid_index = AsyncMock(return_value={_normalize("Artist"): "mbid-ar-disk"})
        repo.get_album_list = AsyncMock(return_value=[_album(id="nd-1", name="Album", artist="Artist")])
        await service.warm_mbid_cache()
        assert service._album_mbid_cache[key] == "mbid-disk"
        assert service._artist_mbid_cache[_normalize("Artist")] == "mbid-ar-disk"
        assert service._mbid_to_navidrome_id.get("mbid-disk") == "nd-1"


class TestHubFavoritesExpansion:
    @pytest.mark.asyncio
    async def test_hub_includes_favorite_artists_and_tracks(self):
        service, repo = _make_service()
        starred = SubsonicSearchResult(
            album=[_album(id="a1", name="Fav Album")],
            artist=[_artist(id="ar1", name="Fav Artist")],
            song=[_song(id="s1", title="Fav Song")],
        )
        repo.get_starred = AsyncMock(return_value=starred)
        repo.get_album_list = AsyncMock(return_value=[])
        repo.get_artists = AsyncMock(return_value=[])
        repo.get_genres = AsyncMock(return_value=[])
        hub = await service.get_hub_data()
        assert len(hub.favorites) == 1
        assert len(hub.favorite_artists) == 1
        assert len(hub.favorite_tracks) == 1
        assert isinstance(hub.favorite_artists[0], NavidromeArtistSummary)
        assert isinstance(hub.favorite_tracks[0], NavidromeTrackInfo)
        assert hub.favorite_artists[0].name == "Fav Artist"
        assert hub.favorite_tracks[0].title == "Fav Song"

    @pytest.mark.asyncio
    async def test_hub_favorites_empty_when_no_starred(self):
        service, repo = _make_service()
        repo.get_starred = AsyncMock(return_value=SubsonicSearchResult())
        repo.get_album_list = AsyncMock(return_value=[])
        repo.get_artists = AsyncMock(return_value=[])
        repo.get_genres = AsyncMock(return_value=[])
        hub = await service.get_hub_data()
        assert hub.favorites == []
        assert hub.favorite_artists == []
        assert hub.favorite_tracks == []

    @pytest.mark.asyncio
    async def test_hub_favorites_graceful_on_error(self):
        service, repo = _make_service()
        repo.get_starred = AsyncMock(side_effect=Exception("network error"))
        repo.get_album_list = AsyncMock(return_value=[])
        repo.get_artists = AsyncMock(return_value=[])
        repo.get_genres = AsyncMock(return_value=[])
        hub = await service.get_hub_data()
        assert hub.favorites == []
        assert hub.favorite_artists == []
        assert hub.favorite_tracks == []


class TestGetStatsStableAcrossRefreshes:
    @pytest.mark.asyncio
    async def test_repeated_calls_return_stable_counts(self):
        # Regression test for issue #371: with 500+ albums, get_stats() grew
        # the cached first page in place, so every refresh reported more
        # albums/tracks. The stub below returns the SAME list objects on
        # every call, exactly like the real cache-backed repository.
        service, repo = _make_service()
        page0 = [_album(id=f"a-{i}", song_count=10) for i in range(500)]
        page1 = [_album(id=f"b-{i}", song_count=10) for i in range(100)]

        async def _album_list(type="alphabeticalByName", size=20, offset=0, **kwargs):
            if size == 1 and offset == 0:
                return [page0[0]]
            if offset == 0:
                return page0
            if offset == 500:
                return page1
            return []

        repo.get_album_list = AsyncMock(side_effect=_album_list)
        repo.get_artists = AsyncMock(
            return_value=[_artist(id=f"ar-{i}") for i in range(5)]
        )
        first = await service.get_stats()
        second = await service.get_stats()
        assert (first.total_albums, first.total_tracks, first.total_artists) == (600, 6000, 5)
        assert (second.total_albums, second.total_tracks, second.total_artists) == (600, 6000, 5)
        assert len(page0) == 500
