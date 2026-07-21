"""Generated-inventory baseline and target benchmark for Feedback Fixes."""

from __future__ import annotations

import argparse
import asyncio
import importlib.util
import json
import os
import platform
import shutil
import sqlite3
import tempfile
import threading
import tracemalloc
from contextlib import nullcontext
from dataclasses import asdict
from datetime import date
from pathlib import Path
from time import perf_counter
from typing import Any, Literal
from unittest.mock import patch

# Some route imports resolve deployment paths. Keep every direct benchmark invocation
# isolated from /app and from the deployed data root.
_BENCHMARK_IMPORT_ROOT: tempfile.TemporaryDirectory[str] | None = None
if "ROOT_APP_DIR" not in os.environ:
    _BENCHMARK_IMPORT_ROOT = tempfile.TemporaryDirectory(
        prefix="feedback-fixes-benchmark-app-"
    )
    os.environ["ROOT_APP_DIR"] = _BENCHMARK_IMPORT_ROOT.name

import msgspec

from api.v1.schemas.library_policies import (
    LibraryPathPolicyRule,
    LibraryRootSettings,
    TypedLibrarySettings,
)
from infrastructure.observability.library_metrics import (
    LibraryMetrics,
    sample_event_loop_delay,
)
from infrastructure.persistence._database import PriorityWriteLock
from infrastructure.persistence.auth_store import AuthStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.persistence.maintenance_manifest import capture_source_identity
from infrastructure.sse_publisher import SSEPublisher
from models.audio import AudioInfo, AudioTag
from models.identification import CandidateEvidence, TrackEvidence
from models.library_work import ScanRequest, ScanScope
from services.native.library_indexer import INDEX_BATCH_SIZE, LibraryIndexer
from services.native.library_inventory_scanner import (
    INVENTORY_BATCH_SIZE,
    INVENTORY_QUEUE_SIZE,
    LibraryInventoryScanner,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.library_reconciler import LibraryReconciler
from services.native.library_scan_coordinator import LibraryScanCoordinator
from services.native.library_scan_events import LibraryScanEventPublisher
from services.native.identification_queue_service import IdentificationQueueService
from services.native.local_album_grouping_service import LocalAlbumGroupingService
from services.native.album_evidence_engine import MATCHER_VERSION
from tests.benchmarks.feedback_fixes_phase10_scenarios import (
    benchmark_evidence_protection,
    benchmark_fingerprint_playback,
    benchmark_flat_grouping,
    benchmark_staged_flat_grouping,
    benchmark_identification_backlog,
    benchmark_sse_protocol,
)
from tests.benchmarks.http_playback_probe import AuthenticatedHTTPPlaybackProbe

EVIDENCE_SUBJECTS = 50_000
EVIDENCE_CANDIDATES_PER_SUBJECT = 10
EVIDENCE_SIZE_LIMIT_BYTES = 2 * 1024 * 1024 * 1024
COMPACTED_SUBJECT_LIMIT_BYTES = 4096
SCAN_RSS_LIMIT_BYTES = 512 * 1024 * 1024
WAL_LIMIT_BYTES = 512 * 1024 * 1024
EVENT_LOOP_P99_LIMIT_SECONDS = 0.1
WRITE_LOCK_P95_LIMIT_SECONDS = 0.25
WRITE_LOCK_MAX_SECONDS = 2.0
PLAYBACK_MAX_SECONDS = 2.0


class _MeasuredLock(PriorityWriteLock):
    def __init__(self) -> None:
        super().__init__()
        self.waits: list[float] = []
        self.foreground_waits: list[float] = []
        self.background_waits: list[float] = []

    def acquire(self) -> None:
        started = perf_counter()
        super().acquire()
        elapsed = perf_counter() - started
        self.waits.append(elapsed)
        self.foreground_waits.append(elapsed)

    def acquire_background(self) -> None:
        started = perf_counter()
        super().acquire_background()
        elapsed = perf_counter() - started
        self.waits.append(elapsed)
        self.background_waits.append(elapsed)


class _BenchmarkStore(NativeLibraryStore):
    def __init__(
        self, db_path: Path, write_lock: _MeasuredLock, metrics: LibraryMetrics
    ) -> None:
        self._benchmark_metrics = metrics
        self.foreground_probe_transactions = 0
        self.grouping_transactions = 0
        self.grouping_active = False
        super().__init__(db_path, write_lock)  # type: ignore[arg-type]

    def _connect(self) -> sqlite3.Connection:
        connection = super()._connect()
        connection.set_trace_callback(
            lambda _sql: self._benchmark_metrics.increment("sql_statements")
        )
        return connection

    async def foreground_write_probe(self, sequence: int) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute(
                "INSERT INTO benchmark_foreground_probe (id, sequence) VALUES (1, ?) "
                "ON CONFLICT(id) DO UPDATE SET sequence = excluded.sequence",
                (sequence,),
            )

        await self._write(operation)
        self.foreground_probe_transactions += 1

    def _execute_background(self, operation: Any) -> Any:
        try:
            return super()._execute_background(operation)
        finally:
            if self.grouping_active:
                self.grouping_transactions += 1


class _MeasuredGroupingService(LocalAlbumGroupingService):
    def __init__(self, store: _BenchmarkStore) -> None:
        super().__init__(store, IdentificationQueueService(store))
        self._benchmark_store = store

    async def regroup_run(self, *args: Any, **kwargs: Any) -> int:
        self._benchmark_store.grouping_active = True
        try:
            return await super().regroup_run(*args, **kwargs)
        finally:
            self._benchmark_store.grouping_active = False


class _SyntheticTagReader:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.calls: list[Path] = []

    def read_tags(self, path: Path) -> tuple[AudioTag, AudioInfo]:
        self.calls.append(path)
        relative = path.relative_to(self.root)
        try:
            index = int(path.stem.rsplit("-", 1)[-1])
        except ValueError:
            index = len(self.calls)
        category = relative.parts[0]
        album_number = index // 20
        if category == "flat_untagged":
            album = ""
        elif category == "box_set":
            album = "Benchmark Box Set"
        else:
            album = f"Album {album_number}"
        artist = (
            "Benchmark Ensemble"
            if category == "box_set"
            else f"Artist {album_number % 5_000}"
        )
        return (
            AudioTag(
                title=f"Track {index}",
                artist=artist,
                album=album,
                album_artist=artist,
                track_number=index % 20 + 1,
                disc_number=1,
                genre="Benchmark",
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


class _CountingPublisher(SSEPublisher):
    def __init__(self) -> None:
        super().__init__()
        self.events: list[dict[str, object]] = []

    async def publish(self, channel: str, event: str, data: Any) -> None:
        self.events.append(
            {
                "channel": channel,
                "event": event,
                "at": perf_counter(),
                "row_revision": data.get("row_revision"),
                "stream_revision": data.get("stream_revision"),
            }
        )
        await super().publish(channel, event, data)


def _percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int(len(ordered) * fraction + 0.999) - 1))
    return ordered[index]


def generate_inventory(root: Path, count: int) -> dict[str, int]:
    """Create the approved mixed directory shape using zero-byte placeholder audio."""
    categories = {
        "ordinary": 0,
        "multidisc": 0,
        "flat_tagged": 0,
        "flat_untagged": 0,
        "local_metadata": 0,
        "excluded": 0,
    }
    names = tuple(categories)
    for index in range(count):
        category = names[index % len(names)]
        if category == "ordinary":
            parent = root / category / f"album-{index // 60:06d}"
        elif category == "multidisc":
            parent = root / category / f"box-{index // 120:06d}" / f"CD{index % 2 + 1}"
        else:
            parent = root / category
        parent.mkdir(parents=True, exist_ok=True)
        (parent / f"track-{index:09d}.flac").touch()
        categories[category] += 1
    return categories


def generate_box_set(root: Path, count: int = 100) -> dict[str, int]:
    for index in range(count):
        disc = index // 20 + 1
        parent = root / "box_set" / f"CD{disc}"
        parent.mkdir(parents=True, exist_ok=True)
        (parent / f"track-{index:03d}.flac").touch()
    return {"box_set": count}


def benchmark_inventory(count: int) -> dict[str, object]:
    metrics = LibraryMetrics.for_library_workload()
    with tempfile.TemporaryDirectory(prefix=f"feedback-fixes-{count}-") as directory:
        root = Path(directory)
        started = perf_counter()
        shape = generate_inventory(root, count)
        generation_seconds = perf_counter() - started
        metrics.sample_rss()

        database = root / "inventory.db"
        connection = sqlite3.connect(database)
        connection.execute("PRAGMA foreign_keys=ON")
        connection.set_trace_callback(lambda _sql: metrics.increment("sql_statements"))
        connection.execute("PRAGMA journal_mode=WAL")
        connection.execute(
            "CREATE TABLE inventory(path TEXT PRIMARY KEY, size INTEGER, mtime_ns INTEGER)"
        )
        discovered = 0
        walk_started = perf_counter()
        for current, _directories, files in os.walk(root):
            if current == str(root):
                metrics.increment("walks")
            batch = []
            for filename in files:
                if not filename.endswith(".flac"):
                    continue
                path = Path(current) / filename
                stat = path.stat()
                metrics.increment("stats")
                batch.append(
                    (str(path.relative_to(root)), stat.st_size, stat.st_mtime_ns)
                )
                discovered += 1
            if batch:
                with metrics.timer("sql_transaction_seconds"):
                    connection.executemany(
                        "INSERT INTO inventory VALUES (?, ?, ?)", batch
                    )
                    connection.commit()
                metrics.increment("sql_transactions")
        walk_seconds = perf_counter() - walk_started
        metrics.sample_rss()
        wal_path = Path(f"{database}-wal")
        if wal_path.exists():
            metrics.set_peak("wal_bytes", wal_path.stat().st_size)
        connection.close()

        snapshot = asdict(metrics.snapshot())
        return {
            "file_count": count,
            "shape": shape,
            "generation_seconds": generation_seconds,
            "walk_and_inventory_seconds": walk_seconds,
            "files_per_second": discovered / walk_seconds if walk_seconds else None,
            "tag_reads": 0,
            "fingerprints": 0,
            "external_calls": 0,
            "metrics": snapshot,
        }


def _scan_request(resolver: LibraryPolicyResolver) -> ScanRequest:
    return ScanRequest(
        kind="incremental",
        trigger="manual",
        policy_revision=resolver.policy_revision,
        scopes=[
            ScanScope(
                root_id="benchmark-root",
                relative_path=".",
                policy_revision=resolver.policy_revision,
            )
        ],
    )


async def _profile_scan(
    coordinator: LibraryScanCoordinator,
    store: _BenchmarkStore,
    root: Path,
    database: Path,
    metrics: LibraryMetrics,
    playback: AuthenticatedHTTPPlaybackProbe,
) -> tuple[object, float]:
    stopped = asyncio.Event()

    async def sample_runtime() -> None:
        loop = asyncio.get_running_loop()
        expected = loop.time() + 0.01
        while not stopped.is_set():
            await asyncio.sleep(max(0.0, expected - loop.time()))
            metrics.observe(
                "event_loop_delay_seconds", max(0.0, loop.time() - expected)
            )
            metrics.sample_rss()
            wal = Path(f"{database}-wal")
            try:
                metrics.set_peak("wal_bytes", float(wal.stat().st_size))
            except FileNotFoundError:
                pass
            expected += 0.01

    async def sample_playback() -> None:
        while not stopped.is_set():
            elapsed = await playback.sample()
            metrics.increment("playback_starts")
            metrics.observe("playback_start_seconds", elapsed)
            auth_elapsed = await playback.sample_auth_me()
            metrics.increment("auth_me_requests")
            metrics.observe("auth_me_seconds", auth_elapsed)
            try:
                await asyncio.wait_for(stopped.wait(), timeout=0.5)
            except TimeoutError:
                pass

    async def sample_foreground_write() -> None:
        sequence = 0
        while not stopped.is_set():
            started = perf_counter()
            await store.foreground_write_probe(sequence)
            metrics.observe("foreground_write_seconds", perf_counter() - started)
            sequence += 1
            try:
                await asyncio.wait_for(stopped.wait(), timeout=0.1)
            except TimeoutError:
                pass

    monitors = [
        asyncio.create_task(sample_runtime()),
        asyncio.create_task(sample_playback()),
        asyncio.create_task(sample_foreground_write()),
    ]
    started = perf_counter()
    try:
        result = await coordinator.run_once({"benchmark-root": root})
        return result, perf_counter() - started
    finally:
        stopped.set()
        await asyncio.gather(*monitors)


async def benchmark_target_scan(
    count: int,
    *,
    layout: Literal["mixed", "box_set"] = "mixed",
    stat_delay_seconds: float = 0.0,
) -> dict[str, object]:
    """Run first, unchanged, and one-file-change scans through the target stack."""

    with tempfile.TemporaryDirectory(prefix=f"feedback-target-{count}-") as directory:
        workspace = Path(directory)
        root = workspace / "Music"
        root.mkdir()
        shape = (
            generate_inventory(root, count)
            if layout == "mixed"
            else generate_box_set(root, count)
        )
        playback_path = workspace / "active-playback.flac"
        shutil.copy2(
            Path(__file__).parents[1] / "fixtures" / "library" / "flac_full_01.flac",
            playback_path,
        )
        with playback_path.open("ab") as playback_stream:
            playback_stream.truncate(256 * 1024)
        database = workspace / "target.db"
        metrics = LibraryMetrics.for_library_workload()
        lock = _MeasuredLock()
        AuthStore(database, lock)
        with sqlite3.connect(database) as connection:
            connection.execute(
                "CREATE TABLE benchmark_foreground_probe "
                "(id INTEGER PRIMARY KEY, sequence INTEGER NOT NULL)"
            )
        store = _BenchmarkStore(database, lock, metrics)
        reader = _SyntheticTagReader(root)
        resolver = LibraryPolicyResolver(
            TypedLibrarySettings(
                library_roots=[
                    LibraryRootSettings(
                        id="benchmark-root",
                        path=str(root),
                        label="Benchmark",
                        policy="automatic",
                        rules=(
                            [
                                LibraryPathPolicyRule(
                                    id="benchmark-local-metadata",
                                    relative_path="local_metadata",
                                    policy="local_metadata",
                                ),
                                LibraryPathPolicyRule(
                                    id="benchmark-excluded",
                                    relative_path="excluded",
                                    policy="excluded",
                                ),
                            ]
                            if layout == "mixed"
                            else []
                        ),
                    )
                ]
            )
        )
        publisher = _CountingPublisher()
        events = LibraryScanEventPublisher(store, publisher)
        walk_calls = 0

        def counted_walk(*args: object, **kwargs: object):
            nonlocal walk_calls
            walk_calls += 1
            return os.walk(*args, **kwargs)

        coordinator = LibraryScanCoordinator(
            store,
            LibraryInventoryScanner(store, directory_walker=counted_walk),
            LibraryIndexer(store, reader, _MeasuredGroupingService(store)),
            LibraryReconciler(store),
            lambda: resolver,
            events,
        )

        playback_probe = await AuthenticatedHTTPPlaybackProbe(
            store, workspace, playback_path
        ).__aenter__()

        print(
            json.dumps(
                {"benchmark": "target_scan", "files": count, "stage": "idle_baseline"}
            ),
            flush=True,
        )
        idle_playback = [await playback_probe.sample() for _ in range(30)]
        print(
            json.dumps(
                {"benchmark": "target_scan", "files": count, "stage": "scenarios"}
            ),
            flush=True,
        )
        idle_rss = metrics.sample_rss() or 0
        lock.waits.clear()
        lock.foreground_waits.clear()
        lock.background_waits.clear()
        fingerprint_calls = 0
        external_calls = 0

        async def run_scenario(name: str) -> dict[str, object]:
            nonlocal walk_calls, fingerprint_calls, external_calls
            reader.calls.clear()
            walks_before = walk_calls
            fingerprints_before = fingerprint_calls
            external_before = external_calls
            sql_before = metrics.snapshot().counters["sql_statements"]
            all_locks_before = len(lock.waits)
            background_locks_before = len(lock.background_waits)
            probes_before = store.foreground_probe_transactions
            grouping_before = store.grouping_transactions
            event_before = len(publisher.events)
            print(
                json.dumps(
                    {
                        "benchmark": "target_scan",
                        "files": count,
                        "scenario": name,
                        "stage": "started",
                    }
                ),
                flush=True,
            )
            requested = await coordinator.request_run(_scan_request(resolver))
            completed, elapsed = await _profile_scan(
                coordinator, store, root, database, metrics, playback_probe
            )
            print(
                json.dumps(
                    {
                        "benchmark": "target_scan",
                        "files": count,
                        "scenario": name,
                        "stage": "completed",
                    }
                ),
                flush=True,
            )
            snapshot = await coordinator.snapshot(requested.run_id)
            scenario_events = publisher.events[event_before:]
            counter_times = [
                float(item["at"])
                for item in scenario_events
                if item["event"] == "scan.progress"
            ]
            counter_spacings = [
                later - earlier
                for earlier, later in zip(counter_times, counter_times[1:])
            ]
            first_event_at = float(scenario_events[0]["at"]) if scenario_events else 0.0
            scan_write_transactions = (
                len(lock.waits)
                - all_locks_before
                - (store.foreground_probe_transactions - probes_before)
                - (store.grouping_transactions - grouping_before)
            )
            return {
                "name": name,
                "state": completed.state if completed is not None else None,
                "elapsed_seconds": elapsed,
                "phase_timings_seconds": (
                    completed.phase_timings if completed is not None else {}
                ),
                "files_per_second": count / elapsed if elapsed else None,
                "tag_reads": len(reader.calls),
                "fingerprints": fingerprint_calls - fingerprints_before,
                "external_calls": external_calls - external_before,
                "walks": walk_calls - walks_before,
                "sql_statements": metrics.snapshot().counters["sql_statements"]
                - sql_before,
                "write_transactions": scan_write_transactions,
                "background_write_transactions": len(lock.background_waits)
                - background_locks_before,
                "grouping_write_transactions": store.grouping_transactions
                - grouping_before,
                "files_per_write_transaction": (
                    count / scan_write_transactions
                    if scan_write_transactions
                    else None
                ),
                "sse_events": len(scenario_events),
                "sse_counter_events": len(counter_times),
                "sse_counter_spacing_seconds": counter_spacings,
                "sse_event_trace": [
                    {
                        "event": item["event"],
                        "offset_seconds": float(item["at"]) - first_event_at,
                        "row_revision": item["row_revision"],
                        "stream_revision": item["stream_revision"],
                    }
                    for item in scenario_events
                ],
                "sse_rate_passed": all(spacing >= 2.0 for spacing in counter_spacings),
                "counters": snapshot.counters,
                "rss_after_scenario_bytes": metrics.sample_rss(),
            }

        original_stat = Path.stat

        def delayed_stat(path: Path, *args: object, **kwargs: object):
            if (
                stat_delay_seconds > 0
                and path.suffix.casefold() == ".flac"
                and path.is_relative_to(root)
            ):
                threading.Event().wait(stat_delay_seconds)
            return original_stat(path, *args, **kwargs)

        stat_context = (
            patch.object(Path, "stat", delayed_stat)
            if stat_delay_seconds > 0
            else nullcontext()
        )

        async def counted_fingerprint(*_args: object, **_kwargs: object):
            nonlocal fingerprint_calls
            fingerprint_calls += 1
            return None

        async def counted_external_list(*_args: object, **_kwargs: object):
            nonlocal external_calls
            external_calls += 1
            return []

        async def counted_external_detail(*_args: object, **_kwargs: object):
            nonlocal external_calls
            external_calls += 1
            return None

        with (
            stat_context,
            patch(
                "services.native.conditional_fingerprint_service.ConditionalFingerprintService.fingerprint_if_needed",
                counted_fingerprint,
            ),
            patch(
                "repositories.musicbrainz_identification_repository.MusicBrainzIdentificationRepository.search_album_candidate_ids",
                counted_external_list,
            ),
            patch(
                "repositories.musicbrainz_identification_repository.MusicBrainzIdentificationRepository.search_recording_candidate_ids",
                counted_external_list,
            ),
            patch(
                "repositories.musicbrainz_identification_repository.MusicBrainzIdentificationRepository.get_album_candidate",
                counted_external_detail,
            ),
        ):
            first = await run_scenario("first_local_index")
            unchanged = await run_scenario("unchanged")
            changed_path = (
                next((root / "ordinary").rglob("*.flac"))
                if layout == "mixed"
                else next((root / "box_set").rglob("*.flac"))
            )
            changed_path.write_bytes(b"changed")
            changed = await run_scenario("one_file_changed")
        snapshot = asdict(metrics.snapshot())
        peak_rss = int(snapshot["peaks"].get("rss_bytes", idle_rss))
        wal_bytes = int(snapshot["peaks"].get("wal_bytes", 0))
        lock_p95 = _percentile(lock.foreground_waits, 0.95) or 0.0
        lock_max = max(lock.foreground_waits, default=0.0)
        event_loop = snapshot["distributions"].get("event_loop_delay_seconds", {})
        playback = snapshot["distributions"].get("playback_start_seconds", {})
        auth_me = snapshot["distributions"].get("auth_me_seconds", {})
        idle_playback_p95 = _percentile(idle_playback, 0.95) or 0.0
        loaded_playback_p95 = playback.get("p95") or 0.0
        playback_http_evidence = playback_probe.evidence()
        await playback_probe.__aexit__(None, None, None)
        with sqlite3.connect(database) as connection:
            page_size = int(connection.execute("PRAGMA page_size").fetchone()[0])
            page_count = int(connection.execute("PRAGMA page_count").fetchone()[0])
            schema_version = int(
                connection.execute("PRAGMA schema_version").fetchone()[0]
            )
            local_album_count = int(
                connection.execute(
                    "SELECT COUNT(*) FROM local_albums "
                    "WHERE retired_into_album_id IS NULL"
                ).fetchone()[0]
            )
            queued_identification_count = int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_identification_jobs "
                    "WHERE state = 'queued'"
                ).fetchone()[0]
            )
        return {
            "file_count": count,
            "layout": layout,
            "filesystem_latency": {
                "kind": "simulated" if stat_delay_seconds else "native",
                "stat_delay_seconds": stat_delay_seconds,
            },
            "shape": shape,
            "queue_capacity": INVENTORY_QUEUE_SIZE,
            "inventory_batch_size": INVENTORY_BATCH_SIZE,
            "index_batch_size": INDEX_BATCH_SIZE,
            "first_local_index": first,
            "unchanged": unchanged,
            "small_change": changed,
            "idle_rss_bytes": idle_rss,
            "peak_rss_bytes": peak_rss,
            "rss_above_idle_bytes": max(0, peak_rss - idle_rss),
            "rss_after_first_bytes": first["rss_after_scenario_bytes"],
            "rss_after_unchanged_bytes": unchanged["rss_after_scenario_bytes"],
            "rss_after_small_change_bytes": changed["rss_after_scenario_bytes"],
            "rss_growth_after_first_bytes": max(
                0,
                int(changed["rss_after_scenario_bytes"] or 0)
                - int(first["rss_after_scenario_bytes"] or 0),
            ),
            "memory_interpretation": (
                "RSS includes SQLite page cache and Python allocator retention; "
                f"the discovery queue is bounded at {INVENTORY_QUEUE_SIZE} items and "
                "post-first-scan growth is reported separately."
            ),
            "peak_wal_bytes": wal_bytes,
            "database_bytes": page_size * page_count,
            "sqlite_schema_version": schema_version,
            "local_album_count": local_album_count,
            "queued_identification_count": queued_identification_count,
            "catalog_readable_before_identification_completion": (
                local_album_count > 0 and queued_identification_count > 0
            ),
            "write_lock_wait_p95_seconds": lock_p95,
            "write_lock_wait_max_seconds": lock_max,
            "foreground_write_samples": len(lock.foreground_waits),
            "background_write_samples": len(lock.background_waits),
            "event_loop_delay_p99_seconds": event_loop.get("p99", 0.0),
            "idle_playback_start_p95_seconds": idle_playback_p95,
            "loaded_playback_start_p95_seconds": loaded_playback_p95,
            "loaded_playback_samples": playback.get("count", 0),
            "auth_me_p95_seconds": auth_me.get("p95") or 0.0,
            "auth_me_samples": auth_me.get("count", 0),
            "loaded_minus_idle_playback_p95_seconds": max(
                0.0, loaded_playback_p95 - idle_playback_p95
            ),
            "playback_http_evidence": playback_http_evidence,
            "gates": {
                "first_scan_one_walk": first["walks"] == 1,
                "unchanged_one_walk": unchanged["walks"] == 1,
                "unchanged_zero_tag_reads": unchanged["tag_reads"] == 0,
                "small_change_one_tag_read": changed["tag_reads"] == 1,
                "first_scan_transaction_ratio": count < 4_096
                or first["write_transactions"] / count < 0.03,
                "unchanged_transaction_ratio": count < 4_096
                or unchanged["write_transactions"] / count < 0.02,
                "zero_scan_fingerprints": all(
                    scenario["fingerprints"] == 0
                    for scenario in (first, unchanged, changed)
                ),
                "zero_scan_external_calls": all(
                    scenario["external_calls"] == 0
                    for scenario in (first, unchanged, changed)
                ),
                "rss": max(0, peak_rss - idle_rss) <= SCAN_RSS_LIMIT_BYTES,
                "wal": wal_bytes <= WAL_LIMIT_BYTES,
                "event_loop": (event_loop.get("p99") or 0.0)
                <= EVENT_LOOP_P99_LIMIT_SECONDS,
                "write_lock_p95": lock_p95 <= WRITE_LOCK_P95_LIMIT_SECONDS,
                "write_lock_max": lock_max <= WRITE_LOCK_MAX_SECONDS,
                "playback": loaded_playback_p95 <= idle_playback_p95 + 0.5
                and loaded_playback_p95 <= PLAYBACK_MAX_SECONDS,
                "auth_me": (auth_me.get("p95") or 0.0) <= 0.5,
                "sse_rate": all(
                    scenario["sse_rate_passed"]
                    for scenario in (first, unchanged, changed)
                ),
            },
        }


async def benchmark_unavailable_root() -> dict[str, object]:
    with tempfile.TemporaryDirectory(prefix="feedback-unavailable-root-") as directory:
        workspace = Path(directory)
        root = workspace / "unavailable"
        database = workspace / "target.db"
        with sqlite3.connect(database) as connection:
            connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        store = NativeLibraryStore(database, threading.Lock())
        resolver = LibraryPolicyResolver(
            TypedLibrarySettings(
                library_roots=[
                    LibraryRootSettings(
                        id="benchmark-root",
                        path=str(root),
                        label="Unavailable benchmark root",
                    )
                ]
            )
        )
        reader = _SyntheticTagReader(root)
        coordinator = LibraryScanCoordinator(
            store,
            LibraryInventoryScanner(store),
            LibraryIndexer(store, reader),
            LibraryReconciler(store),
            lambda: resolver,
        )
        requested = await coordinator.request_run(_scan_request(resolver))
        started = perf_counter()
        completed = await coordinator.run_once({"benchmark-root": root})
        elapsed = perf_counter() - started
        snapshot = await coordinator.snapshot(requested.run_id)
        return {
            "elapsed_seconds": elapsed,
            "state": completed.state if completed else None,
            "terminal_code": completed.terminal_code if completed else None,
            "tag_reads": len(reader.calls),
            "tag_read_paths": [str(path.relative_to(root)) for path in reader.calls],
            "fingerprints": 0,
            "external_calls": 0,
            "counters": snapshot.counters,
            "missing_reconciliation_rows": snapshot.counters.get("missing_count", 0),
            "passed": completed is not None
            and completed.state == "failed"
            and completed.terminal_code == "ROOT_UNAVAILABLE"
            and not reader.calls
            and snapshot.counters.get("missing_count", 0) == 0,
        }


async def benchmark_migration_handoff_scan() -> dict[str, object]:
    """Run the immediate verification scan against the database migration produced."""

    fixture_path = (
        Path(__file__).parents[1]
        / "infrastructure"
        / "test_legacy_catalog_importer.py"
    )
    spec = importlib.util.spec_from_file_location(
        "feedback_fixes_scan_handoff_fixture", fixture_path
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("The migration handoff fixture could not be loaded.")
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    with tempfile.TemporaryDirectory(prefix="feedback-migration-scan-") as directory:
        workspace = Path(directory)
        root = workspace / "Music"
        root.mkdir()
        database = workspace / "library.db"
        fixture._create_source(database, root)
        files = (
            ("Compilation/01.flac", 100, 10_500_000_000),
            ("Compilation/02.flac", 200, 10_750_000_000),
            ("Local Album/01.flac", 300, 11_500_000_000),
            ("Rejected/01.flac", 250, 13_000_000_000),
        )
        for relative_path, size, mtime_ns in files:
            path = root / relative_path
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("wb") as stream:
                stream.truncate(size)
            os.utime(path, ns=(mtime_ns, mtime_ns))
        _fixture_store, importer = fixture._importer(database, root)
        plan, _ = await importer.prepare("benchmark-handoff", now=100)
        await importer.apply(
            "benchmark-handoff",
            expected_source_revision=plan.source_revision,
            now=101,
        )
        with sqlite3.connect(database) as connection:
            legacy_before = int(
                connection.execute(
                    "SELECT COUNT(*) FROM local_tracks "
                    "WHERE stat_revision_kind!='exact'"
                ).fetchone()[0]
            )
        metrics = LibraryMetrics.for_library_workload()
        lock = _MeasuredLock()
        store = _BenchmarkStore(database, lock, metrics)
        reader = _SyntheticTagReader(root)
        resolver = LibraryPolicyResolver(
            TypedLibrarySettings(
                library_roots=[
                    LibraryRootSettings(
                        id="root-1", path=str(root), label="Migrated", policy="automatic"
                    )
                ]
            )
        )
        coordinator = LibraryScanCoordinator(
            store,
            LibraryInventoryScanner(store),
            LibraryIndexer(store, reader),
            LibraryReconciler(store),
            lambda: resolver,
        )
        request = ScanRequest(
            kind="incremental",
            trigger="startup_resume",
            policy_revision=resolver.policy_revision,
            scopes=[
                ScanScope(
                    root_id="root-1",
                    relative_path=".",
                    policy_revision=resolver.policy_revision,
                )
            ],
        )
        await coordinator.request_run(request)
        transactions_before = len(lock.waits)
        started = perf_counter()
        completed = await coordinator.run_once({"root-1": root})
        elapsed = perf_counter() - started
        snapshot = await coordinator.snapshot(completed.id) if completed else None
        with sqlite3.connect(database) as connection:
            exact_after = int(
                connection.execute(
                    "SELECT COUNT(*) FROM local_tracks "
                    "WHERE stat_revision_kind='exact'"
                ).fetchone()[0]
            )
            migrated_tracks = int(
                connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone()[0]
            )
        report = {
            "state": completed.state if completed is not None else None,
            "elapsed_seconds": elapsed,
            "migrated_tracks": migrated_tracks,
            "legacy_revisions_before_scan": legacy_before,
            "exact_revisions_after_scan": exact_after,
            "tag_reads": len(reader.calls),
            "tag_read_paths": [str(path.relative_to(root)) for path in reader.calls],
            "write_transactions": len(lock.waits) - transactions_before,
            "counters": snapshot.counters if snapshot is not None else {},
        }
        report["passed"] = (
            report["state"] == "completed"
            and legacy_before > 0
            and exact_after == migrated_tracks
            and report["tag_reads"] == 0
        )
        return report


async def benchmark_scan_control_latency() -> dict[str, object]:
    async def run_control(control: Literal["pause", "stop"]) -> dict[str, object]:
        with tempfile.TemporaryDirectory(
            prefix=f"feedback-{control}-latency-"
        ) as directory:
            workspace = Path(directory)
            root = workspace / "Music"
            root.mkdir()
            (root / "track-1.flac").touch()
            (root / "track-2.flac").touch()
            database = workspace / "target.db"
            with sqlite3.connect(database) as connection:
                connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
            store = NativeLibraryStore(database, threading.Lock())
            resolver = LibraryPolicyResolver(
                TypedLibrarySettings(
                    library_roots=[
                        LibraryRootSettings(
                            id="benchmark-root",
                            path=str(root),
                            label="Control benchmark",
                        )
                    ]
                )
            )
            entered = threading.Event()
            release = threading.Event()

            class BlockingReader(_SyntheticTagReader):
                def read_tags(self, path: Path) -> tuple[AudioTag, AudioInfo]:
                    entered.set()
                    if not release.wait(timeout=5):
                        raise ValueError("benchmark control checkpoint timed out")
                    return super().read_tags(path)

            coordinator = LibraryScanCoordinator(
                store,
                LibraryInventoryScanner(store),
                LibraryIndexer(store, BlockingReader(root)),
                LibraryReconciler(store),
                lambda: resolver,
            )
            await coordinator.request_run(_scan_request(resolver))
            worker = asyncio.create_task(coordinator.run_once({"benchmark-root": root}))
            if not await asyncio.to_thread(entered.wait, 5):
                raise RuntimeError("The control benchmark did not enter tag reading.")
            current = (await coordinator.current())[0]
            request_started = perf_counter()
            requested = await coordinator.control(
                current.id, control, current.row_revision
            )
            request_seconds = perf_counter() - request_started
            checkpoint_started = perf_counter()
            release.set()
            completed = await worker
            checkpoint_seconds = perf_counter() - checkpoint_started
            acknowledged_in_time = request_seconds <= 1.0
            checkpointed_in_time = checkpoint_seconds <= 2.0
            return {
                "requested_state": requested.state,
                "terminal_state": completed.state if completed else None,
                "request_seconds": request_seconds,
                "request_to_checkpoint_seconds": checkpoint_seconds,
                "passed": completed is not None
                and completed.state
                == ("paused" if control == "pause" else "cancelled")
                and acknowledged_in_time
                and checkpointed_in_time,
            }

    pause = await run_control("pause")
    stop = await run_control("stop")
    return {
        "pause": pause,
        "stop": stop,
        "within_control_deadline": max(
            float(pause["request_to_checkpoint_seconds"]),
            float(stop["request_to_checkpoint_seconds"]),
        )
        <= 2.0,
        "passed": pause["passed"] and stop["passed"],
    }


async def benchmark_latency(*, loaded: bool = False) -> dict[str, object]:
    """Measure isolated idle or CPU-loaded event-loop, lock, and playback latency."""
    metrics = LibraryMetrics.for_library_workload()
    with tempfile.TemporaryDirectory(prefix="feedback-fixes-latency-") as directory:
        audio = Path(directory) / "sample.flac"
        audio.write_bytes(b"fLaC" + b"\0" * (1024 * 1024))

        async def playback_probe() -> None:
            started = perf_counter()

            def read_first_chunk() -> None:
                with audio.open("rb") as stream:
                    stream.read(64 * 1024)

            await asyncio.to_thread(read_first_chunk)
            metrics.increment("playback_starts")
            metrics.observe("playback_start_seconds", perf_counter() - started)

        tasks = [
            sample_event_loop_delay(
                metrics, duration_seconds=0.25, interval_seconds=0.005
            ),
            *(playback_probe() for _ in range(20)),
        ]
        if loaded:

            def cpu_load() -> int:
                return sum(value * value for value in range(2_000_000))

            tasks.append(asyncio.to_thread(cpu_load))
        await asyncio.gather(*tasks)

        lock = asyncio.Lock()
        for _ in range(20):
            started = perf_counter()
            async with lock:
                metrics.observe("write_lock_wait_seconds", perf_counter() - started)

        metrics.set_peak("wal_bytes", 0)
        metrics.sample_rss()
        return asdict(metrics.snapshot())


def benchmark_evidence_storage(
    subjects: int = EVIDENCE_SUBJECTS,
    candidates_per_subject: int = EVIDENCE_CANDIDATES_PER_SUBJECT,
) -> dict[str, object]:
    """Materialize the approved evidence retention shape in NativeLibraryStore."""
    with tempfile.TemporaryDirectory(prefix="feedback-fixes-evidence-") as directory:
        database = Path(directory) / "evidence.db"
        with sqlite3.connect(database) as seed:
            seed.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        NativeLibraryStore(database, threading.Lock())
        connection = sqlite3.connect(database)
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute("PRAGMA journal_mode=WAL")
        connection.execute("PRAGMA synchronous=NORMAL")
        connection.execute(
            "INSERT INTO local_artists "
            "(id, display_name, folded_name, normalized_name, kind, created_at, updated_at) "
            "VALUES ('benchmark-artist', 'Benchmark Artist', 'benchmark artist', "
            "'benchmark artist', 'person', 1, 1)"
        )
        started = perf_counter()
        tracemalloc.start()
        payload_bytes = 0
        evidence_batch: list[tuple[str, str, str, bytes, int, float]] = []
        album_batch: list[tuple[str, str, str, str]] = []
        attempt_batch: list[tuple[str, str, int]] = []
        max_evidence_batch_rows = 0
        for subject in range(subjects):
            attempt_id = f"attempt-{subject:08d}"
            album_id = f"album-{subject:08d}"
            album_batch.append(
                (
                    album_id,
                    f"group-{subject:08d}",
                    f"Album {subject}",
                    f"album {subject}",
                )
            )
            attempt_batch.append((attempt_id, album_id, candidates_per_subject))
            for candidate in range(candidates_per_subject):
                evidence = CandidateEvidence(
                    release_group_mbid=f"rg-{subject:08d}-{candidate:02d}",
                    release_mbid=f"release-{subject:08d}-{candidate:02d}",
                    album_title=f"Album {subject}",
                    album_artist_name=f"Artist {subject % 5000}",
                    track_evidence=[
                        TrackEvidence(
                            local_track_id=f"track-{subject:08d}",
                            classification="contradictory",
                            evidence_kinds=["no_acceptable_candidate_track"],
                            candidate_track_title=f"Candidate {candidate}",
                        )
                    ],
                    score=0.1,
                    reason_code="NO_EXTERNAL_RESULT",
                    matcher_version="feedback-fixes-v1",
                )
                encoded = msgspec.json.encode(evidence)
                payload_bytes += len(encoded)
                evidence_batch.append(
                    (
                        f"evidence-{subject:08d}-{candidate:02d}",
                        attempt_id,
                        f"rg-{subject:08d}-{candidate:02d}",
                        encoded,
                        len(encoded),
                        1.0,
                    )
                )
            max_evidence_batch_rows = max(max_evidence_batch_rows, len(evidence_batch))
            if len(evidence_batch) >= 2_000:
                connection.executemany(
                    "INSERT INTO local_albums "
                    "(id, root_id, grouping_key, title, title_folded, album_artist_id, "
                    "grouping_source, created_at, updated_at) "
                    "VALUES (?, 'benchmark-root', ?, ?, ?, 'benchmark-artist', "
                    "'automatic', 1, 1)",
                    album_batch,
                )
                connection.executemany(
                    "INSERT INTO library_identification_attempts "
                    "(id, local_album_id, trigger, input_tag_revision, "
                    "input_policy_revision, input_file_revision, matcher_version, state, "
                    "terminal_reason_code, candidate_count, started_at, completed_at) "
                    "VALUES (?, ?, 'automatic', 'tag', 'policy', 'file', "
                    "'feedback-fixes-v1', 'no_candidate', 'NO_EXTERNAL_RESULT', ?, 1, 1)",
                    attempt_batch,
                )
                connection.executemany(
                    "INSERT INTO library_identification_evidence "
                    "(id, attempt_id, candidate_key, evidence_json, evidence_size_bytes, created_at) "
                    "VALUES (?,?,?,?,?,?)",
                    evidence_batch,
                )
                connection.commit()
                album_batch.clear()
                attempt_batch.clear()
                evidence_batch.clear()
        if album_batch:
            connection.executemany(
                "INSERT INTO local_albums "
                "(id, root_id, grouping_key, title, title_folded, album_artist_id, "
                "grouping_source, created_at, updated_at) "
                "VALUES (?, 'benchmark-root', ?, ?, ?, 'benchmark-artist', "
                "'automatic', 1, 1)",
                album_batch,
            )
            connection.executemany(
                "INSERT INTO library_identification_attempts "
                "(id, local_album_id, trigger, input_tag_revision, input_policy_revision, "
                "input_file_revision, matcher_version, state, terminal_reason_code, "
                "candidate_count, started_at, completed_at) "
                "VALUES (?, ?, 'automatic', 'tag', 'policy', 'file', "
                "'feedback-fixes-v1', 'no_candidate', 'NO_EXTERNAL_RESULT', ?, 1, 1)",
                attempt_batch,
            )
        if evidence_batch:
            connection.executemany(
                "INSERT INTO library_identification_evidence "
                "(id, attempt_id, candidate_key, evidence_json, evidence_size_bytes, created_at) "
                "VALUES (?,?,?,?,?,?)",
                evidence_batch,
            )
            connection.commit()
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        current_memory, peak_memory = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        page_size = int(connection.execute("PRAGMA page_size").fetchone()[0])
        page_count = int(connection.execute("PRAGMA page_count").fetchone()[0])
        evidence_rows = int(
            connection.execute(
                "SELECT COUNT(*) FROM library_identification_evidence"
            ).fetchone()[0]
        )
        index_rows = int(
            connection.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' "
                "AND tbl_name IN ('library_identification_attempts',"
                "'library_identification_evidence')"
            ).fetchone()[0]
        )
        connection.close()
        elapsed = perf_counter() - started
        compacted = msgspec.json.encode(
            CandidateEvidence(
                release_group_mbid="rg",
                reason_code="NO_EXTERNAL_RESULT",
                matcher_version="feedback-fixes-v1",
            )
        )
        database_bytes = page_size * page_count
        return {
            "subjects": subjects,
            "candidates_per_subject": candidates_per_subject,
            "candidate_rows": evidence_rows,
            "schema": "NativeLibraryStore",
            "database_and_indexes_bytes": database_bytes,
            "payload_bytes": payload_bytes,
            "index_count": index_rows,
            "elapsed_seconds": elapsed,
            "rows_per_second": evidence_rows / elapsed if elapsed else None,
            "peak_python_bytes": peak_memory,
            "current_python_bytes": current_memory,
            "max_evidence_batch_rows": max_evidence_batch_rows,
            "compacted_subject_bytes": len(compacted),
            "size_gate_bytes": EVIDENCE_SIZE_LIMIT_BYTES,
            "compaction_gate_bytes": COMPACTED_SUBJECT_LIMIT_BYTES,
            "size_gate_passed": database_bytes <= EVIDENCE_SIZE_LIMIT_BYTES,
            "compaction_gate_passed": len(compacted) <= COMPACTED_SUBJECT_LIMIT_BYTES,
        }


def _source_identity() -> dict[str, object]:
    return capture_source_identity(Path(__file__).parents[3])


def _host_description() -> dict[str, object]:
    cpu = platform.processor()
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text(encoding="utf-8").splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    memory_bytes: int | None = None
    meminfo = Path("/proc/meminfo")
    if meminfo.exists():
        for line in meminfo.read_text(encoding="utf-8").splitlines():
            if line.startswith("MemTotal:"):
                memory_bytes = int(line.split()[1]) * 1024
                break
    workspace = Path(__file__).parents[3]
    filesystem = "unknown"
    device = "unknown"
    mounts = Path("/proc/mounts")
    if mounts.exists():
        matches: list[tuple[int, str, str]] = []
        for line in mounts.read_text(encoding="utf-8").splitlines():
            fields = line.split()
            if len(fields) < 3:
                continue
            mountpoint = Path(fields[1].replace("\\040", " "))
            if workspace.is_relative_to(mountpoint):
                matches.append((len(mountpoint.parts), fields[0], fields[2]))
        if matches:
            _, device, filesystem = max(matches)
    return {
        "hostname": platform.node(),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cpu": cpu,
        "logical_cpu_count": os.cpu_count(),
        "memory_bytes": memory_bytes,
        "workspace": str(workspace),
        "filesystem": filesystem,
        "device": device,
        "storage_description": "workspace-host temporary directories",
    }


def _gate(
    gate_id: str, requirement: str, measured: object, passed: bool
) -> dict[str, object]:
    return {
        "id": gate_id,
        "requirement": requirement,
        "measured": measured,
        "passed": passed,
    }


def _evaluate_gates(report: dict[str, object]) -> list[dict[str, object]]:
    target_runs = report["target_scan_runs"]
    assert isinstance(target_runs, list)
    full = (
        next(
            run
            for run in target_runs
            if isinstance(run, dict) and run["file_count"] == 115_000
        )
        if any(
            isinstance(run, dict) and run["file_count"] == 115_000
            for run in target_runs
        )
        else max(
            (run for run in target_runs if isinstance(run, dict)),
            key=lambda run: int(run["file_count"]),
        )
    )
    full_scale_completed = int(full["file_count"]) == 115_000
    target_gates = [
        bool(value)
        for run in target_runs
        if isinstance(run, dict)
        for value in run["gates"].values()
    ]
    evidence = report["evidence_storage"]
    protection = report["evidence_protection"]
    box = report["box_set_change"]
    flat = report["adversarial_flat_grouping"]
    backlog = report["identification_backlog"]
    slow = report["latency_injected_filesystem"]
    unavailable = report["unavailable_root"]
    controls = report["scan_control_latency"]
    sse = report["sse_protocol"]
    fingerprint = report["fingerprint_playback"]
    assert all(isinstance(item, dict) for item in (evidence, protection, box, flat))
    peak_wal = max(int(run["peak_wal_bytes"]) for run in target_runs)
    peak_rss = max(int(run["rss_above_idle_bytes"]) for run in target_runs)
    event_loop = max(float(run["event_loop_delay_p99_seconds"]) for run in target_runs)
    lock_p95 = max(float(run["write_lock_wait_p95_seconds"]) for run in target_runs)
    lock_max = max(float(run["write_lock_wait_max_seconds"]) for run in target_runs)
    playback_p95 = float(full["loaded_playback_start_p95_seconds"])
    idle_playback_p95 = float(full["idle_playback_start_p95_seconds"])
    auth_me_p95 = max(float(run["auth_me_p95_seconds"]) for run in target_runs)
    return [
        _gate(
            "scan-115k-complete",
            "115,000-file first, unchanged, and small-change target runs complete",
            {
                "file_count": full["file_count"],
                "first_seconds": full["first_local_index"]["elapsed_seconds"],
                "unchanged_seconds": full["unchanged"]["elapsed_seconds"],
                "small_change_seconds": full["small_change"]["elapsed_seconds"],
            },
            full_scale_completed
            and all(
                full[name]["state"] == "completed"
                for name in ("first_local_index", "unchanged", "small_change")
            ),
        ),
        _gate(
            "one-walk-and-delta",
            "one walk; unchanged zero tag reads; one-file change one tag read",
            {
                "walks": [
                    full[name]["walks"]
                    for name in ("first_local_index", "unchanged", "small_change")
                ],
                "unchanged_tag_reads": full["unchanged"]["tag_reads"],
                "small_change_tag_reads": full["small_change"]["tag_reads"],
            },
            full["unchanged"]["tag_reads"] == 0
            and full["small_change"]["tag_reads"] == 1
            and all(
                full[name]["walks"] == 1
                for name in ("first_local_index", "unchanged", "small_change")
            ),
        ),
        _gate(
            "scan-transaction-ratios",
            "first index <0.03 and unchanged <0.02 SQLite write transactions per file",
            {
                "first": full["first_local_index"]["write_transactions"]
                / full["file_count"],
                "unchanged": full["unchanged"]["write_transactions"]
                / full["file_count"],
            },
            full["first_local_index"]["write_transactions"] / full["file_count"]
            < 0.03
            and full["unchanged"]["write_transactions"] / full["file_count"]
            < 0.02,
        ),
        _gate(
            "migration-immediate-scan",
            "the migrated database is scanned in place with zero unchanged tag reads",
            report["migration_handoff_scan"],
            bool(report["migration_handoff_scan"]["passed"]),
        ),
        _gate(
            "scan-call-elimination",
            "filesystem scans make zero fingerprints and external calls",
            {
                name: {
                    "fingerprints": full[name]["fingerprints"],
                    "external_calls": full[name]["external_calls"],
                }
                for name in ("first_local_index", "unchanged", "small_change")
            },
            all(target_gates),
        ),
        _gate("peak-wal", "<= 512 MiB", peak_wal, peak_wal <= WAL_LIMIT_BYTES),
        _gate(
            "scan-rss-above-idle",
            "<= 512 MiB",
            peak_rss,
            peak_rss <= SCAN_RSS_LIMIT_BYTES,
        ),
        _gate(
            "event-loop-p99",
            "<= 100 ms",
            event_loop,
            event_loop <= EVENT_LOOP_P99_LIMIT_SECONDS,
        ),
        _gate(
            "write-lock-p95",
            "<= 250 ms",
            lock_p95,
            lock_p95 <= WRITE_LOCK_P95_LIMIT_SECONDS,
        ),
        _gate(
            "write-lock-max",
            "<= 2 s",
            lock_max,
            lock_max <= WRITE_LOCK_MAX_SECONDS,
        ),
        _gate(
            "active-scan-playback-p95",
            "authenticated native HTTP range playback <= idle + 500 ms and <= 2 s",
            {
                "idle": idle_playback_p95,
                "loaded": playback_p95,
                "delta": max(0.0, playback_p95 - idle_playback_p95),
                "http": full["playback_http_evidence"],
            },
            playback_p95 <= idle_playback_p95 + 0.5
            and playback_p95 <= PLAYBACK_MAX_SECONDS
            and full["playback_http_evidence"]["authentication_rejections"] == 1
            and full["playback_http_evidence"]["status_codes"] == [206]
            and int(full["playback_http_evidence"]["bytes_received"]) > 0,
        ),
        _gate(
            "active-scan-auth-me-p95",
            "production token validation and user loading <= 500 ms",
            {
                "p95_seconds": auth_me_p95,
                "samples": full["auth_me_samples"],
                "backend": full["playback_http_evidence"][
                    "authentication_backend"
                ],
            },
            auth_me_p95 <= 0.5 and int(full["auth_me_samples"]) > 0,
        ),
        _gate(
            "evidence-storage",
            "<= 2 GiB at 50,000 subjects and 10 candidates",
            evidence["database_and_indexes_bytes"],
            bool(evidence["size_gate_passed"]),
        ),
        _gate(
            "evidence-compaction",
            "<= 4 KiB and protected references survive",
            {
                "bytes": protection["compacted_subject_bytes"],
                "identity": protection["protected_identity_survived"],
                "review": protection["protected_active_review_survived"],
                "manual": protection["protected_manual_decision_survived"],
            },
            bool(protection["passed"]),
        ),
        _gate(
            "box-set-delta",
            "one changed file in a 100-track box reads one tag without duplicating queued album work",
            {
                "tag_reads": box["small_change"]["tag_reads"],
                "identification_enqueued": box["small_change"]["counters"][
                    "identification_enqueued_count"
                ],
            },
            box["small_change"]["tag_reads"] == 1
            and box["small_change"]["counters"]["identification_enqueued_count"] == 0,
        ),
        _gate(
            "flat-directory",
            "flat-directory grouping is durable and keeps background transactions bounded",
            {
                "files": flat["file_count"],
                "seconds": flat["elapsed_seconds"],
                "peak_python_bytes": flat["peak_python_bytes"],
                "background_transaction_p95_seconds": flat[
                    "background_transaction_p95_seconds"
                ],
                "background_transaction_max_seconds": flat[
                    "background_transaction_max_seconds"
                ],
            },
            bool(flat["passed"]),
        ),
        _gate(
            "identification-backlog",
            "administrator/new work starts ahead of 50,000 backlog without starvation",
            {
                "admin_claim_seconds": backlog["administrator_claim_seconds"],
                "automatic_claim_p95_seconds": backlog["automatic_claim_p95_seconds"],
                "claim_order": backlog["claim_order"],
            },
            bool(backlog["passed"]),
        ),
        _gate(
            "network-and-unavailable-roots",
            "latency-injected scans stay bounded; unavailable roots reconcile no missing rows",
            {
                "slow_elapsed_seconds": slow["first_local_index"]["elapsed_seconds"],
                "unavailable_terminal": unavailable["terminal_code"],
            },
            all(slow["gates"].values()) and bool(unavailable["passed"]),
        ),
        _gate(
            "sse-rate-heartbeat",
            "counter updates <= one per 2 s plus transitions and 30 s heartbeat",
            {
                "scan_counter_events": full["first_local_index"]["sse_counter_events"],
                "heartbeat_seconds": sse["heartbeat_interval_seconds"],
            },
            bool(full["gates"]["sse_rate"]) and bool(sse["passed"]),
        ),
        _gate(
            "pause-stop-latency",
            "recorded and normally within a few seconds",
            controls,
            bool(controls["passed"]) and bool(controls["normally_within_few_seconds"]),
        ),
        _gate(
            "fingerprint-playback",
            "real fpcalc contention keeps authenticated HTTP playback within its latency gate",
            fingerprint,
            bool(fingerprint["passed"]),
        ),
        _gate(
            "catalog-before-backlog",
            "local catalog rows are readable while identification remains queued",
            {
                "albums": full["local_album_count"],
                "queued_identification": full["queued_identification_count"],
            },
            bool(full["catalog_readable_before_identification_completion"]),
        ),
    ]


def _write_markdown(report: dict[str, object], output: Path) -> None:
    gates = report["gates"]
    target_runs = report["target_scan_runs"]
    lines = [
        "# Feedback Fixes Final Pre-Maintenance Benchmark",
        "",
        f"Captured: {report['captured_date']}",
        f"Source revision: `{report['source_identity']['application_revision']}`",
        f"Matcher: `{report['matcher_version']}`",
        "",
        "## Gate results",
        "",
        "| Gate | Requirement | Result |",
        "|---|---|---:|",
    ]
    for gate in gates:
        lines.append(
            f"| {gate['id']} | {gate['requirement']} | "
            f"{'PASS' if gate['passed'] else 'FAIL'} |"
        )
    lines.extend(
        [
            "",
            "## Target scan summaries",
            "",
            "| Files | First index | Unchanged | Small change | RSS above idle | WAL |",
            "|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for run in target_runs:
        lines.append(
            f"| {run['file_count']:,} | {run['first_local_index']['elapsed_seconds']:.3f} s | "
            f"{run['unchanged']['elapsed_seconds']:.3f} s | "
            f"{run['small_change']['elapsed_seconds']:.3f} s | "
            f"{run['rss_above_idle_bytes']:,} B | {run['peak_wal_bytes']:,} B |"
        )
    lines.extend(
        [
            "",
            "The JSON artifact contains raw phase timings, throughput, SQL and transaction "
            "counts, event traces, latency distributions, evidence sizing, queue claims, "
            "control timings, and the complete host/dataset description.",
            "",
        ]
    )
    output.write_text("\n".join(lines), encoding="utf-8")


async def _run_phase10(
    generated_sizes: list[int],
    target_sizes: list[int],
    flat_grouping_size: int = 10_000,
) -> dict[str, object]:
    source_identity = _source_identity()
    report: dict[str, object] = {
        "schema": "feedback-fixes-benchmark-v3",
        "captured_date": date.today().isoformat(),
        "source_commit": source_identity["commit"],
        "source_identity": source_identity,
        "matcher_version": MATCHER_VERSION,
        "schema_contract": "native_library_schema.py additive target schema",
        "host": _host_description(),
        "dataset": {
            "generated_sizes": generated_sizes,
            "target_sizes": target_sizes,
            "format_mix": "generated FLAC placeholders plus committed independent FLAC fixture",
            "cache_state": "fresh generated trees and cold target databases",
            "storage": "temporary directories on the workspace filesystem",
            "event_loop_sampling_seconds": 0.01,
            "active_playback_sampling_seconds": 0.5,
            "network_fixture": "simulated 1 ms per FLAC stat/tag-info stat",
        },
        "generated_inventory_runs": [
            benchmark_inventory(size) for size in generated_sizes
        ],
        "target_scan_runs": [
            await benchmark_target_scan(size) for size in target_sizes
        ],
        "migration_handoff_scan": await benchmark_migration_handoff_scan(),
        "box_set_change": await benchmark_target_scan(100, layout="box_set"),
        "adversarial_flat_grouping": await benchmark_staged_flat_grouping(
            flat_grouping_size
        ),
        "latency_injected_filesystem": await benchmark_target_scan(
            1_000, stat_delay_seconds=0.001
        ),
        "unavailable_root": await benchmark_unavailable_root(),
        "identification_backlog": await benchmark_identification_backlog(),
        "scan_control_latency": await benchmark_scan_control_latency(),
        "evidence_shape": {
            "subjects": EVIDENCE_SUBJECTS,
            "candidates_per_subject": EVIDENCE_CANDIDATES_PER_SUBJECT,
            "candidate_rows": EVIDENCE_SUBJECTS * EVIDENCE_CANDIDATES_PER_SUBJECT,
        },
        "evidence_storage": benchmark_evidence_storage(),
        "evidence_protection": await benchmark_evidence_protection(),
        "sse_protocol": await benchmark_sse_protocol(),
        "fingerprint_playback": await benchmark_fingerprint_playback(
            allow_isolated_container=True
        ),
        "isolated_idle_latency": await benchmark_latency(),
        "isolated_cpu_loaded_latency": await benchmark_latency(loaded=True),
    }
    report["gates"] = _evaluate_gates(report)
    report["all_gates_passed"] = all(bool(gate["passed"]) for gate in report["gates"])
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sizes", nargs="+", type=int, required=True)
    parser.add_argument(
        "--target-sizes", nargs="+", type=int, default=[10_000, 115_000]
    )
    parser.add_argument("--flat-grouping-size", type=int, default=10_000)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--markdown-output", type=Path)
    args = parser.parse_args()
    report = asyncio.run(
        _run_phase10(args.sizes, args.target_sizes, args.flat_grouping_size)
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    markdown = args.markdown_output or (
        args.output.parent
        / f"{report['captured_date']}-feedback-fixes-final-benchmark.md"
    )
    _write_markdown(report, markdown)
    print(
        json.dumps(
            {
                "json": str(args.output),
                "markdown": str(markdown),
                "all_gates_passed": report["all_gates_passed"],
            },
            sort_keys=True,
        )
    )
    if not report["all_gates_passed"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
