import asyncio
import os
import sqlite3
import threading
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from api.v1.schemas.library_policies import (
    LibraryPathPolicyRule,
    LibraryRootSettings,
    TypedLibrarySettings,
)
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.audio import AudioInfo, AudioTag
from models.library_work import ScanScope
from services.native.identification_queue_service import IdentificationQueueService
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.target_import_library_service import TargetImportLibraryService


def _seed_database(path: Path) -> None:
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute(
            "CREATE TABLE library_files (id INTEGER PRIMARY KEY, file_path TEXT)"
        )
        connection.execute(
            "INSERT INTO library_files(id, file_path) VALUES (1, '/legacy/sentinel.flac')"
        )


def _resolver(root: Path, *, root_id: str = "root-1") -> LibraryPolicyResolver:
    return LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id=root_id,
                    path=str(root),
                    label="Music",
                    policy="automatic",
                )
            ]
        )
    )


def _tag(
    *,
    title: str = "Track One",
    track_number: int = 1,
    release_track_id: str | None = None,
) -> AudioTag:
    return AudioTag(
        title=title,
        artist="Local Artist",
        album="Local Album",
        album_artist="Local Artist",
        track_number=track_number,
        disc_number=1,
        year=2026,
        genre="Test Genre",
        musicbrainz_release_track_id=release_track_id,
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


@pytest.fixture
def target(tmp_path: Path):
    db_path = tmp_path / "library.db"
    root = tmp_path / "Music_%_Root"
    root.mkdir()
    _seed_database(db_path)
    store = NativeLibraryStore(db_path, threading.Lock())
    resolver = _resolver(root)
    service = TargetImportLibraryService(
        store,
        lambda: resolver,
        IdentificationQueueService(store),
    )
    return db_path, root, store, service


@pytest.mark.asyncio
async def test_import_is_idempotent_and_writes_only_target_catalog(target) -> None:
    db_path, root, store, service = target
    audio = root / "Local Artist" / "Local Album" / "01 Track One.flac"
    audio.parent.mkdir(parents=True)
    audio.write_bytes(b"target-audio")

    first = await service.upsert_file(
        audio,
        _tag(release_track_id="release-track-1"),
        _info(audio.stat().st_size),
        release_group_mbid="release-group-1",
        release_mbid="release-1",
        recording_mbid="recording-1",
        source="download",
        download_task_id="task-1",
        source_path="peer/01 Track One.flac",
    )
    second = await service.upsert_file(
        audio,
        _tag(release_track_id="release-track-1"),
        _info(audio.stat().st_size),
        release_group_mbid="release-group-1",
        release_mbid="release-1",
        recording_mbid="recording-1",
        source="download",
        download_task_id="task-1",
        source_path="peer/01 Track One.flac",
    )

    assert first == second
    local_track = await store.get_local_track(first)
    assert local_track is not None
    assert local_track["embedded_release_track_mbid"] == "release-track-1"
    imported = await service.get_imported_file("task-1", "01 Track One.flac")
    assert imported is not None
    assert imported["id"] == first
    assert imported["provider_release_group_mbid"] is None
    assert imported["ingest_source"] == "download"
    assert await store.resolve_target_id("track", first) == first
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone()[0] == 1
        )
        assert (
            connection.execute("SELECT COUNT(*) FROM library_files").fetchone()[0] == 1
        )
        jobs = connection.execute(
            "SELECT kind, state FROM library_identification_jobs"
        ).fetchall()
    assert jobs == [("automatic", "queued")]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("policy", "with_identity", "availability", "job_kind"),
    [
        ("automatic", False, "indexed", "automatic"),
        ("local_metadata", True, "indexed", "post_processing"),
        ("local_metadata", False, "indexed", None),
        ("excluded", True, "excluded", None),
    ],
)
async def test_import_obeys_the_effective_directory_policy(
    tmp_path: Path,
    policy: str,
    with_identity: bool,
    availability: str,
    job_kind: str | None,
) -> None:
    db_path = tmp_path / "library.db"
    root = tmp_path / "music"
    album_dir = root / "Artist" / "Album"
    album_dir.mkdir(parents=True)
    _seed_database(db_path)
    store = NativeLibraryStore(db_path, threading.Lock())
    resolver = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-1",
                    path=str(root),
                    label="Music",
                    policy="automatic",
                    rules=[
                        LibraryPathPolicyRule(
                            id="album-rule",
                            relative_path="Artist/Album",
                            policy=policy,
                        )
                    ],
                )
            ]
        )
    )
    service = TargetImportLibraryService(
        store, lambda: resolver, IdentificationQueueService(store)
    )
    audio = album_dir / "01 Track.flac"
    audio.write_bytes(b"audio")

    track_id = await service.upsert_file(
        audio,
        _tag(),
        _info(5),
        release_group_mbid="rg-1" if with_identity else None,
    )

    assert (await store.get_target_track(track_id))["availability"] == availability
    with sqlite3.connect(db_path) as connection:
        jobs = connection.execute(
            "SELECT kind FROM library_identification_jobs"
        ).fetchall()
    assert jobs == ([] if job_kind is None else [(job_kind,)])


@pytest.mark.asyncio
async def test_album_grouping_and_import_provenance_refresh_are_stable(target) -> None:
    _db_path, root, store, service = target
    folder = root / "Local Artist" / "Local Album"
    folder.mkdir(parents=True)
    first_path = folder / "01 One.flac"
    second_path = folder / "02 Two.flac"
    first_path.write_bytes(b"one")
    second_path.write_bytes(b"two")

    first_id = await service.upsert_file(
        first_path,
        _tag(title="One", track_number=1),
        _info(3),
        release_group_mbid=None,
        source="drop",
        download_task_id="drop-1",
        source_path="incoming/one.flac",
    )
    second_id = await service.upsert_file(
        second_path,
        _tag(title="Two", track_number=2),
        _info(3),
        release_group_mbid=None,
        source="drop",
        download_task_id="drop-1",
        source_path="incoming/two.flac",
    )
    await service.upsert_file(
        first_path,
        _tag(title="One", track_number=1),
        _info(3),
        release_group_mbid=None,
        source="download",
        download_task_id="retry-2",
        source_path="retry/one.flac",
    )

    first = await store.get_target_track(first_id)
    second = await store.get_target_track(second_id)
    assert first is not None and second is not None
    assert first["release_group_mbid"] == second["release_group_mbid"]
    assert first["download_task_id"] == "retry-2"
    assert first["source_path"] == "retry/one.flac"
    assert first["ingest_source"] == "download"


@pytest.mark.asyncio
async def test_catalog_commit_survives_queue_failure(target) -> None:
    _db_path, root, store, _service = target
    audio = root / "failure.flac"
    audio.write_bytes(b"audio")
    resolver = _resolver(root)
    queue = AsyncMock()
    queue.enqueue_album.side_effect = RuntimeError("queue unavailable")
    service = TargetImportLibraryService(store, lambda: resolver, queue)

    with pytest.raises(RuntimeError, match="queue unavailable"):
        await service.upsert_file(
            audio,
            _tag(),
            _info(5),
            release_group_mbid=None,
            download_task_id="task-failed-enqueue",
            source_path="failure.flac",
        )

    assert await store.get_target_track_by_path(str(audio)) is not None


@pytest.mark.asyncio
async def test_import_waits_for_policy_transition_and_uses_the_new_policy(
    tmp_path: Path,
) -> None:
    db_path = tmp_path / "library.db"
    root = tmp_path / "music"
    root.mkdir()
    _seed_database(db_path)
    store = NativeLibraryStore(db_path, threading.Lock())
    current = _resolver(root)
    excluded = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-1",
                    path=str(root),
                    label="Music",
                    policy="excluded",
                )
            ]
        )
    )
    resolver = {"value": current}
    transition_lock = asyncio.Lock()
    service = TargetImportLibraryService(
        store,
        lambda: resolver["value"],
        IdentificationQueueService(store),
        policy_transition_lock=transition_lock,
    )
    audio = root / "track.flac"
    audio.write_bytes(b"audio")

    async with transition_lock:
        import_task = asyncio.create_task(
            service.upsert_file(audio, _tag(), _info(5), release_group_mbid=None)
        )
        await asyncio.sleep(0)
        assert not import_task.done()
        await store.prepare_policy_transition(
            previous_policy_revision=current.policy_revision,
            proposed_policy_revision=excluded.policy_revision,
            previous_settings_json="{}",
            proposed_settings_json="{}",
            scopes=[
                ScanScope(
                    root_id="root-1",
                    scope_id="root-1",
                    root_path=str(root),
                    relative_path=".",
                    effective_policy="excluded",
                    policy_revision=excluded.policy_revision,
                )
            ],
            prepared_at=1,
        )
        resolver["value"] = excluded
        await store.commit_policy_transition(
            proposed_policy_revision=excluded.policy_revision, updated_at=2
        )

    track_id = await import_task
    track = await store.get_target_track(track_id)
    assert track is not None
    assert track["applied_policy"] == "excluded"
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_identification_jobs"
            ).fetchone()[0]
            == 0
        )


@pytest.mark.asyncio
async def test_reconcile_walks_once_and_scopes_literal_wildcard_paths(
    target, monkeypatch
) -> None:
    _db_path, root, store, service = target
    present = root / "present.flac"
    missing = root / "missing.flac"
    sibling = root.parent / "Music_A_Root" / "outside.flac"
    present.write_bytes(b"present")
    missing.write_bytes(b"missing")
    sibling.parent.mkdir()
    sibling.write_bytes(b"outside")
    for path in (present, missing):
        await service.upsert_file(
            path,
            _tag(title=path.stem),
            _info(path.stat().st_size),
            release_group_mbid=None,
        )

    sibling_resolver = _resolver(sibling.parent, root_id="root-2")
    sibling_service = TargetImportLibraryService(
        store,
        lambda: sibling_resolver,
        IdentificationQueueService(store),
    )
    sibling_id = await sibling_service.upsert_file(
        sibling,
        _tag(title="Outside"),
        _info(sibling.stat().st_size),
        release_group_mbid=None,
    )
    missing.unlink()
    real_walk = os.walk
    walks = 0

    def counted_walk(path):
        nonlocal walks
        walks += 1
        return real_walk(path)

    monkeypatch.setattr(
        "services.native.target_import_library_service.os.walk", counted_walk
    )
    assert await service.reconcile_with_filesystem([root]) == 1
    assert walks == 1
    assert (await store.get_target_track_by_path(str(missing)))[
        "availability"
    ] == "missing"
    assert (await store.get_target_track(sibling_id))["availability"] == "indexed"
