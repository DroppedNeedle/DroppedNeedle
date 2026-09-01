from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import ExternalServiceError
from api.v1.schemas.discover import DiscoverResponse
from api.v1.schemas.home import HomeGenre, HomeResponse, HomeSection
from models.album import AlbumInfo
from models.artist import ArtistInfo
from repositories import musicbrainz_base as mb_base
from services.album_service import AlbumService
from services.artist_service import ArtistService
from services.discover.homepage_service import DiscoverHomepageService
from services.home.facade import HomeService


_MBID = "8e1e9e51-38dc-4df3-8027-a0ada37d4674"
_ARTIST_MBID = "f4a31f0a-51dd-4fa7-986d-3095c40c5ed9"


def _source(monkeypatch, url: str, generation: int) -> None:
    monkeypatch.setattr(mb_base, "_mb_api_base", url)
    monkeypatch.setattr(mb_base, "_mb_source_generation", generation)


def _album_service() -> AlbumService:
    library_repo = MagicMock()
    library_db = MagicMock()
    memory_cache = MagicMock()
    disk_cache = MagicMock()
    preferences = MagicMock()
    preferences.get_advanced_settings.return_value = SimpleNamespace(
        cache_ttl_album_library=3600,
        cache_ttl_album_non_library=600,
    )
    memory_cache.get = AsyncMock(return_value=None)
    memory_cache.set = AsyncMock()
    disk_cache.get_album = AsyncMock(return_value=None)
    disk_cache.set_album = AsyncMock()
    return AlbumService(
        library_repo=library_repo,
        mb_repo=MagicMock(),
        library_db=library_db,
        memory_cache=memory_cache,
        disk_cache=disk_cache,
        preferences_service=preferences,
    )


@pytest.mark.asyncio
async def test_album_provider_result_is_not_published_after_source_switch(monkeypatch):
    _source(monkeypatch, "https://old.example/ws/2", 101)
    service = _album_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._apply_audiodb_album_images = AsyncMock(
        side_effect=lambda info, *a, **k: info
    )

    async def build(*args, **kwargs):
        _source(monkeypatch, "https://new.example/ws/2", 102)
        return AlbumInfo(
            title="Album",
            musicbrainz_id=_MBID,
            artist_name="Artist",
            artist_id=_ARTIST_MBID,
        )

    service._build_album_from_musicbrainz = AsyncMock(side_effect=build)

    result = await service.get_album_info(_MBID.upper())

    assert result.title == "Album"
    service._cache.set.assert_not_awaited()
    service._disk_cache.set_album.assert_not_awaited()


def _artist_service() -> ArtistService:
    library_repo = MagicMock()
    library_repo.is_configured.return_value = False
    memory_cache = MagicMock()
    disk_cache = MagicMock()
    memory_cache.get = AsyncMock(return_value=None)
    memory_cache.set = AsyncMock()
    disk_cache.get_artist = AsyncMock(return_value=None)
    disk_cache.set_artist = AsyncMock()
    preferences = MagicMock()
    preferences.get_advanced_settings.return_value = SimpleNamespace(
        cache_ttl_artist_library=21600,
        cache_ttl_artist_non_library=3600,
    )
    return ArtistService(
        mb_repo=MagicMock(),
        library_repo=library_repo,
        wikidata_repo=MagicMock(),
        preferences_service=preferences,
        memory_cache=memory_cache,
        disk_cache=disk_cache,
    )


@pytest.mark.asyncio
async def test_artist_provider_result_is_not_published_after_source_switch(monkeypatch):
    _source(monkeypatch, "https://old.example/ws/2", 201)
    service = _artist_service()

    async def build(*args, **kwargs):
        _source(monkeypatch, "https://new.example/ws/2", 202)
        return ArtistInfo(name="Artist", musicbrainz_id=_ARTIST_MBID)

    service._build_artist_from_musicbrainz = AsyncMock(side_effect=build)
    with pytest.raises(ExternalServiceError, match="source changed"):
        await service.get_artist_info_basic(_ARTIST_MBID.upper())
    service._cache.set.assert_not_awaited()
    service._disk_cache.set_artist.assert_not_awaited()


def _home_service(memory_cache: MagicMock) -> HomeService:
    return HomeService(
        listenbrainz_repo=MagicMock(),
        jellyfin_repo=MagicMock(),
        library_repo=MagicMock(),
        musicbrainz_repo=MagicMock(),
        preferences_service=MagicMock(),
        memory_cache=memory_cache,
    )


@pytest.mark.asyncio
async def test_home_provider_result_is_not_published_after_source_switch(monkeypatch):
    _source(monkeypatch, "https://old.example/ws/2", 301)
    memory_cache = MagicMock()
    memory_cache.set = AsyncMock()
    service = _home_service(memory_cache)
    service._build_full = AsyncMock(
        side_effect=lambda *args, **kwargs: _switch_and_home(monkeypatch)
    )

    await service.warm_cache("user-1")

    memory_cache.set.assert_not_awaited()


def _switch_and_home(monkeypatch) -> HomeResponse:
    _source(monkeypatch, "https://new.example/ws/2", 302)
    return HomeResponse()


def _discover_service(memory_cache: MagicMock) -> DiscoverHomepageService:
    integration = MagicMock()
    integration.get_discover_cache_key.return_value = "discover:user-1"
    integration.is_jellyfin_enabled.return_value = False
    integration.is_library_configured.return_value = False
    integration.get_integration_status.return_value = MagicMock()
    service = DiscoverHomepageService(
        listenbrainz_repo=MagicMock(),
        jellyfin_repo=MagicMock(),
        library_repo=MagicMock(),
        musicbrainz_repo=MagicMock(),
        integration=integration,
        mbid_resolution=MagicMock(),
        memory_cache=memory_cache,
    )
    service._resolve_user_music = AsyncMock(
        return_value=(None, None, None, None, False, False, "listenbrainz")
    )
    return service


@pytest.mark.asyncio
async def test_discover_provider_result_is_not_published_after_source_switch(
    monkeypatch,
):
    _source(monkeypatch, "https://old.example/ws/2", 401)
    memory_cache = MagicMock()
    memory_cache.set = AsyncMock()
    service = _discover_service(memory_cache)

    async def build(*args, **kwargs):
        _source(monkeypatch, "https://new.example/ws/2", 402)
        return DiscoverResponse(
            genre_list=HomeSection(
                title="Genres",
                type="genres",
                items=[HomeGenre(name="rock")],
            )
        )

    service.build_discover_data = AsyncMock(side_effect=build)

    await service.warm_cache("user-1")

    memory_cache.set.assert_not_awaited()
