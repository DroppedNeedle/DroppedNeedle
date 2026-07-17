from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch
from uuid import UUID

import pytest

from api.v1.schemas.discover import (
    DiscoverQueueItemLight,
    DiscoverResponse,
    TopPickItem,
    TopPicksSection,
)
from api.v1.schemas.home import HomeSection, HomeAlbum
from api.v1.schemas.settings import (
    ListenBrainzConnectionSettings,
    LastFmConnectionSettings,
    PrimaryMusicSourceSettings,
)
from services.discover.homepage_service import (
    DiscoverHomepageService,
    DAILY_MIX_CACHE_TTL,
)
from services.discover.integration_helpers import IntegrationHelpers


def _make_lb_settings(
    enabled: bool = True, username: str = "lbuser"
) -> ListenBrainzConnectionSettings:
    return ListenBrainzConnectionSettings(
        user_token="tok", username=username, enabled=enabled,
    )


def _make_lfm_settings(
    enabled: bool = True, username: str = "lfmuser"
) -> LastFmConnectionSettings:
    return LastFmConnectionSettings(
        api_key="key", shared_secret="secret", session_key="sk",
        username=username, enabled=enabled,
    )


def _make_prefs(
    lb_enabled: bool = True,
    lfm_enabled: bool = False,
    primary_source: str = "listenbrainz",
) -> MagicMock:
    prefs = MagicMock()
    prefs.get_listenbrainz_connection.return_value = _make_lb_settings(enabled=lb_enabled)
    prefs.get_lastfm_connection.return_value = _make_lfm_settings(enabled=lfm_enabled)
    prefs.is_lastfm_enabled.return_value = lfm_enabled
    prefs.get_primary_music_source.return_value = PrimaryMusicSourceSettings(source=primary_source)

    jf_settings = MagicMock()
    jf_settings.enabled = False
    jf_settings.jellyfin_url = ""
    jf_settings.api_key = ""
    prefs.get_jellyfin_connection.return_value = jf_settings

    download_client = MagicMock()
    download_client.enabled = False
    download_client.url = ""
    prefs.get_download_client_settings.return_value = download_client

    yt = MagicMock()
    yt.enabled = False
    yt.api_key = ""
    prefs.get_youtube_connection.return_value = yt

    lf = MagicMock()
    lf.enabled = False
    lf.music_path = ""
    prefs.get_local_files_connection.return_value = lf

    adv = MagicMock()
    adv.discover_queue_size = 10
    adv.discover_queue_ttl = 3600
    adv.discover_queue_seed_artists = 3
    adv.discover_queue_wildcard_slots = 2
    adv.discover_queue_similar_artists_limit = 15
    adv.discover_queue_albums_per_similar = 3
    adv.discover_queue_enrich_ttl = 3600
    adv.discover_queue_lastfm_mbid_max_lookups = 10
    prefs.get_advanced_settings.return_value = adv

    return prefs


def _make_genre_index(
    top_genres: list[tuple[str, int]] | None = None,
    artists_by_genre: dict[str, list[str]] | None = None,
    albums_by_genre: list[dict] | None = None,
) -> AsyncMock:
    genre_index = AsyncMock()
    genre_index.get_top_genres = AsyncMock(return_value=top_genres or [])
    genre_index.get_artists_for_genres = AsyncMock(return_value=artists_by_genre or {})
    genre_index.get_albums_by_genre = AsyncMock(return_value=albums_by_genre or [])
    return genre_index


def _make_cache() -> AsyncMock:
    cache = AsyncMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    return cache


def _make_pool_items(count: int, prefix: str = "album") -> list[DiscoverQueueItemLight]:
    return [
        DiscoverQueueItemLight(
            release_group_mbid=f"rg-{prefix}-{i}",
            album_name=f"{prefix.title()} {i}",
            artist_name=f"Artist {i}",
            artist_mbid=f"artist-{prefix}-{i}",
            recommendation_reason=f"Similar to seed",
        )
        for i in range(count)
    ]


def _make_service(
    genre_index: AsyncMock | None = None,
    cache: AsyncMock | None = None,
) -> DiscoverHomepageService:
    lb_repo = AsyncMock()
    jf_repo = AsyncMock()
    library_repo = AsyncMock()
    mb_repo = AsyncMock()
    prefs = _make_prefs()
    integration = IntegrationHelpers(prefs)
    mbid_resolution = MagicMock()

    return DiscoverHomepageService(
        listenbrainz_repo=lb_repo,
        jellyfin_repo=jf_repo,
        library_repo=library_repo,
        musicbrainz_repo=mb_repo,
        integration=integration,
        mbid_resolution=mbid_resolution,
        memory_cache=cache,
        genre_index=genre_index,
    )


def _make_top_genres(count: int) -> list[tuple[str, int]]:
    genres = ["rock", "pop", "jazz", "electronic", "metal", "hip hop", "classical", "blues"]
    return [(genres[i % len(genres)], 10 - i) for i in range(count)]


def _make_artists_by_genre(genres: list[str], artists_per: int = 5) -> dict[str, list[str]]:
    result: dict[str, list[str]] = {}
    counter = 0
    for genre in genres:
        result[genre] = [str(UUID(int=counter + j + 1)) for j in range(artists_per)]
        counter += artists_per
    return result


class TestDailyMixReturnsEmptyWhenGenreIndexIsNone:
    @pytest.mark.asyncio
    async def test_returns_empty_when_genre_index_is_none(self) -> None:
        service = _make_service(genre_index=None)
        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert result == []


class TestDailyMixReturnsEmptyWhenNoGenres:
    @pytest.mark.asyncio
    async def test_returns_empty_when_no_genres(self) -> None:
        genre_index = _make_genre_index(top_genres=[])
        cache = _make_cache()
        service = _make_service(genre_index=genre_index, cache=cache)
        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert result == []


class TestDailyMixReturnsEmptyWhenAllGenresBelowMinArtists:
    @pytest.mark.asyncio
    async def test_returns_empty_when_all_genres_below_min_artists(self) -> None:
        top_genres = [("rock", 5), ("pop", 3)]
        # 2 artists per genre, under the 3-artist minimum
        artists_by_genre = {
            "rock": [str(UUID(int=i)) for i in range(1, 3)],
            "pop": [str(UUID(int=i)) for i in range(3, 5)],
        }
        genre_index = _make_genre_index(
            top_genres=top_genres, artists_by_genre=artists_by_genre,
        )
        cache = _make_cache()
        service = _make_service(genre_index=genre_index, cache=cache)
        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert result == []


class TestDailyMixCorrectCountWith5QualifyingGenres:
    @pytest.mark.asyncio
    @patch("services.discover.homepage_service.build_similar_artist_pools")
    async def test_returns_correct_count_with_5_qualifying_genres(
        self, mock_pools: AsyncMock,
    ) -> None:
        top_genres = _make_top_genres(5)
        genre_names = [g for g, _ in top_genres]
        artists_by_genre = _make_artists_by_genre(genre_names, artists_per=5)
        albums = [
            {"title": f"Lib Album {i}", "mbid": f"lib-mbid-{i}", "artist_name": f"Artist {i}"}
            for i in range(5)
        ]
        genre_index = _make_genre_index(
            top_genres=top_genres,
            artists_by_genre=artists_by_genre,
            albums_by_genre=albums,
        )
        cache = _make_cache()
        mock_pools.return_value = [_make_pool_items(3, f"pool-{i}") for i in range(3)]

        service = _make_service(genre_index=genre_index, cache=cache)
        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert len(result) == 5


class TestDailyMixCorrectCountWith2QualifyingGenres:
    @pytest.mark.asyncio
    @patch("services.discover.homepage_service.build_similar_artist_pools")
    async def test_returns_correct_count_with_2_qualifying_genres(
        self, mock_pools: AsyncMock,
    ) -> None:
        top_genres = _make_top_genres(2)
        genre_names = [g for g, _ in top_genres]
        artists_by_genre = _make_artists_by_genre(genre_names, artists_per=4)
        albums = [{"title": "Lib 1", "mbid": "lib-1", "artist_name": "Art 1"}]
        genre_index = _make_genre_index(
            top_genres=top_genres,
            artists_by_genre=artists_by_genre,
            albums_by_genre=albums,
        )
        cache = _make_cache()
        mock_pools.return_value = [_make_pool_items(3)]

        service = _make_service(genre_index=genre_index, cache=cache)
        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert len(result) == 2


class TestDailyMixPersonalizedGenres:
    @pytest.mark.asyncio
    async def test_listening_genres_are_ranked_ahead_of_global_library_genres(
        self,
    ) -> None:
        genre_index = _make_genre_index(
            top_genres=[("rock", 10), ("pop", 8)],
            artists_by_genre={
                "dream pop": [str(UUID(int=i)) for i in range(1, 4)],
                "rock": [str(UUID(int=i)) for i in range(4, 7)],
                "pop": [str(UUID(int=i)) for i in range(7, 10)],
            },
        )
        service = _make_service(genre_index=genre_index, cache=_make_cache())
        service._build_single_daily_mix = AsyncMock(return_value=None)

        await service._build_daily_mix_sections(
            "u1",
            "listenbrainz",
            set(),
            listening_genres=[SimpleNamespace(genre="Dream Pop")],
        )

        genre_index.get_artists_for_genres.assert_awaited_once_with(
            ["dream pop", "rock", "pop"]
        )
        assert service._build_single_daily_mix.await_args_list[0].args[1] == "dream pop"


class TestDailyMixSectionTypeIsAlbums:
    @pytest.mark.asyncio
    @patch("services.discover.homepage_service.build_similar_artist_pools")
    async def test_section_type_is_albums(self, mock_pools: AsyncMock) -> None:
        top_genres = [("rock", 10)]
        artists_by_genre = {"rock": [str(UUID(int=i)) for i in range(1, 5)]}
        albums = [{"title": "Album", "mbid": "m1", "artist_name": "Art"}]
        genre_index = _make_genre_index(
            top_genres=top_genres,
            artists_by_genre=artists_by_genre,
            albums_by_genre=albums,
        )
        cache = _make_cache()
        mock_pools.return_value = [_make_pool_items(3)]

        service = _make_service(genre_index=genre_index, cache=cache)
        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert len(result) >= 1
        for section in result:
            assert section.type == "albums"


class TestDailyMixSectionTitleFormat:
    @pytest.mark.asyncio
    @patch("services.discover.homepage_service.build_similar_artist_pools")
    async def test_section_title_format(self, mock_pools: AsyncMock) -> None:
        top_genres = [("rock", 10), ("jazz", 8)]
        artists_by_genre = _make_artists_by_genre(["rock", "jazz"], artists_per=4)
        albums = [{"title": "Album", "mbid": "m1", "artist_name": "Art"}]
        genre_index = _make_genre_index(
            top_genres=top_genres,
            artists_by_genre=artists_by_genre,
            albums_by_genre=albums,
        )
        cache = _make_cache()
        mock_pools.return_value = [_make_pool_items(3)]

        service = _make_service(genre_index=genre_index, cache=cache)
        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert len(result) >= 1
        for i, section in enumerate(result):
            assert section.title.startswith(f"Daily Mix {i + 1} - ")


class TestDailyMixSectionSourceMatchesResolvedSource:
    @pytest.mark.asyncio
    @patch("services.discover.homepage_service.build_similar_artist_pools")
    async def test_section_source_matches_resolved_source(
        self, mock_pools: AsyncMock,
    ) -> None:
        top_genres = [("rock", 10)]
        artists_by_genre = {"rock": [str(UUID(int=i)) for i in range(1, 4)]}
        albums = [{"title": "Album", "mbid": "m1", "artist_name": "Art"}]
        genre_index = _make_genre_index(
            top_genres=top_genres,
            artists_by_genre=artists_by_genre,
            albums_by_genre=albums,
        )
        cache = _make_cache()
        mock_pools.return_value = [_make_pool_items(3)]

        service = _make_service(genre_index=genre_index, cache=cache)
        result = await service._build_daily_mix_sections("u1", "my_source", set())
        assert len(result) >= 1
        for section in result:
            assert section.source == "my_source"


class TestDailyMixFamiliarVsNewRatio:
    @pytest.mark.asyncio
    @patch("services.discover.homepage_service.build_similar_artist_pools")
    async def test_familiar_vs_new_ratio(self, mock_pools: AsyncMock) -> None:
        top_genres = [("rock", 10)]
        artists_by_genre = {"rock": [str(UUID(int=i)) for i in range(1, 5)]}
        albums = [
            {"title": f"Lib {i}", "mbid": f"lib-{i}", "artist_name": f"Art {i}"}
            for i in range(20)
        ]
        genre_index = _make_genre_index(
            top_genres=top_genres,
            artists_by_genre=artists_by_genre,
            albums_by_genre=albums,
        )
        cache = _make_cache()
        mock_pools.return_value = [_make_pool_items(15)]

        service = _make_service(genre_index=genre_index, cache=cache)
        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert len(result) >= 1
        section = result[0]
        total = len(section.items)
        assert total > 0
        new_count = sum(1 for item in section.items if not getattr(item, "in_library", False))
        familiar_count = sum(1 for item in section.items if getattr(item, "in_library", False))
        assert new_count >= total * 0.5 - 1


class TestDailyMixCachedResultReturnedOnSecondCall:
    @pytest.mark.asyncio
    async def test_cached_result_returned_on_second_call(self) -> None:
        cached_sections = [
            HomeSection(title="Cached Mix 1", type="albums", items=[], source="listenbrainz"),
        ]
        cache = _make_cache()
        cache.get = AsyncMock(return_value=cached_sections)
        genre_index = _make_genre_index(top_genres=[("rock", 10)])
        service = _make_service(genre_index=genre_index, cache=cache)

        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert result == cached_sections
        genre_index.get_top_genres.assert_not_awaited()


class TestDailyMixEmptyResultIsCached:
    @pytest.mark.asyncio
    async def test_empty_result_is_cached(self) -> None:
        genre_index = _make_genre_index(top_genres=[])
        cache = _make_cache()
        service = _make_service(genre_index=genre_index, cache=cache)

        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert result == []
        cache.set.assert_awaited_once()
        args = cache.set.call_args
        assert args[0][1] == []
        assert args[0][2] == DAILY_MIX_CACHE_TTL


class TestDailyMixCacheKeyIncludesSourceAndDate:
    def test_cache_key_includes_source_and_date(self) -> None:
        service = _make_service()
        key = service._daily_mix_cache_key("u1", "listenbrainz")
        assert key.startswith("daily_mix:u1:listenbrainz:")
        # date portion is YYYY-MM-DD
        date_part = key.split(":")[-1]
        parts = date_part.split("-")
        assert len(parts) == 3
        assert len(parts[0]) == 4


class TestDailyMixExceptionReturnsEmptyList:
    @pytest.mark.asyncio
    async def test_exception_in_builder_returns_empty_list(self) -> None:
        genre_index = _make_genre_index()
        genre_index.get_top_genres = AsyncMock(side_effect=RuntimeError("db failure"))
        cache = _make_cache()
        service = _make_service(genre_index=genre_index, cache=cache)

        result = await service._build_daily_mix_sections("u1", "listenbrainz", set())
        assert result == []


class TestHasMeaningfulContentWithDailyMixes:
    def test_has_meaningful_content_with_daily_mixes(self) -> None:
        service = _make_service()
        section = HomeSection(
            title="Mix",
            type="albums",
            items=[HomeAlbum(name="Album", artist_name="Artist")],
            source="lb",
        )
        response = DiscoverResponse(daily_mixes=[section])
        assert service._has_meaningful_content(response) is True

    def test_has_meaningful_content_empty(self) -> None:
        service = _make_service()
        response = DiscoverResponse()
        assert service._has_meaningful_content(response) is False


class TestDiscoverAlbumDeduplication:
    def test_removes_repeated_album_when_shelf_stays_healthy(self) -> None:
        service = _make_service()
        response = DiscoverResponse(
            top_picks=TopPicksSection(
                items=[
                    TopPickItem(
                        album=HomeAlbum(name="Featured", mbid="rg-shared"),
                        match_pct=90,
                    )
                ]
            ),
            fresh_releases=HomeSection(
                title="Fresh",
                type="albums",
                items=[
                    HomeAlbum(name="Repeated", mbid="rg-shared"),
                    HomeAlbum(name="One", mbid="rg-1"),
                    HomeAlbum(name="Two", mbid="rg-2"),
                    HomeAlbum(name="Three", mbid="rg-3"),
                ],
            ),
        )

        service._dedupe_album_sections(response)

        assert [item.mbid for item in response.fresh_releases.items] == [
            "rg-1",
            "rg-2",
            "rg-3",
        ]

    def test_keeps_original_shelf_when_deduplication_would_make_it_too_short(
        self,
    ) -> None:
        service = _make_service()
        response = DiscoverResponse(
            top_picks=TopPicksSection(
                items=[
                    TopPickItem(
                        album=HomeAlbum(name="Featured", mbid="rg-shared"),
                        match_pct=90,
                    )
                ]
            ),
            fresh_releases=HomeSection(
                title="Fresh",
                type="albums",
                items=[
                    HomeAlbum(name="Repeated", mbid="rg-shared"),
                    HomeAlbum(name="Only other album", mbid="rg-1"),
                ],
            ),
        )

        service._dedupe_album_sections(response)

        assert [item.mbid for item in response.fresh_releases.items] == [
            "rg-shared",
            "rg-1",
        ]
