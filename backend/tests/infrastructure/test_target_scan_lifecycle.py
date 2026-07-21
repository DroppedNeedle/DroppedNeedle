from __future__ import annotations

import asyncio
import os
import sqlite3
import threading
from datetime import datetime
from pathlib import Path
from typing import get_args

import pytest

from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from core.exceptions import StaleRevisionError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.audio import AudioInfo, AudioTag
from models.library_work import ScanRequest, ScanRun, ScanScope, ScanState
from services.native.library_indexer import INDEX_BATCH_SIZE, LibraryIndexer
from services.native.library_inventory_scanner import (
    DirectoryWalker,
    INVENTORY_BATCH_SIZE,
    INVENTORY_QUEUE_SIZE,
    LibraryInventoryScanner,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.library_reconciler import LibraryReconciler
from services.native.library_scan_coordinator import LibraryScanCoordinator
from services.native.library_schedule_service import LibraryScheduleService


class _TagReader:
    def __init__(self) -> None:
        self.calls: list[Path] = []

    def read_tags(self, path: Path) -> tuple[AudioTag, AudioInfo]:
        self.calls.append(path)
        number = int(path.stem.rsplit("-", 1)[-1])
        return (
            AudioTag(
                title=f"Track {number}",
                artist="Local Artist",
                album="Local Album",
                album_artist="Local Artist",
                track_number=number,
            ),
            AudioInfo(
                duration_seconds=180,
                bitrate=900,
                sample_rate=44_100,
                channels=2,
                file_format="flac",
                file_size_bytes=path.stat().st_size,
                bit_depth=16,
            ),
        )


@pytest.fixture
def target_store(tmp_path: Path) -> NativeLibraryStore:
    db_path = tmp_path / "target.db"
    connection = sqlite3.connect(db_path)
    connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
    connection.commit()
    connection.close()
    return NativeLibraryStore(db_path=db_path, write_lock=threading.Lock())


def _resolver(root: Path) -> LibraryPolicyResolver:
    return LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-a", path=str(root), label="Library", policy="automatic"
                )
            ]
        )
    )


def _request(
    resolver: LibraryPolicyResolver,
    *,
    kind: str = "incremental",
    relative_path: str = ".",
    trigger: str = "manual",
) -> ScanRequest:
    return ScanRequest(
        kind=kind,
        trigger=trigger,
        policy_revision=resolver.policy_revision,
        scopes=[
            ScanScope(
                root_id="root-a",
                relative_path=relative_path,
                policy_revision=resolver.policy_revision,
            )
        ],
    )


def _coordinator(
    store: NativeLibraryStore,
    resolver: LibraryPolicyResolver,
    tag_reader: _TagReader | None = None,
    directory_walker: DirectoryWalker | None = None,
) -> LibraryScanCoordinator:
    reader = tag_reader or _TagReader()
    scanner = (
        LibraryInventoryScanner(store)
        if directory_walker is None
        else LibraryInventoryScanner(store, directory_walker=directory_walker)
    )
    return LibraryScanCoordinator(
        store,
        scanner,
        LibraryIndexer(store, reader),
        LibraryReconciler(store),
        lambda: resolver,
        clock=lambda: 1_800_000_000.0,
    )


def test_scan_state_contract_is_shared_and_complete() -> None:
    assert set(get_args(ScanState)) == {
        "queued",
        "discovering",
        "indexing",
        "reconciling",
        "pausing",
        "paused",
        "stopping",
        "completed",
        "cancelled",
        "superseded_policy_changed",
        "failed",
    }


@pytest.mark.asyncio
async def test_atomic_single_flight_coalescing_union_and_kind_conflict(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)

    first, duplicate = await asyncio.gather(
        coordinator.request_run(_request(resolver)),
        coordinator.request_run(_request(resolver)),
    )
    assert {first.disposition, duplicate.disposition} == {"started", "coalesced"}
    active = await target_store.claim_next_scan_run(now=1_800_000_001)
    assert active is not None

    queued = await coordinator.request_run(
        _request(resolver, relative_path="Disc 1", kind="rescan_files")
    )
    assert queued.disposition == "queued"
    expanded = await coordinator.request_run(
        _request(resolver, relative_path="Disc 2", kind="rescan_files")
    )
    assert expanded.disposition == "expanded"
    conflict = await coordinator.request_run(
        _request(resolver, kind="policy_reconcile")
    )
    assert conflict.disposition == "conflict"
    assert conflict.conflicting_kind == "rescan_files"
    _, scopes, _ = await target_store.get_scan_run(queued.run_id)
    assert {scope.relative_path for scope in scopes} == {"Disc 1", "Disc 2"}


@pytest.mark.asyncio
async def test_every_target_trigger_uses_the_single_request_transaction(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    for trigger in (
        "manual",
        "automatic",
        "subsonic",
        "startup_resume",
        "policy_apply",
    ):
        result = await coordinator.request_run(_request(resolver, trigger=trigger))
        assert result.disposition in {"started", "coalesced"}
    assert await target_store.row_count("library_scan_runs") == 1
    assert await target_store.row_count("library_scan_run_triggers") == 5


@pytest.mark.asyncio
async def test_completed_scan_history_keeps_counters_and_phase_timings(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    requested = await coordinator.request_run(_request(resolver))
    run = await target_store.claim_next_scan_run(now=1_800_000_001)
    assert run is not None and run.id == requested.run_id
    run = await target_store.add_scan_counters(
        run.id,
        {
            "inspected_count": 7,
            "new_count": 1,
            "changed_count": 1,
            "unchanged_count": 5,
        },
        updated_at=1_800_000_002,
    )
    run = await target_store.transition_scan_run(
        run.id,
        expected_state="discovering",
        expected_revision=run.row_revision,
        new_state="indexing",
        now=1_800_000_004,
    )
    run = await target_store.transition_scan_run(
        run.id,
        expected_state="indexing",
        expected_revision=run.row_revision,
        new_state="reconciling",
        now=1_800_000_009,
    )
    await target_store.transition_scan_run(
        run.id,
        expected_state="reconciling",
        expected_revision=run.row_revision,
        new_state="completed",
        now=1_800_000_011,
    )

    history = await target_store.list_scan_history()

    assert history[0].counters["inspected_count"] == 7
    assert history[0].counters["new_count"] == 1
    assert history[0].counters["changed_count"] == 1
    assert history[0].phase_timings == {
        "discovering": 3.0,
        "indexing": 5.0,
        "reconciling": 2.0,
    }


@pytest.mark.asyncio
async def test_indexer_reports_durable_progress_after_each_bounded_batch(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    file_count = INDEX_BATCH_SIZE * 2 + 2
    for ordinal in range(file_count):
        (root / f"track-{ordinal}.flac").write_bytes(b"audio")
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    requested = await coordinator.request_run(_request(resolver))
    run = await target_store.claim_next_scan_run(now=10)
    assert run is not None and run.id == requested.run_id
    _, scopes, _ = await target_store.get_scan_run(run.id)

    async def continue_work(_run_id: str, _revision: str) -> bool:
        return True

    run = await LibraryInventoryScanner(target_store).discover(
        run, scopes, {"root-a": root}, resolver, continue_work
    )
    run = await target_store.transition_scan_run(
        run.id,
        expected_state="discovering",
        expected_revision=run.row_revision,
        new_state="indexing",
        now=11,
    )
    updates: list[int] = []

    previous = 0

    async def record_progress(updated_run: ScanRun) -> None:
        nonlocal previous
        inspected = updated_run.counters["inspected_count"]
        updates.append(inspected - previous)
        previous = inspected

    await LibraryIndexer(target_store, _TagReader()).index(
        run,
        resolver.policy_revision,
        continue_work,
        progress=record_progress,
    )

    _, _, counters = await target_store.get_scan_run(run.id)
    assert updates == [INDEX_BATCH_SIZE, INDEX_BATCH_SIZE, 2]
    assert counters["inspected_count"] == file_count
    assert counters["indexed_count"] == file_count
    assert counters["new_count"] == file_count


@pytest.mark.asyncio
async def test_file_changed_during_both_tag_reads_is_recorded_without_catalog_write(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    track = root / "track-1.flac"
    track.write_bytes(b"audio")
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    requested = await coordinator.request_run(_request(resolver))
    run = await target_store.claim_next_scan_run(now=10)
    assert run is not None and run.id == requested.run_id
    _, scopes, _ = await target_store.get_scan_run(run.id)

    async def continue_work(_run_id: str, _revision: str) -> bool:
        return True

    run = await LibraryInventoryScanner(target_store).discover(
        run, scopes, {"root-a": root}, resolver, continue_work
    )
    run = await target_store.transition_scan_run(
        run.id,
        expected_state="discovering",
        expected_revision=run.row_revision,
        new_state="indexing",
        now=11,
    )

    class MutatingReader:
        calls = 0

        def read_tags(self, path: Path) -> tuple[AudioTag, AudioInfo]:
            self.calls += 1
            path.write_bytes(path.read_bytes() + b"x")
            return (
                AudioTag(
                    title="Track 1",
                    artist="Artist",
                    album="Album",
                    album_artist="Artist",
                    track_number=1,
                ),
                AudioInfo(
                    duration_seconds=1,
                    bitrate=1,
                    sample_rate=44_100,
                    channels=2,
                    file_format="flac",
                    file_size_bytes=path.stat().st_size,
                    bit_depth=16,
                ),
            )

    reader = MutatingReader()
    counts = await LibraryIndexer(target_store, reader).index(
        run, resolver.policy_revision, continue_work
    )

    inventory = await target_store.get_scan_inventory_batch(
        run.id, processing_state="failed", limit=10
    )
    _, _, counters = await target_store.get_scan_run(run.id)
    assert reader.calls == 2
    assert counts["tag_reads"] == 2
    assert inventory[0]["failure_code"] == "FILE_CHANGED_DURING_READ"
    assert counters["inspected_count"] == 1
    assert counters["errored_count"] == 1
    assert await target_store.row_count("local_tracks") == 0


@pytest.mark.asyncio
async def test_transition_matrix_controls_and_restart_recovery(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    requested = await coordinator.request_run(_request(resolver))
    run = await target_store.claim_next_scan_run(now=10)
    assert run is not None and run.state == "discovering"

    paused_request = await coordinator.control(run.id, "pause", run.row_revision)
    assert paused_request.state == "pausing"
    paused = await target_store.transition_scan_run(
        run.id,
        expected_state="pausing",
        expected_revision=paused_request.row_revision,
        new_state="paused",
        now=11,
    )
    recovered = await coordinator.recover()
    assert [item.id for item in recovered] == [requested.run_id]
    resumed = await coordinator.control(paused.id, "resume", paused.row_revision)
    assert resumed.state == "discovering"
    stopping = await coordinator.control(resumed.run_id, "stop", resumed.row_revision)
    assert stopping.state == "stopping"
    recovered = await coordinator.recover()
    assert recovered == []
    terminal, _, _ = await target_store.get_scan_run(run.id)
    assert terminal.state == "cancelled"

    with pytest.raises(StaleRevisionError):
        await target_store.transition_scan_run(
            run.id,
            expected_state="cancelled",
            expected_revision=terminal.row_revision,
            new_state="completed",
            now=12,
        )


@pytest.mark.asyncio
@pytest.mark.parametrize("phase", ["discovering", "indexing", "reconciling"])
async def test_process_restart_resumes_same_run_through_completion(
    target_store: NativeLibraryStore, tmp_path: Path, phase: str
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    (root / "track-1.flac").write_bytes(b"audio")
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    requested = await coordinator.request_run(_request(resolver))
    run = await target_store.claim_next_scan_run(now=10)
    assert run is not None
    _, scopes, _ = await target_store.get_scan_run(run.id)

    async def continue_work(_run_id: str, _revision: str) -> bool:
        return True

    if phase in {"indexing", "reconciling"}:
        run = await LibraryInventoryScanner(target_store).discover(
            run,
            scopes,
            {"root-a": root},
            resolver,
            continue_work,
        )
        run = await target_store.transition_scan_run(
            run.id,
            expected_state="discovering",
            expected_revision=run.row_revision,
            new_state="indexing",
            now=11,
        )
    if phase == "reconciling":
        await LibraryIndexer(target_store, _TagReader()).index(
            run, resolver.policy_revision, continue_work
        )
        run, _, _ = await target_store.get_scan_run(run.id)
        run = await target_store.transition_scan_run(
            run.id,
            expected_state="indexing",
            expected_revision=run.row_revision,
            new_state="reconciling",
            now=12,
        )

    recovered = await coordinator.recover()
    assert [item.id for item in recovered] == [requested.run_id]
    completed = await coordinator.run_once({"root-a": root})

    assert completed is not None
    assert completed.id == requested.run_id
    assert completed.state == "completed"
    with sqlite3.connect(tmp_path / "target.db") as connection:
        triggers = connection.execute(
            "SELECT trigger FROM library_scan_run_triggers WHERE run_id = ? ORDER BY trigger_sequence",
            (requested.run_id,),
        ).fetchall()
    assert triggers[-1][0] == "startup_resume"


@pytest.mark.asyncio
async def test_scan_worker_exception_becomes_typed_terminal_failure(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)

    class BrokenInventory(LibraryInventoryScanner):
        async def discover(self, *args, **kwargs):
            raise RuntimeError("injected worker failure")

    coordinator = LibraryScanCoordinator(
        target_store,
        BrokenInventory(target_store),
        LibraryIndexer(target_store, _TagReader()),
        LibraryReconciler(target_store),
        lambda: resolver,
        clock=lambda: 20,
    )
    requested = await coordinator.request_run(_request(resolver))

    with pytest.raises(RuntimeError, match="injected worker failure"):
        await coordinator.run_once({"root-a": root})

    failed, _, _ = await target_store.get_scan_run(requested.run_id)
    assert failed.state == "failed"
    assert failed.terminal_code == "UNEXPECTED_WORKER_FAILURE"


@pytest.mark.asyncio
@pytest.mark.parametrize("phase", ["discovering", "indexing", "reconciling"])
async def test_pause_resume_and_stop_are_idempotent_at_every_phase(
    target_store: NativeLibraryStore, tmp_path: Path, phase: str
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    await coordinator.request_run(_request(resolver))
    run = await target_store.claim_next_scan_run(now=10)
    assert run is not None
    if phase in {"indexing", "reconciling"}:
        run = await target_store.transition_scan_run(
            run.id,
            expected_state="discovering",
            expected_revision=run.row_revision,
            new_state="indexing",
            now=11,
        )
    if phase == "reconciling":
        run = await target_store.transition_scan_run(
            run.id,
            expected_state="indexing",
            expected_revision=run.row_revision,
            new_state="reconciling",
            now=12,
        )
    original_revision = run.row_revision
    requested = await coordinator.control(run.id, "pause", original_revision)
    repeated = await coordinator.control(run.id, "pause", original_revision)
    assert repeated.row_revision == requested.row_revision
    paused = await target_store.transition_scan_run(
        run.id,
        expected_state="pausing",
        expected_revision=requested.row_revision,
        new_state="paused",
        now=13,
    )
    resumed = await coordinator.control(run.id, "resume", paused.row_revision)
    assert resumed.state == phase
    repeated_resume = await coordinator.control(run.id, "resume", paused.row_revision)
    assert repeated_resume.row_revision == resumed.row_revision
    stopping = await coordinator.control(run.id, "stop", resumed.row_revision)
    repeated_stop = await coordinator.control(run.id, "stop", resumed.row_revision)
    assert stopping.state == "stopping"
    assert repeated_stop.row_revision == stopping.row_revision


@pytest.mark.asyncio
async def test_only_latest_fifty_terminal_runs_are_retained(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    for ordinal in range(51):
        await coordinator.request_run(_request(resolver))
        run = await target_store.claim_next_scan_run(now=ordinal * 10 + 1)
        assert run is not None
        run = await target_store.transition_scan_run(
            run.id,
            expected_state="discovering",
            expected_revision=run.row_revision,
            new_state="indexing",
            now=ordinal * 10 + 2,
        )
        run = await target_store.transition_scan_run(
            run.id,
            expected_state="indexing",
            expected_revision=run.row_revision,
            new_state="reconciling",
            now=ordinal * 10 + 3,
        )
        await target_store.transition_scan_run(
            run.id,
            expected_state="reconciling",
            expected_revision=run.row_revision,
            new_state="completed",
            now=ordinal * 10 + 4,
        )
    await target_store.cleanup_terminal_scan_inventory()
    assert await target_store.row_count("library_scan_runs") == 50
    assert len(await coordinator.history(limit=50)) == 50
    first_page, cursor = await coordinator.history_page(limit=20)
    assert len(first_page) == 20
    assert cursor is not None
    second_page, _ = await coordinator.history_page(limit=20, cursor=cursor)
    assert len(second_page) == 20
    assert {run.id for run in first_page}.isdisjoint(run.id for run in second_page)


@pytest.mark.asyncio
async def test_one_walk_incremental_tag_revisions_and_no_repeat(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    album = root / "Artist" / "Album"
    album.mkdir(parents=True)
    first = album / "track-1.flac"
    second = album / "track-2.flac"
    first.write_bytes(b"one")
    second.write_bytes(b"two")
    resolver = _resolver(root)
    reader = _TagReader()
    real_walk = os.walk
    walk_count = 0

    def counted_walk(*args, **kwargs):
        nonlocal walk_count
        walk_count += 1
        return real_walk(*args, **kwargs)

    coordinator = _coordinator(
        target_store, resolver, reader, directory_walker=counted_walk
    )
    await coordinator.request_run(_request(resolver))
    completed = await coordinator.run_once({"root-a": root})
    assert completed is not None and completed.state == "completed"
    assert completed.counters["new_count"] == 2
    assert completed.counters["changed_count"] == 0
    assert walk_count == 1
    assert len(reader.calls) == 2
    assert INVENTORY_QUEUE_SIZE == 256
    assert INVENTORY_BATCH_SIZE == 256

    await coordinator.request_run(_request(resolver, trigger="automatic"))
    repeated = await coordinator.run_once({"root-a": root})
    assert repeated is not None and repeated.state == "completed"
    assert repeated.counters["new_count"] == 0
    assert repeated.counters["changed_count"] == 0
    assert walk_count == 2
    assert len(reader.calls) == 2

    await coordinator.request_run(_request(resolver, kind="rescan_files"))
    rescanned = await coordinator.run_once({"root-a": root})
    assert rescanned is not None and rescanned.state == "completed"
    assert rescanned.counters["new_count"] == 0
    assert rescanned.counters["changed_count"] == 0
    assert walk_count == 3
    assert len(reader.calls) == 4

    first.write_bytes(b"one changed")
    await coordinator.request_run(_request(resolver))
    changed = await coordinator.run_once({"root-a": root})
    assert changed is not None and changed.state == "completed"
    assert changed.counters["new_count"] == 0
    assert changed.counters["changed_count"] == 1
    assert walk_count == 4
    assert len(reader.calls) == 5
    tracks = await target_store.search_local_tracks("Track")
    assert len(tracks) == 2


@pytest.mark.asyncio
async def test_scan_transaction_ratios_and_tag_read_gates(
    target_store: NativeLibraryStore,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root = tmp_path / "music"
    album = root / "Artist" / "Album"
    album.mkdir(parents=True)
    file_count = 4_096
    for index in range(file_count):
        (album / f"track-{index}.flac").write_bytes(b"audio")
    resolver = _resolver(root)
    reader = _TagReader()

    class NoGrouping:
        async def regroup_run(
            self,
            _run_id: str,
            *,
            now: float,
            checkpoint=None,
            frozen_policy_revision: str = "",
        ) -> int:
            del now, checkpoint, frozen_policy_revision
            return 0

    coordinator = LibraryScanCoordinator(
        target_store,
        LibraryInventoryScanner(target_store),
        LibraryIndexer(target_store, reader, grouping=NoGrouping()),
        LibraryReconciler(target_store),
        lambda: resolver,
        clock=lambda: 1_800_000_000.0,
    )
    original_execute_background = target_store._execute_background
    background_transactions = 0

    def count_background_transaction(operation):
        nonlocal background_transactions
        background_transactions += 1
        return original_execute_background(operation)

    monkeypatch.setattr(
        target_store, "_execute_background", count_background_transaction
    )
    await coordinator.request_run(_request(resolver))
    first = await coordinator.run_once({"root-a": root})

    assert first is not None and first.state == "completed"
    assert len(reader.calls) == file_count
    assert background_transactions / file_count < 0.03
    catalog_revision = await target_store.get_catalog_revision()

    background_transactions = 0
    await coordinator.request_run(_request(resolver))
    unchanged = await coordinator.run_once({"root-a": root})

    assert unchanged is not None and unchanged.state == "completed"
    assert len(reader.calls) == file_count
    assert background_transactions / file_count < 0.02
    assert await target_store.get_catalog_revision() == catalog_revision

    changed_path = album / "track-0.flac"
    changed_path.write_bytes(b"changed")
    await coordinator.request_run(_request(resolver))
    changed = await coordinator.run_once({"root-a": root})

    assert changed is not None and changed.state == "completed"
    assert len(reader.calls) == file_count + 1
    assert await target_store.get_catalog_revision() == catalog_revision


@pytest.mark.asyncio
async def test_reconcile_uses_inventory_and_unavailable_root_cannot_mark_missing(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    track = root / "track-1.flac"
    track.write_bytes(b"one")
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    await coordinator.request_run(_request(resolver))
    await coordinator.run_once({"root-a": root})

    track.unlink()
    await coordinator.request_run(_request(resolver))
    await coordinator.run_once({"root-a": root})
    row = await target_store.search_local_tracks("Track")
    assert row == []
    stored = await target_store.get_stored_sibling_context("root-a", ".")
    assert stored[0]["availability"] == "missing"


@pytest.mark.asyncio
async def test_blocking_tag_read_stays_pausing_until_the_safe_boundary(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    track = root / "track-1.flac"
    track.write_bytes(b"one")
    resolver = _resolver(root)
    entered = threading.Event()
    release = threading.Event()

    class BlockingReader(_TagReader):
        def read_tags(self, path: Path) -> tuple[AudioTag, AudioInfo]:
            entered.set()
            if not release.wait(timeout=2):
                raise ValueError("test timeout")
            return super().read_tags(path)

    coordinator = _coordinator(target_store, resolver, BlockingReader())
    await coordinator.request_run(_request(resolver))
    worker = asyncio.create_task(coordinator.run_once({"root-a": root}))
    assert await asyncio.to_thread(entered.wait, 1)
    current = (await coordinator.current())[0]
    requested = await coordinator.control(current.id, "pause", current.row_revision)
    assert requested.state == "pausing"
    still_pausing = (await coordinator.current())[0]
    assert still_pausing.state == "pausing"
    release.set()
    result = await worker
    assert result is not None and result.state == "paused"
    assert await target_store.search_local_tracks("Track") == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("control", "expected_state"), [("pause", "paused"), ("stop", "cancelled")]
)
async def test_control_at_discovery_phase_boundary_is_settled(
    target_store: NativeLibraryStore,
    tmp_path: Path,
    control: str,
    expected_state: str,
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    await coordinator.request_run(_request(resolver))

    async def request_at_boundary(run, *_args, **_kwargs):
        current, _, _ = await target_store.get_scan_run(run.id)
        await coordinator.control(current.id, control, current.row_revision)
        return run

    coordinator._inventory.discover = request_at_boundary  # type: ignore[method-assign]

    result = await coordinator.run_once({"root-a": root})

    assert result is not None and result.state == expected_state
    persisted, _, _ = await target_store.get_scan_run(result.id)
    assert persisted.state == expected_state


@pytest.mark.asyncio
async def test_control_winning_discovery_revision_race_is_settled(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    await coordinator.request_run(_request(resolver))

    async def lose_revision_race(run, *_args, **_kwargs):
        current, _, _ = await target_store.get_scan_run(run.id)
        await coordinator.control(current.id, "pause", current.row_revision)
        raise StaleRevisionError("control won the scan batch revision race")

    coordinator._inventory.discover = lose_revision_race  # type: ignore[method-assign]

    result = await coordinator.run_once({"root-a": root})

    assert result is not None and result.state == "paused"


@pytest.mark.asyncio
async def test_discovery_cancellation_drains_the_bounded_thread_queue(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    for ordinal in range(600):
        (root / f"track-{ordinal}.flac").write_bytes(b"audio")
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    await coordinator.request_run(_request(resolver))
    run = await target_store.claim_next_scan_run(now=10)
    assert run is not None
    _, scopes, _ = await target_store.get_scan_run(run.id)
    scanner = LibraryInventoryScanner(target_store)
    task = asyncio.create_task(
        scanner.discover(
            run,
            scopes,
            {"root-a": root},
            resolver,
            coordinator.checkpoint,
        )
    )
    await asyncio.sleep(0)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(task, timeout=2)


@pytest.mark.asyncio
async def test_reconciliation_is_bounded_and_resumes_past_its_cursor(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    for ordinal in range(270):
        (root / f"track-{ordinal}.flac").write_bytes(b"audio")
    resolver = _resolver(root)
    coordinator = _coordinator(target_store, resolver)
    await coordinator.request_run(_request(resolver))
    completed = await coordinator.run_once({"root-a": root})
    assert completed is not None and completed.state == "completed"

    for path in root.iterdir():
        path.unlink()
    await coordinator.request_run(_request(resolver))
    reconciled = await coordinator.run_once({"root-a": root})
    assert reconciled is not None and reconciled.state == "completed"
    stored = await target_store.get_stored_sibling_context("root-a", ".")
    assert len(stored) == 270
    assert {row["availability"] for row in stored} == {"missing"}
    root.rmdir()
    await coordinator.request_run(_request(resolver))
    failed = await coordinator.run_once({"root-a": root})
    assert failed is not None and failed.state == "failed"
    stored = await target_store.get_stored_sibling_context("root-a", ".")
    assert stored[0]["availability"] == "missing"


@pytest.mark.asyncio
@pytest.mark.parametrize("phase", ["discovering", "indexing", "reconciling", "paused"])
async def test_policy_supersession_is_terminal_and_queues_nothing(
    target_store: NativeLibraryStore, tmp_path: Path, phase: str
) -> None:
    root = tmp_path / "music"
    root.mkdir()
    resolver = _resolver(root)
    current = resolver
    coordinator = LibraryScanCoordinator(
        target_store,
        LibraryInventoryScanner(target_store),
        LibraryIndexer(target_store, _TagReader()),
        LibraryReconciler(target_store),
        lambda: current,
        clock=lambda: 20,
    )
    await coordinator.request_run(_request(resolver))
    run = await target_store.claim_next_scan_run(now=20)
    assert run is not None
    if phase in {"indexing", "reconciling", "paused"}:
        run = await target_store.transition_scan_run(
            run.id,
            expected_state="discovering",
            expected_revision=run.row_revision,
            new_state="indexing",
            now=21,
        )
    if phase in {"reconciling", "paused"}:
        run = await target_store.transition_scan_run(
            run.id,
            expected_state="indexing",
            expected_revision=run.row_revision,
            new_state="reconciling",
            now=22,
        )
    if phase == "paused":
        requested = await coordinator.control(run.id, "pause", run.row_revision)
        run = await target_store.transition_scan_run(
            run.id,
            expected_state="pausing",
            expected_revision=requested.row_revision,
            new_state="paused",
            now=23,
        )
    current = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-a", path=str(root), label="Library", policy="excluded"
                )
            ]
        )
    )
    assert not await coordinator.checkpoint(run.id, resolver.policy_revision)
    terminal, _, _ = await target_store.get_scan_run(run.id)
    assert terminal.state == "superseded_policy_changed"
    assert await coordinator.current() == []
    assert await target_store.row_count("library_scan_inventory") == 0


@pytest.mark.asyncio
async def test_scheduler_anchor_is_not_hidden_by_many_policy_reconciliations(
    target_store: NativeLibraryStore, tmp_path: Path
) -> None:
    await target_store.create_scan_run(
        ScanRun(id="filesystem", kind="incremental", trigger="automatic", queued_at=1)
    )
    with sqlite3.connect(tmp_path / "target.db") as connection:
        connection.execute(
            "UPDATE library_scan_runs SET state = 'completed', phase = 'reconciling', "
            "terminal_at = queued_at, updated_at = queued_at WHERE id = 'filesystem'"
        )
    for index in range(60):
        await target_store.create_scan_run(
            ScanRun(
                id=f"policy-{index}",
                kind="policy_reconcile",
                trigger="policy_apply",
                aggregate_scope="selected",
                queued_at=10 + index,
            )
        )
        with sqlite3.connect(tmp_path / "target.db") as connection:
            connection.execute(
                "UPDATE library_scan_runs SET state = 'completed', phase = 'reconciling', "
                "terminal_at = queued_at, updated_at = queued_at WHERE id = ?",
                (f"policy-{index}",),
            )
    anchor = await target_store.get_latest_filesystem_scan_terminal()

    assert anchor is not None
    assert anchor.id == "filesystem"


@pytest.mark.parametrize(
    ("frequency", "duration"),
    [
        ("5min", 300),
        ("10min", 600),
        ("30min", 1_800),
        ("1hr", 3_600),
        ("6hr", 21_600),
        ("12hr", 43_200),
        ("24hr", 86_400),
        ("3d", 259_200),
        ("7d", 604_800),
    ],
)
def test_interval_schedule_anchors_to_terminal_time(
    frequency: str, duration: int
) -> None:
    terminal = 1_800_000_000.0
    now = datetime.fromtimestamp(terminal + duration)
    due = LibraryScheduleService.next_due(
        frequency, "03:00", terminal, now=now, timezone_name="Europe/London"
    )
    assert due is not None
    assert due.timestamp() == terminal + duration


def test_daily_schedule_handles_dst_and_manual() -> None:
    now = datetime.fromisoformat("2026-03-28T12:00:00+00:00")
    due = LibraryScheduleService.next_due(
        "daily",
        "01:30",
        now.timestamp(),
        now=now,
        timezone_name="Europe/London",
    )
    assert due is not None
    assert due.date().isoformat() == "2026-03-29"
    assert (due.hour, due.minute) == (2, 30)
    repeated_now = datetime.fromisoformat("2026-10-24T12:00:00+00:00")
    repeated = LibraryScheduleService.next_due(
        "daily",
        "01:30",
        repeated_now.timestamp(),
        now=repeated_now,
        timezone_name="Europe/London",
    )
    assert repeated is not None
    assert repeated.fold == 0
    assert (
        LibraryScheduleService.next_due(
            "manual", "03:00", None, now=now, timezone_name="Europe/London"
        )
        is None
    )
