"""LidarrImportRepository against the mock Lidarr transport (the real HTTP + decode path).

Confirms tolerant decode, version read, and that every failure mode raises
``LidarrImportError`` (never a raw httpx/msgspec error), with the bad-key ``auth`` flag set.
"""

import asyncio

import pytest

from core.exceptions import LidarrImportError
from repositories.lidarr_import import LidarrImportRepository
from repositories.lidarr_import import lidarr_import_repository as repo_module
from tests.mocks import lidarr_mock

URL = "http://lidarr.test"


@pytest.fixture(autouse=True)
def _fast_and_clean(monkeypatch):
    # No real backoff waits, and a clean circuit breaker per test (it's module-level).
    async def _no_sleep(_seconds):
        return None

    monkeypatch.setattr(asyncio, "sleep", _no_sleep)
    repo_module._lidarr_import_circuit_breaker.reset()
    yield
    repo_module._lidarr_import_circuit_breaker.reset()


@pytest.mark.asyncio
async def test_get_system_status_returns_version():
    repo = LidarrImportRepository(lidarr_mock.client_for(lidarr_mock.lidarr_handler))
    status = await repo.get_system_status(URL, lidarr_mock.GOOD_KEY)
    assert status.version == lidarr_mock.VERSION


@pytest.mark.asyncio
async def test_list_artists_decodes_tolerantly():
    repo = LidarrImportRepository(lidarr_mock.client_for(lidarr_mock.lidarr_handler))
    artists = await repo.list_artists(URL, lidarr_mock.GOOD_KEY)
    assert len(artists) == len(lidarr_mock.ARTISTS)
    auto = next(a for a in artists if a.foreign_artist_id == lidarr_mock.MBID_AUTO)
    assert auto.artist_name == "Auto Artist"
    assert auto.monitored is True
    assert auto.monitor_new_items == "all"
    assert auto.status == "continuing"
    assert auto.statistics is not None and auto.statistics.track_file_count == 42
    # mbId is never read even when present on the wire.
    assert not hasattr(auto, "mb_id")


@pytest.mark.asyncio
async def test_bad_key_raises_auth_error():
    repo = LidarrImportRepository(lidarr_mock.client_for(lidarr_mock.lidarr_handler))
    with pytest.raises(LidarrImportError) as exc:
        await repo.get_system_status(URL, "wrong-key")
    assert exc.value.auth is True


@pytest.mark.asyncio
async def test_unreachable_raises_lidarr_error_not_raw_httpx():
    repo = LidarrImportRepository(lidarr_mock.client_for(lidarr_mock.unreachable_handler))
    with pytest.raises(LidarrImportError) as exc:
        await repo.list_artists(URL, lidarr_mock.GOOD_KEY)
    assert exc.value.auth is False


@pytest.mark.asyncio
async def test_decode_failure_raises_lidarr_error():
    repo = LidarrImportRepository(lidarr_mock.client_for(lidarr_mock.garbage_handler))
    with pytest.raises(LidarrImportError):
        await repo.list_artists(URL, lidarr_mock.GOOD_KEY)


@pytest.mark.asyncio
async def test_malformed_url_raises_lidarr_error_not_raw_httpx():
    # A bad-port typo makes httpx raise InvalidURL (NOT an httpx.HTTPError); it must still
    # surface as LidarrImportError so the Test route reports it and the import path 503s.
    repo = LidarrImportRepository(lidarr_mock.client_for(lidarr_mock.lidarr_handler))
    with pytest.raises(LidarrImportError):
        await repo.get_system_status("http://192.168.1.5:808o", lidarr_mock.GOOD_KEY)
