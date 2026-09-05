"""list_target_playlist_export_rows joins playlist entries to local files.

Entries with no local track keep a NULL file_path instead of vanishing, so
the exporter can count them rather than silently dropping them."""

import threading
from pathlib import Path

import pytest

from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
)


@pytest.fixture
def store(tmp_path: Path) -> NativeLibraryStore:
    return NativeLibraryStore(tmp_path / "library.db", threading.Lock())


async def _seed_track(store: NativeLibraryStore, *, track_id: str = "track-1") -> None:
    artist = LocalArtist(
        id="artist-1",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="album-1",
        root_id="root",
        grouping_key="group-album-1",
        title="Album",
        album_artist_id=artist.id,
        album_artist_name="Artist",
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
        title="Track",
        artist_name="Artist",
        album_title="Album",
        album_artist_name="Artist",
        track_number=1,
        duration_seconds=180,
        file_format="flac",
        imported_at=1,
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


def _track(index: int, **overrides) -> dict:
    entry = {
        "id": f"pt-{index}",
        "track_name": f"Song {index}",
        "artist_name": "Artist",
        "album_name": "Album",
        "source_type": "local",
        "library_file_id": "track-1",
        "duration": 180,
        "created_at": "t",
    }
    entry.update(overrides)
    return entry


@pytest.mark.asyncio
async def test_export_rows_join_file_paths_in_position_order(
    store: NativeLibraryStore,
) -> None:
    await _seed_track(store)
    await store.create_target_playlist(
        playlist_id="p1",
        name="Mix",
        source_ref=None,
        user_id="u1",
        created_at="t",
    )
    await store.add_target_playlist_tracks(
        "p1",
        [
            _track(1, id="pt-1", track_name="First"),
            _track(
                2,
                id="pt-2",
                track_name="Stream",
                source_type="navidrome",
                library_file_id=None,
            ),
            _track(3, id="pt-3", track_name="Third"),
        ],
        position=None,
        changed_at="t",
    )

    rows = await store.list_target_playlist_export_rows("p1")

    assert [row["track_name"] for row in rows] == ["First", "Stream", "Third"]
    assert rows[0]["file_path"] == "/music/track-1.flac"
    assert rows[1]["file_path"] is None
    assert rows[2]["file_path"] == "/music/track-1.flac"


@pytest.mark.asyncio
async def test_export_rows_constructible_twice_on_same_path(
    tmp_path: Path,
) -> None:
    """Idempotency ratchet: the new query adds no schema, so a second store
    on the same file must work unchanged."""
    path = tmp_path / "library.db"
    first = NativeLibraryStore(path, threading.Lock())
    await first.create_target_playlist(
        playlist_id="p1", name="Mix", source_ref=None, user_id="u1", created_at="t"
    )
    second = NativeLibraryStore(path, threading.Lock())

    assert await second.list_target_playlist_export_rows("p1") == []
