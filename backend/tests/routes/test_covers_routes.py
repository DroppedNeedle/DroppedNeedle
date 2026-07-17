import hashlib

import pytest
from unittest.mock import AsyncMock, MagicMock, ANY

from fastapi import FastAPI
from fastapi.testclient import TestClient

from api.v1.routes.covers import router
from core.dependencies import get_coverart_repository
from core.exceptions import ClientDisconnectedError
from core.exception_handlers import client_disconnected_handler


@pytest.fixture
def mock_cover_repo():
    mock = MagicMock()
    mock.get_release_group_cover = AsyncMock(return_value=(b'rg-image', 'image/jpeg', 'library'))
    mock.get_release_cover = AsyncMock(return_value=(b'rel-image', 'image/jpeg', 'jellyfin'))
    mock.get_artist_image = AsyncMock(return_value=(b'artist-image', 'image/png', 'wikidata'))
    mock.get_release_group_cover_etag = AsyncMock(return_value='etag-rg')
    mock.get_release_cover_etag = AsyncMock(return_value='etag-rel')
    mock.get_artist_image_etag = AsyncMock(return_value='etag-artist')
    mock.debug_artist_image = AsyncMock(side_effect=lambda _artist_id, debug_info: debug_info)
    # sync warming checks default to "not warming" so a None cover renders the placeholder;
    # tests that exercise the warming (202) path flip these explicitly.
    mock.is_rg_cover_warming = MagicMock(return_value=False)
    mock.is_release_cover_warming = MagicMock(return_value=False)
    mock.is_artist_cover_warming = MagicMock(return_value=False)
    return mock


@pytest.fixture
def client(mock_cover_repo):
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_coverart_repository] = lambda: mock_cover_repo
    app.add_exception_handler(ClientDisconnectedError, client_disconnected_handler)
    return TestClient(app)


def test_release_group_uses_dynamic_source_header(client):
    response = client.get('/covers/release-group/11111111-1111-1111-1111-111111111111?size=500')

    assert response.status_code == 200
    assert response.headers['x-cover-source'] == 'library'


def test_release_uses_dynamic_source_header(client):
    response = client.get('/covers/release/22222222-2222-2222-2222-222222222222?size=500')

    assert response.status_code == 200
    assert response.headers['x-cover-source'] == 'jellyfin'


def test_caa_fallback_is_not_browser_immutable(client, mock_cover_repo):
    mock_cover_repo.get_release_group_cover = AsyncMock(
        return_value=(b'caa-image', 'image/jpeg', 'cover-art-archive')
    )

    response = client.get('/covers/release-group/11111111-1111-1111-1111-111111111111')

    assert response.headers['cache-control'] == 'public, max-age=300'


def test_audiodb_cover_remains_browser_immutable(client, mock_cover_repo):
    mock_cover_repo.get_release_group_cover = AsyncMock(
        return_value=(b'audiodb-image', 'image/jpeg', 'audiodb')
    )

    response = client.get('/covers/release-group/11111111-1111-1111-1111-111111111111')

    assert response.headers['cache-control'] == 'public, max-age=31536000, immutable'


def test_artist_uses_dynamic_source_header(client, mock_cover_repo):
    mock_cover_repo.get_artist_image = AsyncMock(return_value=(b'artist-image', 'image/png', 'library'))

    response = client.get('/covers/artist/33333333-3333-3333-3333-333333333333?size=250')

    assert response.status_code == 200
    assert response.headers['x-cover-source'] == 'library'


def test_release_group_uses_placeholder_header_when_missing(client, mock_cover_repo):
    mock_cover_repo.get_release_group_cover = AsyncMock(return_value=None)

    response = client.get('/covers/release-group/44444444-4444-4444-4444-444444444444?size=500')

    assert response.status_code == 200
    assert response.headers['x-cover-source'] == 'placeholder'


def test_artist_uses_placeholder_header_when_missing(client, mock_cover_repo):
    mock_cover_repo.get_artist_image = AsyncMock(return_value=None)

    response = client.get('/covers/artist/55555555-5555-5555-5555-555555555555')

    assert response.status_code == 200
    assert response.headers['x-cover-source'] == 'placeholder'


def test_release_group_returns_202_warming_while_resolving(client, mock_cover_repo):
    mock_cover_repo.get_release_group_cover = AsyncMock(return_value=None)
    mock_cover_repo.is_rg_cover_warming = MagicMock(return_value=True)

    response = client.get('/covers/release-group/44444444-4444-4444-4444-444444444444?size=250')

    assert response.status_code == 202
    assert response.headers['x-cover-source'] == 'warming'
    assert response.headers['cache-control'] == 'no-store'


def test_artist_returns_202_warming_while_resolving(client, mock_cover_repo):
    mock_cover_repo.get_artist_image = AsyncMock(return_value=None)
    mock_cover_repo.is_artist_cover_warming = MagicMock(return_value=True)

    response = client.get('/covers/artist/55555555-5555-5555-5555-555555555555')

    assert response.status_code == 202
    assert response.headers['x-cover-source'] == 'warming'
    assert response.headers['cache-control'] == 'no-store'


def test_release_returns_202_warming_while_resolving(client, mock_cover_repo):
    mock_cover_repo.get_release_cover = AsyncMock(return_value=None)
    mock_cover_repo.is_release_cover_warming = MagicMock(return_value=True)

    response = client.get('/covers/release/55555555-5555-5555-5555-555555555555')

    assert response.status_code == 202
    assert response.headers['x-cover-source'] == 'warming'
    assert response.headers['cache-control'] == 'no-store'


def test_release_group_original_size_maps_to_none(client, mock_cover_repo):
    response = client.get('/covers/release-group/66666666-6666-6666-6666-666666666666?size=original')

    assert response.status_code == 200
    mock_cover_repo.get_release_group_cover.assert_awaited_once_with(
        '66666666-6666-6666-6666-666666666666',
        None,
        is_disconnected=ANY,
        defer_best_release=True,
    )


def test_release_rejects_invalid_size(client):
    response = client.get('/covers/release/77777777-7777-7777-7777-777777777777?size=999')

    assert response.status_code == 400


def test_release_group_sets_etag_header(client):
    response = client.get('/covers/release-group/11111111-1111-1111-1111-111111111111?size=500')

    assert response.status_code == 200
    expected = hashlib.sha1(b'rg-image').hexdigest()
    assert response.headers['etag'] == f'"{expected}"'


def test_release_group_returns_304_when_etag_matches(client, mock_cover_repo):
    expected = hashlib.sha1(b'rg-image').hexdigest()
    response = client.get(
        '/covers/release-group/11111111-1111-1111-1111-111111111111?size=500',
        headers={'If-None-Match': f'"{expected}"'},
    )

    assert response.status_code == 304
    mock_cover_repo.get_release_group_cover.assert_awaited_once()


def test_artist_returns_304_when_etag_matches(client, mock_cover_repo):
    response = client.get(
        '/covers/artist/33333333-3333-3333-3333-333333333333?size=250',
        headers={'If-None-Match': '"etag-artist"'},
    )

    assert response.status_code == 304
    mock_cover_repo.get_artist_image.assert_not_awaited()


def test_debug_artist_cover_recommends_negative_cache(client, mock_cover_repo):
    async def _debug_with_negative(_artist_id, debug_info):
        debug_info['disk_cache']['negative_250'] = True
        return debug_info

    mock_cover_repo.debug_artist_image = AsyncMock(side_effect=_debug_with_negative)

    response = client.get('/covers/debug/artist/33333333-3333-3333-3333-333333333333')

    assert response.status_code == 200
    assert 'negative cache entry' in response.json()['recommendation'].lower()


def test_release_group_returns_204_on_disconnect(client, mock_cover_repo):
    mock_cover_repo.get_release_group_cover_etag = AsyncMock(return_value=None)
    mock_cover_repo.get_release_group_cover = AsyncMock(
        side_effect=ClientDisconnectedError("disconnected")
    )
    response = client.get('/covers/release-group/66666666-6666-6666-6666-666666666666')
    assert response.status_code == 204


def test_release_returns_204_on_disconnect(client, mock_cover_repo):
    mock_cover_repo.get_release_cover_etag = AsyncMock(return_value=None)
    mock_cover_repo.get_release_cover = AsyncMock(
        side_effect=ClientDisconnectedError("disconnected")
    )
    response = client.get('/covers/release/66666666-6666-6666-6666-666666666666')
    assert response.status_code == 204


def test_artist_returns_204_on_disconnect(client, mock_cover_repo):
    mock_cover_repo.get_artist_image_etag = AsyncMock(return_value=None)
    mock_cover_repo.get_artist_image = AsyncMock(
        side_effect=ClientDisconnectedError("disconnected")
    )
    response = client.get('/covers/artist/33333333-3333-3333-3333-333333333333')
    assert response.status_code == 204
