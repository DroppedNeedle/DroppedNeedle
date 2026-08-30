from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

import services.spotify_import_service as spotify_module
from services.spotify_import_service import SpotifyImportService


def _service(mb_repo) -> SpotifyImportService:
    return SpotifyImportService(
        client_factory=AsyncMock(),
        playlist_repo=MagicMock(),
        mb_repo=mb_repo,
        playlist_service=MagicMock(),
    )


@pytest.mark.asyncio
async def test_isrc_durable_rows_use_cache_only_before_wire(monkeypatch):
    store = MagicMock()
    store.get_recordings_by_isrc = AsyncMock(return_value=["REC-B", "rec-A", "REC-B"])
    repo = SimpleNamespace(mb_canonical_store=store)
    repo.get_cached_recording_to_release_group = AsyncMock(
        side_effect=lambda recording: (
            f"rg-{recording}" if recording == "rec-a" else None
        )
    )
    repo.resolve_recording_to_release_group = AsyncMock(
        side_effect=lambda recording: f"rg-wire-{recording}"
    )
    service = _service(repo)
    provider = AsyncMock(side_effect=AssertionError("cache hit reached /isrc"))
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)

    result = await service._resolve_mbid("US123", "Artist", "Album")

    assert result == "rg-rec-a"
    repo.get_cached_recording_to_release_group.assert_awaited_once_with("rec-a")
    repo.resolve_recording_to_release_group.assert_not_awaited()
    provider.assert_not_awaited()


@pytest.mark.asyncio
async def test_isrc_durable_misses_use_one_wire_then_resolve_returned_recordings(
    monkeypatch,
):
    store = MagicMock()
    store.get_recordings_by_isrc = AsyncMock(return_value=["REC-B", "rec-A"])
    store.save_isrc_recordings = AsyncMock()
    repo = SimpleNamespace(mb_canonical_store=store)
    repo.get_cached_recording_to_release_group = AsyncMock(return_value=None)
    repo.resolve_recording_to_release_group = AsyncMock(
        side_effect=[None, "rg-rec-wire-b"]
    )
    service = _service(repo)
    calls = []
    provider = AsyncMock(
        side_effect=lambda path, **_kwargs: (
            calls.append(path)
            or {
                "recordings": [
                    {"id": "rec-wire-a", "releases": []},
                    {"id": "rec-wire-b", "releases": []},
                ]
            }
        )
    )
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)

    result = await service._resolve_mbid("US456", "Artist", "Album")

    assert result == "rg-rec-wire-b"
    assert calls == ["/isrc/US456"]
    assert [
        call.args[0]
        for call in repo.get_cached_recording_to_release_group.await_args_list
    ] == ["rec-a", "rec-b"]
    assert [
        call.args[0] for call in repo.resolve_recording_to_release_group.await_args_list
    ] == ["rec-wire-a", "rec-wire-b"]
    store.save_isrc_recordings.assert_awaited_once_with(
        [("US456", "rec-wire-a"), ("US456", "rec-wire-b")]
    )
    assert (
        provider.await_count + repo.resolve_recording_to_release_group.await_count == 3
    )
