from pathlib import Path
from unittest.mock import AsyncMock, Mock

import pytest
from mutagen.flac import FLAC, Picture

from services.home.local_album_artwork_service import LocalAlbumArtworkService

PNG = b'\x89PNG\r\n\x1a\ncover'


def embedded_flac(path):
    # Minimal FLAC metadata, enough to exercise the actual Mutagen file-object seam.
    info = bytearray(34)
    info[10:18] = ((44100 << 44) | (1 << 41) | (15 << 36)).to_bytes(8, 'big')
    path.write_bytes(b'fLaC' + b'\x80\x00\x00\x22' + info)
    audio = FLAC(path)
    picture = Picture()
    picture.type = 3
    picture.mime = 'image/png'
    picture.data = PNG
    audio.add_picture(picture)
    audio.save()


def source(root: Path, relative='album/track.flac'):
    return {'root_id': 'music', 'relative_path': relative, 'file_path': str(root / relative)}


def service(rows):
    store = AsyncMock()
    store.get_local_artwork_sources.return_value = rows
    return LocalAlbumArtworkService(store)


@pytest.mark.asyncio
async def test_local_only_sidecar_needs_no_provider_or_catalog_write(tmp_path):
    root = tmp_path.resolve()
    track = root / 'album/track.flac'
    track.parent.mkdir()
    track.write_bytes(b'audio')
    (track.parent / 'Cover.png').write_bytes(PNG)
    reader = service([source(root)])
    reader._tagger.read_cover_art = Mock(side_effect=AssertionError('sidecar wins'))

    result = await reader.get('unlinked-local-id', {'music': root})

    assert result[:3] == (PNG, 'image/png', 'folder')
    reader._store.get_local_artwork_sources.assert_awaited_once_with('unlinked-local-id')
    assert [call[0] for call in reader._store.mock_calls] == ['get_local_artwork_sources']


@pytest.mark.asyncio
async def test_embedded_read_uses_safely_opened_file_not_path(tmp_path):
    root = tmp_path.resolve()
    track = root / 'album/track.flac'
    track.parent.mkdir()
    track.write_bytes(b'audio')
    reader = service([source(root)])
    def embedded(stream):
        assert stream.read() == b'audio'
        assert not isinstance(stream, Path)
        return PNG
    reader._tagger.read_cover_art = Mock(side_effect=embedded)

    assert (await reader.get('album', {'music': root}))[:3] == (PNG, 'image/png', 'embedded')


@pytest.mark.asyncio
async def test_real_flac_embedded_art_without_catalog_identity(tmp_path):
    root = tmp_path.resolve()
    track = root / 'album/track.flac'
    track.parent.mkdir()
    embedded_flac(track)
    reader = service([source(root)])
    assert (await reader.get('local-only', {'music': root}))[:3] == (PNG, 'image/png', 'embedded')


@pytest.mark.asyncio
@pytest.mark.parametrize('case', ['missing', 'symlink_file', 'symlink_directory', 'traversal', 'unknown_root', 'excluded'])
async def test_unsafe_or_missing_sources_cannot_authorize_artwork(tmp_path, case):
    root = tmp_path.resolve() / 'music'
    root.mkdir()
    outside = tmp_path.resolve() / 'outside'
    outside.mkdir()
    (outside / 'track.flac').write_bytes(b'audio')
    (outside / 'cover.png').write_bytes(PNG)
    row = source(root)
    if case == 'symlink_directory':
        (root / 'album').symlink_to(outside, target_is_directory=True)
    else:
        (root / 'album').mkdir()
        (root / 'album/cover.png').write_bytes(PNG)
        if case == 'symlink_file':
            (root / 'album/track.flac').symlink_to(outside / 'track.flac')
        elif case == 'traversal':
            row = source(root, '../outside/track.flac')
        elif case in ('unknown_root', 'excluded'):
            (root / 'album/track.flac').write_bytes(b'audio')
    reader = service([row])
    reader._tagger.read_cover_art = Mock(side_effect=AssertionError('must not read'))

    assert await reader.get('album', {} if case == 'unknown_root' else {'music': root},
                            lambda _: case != 'excluded') is None


@pytest.mark.asyncio
async def test_symlink_sidecar_is_not_read_and_empty_art_is_terminal(tmp_path):
    root = tmp_path.resolve()
    (root / 'album').mkdir()
    (root / 'album/track.flac').write_bytes(b'audio')
    (root / 'outside.png').write_bytes(PNG)
    (root / 'album/cover.png').symlink_to(root / 'outside.png')
    reader = service([source(root)])
    reader._tagger.read_cover_art = Mock(return_value=None)
    assert await reader.get('album', {'music': root}) is None


@pytest.mark.asyncio
async def test_no_indexed_sources_returns_missing_without_reads(tmp_path):
    reader = service([])
    reader._tagger.read_cover_art = Mock(side_effect=AssertionError('must not read'))
    assert await reader.get('missing-or-retired', {'music': tmp_path.resolve()}) is None


@pytest.mark.asyncio
async def test_middle_track_only_embedded_art_is_found_in_ordinary_album(tmp_path):
    root = tmp_path.resolve()
    rows = []
    for number in range(1, 13):
        relative = f'album/{number:02}.flac'
        path = root / relative
        path.parent.mkdir(exist_ok=True)
        if number == 6:
            embedded_flac(path)
        else:
            path.write_bytes(b'not an audio file')
        rows.append(source(root, relative))
    reader = service(rows)
    result = await reader.get('local-only', {'music': root})
    assert result[:3] == (PNG, 'image/png', 'embedded')
