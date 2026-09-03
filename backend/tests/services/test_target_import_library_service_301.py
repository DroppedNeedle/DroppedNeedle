"""Issue #301: provider-RG album reuse on import + delete-tolerant removal.

Single-owner import attaches to the existing album instead of minting a
duplicate, multi-owner import attaches to the oldest owner, and removing an
album whose files are already gone still cleans the catalog rows.
"""

import sqlite3
import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from api.v1.schemas.library_policies import (
    LibraryRootSettings,
    TypedLibrarySettings,
)
from core.exceptions import ResourceNotFoundError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.audio import AudioInfo, AudioTag
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
)
from services.local_files_service import LocalFilesService
from services.native.identification_queue_service import IdentificationQueueService
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.target_catalog_writer_service import TargetCatalogWriterService
from services.native.target_import_library_service import TargetImportLibraryService
from services.native.target_library_repository import TargetLibraryRepository
from services.native.target_native_library_service import TargetNativeLibraryService

RG = "11111111-2222-4333-8444-555555555555"


def _seed_database(path: Path) -> None:
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users(id) VALUES ('user-1')")
        connection.execute(
            "CREATE TABLE library_files (id INTEGER PRIMARY KEY, file_path TEXT)"
        )
        connection.execute(
            "INSERT INTO library_files(id, file_path) VALUES (1, '/legacy/sentinel.flac')"
        )


def _resolver(root: Path) -> LibraryPolicyResolver:
    return LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-1",
                    path=str(root),
                    label="Music",
                    policy="automatic",
                )
            ]
        )
    )


def _tag(album: str = "Seed Album", artist: str = "Seed Artist") -> AudioTag:
    return AudioTag(
        title="Seed Track",
        artist=artist,
        album=album,
        album_artist=artist,
        track_number=1,
        disc_number=1,
        year=2026,
        genre="Test Genre",
    )

def _info(size: int) -> AudioInfo:
    return AudioInfo(
        duration_seconds=181,
        bitrate=900,
        sample_rate=44_100,
        channels=2,
        file_format="flac",
        file_size_bytes=size,
        bit_depth=16,
    )


def _membership(
    album_id: str,
    track_id: str,
    *,
    created_at: float = 1000.0,
    file_path: str = "/music/seed/01.flac",
    title: str = "Seed Album",
    artist_name: str = "Seed Artist",
    policy_revision: str = "",
) -> CatalogMembership:
    artist = LocalArtist(
        id=f"artist-{album_id}",
        display_name=artist_name,
        folded_name=artist_name.casefold(),
        kind="person",
        created_at=created_at,
        updated_at=created_at,
    )
    album = LocalAlbum(
        id=album_id,
        root_id="root-1",
        grouping_key=f"root-1:seed-dir:{title.casefold()}:{artist_name.casefold()}",
        title=title,
        album_artist_id=artist.id,
        album_artist_name=artist_name,
        created_at=created_at,
        updated_at=created_at,
    )
    track = LocalTrack(
        id=track_id,
        local_album_id=album.id,
        root_id="root-1",
        file_path=file_path,
        relative_path=file_path.removeprefix("/music/"),
        path_hash=f"hash-{track_id}",
        file_size_bytes=100,
        file_mtime_ns=200,
        stat_revision=f"stat-{track_id}",
        title="Seed Track",
        artist_name=artist_name,
        album_title=title,
        album_artist_name=artist_name,
        file_format="flac",
        imported_at=created_at,
        desired_policy_revision=policy_revision,
        applied_policy_revision=policy_revision,
        applied_policy="automatic",
    )
    return CatalogMembership(
        album=album,
        artists=[artist],
        tracks=[track],
        track_credits={track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]},
    )


def _add_identity(db_path: Path, album_id: str, rg: str = RG) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'manual', 2)",
            (album_id, rg),
        )


def _setup(
    tmp_path: Path,
) -> tuple[Path, Path, NativeLibraryStore, TargetImportLibraryService, LibraryPolicyResolver]:
    db_path = tmp_path / "library.db"
    root = tmp_path / "Music"
    root.mkdir()
    _seed_database(db_path)
    store = NativeLibraryStore(db_path, threading.Lock())
    resolver = _resolver(root)
    service = TargetImportLibraryService(
        store, lambda: resolver, IdentificationQueueService(store)
    )
    return db_path, root, store, service, resolver


def _album_count(db_path: Path) -> int:
    with sqlite3.connect(db_path) as connection:
        return int(connection.execute("SELECT COUNT(*) FROM local_albums").fetchone()[0])


def _grouping_key(db_path: Path, album_id: str) -> str:
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT grouping_key FROM local_albums WHERE id = ?", (album_id,)
        ).fetchone()
    assert row is not None
    return str(row[0])


@pytest.mark.asyncio
async def test_single_owner_reuses_existing_album(tmp_path: Path) -> None:
    db_path, root, store, service, resolver = _setup(tmp_path)
    await store.create_catalog_membership(
        _membership("album-seed", "track-seed", policy_revision=resolver.policy_revision)
    )
    _add_identity(db_path, "album-seed")
    before_key = _grouping_key(db_path, "album-seed")

    # The old decider only matches local ids/aliases, never a provider RG.
    assert await store.resolve_target_id("album", RG) is None
    assert await store.find_import_reuse_album_id(RG) == "album-seed"

    # Same RG titles, but a DIFFERENT directory: pre-fix this minted album #2.
    audio = root / "Other Dir" / "01.flac"
    audio.parent.mkdir(parents=True)
    audio.write_bytes(b"audio")
    track_id = await service.upsert_file(
        audio, _tag(), _info(audio.stat().st_size), release_group_mbid=RG
    )

    assert _album_count(db_path) == 1
    track = await store.get_target_track(track_id)
    assert track is not None and track["local_album_id"] == "album-seed"
    assert _grouping_key(db_path, "album-seed") == before_key


@pytest.mark.asyncio
async def test_published_builder_reuses_existing_album(tmp_path: Path) -> None:
    db_path, root, store, service, resolver = _setup(tmp_path)
    await store.create_catalog_membership(
        _membership("album-seed", "track-seed", policy_revision=resolver.policy_revision)
    )
    _add_identity(db_path, "album-seed")

    audio = root / "Third Dir" / "02.flac"
    audio.parent.mkdir(parents=True)
    audio.write_bytes(b"audio")
    write = await service._build_published_import_write(
        audio,
        _tag(),
        _info(audio.stat().st_size),
        resolver=_resolver(root),
        release_group_mbid=RG,
        release_mbid=None,
        recording_mbid=None,
        source="download",
        download_task_id=None,
        source_path=None,
        file_mtime=None,
    )

    assert write.album.id == "album-seed"
    assert write.track.local_album_id == "album-seed"


@pytest.mark.asyncio
async def test_multi_owner_attaches_to_oldest_without_minting(tmp_path: Path) -> None:
    db_path, root, store, service, resolver = _setup(tmp_path)
    await store.create_catalog_membership(
        _membership("album-old", "track-old", created_at=1000.0,
                    file_path="/music/old/01.flac",
                    policy_revision=resolver.policy_revision)
    )
    await store.create_catalog_membership(
        _membership("album-new", "track-new", created_at=2000.0,
                    file_path="/music/new/01.flac",
                    policy_revision=resolver.policy_revision)
    )
    _add_identity(db_path, "album-old")
    _add_identity(db_path, "album-new")

    assert await store.find_import_reuse_album_id(RG) == "album-old"

    audio = root / "Fourth Dir" / "01.flac"
    audio.parent.mkdir(parents=True)
    audio.write_bytes(b"audio")
    track_id = await service.upsert_file(
        audio, _tag(), _info(audio.stat().st_size), release_group_mbid=RG
    )

    assert _album_count(db_path) == 2
    track = await store.get_target_track(track_id)
    assert track is not None and track["local_album_id"] == "album-old"


@pytest.mark.asyncio
async def test_remove_album_with_missing_files_still_cleans_catalog(
    tmp_path: Path,
) -> None:
    db_path, root, store, _service, resolver = _setup(tmp_path)
    await store.create_catalog_membership(
        _membership("album-del", "track-del", file_path=str(root / "gone.flac"),
                    policy_revision=resolver.policy_revision)
    )
    preferences = SimpleNamespace(
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[SimpleNamespace(path=str(root))]
        )
    )
    local_files = LocalFilesService(
        TargetLibraryRepository(store), preferences, AsyncMock()
    )
    writer = TargetCatalogWriterService(
        store, local_files, TargetNativeLibraryService(store)
    )

    # Bytes never existed on disk: no File-not-found 404, rows still cleaned.
    await writer.remove_album("album-del", actor_user_id="user-1", delete_files=True)
    row = await store.get_target_track("track-del")
    assert row is not None and row["availability"] == "missing"

    # Ghost retry once rows are already missing: still success, still no 404.
    await writer.remove_album("album-del", actor_user_id="user-1", delete_files=True)

    # Only a truly unknown album is a 404.
    with pytest.raises(ResourceNotFoundError):
        await writer.remove_album("no-such-album", actor_user_id="user-1", delete_files=True)


@pytest.mark.asyncio
async def test_remove_ghost_album_with_recycle_cleans_catalog(tmp_path: Path) -> None:
    db_path, root, store, _service, resolver = _setup(tmp_path)
    await store.create_catalog_membership(
        _membership("album-ghost", "track-ghost", file_path=str(root / "gone.flac"),
                    policy_revision=resolver.policy_revision)
    )
    preferences = SimpleNamespace(
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[SimpleNamespace(path=str(root))]
        )
    )
    local_files = LocalFilesService(
        TargetLibraryRepository(store), preferences, AsyncMock()
    )
    writer = TargetCatalogWriterService(
        store, local_files, TargetNativeLibraryService(store)
    )

    # All bytes absent: recycling has nothing to move, catalog rows still go.
    # The first removal marks the row missing (no indexed rows remain); the
    # retry with recycle_files exercises the ghost-recycle fallback.
    await writer.remove_album("album-ghost", actor_user_id="user-1", delete_files=True)
    await writer.remove_album(
        "album-ghost", actor_user_id="user-1", delete_files=False, recycle_files=True
    )
    row = await store.get_target_track("track-ghost")
    assert row is not None and row["availability"] == "missing"
