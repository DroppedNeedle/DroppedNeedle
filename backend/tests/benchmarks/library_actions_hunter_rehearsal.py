"""Opt-in large-catalog rehearsal for policy and bulk-review responsiveness."""

from __future__ import annotations

import argparse
import asyncio
import json
import sqlite3
import tempfile
import time
import tracemalloc
from pathlib import Path
from time import perf_counter

from api.v1.schemas.library_operations import (
    BulkReviewPreviewRequest,
    BulkReviewSelection,
)
from infrastructure.persistence._database import PriorityWriteLock
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_work import ScanScope
from services.native.background_workload_gate import BackgroundWorkloadGate
from services.native.album_candidate_service import AlbumCandidateService
from services.native.album_evidence_engine import AlbumEvidenceEngine
from services.native.explicit_reidentification_worker import (
    ExplicitReidentificationWorker,
)
from services.native.identity_repair_service import IdentityRepairService
from services.native.library_operation_service import LibraryOperationService
from services.native.library_operation_supervisor import LibraryOperationSupervisor
from services.native.library_review_service import LibraryReviewService
from services.native.reidentification_service import ReidentificationService

FOREGROUND_WRITE_P95_LIMIT_SECONDS = 0.250
POLICY_RESPONSE_LIMIT_SECONDS = 5.0
PREVIEW_RESPONSE_LIMIT_SECONDS = 30.0
PREVIEW_MEMORY_LIMIT_BYTES = 256 * 1024 * 1024


class _ProviderProbe:
    def __init__(self) -> None:
        self.calls = 0

    async def search_album_candidate_ids(self, query, limit, priority):
        self.calls += 1
        return []

    async def search_recording_candidate_ids(self, artist, title, limit, priority):
        self.calls += 1
        return []

    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        self.calls += 1
        raise AssertionError("An empty candidate search must not load a candidate.")


def _percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int(len(ordered) * fraction + 0.999) - 1))
    return ordered[index]


def _seed(database: Path, *, tracks: int, reviews: int) -> NativeLibraryStore:
    if tracks < reviews or tracks % reviews:
        raise ValueError("Tracks must be an exact multiple of reviews.")
    with sqlite3.connect(database) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users(id) VALUES ('admin')")
    store = NativeLibraryStore(database, PriorityWriteLock())
    tracks_per_album = tracks // reviews
    batch_size = 10_000
    with sqlite3.connect(database) as connection:
        connection.execute("PRAGMA foreign_keys = ON")
        connection.execute(
            "INSERT INTO local_artists "
            "(id, display_name, folded_name, normalized_name, kind, created_at, updated_at) "
            "VALUES ('hunter-artist','Hunter Artist','hunter artist','hunter artist','group',1,1)"
        )
        for start in range(0, reviews, batch_size):
            end = min(start + batch_size, reviews)
            connection.executemany(
                "INSERT INTO local_albums "
                "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
                "album_artist_name_folded, album_artist_id, grouping_source, created_at, updated_at) "
                "VALUES (?, 'hunter-root', ?, ?, ?, 'Hunter Artist', 'hunter artist', "
                "'hunter-artist', 'automatic', 1, 1)",
                (
                    (
                        f"hunter-album-{index}",
                        f"hunter-group-{index}",
                        f"Hunter Album {index}",
                        f"hunter album {index}",
                    )
                    for index in range(start, end)
                ),
            )
            connection.executemany(
                "INSERT INTO library_identification_reviews "
                "(id, local_album_id, state, reason_code, input_revision, created_at, updated_at) "
                "VALUES (?, ?, 'needs_review', 'NO_SAFE_MATCH', ?, ?, ?)",
                (
                    (
                        f"hunter-review-{index}",
                        f"hunter-album-{index}",
                        f"hunter-input-{index}",
                        float(index),
                        float(index),
                    )
                    for index in range(start, end)
                ),
            )
            connection.commit()
        for start in range(0, tracks, batch_size):
            end = min(start + batch_size, tracks)
            connection.executemany(
                "INSERT INTO local_tracks "
                "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
                "file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, "
                "album_title, album_title_folded, album_artist_name, album_artist_name_folded, "
                "file_format, ingest_source, imported_at, membership_source, applied_policy) "
                "VALUES (?, ?, 'hunter-root', ?, ?, ?, 1, 1, ?, ?, ?, ?, ?, "
                "'Hunter Artist', 'hunter artist', 'flac', 'benchmark', 1, 'automatic', 'automatic')",
                (
                    (
                        f"hunter-track-{index}",
                        f"hunter-album-{index // tracks_per_album}",
                        f"/music/Hunter/{index // tracks_per_album}/{index}.flac",
                        f"Hunter/{index // tracks_per_album}/{index}.flac",
                        f"hunter-hash-{index}",
                        f"hunter-stat-{index}",
                        f"Track {index}",
                        f"track {index}",
                        f"Hunter Album {index // tracks_per_album}",
                        f"hunter album {index // tracks_per_album}",
                    )
                    for index in range(start, end)
                ),
            )
            connection.commit()
    return store


async def run(*, tracks: int, reviews: int) -> dict[str, object]:
    with tempfile.TemporaryDirectory(
        prefix="droppedneedle-library-actions-"
    ) as scratch:
        store = _seed(Path(scratch) / "hunter.db", tracks=tracks, reviews=reviews)
        gate = BackgroundWorkloadGate()
        gate.set_scan_active(True)

        stop_scan_writes = asyncio.Event()

        async def active_scan_writes() -> None:
            while not stop_scan_writes.is_set():
                await store._background_write(
                    lambda connection: connection.execute(
                        "UPDATE library_event_stream_revisions SET value = value "
                        "WHERE stream_kind = 'scan'"
                    )
                )
                await asyncio.sleep(0)

        scan_task = asyncio.create_task(active_scan_writes())
        foreground_latencies: list[float] = []
        for _ in range(30):
            started = perf_counter()
            await store._write(
                lambda connection: connection.execute(
                    "UPDATE library_event_stream_revisions SET value = value "
                    "WHERE stream_kind = 'operation'"
                )
            )
            foreground_latencies.append(perf_counter() - started)

        policy_started = perf_counter()
        tree_counts = await store.get_policy_scope_counts(
            [("hunter-root", "."), ("hunter-root", "Hunter/0")]
        )
        impact_counts = await store.get_policy_scope_total_counts(
            [
                ScanScope(
                    root_id="hunter-root",
                    relative_path=".",
                    policy_revision="hunter-policy",
                )
            ]
        )
        policy_seconds = perf_counter() - policy_started

        tracemalloc.start()
        preview_started = perf_counter()
        preview = await LibraryReviewService(store).preview_bulk(
            BulkReviewPreviewRequest(
                action="retry",
                selection=BulkReviewSelection(
                    normalized_filter={"state": "needs_review"},
                    catalog_revision=await store.get_catalog_revision(),
                ),
            ),
            now=time.time(),
        )
        preview_seconds = perf_counter() - preview_started
        _current_memory, peak_memory = tracemalloc.get_traced_memory()
        tracemalloc.stop()

        explicit = await ReidentificationService(store).create_or_coalesce(
            "hunter-album-0",
            "admin",
            idempotency_key="hunter-scan-gated-retry",
            now=time.time(),
        )
        provider = _ProviderProbe()
        supervisor = LibraryOperationSupervisor(
            store,
            LibraryOperationService(store),
            IdentityRepairService(store),
            ExplicitReidentificationWorker(
                store,
                AlbumCandidateService(provider),
                AlbumEvidenceEngine(),
                workload_gate=gate,
            ),
            gate,
        )
        blocked_retry = await supervisor.run_once("hunter-worker", now=time.time())
        provider_calls_while_scan_active = provider.calls

        gate.set_scan_active(False)
        completed_retry = await supervisor.run_once("hunter-worker", now=time.time())
        stop_scan_writes.set()
        await scan_task
        foreground_p95 = _percentile(foreground_latencies, 0.95)
        gates = {
            "foreground_write_p95": foreground_p95
            <= FOREGROUND_WRITE_P95_LIMIT_SECONDS,
            "policy_responsive": policy_seconds <= POLICY_RESPONSE_LIMIT_SECONDS,
            "preview_responsive": preview_seconds <= PREVIEW_RESPONSE_LIMIT_SECONDS,
            "preview_memory_bounded": peak_memory <= PREVIEW_MEMORY_LIMIT_BYTES,
            "provider_waited_for_scan": blocked_retry is None
            and provider_calls_while_scan_active == 0
            and completed_retry is not None
            and completed_retry.id == explicit["id"]
            and provider.calls > 0,
        }
        return {
            "shape": {"tracks": tracks, "reviews": reviews},
            "active_scan": True,
            "counts": {
                "tree": {str(key): value for key, value in tree_counts.items()},
                "impact": impact_counts,
                "preview_eligible": preview.eligible_count,
            },
            "seconds": {
                "foreground_write_p95": round(foreground_p95, 6),
                "policy": round(policy_seconds, 6),
                "preview": round(preview_seconds, 6),
            },
            "preview_peak_memory_bytes": peak_memory,
            "provider_calls_while_scan_active": provider_calls_while_scan_active,
            "provider_calls_after_release": provider.calls,
            "gates": gates,
            "passed": all(gates.values()),
        }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tracks", type=int, default=1_000_000)
    parser.add_argument("--reviews", type=int, default=100_000)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    report = asyncio.run(run(tracks=args.tracks, reviews=args.reviews))
    encoded = json.dumps(report, indent=2, sort_keys=True)
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded + "\n")
    print(encoded)
    if not report["passed"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
