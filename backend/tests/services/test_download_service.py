"""DownloadService tests: library check, search pipeline, pick/cancel ownership +
bounds (domain exceptions), and the downloads-mount health check."""

import asyncio
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import (
    ConfigurationError,
    PermissionDeniedError,
    ResourceNotFoundError,
    ValidationError,
)
from core.task_registry import TaskRegistry
from infrastructure.queue.priority_queue import RequestPriority
from models.download import DownloadTask, ScoredCandidate, SearchJob
from repositories.protocols.download_client import DownloadSearchResult
from services.native.download_service import (
    ALREADY_IN_LIBRARY,
    DownloadService,
    check_downloads_mount,
)


def _candidate() -> ScoredCandidate:
    return ScoredCandidate(
        username="alice",
        parent_directory="A - B",
        files=[
            DownloadSearchResult(
                username="alice", filename="A - B/01.flac", parent_directory="A - B",
                size=1, extension="flac",
            )
        ],
        coherence=0.9,
        file_confidence=0.85,
        final_score=0.88,
        tier="auto",
    )


def _make_service(
    owner_id="u1", *, in_library=False, enabled=True,
    upgrade_allowed=False, quality_cutoff="lossless", held_tier=None,
    album_service=None, track_matcher=None,
):
    client = AsyncMock()
    # Search is the indexer's job after the split (D2); it returns IndexerResults,
    # which the service unwraps to soulseek DownloadSearchResults before scoring.
    indexer = AsyncMock()
    indexer.search_album.return_value = []
    scorer = AsyncMock()
    scorer.rank.return_value = [_candidate()]
    library = AsyncMock()
    library.has_album.return_value = in_library
    # The album gate is now tier-aware (step 8): a held album reports its worst tier, an
    # absent one reports None. With upgrades off (the default) any held tier still skips.
    library.album_quality_tier.return_value = held_tier if held_tier is not None else (
        "lossless" if in_library else None
    )
    store = AsyncMock()
    store.create_search_job.return_value = SearchJob(
        id="job1", user_id=owner_id, artist_name="A", album_title="B"
    )
    store.get_search_job.return_value = SearchJob(
        id="job1", user_id=owner_id, artist_name="A", album_title="B", release_group_mbid="rg"
    )
    store.get_search_job_candidates.return_value = [_candidate()]
    store.create_task.return_value = DownloadTask(id="task1", user_id=owner_id)
    # No orchestrator task parked on the job unless a test says otherwise (the pick
    # path resumes a parked task in preference to creating a new one).
    store.get_parked_task_for_search_job.return_value = None
    bus = AsyncMock()
    # dispatch() is sync (returns an asyncio.Task); cancel/retry are async
    orchestrator = MagicMock()
    orchestrator.cancel_task = AsyncMock()
    orchestrator.retry_task = AsyncMock(return_value="task-retry")
    service = DownloadService(
        client, indexer, scorer, library, store, bus, orchestrator, enabled=enabled,
        upgrade_allowed=upgrade_allowed, quality_cutoff=quality_cutoff,
        album_service=album_service, track_matcher=track_matcher,
    )
    # The 4th element is the search source (indexer) - the only thing tests poke for
    # search behaviour now that search is split off the download client.
    return service, store, bus, indexer, scorer, orchestrator


@pytest.mark.asyncio
async def test_disabled_client_blocks_every_download_entry_point():
    # When the download client is disabled in Settings, no path may start a
    # download - including retry_task, which re-dispatches a fresh task.
    service, store, _bus, _client, _scorer, orchestrator = _make_service(enabled=False)
    calls = [
        lambda: service.search_album("u1", "A", "B", release_group_mbid="rg"),
        lambda: service.request_album("u1", "rg", "A", "B"),
        lambda: service.request_track("u1", "rec", "A", "Track"),
        lambda: service.pick_candidate("u1", "job1", 0),
        lambda: service.retry_task("task1", "u1", "user"),
    ]
    for make in calls:
        with pytest.raises(ConfigurationError):
            await make()
    store.create_task.assert_not_called()
    orchestrator.retry_task.assert_not_called()


@pytest.mark.asyncio
async def test_search_album_already_in_library():
    service, store, *_ = _make_service(in_library=True)
    result = await service.search_album("u1", "A", "B", release_group_mbid="rg")
    assert result == ALREADY_IN_LIBRARY
    store.create_search_job.assert_not_called()


@pytest.mark.asyncio
async def test_search_album_below_cutoff_still_satisfied_for_non_upgrade():
    # D18 (origin-aware gate): only an origin='upgrade' request may re-fetch a
    # below-cutoff held album. A manual search is a user action - re-fetching here
    # would download bytes replace-on-import then refuses to place.
    service, store, *_ = _make_service(
        held_tier="mp3_320", upgrade_allowed=True, quality_cutoff="lossless"
    )
    result = await service.search_album("u1", "A", "B", release_group_mbid="rg")
    assert result == ALREADY_IN_LIBRARY
    store.create_search_job.assert_not_called()


@pytest.mark.asyncio
async def test_request_album_upgrade_origin_refetches_below_cutoff():
    # The upgrade path itself: origin='upgrade' + upgrades on + held below cutoff
    # -> not satisfied -> a task is created.
    service, store, *_ = _make_service(
        held_tier="mp3_320", upgrade_allowed=True, quality_cutoff="lossless"
    )
    store.get_active_task_for_album.return_value = None
    result = await service.request_album("u1", "rg", "A", "B", origin="upgrade")
    assert result == "task1"
    store.create_task.assert_called_once()
    assert store.create_task.call_args.kwargs["origin"] == "upgrade"


@pytest.mark.asyncio
async def test_request_album_upgrade_origin_blocked_when_upgrades_off():
    # The master toggle wins: origin='upgrade' with upgrade_allowed=False is satisfied.
    service, store, *_ = _make_service(
        held_tier="mp3_320", upgrade_allowed=False, quality_cutoff="lossless"
    )
    result = await service.request_album("u1", "rg", "A", "B", origin="upgrade")
    assert result == ALREADY_IN_LIBRARY
    store.create_task.assert_not_called()


@pytest.mark.asyncio
async def test_request_album_user_origin_never_refetches_held_album():
    service, store, *_ = _make_service(
        held_tier="mp3_192", upgrade_allowed=True, quality_cutoff="lossless"
    )
    result = await service.request_album("u1", "rg", "A", "B", origin="user")
    assert result == ALREADY_IN_LIBRARY
    store.create_task.assert_not_called()


@pytest.mark.asyncio
async def test_request_track_upgrade_uses_recording_floor():
    # Per-track upgrades gate on the RECORDING's best held tier (D12), not album-worst.
    service, store, *_ = _make_service(upgrade_allowed=True, quality_cutoff="lossless")
    service._library.recording_quality_tier = AsyncMock(return_value="mp3_320")
    store.get_active_task_for_track.return_value = None
    result = await service.request_track(
        "u1", "rec-1", "A", "Track", release_group_mbid="rg", origin="upgrade"
    )
    assert result == "task1"
    service._library.recording_quality_tier.assert_awaited_once_with("rec-1")

    # At the cutoff already -> nothing to upgrade.
    service._library.recording_quality_tier = AsyncMock(return_value="lossless")
    result = await service.request_track(
        "u1", "rec-1", "A", "Track", release_group_mbid="rg", origin="upgrade"
    )
    assert result == ALREADY_IN_LIBRARY


@pytest.mark.asyncio
async def test_search_album_upgrades_skip_once_cutoff_met():
    # Held at the cutoff -> satisfied even with upgrades on (don't upgrade past the cutoff).
    service, store, *_ = _make_service(
        held_tier="lossless", upgrade_allowed=True, quality_cutoff="lossless"
    )
    result = await service.search_album("u1", "A", "B", release_group_mbid="rg")
    assert result == ALREADY_IN_LIBRARY
    store.create_search_job.assert_not_called()


@pytest.mark.asyncio
async def test_search_album_creates_job_and_runs_search():
    service, store, bus, *_ = _make_service()
    job_id = await service.search_album("u1", "A", "B", release_group_mbid="rg")
    assert job_id == "job1"
    store.create_search_job.assert_called_once()
    # Await the registered background search deterministically (no wall-clock sleep).
    await TaskRegistry.get_instance().get_all()["search-job1"]
    store.set_search_job_candidates.assert_awaited_once()
    store.update_search_job_status.assert_any_await("job1", "completed")
    # SSE emits a 'searching' status event, then a 'complete' event with the payload.
    events = {call.args[1]: call.args[2] for call in bus.publish.await_args_list}
    assert events["status"] == {"status": "searching"}
    assert events["complete"]["candidate_count"] == 1
    assert events["complete"]["top_score"] == _candidate().final_score


@pytest.mark.asyncio
async def test_run_search_failure_marks_failed():
    service, store, bus, indexer, _, _ = _make_service()
    indexer.search_album.side_effect = RuntimeError("boom")
    await service._run_search("job1", "A", "B", None, 12)
    store.update_search_job_status.assert_any_await("job1", "failed", error="search failed")


@pytest.mark.asyncio
async def test_pick_candidate_creates_queued_task_and_matches():
    service, store, *_ = _make_service()
    task_id = await service.pick_candidate("u1", "job1", 0)
    assert task_id == "task1"
    store.create_task.assert_awaited_once()
    kwargs = store.create_task.await_args.kwargs
    assert kwargs["status"] == "queued"
    assert kwargs["source_username"] == "alice"
    assert kwargs["search_job_id"] == "job1"
    assert kwargs["candidate_index"] == 0
    store.update_search_job_status.assert_any_await("job1", "matched")


@pytest.mark.asyncio
async def test_pick_candidate_non_owner_raises_permission_denied():
    service, *_ = _make_service(owner_id="someone-else")
    with pytest.raises(PermissionDeniedError):
        await service.pick_candidate("u1", "job1", 0)


@pytest.mark.asyncio
async def test_pick_candidate_bad_index_raises_validation_error():
    service, *_ = _make_service()
    with pytest.raises(ValidationError):
        await service.pick_candidate("u1", "job1", 5)
    with pytest.raises(ValidationError):
        await service.pick_candidate("u1", "job1", -1)


@pytest.mark.asyncio
async def test_pick_candidate_missing_job_raises_not_found():
    service, store, *_ = _make_service()
    store.get_search_job.return_value = None
    with pytest.raises(ResourceNotFoundError):
        await service.pick_candidate("u1", "job1", 0)


# --- single-track identity threading + parked-task resume (2026-07-05 incident, P1) ---


def _single_album_service(*, tracks=None, total=1, fail=False):
    """AlbumService stub: get_album_tracks_info -> a 1-track release by default.
    MusicBrainz track lengths are MILLISECONDS."""
    svc = AsyncMock()
    if fail:
        svc.get_album_tracks_info.side_effect = RuntimeError("MB down")
        return svc
    if tracks is None:
        tracks = [
            SimpleNamespace(
                position=1, disc_number=1, title="the arrival",
                recording_id="rec-180ceef5", length=155556,
            )
        ]
    svc.get_album_tracks_info.return_value = SimpleNamespace(
        tracks=tracks, total_tracks=total
    )
    return svc


@pytest.mark.asyncio
async def test_request_album_threads_single_track_identity():
    """A 1-track release request carries the recording identity onto the task -
    title, recording MBID, canonical duration (ms -> s) - so search scores per-file
    and import verifies the canonical length."""
    service, store, *_ = _make_service(album_service=_single_album_service())
    store.get_active_task_for_album.return_value = None

    await service.request_album("u1", "rg", "Yan Qing", "the arrival")

    kwargs = store.create_task.await_args.kwargs
    assert kwargs["track_count"] == 1
    assert kwargs["track_title"] == "the arrival"
    assert kwargs["recording_mbid"] == "rec-180ceef5"
    assert kwargs["track_duration_seconds"] == pytest.approx(155.556)


@pytest.mark.asyncio
async def test_request_album_multi_track_release_threads_nothing():
    tracks = [SimpleNamespace(position=i, disc_number=1, title=f"T{i}",
                              recording_id=f"r{i}", length=200000) for i in (1, 2, 3)]
    service, store, *_ = _make_service(
        album_service=_single_album_service(tracks=tracks, total=3)
    )
    store.get_active_task_for_album.return_value = None

    await service.request_album("u1", "rg", "A", "B")

    kwargs = store.create_task.await_args.kwargs
    assert kwargs["track_count"] == 3
    assert kwargs["track_title"] is None
    assert kwargs["recording_mbid"] is None
    assert kwargs["track_duration_seconds"] is None


@pytest.mark.asyncio
async def test_request_album_mb_failure_never_blocks_the_download():
    # Identity threading is best-effort: MB down -> fields stay None, task still
    # created (the un-threaded task falls back to the album scorer).
    service, store, *_ = _make_service(album_service=_single_album_service(fail=True))
    store.get_active_task_for_album.return_value = None

    result = await service.request_album("u1", "rg", "A", "B")

    assert result == "task1"
    kwargs = store.create_task.await_args.kwargs
    assert kwargs["track_title"] is None
    assert kwargs["track_duration_seconds"] is None


@pytest.mark.asyncio
async def test_pick_candidate_resumes_parked_task_not_a_new_one():
    """R1 (incident review blocker): a pick on a parked orchestrator task must RESUME
    the original task - a fresh task drops the threaded identity (the import gates
    never arm) and the request linkage (terminal sync matches on the task id)."""
    service, store, _bus, _client, _scorer, orchestrator = _make_service()
    store.get_parked_task_for_search_job.return_value = DownloadTask(
        id="parked1", user_id="u1", download_type="album", track_count=1,
        track_title="the arrival", track_duration_seconds=155.556,
    )

    task_id = await service.pick_candidate("u1", "job1", 0)

    assert task_id == "parked1"
    store.create_task.assert_not_called()
    link = store.link_picked_candidate.await_args.kwargs
    assert link["task_id"] == "parked1"
    assert link["candidate_index"] == 0
    assert link["source_username"] == "alice"
    orchestrator.dispatch.assert_called_once_with("parked1")


@pytest.mark.asyncio
async def test_pick_candidate_standalone_single_rethreads_identity():
    # A standalone manual-search job (no parked task) carries no identity columns -
    # the pick re-resolves them so the canonical-duration/title gates still arm.
    service, store, *_ = _make_service(album_service=_single_album_service())
    store.get_search_job.return_value = SearchJob(
        id="job1", user_id="u1", artist_name="Yan Qing", album_title="the arrival",
        release_group_mbid="rg", track_count=1,
    )

    await service.pick_candidate("u1", "job1", 0)

    kwargs = store.create_task.await_args.kwargs
    assert kwargs["track_title"] == "the arrival"
    assert kwargs["recording_mbid"] == "rec-180ceef5"
    assert kwargs["track_duration_seconds"] == pytest.approx(155.556)


@pytest.mark.asyncio
async def test_search_soulseek_single_scores_via_track_matcher():
    """The manual-search lane applies the same 1-track rule as the auto path: a
    single scores per-file (track matcher), not with the folder scorer's
    count_ratio freebie."""
    track_matcher = MagicMock()
    track_matcher.rank = AsyncMock(return_value=[])
    service, _store, _bus, indexer, scorer, _orch = _make_service(
        track_matcher=track_matcher
    )

    from models.download import TargetAlbum

    target = TargetAlbum(artist_name="Yan Qing", album_title="the arrival", track_count=1)
    await service._search_soulseek(target, ("rec-180ceef5", "the arrival", 155.556))

    track_matcher.rank.assert_awaited_once()
    scorer.rank.assert_not_awaited()
    track_target = track_matcher.rank.await_args.args[0]
    assert track_target.track_title == "the arrival"
    assert track_target.duration_seconds == pytest.approx(155.556)


@pytest.mark.asyncio
async def test_search_soulseek_without_identity_uses_folder_scorer():
    service, _store, _bus, _indexer, scorer, _orch = _make_service()

    from models.download import TargetAlbum

    target = TargetAlbum(artist_name="A", album_title="B", track_count=1)
    await service._search_soulseek(target, None)

    scorer.rank.assert_awaited_once()


@pytest.mark.asyncio
async def test_cancel_search_owner():
    service, store, *_ = _make_service()
    assert await service.cancel_search("u1", "job1") is True
    store.update_search_job_status.assert_any_await("job1", "cancelled")


@pytest.mark.asyncio
async def test_cancel_search_non_owner_raises():
    service, *_ = _make_service(owner_id="someone-else")
    with pytest.raises(PermissionDeniedError):
        await service.cancel_search("u1", "job1")


def _make_service_with_mb(owner_id="u1"):
    """A service wired with a MusicBrainz matcher + repo for request_track tests."""
    service, store, bus, client, scorer, orchestrator = _make_service(owner_id)
    matcher = MagicMock()
    matcher.resolve_recording_to_release_group = AsyncMock(return_value="rg-x")
    mb = MagicMock()
    mb.get_release_group = AsyncMock(
        return_value=SimpleNamespace(
            title="Resolved Album", artist_name="Resolved Artist", year=2001, artist_id="artist-mbid-1"
        )
    )
    service._matcher = matcher
    service._mb = mb
    return service, store, client, orchestrator, matcher, mb


@pytest.mark.asyncio
async def test_request_album_already_in_library():
    service, store, *_ = _make_service(in_library=True)
    result = await service.request_album("u1", "rg", "A", "B")
    assert result == ALREADY_IN_LIBRARY
    store.create_task.assert_not_called()


@pytest.mark.asyncio
async def test_request_album_dedup_returns_existing_task():
    service, store, _bus, _client, _scorer, orchestrator = _make_service()
    store.get_active_task_for_album.return_value = DownloadTask(id="existing", user_id="u1")
    result = await service.request_album("u1", "rg", "A", "B")
    assert result == "existing"
    store.create_task.assert_not_called()
    orchestrator.dispatch.assert_not_called()


@pytest.mark.asyncio
async def test_request_album_creates_task_and_dispatches():
    service, store, _bus, _client, _scorer, orchestrator = _make_service()
    store.get_active_task_for_album.return_value = None
    store.create_task.return_value = DownloadTask(id="new-task", user_id="u1")
    result = await service.request_album("u1", "rg", "Artist", "Album", year=1999)
    assert result == "new-task"
    store.create_task.assert_awaited_once()
    orchestrator.dispatch.assert_called_once_with("new-task")


@pytest.mark.asyncio
async def test_request_album_clears_blocklist_on_new_request():
    # A manual re-request is an explicit "try again": clear the album's blocklist so a
    # release quarantined by an earlier failed attempt is reconsidered.
    service, store, _bus, _client, _scorer, _orch = _make_service()
    store.get_active_task_for_album.return_value = None
    store.create_task.return_value = DownloadTask(id="new-task", user_id="u1")
    await service.request_album("u1", "rg", "Artist", "Album", year=1999)
    store.delete_quarantine_for_album.assert_awaited_once_with("rg")


@pytest.mark.asyncio
async def test_request_album_track_request_does_not_clear_blocklist():
    # A per-track request must not wipe the whole album's blocklist.
    service, store, _bus, _client, _scorer, _orch = _make_service()
    store.get_active_task_for_track.return_value = None
    store.create_task.return_value = DownloadTask(id="t", user_id="u1")
    await service.request_album(
        "u1", "rg", "Artist", "Album", year=1999,
        recording_mbid="rec", download_type="track",
    )
    store.delete_quarantine_for_album.assert_not_called()


@pytest.mark.asyncio
async def test_cancel_album_retries_returns_count():
    # Removing an album cancels its pending auto-retries; the count flows back to the route.
    service, store, *_ = _make_service()
    store.cancel_album_auto_retries.return_value = ["t1", "t2"]
    count = await service.cancel_album_retries("rg")
    assert count == 2
    store.cancel_album_auto_retries.assert_awaited_once_with("rg")


@pytest.mark.asyncio
async def test_clear_finished_deletes_completed_and_cancelled():
    # The "Clear" bulk action hard-deletes the user's terminal completed+cancelled rows.
    service, store, *_ = _make_service()
    store.delete_tasks_by_status.return_value = 3
    cleared = await service.clear_finished("u1", "user")
    assert cleared == 3
    store.delete_tasks_by_status.assert_awaited_once_with(
        "u1", "user", ["completed", "cancelled"]
    )


@pytest.mark.asyncio
async def test_stop_all_retries_cancels_only_pending_retries():
    # Only failed/partial tasks with a PENDING next_retry_at ("wanted") are stopped;
    # exhausted ones are left for retry-all-failed.
    service, store, _bus, _client, _scorer, orch = _make_service()
    wanted = DownloadTask(id="w", user_id="u1", status="failed", retry_count=1)
    exhausted = DownloadTask(id="e", user_id="u1", status="failed", retry_count=6)
    partial = DownloadTask(id="p", user_id="u1", status="partial", retry_count=0)
    store.list_tasks_by_status.return_value = [wanted, exhausted, partial]
    pending = {"w", "p"}
    orch.next_retry_at = lambda task: 123.0 if task.id in pending else None

    stopped = await service.stop_all_retries("u1", "user")

    assert stopped == 2
    store.list_tasks_by_status.assert_awaited_once_with("u1", "user", ["failed", "partial"])
    assert {c.args[0] for c in orch.cancel_task.await_args_list} == {"w", "p"}


@pytest.mark.asyncio
async def test_retry_all_failed_retries_only_exhausted_failures():
    # Only failed tasks with NO pending next_retry_at (exhausted / auto-retry off) are
    # re-dispatched; tasks still scheduled to auto-retry are left alone.
    service, store, _bus, _client, _scorer, orch = _make_service()
    exhausted = DownloadTask(id="e", user_id="u1", status="failed", retry_count=6)
    wanted = DownloadTask(id="w", user_id="u1", status="failed", retry_count=1)
    store.list_tasks_by_status.return_value = [exhausted, wanted]
    orch.next_retry_at = lambda task: None if task.id == "e" else 123.0

    retried = await service.retry_all_failed("u1", "user")

    assert retried == 1
    store.list_tasks_by_status.assert_awaited_once_with("u1", "user", ["failed"])
    assert [c.args[0] for c in orch.retry_task.await_args_list] == ["e"]


@pytest.mark.asyncio
async def test_request_album_backfills_year_when_missing():
    # A compact request button sends no year; the service backfills it from the
    # release group so the album folder isn't created as "Album ()".
    service, store, _client, _orchestrator, _matcher, mb = _make_service_with_mb()
    store.get_active_task_for_album.return_value = None

    await service.request_album("u1", "rg", "Radiohead", "OK Computer")  # year omitted

    mb.get_release_group.assert_awaited_once_with("rg")
    assert store.create_task.await_args.kwargs["year"] == 2001  # from the mb stub


@pytest.mark.asyncio
async def test_request_album_year_backfill_failure_still_creates_task():
    # The year is a nicety: a MusicBrainz failure must not fail the download.
    service, store, _client, _orchestrator, _matcher, mb = _make_service_with_mb()
    store.get_active_task_for_album.return_value = None
    mb.get_release_group = AsyncMock(side_effect=RuntimeError("MB down"))

    result = await service.request_album("u1", "rg", "Radiohead", "OK Computer")

    assert result == "task1"  # request still succeeded
    assert store.create_task.await_args.kwargs["year"] is None  # degraded gracefully


def _with_album_service(service, *, total_tracks=12, raises=False):
    """Attach a stub AlbumService so the track-count backfill has a resolver."""
    album_service = MagicMock()
    if raises:
        album_service.get_album_tracks_info = AsyncMock(side_effect=RuntimeError("MB down"))
    else:
        album_service.get_album_tracks_info = AsyncMock(
            return_value=SimpleNamespace(total_tracks=total_tracks)
        )
    service._album_service = album_service
    return album_service


@pytest.mark.asyncio
async def test_request_album_backfills_track_count_from_musicbrainz():
    # The bug: every request path omits track_count, so the orchestrator's completeness
    # gate can't tell a 2-of-12 source from a full album and accepts the partial. The
    # service must backfill the count from MusicBrainz.
    service, store, _bus, _client, _scorer, _orch = _make_service()
    store.get_active_task_for_album.return_value = None
    album_service = _with_album_service(service, total_tracks=12)

    await service.request_album("u1", "rg", "Artist", "Album", year=1999)

    # user-path backfills stay at USER_INITIATED priority (the wanted scout is the
    # only caller that passes BACKGROUND_SYNC)
    album_service.get_album_tracks_info.assert_awaited_once_with(
        "rg", priority=RequestPriority.USER_INITIATED
    )
    assert store.create_task.await_args.kwargs["track_count"] == 12


@pytest.mark.asyncio
async def test_request_album_track_count_backfill_failure_still_creates_task():
    # The completeness target is best-effort: a MusicBrainz failure must not block the
    # download (it degrades to the unknown-count behaviour, never an error).
    service, store, _bus, _client, _scorer, _orch = _make_service()
    store.get_active_task_for_album.return_value = None
    _with_album_service(service, raises=True)

    result = await service.request_album("u1", "rg", "Artist", "Album", year=1999)

    assert result == "task1"
    assert store.create_task.await_args.kwargs["track_count"] is None


@pytest.mark.asyncio
async def test_request_album_keeps_explicit_track_count_without_mb_call():
    # A caller that already knows the count -> no MusicBrainz round-trip, value kept.
    service, store, _bus, _client, _scorer, _orch = _make_service()
    store.get_active_task_for_album.return_value = None
    album_service = _with_album_service(service, total_tracks=99)

    await service.request_album("u1", "rg", "Artist", "Album", year=1999, track_count=10)

    assert store.create_task.await_args.kwargs["track_count"] == 10
    album_service.get_album_tracks_info.assert_not_called()


@pytest.mark.asyncio
async def test_search_album_backfills_track_count_for_scorer_and_pick():
    # The manual search path feeds the scorer + the eventual picked task; backfill the
    # count there too so a partial folder can be down-ranked.
    service, store, *_ = _make_service()
    _with_album_service(service, total_tracks=8)

    await service.search_album("u1", "A", "B", release_group_mbid="rg")

    assert store.create_search_job.await_args.kwargs["track_count"] == 8


@pytest.mark.asyncio
async def test_request_track_already_in_library():
    service, store, _bus, _client, _scorer, orchestrator = _make_service()
    service._library.has_track.return_value = True
    result = await service.request_track("u1", "rec-1", "Artist", "Track")
    assert result == ALREADY_IN_LIBRARY
    store.create_task.assert_not_called()


@pytest.mark.asyncio
async def test_request_track_without_resolver_raises_validation():
    # _make_service has matcher=None; an unresolved release group is a 400.
    service, _store, _bus, _client, _scorer, _orch = _make_service()
    service._library.has_track.return_value = False
    with pytest.raises(ValidationError):
        await service.request_track("u1", "rec-1", "Artist", "Track")


@pytest.mark.asyncio
async def test_request_track_resolves_and_creates_track_task():
    service, store, _client, orchestrator, matcher, mb = _make_service_with_mb()
    service._library.has_track.return_value = False
    store.get_active_task_for_track.return_value = None
    store.create_task.return_value = DownloadTask(id="track-task", user_id="u1")

    result = await service.request_track("u1", "rec-1", "", "Airbag", duration_seconds=212)

    assert result == "track-task"
    matcher.resolve_recording_to_release_group.assert_awaited_once_with("rec-1")
    mb.get_release_group.assert_awaited_once_with("rg-x")
    kwargs = store.create_task.await_args.kwargs
    assert kwargs["download_type"] == "track"
    assert kwargs["track_count"] == 1
    assert kwargs["recording_mbid"] == "rec-1"
    assert kwargs["track_title"] == "Airbag"
    # the user-supplied duration is threaded onto the task for TrackMatcher
    assert kwargs["track_duration_seconds"] == 212
    orchestrator.dispatch.assert_called_once_with("track-task")


@pytest.mark.asyncio
async def test_request_track_dedup_is_recording_keyed_not_album_keyed():
    # A second, different track of the same album must NOT be swallowed by the
    # album-keyed dedup: track tasks dedup on the recording.
    service, store, _client, orchestrator, _matcher, _mb = _make_service_with_mb()
    service._library.has_track.return_value = False
    store.get_active_task_for_track.return_value = None
    store.get_active_task_for_album.return_value = DownloadTask(id="album-active", user_id="u1")
    store.create_task.return_value = DownloadTask(id="track-task", user_id="u1")

    result = await service.request_track("u1", "rec-2", "Artist", "Lucky", release_group_mbid="rg-x")

    assert result == "track-task"
    store.get_active_task_for_track.assert_awaited_once_with("rec-2", "u1")
    store.get_active_task_for_album.assert_not_called()


@pytest.mark.asyncio
async def test_cancel_task_delegates_to_orchestrator():
    service, _store, _bus, _client, _scorer, orchestrator = _make_service()
    await service.cancel_task("t1", "u1", "user")
    orchestrator.cancel_task.assert_awaited_once_with("t1", "u1", "user")


@pytest.mark.asyncio
async def test_retry_task_delegates_to_orchestrator():
    service, _store, _bus, _client, _scorer, orchestrator = _make_service()
    result = await service.retry_task("t1", "u1", "user")
    assert result == "task-retry"
    orchestrator.retry_task.assert_awaited_once_with("t1", "u1", "user")


@pytest.mark.asyncio
async def test_reimport_task_delegates_to_orchestrator():
    # Admin gating lives at the route (CurrentAdminDep); the facade just forwards the
    # task id. Guards the facade<->orchestrator signature from drifting apart.
    service, _store, _bus, _client, _scorer, orchestrator = _make_service()
    orchestrator.reimport_task = AsyncMock(return_value="reimported")
    result = await service.reimport_task("t1")
    assert result == "reimported"
    orchestrator.reimport_task.assert_awaited_once_with("t1")


@pytest.mark.asyncio
async def test_reimport_task_blocked_when_disabled():
    service, _store, _bus, _client, _scorer, orchestrator = _make_service(enabled=False)
    orchestrator.reimport_task = AsyncMock()
    with pytest.raises(ConfigurationError):
        await service.reimport_task("t1")
    orchestrator.reimport_task.assert_not_called()


def test_mount_not_set():
    assert check_downloads_mount(None, []).reason == "not_set"
    assert check_downloads_mount("", []).reason == "not_set"


def test_mount_missing(tmp_path):
    status = check_downloads_mount(tmp_path / "nope", [tmp_path])
    assert status.ok is False
    assert status.reason == "missing"


def test_mount_ok(tmp_path):
    downloads = tmp_path / "dl"
    downloads.mkdir()
    status = check_downloads_mount(downloads, [tmp_path])
    assert status.ok is True
    assert status.reason == "ok"


def test_mount_not_writable(tmp_path, monkeypatch):
    downloads = tmp_path / "dl"
    downloads.mkdir()
    monkeypatch.setattr("services.native.download_service.os.access", lambda p, m: False)
    status = check_downloads_mount(downloads, [tmp_path])
    assert status.ok is False
    assert status.reason == "not_writable"


def test_mount_different_filesystem(tmp_path, monkeypatch):
    downloads = tmp_path / "dl"
    downloads.mkdir()
    library = tmp_path / "lib"
    library.mkdir()

    class _FakeStat:
        def __init__(self, dev):
            self.st_dev = dev

    def fake_stat(self, *args, **kwargs):
        return _FakeStat(1 if self == downloads else 2)

    monkeypatch.setattr(Path, "stat", fake_stat)
    status = check_downloads_mount(downloads, [library])
    assert status.ok is False
    assert status.reason == "different_filesystem"


# -- held imports (import anyway / discard) --


def _held_service(store, file_processor):
    """A DownloadService with only the deps the held methods touch."""
    library = MagicMock()
    library.reconcile_with_filesystem = AsyncMock()
    orchestrator = MagicMock()
    orchestrator.settle_after_manual_import = AsyncMock()
    return DownloadService(
        MagicMock(), MagicMock(), MagicMock(), library, store, MagicMock(),
        orchestrator, file_processor=file_processor,
    )


async def _record_held(store, path, *, task_id="t-1"):
    return await store.record_held_import(
        user_id="user-a", held_path=str(path), reason="fingerprint_mismatch", source="usenet",
        source_task_id=task_id, release_group_mbid="rg-1", release_mbid=None, recording_mbid="rec-3",
        track_number=3, disc_number=1, track_title="You Shook Me", artist_name="Led Zeppelin",
        artist_mbid="678d88b2-87b0-403b-b63d-5da7465aecc3",
        album_title="Led Zeppelin", year=1969, original_filename="x.flac", file_format="flac",
        duration_seconds=388.0, evidence_title="X", evidence_artist="Y", evidence_score=0.9,
        naming_template="{album}/{track}",
    )


@pytest.mark.asyncio
async def test_discard_held_deletes_the_file(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "x.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    hid = await _record_held(store, held_file)
    svc = _held_service(store, MagicMock())

    await svc.discard_held(hid, "user-a", "user")

    assert not held_file.exists()  # the rejected file is ALWAYS removed (the requirement)
    assert await store.list_held_imports("user-a", "user") == []  # dropped from review
    assert await store.has_unresolved_held_for_task("t-1") is False  # auto-retry can resume


@pytest.mark.asyncio
async def test_import_held_places_and_resolves(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "x.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    hid = await _record_held(store, held_file)
    fp = MagicMock()
    fp.place_held_file = AsyncMock(return_value=Path("/music/Led Zeppelin/03 You Shook Me.flac"))
    svc = _held_service(store, fp)

    final_path = await svc.import_held(hid, "user-a", "user")

    assert final_path.endswith("03 You Shook Me.flac")
    fp.place_held_file.assert_awaited_once()
    assert await store.list_held_imports("user-a", "user") == []  # resolved -> off the review list
    assert await store.has_unresolved_held_for_task("t-1") is False
    # the source task is re-measured so a completed album stops showing a phantom retry
    svc._orchestrator.settle_after_manual_import.assert_awaited_once_with("t-1")


@pytest.mark.asyncio
async def test_import_held_unknown_id_raises_not_found(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    svc = _held_service(store, MagicMock())
    with pytest.raises(ResourceNotFoundError):
        await svc.import_held(999, "user-a", "user")


@pytest.mark.asyncio
async def test_purge_album_downloads_clears_tasks_held_and_quarantine(tmp_path):
    # Removing an album must clear its whole download-side footprint: cancel retries (no
    # resurrection), delete held tracks + their files, and drop blocklist entries.
    import sqlite3
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    db_path = tmp_path / "library.db"
    conn = sqlite3.connect(db_path)
    conn.execute("CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)")
    conn.execute("INSERT OR IGNORE INTO auth_users VALUES ('user-a','a','user')")
    conn.commit()
    conn.close()
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    RG = "rg-1"
    task = await store.create_task(
        user_id="user-a", release_group_mbid=RG, artist_name="a", album_title="b", source="usenet"
    )
    await store.update_status(task.id, "partial", files_completed=7)  # would auto-retry
    await store.record_quarantine(
        source="usenet", identity="bad-release", reason="verify_failed", release_group_mbid=RG
    )
    held_file = tmp_path / "held" / "x.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    await store.record_held_import(
        user_id="user-a", held_path=str(held_file), reason="fingerprint_mismatch", source="usenet",
        source_task_id=task.id, release_group_mbid=RG, release_mbid=None, recording_mbid=None,
        track_number=3, disc_number=1, track_title="t", artist_name="a", artist_mbid=None,
        album_title="b", year=None,
        original_filename="x.flac", file_format="flac", duration_seconds=1.0, evidence_title=None,
        evidence_artist=None, evidence_score=None, naming_template=None,
    )
    svc = _held_service(store, MagicMock())

    await svc.purge_album_downloads(RG)

    assert (await store.get_task(task.id)).status == "cancelled"  # no auto-retry resurrection
    assert await store.list_held_imports("user-a", "user") == []  # held rows gone
    assert not held_file.exists()  # held file deleted from disk
    assert await store.list_quarantine() == []  # blocklist cleared


@pytest.mark.asyncio
async def test_storage_admission_blocks_request_album_before_task_creation():
    """Layer 2 (Feature C): an over-cap user is rejected at request_album with no
    task created; an upgrade with the same quota service is exempt (checked by the
    service passing origin through)."""
    from core.exceptions import ValidationError as VErr

    service, store, *_ = _make_service()
    quota = AsyncMock()
    quota.check_storage_admission.side_effect = VErr("Library storage limit reached (10.0 / 10 GB)")
    service._quota = quota
    store.get_active_task_for_album.return_value = None

    with pytest.raises(VErr):
        await service.request_album("u1", "rg", "A", "B")
    store.create_task.assert_not_called()
    quota.check_storage_admission.assert_awaited_once_with("u1", "user")


@pytest.mark.asyncio
async def test_storage_admission_blocks_pick_candidate():
    """The manual-pick path is a task-creation site too - it gets the same gate."""
    from core.exceptions import ValidationError as VErr

    service, store, *_ = _make_service()
    quota = AsyncMock()
    quota.check_storage_admission.side_effect = VErr("Your storage budget is full (5.0 / 5 GB)")
    service._quota = quota

    with pytest.raises(VErr):
        await service.pick_candidate("u1", "job1", 0)
    store.create_task.assert_not_called()


@pytest.mark.asyncio
async def test_upgrade_origin_passes_through_admission():
    service, store, *_ = _make_service(
        held_tier="mp3_192", upgrade_allowed=True, quality_cutoff="lossless"
    )
    quota = AsyncMock()
    service._quota = quota
    store.get_active_task_for_album.return_value = None

    await service.request_album("u1", "rg", "A", "B", origin="upgrade")

    quota.check_storage_admission.assert_awaited_once_with("u1", "upgrade")


@pytest.mark.asyncio
async def test_upgrade_origin_never_fetches_an_unheld_album():
    """An un-held album is no upgrade target - origin='upgrade' would
    otherwise bypass the caps/quotas (upgrades are exempt) and the master toggle."""
    service, store, *_ = _make_service(
        held_tier=None, upgrade_allowed=True, quality_cutoff="lossless"
    )
    service._library.album_quality_tier = AsyncMock(return_value=None)

    result = await service.request_album("u1", "rg", "A", "B", origin="upgrade")

    assert result == ALREADY_IN_LIBRARY
    store.create_task.assert_not_called()


@pytest.mark.asyncio
async def test_upgrade_origin_never_fetches_an_unheld_recording():
    service, store, *_ = _make_service(upgrade_allowed=True, quality_cutoff="lossless")
    service._library.recording_quality_tier = AsyncMock(return_value=None)

    result = await service.request_track(
        "u1", "rec-1", "A", "Track", release_group_mbid="rg", origin="upgrade"
    )

    assert result == ALREADY_IN_LIBRARY
    store.create_task.assert_not_called()
