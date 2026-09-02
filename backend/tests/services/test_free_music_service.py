"""FreeMusicService: candidate ranking, the licence-only rule, download with
progress, cancel, retry, and the handoff to the import pipeline.

The Archive is stubbed with AsyncMock (house rule for metadata repos); the store
is a real SQLite file; downloads write real bytes into tmp_path.
"""

import asyncio
import json
import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import ValidationError
from infrastructure.persistence.free_music_store import FreeMusicStore
from models.free_music import FreeMusicStatus
from models.acquisition_quality import QualityRecipeEntry
from repositories.archive_repository import ArchiveError, ArchiveFile, ArchiveItem
from services.native.free_music_service import FreeMusicService

CC = "http://creativecommons.org/licenses/by-nc-sa/3.0/"


def _item(identifier="jamendo-117853", title="Guess Who's a Mess") -> ArchiveItem:
    return ArchiveItem(
        identifier=identifier, title=title, creator="Brad Sucks", licence_url=CC
    )


def _files(fmt="VBR MP3", count=10, size=500) -> list[ArchiveFile]:
    return [
        ArchiveFile(
            name=f"{i:02d}.mp3", format=fmt, size_bytes=size, track=i, title=f"Song {i}"
        )
        for i in range(1, count + 1)
    ]


def _build(
    tmp_path, *, items=None, files=None, preferred="mp3", enabled=True, sse=None
):
    store = FreeMusicStore(tmp_path / "library.db", threading.Lock())
    archive = AsyncMock()
    archive.search_audio = AsyncMock(
        return_value=items if items is not None else [_item()]
    )
    archive.get_item_files = AsyncMock(
        return_value=(CC, files if files is not None else _files())
    )
    archive.extension_for = MagicMock(
        side_effect=lambda f: {"vbr mp3": "mp3", "flac": "flac"}.get(f.lower(), "")
    )

    async def _stream(identifier, filename):
        yield b"audio-bytes"

    archive.stream_file = _stream

    incoming = tmp_path / "incoming"
    incoming.mkdir()
    drop_import = MagicMock()
    drop_import.incoming_dir = lambda: incoming
    drop_import.create_job = AsyncMock()

    from api.v1.schemas.settings import DownloadPolicySettings

    def _policy():
        # Existing-install migration flavour: every tier accepted,
        # hi-res-first lossless, unknown evidence allowed as final fallback -
        # i.e. today's behaviour expressed through the snapshot fields.
        return DownloadPolicySettings(quality_min="low")

    prefs = SimpleNamespace(
        get_free_music_settings=lambda: SimpleNamespace(
            enabled=enabled, preferred_format=preferred
        ),
        get_download_policy=_policy,
    )
    service = FreeMusicService(
        store=store,
        archive=archive,
        drop_import=drop_import,
        preferences_service=prefs,
        sse_publisher=sse or AsyncMock(),
    )
    return service, store, archive, drop_import


async def _settle(service, store, task_id):
    for _ in range(2000):
        task = await store.get(task_id)
        if task and task.status in FreeMusicStatus.TERMINAL:
            return task
        await asyncio.sleep(0)
    raise AssertionError(f"task never settled: {(await store.get(task_id)).status}")


# -- the happy path --


@pytest.mark.asyncio
async def test_album_request_downloads_and_hands_off_to_the_importer(tmp_path):
    service, store, _, drop_import = _build(tmp_path)

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="d0484284-1ee7-4157-951a-50f003cbcfb4",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.COMPLETED
    assert task.identifier == "jamendo-117853"
    assert task.licence_url == CC
    assert task.files_total == 10 and task.files_completed == 10

    drop_import.create_job.assert_awaited_once()
    uploads = drop_import.create_job.await_args.kwargs["uploads"]
    assert len(uploads) == 10
    assert all(Path(p).name.endswith(".mp3") for _n, p in uploads)


@pytest.mark.asyncio
async def test_progress_events_carry_the_status(tmp_path):
    """The frontend sweeps its library caches only when status == 'completed', so a
    missing or wrong status silently leaves stale grids after a download lands."""
    sse = AsyncMock()
    service, store, _, _ = _build(tmp_path, sse=sse)

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="d0484284-1ee7-4157-951a-50f003cbcfb4",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    await _settle(service, store, task_id)

    published = [call.args for call in sse.publish.await_args_list]
    assert all(channel == "user:u1" for channel, _event, _payload in published)
    assert all(event == "free_music_updated" for _channel, event, _payload in published)

    statuses = [payload["status"] for _channel, _event, payload in published]
    assert statuses[-1] == FreeMusicStatus.COMPLETED
    assert statuses.count(FreeMusicStatus.COMPLETED) == 1
    assert FreeMusicStatus.IMPORTING in statuses
    assert all(payload["task_id"] == task_id for _c, _e, payload in published)


@pytest.mark.asyncio
async def test_a_failure_publishes_a_failed_status(tmp_path):
    sse = AsyncMock()
    service, store, _, _ = _build(tmp_path, items=[], sse=sse)

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Nobody",
        album_title="Nothing",
    )
    await _settle(service, store, task_id)

    statuses = [call.args[2]["status"] for call in sse.publish.await_args_list]
    assert statuses[-1] == FreeMusicStatus.FAILED


@pytest.mark.asyncio
async def test_staging_is_removed_after_a_successful_import(tmp_path):
    service, store, _, _ = _build(tmp_path)
    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    await _settle(service, store, task_id)

    assert not (tmp_path / "incoming" / f"free-{task_id}").exists()


# -- the licence-only rule --


@pytest.mark.asyncio
async def test_an_item_whose_licence_cannot_be_read_is_never_downloaded(tmp_path):
    """`a-new-low-in-hifi` is a real CC album with no licenseurl. Free Music
    must not touch it - the repository reports ('', []) and there is no candidate."""
    service, store, archive, drop_import = _build(tmp_path)
    archive.get_item_files = AsyncMock(return_value=("", []))

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="A New Low in Hi-Fi",
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.FAILED
    assert "No source has this" in (task.error or "")
    drop_import.create_job.assert_not_awaited()


@pytest.mark.asyncio
async def test_a_title_that_does_not_resemble_the_request_is_rejected(tmp_path):
    """The Archive is full of tributes and remasters by the same artist."""
    service, store, _, drop_import = _build(
        tmp_path,
        items=[_item(identifier="tribute", title="Piano Tribute to Brad Sucks")],
    )

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.FAILED
    drop_import.create_job.assert_not_awaited()


# -- ranking --


@pytest.mark.asyncio
async def test_snapshot_preference_orders_flac_above_unknown_bitrate_mp3(tmp_path):
    """free_music.preferred_format is no longer read for ranking: the stored
    snapshot's global preference step decides - lossless first, proven-lossy-
    unknown-bitrate as its canonical ``low`` projection."""
    both = _files("VBR MP3", 10) + _files("FLAC", 10, size=5000)
    service, store, _, _ = _build(tmp_path, files=both, preferred="mp3")

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    task = await _settle(service, store, task_id)
    assert task.format == "flac"
    # creation-time snapshot is pinned onto the row
    assert task.quality_snapshot_hash
    ladder = json.loads(task.tried_candidates_json or "[]")
    assert {"identifier": "jamendo-117853", "format": "FLAC"} in [
        {key: value for key, value in entry.items() if key not in {"reason", "outcome"}}
        for entry in ladder
    ]


@pytest.mark.asyncio
async def test_v2_recipe_review_candidate_uses_exact_probe_before_import(tmp_path):
    """A review-only Archive label may download, but only exact local quality
    evidence can authorize the Drop Import handoff."""
    from api.v1.schemas.settings import DownloadPolicySettings

    service, store, _archive, drop_import = _build(tmp_path, files=_files("FLAC"))
    policy = DownloadPolicySettings(
        flac_mp3_only=True,
        quality_recipe=[QualityRecipeEntry(format="flac", quality="cd")],
        unknown_quality_behavior="review",
    )
    service._prefs.get_download_policy = lambda: policy
    probe = MagicMock()
    probe.read_tags.return_value = (
        None,
        SimpleNamespace(
            file_format="flac",
            bitrate=700,
            sample_rate=44100,
            bit_depth=16,
            file_size_bytes=500,
        ),
    )
    service._probe_tagger = probe

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.COMPLETED
    drop_import.create_job.assert_awaited_once()


@pytest.mark.asyncio
async def test_v2_recipe_review_probe_unavailable_tries_next_without_import(tmp_path):
    """An unavailable exact probe must record the outcome, clean the bytes, and
    continue down the persisted candidate ladder without publishing."""
    from api.v1.schemas.settings import DownloadPolicySettings

    files = _files("FLAC", count=1, size=5000) + _files("VBR MP3", count=1, size=1000)
    service, store, archive, drop_import = _build(tmp_path, files=files)
    policy = DownloadPolicySettings(
        flac_mp3_only=True,
        quality_recipe=[
            QualityRecipeEntry(format="flac", quality="cd"),
            QualityRecipeEntry(format="mp3", quality="320_plus"),
        ],
        unknown_quality_behavior="review",
    )
    service._prefs.get_download_policy = lambda: policy
    service._probe_tagger = None
    stream_calls: list[tuple[str, str]] = []

    async def stream(identifier, filename):
        stream_calls.append((identifier, filename))
        yield b"audio-bytes"

    archive.stream_file = stream

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.FAILED
    assert len(stream_calls) == 2
    assert "quality-verified" in (task.error or "")
    drop_import.create_job.assert_not_awaited()
    ladder = json.loads(task.tried_candidates_json)
    assert [entry["outcome"] for entry in ladder] == [
        "quality_probe_unavailable",
        "quality_probe_unavailable",
    ]


@pytest.mark.asyncio
async def test_v2_recipe_reject_excludes_candidate_before_download(tmp_path):
    """The explicit reject rule remains a pre-download exclusion."""
    from api.v1.schemas.settings import DownloadPolicySettings

    service, store, archive, drop_import = _build(tmp_path, files=_files("FLAC"))
    policy = DownloadPolicySettings(
        flac_mp3_only=True,
        quality_recipe=[QualityRecipeEntry(format="flac", quality="cd")],
        unknown_quality_behavior="reject",
    )
    service._prefs.get_download_policy = lambda: policy
    stream_calls: list[tuple[str, str]] = []

    async def stream(identifier, filename):
        stream_calls.append((identifier, filename))
        yield b"audio-bytes"

    archive.stream_file = stream

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.FAILED
    assert stream_calls == []
    drop_import.create_job.assert_not_awaited()


@pytest.mark.asyncio
async def test_v1_flac_mp3_only_rejects_archive_ogg_before_download(tmp_path):
    service, store, archive, drop_import = _build(
        tmp_path, files=_files("Ogg Vorbis", count=1)
    )
    archive.extension_for = MagicMock(return_value="ogg")
    stream_calls: list[tuple[str, str]] = []

    async def stream(identifier, filename):
        stream_calls.append((identifier, filename))
        yield b"audio-bytes"

    archive.stream_file = stream

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.FAILED
    assert stream_calls == []
    drop_import.create_job.assert_not_awaited()


@pytest.mark.asyncio
async def test_v1_outside_policy_candidate_is_probed_then_fails_over(tmp_path):
    from api.v1.schemas.settings import DownloadPolicySettings

    items = [_item(identifier="low"), _item(identifier="good")]
    service, store, archive, drop_import = _build(tmp_path, items=items)
    archive.extension_for = MagicMock(return_value="mp3")
    archive.get_item_files = AsyncMock(
        side_effect=[
            (CC, _files("VBR MP3", count=1, size=200)),
            (CC, _files("VBR MP3", count=1, size=100)),
        ]
    )
    service._prefs.get_download_policy = lambda: DownloadPolicySettings(
        quality_min="mp3_192",
        quality_max="lossless",
        lossy_min_bitrate_kbps=192,
        lossy_max_bitrate_kbps=320,
        preferred_lossy_bitrate_kbps=320,
    )
    service._probe_tagger = MagicMock()
    service._probe_tagger.read_tags.side_effect = [
        (
            None,
            SimpleNamespace(
                file_format="mp3",
                bitrate=128,
                sample_rate=44100,
                bit_depth=None,
                file_size_bytes=200,
            ),
        ),
        (
            None,
            SimpleNamespace(
                file_format="mp3",
                bitrate=320,
                sample_rate=44100,
                bit_depth=None,
                file_size_bytes=100,
            ),
        ),
    ]
    stream_calls: list[tuple[str, str]] = []

    async def stream(identifier, filename):
        stream_calls.append((identifier, filename))
        yield b"audio-bytes"

    archive.stream_file = stream

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.COMPLETED
    assert [identifier for identifier, _filename in stream_calls] == ["low", "good"]
    drop_import.create_job.assert_awaited_once()
    ladder = json.loads(task.tried_candidates_json)
    assert [entry["outcome"] for entry in ladder] == [
        "post_download_quality_mismatch",
        "completed",
    ]


@pytest.mark.asyncio
async def test_track_count_agreement_beats_format_preference(tmp_path):
    """A 10-track MusicBrainz album should not pull a 2-track FMA sampler."""
    sampler = _files("FLAC", 2, size=5000)
    full = _files("VBR MP3", 10)
    service, store, archive, _ = _build(
        tmp_path, files=sampler + full, preferred="flac"
    )

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
        track_count=10,
    )
    task = await _settle(service, store, task_id)
    assert task.format == "mp3"
    assert task.files_total == 10


# -- failures, cancel, retry --


@pytest.mark.asyncio
async def test_a_dead_archive_fails_the_task_with_a_readable_error(tmp_path):
    service, store, archive, _ = _build(tmp_path)
    archive.search_audio = AsyncMock(side_effect=ArchiveError("boom"))

    task_id = await service.request_album(
        user_id="u1", release_group_mbid="rg", artist_name="A", album_title="B"
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.FAILED
    assert "Internet Archive" in (task.error or "")


@pytest.mark.asyncio
async def test_download_retries_once_then_fails(tmp_path):
    service, store, archive, drop_import = _build(tmp_path)
    attempts = {"n": 0}

    async def _flaky(identifier, filename):
        attempts["n"] += 1
        raise OSError("connection reset")
        yield b""  # pragma: no cover - generator shape

    archive.stream_file = _flaky

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.FAILED
    assert attempts["n"] == 2  # one retry, then give up
    assert not (tmp_path / "incoming" / f"free-{task_id}").exists()
    drop_import.create_job.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_stops_the_download_and_cleans_up(tmp_path):
    service, store, archive, drop_import = _build(tmp_path)
    started = asyncio.Event()

    async def _slow(identifier, filename):
        started.set()
        await asyncio.sleep(0.05)
        yield b"x"

    archive.stream_file = _slow

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    await asyncio.wait_for(started.wait(), timeout=2)
    await service.cancel(task_id, user_id="u1", is_admin=False)

    for _ in range(200):
        if not (tmp_path / "incoming" / f"free-{task_id}").exists():
            break
        await asyncio.sleep(0.01)

    task = await store.get(task_id)
    assert task.status == FreeMusicStatus.CANCELLED
    drop_import.create_job.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancelled_search_failure_cannot_overwrite_cancelled_status(tmp_path):
    service, store, archive, _ = _build(tmp_path)
    started = asyncio.Event()
    release = asyncio.Event()

    async def _failing_search(*_args):
        started.set()
        await release.wait()
        raise ArchiveError("down")

    archive.search_audio = _failing_search
    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="A",
        album_title="B",
    )
    await asyncio.wait_for(started.wait(), timeout=2)

    await service.cancel(task_id, user_id="u1", is_admin=False)
    handle = service._tasks[task_id]
    release.set()
    await asyncio.wait_for(handle, timeout=2)

    assert (await store.get(task_id)).status == FreeMusicStatus.CANCELLED


@pytest.mark.asyncio
async def test_import_handoff_is_not_cancellable(tmp_path):
    service, store, _, drop_import = _build(tmp_path)
    importing = asyncio.Event()
    release = asyncio.Event()

    async def _slow_import(**_kwargs):
        importing.set()
        await release.wait()

    drop_import.create_job = _slow_import
    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    await asyncio.wait_for(importing.wait(), timeout=2)

    with pytest.raises(ValidationError, match="already being added"):
        await service.cancel(task_id, user_id="u1", is_admin=False)

    release.set()
    assert (await _settle(service, store, task_id)).status == FreeMusicStatus.COMPLETED


@pytest.mark.asyncio
async def test_retry_waits_for_cancelled_attempt_to_stop(tmp_path):
    service, store, archive, drop_import = _build(tmp_path)
    started = asyncio.Event()
    release = asyncio.Event()

    async def _slow_stream(_identifier, _filename):
        started.set()
        await release.wait()
        yield b"audio"

    archive.stream_file = _slow_stream
    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    await asyncio.wait_for(started.wait(), timeout=2)
    await service.cancel(task_id, user_id="u1", is_admin=False)

    with pytest.raises(ValidationError, match="Wait for this download to stop"):
        await service.retry(task_id, user_id="u1", is_admin=False)

    handle = service._tasks[task_id]
    release.set()
    await asyncio.wait_for(handle, timeout=2)

    async def _stream(_identifier, _filename):
        yield b"audio"

    archive.stream_file = _stream
    await service.retry(task_id, user_id="u1", is_admin=False)
    assert (await _settle(service, store, task_id)).status == FreeMusicStatus.COMPLETED
    drop_import.create_job.assert_awaited_once()


@pytest.mark.asyncio
async def test_cancel_rejects_a_finished_task(tmp_path):
    service, store, _, _ = _build(tmp_path)
    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    await _settle(service, store, task_id)

    with pytest.raises(ValidationError):
        await service.cancel(task_id, user_id="u1", is_admin=False)


@pytest.mark.asyncio
async def test_a_non_owner_cannot_see_or_cancel_a_task(tmp_path):
    from core.exceptions import ResourceNotFoundError

    service, store, _, _ = _build(tmp_path)
    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
    )
    await _settle(service, store, task_id)

    with pytest.raises(ResourceNotFoundError):
        await service.get_task(task_id, user_id="intruder", is_admin=False)
    with pytest.raises(ResourceNotFoundError):
        await service.remove(task_id, user_id="intruder", is_admin=False)
    # an admin may
    assert await service.get_task(task_id, user_id="intruder", is_admin=True)


@pytest.mark.asyncio
async def test_retry_reruns_a_failed_task(tmp_path):
    service, store, archive, drop_import = _build(tmp_path)
    archive.search_audio = AsyncMock(side_effect=ArchiveError("down"))

    task_id = await service.request_album(
        user_id="u1",
        release_group_mbid="rg",
        artist_name="Brad Sucks",
        album_title="Guess Who's a Mess",
        track_count=10,
    )
    await _settle(service, store, task_id)

    archive.search_audio = AsyncMock(return_value=[_item()])
    archive.get_item_files = AsyncMock(
        return_value=(CC, _files("FLAC", 2, size=5000) + _files("VBR MP3", 10))
    )
    await service.retry(task_id, user_id="u1", is_admin=False)
    task = await _settle(service, store, task_id)

    assert task.status == FreeMusicStatus.COMPLETED
    assert task.track_count == 10
    assert task.format == "mp3"
    assert task.files_total == 10
    drop_import.create_job.assert_awaited_once()


@pytest.mark.asyncio
async def test_remove_deletes_terminal_history_without_touching_files(tmp_path):
    sse = AsyncMock()
    service, store, _, _ = _build(tmp_path, sse=sse)
    await store.create("done", "u1", "album", "rg", "A", "B")
    await store.update("done", status=FreeMusicStatus.COMPLETED)

    await service.remove("done", user_id="u1", is_admin=False)

    assert await store.get("done") is None
    assert sse.publish.await_args.args[2]["status"] == "removed"


@pytest.mark.asyncio
async def test_remove_refuses_active_history(tmp_path):
    service, store, _, _ = _build(tmp_path)
    await store.create("active", "u1", "album", "rg", "A", "B")

    with pytest.raises(ValidationError, match="Cancel this download"):
        await service.remove("active", user_id="u1", is_admin=False)

    assert await store.get("active") is not None


@pytest.mark.asyncio
async def test_clear_history_is_scoped_and_keeps_active_tasks(tmp_path):
    service, store, _, _ = _build(tmp_path)
    await store.create("u1-failed", "u1", "album", "rg-1", "A", "B")
    await store.create("u1-active", "u1", "album", "rg-2", "A", "B")
    await store.create("u2-done", "u2", "album", "rg-3", "A", "B")
    await store.update("u1-failed", status=FreeMusicStatus.FAILED)
    await store.update("u2-done", status=FreeMusicStatus.COMPLETED)

    assert await service.clear_history(user_id="u1", include_all=False) == 1
    assert await store.get("u1-failed") is None
    assert await store.get("u1-active") is not None
    assert await store.get("u2-done") is not None


@pytest.mark.asyncio
@pytest.mark.parametrize("clear_all", [False, True])
async def test_cancel_returns_its_snapshot_when_history_is_removed_concurrently(
    tmp_path, monkeypatch, clear_all
):
    service, store, _, _ = _build(tmp_path)
    await store.create("active", "u1", "album", "rg", "A", "B")
    service._lifecycle_locks["active"] = asyncio.Lock()

    async def remove_during_publish(*_args):
        if clear_all:
            await store.delete_terminal_tasks(user_id="u1")
        else:
            await store.delete_terminal("active")

    monkeypatch.setattr(service, "_publish", remove_during_publish)

    cancelled = await service.cancel("active", user_id="u1", is_admin=False)

    assert cancelled.status == FreeMusicStatus.CANCELLED
    assert await store.get("active") is None


@pytest.mark.asyncio
async def test_disabled_free_music_refuses_a_request(tmp_path):
    service, _, _, _ = _build(tmp_path, enabled=False)
    assert service.is_ready() is False
    with pytest.raises(ValidationError):
        await service.request_album(
            user_id="u1", release_group_mbid="rg", artist_name="A", album_title="B"
        )


@pytest.mark.asyncio
async def test_sweep_stale_fails_an_interrupted_task(tmp_path):
    service, store, _, _ = _build(tmp_path)
    await store.create("orphan", "u1", "album", "rg", "A", "B")

    await service.sweep_stale()

    assert (await store.get("orphan")).status == FreeMusicStatus.FAILED
