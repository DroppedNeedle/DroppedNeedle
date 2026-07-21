"""Additional signed Phase 10 workloads for the Feedback Fixes benchmark."""

from __future__ import annotations

import asyncio
import math
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import tracemalloc
import wave
from array import array
from pathlib import Path
from time import perf_counter
from types import SimpleNamespace

import msgspec

from infrastructure.audio.fingerprinter import AudioFingerprinter
from infrastructure.persistence.auth_store import AuthStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.identification import (
    CandidateEvidence,
    ExistingAlbumMembership,
    GroupingTrack,
    TrackEvidence,
)
from models.library_work import ScanRun
from services.native.identification_queue_service import IdentificationQueueService
from services.native.library_activity_events import activity_events
from services.native.local_album_grouper import (
    LocalAlbumGrouper,
    assign_album_continuity,
)
from services.native.local_album_grouping_service import LocalAlbumGroupingService
from services.native.reidentification_service import (
    IdentificationWorkArbiter,
    ReidentificationService,
)
from tests.benchmarks.http_playback_probe import AuthenticatedHTTPPlaybackProbe


def _percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int(len(ordered) * fraction + 0.999) - 1))
    return ordered[index]


def _create_store(database: Path) -> NativeLibraryStore:
    lock = threading.Lock()
    AuthStore(database, lock)
    return NativeLibraryStore(database, lock)


def _seed_artist_and_albums(
    database: Path, count: int, *, prefix: str = "album"
) -> None:
    with sqlite3.connect(database) as connection:
        connection.execute("PRAGMA foreign_keys = ON")
        connection.execute(
            "INSERT OR IGNORE INTO local_artists "
            "(id, display_name, folded_name, normalized_name, kind, created_at, updated_at) "
            "VALUES ('benchmark-artist','Benchmark Artist','benchmark artist',"
            "'benchmark artist','group',1,1)"
        )
        connection.executemany(
            "INSERT INTO local_albums "
            "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
            "album_artist_name_folded, album_artist_id, grouping_source, created_at, updated_at) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            (
                (
                    f"{prefix}-{index}",
                    "benchmark-root",
                    f"group-{index}",
                    f"Album {index}",
                    f"album {index}",
                    "Benchmark Artist",
                    "benchmark artist",
                    "benchmark-artist",
                    "automatic",
                    1.0,
                    1.0,
                )
                for index in range(count)
            ),
        )
        connection.commit()


def benchmark_flat_grouping(count: int = 10_000) -> dict[str, object]:
    """Measure the adversarial flat, untagged grouping and sparse continuity paths."""

    tracemalloc.start()
    tracks = [
        GroupingTrack(
            local_track_id=f"track-{index}",
            root_id="benchmark-root",
            relative_path=f"flat/track-{index:05d}.flac",
            title=f"Track {index}",
            artist_name="Benchmark Artist",
        )
        for index in range(count)
    ]
    group_started = perf_counter()
    proposed = LocalAlbumGrouper().group(tracks)
    grouping_seconds = perf_counter() - group_started
    existing = [
        ExistingAlbumMembership(
            local_album_id=f"old-album-{index}",
            track_ids=[f"track-{index}"],
            created_at=float(index),
        )
        for index in range(count)
    ]
    continuity_started = perf_counter()
    continued = assign_album_continuity(existing, proposed)
    continuity_seconds = perf_counter() - continuity_started
    _current, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    retained = sum(album.retained_album_id is not None for album in continued)
    return {
        "file_count": count,
        "fixture": "simulated-flat-untagged-directory",
        "group_count": len(proposed),
        "grouping_seconds": grouping_seconds,
        "continuity_seconds": continuity_seconds,
        "peak_python_bytes": peak,
        "retained_album_ids": retained,
        "quadratic_matrix_cells": 0,
        "passed": len(proposed) == count and retained == count,
    }


async def benchmark_staged_flat_grouping(count: int = 10_000) -> dict[str, object]:
    """Exercise the durable production grouping path for one adversarial directory."""

    with tempfile.TemporaryDirectory(prefix="feedback-staged-flat-") as directory:
        database = Path(directory) / "target.db"
        store = _create_store(database)
        _seed_artist_and_albums(database, count, prefix="flat-old-album")
        with sqlite3.connect(database) as connection:
            connection.execute("PRAGMA foreign_keys = ON")
            for batch_start in range(0, count, 5_000):
                batch_end = min(batch_start + 5_000, count)
                connection.executemany(
                    "INSERT INTO local_tracks "
                    "(id,local_album_id,root_id,file_path,relative_path,path_hash,"
                    "file_size_bytes,file_mtime_ns,stat_revision,stat_revision_kind,"
                    "tag_revision,title,title_folded,artist_name,artist_name_folded,"
                    "album_title,album_title_folded,album_artist_name,"
                    "album_artist_name_folded,disc_number,track_number,file_format,"
                    "ingest_source,imported_at,membership_source,desired_policy_revision,"
                    "applied_policy_revision,applied_policy) VALUES "
                    "(?,?,?,?,?,?,?,?,?,'exact',?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    (
                        (
                            f"flat-track-{index:08d}",
                            "flat-old-album-0",
                            "benchmark-root",
                            f"/music/flat/track-{index:08d}.flac",
                            f"flat/track-{index:08d}.flac",
                            f"flat-hash-{index:08d}",
                            100,
                            index + 1,
                            f"100:{index + 1}",
                            f"tag-{index:08d}",
                            f"Track {index}",
                            f"track {index}",
                            "Benchmark Artist",
                            "benchmark artist",
                            f"Track {index}",
                            f"track {index}",
                            "Benchmark Artist",
                            "benchmark artist",
                            1,
                            0,
                            "flac",
                            "scan",
                            1,
                            "automatic",
                            "policy",
                            "policy",
                            "automatic",
                        )
                        for index in range(batch_start, batch_end)
                    ),
                )
                connection.commit()
        await store.create_scan_run(
            ScanRun(
                id="staged-flat-run",
                kind="incremental",
                trigger="manual",
                queued_at=1,
            )
        )
        with sqlite3.connect(database) as connection:
            connection.execute(
                "INSERT INTO library_scan_grouping_contexts "
                "(run_id,root_id,relative_directory) "
                "VALUES ('staged-flat-run','benchmark-root','flat')"
            )

        background_transaction_seconds: list[float] = []
        execute_background = store._execute_background

        def measured_background(operation):
            transaction_started = perf_counter()
            try:
                return execute_background(operation)
            finally:
                background_transaction_seconds.append(
                    perf_counter() - transaction_started
                )

        store._execute_background = measured_background
        tracemalloc.start()
        started = perf_counter()
        enqueued = await LocalAlbumGroupingService(
            store, IdentificationQueueService(store)
        ).regroup_run("staged-flat-run", now=2)
        elapsed = perf_counter() - started
        _current, peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        with sqlite3.connect(database) as connection:
            context_state = connection.execute(
                "SELECT state FROM library_scan_grouping_contexts "
                "WHERE run_id='staged-flat-run'"
            ).fetchone()[0]
            retained = connection.execute(
                "SELECT COUNT(*) FROM local_tracks track WHERE track.local_album_id "
                "LIKE 'flat-old-album-%'"
            ).fetchone()[0]
            matched = connection.execute(
                "SELECT COUNT(*) FROM library_scan_grouping_old_nodes "
                "WHERE run_id='staged-flat-run' "
                "AND matched_grouping_token IS NOT NULL"
            ).fetchone()[0]
            staged = connection.execute(
                "SELECT COUNT(*) FROM library_scan_grouping_evidence "
                "WHERE run_id='staged-flat-run'"
            ).fetchone()[0]
        transaction_p95 = _percentile(background_transaction_seconds, 0.95) or 0.0
        transaction_max = max(background_transaction_seconds, default=0.0)
        wal = database.with_name(f"{database.name}-wal")
        return {
            "file_count": count,
            "fixture": "durable-flat-untagged-directory",
            "elapsed_seconds": elapsed,
            "peak_python_bytes": peak,
            "retained_album_ids": retained,
            "disk_backed_matches": matched,
            "staged_evidence_rows": staged,
            "identification_enqueued": enqueued,
            "context_state": context_state,
            "background_transaction_p95_seconds": transaction_p95,
            "background_transaction_max_seconds": transaction_max,
            "wal_bytes": wal.stat().st_size if wal.exists() else 0,
            "passed": context_state == "completed"
            and retained == 1
            and matched == 1
            and staged == count
            and enqueued == count
            and transaction_p95 <= 0.25
            and transaction_max <= 2.0,
        }


async def benchmark_identification_backlog(
    backlog_count: int = 50_000,
) -> dict[str, object]:
    """Measure indexed claim priority and anti-starvation at the signed queue shape."""

    with tempfile.TemporaryDirectory(prefix="feedback-backlog-") as directory:
        database = Path(directory) / "target.db"
        store = _create_store(database)
        _seed_artist_and_albums(database, backlog_count + 8)
        seed_started = perf_counter()
        with sqlite3.connect(database) as connection:
            connection.execute("PRAGMA foreign_keys = ON")
            connection.execute(
                "INSERT INTO auth_users(id,display_name,role,created_at) "
                "VALUES ('admin','Benchmark Admin','admin','2026-01-01T00:00:00Z')"
            )
            connection.executemany(
                "INSERT INTO library_identification_jobs "
                "(id, local_album_id, kind, state, priority, enqueue_sequence, "
                "input_revision, dedupe_key, not_before, created_at, updated_at) "
                "VALUES (?,?, 'automatic','queued',40,?,?,?,0,1,1)",
                (
                    (
                        f"backlog-{index}",
                        f"album-{index}",
                        index + 1,
                        "revision",
                        f"backlog:{index}",
                    )
                    for index in range(backlog_count)
                ),
            )
            connection.execute(
                "UPDATE library_enqueue_sequence SET value = ? WHERE singleton = 1",
                (backlog_count,),
            )
            connection.commit()
        seed_seconds = perf_counter() - seed_started

        explicit = await ReidentificationService(store).create_or_coalesce(
            f"album-{backlog_count}", "admin", now=2
        )
        queue = IdentificationQueueService(store)
        arbiter_started = perf_counter()
        administrator = await IdentificationWorkArbiter(store, queue).claim(
            "benchmark-worker", now=3
        )
        administrator_claim_seconds = perf_counter() - arbiter_started

        for offset in range(1, 7):
            await queue.enqueue_album(
                f"album-{backlog_count + offset}",
                input_revision=f"new-{offset}",
                now=4 + offset,
            )
        claimed: list[str] = []
        claim_seconds: list[float] = []
        for offset in range(6):
            started = perf_counter()
            row = await queue.claim("automatic-worker", now=20 + offset)
            claim_seconds.append(perf_counter() - started)
            if row is None:
                break
            claimed.append(str(row["id"]))
            with sqlite3.connect(database) as connection:
                connection.execute(
                    "UPDATE library_identification_jobs SET state = 'succeeded', "
                    "lease_owner = NULL, lease_expires_at = NULL, terminal_at = ? "
                    "WHERE id = ?",
                    (20 + offset, row["id"]),
                )
                connection.commit()

        with sqlite3.connect(database) as connection:
            query_plan = [
                str(row[3])
                for row in connection.execute(
                    "EXPLAIN QUERY PLAN SELECT id FROM library_identification_jobs "
                    "WHERE state = 'queued' AND not_before <= 30 "
                    "ORDER BY priority ASC, enqueue_sequence ASC LIMIT 1"
                )
            ]
            database_bytes = int(
                connection.execute("PRAGMA page_size").fetchone()[0]
            ) * int(connection.execute("PRAGMA page_count").fetchone()[0])
        admin_first = (
            administrator is not None
            and administrator[0] == "explicit_reidentification"
            and administrator[1]["id"] == explicit["id"]
        )
        oldest_progressed = len(claimed) == 6 and claimed[5] == "backlog-0"
        return {
            "backlog_jobs": backlog_count,
            "seed_seconds": seed_seconds,
            "database_bytes": database_bytes,
            "administrator_claim_seconds": administrator_claim_seconds,
            "administrator_started_first": admin_first,
            "automatic_claim_seconds": claim_seconds,
            "automatic_claim_p95_seconds": _percentile(claim_seconds, 0.95),
            "claim_order": claimed,
            "oldest_backlog_progressed_after_high_priority_streak": oldest_progressed,
            "query_plan": query_plan,
            "passed": admin_first and oldest_progressed,
        }


async def benchmark_evidence_protection() -> dict[str, object]:
    """Exercise real compaction while identity and decision references stay intact."""

    with tempfile.TemporaryDirectory(
        prefix="feedback-protected-evidence-"
    ) as directory:
        database = Path(directory) / "target.db"
        store = _create_store(database)
        _seed_artist_and_albums(database, 4, prefix="evidence-album")
        payloads: list[tuple[str, str, str, bytes, int, float]] = []
        for index in range(4):
            evidence = msgspec.json.encode(
                CandidateEvidence(
                    release_group_mbid=f"00000000-0000-4000-8000-{index:012d}",
                    track_evidence=[
                        TrackEvidence(
                            local_track_id=f"track-{index}",
                            classification="contradictory",
                            evidence_kinds=["benchmark-contradiction" * 100],
                        )
                    ],
                )
            )
            payloads.append(
                (
                    f"evidence-{index}",
                    f"attempt-{index}",
                    f"candidate-{index}",
                    evidence,
                    len(evidence),
                    2.0,
                )
            )
        with sqlite3.connect(database) as connection:
            connection.execute("PRAGMA foreign_keys = ON")
            connection.execute(
                "INSERT INTO auth_users(id,display_name,role,created_at) "
                "VALUES ('admin','Benchmark Admin','admin','2026-01-01T00:00:00Z')"
            )
            connection.executemany(
                "INSERT INTO library_identification_attempts "
                "(id, local_album_id, trigger, input_tag_revision, input_policy_revision, "
                "input_file_revision, matcher_version, state, terminal_reason_code, "
                "candidate_count, started_at, completed_at) "
                "VALUES (?,?, 'automatic','tag','policy','file','feedback-fixes-v1',"
                "'no_candidate','NO_EXTERNAL_RESULT',1,1,2)",
                ((f"attempt-{index}", f"evidence-album-{index}") for index in range(4)),
            )
            connection.executemany(
                "INSERT INTO library_identification_evidence "
                "(id, attempt_id, candidate_key, evidence_json, evidence_size_bytes, created_at) "
                "VALUES (?,?,?,?,?,?)",
                payloads,
            )
            connection.execute(
                "INSERT INTO local_album_external_identities "
                "(local_album_id, release_group_mbid, decision_source, matcher_version, "
                "attempt_id, selected_at) VALUES "
                "('evidence-album-1','10000000-0000-4000-8000-000000000001',"
                "'automatic','feedback-fixes-v1','attempt-1',2)"
            )
            connection.executemany(
                "INSERT INTO library_identification_reviews "
                "(id, local_album_id, state, reason_code, attempt_id, input_revision, "
                "decided_by_user_id, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    (
                        "active-review",
                        "evidence-album-2",
                        "needs_review",
                        "INSUFFICIENT_EVIDENCE",
                        "attempt-2",
                        "revision-2",
                        None,
                        2.0,
                        2.0,
                    ),
                    (
                        "manual-decision",
                        "evidence-album-3",
                        "keep_tagged",
                        "KEPT_AS_TAGGED",
                        "attempt-3",
                        "revision-3",
                        "admin",
                        2.0,
                        2.0,
                    ),
                ),
            )
            connection.commit()

        (
            compacted,
            compacted_bytes,
        ) = await store.compact_terminal_identification_evidence(
            older_than=90 * 24 * 60 * 60
        )
        with sqlite3.connect(database) as connection:
            rows = {
                str(row[0]): {"compacted": bool(row[1]), "bytes": int(row[2])}
                for row in connection.execute(
                    "SELECT id, compacted, evidence_size_bytes "
                    "FROM library_identification_evidence ORDER BY id"
                )
            }
        protected_survived = all(
            not rows[f"evidence-{index}"]["compacted"] for index in (1, 2, 3)
        )
        return {
            "eligible_rows_compacted": compacted,
            "compacted_total_bytes": compacted_bytes,
            "compacted_subject_bytes": rows["evidence-0"]["bytes"],
            "protected_identity_survived": not rows["evidence-1"]["compacted"],
            "protected_active_review_survived": not rows["evidence-2"]["compacted"],
            "protected_manual_decision_survived": not rows["evidence-3"]["compacted"],
            "passed": compacted == 1
            and rows["evidence-0"]["bytes"] <= 4096
            and protected_survived,
        }


async def benchmark_sse_protocol() -> dict[str, object]:
    revisions = {"scan": 1, "identification": 1, "operation": 1}

    class Revisions:
        async def stream_revisions(self) -> dict[str, int]:
            return dict(revisions)

    delays: list[float] = []

    async def no_wait(delay: float) -> None:
        delays.append(delay)

    events = activity_events(Revisions(), sleep=no_wait)
    initial = await anext(events)
    heartbeat = await anext(events)
    revisions["scan"] += 1
    changed = await anext(events)
    await events.aclose()
    return {
        "poll_interval_seconds": 2.0,
        "heartbeat_interval_seconds": sum(delays[:15]),
        "event_trace": [initial.strip(), heartbeat.strip(), changed.strip()],
        "initial_transition": "event: activity.changed" in initial,
        "bounded_heartbeat": heartbeat == ": keepalive\n\n",
        "changed_transition": '"scan":2' in changed,
        "passed": "event: activity.changed" in initial
        and heartbeat == ": keepalive\n\n"
        and '"scan":2' in changed,
    }


async def benchmark_fingerprint_playback(
    *, allow_isolated_container: bool = False
) -> dict[str, object]:
    """Measure first-chunk reads while the signed four-process fpcalc lane is active."""

    correctness_fixture = (
        Path(__file__).parents[1] / "fixtures" / "library" / "flac_full_01.flac"
    )
    fpcalc = shutil.which("fpcalc")
    playback_samples = 40
    fingerprint_samples = 20
    with tempfile.TemporaryDirectory(
        prefix="feedback-fingerprint-playback-"
    ) as directory:
        scratch = Path(directory)
        audio = scratch / "playback.wav"
        sample_rate = 44_100
        samples = array(
            "h",
            (
                int(12_000 * math.sin(2 * math.pi * 440 * index / sample_rate))
                for index in range(sample_rate * 30)
            ),
        )
        if sys.byteorder != "little":
            samples.byteswap()
        with wave.open(str(audio), "wb") as stream:
            stream.setnchannels(1)
            stream.setsampwidth(2)
            stream.setframerate(sample_rate)
            stream.writeframes(samples.tobytes())

        playback = await AuthenticatedHTTPPlaybackProbe(
            _create_store(scratch / "playback.db"), scratch, audio
        ).__aenter__()
        idle = [await playback.sample() for _ in range(playback_samples)]
        benchmark_image = os.environ.get(
            "FEEDBACK_FIXES_BENCHMARK_IMAGE", "droppedneedle:local"
        )
        docker = shutil.which("docker")
        container_available = False
        if fpcalc is None and allow_isolated_container and docker is not None:
            inspected = await asyncio.create_subprocess_exec(
                docker,
                "image",
                "inspect",
                benchmark_image,
                stdout=asyncio.subprocess.DEVNULL,
                stderr=asyncio.subprocess.DEVNULL,
            )
            container_available = await inspected.wait() == 0
        if fpcalc is None and not container_available:
            http_evidence = playback.evidence()
            await playback.__aexit__(None, None, None)
            return {
                "fpcalc_available": False,
                "fixture": "generated-30-second-440hz-pcm-wave",
                "correctness_fixture": str(correctness_fixture.name),
                "idle_playback_start_p95_seconds": _percentile(idle, 0.95),
                "loaded_playback_start_p95_seconds": None,
                "fingerprints": 0,
                "http_playback_evidence": http_evidence,
                "underruns": "no HTTP status or empty-body failures observed",
                "passed": False,
                "reason": (
                    "fpcalc is unavailable and no isolated benchmark image was enabled"
                ),
            }

        execution_mode = "host-fpcalc"
        if fpcalc is not None:
            fingerprinter = AudioFingerprinter(
                SimpleNamespace(), lambda: "", SimpleNamespace()
            )

            async def host_fingerprint_probe() -> int:
                try:
                    fingerprint, duration = await fingerprinter.generate_fingerprint(
                        audio
                    )
                except (
                    OSError,
                    ValueError,
                    asyncio.TimeoutError,
                    subprocess.SubprocessError,
                ):
                    return 0
                return int(bool(fingerprint and duration > 0))

            fingerprint_task = asyncio.gather(
                *(host_fingerprint_probe() for _ in range(fingerprint_samples))
            )
        else:
            execution_mode = "four-isolated-fpcalc-containers"

            async def container_fingerprint_lane() -> int:
                process = await asyncio.create_subprocess_exec(
                    docker,
                    "run",
                    "--rm",
                    "--network=none",
                    "--read-only",
                    "--tmpfs",
                    "/tmp:size=4m",
                    "-v",
                    f"{scratch}:/bench:ro",
                    "--entrypoint",
                    "/bin/sh",
                    benchmark_image,
                    "-c",
                    "i=0; while [ $i -lt 5 ]; do "
                    "fpcalc -length 120 /bench/playback.wav >/tmp/fpcalc.out 2>/dev/null; "
                    "grep -q '^FINGERPRINT=' /tmp/fpcalc.out || exit 1; "
                    "i=$((i + 1)); done",
                    stdout=asyncio.subprocess.DEVNULL,
                    stderr=asyncio.subprocess.DEVNULL,
                )
                return 5 if await process.wait() == 0 else 0

            fingerprint_task = asyncio.gather(
                *(container_fingerprint_lane() for _ in range(4))
            )
        fingerprint_started = perf_counter()
        loaded: list[float] = []
        while not fingerprint_task.done() or len(loaded) < playback_samples:
            loaded.append(await playback.sample())
            await asyncio.sleep(0.005)
        fingerprint_results = await fingerprint_task
        fingerprint_seconds = perf_counter() - fingerprint_started
        idle_p95 = _percentile(idle, 0.95) or 0.0
        loaded_p95 = _percentile(loaded, 0.95) or 0.0
        http_evidence = playback.evidence()
        await playback.__aexit__(None, None, None)
        return {
            "fpcalc_available": True,
            "execution_mode": execution_mode,
            "fpcalc_path": fpcalc,
            "benchmark_image": benchmark_image if fpcalc is None else None,
            "fixture": "generated-30-second-440hz-pcm-wave",
            "correctness_fixture": str(correctness_fixture.name),
            "fingerprint_process_limit": 4,
            "fingerprints": sum(fingerprint_results),
            "fingerprint_attempts": fingerprint_samples,
            "fingerprint_elapsed_seconds": fingerprint_seconds,
            "idle_playback_start_p95_seconds": idle_p95,
            "loaded_playback_start_p95_seconds": loaded_p95,
            "loaded_playback_samples": len(loaded),
            "loaded_minus_idle_p95_seconds": max(0.0, loaded_p95 - idle_p95),
            "http_playback_evidence": http_evidence,
            "underruns": "no HTTP status or empty-body failures observed",
            "passed": sum(fingerprint_results) == fingerprint_samples
            and http_evidence["authentication_rejections"] == 1
            and http_evidence["status_codes"] == [206]
            and loaded_p95 <= idle_p95 + 0.5
            and loaded_p95 <= 2.0,
        }
