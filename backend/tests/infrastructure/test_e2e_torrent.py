"""E2E torrent pipeline: Soulseek finds nothing acceptable -> torrent fallback ->
Prowlarr search -> qBittorrent add (tag correlation) -> poll -> seeding-safe COPY
import (D18) into library_files. Plus the failure policies: a dead torrent is
blocklisted with NO propagation leniency (torrents don't propagate), while local
faults (import failure, unreachable mount) never blocklist or delete.

Unlike the Usenet e2e (which fakes at the DownloadClientProtocol level), this fakes
ONLY the HTTP wire via httpx.MockTransport, speaking the documented provider
formats: Prowlarr's /api/v1/search JSON (camelCase ReleaseResource rows, X-Api-Key
auth) and qBittorrent's Web API v2 (Bearer API key, torrents/add returning no hash
-> tag correlation, torrents/info state/progress/content_path/save_path,
torrents/delete). Everything above the wire is real: ProwlarrClient/Indexer,
QbittorrentClient/DownloadClient (state mapping, save_path->mount remap, the
never-delete-a-completed-torrent rule), TorrentStrategy (copy-for-import), the
orchestrator, and the folder import against re-tagged copies of the test FLAC."""

import shutil
import sqlite3
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from urllib.parse import parse_qs

import httpx
import pytest
from mutagen.flac import FLAC

from infrastructure.audio.tagger import AudioTagger
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.library_db import LibraryDB
from infrastructure.sse_publisher import SSEPublisher
from models.common import ServiceStatus
from models.download_identity import usenet_identity
from models.download_manifest import ManifestCodec
from repositories.prowlarr.prowlarr_client import ProwlarrClient
from repositories.prowlarr.prowlarr_indexer import ProwlarrIndexer
from repositories.qbittorrent.qbittorrent_client import QbittorrentClient
from repositories.qbittorrent.qbittorrent_download_client import QbittorrentDownloadClient
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.download_orchestrator import DownloadOrchestrator
from services.native.file_processor import FileProcessor
from services.native.library_manager import LibraryManager
from services.native.naming import NamingTemplateEngine
from services.native.torrent_release_scorer import TorrentReleaseScorer
from services.native.track_matcher import TrackMatcher

FIXTURE_FLAC = Path(__file__).resolve().parent.parent / "fixtures" / "library" / "flac_full_01.flac"
_TEMPLATE = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"

_TITLE = "Radiohead - OK Computer [FLAC]"
_SIZE = 600_000_000
# qBittorrent's OWN filesystem namespace; DroppedNeedle sees the same data at the
# mount passed to QbittorrentDownloadClient (the save_path-prefix remap under test).
_QBT_SAVE_PATH = "/downloads"
_API_KEY = "qbt_" + "k" * 28  # qBittorrent 5.2 API keys are qbt_-prefixed


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute("CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)")
        conn.execute("INSERT OR IGNORE INTO auth_users (id, username, role) VALUES (?, ?, ?)", ("user-a", "alice", "admin"))
        conn.commit()
    finally:
        conn.close()


def _place_track(folder: Path, filename: str, *, title: str, track: int) -> None:
    """Copy the fixture FLAC and re-tag it as a distinct track (the torrent payload)."""
    dest = folder / filename
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(FIXTURE_FLAC, dest)
    audio = FLAC(dest)
    audio["title"] = title
    audio["tracknumber"] = str(track)
    audio["album"] = "OK Computer"
    audio.save()


class _EmptySlskdIndexer:
    @property
    def indexer_name(self):
        return "soulseek"

    def is_configured(self):
        return True

    async def health_check(self):
        return ServiceStatus(status="ok")

    async def search_album(self, *a, **k):
        return []  # Soulseek finds nothing -> forces the torrent fallback

    async def search_track(self, *a, **k):
        return []


class _FakeQbtServer:
    """qBittorrent Web API v2 at the wire, per the documented behavior: Bearer API-key
    auth (5.2+), torrents/add returns NO hash (only "Ok."/"Fails.") so the client must
    correlate by tag, torrents/info supports tag=/hashes=/category= filters, and
    torrents/delete takes hashes + deleteFiles."""

    def __init__(self, *, add_state: str = "stalledUP", add_progress: float = 1.0,
                 content_path: str = f"{_QBT_SAVE_PATH}/{_TITLE}"):
        self._add_state = add_state
        self._add_progress = add_progress
        self._content_path = content_path
        self.rows: list[dict] = []
        self.add_calls: list[dict] = []
        self.delete_calls: list[dict] = []
        self.auth_headers: set[str] = set()

    def transport(self) -> httpx.MockTransport:
        return httpx.MockTransport(self._handle)

    def _handle(self, request: httpx.Request) -> httpx.Response:
        self.auth_headers.add(request.headers.get("Authorization", ""))
        path = request.url.path
        if path.endswith("/app/version"):
            return httpx.Response(200, text="v5.2.1")
        if path.endswith("/torrents/add"):
            form = {k: v[0] for k, v in parse_qs(request.content.decode()).items()}
            self.add_calls.append(form)
            self.rows.append({
                "hash": "a" * 40, "name": _TITLE, "state": self._add_state,
                "progress": self._add_progress, "size": _SIZE,
                "downloaded": int(_SIZE * self._add_progress), "dlspeed": 0,
                "num_seeds": 12, "category": form.get("category", ""),
                "tags": form.get("tags", ""), "content_path": self._content_path,
                "save_path": _QBT_SAVE_PATH,
            })
            return httpx.Response(200, text="Ok.")
        if path.endswith("/torrents/info"):
            params = request.url.params
            rows = self.rows
            if params.get("hashes"):
                wanted = set(params["hashes"].split("|"))
                rows = [r for r in rows if r["hash"] in wanted]
            if params.get("tag"):
                rows = [r for r in rows if params["tag"] in [t.strip() for t in r["tags"].split(",")]]
            if params.get("category"):
                rows = [r for r in rows if r["category"] == params["category"]]
            return httpx.Response(200, json=rows)
        if path.endswith("/torrents/delete"):
            form = {k: v[0] for k, v in parse_qs(request.content.decode()).items()}
            self.delete_calls.append(form)
            wanted = set(form.get("hashes", "").split("|"))
            self.rows = [r for r in self.rows if r["hash"] not in wanted]
            return httpx.Response(200, text="")
        return httpx.Response(404, text="")


class _FakeProwlarrServer:
    """Prowlarr /api/v1 at the wire: X-Api-Key auth; /search returns camelCase
    ReleaseResource rows tagged with their protocol (usenet AND torrent - the
    indexer must keep only the torrent arm here)."""

    def __init__(self, rows: list[dict]):
        self._rows = rows
        self.api_key_headers: set[str] = set()
        self.search_calls: list[dict] = []

    def transport(self) -> httpx.MockTransport:
        return httpx.MockTransport(self._handle)

    def _handle(self, request: httpx.Request) -> httpx.Response:
        self.api_key_headers.add(request.headers.get("X-Api-Key", ""))
        path = request.url.path
        if path.endswith("/system/status"):
            return httpx.Response(200, json={"version": "2.1.0", "appName": "Prowlarr"})
        if path.endswith("/search"):
            self.search_calls.append(dict(request.url.params))
            return httpx.Response(200, json=self._rows)
        return httpx.Response(404, text="")


def _torrent_row(*, publish_date: str, seeders: int = 50) -> dict:
    return {
        "guid": "prowlarr-guid-1", "title": _TITLE, "size": _SIZE,
        "indexerId": 7, "indexer": "PrivateHD", "protocol": "torrent",
        "downloadUrl": "/download/1?apikey=redacted", "magnetUrl": "magnet:?xt=urn:btih:" + "a" * 40,
        "infoHash": "a" * 40, "categories": [{"id": 3040, "name": "Audio/Lossless"}],
        "seeders": seeders, "leechers": 3, "grabs": 20, "publishDate": publish_date,
    }


def _usenet_row() -> dict:
    # A usenet-protocol sibling in the same Prowlarr response; the torrent arm must ignore it.
    return {
        "guid": "prowlarr-guid-2", "title": _TITLE, "size": _SIZE,
        "indexerId": 8, "indexer": "NZBIdx", "protocol": "usenet",
        "downloadUrl": "/download/2?apikey=redacted", "categories": [{"id": 3040, "name": "Audio/Lossless"}],
        "grabs": 100, "publishDate": "2020-01-01T00:00:00Z",
    }


def _album_service(tracks):
    svc = SimpleNamespace()

    async def get_album_tracks_info(rg):
        return SimpleNamespace(tracks=tracks, total_tracks=len(tracks))

    svc.get_album_tracks_info = get_album_tracks_info
    return svc


def _track(position, title, length_ms):
    return SimpleNamespace(position=position, title=title, disc_number=1, length=length_ms, recording_id=None)


def _build(tmp_path: Path, *, album_tracks, qbt: _FakeQbtServer,
           prowlarr_rows=None, mount: Path | None = None):
    library = tmp_path / "library"
    staging = tmp_path / "staging"
    for p in (library, staging):
        p.mkdir(parents=True, exist_ok=True)
    if mount is None:
        mount = tmp_path / "qbt-mount"
        mount.mkdir(parents=True, exist_ok=True)
    db_path = tmp_path / "library.db"
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    manager = LibraryManager(LibraryDB(db_path=tmp_path / "library_files.db", write_lock=threading.Lock()))

    qbt_client = QbittorrentClient(
        httpx.AsyncClient(transport=qbt.transport()), "http://qbt:8080", _API_KEY,
        retry_backoff=0.0,
    )
    qbt_dl = QbittorrentDownloadClient(
        qbt_client, "http://qbt:8080", _API_KEY, mount, category="droppedneedle"
    )
    if prowlarr_rows is None:
        prowlarr_rows = [_torrent_row(publish_date="2020-01-01T00:00:00Z"), _usenet_row()]
    prowlarr = _FakeProwlarrServer(prowlarr_rows)
    indexer = ProwlarrIndexer(
        ProwlarrClient(
            httpx.AsyncClient(transport=prowlarr.transport()), "http://prowlarr:9696",
            "prowlarr-key", retry_backoff=0.0,
        ),
        categories=[3000, 3040],
    )
    fp = FileProcessor(
        AudioTagger(), naming_engine=NamingTemplateEngine(), library_manager=manager,
        library_paths=[library], client=qbt_dl,
        slskd_downloads_path=tmp_path / "slskd", fingerprinter=None, verify_downloads=False,
    )
    orch = DownloadOrchestrator(
        client=_EmptySlskdIndexer(),  # placeholder; download-side not used for torrent
        indexer=_EmptySlskdIndexer(),
        download_store=store, file_processor=fp, library_manager=manager,
        scorer=AlbumPreflightScorer(store, quality_min="low", flac_mp3_only=False),
        track_matcher=TrackMatcher(store, quality_min="low", flac_mp3_only=False),
        manifest_codec=ManifestCodec(), event_bus=SSEPublisher(), staging_path=staging,
        naming_template=_TEMPLATE, poll_interval=0.0, auto_accept_threshold=0.5, manual_threshold=0.1,
        torrent_indexer=indexer, torrent_client=qbt_dl,
        torrent_scorer=TorrentReleaseScorer(store, quality_min="low", flac_mp3_only=False),
        torrent_enabled=True, album_service=_album_service(album_tracks),
        source_priority=["soulseek", "torrent"], usenet_import_settle_seconds=0.0,
    )
    return store, manager, orch, prowlarr, library, mount


async def _create_album_task(store, track_count: int):
    return await store.create_task(
        user_id="user-a", download_type="album", release_group_mbid="rg-okc",
        artist_name="Radiohead", album_title="OK Computer", year=1997, track_count=track_count,
    )


@pytest.mark.asyncio
async def test_torrent_fallback_album_imports_and_keeps_seeding(tmp_path: Path):
    qbt = _FakeQbtServer()  # torrent is complete on first poll (stalledUP = seeding)
    store, manager, orch, prowlarr, library, mount = _build(
        tmp_path, album_tracks=[_track(1, "Airbag", 300), _track(2, "Paranoid Android", 300)], qbt=qbt
    )
    payload = mount / _TITLE
    _place_track(payload, "01 Airbag.flac", title="Airbag", track=1)
    _place_track(payload, "02 Paranoid Android.flac", title="Paranoid Android", track=2)

    task = await _create_album_task(store, 2)
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    assert final.source == "torrent"  # routed to torrent after Soulseek found nothing
    assert final.download_client == "qbittorrent"
    assert len(list(library.rglob("*.flac"))) == 2  # both tracks imported
    # The seeding invariant: the import worked on COPIES - the torrent's payload is
    # untouched and the completed torrent was never deleted (post-import cleanup ran,
    # but the client's private-tracker rule left it seeding).
    assert sorted(p.name for p in payload.glob("*.flac")) == ["01 Airbag.flac", "02 Paranoid Android.flac"]
    assert qbt.delete_calls == []
    assert not (tmp_path / "staging" / task.id / "torrent-import").exists()  # scratch cleaned
    # Wire-level checks: category + correlation tag on the add; documented auth on both APIs.
    (add,) = qbt.add_calls
    assert add["category"] == "droppedneedle"
    assert add["tags"] == f"droppedneedle-{task.id}-0"
    assert "urls" in add
    assert qbt.auth_headers == {f"Bearer {_API_KEY}"}
    assert prowlarr.api_key_headers == {"prowlarr-key"}
    assert prowlarr.search_calls[0]["type"] == "search"


@pytest.mark.asyncio
async def test_torrent_per_track_imports_exactly_one(tmp_path: Path):
    # Mirror of Usenet D4: a per-track request grabs the ALBUM torrent but imports only
    # the requested track; the siblings stay behind (and keep seeding).
    qbt = _FakeQbtServer()
    store, manager, orch, prowlarr, library, mount = _build(
        tmp_path, album_tracks=[_track(2, "Paranoid Android", 300)], qbt=qbt
    )
    payload = mount / _TITLE
    _place_track(payload, "01 Airbag.flac", title="Airbag", track=1)
    _place_track(payload, "02 Paranoid Android.flac", title="Paranoid Android", track=2)

    task = await store.create_task(
        user_id="user-a", download_type="track", release_group_mbid="rg-okc",
        recording_mbid="rec-pa", artist_name="Radiohead", album_title="OK Computer",
        track_title="Paranoid Android", track_number=2, disc_number=1,
        track_duration_seconds=0.3, year=1997, track_count=12,
    )
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    flacs = list(library.rglob("*.flac"))
    assert len(flacs) == 1
    assert flacs[0].name.endswith("Paranoid Android.flac")
    assert len(list(payload.glob("*.flac"))) == 2  # payload intact, still seeding


@pytest.mark.asyncio
async def test_torrent_failed_release_blocklisted_even_when_young(tmp_path: Path):
    # qBittorrent reports the torrent errored. Unlike Usenet there is NO propagation
    # age guard - torrents don't propagate, a dead torrent stays dead - so even a
    # freshly-published release is blocklisted by its title+size identity.
    young = (datetime.now(timezone.utc) - timedelta(seconds=60)).isoformat()
    qbt = _FakeQbtServer(add_state="error", add_progress=0.4)
    store, manager, orch, prowlarr, library, mount = _build(
        tmp_path, album_tracks=[_track(1, "Airbag", 300)], qbt=qbt,
        prowlarr_rows=[_torrent_row(publish_date=young)],
    )

    task = await _create_album_task(store, 1)
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert ("torrent", usenet_identity(_TITLE, _SIZE)) in await store.load_quarantine_set()


@pytest.mark.asyncio
async def test_torrent_completed_under_delivery_blocklists_and_keeps_partial(tmp_path: Path):
    # The torrent COMPLETED but only delivers 1 of the album's 2 tracks. Keep what we
    # got, but blocklist the release (confirmed under-delivery, no age leniency) so
    # retry/failover finds a complete one - and still never delete the seeding torrent.
    qbt = _FakeQbtServer()
    store, manager, orch, prowlarr, library, mount = _build(
        tmp_path, album_tracks=[_track(1, "Airbag", 300), _track(2, "Paranoid Android", 300)], qbt=qbt
    )
    payload = mount / _TITLE
    _place_track(payload, "01 Airbag.flac", title="Airbag", track=1)  # only 1 of 2

    task = await _create_album_task(store, 2)
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "partial"
    assert len(list(library.rglob("*.flac"))) == 1  # the delivered track is kept
    assert ("torrent", usenet_identity(_TITLE, _SIZE)) in await store.load_quarantine_set()
    assert qbt.delete_calls == []  # under-delivery still never deletes the torrent


@pytest.mark.asyncio
async def test_torrent_import_fault_does_not_blocklist(tmp_path: Path):
    # The torrent delivered its files but writing into the library failed locally
    # (perms / cross-mount fault). That's OUR fault, not the release's (review H3).
    from services.native.file_processor import IMPORT_FAILED, FileFailure, ProcessResult

    qbt = _FakeQbtServer()
    store, manager, orch, prowlarr, library, mount = _build(
        tmp_path, album_tracks=[_track(1, "Airbag", 300)], qbt=qbt
    )
    payload = mount / _TITLE
    _place_track(payload, "01 Airbag.flac", title="Airbag", track=1)

    async def _import_fails(manifest, files):
        return ProcessResult(succeeded=[], failed=[FileFailure(filename="01 Airbag.flac", reason=IMPORT_FAILED)])

    orch._strategies["torrent"]._file_processor.process_downloaded_folder = _import_fails
    task = await _create_album_task(store, 1)
    await orch.process_task(task.id)
    assert await store.load_quarantine_set() == set()  # local import fault -> NOT blocklisted


@pytest.mark.asyncio
async def test_torrent_unreachable_mount_does_not_blocklist_or_delete(tmp_path: Path):
    # The downloads MOUNT is unreachable (environment fault): qBittorrent says the
    # torrent completed, but nothing is enumerable on our side. Must NOT blocklist the
    # release and NOT delete the client's data - and the error names the right client.
    qbt = _FakeQbtServer()
    store, manager, orch, prowlarr, library, mount = _build(
        tmp_path, album_tracks=[_track(1, "Airbag", 300)], qbt=qbt,
        mount=tmp_path / "missing-mount",  # never created -> mount root unreachable
    )

    task = await _create_album_task(store, 1)
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert "torrent client downloads mount" in (final.error_message or "")
    assert await store.load_quarantine_set() == set()  # good release NOT blocklisted
    assert qbt.delete_calls == []  # the seeding data was NOT deleted
