from pathlib import Path

import pytest

from infrastructure.persistence.maintenance_manifest import capture_source_identity
from tests.benchmarks.feedback_fixes_benchmark import (
    EVIDENCE_CANDIDATES_PER_SUBJECT,
    EVIDENCE_SUBJECTS,
    benchmark_evidence_storage,
    benchmark_inventory,
    benchmark_latency,
    benchmark_migration_handoff_scan,
    benchmark_scan_control_latency,
    benchmark_target_scan,
    benchmark_unavailable_root,
)
from tests.benchmarks.feedback_fixes_root_mapping import rehearse
from tests.benchmarks.feedback_fixes_maintenance_rehearsal import (
    run as run_maintenance_rehearsal,
)
from tests.benchmarks.feedback_fixes_phase10_scenarios import (
    benchmark_evidence_protection,
    benchmark_fingerprint_playback,
    benchmark_flat_grouping,
    benchmark_staged_flat_grouping,
    benchmark_identification_backlog,
    benchmark_sse_protocol,
)


def test_generated_inventory_manifest_and_measurements() -> None:
    report = benchmark_inventory(120)

    assert report["file_count"] == 120
    assert sum(report["shape"].values()) == 120
    assert set(report["shape"].values()) == {20}
    assert report["tag_reads"] == 0
    assert report["fingerprints"] == 0
    assert report["external_calls"] == 0
    assert report["metrics"]["counters"]["stats"] == 120
    assert report["metrics"]["counters"]["walks"] == 1
    assert report["metrics"]["counters"]["sql_statements"] > 0


def test_feedback_fixes_evidence_shape_is_pinned() -> None:
    assert EVIDENCE_SUBJECTS == 50_000
    assert EVIDENCE_CANDIDATES_PER_SUBJECT == 10
    assert EVIDENCE_SUBJECTS * EVIDENCE_CANDIDATES_PER_SUBJECT == 500_000


@pytest.mark.asyncio
async def test_target_scan_benchmark_exercises_incremental_stack() -> None:
    report = await benchmark_target_scan(120)

    assert report["file_count"] == 120
    assert report["first_local_index"]["state"] == "completed"
    assert report["first_local_index"]["tag_reads"] == 100
    assert report["unchanged"]["tag_reads"] == 0
    assert report["small_change"]["tag_reads"] == 1
    assert report["small_change"]["counters"]["identification_enqueued_count"] == 0
    assert all(report["gates"].values())


@pytest.mark.asyncio
async def test_migration_handoff_scan_reuses_the_migrated_database() -> None:
    report = await benchmark_migration_handoff_scan()

    assert report["legacy_revisions_before_scan"] == report["migrated_tracks"]
    assert report["exact_revisions_after_scan"] == report["migrated_tracks"]
    assert report["tag_reads"] == 0
    assert report["passed"] is True


@pytest.mark.asyncio
async def test_box_set_network_unavailable_and_control_benchmarks() -> None:
    box = await benchmark_target_scan(100, layout="box_set")
    slow = await benchmark_target_scan(60, stat_delay_seconds=0.0005)
    unavailable = await benchmark_unavailable_root()
    controls = await benchmark_scan_control_latency()

    assert box["small_change"]["tag_reads"] == 1
    assert box["small_change"]["counters"]["identification_enqueued_count"] == 0
    assert all(slow["gates"].values())
    assert unavailable["passed"] is True
    assert controls["passed"] is True


def test_flat_grouping_benchmark_uses_sparse_continuity() -> None:
    report = benchmark_flat_grouping(1_000)

    assert report["group_count"] == 1_000
    assert report["quadratic_matrix_cells"] == 0
    assert report["passed"] is True


@pytest.mark.asyncio
async def test_staged_flat_grouping_benchmark_uses_durable_rows(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 50
    )
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.CONTINUITY_COMPONENT_EDGE_LIMIT",
        50,
    )
    report = await benchmark_staged_flat_grouping(100)

    assert report["staged_evidence_rows"] == 100
    assert report["retained_album_ids"] == 1
    assert report["disk_backed_matches"] == 1
    assert report["passed"] is True


@pytest.mark.asyncio
async def test_backlog_evidence_sse_and_fingerprint_benchmark_contracts() -> None:
    backlog = await benchmark_identification_backlog(1_000)
    evidence = await benchmark_evidence_protection()
    sse = await benchmark_sse_protocol()
    fingerprint = await benchmark_fingerprint_playback()

    assert backlog["administrator_started_first"] is True
    assert backlog["oldest_backlog_progressed_after_high_priority_streak"] is True
    assert evidence["passed"] is True
    assert sse["passed"] is True
    assert fingerprint["fpcalc_available"] in {True, False}
    assert fingerprint["idle_playback_start_p95_seconds"] is not None


def test_evidence_storage_benchmark_materializes_payload_and_indexes() -> None:
    report = benchmark_evidence_storage(100, 10)

    assert report["candidate_rows"] == 1_000
    assert report["index_count"] >= 2
    assert report["database_and_indexes_bytes"] > report["payload_bytes"]
    assert report["size_gate_passed"] is True
    assert report["compaction_gate_passed"] is True


@pytest.mark.asyncio
async def test_feedback_fixes_latency_report_covers_numeric_gates() -> None:
    report = await benchmark_latency(loaded=True)

    assert report["counters"]["playback_starts"] == 20
    assert report["counters"]["sse_events"] == 0
    assert report["distributions"]["event_loop_delay_seconds"]["count"] > 0
    assert report["distributions"]["write_lock_wait_seconds"]["count"] == 20
    assert report["distributions"]["playback_start_seconds"]["count"] == 20


@pytest.mark.asyncio
async def test_feedback_fixes_root_mapping_rehearsal() -> None:
    report = await rehearse()

    assert report["root_ids_stable"] is True
    assert report["migration_idempotent"] is True
    assert report["source_database_unchanged_by_dry_run"] is True
    assert report["report"]["source_count"] == 100
    assert report["report"]["mapped_count"] == 100
    assert report["report"]["blocking"] is False


@pytest.mark.asyncio
async def test_complete_manifest_migration_startup_and_rollback_rehearsal(
    tmp_path,
) -> None:
    repository_root = Path(__file__).resolve().parents[3]
    source_identity = capture_source_identity(repository_root)
    report = await run_maintenance_rehearsal(
        tmp_path / "maintenance.json",
        source_commit=source_identity["commit"],
        file_count=100,
        managed_asset_bytes=1024 * 1024,
    )

    assert report["closed_source_writer_count"] == 0
    assert report["manifest"]["validated"] is True
    assert report["manifest"]["encryption_key_present"] is True
    assert report["manifest"]["secret_pair_valid"] is True
    assert report["manifest"]["managed_assets"] == 258
    assert report["source_identity"] == source_identity
    source_smoke = report["source_restore"]["smoke"]
    assert source_smoke["native_playback_prefix_ok"] is True
    assert source_smoke["subsonic_playback_prefix_ok"] is True
    assert source_smoke["jellyfin_playback_prefix_ok"] is True
    assert source_smoke["restored_artwork_bytes_match"] is True
    assert source_smoke["paired_secret_loaded_by_application"] is True
    assert report["migration"]["idempotent"] is True
    assert report["migration"]["network_calls"] == 0
    assert report["migration"]["tag_reads"] == 0
    assert report["migration"]["fingerprints"] == 0
    assert report["target_startup"]["validated"] is True
    assert report["target_startup"]["store_constructed_twice"] is True
    assert report["target_startup"]["failed_invariant_refused"] is True
    target_smoke = report["target_startup"]["smoke"]
    assert target_smoke["local_only_browse_consistent"] is True
    assert target_smoke["local_only_native"] is True
    assert target_smoke["local_only_subsonic"] is True
    assert target_smoke["local_only_jellyfin"] is True
    assert target_smoke["native_range_status"] == 206
    assert target_smoke["subsonic_range_status"] == 206
    assert target_smoke["jellyfin_range_status"] == 206
    assert target_smoke["cached_native_artwork_status"] == 200
    assert target_smoke["restored_artwork_bytes_match"] is True
    assert report["full_rollback"]["smoke"]["native_playback_prefix_ok"] is True
    assert len(report["process_transcript"]) == 8
    stopped = [
        event for event in report["process_transcript"] if event["event"] == "stopped"
    ]
    assert len(stopped) == 4
    assert all(event["process_exited"] for event in stopped)
    assert all(event["database_writer_lock_available"] for event in stopped)
    assert report["no_runtime_selector"] is True
    assert report["no_final_delta_or_dual_write"] is True
