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
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
)
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


# --- linking by recording (#204 follow-up) ------------------------------------


async def _seed_local_track(
    store: NativeLibraryStore,
    track_id: str,
    *,
    recording: str | None,
    album_id: str = "album-1",
) -> None:
    artist = LocalArtist(
        id="artist-1",
        display_name="Creedence Clearwater Revival",
        folded_name="creedence clearwater revival",
        normalized_name="creedence clearwater revival",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id=album_id,
        root_id="root",
        grouping_key=f"group-{album_id}",
        title="Willy and the Poor Boys",
        album_artist_id=artist.id,
        album_artist_name=artist.display_name,
        created_at=1,
        updated_at=1,
    )
    track = LocalTrack(
        id=track_id,
        local_album_id=album.id,
        root_id="root",
        file_path=f"/music/{track_id}.flac",
        relative_path=f"{track_id}.flac",
        path_hash=f"hash-{track_id}",
        file_size_bytes=1,
        file_mtime_ns=2,
        stat_revision="stat-1",
        tag_revision=f"tag-{track_id}",
        title="Fortunate Son",
        artist_name=artist.display_name,
        album_title=album.title,
        album_artist_name=artist.display_name,
        track_number=6,
        duration_seconds=140,
        file_format="flac",
        imported_at=1,
        embedded_recording_mbid=recording,
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=[track],
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
            },
        )
    )


async def _entry_playlist(repo, *entries):
    playlist = await repo.create_playlist("CCR", user_id="user-1")
    base = {
        "artist_name": "Creedence Clearwater Revival",
        "album_name": "Chronicle",
        "source_type": "",
    }
    await repo.add_tracks(playlist.id, [{**base, **entry} for entry in entries])
    return playlist.id, {t.track_name: t for t in await repo.get_tracks(playlist.id)}


@pytest.mark.asyncio
async def test_links_by_embedded_recording_and_heals_unknown_source(tmp_path):
    store = _store(tmp_path)
    repo = TargetPlaylistRepository(store)
    await _seed_local_track(store, "track-fs", recording="REC-FS")
    pid, _before = await _entry_playlist(
        repo,
        {"track_name": "Fortunate Son", "recording_mbid": "rec-fs"},
        {"track_name": "Not downloaded", "recording_mbid": "rec-none"},
        {"track_name": "No recording"},
    )

    assert await repo.link_tracks_by_recording(pid) == 1

    after = {t.track_name: t for t in await repo.get_tracks(pid)}
    linked = after["Fortunate Son"]
    assert (linked.library_file_id, linked.source_type, linked.track_source_id) == (
        "track-fs",
        "local",
        "track-fs",
    )
    assert linked.available_sources == ["local"]
    assert after["Not downloaded"].library_file_id is None
    assert after["No recording"].source_type == ""
    # The export now sees the file.
    rows = await store.list_target_playlist_export_rows(pid)
    assert [r["file_path"] for r in rows] == ["/music/track-fs.flac", None, None]
    # Idempotent.
    assert await repo.link_tracks_by_recording(pid) == 0


@pytest.mark.asyncio
async def test_links_by_identity_row_and_keeps_an_existing_source(tmp_path):
    store = _store(tmp_path)
    repo = TargetPlaylistRepository(store)
    await _seed_local_track(store, "track-fs", recording=None)
    with sqlite3.connect(store.db_path) as conn:
        conn.execute(
            "INSERT INTO local_track_external_identities "
            "(local_track_id, recording_mbid, decision_source, selected_at) "
            "VALUES ('track-fs', 'rec-fs', 'manual', 1)"
        )
    pid, _ = await _entry_playlist(
        repo,
        {
            "track_name": "Fortunate Son",
            "recording_mbid": "rec-fs",
            "source_type": "navidrome",
            "available_sources": ["navidrome"],
        },
    )

    assert await repo.link_tracks_by_recording(pid) == 1

    (track,) = await repo.get_tracks(pid)
    assert track.library_file_id == "track-fs"
    assert track.source_type == "navidrome"  # never clobbered
    assert track.available_sources == ["navidrome"]


@pytest.mark.asyncio
async def test_ignores_unavailable_tracks_and_other_playlists(tmp_path):
    store = _store(tmp_path)
    repo = TargetPlaylistRepository(store)
    await _seed_local_track(store, "track-gone", recording="rec-fs")
    with sqlite3.connect(store.db_path) as conn:
        conn.execute("UPDATE local_tracks SET availability = 'missing'")
    pid, _ = await _entry_playlist(
        repo, {"track_name": "Fortunate Son", "recording_mbid": "rec-fs"}
    )
    other, _ = await _entry_playlist(
        repo, {"track_name": "Fortunate Son", "recording_mbid": "rec-fs"}
    )

    assert await repo.link_tracks_by_recording(pid) == 0

    with sqlite3.connect(store.db_path) as conn:
        conn.execute("UPDATE local_tracks SET availability = 'indexed'")
    assert await repo.link_tracks_by_recording(pid) == 1
    (untouched,) = await repo.get_tracks(other)
    assert untouched.library_file_id is None


@pytest.mark.asyncio
async def test_navidrome_export_writes_a_recording_linked_entry(tmp_path):
    """The reported bug: requested tracks never reached Navidrome because the
    export only writes linked entries; it now links by recording first."""
    from services.navidrome_playlist_export_service import (
        NavidromePlaylistExportService,
    )

    store = _store(tmp_path)
    repo = TargetPlaylistRepository(store)
    await _seed_local_track(store, "track-fs", recording="rec-fs")
    pid, _ = await _entry_playlist(
        repo, {"track_name": "Fortunate Son", "recording_mbid": "rec-fs"}
    )
    out = tmp_path / "playlists"
    out.mkdir()

    result = await NavidromePlaylistExportService(store).sync(
        target_dir=str(out), scope="all"
    )

    assert (result.written, result.skipped_empty) == (1, 0)
    (exported,) = out.glob("*.m3u8")
    assert "track-fs.flac" in exported.read_text()
