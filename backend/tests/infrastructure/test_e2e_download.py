"""E2E gate: search -> match -> enqueue -> poll -> process -> import -> library_files row.

A stub download client returns realistic results that the real scorer/matcher score;
everything else (DownloadStore, LibraryDB, AudioTagger, FileProcessor, orchestrator) is
real. The real-slskd container E2E is the Phase 9 gate (task-059).
"""

import shutil
import sqlite3
import threading
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from infrastructure.audio.tagger import AudioTagger
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.library_db import LibraryDB
from infrastructure.persistence.request_history import RequestHistoryStore
from infrastructure.sse_publisher import SSEPublisher
from models.common import ServiceStatus
from models.download import DownloadSearchResult
from models.download_manifest import ManifestCodec
from repositories.protocols.download_client import (
    DownloadMaterialization,
    DownloadTaskStatus,
    MountDiagnosis,
    TaskHandle,
)
from repositories.protocols.indexer import IndexerResult
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.acquisition_cleanup_service import AcquisitionCleanupService
from services.native.download_orchestrator import DownloadOrchestrator
from services.native.download_service import DownloadService
from services.native.file_processor import FileProcessor
from services.native.library_manager import LibraryManager
from services.native.naming import NamingTemplateEngine
from services.native.track_matcher import TrackMatcher
from services.request_service import RequestService
from tests.helpers import make_builtin_dispatcher, make_test_import_publisher

FIXTURE_FLAC = (
    Path(__file__).resolve().parent.parent
    / "fixtures"
    / "library"
    / "flac_full_01.flac"
)
_TEMPLATE = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)"
        )
        conn.execute(
            "INSERT OR IGNORE INTO auth_users (id, username, role) VALUES (?, ?, ?)",
            ("user-a", "alice", "admin"),
        )
        conn.commit()
    finally:
        conn.close()


class _StubIndexer:
    """Search half of the split (D2): returns the placed fixtures as soulseek
    ``IndexerResult``s, which the orchestrator/service unwrap before scoring."""

    def __init__(self, album=None, track=None) -> None:
        self._album = album or []
        self._track = track or []

    @property
    def indexer_name(self) -> str:
        return "soulseek"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def search_album(
        self, artist, album, year=None, track_count=None, *, timeout=30.0
    ):
        return [IndexerResult(source="soulseek", soulseek=r) for r in self._album]

    async def search_track(
        self, artist, track, album=None, duration_seconds=None, *, timeout=30.0
    ):
        return [IndexerResult(source="soulseek", soulseek=r) for r in self._track]


class _StubClient:
    def __init__(self, downloads_root: Path) -> None:
        self._root = downloads_root
        self.discarded: list[TaskHandle] = []

    @property
    def client_name(self) -> str:
        return "stub"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def enqueue(self, request) -> TaskHandle:
        files = request.files
        return TaskHandle(
            source="soulseek",
            username=files[0].username,
            filenames=[f.filename for f in files],
        )

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        n = len(handle.filenames)
        return DownloadTaskStatus(
            task_id="",
            status="completed",
            files_total=n,
            files_completed=n,
            bytes_total=0,
            bytes_downloaded=0,
            progress_percent=100.0,
        )

    async def abort(self, handle: TaskHandle) -> bool:
        return True

    async def inspect_materialization(
        self, handle: TaskHandle
    ) -> DownloadMaterialization:
        return DownloadMaterialization(
            state="completed",
            mount_root=str(self._root),
            file_paths=[
                str(self._root / value.replace("\\", "/").lstrip("/"))
                for value in handle.filenames
            ],
            mount_healthy=True,
        )

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        self.discarded.append(handle)
        return True

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        return [self._root / f.replace("\\", "/").lstrip("/") for f in handle.filenames]

    async def get_file_path(
        self, handle: TaskHandle, remote_filename: str, size: int | None = None
    ):
        return self._root / remote_filename.replace("\\", "/").lstrip("/")

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        return MountDiagnosis(supported=False)


class _CleanupLibrary:
    async def get_library_management_import_bundle(self, bundle_id: str):
        return MagicMock(state="completed")

    async def list_acquisition_import_bundles_for_download_task(self, task_id: str):
        return []


def _place_fixture(downloads_root: Path, rel: str) -> DownloadSearchResult:
    dest = downloads_root / rel
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(FIXTURE_FLAC, dest)
    parent = rel.rsplit("/", 1)[0]
    return DownloadSearchResult(
        username="peer1",
        filename=rel,
        parent_directory=parent,
        size=dest.stat().st_size,
        extension="flac",
        bitrate=1000,
        has_free_slot=True,
        upload_speed=5_000_000,
    )


def _build(tmp_path: Path, *, album=None, track=None):
    downloads = tmp_path / "slskd_downloads"
    library = tmp_path / "library"
    staging = tmp_path / "staging"
    for p in (downloads, library, staging):
        p.mkdir(parents=True, exist_ok=True)

    db_path = tmp_path / "library.db"
    lock = threading.Lock()
    store = DownloadStore(db_path=db_path, write_lock=lock)
    _seed_auth_users(db_path)
    library_db = LibraryDB(
        db_path=tmp_path / "library_files.db", write_lock=threading.Lock()
    )
    manager = LibraryManager(library_db)
    client = _StubClient(downloads)
    indexer = _StubIndexer(album=album, track=track)

    fp = FileProcessor(
        AudioTagger(),
        naming_engine=NamingTemplateEngine(),
        library_manager=manager,
        library_paths=[library],
        client=client,
        slskd_downloads_path=downloads,
        fingerprinter=None,
        verify_downloads=False,
        library_root_ids=["root-a"],
        publish_import_bundle=make_test_import_publisher(manager, {"root-a": library}),
        policy_revision_getter=lambda: "test-policy",
    )
    cleanup = AcquisitionCleanupService(
        store, _CleanupLibrary(), lambda source: client, lambda: downloads
    )
    orch = DownloadOrchestrator(
        client=client,
        indexer=indexer,
        download_store=store,
        file_processor=fp,
        library_manager=manager,
        scorer=AlbumPreflightScorer(store, quality_min="low", flac_mp3_only=False),
        track_matcher=TrackMatcher(store, quality_min="low", flac_mp3_only=False),
        manifest_codec=ManifestCodec(),
        event_bus=SSEPublisher(),
        staging_path=staging,
        naming_template=_TEMPLATE,
        poll_interval=0.0,
        auto_accept_threshold=0.5,
        manual_threshold=0.1,
        cleanup_service=cleanup,
    )
    return store, manager, orch, client, library


@pytest.mark.asyncio
async def test_full_download_to_library(tmp_path: Path):
    album = [
        _place_fixture(
            tmp_path / "slskd_downloads", "Radiohead - OK Computer/01 Airbag.flac"
        )
    ]
    store, manager, orch, client, library = _build(tmp_path, album=album)

    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final is not None
    assert final.status == "completed"
    assert await manager.has_album("rg-okc") is True
    flacs = list(library.rglob("*.flac"))
    assert len(flacs) == 1
    tag, _info = AudioTagger().read_tags(flacs[0])
    assert tag.album == "OK Computer"
    assert tag.musicbrainz_release_group_id == "rg-okc"
    # successful import clears the slskd transfer records
    assert len(client.discarded) == 1


@pytest.mark.asyncio
async def test_track_request_to_library(tmp_path: Path):
    track = [
        _place_fixture(
            tmp_path / "slskd_downloads", "Radiohead - OK Computer/Airbag.flac"
        )
    ]
    store, manager, orch, client, library = _build(tmp_path, track=track)

    task = await store.create_task(
        user_id="user-a",
        download_type="track",
        release_group_mbid="rg-okc-track",
        recording_mbid="rec-airbag",
        artist_name="Radiohead",
        album_title="OK Computer",
        track_title="Airbag",
        year=1997,
        track_count=1,
    )

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final is not None
    assert final.status == "completed"
    flacs = list(library.rglob("*.flac"))
    assert len(flacs) == 1
    assert await manager.has_album("rg-okc-track") is True


@pytest.mark.asyncio
async def test_request_links_download_task_id(tmp_path: Path):
    """RequestService auto-approve creates a real download task and links its id to
    request_history. Orchestrator dispatch is stubbed for determinism."""
    album = [
        _place_fixture(
            tmp_path / "slskd_downloads", "Radiohead - OK Computer/01 Airbag.flac"
        )
    ]
    store, manager, _orch, client, _library = _build(tmp_path, album=album)

    history = RequestHistoryStore(
        db_path=tmp_path / "library.db", write_lock=threading.Lock()
    )
    no_op_orch = MagicMock()
    no_op_orch.dispatch = MagicMock()
    download_service = DownloadService(
        client,
        _StubIndexer(),
        AlbumPreflightScorer(store, quality_min="low", flac_mp3_only=False),
        manager,
        store,
        SSEPublisher(),
        no_op_orch,
    )
    get_ds = lambda: download_service  # noqa: E731
    request_service = RequestService(
        history,
        get_download_service=get_ds,
        acquisition=make_builtin_dispatcher(get_ds),
    )

    resp = await request_service.request_album(
        "rg-okc",
        artist="Radiohead",
        album="OK Computer",
        year=1997,
        user_id="user-a",
        user_role="admin",
    )
    assert resp.success is True

    record = await history.async_get_record("rg-okc")
    assert record is not None
    assert record.download_task_id is not None
    linked = await store.get_task(record.download_task_id)
    assert linked is not None
    assert linked.release_group_mbid == "rg-okc"
    no_op_orch.dispatch.assert_called_once()
