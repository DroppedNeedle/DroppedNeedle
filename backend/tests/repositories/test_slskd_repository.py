"""SlskdRepository tests: protocol conformance, semaphore enforcement,
slskd JSON -> DownloadSearchResult translation, query building, path parsing,
and (username, filenames) status/cancel correlation."""

import asyncio
import threading
from pathlib import Path
from unittest.mock import AsyncMock

import httpx
import pytest

from repositories.protocols.download_client import (
    DownloadClientProtocol,
    DownloadFileRef,
    TaskRef,
)
from repositories.slskd.slskd_client import SlskdClient
from repositories.slskd.slskd_models import (
    SlskdDirectories,
    SlskdEnqueueResponse,
    SlskdFile,
    SlskdOptions,
    SlskdSearchResponse,
    SlskdTransfer,
    SlskdUserSearchResponse,
)
from repositories.slskd.slskd_repository import SlskdRepository
from tests.mocks import slskd_mock


class _ConcFake:
    """Concurrency-tracking fake SlskdClient for semaphore tests."""

    def __init__(self) -> None:
        self.search_active = 0
        self.search_max = 0
        self.enqueue_active = 0
        self.enqueue_max = 0

    async def start_search(self, query, *, timeout_seconds=30.0):
        self.search_active += 1
        self.search_max = max(self.search_max, self.search_active)
        await asyncio.sleep(0.05)
        self.search_active -= 1
        return SlskdSearchResponse(id="s", is_complete=True)

    async def get_search_state(self, search_id):
        return SlskdSearchResponse(id=search_id, is_complete=True)

    async def get_search_responses(self, search_id):
        return []

    async def enqueue(self, username, files):
        self.enqueue_active += 1
        self.enqueue_max = max(self.enqueue_max, self.enqueue_active)
        await asyncio.sleep(0.05)
        self.enqueue_active -= 1
        return SlskdEnqueueResponse()

    async def get_downloads(self, username):
        return []

    async def cancel_transfer(self, username, transfer_id):
        return True

    async def health_check(self):
        return {"version": {"current": "0.25.1.0"}}


@pytest.fixture
def mock_repo() -> SlskdRepository:
    slskd_mock.reset_state()
    http = httpx.AsyncClient(transport=httpx.ASGITransport(app=slskd_mock.app))
    client = SlskdClient(http, "http://slskd", "test-key")
    return SlskdRepository(
        client=client, url="http://slskd", api_key="test-key", downloads_mount=Path("/dl")
    )


def test_is_instance_of_protocol(mock_repo):
    assert isinstance(mock_repo, DownloadClientProtocol)
    assert mock_repo.is_configured() is True


def test_is_configured_requires_url_and_key():
    repo = SlskdRepository(
        client=None, url="", api_key="", downloads_mount=Path("/dl")
    )
    assert repo.is_configured() is False


@pytest.mark.asyncio
async def test_search_semaphore_serializes():
    fake = _ConcFake()
    repo = SlskdRepository(
        client=fake, url="u", api_key="k", downloads_mount=Path("/dl"), concurrent_searches=1
    )
    await asyncio.gather(*(repo.search_album("A", f"Album {i}") for i in range(5)))
    assert fake.search_max == 1


@pytest.mark.asyncio
async def test_enqueue_semaphore_serializes_and_returns_taskref():
    fake = _ConcFake()
    repo = SlskdRepository(
        client=fake, url="u", api_key="k", downloads_mount=Path("/dl"), concurrent_enqueues=1
    )
    refs = await asyncio.gather(
        *(
            repo.enqueue([DownloadFileRef(username="alice", filename=f"f{i}.flac", size=10)])
            for i in range(5)
        )
    )
    assert fake.enqueue_max == 1
    assert all(isinstance(r, TaskRef) for r in refs)
    assert refs[0].username == "alice"
    assert refs[0].filenames == ["f0.flac"]


class _LaggedCompletionFake:
    """slskd surfaces /responses ONLY after the search completes, and completion
    lands AFTER the searchTimeout window. This fake reproduces that: the first
    `incomplete_polls` state checks report in-progress, then it completes."""

    def __init__(self, incomplete_polls, files):
        self._incomplete_polls = incomplete_polls
        self._files = files
        self.state_calls = 0

    async def start_search(self, query, *, timeout_seconds=30.0):
        return SlskdSearchResponse(id="s", is_complete=False)

    async def get_search_state(self, search_id):
        self.state_calls += 1
        return SlskdSearchResponse(id=search_id, is_complete=self.state_calls > self._incomplete_polls)

    async def get_search_responses(self, search_id):
        return self._files


@pytest.mark.asyncio
async def test_search_waits_for_completion_past_the_search_window():
    # Regression: slskd returns results only once the search completes, which is
    # AFTER the searchTimeout window. The old code used `timeout` for BOTH the
    # search window and the poll deadline, so it always gave up before slskd
    # surfaced anything -> 0 results on every search.
    files = [
        SlskdUserSearchResponse(
            username="alice",
            file_count=1,
            files=[SlskdFile(filename="Alice\\Album\\01.flac", size=1000)],
        )
    ]
    fake = _LaggedCompletionFake(incomplete_polls=2, files=files)
    repo = SlskdRepository(client=fake, url="u", api_key="k", downloads_mount=Path("/dl"))
    repo._COMPLETION_GRACE_SECONDS = 5.0  # bound the test if the fix regresses
    # timeout=0 -> the OLD deadline (now + timeout) would poll zero times. The fix
    # keeps polling through the grace window until the lagged completion.
    results = await repo.search_album("A", "Album", timeout=0.0)
    assert len(results) == 1
    assert results[0].username == "alice"
    assert fake.state_calls > 2  # proves it polled past the in-progress window


class _LadderFake:
    """Returns canned responses only for specific query strings, recording the
    order queries were tried - to test the escalating broad-search fallback."""

    def __init__(self, results_by_query):
        self._by_query = results_by_query
        self.queries = []
        self._last = ""

    async def start_search(self, query, *, timeout_seconds=30.0):
        self.queries.append(query)
        self._last = query
        return SlskdSearchResponse(id="s", is_complete=True)

    async def get_search_state(self, search_id):
        return SlskdSearchResponse(id=search_id, is_complete=True)

    async def get_search_responses(self, search_id):
        return self._by_query.get(self._last, [])


def _one_file(username="bob", filename="bob\\Folder\\01.flac"):
    return [SlskdUserSearchResponse(username=username, file_count=1, files=[SlskdFile(filename=filename, size=1)])]


def test_album_query_ladder_escalates_and_dedupes():
    with_year = SlskdRepository._album_query_ladder("Artist", "Album", 2000)
    assert with_year == ["Artist Album 2000", "Artist Album", "Artist"]
    # year=None collapses the first two rungs into one.
    no_year = SlskdRepository._album_query_ladder("Artist", "Album", None)
    assert no_year == ["Artist Album", "Artist"]


@pytest.mark.asyncio
async def test_search_album_stops_at_first_nonempty():
    q0 = SlskdRepository._build_album_query("Artist", "Album", 2000)
    fake = _LadderFake({q0: _one_file()})
    repo = SlskdRepository(client=fake, url="u", api_key="k", downloads_mount=Path("/dl"))
    results = await repo.search_album("Artist", "Album", 2000)
    assert len(results) == 1
    assert fake.queries == [q0]  # first rung had results -> no wasted broad search


@pytest.mark.asyncio
async def test_search_album_falls_back_to_broad_query_on_empty():
    # Only the broadest (artist-only) rung has results - the Aphex Twin case.
    fake = _LadderFake({"Aphex Twin": _one_file(filename="bob\\Aphex Twin\\SAW 85-92\\01.flac")})
    repo = SlskdRepository(client=fake, url="u", api_key="k", downloads_mount=Path("/dl"))
    results = await repo.search_album("Aphex Twin", "Selected Ambient Works 85-92", 1992)
    assert len(results) == 1
    assert fake.queries[0] == SlskdRepository._build_album_query("Aphex Twin", "Selected Ambient Works 85-92", 1992)
    assert fake.queries[-1] == "Aphex Twin"  # escalated all the way to artist-only
    assert len(fake.queries) == 3


@pytest.mark.asyncio
async def test_search_album_empty_when_whole_ladder_empty():
    fake = _LadderFake({})
    repo = SlskdRepository(client=fake, url="u", api_key="k", downloads_mount=Path("/dl"))
    assert await repo.search_album("Nobody", "Nothing", 1999) == []
    assert len(fake.queries) == 3  # exhausted the ladder before giving up


@pytest.mark.asyncio
async def test_search_track_keeps_title_and_does_not_go_artist_only():
    fake = _LadderFake({})
    repo = SlskdRepository(client=fake, url="u", api_key="k", downloads_mount=Path("/dl"))
    await repo.search_track("Artist", "Song", "Album")
    # two rungs, both keep the track title; never falls back to artist-only.
    assert fake.queries == ["Artist Song Album", "Artist Song"]


@pytest.mark.asyncio
async def test_search_album_translates_results(mock_repo):
    results = await mock_repo.search_album("Radiohead", "OK Computer", 1997, 12)
    alice = [r for r in results if r.username == "alice"]
    assert len(alice) == 12
    # extension parsed from filename even though slskd's field is empty (C6a).
    assert all(r.extension == "flac" for r in alice)
    # parent_directory parsed from a Windows-style path.
    assert alice[0].parent_directory == "Radiohead - OK Computer (1997)"
    # lossless: bitrate left None (C6b).
    assert alice[0].bitrate is None


@pytest.mark.asyncio
async def test_get_status_correlates_by_filename(mock_repo):
    ref = await mock_repo.enqueue(
        [DownloadFileRef(username="alice", filename="dir/a.flac", size=100)]
    )
    status = await mock_repo.get_status(ref)
    assert status.files_total == 1
    assert status.files_completed == 1
    assert status.status == "completed"


@pytest.mark.asyncio
async def test_cancel_removes_matching_transfers(mock_repo):
    ref = await mock_repo.enqueue(
        [DownloadFileRef(username="alice", filename="dir/a.flac", size=100)]
    )
    assert await mock_repo.cancel(ref) is True
    status = await mock_repo.get_status(ref)
    assert status.files_completed == 0


@pytest.mark.asyncio
async def test_get_file_path_resolves_slskd_leaf_folder_layout(tmp_path):
    # slskd writes {downloads}/{leaf remote folder}/{filename}; the remote filename
    # carries the FULL peer path but only the leaf folder survives on disk.
    leaf = tmp_path / "The Marías"
    leaf.mkdir()
    f = leaf / "The Marías - Nobody New.mp3"
    f.write_bytes(b"x")
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    path = await repo.get_file_path(
        "alice", "@@peer\\Music\\The Marías\\The Marías - Nobody New.mp3"
    )
    assert path == f.resolve()


@pytest.mark.asyncio
async def test_get_file_path_flat_layout_fallback(tmp_path):
    f = tmp_path / "01 song.flac"
    f.write_bytes(b"x")
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    path = await repo.get_file_path("alice", "Artist/Album/01 song.flac")
    assert path == f.resolve()


@pytest.mark.asyncio
async def test_get_file_path_scans_for_sanitised_folder(tmp_path):
    # slskd may rename the local folder; fall back to a one-level scan for the file.
    folder = tmp_path / "Artist - Album"
    folder.mkdir()
    f = folder / "02 song.flac"
    f.write_bytes(b"x")
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    path = await repo.get_file_path("alice", "Some/Other/Remote/02 song.flac")
    assert path == f.resolve()


@pytest.mark.asyncio
async def test_get_file_path_missing_returns_none(tmp_path):
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    assert await repo.get_file_path("alice", "Artist/Album/missing.flac") is None


@pytest.mark.asyncio
async def test_get_file_path_rejects_traversal(tmp_path):
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    assert await repo.get_file_path("alice", "../../etc/passwd") is None


@pytest.mark.asyncio
async def test_get_file_path_username_nested_album_layout(tmp_path):
    # some slskd setups file downloads under {downloads}/{username}/{album}/{file};
    # the leaf-folder and one-level-scan strategies miss it (it's two deep), but the
    # username-scoped walk finds it.
    folder = tmp_path / "dshaw8772" / "2024 Some Album"
    folder.mkdir(parents=True)
    f = folder / "Artist - Album - 01 - Track.flac"
    f.write_bytes(b"x")
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    path = await repo.get_file_path(
        "dshaw8772", "@@peer\\Music\\2024 Some Album\\Artist - Album - 01 - Track.flac"
    )
    assert path == f.resolve()


@pytest.mark.asyncio
async def test_get_file_path_username_walk_handles_glob_chars(tmp_path):
    # a filename with glob metacharacters ([], *, ?) must match literally in the walk
    # (an rglob-based search would misinterpret them).
    folder = tmp_path / "peer1" / "Disc [2019]"
    folder.mkdir(parents=True)
    f = folder / "01 - Song [Remix].flac"
    f.write_bytes(b"x")
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    path = await repo.get_file_path("peer1", "@@p\\X\\Disc [2019]\\01 - Song [Remix].flac")
    assert path == f.resolve()


@pytest.mark.asyncio
async def test_get_file_path_size_fallback_for_sanitised_filename(tmp_path):
    # slskd stripped an illegal char from the on-disk name, so the basename no longer
    # matches anywhere; an exact byte-size match under the peer's folder recovers it.
    folder = tmp_path / "peer1"
    folder.mkdir()
    f = folder / "Track _ Sanitised.flac"  # on-disk: '?' replaced with '_'
    f.write_bytes(b"abcdefghij")  # size 10
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    path = await repo.get_file_path("peer1", "@@p\\Album\\Track ? Sanitised.flac", size=10)
    assert path == f.resolve()


@pytest.mark.asyncio
async def test_get_file_path_size_fallback_is_scoped_to_peer(tmp_path):
    # a same-size file under a DIFFERENT peer must not be returned by the size fallback
    (tmp_path / "other").mkdir()
    (tmp_path / "other" / "decoy.flac").write_bytes(b"abcdefghij")  # size 10, wrong peer
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    assert await repo.get_file_path("peer1", "@@p\\A\\missing.flac", size=10) is None


@pytest.mark.asyncio
async def test_get_file_path_deep_nested_non_username_folder(tmp_path):
    # some setups nest {downloads}/{artist}/{album}/{file} under a folder that is NOT
    # the peer's username, two levels deep: the leaf-folder (1), one-level-scan (4) and
    # username walk (3/5) all miss it. The whole-mount name+size fallback recovers it.
    folder = tmp_path / "The Beatles" / "Abbey Road (1969)"
    folder.mkdir(parents=True)
    f = folder / "01 - Come Together.flac"
    f.write_bytes(b"abcdefghij")  # size 10
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    path = await repo.get_file_path(
        "peer1", "@@p\\Music\\Abbey Road (1969)\\01 - Come Together.flac", size=10
    )
    assert path == f.resolve()


@pytest.mark.asyncio
async def test_get_file_path_whole_mount_fallback_disambiguates_by_size(tmp_path):
    # two files share the basename in different non-username folders; only the one whose
    # byte size matches is the real download, so size must break the tie (never grab the
    # wrong same-named track).
    wrong = tmp_path / "ArtistA" / "AlbumA" / "01 - Track.flac"
    wrong.parent.mkdir(parents=True)
    wrong.write_bytes(b"x")  # size 1, wrong file
    right = tmp_path / "ArtistB" / "AlbumB" / "01 - Track.flac"
    right.parent.mkdir(parents=True)
    right.write_bytes(b"abcdefghij")  # size 10, the real download
    repo = SlskdRepository(client=None, url="", api_key="", downloads_mount=tmp_path)
    path = await repo.get_file_path("peer1", "@@p\\X\\AlbumB\\01 - Track.flac", size=10)
    assert path == right.resolve()


def _completed(filename, username="peer"):
    return SlskdTransfer(id=filename, username=username, filename=filename, state="Completed, Succeeded")


@pytest.mark.asyncio
async def test_diagnose_mount_flags_completed_downloads_but_empty_mount(tmp_path):
    # slskd finished downloads but our (empty) mount shows nothing -> silent misconfig
    client = AsyncMock()
    client.get_all_downloads = AsyncMock(return_value=[_completed("a/01.flac"), _completed("a/02.flac")])
    client.get_options = AsyncMock(return_value=SlskdOptions())
    repo = SlskdRepository(client=client, url="", api_key="", downloads_mount=tmp_path)
    diag = await repo.diagnose_downloads_mount()
    assert diag.supported is True
    assert diag.completed_downloads == 2
    assert diag.mount_has_files is False


@pytest.mark.asyncio
async def test_diagnose_mount_ok_when_mount_has_files(tmp_path):
    (tmp_path / "peer").mkdir()
    (tmp_path / "peer" / "01.flac").write_bytes(b"x")
    client = AsyncMock()
    client.get_all_downloads = AsyncMock(return_value=[_completed("peer/01.flac")])
    client.get_options = AsyncMock(return_value=SlskdOptions())
    repo = SlskdRepository(client=client, url="", api_key="", downloads_mount=tmp_path)
    diag = await repo.diagnose_downloads_mount()
    assert diag.completed_downloads == 1
    assert diag.mount_has_files is True
    assert diag.resolvable_downloads == 1  # the finished file is locatable under the mount


@pytest.mark.asyncio
async def test_diagnose_mount_flags_unresolvable_downloads_under_full_parent_mount(tmp_path):
    # The footgun: the mount is a PARENT of slskd's downloads (e.g. the whole media
    # library) - full of files, so mount_has_files is True, yet none of slskd's finished
    # downloads resolve under it. resolvable_downloads is the honest signal.
    (tmp_path / "library").mkdir()
    (tmp_path / "library" / "unrelated.flac").write_bytes(b"x")
    client = AsyncMock()
    client.get_all_downloads = AsyncMock(return_value=[
        _completed("slskd/complete/a/01.flac"), _completed("slskd/complete/a/02.flac"),
    ])
    client.get_options = AsyncMock(
        return_value=SlskdOptions(directories=SlskdDirectories(downloads="/data/downloads/slskd/complete"))
    )
    repo = SlskdRepository(client=client, url="", api_key="", downloads_mount=tmp_path)
    diag = await repo.diagnose_downloads_mount()
    assert diag.completed_downloads == 2
    assert diag.mount_has_files is True       # fooled by the library file
    assert diag.sampled_downloads == 2
    assert diag.resolvable_downloads == 0     # but DN can locate none of slskd's files
    # and we surface slskd's own path so the advisory can name it
    assert diag.client_downloads_dir == "/data/downloads/slskd/complete"


@pytest.mark.asyncio
async def test_get_file_path_runs_off_the_event_loop(tmp_path, monkeypatch):
    # The file finder does potentially large filesystem walks. It must run in a worker
    # thread, not inline - otherwise a big/misconfigured mount freezes the whole loop
    # (polling, SSE, requests, the cancel button) -> "won't cancel and everything hangs".
    repo = SlskdRepository(client=AsyncMock(), url="", api_key="", downloads_mount=tmp_path)
    release = threading.Event()

    def slow_locate(*_args, **_kwargs):
        release.wait(timeout=5)  # block the worker thread, not the loop
        return None

    monkeypatch.setattr(repo, "_locate_file", slow_locate)
    locate = asyncio.create_task(repo.get_file_path("peer", "album/01.flac"))

    ticks = 0
    for _ in range(5):
        await asyncio.sleep(0.01)  # the loop keeps turning only if the walk is off it
        ticks += 1
    assert ticks == 5
    assert not locate.done()  # still blocked in its thread, but the loop ran freely

    release.set()
    assert await locate is None


@pytest.mark.asyncio
async def test_diagnose_mount_no_completed_downloads_skips_walk(tmp_path):
    # in-progress only -> nothing to flag, and the mount walk is skipped
    client = AsyncMock()
    client.get_all_downloads = AsyncMock(return_value=[
        SlskdTransfer(id="1", username="peer", filename="a/01.flac", state="InProgress")
    ])
    client.get_options = AsyncMock(return_value=SlskdOptions())
    repo = SlskdRepository(client=client, url="", api_key="", downloads_mount=tmp_path)
    diag = await repo.diagnose_downloads_mount()
    assert diag.completed_downloads == 0
    assert diag.mount_has_files is True


@pytest.mark.asyncio
async def test_diagnose_mount_never_raises_on_client_error(tmp_path):
    client = AsyncMock()
    client.get_all_downloads = AsyncMock(side_effect=RuntimeError("slskd down"))
    repo = SlskdRepository(client=client, url="", api_key="", downloads_mount=tmp_path)
    diag = await repo.diagnose_downloads_mount()
    assert diag.supported is True
    assert diag.completed_downloads == 0  # degraded, no false alarm


def test_parse_search_responses_windows_linux_and_disc():
    resp = SlskdUserSearchResponse(
        username="u",
        has_free_upload_slot=True,
        upload_speed=5,
        files=[
            SlskdFile(filename="Artist\\Album\\01 song.flac", size=1),
            SlskdFile(filename="Artist/Album/CD 2/05 song.flac", size=2),
        ],
    )
    out = SlskdRepository._parse_search_responses([resp])
    assert out[0].parent_directory == "Album"
    assert out[0].extension == "flac"
    # disc-pattern directory walked past to the album-level folder.
    assert out[1].parent_directory == "Album"


def test_sanitize_query_preserves_hyphenated_names():
    assert SlskdRepository._sanitize_query("AC-DC - Back in Black") == "AC-DC Back in Black"


def test_build_album_query_joins_with_separator():
    query = SlskdRepository._build_album_query("Radiohead", "OK Computer", 1997)
    assert query == "Radiohead OK Computer 1997"
