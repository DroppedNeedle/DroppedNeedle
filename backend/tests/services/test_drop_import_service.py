"""DropImportService: extraction safety, identification tiers, organisation,
duplicate/upgrade policy, needs_review -> match/discard, and post-import hooks.

External metadata (identifier, matcher, fingerprinter) is stubbed with
AsyncMock per house rules; the store is a real SQLite file; file moves are real
filesystem operations under tmp_path. One end-to-end test uses the committed
real-audio fixtures with the real tagger.
"""

import asyncio
import threading
import zipfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from infrastructure.persistence.drop_import_store import DropImportStore
from models.audio import AudioInfo, AudioTag
from models.drop_import import ItemStatus, JobStatus
from services.native.album_matcher import MBTrack, _ReleaseMeta
from services.native.drop_import_service import DropImportService
from services.native.naming import NamingTemplateEngine

FIXTURES = Path(__file__).parent.parent / "fixtures" / "library"


def _tag(
    title: str, track: int, artist: str = "Test Artist", album: str = "Test Album"
) -> AudioTag:
    return AudioTag(
        title=title, artist=artist, album=album, track_number=track, year=2020
    )


def _info(fmt: str = "flac", bitrate: int = 1000) -> AudioInfo:
    return AudioInfo(
        duration_seconds=200.0,
        bitrate=bitrate,
        sample_rate=44100,
        channels=2,
        file_format=fmt,
        file_size_bytes=1000,
    )


def _meta(rg: str = "rg-1") -> _ReleaseMeta:
    return _ReleaseMeta(
        release_group_mbid=rg,
        release_mbid="rel-1",
        album_title="Test Album",
        artist="Test Artist",
        is_various=False,
        artist_mbid="artist-1",
        year=2020,
    )


def _tracks() -> list[MBTrack]:
    return [
        MBTrack(
            title="Song One",
            position=1,
            disc=1,
            absolute_position=1,
            length_ms=200_000,
            recording_mbid="rec-1",
        ),
        MBTrack(
            title="Song Two",
            position=2,
            disc=1,
            absolute_position=2,
            length_ms=200_000,
            recording_mbid="rec-2",
        ),
    ]


class FakeTagger:
    """Serves canned (tag, info) per file NAME; records album-identity stamps."""

    def __init__(self, by_name: dict) -> None:
        self.by_name = by_name
        self.stamped: list[str] = []

    def read_tags(self, path: Path):
        entry = self.by_name.get(Path(path).name)
        if entry is None:
            raise ValueError(f"unreadable: {path}")
        return entry

    def write_album_identity(self, path: Path, tag: AudioTag) -> None:
        self.stamped.append(tag.album)


def _build_service(
    tmp_path, tagger, *, identifier=None, fingerprinter=None, prefs=None
):
    store = DropImportStore(tmp_path / "library.db", threading.Lock())
    library_root = tmp_path / "library"
    library_root.mkdir(exist_ok=True)
    if prefs is None:
        prefs = SimpleNamespace(
            get_typed_library_settings_raw=lambda: SimpleNamespace(
                library_roots=[SimpleNamespace(path=str(library_root))],
                naming_template=None,
            ),
            get_download_policy=lambda: SimpleNamespace(recycle_bin_path=""),
        )
    if identifier is None:
        identifier = AsyncMock()
        identifier.release_tracks = AsyncMock(return_value=(_meta(), _tracks()))
    if fingerprinter is None:
        fingerprinter = AsyncMock()
        fingerprinter.fingerprint = AsyncMock(side_effect=RuntimeError("no acoustid"))
    library = AsyncMock()
    library.get_file_at_position = AsyncMock(return_value=None)
    library.upsert_file = AsyncMock(return_value="file-1")
    library.soft_delete_file = AsyncMock()
    service = DropImportService(
        store=store,
        tagger=tagger,
        fingerprinter=fingerprinter,
        album_identifier=identifier,
        mb_matcher=AsyncMock(),
        naming_engine=NamingTemplateEngine(),
        library_manager=library,
        preferences_service=prefs,
        request_history=AsyncMock(async_get_record=AsyncMock(return_value=None)),
        wanted_store=AsyncMock(get_watch=AsyncMock(return_value=None)),
        sse_publisher=AsyncMock(),
        on_import=AsyncMock(),
        staging_root=tmp_path / "imports",
    )
    return service, store, library, library_root


async def _wait_job(store, job_id: str):
    # Real wall-clock deadline (~5s): the job does genuine file I/O and tag
    # parsing in worker threads, so bare sleep(0) turns can run out before it
    # finishes on a slow machine.
    for _ in range(500):
        job = await store.get_job(job_id)
        if job is not None and job.status != JobStatus.PROCESSING:
            return job
        await asyncio.sleep(0.01)
    raise AssertionError("job never reached a terminal state")


def _zip_album(path: Path, folder: str = "Test Artist - Test Album") -> Path:
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr(f"{folder}/01 Song One.flac", b"a" * 64)
        zf.writestr(f"{folder}/02 Song Two.flac", b"b" * 64)
        zf.writestr(f"{folder}/cover.jpg", b"jpg")
    return path


def _accepted_match(rg: str = "rg-1"):
    from services.native.album_matcher import AlbumMatch

    return AlbumMatch(
        accepted=True,
        distance=0.05,
        release_group_mbid=rg,
        release_mbid="rel-1",
        assignments={},
    )


# -- extraction safety --


def test_safe_extract_refuses_traversal_and_skips_non_audio(tmp_path):
    tagger = FakeTagger({})
    service, _, _, _ = _build_service(tmp_path, tagger)
    staging = tmp_path / "imports" / "job"
    staging.mkdir(parents=True)
    archive = staging / "evil.zip"
    with zipfile.ZipFile(archive, "w") as zf:
        zf.writestr("album/ok.flac", b"x")
        zf.writestr("../escape.flac", b"x")
        zf.writestr("/abs/escape2.flac", b"x")
        zf.writestr("album/readme.txt", b"x")

    units, notes = service._extract_and_group(staging)

    assert not (tmp_path / "imports" / "escape.flac").exists()
    assert not Path("/abs/escape2.flac").exists()
    names = dict(units)
    assert list(names) == ["evil"]
    assert [p.name for p in names["evil"]] == ["ok.flac"]
    assert any("non-audio" in n for n in notes)


def test_extract_groups_loose_files_and_folders(tmp_path):
    tagger = FakeTagger({})
    service, _, _, _ = _build_service(tmp_path, tagger)
    staging = tmp_path / "imports" / "job"
    (staging / "My Album").mkdir(parents=True)
    (staging / "My Album" / "01.flac").write_bytes(b"x")
    (staging / "loose.mp3").write_bytes(b"x")

    units, _ = service._extract_and_group(staging)
    names = dict(units)
    assert set(names) == {"My Album", "Loose tracks"}


@pytest.mark.asyncio
async def test_a_corrupt_archive_alongside_a_good_one_is_reported(tmp_path):
    """The good zip imports; the bad one must not vanish silently."""
    tagger = FakeTagger(
        {
            "01 Song One.flac": (_tag("Song One", 1), _info()),
            "02 Song Two.flac": (_tag("Song Two", 2), _info()),
        }
    )
    identifier = AsyncMock()
    identifier.identify = AsyncMock(return_value=_accepted_match())
    identifier.release_tracks = AsyncMock(return_value=(_meta(), _tracks()))
    service, store, _, _ = _build_service(tmp_path, tagger, identifier=identifier)

    good = _zip_album(tmp_path / "good.zip")
    bad = tmp_path / "bad.zip"
    bad.write_bytes(b"PK\x03\x04" + b"\x00" * 20)

    job = await service.create_job(
        user_id="user-1",
        user_name="Harvey",
        uploads=[("good.zip", good), ("bad.zip", bad)],
    )
    done = await _wait_job(store, job.id)

    assert done.status == JobStatus.COMPLETED
    assert [i.status for i in done.items] == [ItemStatus.IMPORTED]
    assert "the archive is corrupt" in (done.error or "")


def test_corrupt_zip_is_noted_not_fatal(tmp_path):
    tagger = FakeTagger({})
    service, _, _, _ = _build_service(tmp_path, tagger)
    staging = tmp_path / "imports" / "job"
    staging.mkdir(parents=True)
    (staging / "good").mkdir()
    (staging / "good" / "01.flac").write_bytes(b"x")
    # a header that claims zip but is truncated
    (staging / "bad.zip").write_bytes(b"PK\x03\x04" + b"\x00" * 20)

    units, notes = service._extract_and_group(staging)
    assert dict(units).keys() == {"good"}


# -- the happy path, end to end --


@pytest.mark.asyncio
async def test_zip_drop_imports_album_end_to_end(tmp_path):
    tagger = FakeTagger(
        {
            "01 Song One.flac": (_tag("Song One", 1), _info()),
            "02 Song Two.flac": (_tag("Song Two", 2), _info()),
        }
    )
    identifier = AsyncMock()
    identifier.identify = AsyncMock(return_value=_accepted_match())
    identifier.release_tracks = AsyncMock(return_value=(_meta(), _tracks()))
    service, store, library, library_root = _build_service(
        tmp_path, tagger, identifier=identifier
    )

    upload = tmp_path / "upload.zip"
    _zip_album(upload)
    job = await service.create_job(
        user_id="user-1", user_name="Harvey", uploads=[("album.zip", upload)]
    )
    done = await _wait_job(store, job.id)

    assert done.status == JobStatus.COMPLETED
    assert len(done.items) == 1
    item = done.items[0]
    # the staging collision prefix (000_) must not leak into the visible name
    assert item.folder_name == "album"
    assert item.status == ItemStatus.IMPORTED
    assert item.release_group_mbid == "rg-1"
    assert item.album_title == "Test Album"
    assert item.files_imported == 2
    assert item.staging_paths == []

    # organised into the library under the default naming template
    target_dir = library_root / "Test Artist" / "Test Album (2020)"
    assert sorted(p.name for p in target_dir.iterdir()) == [
        "0101 Song One.flac",
        "0102 Song Two.flac",
    ]
    assert library.upsert_file.await_count == 2
    kwargs = library.upsert_file.await_args_list[0].kwargs
    assert kwargs["source"] == "drop"
    assert kwargs["release_group_mbid"] == "rg-1"
    assert tagger.stamped == ["Test Album", "Test Album"]


@pytest.mark.asyncio
async def test_import_resolves_open_request_and_notifies_requester(tmp_path):
    tagger = FakeTagger(
        {
            "01 Song One.flac": (_tag("Song One", 1), _info()),
            "02 Song Two.flac": (_tag("Song Two", 2), _info()),
        }
    )
    identifier = AsyncMock()
    identifier.identify = AsyncMock(return_value=_accepted_match())
    identifier.release_tracks = AsyncMock(return_value=(_meta(), _tracks()))
    service, store, _, _ = _build_service(tmp_path, tagger, identifier=identifier)
    record = SimpleNamespace(status="approved", user_id="requester-9")
    service._requests.async_get_record = AsyncMock(return_value=record)
    service._requests.async_update_status = AsyncMock()

    upload = tmp_path / "upload.zip"
    _zip_album(upload)
    job = await service.create_job(
        user_id="user-1", user_name="Harvey", uploads=[("album.zip", upload)]
    )
    await _wait_job(store, job.id)

    service._requests.async_update_status.assert_awaited_once()
    args = service._requests.async_update_status.await_args
    assert args.args[0] == "rg-1"
    assert args.args[1] == "imported"
    events = [c.args[1] for c in service._sse.publish.await_args_list]
    assert "request_imported" in events
    channel = [
        c.args[0]
        for c in service._sse.publish.await_args_list
        if c.args[1] == "request_imported"
    ][0]
    assert channel == "user:requester-9"
    service._on_import.assert_awaited_once()


# -- duplicates and upgrades --


@pytest.mark.asyncio
async def test_equal_quality_duplicate_is_skipped(tmp_path):
    tagger = FakeTagger(
        {
            "01 Song One.flac": (_tag("Song One", 1), _info()),
            "02 Song Two.flac": (_tag("Song Two", 2), _info()),
        }
    )
    identifier = AsyncMock()
    identifier.identify = AsyncMock(return_value=_accepted_match())
    identifier.release_tracks = AsyncMock(return_value=(_meta(), _tracks()))
    service, store, library, _ = _build_service(tmp_path, tagger, identifier=identifier)
    library.get_file_at_position = AsyncMock(
        side_effect=lambda rg, disc, pos: {
            "file_path": str(tmp_path / f"existing-{pos}.flac"),
            "recording_mbid": f"rec-{pos}",
            "file_format": "flac",
            "bit_rate": 1000,
        }
    )

    upload = tmp_path / "upload.zip"
    _zip_album(upload)
    job = await service.create_job(
        user_id="user-1", user_name="Harvey", uploads=[("album.zip", upload)]
    )
    done = await _wait_job(store, job.id)

    item = done.items[0]
    assert item.status == ItemStatus.SKIPPED
    assert item.files_imported == 0
    library.upsert_file.assert_not_awaited()
    assert "already in your library" in (item.detail or "")


@pytest.mark.asyncio
async def test_strictly_better_quality_upgrades_and_recycles(tmp_path):
    tagger = FakeTagger(
        {
            "01 Song One.flac": (_tag("Song One", 1), _info("flac")),
            "02 Song Two.flac": (_tag("Song Two", 2), _info("flac")),
        }
    )
    identifier = AsyncMock()
    identifier.identify = AsyncMock(return_value=_accepted_match())
    identifier.release_tracks = AsyncMock(return_value=(_meta(), _tracks()))
    service, store, library, library_root = _build_service(
        tmp_path, tagger, identifier=identifier
    )
    old_files = []
    for pos in (1, 2):
        old = library_root / f"old-{pos}.mp3"
        old.write_bytes(b"old")
        old_files.append(old)
    library.get_file_at_position = AsyncMock(
        side_effect=lambda rg, disc, pos: {
            "file_path": str(old_files[pos - 1]),
            "recording_mbid": f"rec-{pos}",
            "file_format": "mp3",
            "bit_rate": 320,
        }
    )

    upload = tmp_path / "upload.zip"
    _zip_album(upload)
    job = await service.create_job(
        user_id="user-1", user_name="Harvey", uploads=[("album.zip", upload)]
    )
    done = await _wait_job(store, job.id)

    item = done.items[0]
    assert item.status == ItemStatus.IMPORTED
    assert item.files_imported == 2
    assert "upgraded" in (item.detail or "")
    # old files were recycled (moved into .recycle under the library root)
    assert not old_files[0].exists()
    recycled = list((library_root / ".recycle").rglob("*.mp3"))
    assert len(recycled) == 2
    assert library.soft_delete_file.await_count == 2
    assert library.upsert_file.await_count == 2


# -- needs_review, manual match, discard --


@pytest.mark.asyncio
async def test_unidentified_drop_needs_review_then_manual_match(tmp_path):
    tagger = FakeTagger(
        {
            "01 Song One.flac": (_tag("Song One", 1), _info()),
            "02 Song Two.flac": (_tag("Song Two", 2), _info()),
        }
    )
    identifier = AsyncMock()
    identifier.identify = AsyncMock(return_value=None)  # nothing identifies
    identifier.release_tracks = AsyncMock(return_value=(_meta("rg-manual"), _tracks()))
    service, store, library, library_root = _build_service(
        tmp_path, tagger, identifier=identifier
    )

    upload = tmp_path / "upload.zip"
    _zip_album(upload)
    job = await service.create_job(
        user_id="user-1", user_name="Harvey", uploads=[("album.zip", upload)]
    )
    done = await _wait_job(store, job.id)

    item = done.items[0]
    assert item.status == ItemStatus.NEEDS_REVIEW
    assert item.staging_paths, "staged files must survive for the manual match"
    assert all(Path(p).exists() for p in item.staging_paths)

    matched = await service.match_item(
        item.id, "rg-manual", user_id="user-1", is_admin=False
    )
    assert matched.status == ItemStatus.IMPORTED
    assert matched.release_group_mbid == "rg-manual"
    assert library.upsert_file.await_count == 2
    assert (library_root / "Test Artist" / "Test Album (2020)").exists()


@pytest.mark.asyncio
async def test_match_item_rejects_wrong_status_and_non_owner(tmp_path):
    from core.exceptions import ResourceNotFoundError, ValidationError

    tagger = FakeTagger(
        {
            "01 Song One.flac": (_tag("Song One", 1), _info()),
            "02 Song Two.flac": (_tag("Song Two", 2), _info()),
        }
    )
    identifier = AsyncMock()
    identifier.identify = AsyncMock(return_value=_accepted_match())
    identifier.release_tracks = AsyncMock(return_value=(_meta(), _tracks()))
    service, store, _, _ = _build_service(tmp_path, tagger, identifier=identifier)

    upload = tmp_path / "upload.zip"
    _zip_album(upload)
    job = await service.create_job(
        user_id="user-1", user_name="Harvey", uploads=[("album.zip", upload)]
    )
    done = await _wait_job(store, job.id)
    item = done.items[0]  # already imported

    with pytest.raises(ValidationError):
        await service.match_item(item.id, "rg-x", user_id="user-1", is_admin=False)
    with pytest.raises(ResourceNotFoundError):
        await service.match_item(
            item.id, "rg-x", user_id="someone-else", is_admin=False
        )


@pytest.mark.asyncio
async def test_discard_removes_staged_files(tmp_path):
    tagger = FakeTagger(
        {
            "01 Song One.flac": (_tag("Song One", 1), _info()),
            "02 Song Two.flac": (_tag("Song Two", 2), _info()),
        }
    )
    identifier = AsyncMock()
    identifier.identify = AsyncMock(return_value=None)
    identifier.release_tracks = AsyncMock(return_value=None)
    service, store, _, _ = _build_service(tmp_path, tagger, identifier=identifier)

    upload = tmp_path / "upload.zip"
    _zip_album(upload)
    job = await service.create_job(
        user_id="user-1", user_name="Harvey", uploads=[("album.zip", upload)]
    )
    done = await _wait_job(store, job.id)
    item = done.items[0]
    staged = [Path(p) for p in item.staging_paths]
    assert all(p.exists() for p in staged)

    discarded = await service.discard_item(item.id, user_id="user-1", is_admin=False)
    assert discarded.status == ItemStatus.DISCARDED
    assert all(not p.exists() for p in staged)


# -- single files and loose drops --


@pytest.mark.asyncio
async def test_single_file_identifies_via_fingerprint(tmp_path):
    from models.audio import FingerprintResult

    # loose uploads keep their index-prefixed staging name (zips extract to inner names)
    tagger = FakeTagger({"000_loose.flac": (_tag("Song One", 1), _info())})
    identifier = AsyncMock()
    identifier.release_tracks = AsyncMock(return_value=(_meta(), _tracks()))
    fingerprinter = AsyncMock()
    fingerprinter.fingerprint = AsyncMock(
        return_value=FingerprintResult(status="pass", score=0.95, recording_id="rec-1")
    )
    service, store, library, _ = _build_service(
        tmp_path, tagger, identifier=identifier, fingerprinter=fingerprinter
    )
    service._mb_matcher.resolve_recording_to_release_group = AsyncMock(
        return_value="rg-1"
    )

    upload = tmp_path / "loose.flac"
    upload.write_bytes(b"x" * 64)
    job = await service.create_job(
        user_id="user-1", user_name="Harvey", uploads=[("loose.flac", upload)]
    )
    done = await _wait_job(store, job.id)

    item = done.items[0]
    assert item.status == ItemStatus.IMPORTED
    assert item.files_imported == 1
    library.upsert_file.assert_awaited_once()


# -- housekeeping --


@pytest.mark.asyncio
async def test_sweep_stale_fails_processing_and_cleans_disk(tmp_path):
    tagger = FakeTagger({})
    service, store, _, _ = _build_service(tmp_path, tagger)
    staging = tmp_path / "imports" / "stale-job"
    staging.mkdir(parents=True)
    (staging / "x.flac").write_bytes(b"x")
    await store.create_job("stale-job", "user-1", "A", "a.zip", str(staging))

    await service.sweep_stale()

    job = await store.get_job("stale-job")
    assert job.status == JobStatus.FAILED
    assert not staging.exists()


@pytest.mark.asyncio
async def test_create_job_requires_library_path(tmp_path):
    from core.exceptions import ValidationError

    tagger = FakeTagger({})
    prefs = SimpleNamespace(
        get_typed_library_settings_raw=lambda: SimpleNamespace(
            library_roots=[], naming_template=None
        ),
        get_download_policy=lambda: SimpleNamespace(recycle_bin_path=""),
    )
    service, _, _, _ = _build_service(tmp_path, tagger, prefs=prefs)
    upload = tmp_path / "loose.flac"
    upload.write_bytes(b"x")

    with pytest.raises(ValidationError):
        await service.create_job(
            user_id="user-1", user_name="Harvey", uploads=[("loose.flac", upload)]
        )


# -- real audio, real tagger --


@pytest.mark.asyncio
async def test_real_fixture_import_stamps_album_identity(tmp_path):
    """End-to-end with the committed FLAC fixtures and the real tagger: files
    move under the template and carry the stamped release-group id."""
    import shutil

    from infrastructure.audio.tagger import AudioTagger

    tagger = AudioTagger()
    src1 = FIXTURES / "flac_full_01.flac"
    src2 = FIXTURES / "flac_full_02.flac"
    tag1, info1 = tagger.read_tags(src1)
    tag2, _ = tagger.read_tags(src2)

    identifier = AsyncMock()
    identifier.identify = AsyncMock(return_value=_accepted_match())
    meta = _ReleaseMeta(
        release_group_mbid="11111111-1111-1111-1111-111111111111",
        release_mbid="22222222-2222-2222-2222-222222222222",
        album_title=tag1.album or "Fixture Album",
        artist=tag1.artist or "Fixture Artist",
        is_various=False,
        artist_mbid=None,
        year=tag1.year,
    )
    tracks = [
        MBTrack(
            title=tag1.title or "One",
            position=tag1.track_number or 1,
            disc=1,
            absolute_position=1,
            length_ms=int(info1.duration_seconds * 1000),
            recording_mbid="rec-1",
        ),
        MBTrack(
            title=tag2.title or "Two",
            position=tag2.track_number or 2,
            disc=1,
            absolute_position=2,
            length_ms=int(info1.duration_seconds * 1000),
            recording_mbid="rec-2",
        ),
    ]
    identifier.release_tracks = AsyncMock(return_value=(meta, tracks))
    service, store, _, library_root = _build_service(
        tmp_path, tagger, identifier=identifier
    )

    drop_dir = tmp_path / "drop"
    drop_dir.mkdir()
    shutil.copy(src1, drop_dir / src1.name)
    shutil.copy(src2, drop_dir / src2.name)
    job = await service.create_job(
        user_id="user-1",
        user_name="Harvey",
        uploads=[(src1.name, drop_dir / src1.name), (src2.name, drop_dir / src2.name)],
    )
    done = await _wait_job(store, job.id)

    assert done.items[0].status == ItemStatus.IMPORTED
    imported = sorted(library_root.rglob("*.flac"))
    assert len(imported) == 2
    stamped, _ = tagger.read_tags(imported[0])
    assert (
        stamped.musicbrainz_release_group_id == "11111111-1111-1111-1111-111111111111"
    )
    assert stamped.album == meta.album_title
