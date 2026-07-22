"""Security: no slskd API key, bearer token, or other credential reaches a log line.

Two angles:
- The HTTP layer (``SlskdClient``) carries the key in an ``X-API-Key`` header; drive
  a full request cycle against the slskd mock with a sentinel key and assert it never
  appears in captured logs.
- The orchestrator import pipeline runs end to end (stub client + real scorer/processor)
  and we assert (a) no sentinel/credential string is logged and (b) the named structured
  events (Task 056) actually fire with their ``extra`` fields.
"""

import logging
import shutil
import sqlite3
import threading
from pathlib import Path

import httpx
import pytest

from infrastructure.audio.tagger import AudioTagger
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.library_db import LibraryDB
from infrastructure.sse_publisher import SSEPublisher
from models.common import ServiceStatus
from models.download import DownloadSearchResult
from models.download_manifest import ManifestCodec
from repositories.protocols.download_client import (
    DownloadTaskStatus,
    MountDiagnosis,
    TaskHandle,
)
from repositories.protocols.indexer import IndexerResult
from repositories.slskd.slskd_client import SlskdClient
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.download_orchestrator import DownloadOrchestrator
from services.native.file_processor import FileProcessor
from services.native.library_manager import LibraryManager
from services.native.naming import NamingTemplateEngine
from services.native.track_matcher import TrackMatcher
from tests.mocks import slskd_mock

SENTINEL_KEY = "TEST_KEY_DO_NOT_LEAK_12345"
FIXTURE_FLAC = Path(__file__).resolve().parent.parent / "fixtures" / "library" / "flac_full_01.flac"
_TEMPLATE = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"


@pytest.mark.asyncio
async def test_slskd_http_layer_never_logs_api_key(caplog):
    """A full slskd request cycle with a sentinel key leaves no trace of it in logs."""
    slskd_mock.reset_state()
    transport = httpx.ASGITransport(app=slskd_mock.app)
    http = httpx.AsyncClient(transport=transport)
    client = SlskdClient(http, "http://slskd", SENTINEL_KEY)

    with caplog.at_level(logging.DEBUG):
        await client.health_check()
        search = await client.start_search("Radiohead - OK Computer", timeout_seconds=5)
        await client.get_search_responses(search.id)
        await client.enqueue("alice", [{"filename": "dir/a.flac", "size": 100}])
        await client.get_downloads("alice")
    await http.aclose()

    assert SENTINEL_KEY not in caplog.text
    assert "X-API-Key" not in caplog.text
    assert "Bearer " not in caplog.text


class _StubIndexer:
    """Canned soulseek search results (the search half of the split, D2)."""

    def __init__(self, album: list) -> None:
        self._album = album

    @property
    def indexer_name(self) -> str:
        return "soulseek"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def search_album(self, artist, album, year=None, track_count=None, *, timeout=30.0):
        return [IndexerResult(source="soulseek", soulseek=r) for r in self._album]

    async def search_track(self, artist, track, album=None, duration_seconds=None, *, timeout=30.0):
        return []


class _StubClient:
    """Fixture files served from a temp downloads dir."""

    def __init__(self, downloads_root: Path) -> None:
        self._root = downloads_root

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
            task_id="", status="completed", files_total=n, files_completed=n,
            bytes_total=0, bytes_downloaded=0, progress_percent=100.0,
            # the orchestrator imports only these (mirrors the real repository)
            succeeded_filenames=list(handle.filenames),
        )

    async def cancel(self, handle: TaskHandle) -> bool:
        return True

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        return [self._root / f.replace("\\", "/").lstrip("/") for f in handle.filenames]

    async def get_file_path(self, handle: TaskHandle, remote_filename: str, size: int | None = None):
        return self._root / remote_filename.replace("\\", "/").lstrip("/")

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        return MountDiagnosis(supported=False)


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


def _place_fixture(downloads_root: Path, rel: str) -> DownloadSearchResult:
    dest = downloads_root / rel
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(FIXTURE_FLAC, dest)
    return DownloadSearchResult(
        username="peer1",
        filename=rel,
        parent_directory=rel.rsplit("/", 1)[0],
        size=dest.stat().st_size,
        extension="flac",
        bitrate=1000,
        has_free_slot=True,
        upload_speed=5_000_000,
    )


async def _run_full_download(tmp_path: Path) -> tuple[DownloadStore, str]:
    downloads = tmp_path / "slskd_downloads"
    library = tmp_path / "library"
    staging = tmp_path / "staging"
    for p in (downloads, library, staging):
        p.mkdir(parents=True, exist_ok=True)

    album = [_place_fixture(downloads, "Radiohead - OK Computer/01 Airbag.flac")]
    db_path = tmp_path / "library.db"
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    library_db = LibraryDB(db_path=tmp_path / "library_files.db", write_lock=threading.Lock())
    manager = LibraryManager(library_db)
    client = _StubClient(downloads)
    indexer = _StubIndexer(album)
    fp = FileProcessor(
        AudioTagger(), naming_engine=NamingTemplateEngine(), library_manager=manager,
        library_paths=[library], client=client, slskd_downloads_path=downloads,
        fingerprinter=None, verify_downloads=False,
    )
    orch = DownloadOrchestrator(
        client=client, indexer=indexer, download_store=store, file_processor=fp, library_manager=manager,
        scorer=AlbumPreflightScorer(store, quality_min="low", flac_mp3_only=False),
        track_matcher=TrackMatcher(store, quality_min="low", flac_mp3_only=False),
        manifest_codec=ManifestCodec(), event_bus=SSEPublisher(), staging_path=staging,
        naming_template=_TEMPLATE, poll_interval=0.0,
        auto_accept_threshold=0.5, manual_threshold=0.1,
    )
    task = await store.create_task(
        user_id="user-a", download_type="album", release_group_mbid="rg-okc",
        artist_name="Radiohead", album_title="OK Computer", year=1997, track_count=1,
    )
    await orch.process_task(task.id)
    return store, task.id


@pytest.mark.asyncio
async def test_full_download_cycle_logs_no_credentials(tmp_path, caplog):
    """A complete download->import emits no credential and no raw auth header."""
    with caplog.at_level(logging.DEBUG):
        store, task_id = await _run_full_download(tmp_path)

    final = await store.get_task(task_id)
    assert final is not None and final.status == "completed"
    assert SENTINEL_KEY not in caplog.text
    assert "Bearer " not in caplog.text
    assert "X-API-Key" not in caplog.text
    # No structured extra value should carry a credential either.
    for record in caplog.records:
        assert SENTINEL_KEY not in str(record.__dict__)


@pytest.mark.asyncio
async def test_full_download_cycle_fires_structured_events(tmp_path, caplog):
    """Task 056: the named lifecycle events fire with their structured extra fields."""
    with caplog.at_level(logging.DEBUG):
        store, task_id = await _run_full_download(tmp_path)

    events = {r.getMessage() for r in caplog.records}
    for expected in (
        "download.started",
        "download.search.completed",
        "download.enqueued",
        "download.processing",
        "download.completed",
        "process.completed",
        "preflight.ranked",
    ):
        assert expected in events, f"missing structured event: {expected}"

    enqueued = next(r for r in caplog.records if r.getMessage() == "download.enqueued")
    assert enqueued.task_id == task_id
    assert enqueued.files_total >= 1
    started = next(r for r in caplog.records if r.getMessage() == "download.started")
    assert started.user_id == "user-a"
    assert started.release_group_mbid == "rg-okc"
