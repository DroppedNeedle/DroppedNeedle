"""DownloadService tests: library check, search pipeline, pick/cancel ownership +
bounds (domain exceptions), and the downloads-mount health check."""

import asyncio
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import (
    AutomaticManagementHoldError,
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
    _ordinary_held_action_locks,
    _ordinary_held_action_lock_users,
    check_downloads_mount,
)


def _candidate() -> ScoredCandidate:
    return ScoredCandidate(
        username="alice",
        parent_directory="A - B",
        files=[
            DownloadSearchResult(
                username="alice",
                filename="A - B/01.flac",
                parent_directory="A - B",
                size=1,
                extension="flac",
            )
        ],
        coherence=0.9,
        file_confidence=0.85,
        final_score=0.88,
        tier="auto",
    )


def _make_service(
    owner_id="u1",
    *,
    in_library=False,
    enabled=True,
    upgrade_allowed=False,
    quality_cutoff="lossless",
    held_tier=None,
    album_service=None,
    track_matcher=None,
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
    library.album_quality_tier.return_value = (
        held_tier if held_tier is not None else ("lossless" if in_library else None)
    )
    store = AsyncMock()
    store.create_search_job.return_value = SearchJob(
        id="job1", user_id=owner_id, artist_name="A", album_title="B"
    )
    store.get_search_job.return_value = SearchJob(
        id="job1",
        user_id=owner_id,
        artist_name="A",
        album_title="B",
        release_group_mbid="rg",
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
        client,
        indexer,
        scorer,
        library,
        store,
        bus,
        orchestrator,
        enabled=enabled,
        upgrade_allowed=upgrade_allowed,
        quality_cutoff=quality_cutoff,
        album_service=album_service,
        track_matcher=track_matcher,
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
async def test_activity_summary_delegates_user_scope_to_store():
    service, store, *_rest = _make_service()
    expected = object()
    store.get_activity_summary.return_value = expected

    result = await service.get_activity_summary("u1", "user")

    assert result is expected
    store.get_activity_summary.assert_awaited_once_with("u1", "user")


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
async def test_get_search_job_projects_current_order_with_original_pick_index():
    service, store, *_ = _make_service()
    manual = ScoredCandidate(
        username="manual",
        parent_directory="B Deluxe",
        files=[
            DownloadSearchResult(
                username="manual",
                filename="Music/A/B Deluxe/01.flac",
                parent_directory="B Deluxe",
                size=1,
                extension="flac",
            )
        ],
        final_score=0.59,
        tier="manual",
    )
    store.get_search_job_candidates.return_value = [manual, _candidate()]

    _job, candidates = await service.get_search_job("u1", "job1")

    assert [candidate.username for candidate in candidates] == ["alice", "manual"]
    assert [candidate.candidate_index for candidate in candidates] == [1, 0]


@pytest.mark.asyncio
async def test_run_search_failure_marks_failed():
    service, store, bus, indexer, _, _ = _make_service()
    indexer.search_album.side_effect = RuntimeError("boom")
    await service._run_search("job1", "A", "B", None, 12)
    store.update_search_job_status.assert_any_await(
        "job1", "failed", error="search failed"
    )


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


# single-track identity threading + parked-task resume (2026-07-05 incident, P1)


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
                position=1,
                disc_number=1,
                title="the arrival",
                recording_id="rec-180ceef5",
                release_track_id="release-track-1",
                length=155556,
            )
        ]
    for index, track in enumerate(tracks, start=1):
        if not hasattr(track, "release_track_id"):
            track.release_track_id = f"release-track-{index}"
    info = SimpleNamespace(
        tracks=tracks, total_tracks=total, selected_release_mbid="release-1"
    )
    svc.get_album_tracks_info.return_value = info
    svc.get_exact_edition_tracks_info.return_value = info
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
    assert kwargs["release_mbid"] == "release-1"
    assert kwargs["release_track_mbid"] == "release-track-1"
    assert kwargs["track_number"] == 1
    assert kwargs["disc_number"] == 1
    assert kwargs["track_duration_seconds"] == pytest.approx(155.556)


@pytest.mark.asyncio
async def test_request_album_multi_track_release_threads_nothing():
    tracks = [
        SimpleNamespace(
            position=i,
            disc_number=1,
            title=f"T{i}",
            recording_id=f"r{i}",
            length=200000,
        )
        for i in (1, 2, 3)
    ]
    service, store, *_ = _make_service(
        album_service=_single_album_service(tracks=tracks, total=3)
    )
    store.get_active_task_for_album.return_value = None

    await service.request_album("u1", "rg", "A", "B")

    kwargs = store.create_task.await_args.kwargs
    assert kwargs["track_count"] == 3
    assert kwargs["release_mbid"] == "release-1"
    assert kwargs["release_track_mbid"] is None
    assert kwargs["track_title"] is None
    assert kwargs["recording_mbid"] is None
    assert kwargs["track_duration_seconds"] is None


@pytest.mark.asyncio
async def test_request_album_explicit_edition_never_uses_fallback_resolver():
    album_service = _single_album_service()
    service, store, *_ = _make_service(album_service=album_service)
    store.get_active_task_for_album.return_value = None

    await service.request_album(
        "u1", "rg", "Yan Qing", "the arrival", release_mbid="release-explicit"
    )

    album_service.get_exact_edition_tracks_info.assert_awaited_once_with(
        "rg",
        "release-explicit",
        priority=RequestPriority.USER_INITIATED,
    )
    album_service.get_album_tracks_info.assert_not_awaited()
    assert store.create_task.await_args.kwargs["release_mbid"] == "release-explicit"


@pytest.mark.asyncio
async def test_request_album_mb_failure_starts_no_download_without_exact_identity():
    service, store, *_ = _make_service(album_service=_single_album_service(fail=True))
    store.get_active_task_for_album.return_value = None

    with pytest.raises(ValidationError, match="exact MusicBrainz edition"):
        await service.request_album("u1", "rg", "A", "B")

    store.create_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_pick_candidate_resumes_parked_task_not_a_new_one():
    """R1 (incident review blocker): a pick on a parked orchestrator task must RESUME
    the original task - a fresh task drops the threaded identity (the import gates
    never arm) and the request linkage (terminal sync matches on the task id)."""
    service, store, _bus, _client, _scorer, orchestrator = _make_service()
    store.get_parked_task_for_search_job.return_value = DownloadTask(
        id="parked1",
        user_id="u1",
        download_type="album",
        track_count=1,
        track_title="the arrival",
        track_duration_seconds=155.556,
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
        id="job1",
        user_id="u1",
        artist_name="Yan Qing",
        album_title="the arrival",
        release_group_mbid="rg",
        track_count=1,
    )

    await service.pick_candidate("u1", "job1", 0)

    kwargs = store.create_task.await_args.kwargs
    assert kwargs["release_mbid"] == "release-1"
    assert kwargs["release_track_mbid"] == "release-track-1"
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

    target = TargetAlbum(
        artist_name="Yan Qing", album_title="the arrival", track_count=1
    )
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
            title="Resolved Album",
            artist_name="Resolved Artist",
            year=2001,
            artist_id="artist-mbid-1",
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
    store.get_active_task_for_album.return_value = DownloadTask(
        id="existing", user_id="u1"
    )
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
        "u1",
        "rg",
        "Artist",
        "Album",
        year=1999,
        recording_mbid="rec",
        download_type="track",
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
    store.list_tasks_by_status.assert_awaited_once_with(
        "u1", "user", ["failed", "partial"]
    )
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
        album_service.get_album_tracks_info = AsyncMock(
            side_effect=RuntimeError("MB down")
        )
    else:
        tracks = [
            SimpleNamespace(
                position=index,
                disc_number=1,
                title=f"Track {index}",
                recording_id=f"recording-{index}",
                release_track_id=f"release-track-{index}",
                length=180_000,
            )
            for index in range(1, total_tracks + 1)
        ]
        album_service.get_album_tracks_info = AsyncMock(
            return_value=SimpleNamespace(
                total_tracks=total_tracks,
                tracks=tracks,
                selected_release_mbid="release-1",
            )
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
async def test_request_album_track_map_failure_starts_no_download():
    service, store, _bus, _client, _scorer, _orch = _make_service()
    store.get_active_task_for_album.return_value = None
    _with_album_service(service, raises=True)

    with pytest.raises(ValidationError, match="exact MusicBrainz edition"):
        await service.request_album("u1", "rg", "Artist", "Album", year=1999)

    store.create_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_request_album_verifies_exact_map_even_with_explicit_track_count():
    service, store, _bus, _client, _scorer, _orch = _make_service()
    store.get_active_task_for_album.return_value = None
    album_service = _with_album_service(service, total_tracks=99)

    await service.request_album(
        "u1", "rg", "Artist", "Album", year=1999, track_count=10
    )

    assert store.create_task.await_args.kwargs["track_count"] == 99
    album_service.get_album_tracks_info.assert_awaited_once()


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

    result = await service.request_track(
        "u1", "rec-1", "", "Airbag", duration_seconds=212
    )

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
async def test_request_track_persists_exact_release_track_mapping():
    album_service = _single_album_service()
    service, store, *_ = _make_service(album_service=album_service)
    service._library.has_track.return_value = False
    store.get_active_task_for_track.return_value = None

    await service.request_track(
        "u1",
        "rec-180ceef5",
        "Yan Qing",
        "the arrival",
        release_group_mbid="rg",
    )

    kwargs = store.create_task.await_args.kwargs
    assert kwargs["release_mbid"] == "release-1"
    assert kwargs["release_track_mbid"] == "release-track-1"
    assert (kwargs["disc_number"], kwargs["track_number"]) == (1, 1)


@pytest.mark.asyncio
async def test_request_track_dedup_is_recording_keyed_not_album_keyed():
    # A second, different track of the same album must NOT be swallowed by the
    # album-keyed dedup: track tasks dedup on the recording.
    service, store, _client, orchestrator, _matcher, _mb = _make_service_with_mb()
    service._library.has_track.return_value = False
    store.get_active_task_for_track.return_value = None
    store.get_active_task_for_album.return_value = DownloadTask(
        id="album-active", user_id="u1"
    )
    store.create_task.return_value = DownloadTask(id="track-task", user_id="u1")

    result = await service.request_track(
        "u1", "rec-2", "Artist", "Lucky", release_group_mbid="rg-x"
    )

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
    monkeypatch.setattr(
        "services.native.download_service.os.access", lambda p, m: False
    )
    status = check_downloads_mount(downloads, [tmp_path])
    assert status.ok is False
    assert status.reason == "not_writable"


def test_mount_different_filesystem(tmp_path, monkeypatch):
    downloads = tmp_path / "dl"
    downloads.mkdir()
    library = tmp_path / "lib"
    library.mkdir()

    monkeypatch.setattr(
        "services.native.download_service.check_move_boundary",
        lambda _source, _destination: SimpleNamespace(
            move_supported=False, reason="different_filesystem"
        ),
    )
    status = check_downloads_mount(downloads, [library])
    assert status.ok is True
    assert status.move_supported is False
    assert status.reason == "different_filesystem"


def test_mount_reason_prefers_a_known_boundary_over_a_stat_error(tmp_path, monkeypatch):
    downloads = tmp_path / "dl"
    downloads.mkdir()
    libraries = [tmp_path / "first", tmp_path / "second"]
    for library in libraries:
        library.mkdir()
    reasons = iter(("stat_error", "different_filesystem"))
    monkeypatch.setattr(
        "services.native.download_service.check_move_boundary",
        lambda _source, _destination: SimpleNamespace(
            move_supported=False, reason=next(reasons)
        ),
    )

    status = check_downloads_mount(downloads, libraries)

    assert status.ok is True
    assert status.move_supported is False
    assert status.reason == "different_filesystem"


# held imports (import anyway / discard)


def _held_service(store, file_processor, library_reconciler=None, album_service=None):
    """A DownloadService with only the deps the held methods touch."""
    library = MagicMock()
    library.reconcile_with_filesystem = AsyncMock()
    orchestrator = MagicMock()
    orchestrator.cancel_task = AsyncMock()
    orchestrator.settle_after_manual_import = AsyncMock()
    return DownloadService(
        MagicMock(),
        MagicMock(),
        MagicMock(),
        library,
        store,
        MagicMock(),
        orchestrator,
        file_processor=file_processor,
        library_reconciler=library_reconciler,
        album_service=album_service,
    )


async def _record_held(
    store,
    path,
    *,
    task_id="t-1",
    reason="fingerprint_mismatch",
    origin="user",
    track_number=3,
    release_mbid=None,
    release_track_mbid=None,
    recording_mbid="rec-3",
    management_retry_count=0,
    management_next_retry_at=None,
):
    return await store.record_held_import(
        user_id="user-a",
        held_path=str(path),
        reason=reason,
        origin=origin,
        source="usenet",
        source_task_id=task_id,
        release_group_mbid="rg-1",
        release_mbid=release_mbid,
        release_track_mbid=release_track_mbid,
        recording_mbid=recording_mbid,
        track_number=track_number,
        disc_number=1,
        track_title="You Shook Me",
        artist_name="Led Zeppelin",
        artist_mbid="678d88b2-87b0-403b-b63d-5da7465aecc3",
        album_title="Led Zeppelin",
        year=1969,
        original_filename="x.flac",
        file_format="flac",
        duration_seconds=388.0,
        evidence_title="X",
        evidence_artist="Y",
        evidence_score=0.9,
        naming_template="{album}/{track}",
        management_retry_count=management_retry_count,
        management_next_retry_at=management_next_retry_at,
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

    assert (
        not held_file.exists()
    )  # the rejected file is ALWAYS removed (the requirement)
    assert await store.list_held_imports("user-a", "user") == []  # dropped from review
    assert (
        await store.has_unresolved_held_for_task("t-1") is False
    )  # auto-retry can resume


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
    fp.place_held_file = AsyncMock(
        return_value=Path("/music/Led Zeppelin/03 You Shook Me.flac")
    )
    reconciler = MagicMock()
    reconciler.reconcile_with_filesystem = AsyncMock()
    svc = _held_service(store, fp, reconciler)

    final_path = await svc.import_held(hid, "user-a", "user")

    assert final_path.endswith("03 You Shook Me.flac")
    fp.place_held_file.assert_awaited_once()
    reconciler.reconcile_with_filesystem.assert_awaited_once_with(
        targets=[Path("/music/Led Zeppelin")]
    )
    assert (
        await store.list_held_imports("user-a", "user") == []
    )  # resolved -> off the review list
    assert await store.has_unresolved_held_for_task("t-1") is False
    # the source task is re-measured so a completed album stops showing a phantom retry
    svc._orchestrator.settle_after_manual_import.assert_awaited_once_with("t-1")


@pytest.mark.asyncio
async def test_edition_conversion_held_actions_stay_in_conversion_workflow(
    tmp_path,
):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "conversion.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    held_id = await _record_held(store, held_file, origin="edition_conversion")
    processor = MagicMock()
    svc = _held_service(store, processor)

    for action in (svc.import_held, svc.discard_held):
        with pytest.raises(
            ValidationError, match="dedicated edition conversion workflow"
        ):
            await action(held_id, "user-a", "user")

    assert held_file.exists()
    held = await store.get_held_import(held_id, "user-a", "user")
    assert held is not None and held.status == "held"
    processor.place_held_file.assert_not_called()


@pytest.mark.asyncio
async def test_upgrade_held_import_remains_allowed(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "upgrade.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    held_id = await _record_held(store, held_file, origin="upgrade")
    processor = MagicMock()
    processor.place_held_file = AsyncMock(return_value=Path("/music/upgrade.flac"))
    reconciler = MagicMock()
    reconciler.reconcile_with_filesystem = AsyncMock()
    svc = _held_service(store, processor, reconciler)

    result = await svc.import_held(held_id, "user-a", "user")

    assert result == "/music/upgrade.flac"
    processor.place_held_file.assert_awaited_once()
    assert await store.get_held_import(held_id, "user-a", "user") is None


@pytest.mark.asyncio
async def test_import_held_without_library_root_propagates_and_stays_held(tmp_path):
    """No library root configured: the ConfigurationError propagates to the route's
    400 mapping and the row stays held - the user restores a root and retries."""
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "x.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    hid = await _record_held(store, held_file)
    fp = MagicMock()
    fp.place_held_file = AsyncMock(
        side_effect=ConfigurationError(
            "No library root is configured - restore one in Settings → Library, then try again."
        )
    )
    svc = _held_service(store, fp)
    store.resolve_held_import = AsyncMock()

    with pytest.raises(ConfigurationError, match="No library root is configured"):
        await svc.import_held(hid, "user-a", "user")

    store.resolve_held_import.assert_not_awaited()  # NOT resolved: retry stays possible
    svc._orchestrator.settle_after_manual_import.assert_not_awaited()
    held = await store.list_held_imports("user-a", "user")
    assert [value.id for value in held] == [hid]
    assert await store.has_unresolved_held_for_task("t-1") is True
    assert _ordinary_held_action_locks == {}
    assert _ordinary_held_action_lock_users == {}


@pytest.mark.asyncio
async def test_automatic_management_hold_propagates_and_stays_held(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "blocked.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    held_id = await _record_held(store, held_file)
    processor = MagicMock()
    processor.place_held_file = AsyncMock(
        side_effect=AutomaticManagementHoldError(
            "TRACK_NOT_MAPPED",
            "provider secret /srv/private/profile.py path /library/blocked.flac",
        )
    )
    svc = _held_service(store, processor)
    store.resolve_held_import = AsyncMock()

    with pytest.raises(AutomaticManagementHoldError):
        await svc.import_held(held_id, "user-a", "user")

    store.resolve_held_import.assert_not_awaited()
    assert held_file.exists()
    held = await store.get_held_import(held_id, "user-a", "user")
    assert held is not None and held.status == "held"


@pytest.mark.parametrize("winner", ["import", "discard"])
@pytest.mark.asyncio
async def test_ordinary_held_import_and_discard_serialize_by_held_id(tmp_path, winner):
    import sqlite3
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    assert _ordinary_held_action_locks == {}
    assert _ordinary_held_action_lock_users == {}
    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "track.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"held-bytes")
    held_id = await _record_held(store, held_file, task_id="ordinary-held")
    destination = tmp_path / "library" / "track.flac"
    entered = asyncio.Event()
    release = asyncio.Event()

    async def import_effect(held):  # noqa: ANN001
        entered.set()
        await release.wait()
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(Path(held.held_path).read_bytes())
        Path(held.held_path).unlink()
        return destination

    async def discard_effect(held):  # noqa: ANN001
        entered.set()
        await release.wait()
        for value in held:
            Path(value.held_path).unlink(missing_ok=True)

    import_processor = MagicMock()
    import_processor.place_held_file = AsyncMock(side_effect=import_effect)
    discard_processor = MagicMock()
    discard_processor.place_held_file = AsyncMock()
    service_import = _held_service(store, import_processor)
    service_discard = _held_service(store, discard_processor)
    delete = AsyncMock(side_effect=discard_effect)
    service_discard._delete_discarded_held_files = delete

    if winner == "import":
        first = asyncio.create_task(
            service_import.import_held(held_id, "user-a", "user")
        )
    else:
        first = asyncio.create_task(
            service_discard.discard_held(held_id, "user-a", "user")
        )
    await entered.wait()
    if winner == "import":
        second = asyncio.create_task(
            service_discard.discard_held(held_id, "user-a", "user")
        )
    else:
        second = asyncio.create_task(
            service_import.import_held(held_id, "user-a", "user")
        )
    await asyncio.sleep(0)

    assert not second.done()
    assert _ordinary_held_action_lock_users[held_id] == 2
    if winner == "import":
        import_processor.place_held_file.assert_awaited_once()
        discard_processor.place_held_file.assert_not_awaited()
        delete.assert_not_awaited()
    else:
        import_processor.place_held_file.assert_not_awaited()
        discard_processor.place_held_file.assert_not_awaited()
        assert delete.await_count == 1

    release.set()
    await first
    with pytest.raises(ResourceNotFoundError):
        await second

    with sqlite3.connect(store.db_path) as connection:
        status = connection.execute(
            "SELECT status FROM held_imports WHERE id=?", (held_id,)
        ).fetchone()[0]
    assert status == ("imported" if winner == "import" else "discarded")
    assert not held_file.exists()
    if winner == "import":
        assert destination.read_bytes() == b"held-bytes"
    else:
        assert not destination.exists()
    assert _ordinary_held_action_locks == {}
    assert _ordinary_held_action_lock_users == {}


@pytest.mark.asyncio
async def test_duplicate_ordinary_held_imports_have_one_side_effect(
    tmp_path,
):
    import sqlite3
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    assert _ordinary_held_action_locks == {}
    assert _ordinary_held_action_lock_users == {}
    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "track.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"held-bytes")
    held_id = await _record_held(store, held_file, task_id="ordinary-held")
    destination = tmp_path / "library" / "track.flac"
    entered = asyncio.Event()
    release = asyncio.Event()

    async def import_effect(held):  # noqa: ANN001
        entered.set()
        await release.wait()
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(Path(held.held_path).read_bytes())
        Path(held.held_path).unlink()
        return destination

    first_processor = MagicMock()
    first_processor.place_held_file = AsyncMock(side_effect=import_effect)
    second_processor = MagicMock()
    second_processor.place_held_file = AsyncMock()
    first_service = _held_service(store, first_processor)
    second_service = _held_service(store, second_processor)

    first = asyncio.create_task(first_service.import_held(held_id, "user-a", "user"))
    await entered.wait()
    second = asyncio.create_task(second_service.import_held(held_id, "user-a", "user"))
    await asyncio.sleep(0)

    assert not second.done()
    assert _ordinary_held_action_lock_users[held_id] == 2
    assert first_processor.place_held_file.await_count == 1
    assert second_processor.place_held_file.await_count == 0

    release.set()
    assert await first == str(destination)
    with pytest.raises(ResourceNotFoundError):
        await second

    with sqlite3.connect(store.db_path) as connection:
        status = connection.execute(
            "SELECT status FROM held_imports WHERE id=?", (held_id,)
        ).fetchone()[0]
    assert (
        first_processor.place_held_file.await_count
        + second_processor.place_held_file.await_count
        == 1
    )
    assert status == "imported"
    assert not held_file.exists()
    assert destination.read_bytes() == b"held-bytes"
    assert _ordinary_held_action_locks == {}
    assert _ordinary_held_action_lock_users == {}


@pytest.mark.asyncio
async def test_ordinary_held_action_registry_cleans_up_cancelled_waiter(tmp_path):
    import sqlite3
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    assert _ordinary_held_action_locks == {}
    assert _ordinary_held_action_lock_users == {}
    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "track.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"held-bytes")
    held_id = await _record_held(store, held_file, task_id="ordinary-held")
    destination = tmp_path / "library" / "track.flac"
    entered = asyncio.Event()
    release = asyncio.Event()

    async def import_effect(held):  # noqa: ANN001
        entered.set()
        await release.wait()
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(Path(held.held_path).read_bytes())
        Path(held.held_path).unlink()
        return destination

    first_processor = MagicMock()
    first_processor.place_held_file = AsyncMock(side_effect=import_effect)
    second_processor = MagicMock()
    second_processor.place_held_file = AsyncMock()
    first_service = _held_service(store, first_processor)
    second_service = _held_service(store, second_processor)

    first = asyncio.create_task(first_service.import_held(held_id, "user-a", "user"))
    await entered.wait()
    second = asyncio.create_task(second_service.import_held(held_id, "user-a", "user"))
    await asyncio.sleep(0)
    assert _ordinary_held_action_lock_users[held_id] == 2

    second.cancel()
    with pytest.raises(asyncio.CancelledError):
        await second
    assert _ordinary_held_action_lock_users[held_id] == 1
    assert first_processor.place_held_file.await_count == 1
    assert second_processor.place_held_file.await_count == 0

    release.set()
    assert await first == str(destination)
    with sqlite3.connect(store.db_path) as connection:
        status = connection.execute(
            "SELECT status FROM held_imports WHERE id=?", (held_id,)
        ).fetchone()[0]
    assert status == "imported"
    assert not held_file.exists()
    assert destination.read_bytes() == b"held-bytes"
    assert _ordinary_held_action_locks == {}
    assert _ordinary_held_action_lock_users == {}


@pytest.mark.asyncio
async def test_import_held_unknown_id_raises_not_found(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    svc = _held_service(store, MagicMock())
    with pytest.raises(ResourceNotFoundError):
        await svc.import_held(999, "user-a", "user")


@pytest.mark.asyncio
async def test_management_hold_retries_the_complete_unit_and_settles_task(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_files = []
    for track in (1, 2):
        held_file = tmp_path / "held" / f"{track}.flac"
        held_file.parent.mkdir(exist_ok=True)
        held_file.write_bytes(b"audio")
        await _record_held(
            store,
            held_file,
            task_id="managed-task",
            reason="management:PROFILE_CHANGED",
            track_number=track,
            release_mbid="release-1",
            release_track_mbid=f"release-track-{track}",
        )
        held_files.append(held_file)
    processor = MagicMock()
    processor.place_held_management_bundle = AsyncMock(
        return_value=[Path("/music/Album/01.flac"), Path("/music/Album/02.flac")]
    )
    reconciler = MagicMock()
    reconciler.reconcile_with_filesystem = AsyncMock()
    service = _held_service(store, processor, reconciler)
    store.get_task = AsyncMock(
        return_value=DownloadTask(
            id="managed-task",
            user_id="user-a",
            release_group_mbid="rg-1",
            release_mbid="release-1",
        )
    )

    result = await service.retry_management_hold("managed-task", "user-a", "user")

    assert result == ["/music/Album/01.flac", "/music/Album/02.flac"]
    unit = processor.place_held_management_bundle.await_args.args[0]
    assert sorted(value.track_number for value in unit) == [1, 2]
    assert all(value.source_task_id == "managed-task" for value in unit)
    assert await store.list_held_imports("user-a", "user") == []
    reconciler.reconcile_with_filesystem.assert_awaited_once_with(
        targets=[Path("/music/Album")]
    )
    service._orchestrator.settle_after_manual_import.assert_awaited_once_with(
        "managed-task"
    )


@pytest.mark.asyncio
async def test_management_hold_retry_preserves_unit_and_refreshes_reason_on_failure(
    tmp_path,
):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held.flac"
    held_file.write_bytes(b"audio")
    await _record_held(
        store,
        held_file,
        task_id="managed-task",
        reason="management:PROFILE_CHANGED",
        release_mbid="release-1",
        release_track_mbid="release-track-3",
    )
    processor = MagicMock()
    processor.place_held_management_bundle = AsyncMock(
        side_effect=AutomaticManagementHoldError(
            "SIDECAR_COLLISION", "cover.jpg conflicts with a different file"
        )
    )
    service = _held_service(store, processor)
    store.get_task = AsyncMock(
        return_value=DownloadTask(
            id="managed-task",
            user_id="user-a",
            release_group_mbid="rg-1",
            release_mbid="release-1",
        )
    )

    with pytest.raises(ValidationError, match="cover.jpg conflicts"):
        await service.retry_management_hold("managed-task", "user-a", "user")

    held = await store.list_held_imports("user-a", "user")
    assert len(held) == 1
    assert held[0].reason == "management:SIDECAR_COLLISION"
    assert held[0].reason_detail == "cover.jpg conflicts with a different file"
    service._orchestrator.settle_after_manual_import.assert_not_awaited()


@pytest.mark.asyncio
async def test_transient_management_hold_schedules_and_runs_without_redownload(
    tmp_path, monkeypatch
):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held.flac"
    held_file.write_bytes(b"audio")
    await _record_held(
        store,
        held_file,
        task_id="managed-task",
        reason="management:PROFILE_CHANGED",
        release_mbid="release-1",
        release_track_mbid="release-track-3",
    )
    processor = MagicMock()
    processor.place_held_management_bundle = AsyncMock(
        side_effect=AutomaticManagementHoldError(
            "ROOT_UNAVAILABLE", "The destination interrupted the staged write."
        )
    )
    service = _held_service(store, processor)
    store.get_task = AsyncMock(
        return_value=DownloadTask(
            id="managed-task",
            user_id="user-a",
            release_group_mbid="rg-1",
            release_mbid="release-1",
        )
    )
    monkeypatch.setattr("services.native.download_service.time.time", lambda: 1000.0)

    with pytest.raises(ValidationError, match="destination interrupted"):
        await service.retry_management_hold("managed-task", "user-a", "user")

    held = await store.list_held_imports(
        "user-a", "user", source_task_id="managed-task"
    )
    assert {value.management_retry_count for value in held} == {1}
    assert {value.management_next_retry_at for value in held} == {1300.0}

    processor.place_held_management_bundle.side_effect = None
    processor.place_held_management_bundle.return_value = [Path("/music/Album/03.flac")]
    monkeypatch.setattr("services.native.download_service.time.time", lambda: 1300.0)
    await service.retry_due_management_holds()

    assert await store.list_held_imports("user-a", "user") == []
    assert processor.place_held_management_bundle.await_count == 2


def _legacy_hold_album_service(*, fail: bool = False):
    service = MagicMock()
    service.resolve_edition = AsyncMock(return_value="release-1")
    if fail:
        service.get_exact_edition_tracks_info = AsyncMock(
            side_effect=RuntimeError("provider unavailable")
        )
    else:
        service.get_exact_edition_tracks_info = AsyncMock(
            return_value=SimpleNamespace(
                tracks=[
                    SimpleNamespace(
                        disc_number=1,
                        position=track,
                        recording_id=f"rec-{track}",
                        release_track_id=f"release-track-{track}",
                    )
                    for track in (1, 2)
                ]
            )
        )
    return service


async def _legacy_management_unit(store, tmp_path):
    import sqlite3

    with sqlite3.connect(store.db_path) as conn:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users "
            "(id TEXT PRIMARY KEY, username TEXT, role TEXT)"
        )
        conn.execute(
            "INSERT INTO auth_users (id, username, role) VALUES ('user-a', 'alice', 'user')"
        )
        conn.commit()
    task = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-1",
        artist_name="Led Zeppelin",
        album_title="Led Zeppelin",
        track_count=2,
        status="failed",
    )
    for track in (1, 2):
        path = tmp_path / f"legacy-{track}.flac"
        path.write_bytes(b"audio")
        await _record_held(
            store,
            path,
            task_id=task.id,
            reason="management:TRACK_NOT_MAPPED",
            track_number=track,
            recording_mbid=f"rec-{track}",
        )
    return task


@pytest.mark.asyncio
async def test_legacy_management_hold_repairs_complete_provider_map_before_retry(
    tmp_path,
):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    task = await _legacy_management_unit(store, tmp_path)
    processor = MagicMock()
    processor.place_held_management_bundle = AsyncMock(
        return_value=[Path("/music/Album/01.flac"), Path("/music/Album/02.flac")]
    )
    service = _held_service(
        store,
        processor,
        album_service=_legacy_hold_album_service(),
    )

    await service.retry_management_hold(task.id, "user-a", "user")

    repaired_task = await store.get_task(task.id)
    assert repaired_task.release_mbid == "release-1"
    repaired = processor.place_held_management_bundle.await_args.args[0]
    assert {value.release_track_mbid for value in repaired} == {
        "release-track-1",
        "release-track-2",
    }
    assert all(value.release_mbid == "release-1" for value in repaired)


@pytest.mark.asyncio
async def test_legacy_management_hold_rejects_partial_map_without_publication(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    task = await _legacy_management_unit(store, tmp_path)
    held = await store.list_held_imports("user-a", "user", source_task_id=task.id)
    await store.resolve_held_import(held[1].id, "discarded")
    processor = MagicMock()
    processor.place_held_management_bundle = AsyncMock()
    service = _held_service(
        store,
        processor,
        album_service=_legacy_hold_album_service(),
    )

    with pytest.raises(ValidationError, match="complete positional match"):
        await service.retry_management_hold(task.id, "user-a", "user")

    remaining = await store.list_held_imports("user-a", "user", source_task_id=task.id)
    assert len(remaining) == 1
    assert remaining[0].release_mbid is None
    assert remaining[0].release_track_mbid is None
    processor.place_held_management_bundle.assert_not_awaited()


@pytest.mark.asyncio
async def test_legacy_management_hold_provider_failure_keeps_every_file_held(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    task = await _legacy_management_unit(store, tmp_path)
    processor = MagicMock()
    processor.place_held_management_bundle = AsyncMock()
    service = _held_service(
        store,
        processor,
        album_service=_legacy_hold_album_service(fail=True),
    )

    with pytest.raises(ValidationError, match="could not prove"):
        await service.retry_management_hold(task.id, "user-a", "user")

    held = await store.list_held_imports("user-a", "user", source_task_id=task.id)
    assert len(held) == 2
    assert {value.reason for value in held} == {"management:METADATA_UNAVAILABLE"}
    assert all(value.release_mbid is None for value in held)
    assert all(value.release_track_mbid is None for value in held)
    processor.place_held_management_bundle.assert_not_awaited()


@pytest.mark.asyncio
async def test_track_not_mapped_hold_with_full_expected_ids_is_not_repaired(tmp_path):
    """Provider IDs alone cannot upgrade a file mapping that acquisition rejected."""

    import sqlite3
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "CREATE TABLE auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)"
        )
        connection.execute(
            "INSERT INTO auth_users (id, username, role) VALUES ('user-a', 'alice', 'user')"
        )
    task = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-1",
        release_mbid="release-1",
        artist_name="Led Zeppelin",
        album_title="Led Zeppelin",
        track_count=1,
        status="failed",
    )
    held_file = tmp_path / "held.flac"
    held_file.write_bytes(b"audio")
    await _record_held(
        store,
        held_file,
        task_id=task.id,
        reason="management:TRACK_NOT_MAPPED",
        track_number=1,
        release_mbid="release-1",
        release_track_mbid="release-track-1",
        recording_mbid="rec-1",
    )
    processor = MagicMock()
    processor.place_held_management_bundle = AsyncMock()
    album_service = _legacy_hold_album_service()
    service = _held_service(
        store,
        processor,
        album_service=album_service,
    )

    with pytest.raises(
        ValidationError, match="did not provide enough recording evidence"
    ):
        await service.retry_management_hold(task.id, "user-a", "user")

    held = await store.list_held_imports("user-a", "user", source_task_id=task.id)
    assert len(held) == 1
    assert held[0].reason == "management:TRACK_NOT_MAPPED"
    assert held_file.is_file()
    album_service.resolve_edition.assert_not_awaited()
    processor.place_held_management_bundle.assert_not_awaited()


@pytest.mark.asyncio
async def test_management_hold_actions_serialize_one_task(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held.flac"
    held_file.write_bytes(b"audio")
    await _record_held(
        store,
        held_file,
        task_id="managed-task",
        reason="management:PROFILE_CHANGED",
        release_mbid="release-1",
        release_track_mbid="release-track-3",
    )
    entered = asyncio.Event()
    release = asyncio.Event()

    async def publish(_held):  # noqa: ANN001
        entered.set()
        await release.wait()
        return [Path("/music/Album/01.flac")]

    processor = MagicMock()
    processor.place_held_management_bundle = AsyncMock(side_effect=publish)
    service = _held_service(store, processor)
    store.get_task = AsyncMock(
        return_value=DownloadTask(
            id="managed-task",
            user_id="user-a",
            release_group_mbid="rg-1",
            release_mbid="release-1",
        )
    )

    first = asyncio.create_task(
        service.retry_management_hold("managed-task", "user-a", "user")
    )
    await entered.wait()
    second = asyncio.create_task(
        service.retry_management_hold("managed-task", "user-a", "user")
    )
    await asyncio.sleep(0)

    processor.place_held_management_bundle.assert_awaited_once()
    release.set()
    assert await first == ["/music/Album/01.flac"]
    with pytest.raises(ResourceNotFoundError):
        await second
    assert service._management_hold_locks == {}
    assert service._management_hold_lock_users == {}


@pytest.mark.asyncio
async def test_management_hold_cannot_use_per_track_escape_hatches(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held.flac"
    held_file.write_bytes(b"audio")
    held_id = await _record_held(
        store,
        held_file,
        task_id="managed-task",
        reason="management:PROFILE_CHANGED",
    )
    service = _held_service(store, MagicMock())

    with pytest.raises(ValidationError, match="complete acquisition unit"):
        await service.import_held(held_id, "user-a", "user")
    with pytest.raises(ValidationError, match="complete acquisition unit"):
        await service.discard_held(held_id, "user-a", "user")
    assert held_file.exists()
    assert len(await store.list_held_imports("user-a", "user")) == 1


@pytest.mark.asyncio
async def test_discard_management_hold_deletes_complete_unit_and_cancels_task(tmp_path):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_files = []
    for track in (1, 2):
        held_file = tmp_path / f"held-{track}.flac"
        held_file.write_bytes(b"audio")
        await _record_held(
            store,
            held_file,
            task_id="managed-task",
            reason="management:PROFILE_CHANGED",
            track_number=track,
        )
        held_files.append(held_file)
    service = _held_service(store, MagicMock())

    count = await service.discard_management_hold("managed-task", "user-a", "user")

    assert count == 2
    assert all(not value.exists() for value in held_files)
    assert await store.list_held_imports("user-a", "user") == []
    service._orchestrator.cancel_task.assert_awaited_once_with(
        "managed-task", "user-a", "user"
    )


@pytest.mark.asyncio
async def test_discard_management_hold_retries_file_cleanup_after_unlink_failure(
    tmp_path, monkeypatch
):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_files = []
    for track in (1, 2):
        held_file = tmp_path / f"held-{track}.flac"
        held_file.write_bytes(b"audio")
        await _record_held(
            store,
            held_file,
            task_id="managed-task",
            reason="management:PROFILE_CHANGED",
            track_number=track,
        )
        held_files.append(held_file)
    service = _held_service(store, MagicMock())
    unlink = Path.unlink

    def fail_second_file(source, *, missing_ok=False):  # noqa: ANN001
        if source == held_files[1] and source.exists():
            raise PermissionError("read-only directory")
        return unlink(source, missing_ok=missing_ok)

    monkeypatch.setattr(Path, "unlink", fail_second_file)

    count = await service.discard_management_hold("managed-task", "user-a", "user")

    assert count == 2
    assert held_files[0].exists() is False
    assert held_files[1].exists() is True
    assert await store.list_held_imports("user-a", "user") == []
    pending = await store.list_pending_discard_file_cleanups()
    assert [value.held_path for value in pending] == [str(held_files[1])]
    service._orchestrator.cancel_task.assert_awaited_once()

    monkeypatch.setattr(Path, "unlink", unlink)
    await service.cleanup_discarded_held_files()

    assert held_files[1].exists() is False
    assert await store.list_pending_discard_file_cleanups() == []


@pytest.mark.asyncio
async def test_discard_management_hold_restores_files_when_database_update_fails(
    tmp_path,
):
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held.flac"
    held_file.write_bytes(b"audio")
    await _record_held(
        store,
        held_file,
        task_id="managed-task",
        reason="management:PROFILE_CHANGED",
    )
    service = _held_service(store, MagicMock())
    original_resolve = store.resolve_held_imports
    store.resolve_held_imports = AsyncMock(side_effect=RuntimeError("database busy"))

    with pytest.raises(RuntimeError, match="database busy"):
        await service.discard_management_hold("managed-task", "user-a", "user")

    assert held_file.exists()
    store.resolve_held_imports = original_resolve
    assert len(await store.list_held_imports("user-a", "user")) == 1
    service._orchestrator.cancel_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_purge_album_downloads_clears_tasks_held_and_quarantine(tmp_path):
    # Removing an album must clear its whole download-side footprint: cancel retries (no
    # resurrection), delete held tracks + their files, and drop blocklist entries.
    import sqlite3
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    db_path = tmp_path / "library.db"
    conn = sqlite3.connect(db_path)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)"
    )
    conn.execute("INSERT OR IGNORE INTO auth_users VALUES ('user-a','a','user')")
    conn.commit()
    conn.close()
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    RG = "rg-1"
    task = await store.create_task(
        user_id="user-a",
        release_group_mbid=RG,
        artist_name="a",
        album_title="b",
        source="usenet",
    )
    await store.update_status(task.id, "partial", files_completed=7)  # would auto-retry
    await store.record_quarantine(
        source="usenet",
        identity="bad-release",
        reason="verify_failed",
        release_group_mbid=RG,
    )
    held_file = tmp_path / "held" / "x.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    await store.record_held_import(
        user_id="user-a",
        held_path=str(held_file),
        reason="fingerprint_mismatch",
        source="usenet",
        source_task_id=task.id,
        release_group_mbid=RG,
        release_mbid=None,
        recording_mbid=None,
        track_number=3,
        disc_number=1,
        track_title="t",
        artist_name="a",
        artist_mbid=None,
        album_title="b",
        year=None,
        original_filename="x.flac",
        file_format="flac",
        duration_seconds=1.0,
        evidence_title=None,
        evidence_artist=None,
        evidence_score=None,
        naming_template=None,
    )
    svc = _held_service(store, MagicMock())

    await svc.purge_album_downloads(RG)

    assert (
        await store.get_task(task.id)
    ).status == "cancelled"  # no auto-retry resurrection
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
    quota.check_storage_admission.side_effect = VErr(
        "Library storage limit reached (10.0 / 10 GB)"
    )
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
    quota.check_storage_admission.side_effect = VErr(
        "Your storage budget is full (5.0 / 5 GB)"
    )
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


@pytest.mark.asyncio
async def test_import_held_resolves_only_after_source_consumption(tmp_path):
    """F-INDEXREC-04: a result-aware seam proves import_held resolves the row as
    imported only after the validated no-op consumed the held source off disk."""
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "x.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    hid = await _record_held(store, held_file)
    fp = MagicMock()
    consumption_order: list[str] = []

    async def place(held):
        # Simulate the validated no-op branch: consume the source, then return.
        await asyncio.to_thread(Path(held.held_path).unlink, True)
        consumption_order.append("unlinked")
        return Path("/music/Led Zeppelin/03 You Shook Me.flac")

    fp.place_held_file = AsyncMock(side_effect=place)
    svc = _held_service(store, fp)

    final_path = await svc.import_held(hid, "user-a", "user")

    assert final_path.endswith("03 You Shook Me.flac")
    assert consumption_order == ["unlinked"]
    assert not held_file.exists()
    assert await store.list_held_imports("user-a", "user") == []
    assert await store.has_unresolved_held_for_task("t-1") is False


@pytest.mark.asyncio
async def test_import_held_unlink_failure_leaves_row_retryable(tmp_path):
    """F-INDEXREC-04 negative boundary: an unlink I/O error inside the no-op
    branch propagates and the held row is NOT resolved as imported."""
    import threading

    from infrastructure.persistence.download_store import DownloadStore

    store = DownloadStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    held_file = tmp_path / "held" / "x.flac"
    held_file.parent.mkdir()
    held_file.write_bytes(b"audio")
    hid = await _record_held(store, held_file)
    fp = MagicMock()

    async def place(_held):
        raise OSError("disk I/O error during unlink")

    fp.place_held_file = AsyncMock(side_effect=place)
    svc = _held_service(store, fp)

    with pytest.raises(OSError, match="disk I/O error"):
        await svc.import_held(hid, "user-a", "user")

    held = await store.list_held_imports("user-a", "user")
    assert [value.id for value in held] == [hid]
    assert await store.has_unresolved_held_for_task("t-1") is True
    assert held_file.exists()
