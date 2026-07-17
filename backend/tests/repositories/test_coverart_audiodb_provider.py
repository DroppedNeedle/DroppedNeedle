import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest

from infrastructure.queue.priority_queue import RequestPriority
from repositories.audiodb_models import AudioDBAlbumImages, AudioDBArtistImages
from repositories.coverart_album import AlbumCoverFetcher
from repositories.coverart_artist import ArtistImageFetcher, TransientImageFetchError


def _response(
    status_code: int = 200,
    content_type: str = "image/jpeg",
    content: bytes = b"img",
) -> MagicMock:
    response = MagicMock()
    response.status_code = status_code
    response.headers = {"content-type": content_type}
    response.content = content
    return response


@pytest.mark.asyncio
async def test_album_fetch_from_audiodb_downloads_and_writes_cache():
    http_get = AsyncMock(return_value=_response())
    write_cache = AsyncMock()
    audiodb_service = MagicMock()
    audiodb_service.get_cached_album_images = AsyncMock(
        return_value=AudioDBAlbumImages(album_thumb_url="https://r2.theaudiodb.com/album.jpg")
    )
    fetcher = AlbumCoverFetcher(
        http_get_fn=http_get,
        write_cache_fn=write_cache,
        audiodb_service=audiodb_service,
    )

    result = await fetcher._fetch_from_audiodb("release-group-id", Path("/tmp/album.bin"))

    assert result == (b"img", "image/jpeg", "audiodb")
    audiodb_service.get_cached_album_images.assert_awaited_once_with("release-group-id")
    http_get.assert_awaited_once()
    await asyncio.sleep(0)
    assert write_cache.await_count == 1


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "cached_value",
    [
        None,
        AudioDBAlbumImages(is_negative=True),
        AudioDBAlbumImages(album_thumb_url=None),
    ],
)
async def test_album_fetch_from_audiodb_skips_when_cache_not_usable(cached_value):
    http_get = AsyncMock(return_value=_response())
    audiodb_service = MagicMock()
    audiodb_service.get_cached_album_images = AsyncMock(return_value=cached_value)
    fetcher = AlbumCoverFetcher(
        http_get_fn=http_get,
        write_cache_fn=AsyncMock(),
        audiodb_service=audiodb_service,
    )

    result = await fetcher._fetch_from_audiodb("release-group-id", Path("/tmp/album.bin"))

    assert result is None
    http_get.assert_not_awaited()


@pytest.mark.asyncio
async def test_album_fetch_from_audiodb_returns_none_when_service_missing():
    fetcher = AlbumCoverFetcher(
        http_get_fn=AsyncMock(return_value=_response()),
        write_cache_fn=AsyncMock(),
        audiodb_service=None,
    )

    result = await fetcher._fetch_from_audiodb("release-group-id", Path("/tmp/album.bin"))

    assert result is None


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "status_code,content_type",
    [
        (404, "image/jpeg"),
        (200, "application/json"),
    ],
)
async def test_album_fetch_from_audiodb_returns_none_for_invalid_download(status_code, content_type):
    http_get = AsyncMock(return_value=_response(status_code=status_code, content_type=content_type))
    write_cache = AsyncMock()
    audiodb_service = MagicMock()
    audiodb_service.get_cached_album_images = AsyncMock(
        return_value=AudioDBAlbumImages(album_thumb_url="https://r2.theaudiodb.com/album.jpg")
    )
    fetcher = AlbumCoverFetcher(
        http_get_fn=http_get,
        write_cache_fn=write_cache,
        audiodb_service=audiodb_service,
    )

    result = await fetcher._fetch_from_audiodb("release-group-id", Path("/tmp/album.bin"))

    assert result is None
    await asyncio.sleep(0)
    assert write_cache.await_count == 0


@pytest.mark.asyncio
async def test_album_fetch_from_audiodb_returns_none_on_exception():
    audiodb_service = MagicMock()
    audiodb_service.get_cached_album_images = AsyncMock(
        return_value=AudioDBAlbumImages(album_thumb_url="https://r2.theaudiodb.com/album.jpg")
    )
    http_get = AsyncMock(side_effect=RuntimeError("boom"))
    write_cache = AsyncMock()
    fetcher = AlbumCoverFetcher(
        http_get_fn=http_get,
        write_cache_fn=write_cache,
        audiodb_service=audiodb_service,
    )

    result = await fetcher._fetch_from_audiodb("release-group-id", Path("/tmp/album.bin"))

    assert result is None
    await asyncio.sleep(0)
    assert write_cache.await_count == 0


@pytest.mark.asyncio
async def test_release_cover_skips_audiodb_when_release_group_id_unavailable():
    http_get = AsyncMock(return_value=_response(content=b"cover"))
    mb_repo = MagicMock()
    mb_repo.get_release_group_id_from_release = AsyncMock(return_value=None)
    audiodb_service = MagicMock()
    audiodb_service.get_cached_album_images = AsyncMock()
    fetcher = AlbumCoverFetcher(
        http_get_fn=http_get,
        write_cache_fn=AsyncMock(),
        mb_repo=mb_repo,
        audiodb_service=audiodb_service,
    )
    fetcher._fetch_release_local_sources = AsyncMock(return_value=None)

    result = await fetcher.fetch_release_cover("release-id", None, Path("/tmp/release.bin"))
    await asyncio.sleep(0)

    assert result == (b"cover", "image/jpeg", "cover-art-archive")
    mb_repo.get_release_group_id_from_release.assert_awaited_once_with(
        "release-id", priority=RequestPriority.BACKGROUND_SYNC
    )
    audiodb_service.get_cached_album_images.assert_not_awaited()


@pytest.mark.asyncio
async def test_album_release_group_chain_uses_audiodb_before_coverartarchive():
    http_get = AsyncMock()
    fetcher = AlbumCoverFetcher(http_get_fn=http_get, write_cache_fn=AsyncMock(), audiodb_service=MagicMock())
    fetcher._fetch_release_group_local_sources = AsyncMock(return_value=None)
    fetcher._fetch_from_audiodb = AsyncMock(return_value=(b"img", "image/jpeg", "audiodb"))

    result = await fetcher.fetch_release_group_cover("release-group-id", None, Path("/tmp/album.bin"))

    assert result is not None and result[2] == "audiodb"
    fetcher._fetch_from_audiodb.assert_awaited_once_with("release-group-id", Path("/tmp/album.bin"), priority=RequestPriority.IMAGE_FETCH)
    http_get.assert_not_awaited()


@pytest.mark.asyncio
async def test_album_release_group_chain_falls_back_to_coverartarchive_when_audiodb_misses():
    http_get = AsyncMock(return_value=_response(content=b"cover"))
    fetcher = AlbumCoverFetcher(http_get_fn=http_get, write_cache_fn=AsyncMock(), audiodb_service=MagicMock())
    fetcher._fetch_release_group_local_sources = AsyncMock(return_value=None)
    fetcher._fetch_from_audiodb = AsyncMock(return_value=None)
    fetcher._get_cover_from_best_release = AsyncMock(return_value=None)

    result = await fetcher.fetch_release_group_cover("release-group-id", None, Path("/tmp/album.bin"))

    assert result == (b"cover", "image/jpeg", "cover-art-archive")
    fetcher._fetch_from_audiodb.assert_awaited_once()


@pytest.mark.asyncio
async def test_release_cover_uses_audiodb_before_coverartarchive():
    http_get = AsyncMock(return_value=_response(status_code=404))
    mb_repo = MagicMock()
    mb_repo.get_release_group_id_from_release = AsyncMock(return_value="release-group-id")
    fetcher = AlbumCoverFetcher(
        http_get_fn=http_get,
        write_cache_fn=AsyncMock(),
        mb_repo=mb_repo,
        audiodb_service=MagicMock(),
    )
    fetcher._fetch_release_local_sources = AsyncMock(return_value=None)
    fetcher._fetch_from_audiodb = AsyncMock(return_value=(b"img", "image/jpeg", "audiodb"))
    fetcher._release_audiodb_warming["release-id"] = (
        "release-group-id",
        float("inf"),
    )

    result = await fetcher.fetch_release_cover("release-id", None, Path("/tmp/release.bin"))

    assert result is not None and result[2] == "audiodb"
    fetcher._fetch_from_audiodb.assert_awaited_once_with("release-group-id", Path("/tmp/release.bin"), priority=RequestPriority.IMAGE_FETCH)
    assert "release-id" not in fetcher._release_audiodb_warming
    http_get.assert_not_awaited()


@pytest.mark.asyncio
async def test_release_cover_uses_caa_while_release_mapping_resolves():
    http_get = AsyncMock(return_value=_response(content=b"caa-image"))
    mb_repo = MagicMock()
    mb_repo.get_release_group_id_from_release = AsyncMock(return_value="release-group-id")
    fetcher = AlbumCoverFetcher(
        http_get_fn=http_get,
        write_cache_fn=AsyncMock(),
        mb_repo=mb_repo,
        audiodb_service=MagicMock(),
    )
    fetcher._fetch_release_local_sources = AsyncMock(return_value=None)

    result = await fetcher.fetch_release_cover(
        "release-id", None, Path("/tmp/release.bin")
    )
    await asyncio.sleep(0)

    assert result == (b"caa-image", "image/jpeg", "cover-art-archive")
    assert fetcher._release_audiodb_warming["release-id"][0] == "release-group-id"


@pytest.mark.asyncio
async def test_release_cover_does_not_wait_for_slow_musicbrainz_mapping():
    mapping_started = asyncio.Event()
    allow_mapping = asyncio.Event()

    async def slow_mapping(*args, **kwargs):
        mapping_started.set()
        await allow_mapping.wait()
        return "release-group-id"

    mb_repo = MagicMock()
    mb_repo.get_release_group_id_from_release = AsyncMock(side_effect=slow_mapping)
    fetcher = AlbumCoverFetcher(
        http_get_fn=AsyncMock(return_value=_response(content=b"caa-image")),
        write_cache_fn=AsyncMock(),
        mb_repo=mb_repo,
        audiodb_service=MagicMock(),
    )
    fetcher._fetch_release_local_sources = AsyncMock(return_value=None)

    result = await asyncio.wait_for(
        fetcher.fetch_release_cover(
            "release-id", None, Path("/tmp/release.bin")
        ),
        timeout=0.1,
    )

    assert result == (b"caa-image", "image/jpeg", "cover-art-archive")
    await asyncio.sleep(0)
    assert mapping_started.is_set()
    allow_mapping.set()
    await fetcher._release_audiodb_resolvers["release-id"]


@pytest.mark.asyncio
async def test_release_group_cover_skips_audiodb_when_service_missing():
    http_get = AsyncMock(return_value=_response(content=b"cover"))
    fetcher = AlbumCoverFetcher(http_get_fn=http_get, write_cache_fn=AsyncMock(), audiodb_service=None)
    fetcher._fetch_release_group_local_sources = AsyncMock(return_value=None)
    fetcher._get_cover_from_best_release = AsyncMock(return_value=None)

    result = await fetcher.fetch_release_group_cover("release-group-id", None, Path("/tmp/album.bin"))

    assert result == (b"cover", "image/jpeg", "cover-art-archive")


@pytest.mark.asyncio
async def test_artist_fetch_from_audiodb_downloads_and_writes_cache():
    http_get = AsyncMock(return_value=_response())
    write_cache = AsyncMock()
    audiodb_service = MagicMock()
    audiodb_service.get_cached_artist_images = AsyncMock(
        return_value=AudioDBArtistImages(thumb_url="https://r2.theaudiodb.com/artist.jpg")
    )
    fetcher = ArtistImageFetcher(
        http_get_fn=http_get,
        write_cache_fn=write_cache,
        cache=MagicMock(),
        audiodb_service=audiodb_service,
    )

    result = await fetcher._fetch_from_audiodb("artist-id", Path("/tmp/artist.bin"))

    assert result == (b"img", "image/jpeg", "audiodb")
    audiodb_service.get_cached_artist_images.assert_awaited_once_with("artist-id")
    http_get.assert_awaited_once()
    await asyncio.sleep(0)
    assert write_cache.await_count == 1


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "cached_value",
    [
        None,
        AudioDBArtistImages(is_negative=True),
        AudioDBArtistImages(thumb_url=None),
    ],
)
async def test_artist_fetch_from_audiodb_skips_when_cache_not_usable(cached_value):
    http_get = AsyncMock(return_value=_response())
    audiodb_service = MagicMock()
    audiodb_service.get_cached_artist_images = AsyncMock(return_value=cached_value)
    fetcher = ArtistImageFetcher(
        http_get_fn=http_get,
        write_cache_fn=AsyncMock(),
        cache=MagicMock(),
        audiodb_service=audiodb_service,
    )

    result = await fetcher._fetch_from_audiodb("artist-id", Path("/tmp/artist.bin"))

    assert result is None
    http_get.assert_not_awaited()


@pytest.mark.asyncio
async def test_artist_fetch_from_audiodb_reraises_transient_exception():
    http_get = AsyncMock(side_effect=httpx.TimeoutException("timeout"))
    audiodb_service = MagicMock()
    audiodb_service.get_cached_artist_images = AsyncMock(
        return_value=AudioDBArtistImages(thumb_url="https://r2.theaudiodb.com/artist.jpg")
    )
    fetcher = ArtistImageFetcher(
        http_get_fn=http_get,
        write_cache_fn=AsyncMock(),
        cache=MagicMock(),
        audiodb_service=audiodb_service,
    )

    with pytest.raises(httpx.TimeoutException):
        await fetcher._fetch_from_audiodb("artist-id", Path("/tmp/artist.bin"))


@pytest.mark.asyncio
async def test_artist_fetch_from_audiodb_handles_non_transient_exception():
    http_get = AsyncMock(side_effect=RuntimeError("boom"))
    audiodb_service = MagicMock()
    audiodb_service.get_cached_artist_images = AsyncMock(
        return_value=AudioDBArtistImages(thumb_url="https://r2.theaudiodb.com/artist.jpg")
    )
    fetcher = ArtistImageFetcher(
        http_get_fn=http_get,
        write_cache_fn=AsyncMock(),
        cache=MagicMock(),
        audiodb_service=audiodb_service,
    )

    result = await fetcher._fetch_from_audiodb("artist-id", Path("/tmp/artist.bin"))

    assert result is None


@pytest.mark.asyncio
async def test_artist_fetch_from_audiodb_cache_miss_enqueues_warm():
    audiodb_service = MagicMock()
    audiodb_service.get_cached_artist_images = AsyncMock(return_value=None)
    audiodb_service.fetch_and_cache_artist_images = AsyncMock()
    browse_queue = MagicMock()
    browse_queue.enqueue = AsyncMock()
    fetcher = ArtistImageFetcher(
        http_get_fn=AsyncMock(),
        write_cache_fn=AsyncMock(),
        cache=MagicMock(),
        audiodb_service=audiodb_service,
        audiodb_browse_queue=browse_queue,
    )

    result = await fetcher._fetch_from_audiodb("artist-id", Path("/tmp/artist.bin"))

    assert result is None
    browse_queue.enqueue.assert_awaited_once_with("artist", "artist-id")
    audiodb_service.fetch_and_cache_artist_images.assert_not_awaited()


@pytest.mark.asyncio
async def test_artist_chain_uses_audiodb_before_wikidata():
    fetcher = ArtistImageFetcher(
        http_get_fn=AsyncMock(),
        write_cache_fn=AsyncMock(),
        cache=MagicMock(),
        audiodb_service=MagicMock(),
    )
    fetcher._fetch_local_sources = AsyncMock(return_value=(None, False))
    fetcher._fetch_from_audiodb = AsyncMock(return_value=(b"img", "image/jpeg", "audiodb"))
    fetcher._fetch_from_wikidata = AsyncMock(return_value=(b"wiki", "image/jpeg", "wikidata"))

    result = await fetcher.fetch_artist_image("artist-id", 300, Path("/tmp/artist.bin"))

    assert result is not None and result[2] == "audiodb"
    fetcher._fetch_from_wikidata.assert_not_awaited()


@pytest.mark.asyncio
async def test_artist_chain_falls_back_to_wikidata_after_audiodb_miss():
    fetcher = ArtistImageFetcher(
        http_get_fn=AsyncMock(),
        write_cache_fn=AsyncMock(),
        cache=MagicMock(),
        audiodb_service=MagicMock(),
    )
    fetcher._fetch_local_sources = AsyncMock(return_value=(None, False))
    fetcher._fetch_from_audiodb = AsyncMock(return_value=None)
    fetcher._fetch_from_wikidata = AsyncMock(return_value=(b"wiki", "image/jpeg", "wikidata"))

    result = await fetcher.fetch_artist_image("artist-id", 300, Path("/tmp/artist.bin"))

    assert result is not None and result[2] == "wikidata"
    fetcher._fetch_from_wikidata.assert_awaited_once()


@pytest.mark.asyncio
async def test_artist_chain_skips_audiodb_when_service_missing():
    fetcher = ArtistImageFetcher(
        http_get_fn=AsyncMock(),
        write_cache_fn=AsyncMock(),
        cache=MagicMock(),
        audiodb_service=None,
    )
    fetcher._fetch_local_sources = AsyncMock(return_value=(None, False))
    fetcher._fetch_from_wikidata = AsyncMock(return_value=(b"wiki", "image/jpeg", "wikidata"))

    result = await fetcher.fetch_artist_image("artist-id", 300, Path("/tmp/artist.bin"))

    assert result is not None and result[2] == "wikidata"


@pytest.mark.asyncio
async def test_artist_chain_raises_transient_when_audiodb_fails_transiently_and_no_fallback_result():
    fetcher = ArtistImageFetcher(
        http_get_fn=AsyncMock(),
        write_cache_fn=AsyncMock(),
        cache=MagicMock(),
        audiodb_service=MagicMock(),
    )
    fetcher._fetch_local_sources = AsyncMock(return_value=(None, False))
    fetcher._fetch_from_audiodb = AsyncMock(side_effect=httpx.TimeoutException("timeout"))
    fetcher._fetch_from_wikidata = AsyncMock(return_value=None)

    with pytest.raises(TransientImageFetchError):
        await fetcher.fetch_artist_image("artist-id", 300, Path("/tmp/artist.bin"))
