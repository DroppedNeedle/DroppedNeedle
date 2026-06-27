from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest

import repositories.coverart_repository as coverart_repository_module
from repositories.coverart_repository import CoverArtRepository, _sniff_image_content_type


RELEASE_GROUP_MBID = '11111111-1111-1111-1111-111111111111'
_JPEG = b'\xff\xd8\xff\xe0' + b'\x00' * 16
_PNG = b'\x89PNG\r\n\x1a\n' + b'\x00' * 8


def _miss_external(monkeypatch):
    async def dedupe_return_none(_key, _factory):
        return None

    monkeypatch.setattr(coverart_repository_module._deduplicator, 'dedupe', dedupe_return_none)


@pytest.mark.parametrize(
    'data,expected',
    [
        (_JPEG, 'image/jpeg'),
        (_PNG, 'image/png'),
        (b'GIF89a' + b'\x00' * 8, 'image/gif'),
        (b'RIFF\x00\x00\x00\x00WEBP', 'image/webp'),
        (b'<svg xmlns="http://www.w3.org/2000/svg"></svg>', None),
        (b'not an image', None),
        (b'\xff\xd8', None),  # too short
    ],
)
def test_sniff_image_content_type(data, expected):
    assert _sniff_image_content_type(data) == expected


@pytest.mark.asyncio
async def test_embedded_cover_served_when_every_external_source_misses(tmp_path, monkeypatch):
    track = tmp_path / 'track.flac'
    track.write_bytes(b'fake flac')

    library_db = MagicMock()
    library_db.get_library_files_for_album = AsyncMock(return_value=[{'file_path': str(track)}])

    async with httpx.AsyncClient() as http_client:
        repo = CoverArtRepository(
            http_client=http_client, cache=MagicMock(), cache_dir=tmp_path, library_db=library_db
        )
        repo._disk_cache.read = AsyncMock(return_value=None)
        repo._disk_cache.is_negative = AsyncMock(return_value=False)
        repo._disk_cache.write_negative = AsyncMock()
        repo._disk_cache.write = AsyncMock()
        repo._tagger.read_cover_art = MagicMock(return_value=_JPEG)
        _miss_external(monkeypatch)

        result = await repo.get_release_group_cover(RELEASE_GROUP_MBID, size='500')

        assert result == (_JPEG, 'image/jpeg', 'embedded')
        repo._disk_cache.write_negative.assert_not_awaited()
        repo._disk_cache.write.assert_awaited_once()
        assert repo._disk_cache.write.await_args.args[3] == {'source': 'embedded'}


@pytest.mark.asyncio
async def test_no_library_db_falls_through_to_negative_cache(tmp_path, monkeypatch):
    async with httpx.AsyncClient() as http_client:
        repo = CoverArtRepository(http_client=http_client, cache=MagicMock(), cache_dir=tmp_path)
        repo._disk_cache.read = AsyncMock(return_value=None)
        repo._disk_cache.is_negative = AsyncMock(return_value=False)
        repo._disk_cache.write_negative = AsyncMock()
        _miss_external(monkeypatch)

        result = await repo.get_release_group_cover(RELEASE_GROUP_MBID, size='500')

        assert result is None
        repo._disk_cache.write_negative.assert_awaited_once()


@pytest.mark.asyncio
async def test_non_raster_embedded_art_is_skipped(tmp_path, monkeypatch):
    track = tmp_path / 'track.mp3'
    track.write_bytes(b'fake mp3')

    library_db = MagicMock()
    library_db.get_library_files_for_album = AsyncMock(return_value=[{'file_path': str(track)}])

    async with httpx.AsyncClient() as http_client:
        repo = CoverArtRepository(
            http_client=http_client, cache=MagicMock(), cache_dir=tmp_path, library_db=library_db
        )
        repo._disk_cache.read = AsyncMock(return_value=None)
        repo._disk_cache.is_negative = AsyncMock(return_value=False)
        repo._disk_cache.write_negative = AsyncMock()
        repo._tagger.read_cover_art = MagicMock(return_value=b'<svg></svg>')
        _miss_external(monkeypatch)

        result = await repo.get_release_group_cover(RELEASE_GROUP_MBID, size='500')

        assert result is None
        repo._disk_cache.write_negative.assert_awaited_once()
