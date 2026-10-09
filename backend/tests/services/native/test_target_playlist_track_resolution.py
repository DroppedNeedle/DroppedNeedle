"""Target playlist tables carry "Request all tracks" state (#204).

Production serves target_main, whose playlists live in library_playlist_tracks
behind TargetPlaylistRepository, so the identity/resolution columns and the
Spotify re-sync carry-over must work there, not only in the legacy table.
"""

import sqlite3
import threading
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.persistence.native_library_store import NativeLibraryStore
from repositories.playlist_repository import TrackResolution
from services.native.target_reference_adapters import TargetPlaylistRepository
from services.playlist_service import PlaylistService
from services.spotify_import_service import SpotifyImportService

_COLUMNS = {
    "recording_mbid",
    "isrc",
    "target_release_group_mbid",
    "home_tier",
    "resolution_status",
    "flag_options",
}


def _store(tmp_path: Path) -> NativeLibraryStore:
    auth = tmp_path / "auth.db"
    if not auth.exists():
        with sqlite3.connect(auth) as conn:
            conn.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
    return NativeLibraryStore(tmp_path / "target.db", threading.Lock())


def _columns(db: Path) -> set[str]:
    with sqlite3.connect(db) as conn:
        return {
            row[1] for row in conn.execute("PRAGMA table_info(library_playlist_tracks)")
        }


def test_columns_are_added_idempotently(tmp_path):
    store = _store(tmp_path)
    assert _COLUMNS <= _columns(store.db_path)
    _store(tmp_path)  # reopening an upgraded database must not fail
    assert _COLUMNS <= _columns(store.db_path)


@pytest.mark.asyncio
async def test_identity_and_resolution_round_trip(tmp_path):
    repo = TargetPlaylistRepository(_store(tmp_path))
    playlist = await repo.create_playlist("CCR", user_id="user-1")
    (track,) = await repo.add_tracks(
        playlist.id,
        [
            {
                "track_name": "Fortunate Son",
                "artist_name": "Creedence Clearwater Revival",
                "album_name": "Chronicle",
                "source_type": "",
                "isrc": "USFI86900049",
                "recording_mbid": "rec-1",
            }
        ],
    )
    assert (track.isrc, track.recording_mbid, track.resolution_status) == (
        "USFI86900049",
        "rec-1",
        None,
    )
    before = (await repo.get_playlist(playlist.id)).updated_at

    options = [{"kind": "compilation", "release_group_mbid": "rg-chronicle"}]
    updated = await repo.batch_update_resolutions(
        playlist.id,
        {
            track.id: TrackResolution(
                None, None, "flagged", recording_mbid="rec-2", flag_options=options
            ),
            "missing": TrackResolution("rg", "album", "resolved"),
        },
    )
    assert updated == 1
    stored = await repo.get_track(playlist.id, track.id)
    assert (stored.recording_mbid, stored.resolution_status, stored.flag_options) == (
        "rec-2",
        "flagged",
        options,
    )
    assert (await repo.get_playlist(playlist.id)).updated_at == before

    # A source update (the post-import auto-link) keeps the verdict.
    relinked = await repo.update_track_source(
        playlist.id, track.id, available_sources=["navidrome"]
    )
    assert relinked.resolution_status == "flagged"
    assert relinked.flag_options == options

    # Another playlist cannot be written through this one's id.
    other = await repo.create_playlist("Other", user_id="user-1")
    assert (
        await repo.batch_update_resolutions(
            other.id, {track.id: TrackResolution("rg", "album", "resolved")}
        )
        == 0
    )


@pytest.mark.asyncio
async def test_spotify_resync_keeps_resolutions_on_target_tables(tmp_path):
    repo = TargetPlaylistRepository(_store(tmp_path))
    playlists = PlaylistService(repo=None, cache_dir=tmp_path, async_repo=repo)
    client = AsyncMock()
    client.get_playlist.return_value = {"id": "spot-1", "name": "Mix", "images": []}
    base = {
        "artists": [{"name": "Creedence Clearwater Revival"}],
        "album": {"id": "", "name": "Chronicle", "images": []},
        "duration_ms": 140_000,
    }
    song = {**base, "name": "Fortunate Son", "external_ids": {"isrc": "USFI86900049"}}
    client.get_playlist_tracks.return_value = [song]
    factory = AsyncMock()
    factory.resolve_spotify.return_value = client
    svc = SpotifyImportService(
        client_factory=factory,
        playlist_repo=None,
        async_playlist_repo=repo,
        mb_repo=AsyncMock(),
        playlist_service=playlists,
        cache=InMemoryCache(),
    )

    playlist_id = await svc.ensure_playlist_record("user-1", "spot-1", "Mix")
    await svc.populate_playlist("user-1", "spot-1", playlist_id)
    (first,) = await repo.get_tracks(playlist_id)
    assert first.isrc == "USFI86900049"
    await repo.batch_update_resolutions(
        playlist_id,
        {
            first.id: TrackResolution(
                "rg-willy", "album", "chosen", recording_mbid="rec-fs"
            )
        },
    )

    await svc.populate_playlist("user-1", "spot-1", playlist_id)

    (second,) = await repo.get_tracks(playlist_id)
    assert second.id != first.id
    assert (
        second.recording_mbid,
        second.target_release_group_mbid,
        second.resolution_status,
    ) == ("rec-fs", "rg-willy", "chosen")
