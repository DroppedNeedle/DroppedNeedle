#!/usr/bin/env python3
"""End-to-end workload orchestrator for Stage-1 budgets (stdlib only).

Full sequence against a LOCAL throwaway instance on a free port:
  boot (cold) -> setup admin -> set library root (local_metadata) ->
  initial scan (auto-triggered, observed) -> identification drain ->
  DB snapshot -> no-op incremental scan -> rescan_files scan ->
  API latency bench -> post-workload RSS + DB snapshot -> clean shutdown.

Writes a single JSON report to <workdir>/report.json and prints a short
budget summary to stdout.

Usage:
  run_workload.py --corpus <music-dir> --workdir <run-dir> [--seed 1]
      [--policy local_metadata] [--skip-rescan] [--skip-api]
      [--search-hit Perf] [--api-requests 50]

Safety: refuses prod ports/paths; binds 127.0.0.1 only; never touches the
working tree or prod volumes. Rerunnable: each run uses a fresh app dir.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import sys
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import api_latency  # noqa: E402
import boot  # noqa: E402
import common  # noqa: E402
import db_observer  # noqa: E402
import rss as rss_mod  # noqa: E402
import scan as scan_mod  # noqa: E402


def setup_admin(base: str) -> tuple[str, dict]:
    """Create the first admin on a fresh instance. Returns (token, user)."""
    password = secrets.token_urlsafe(24)  # throwaway; breach-check fails open
    _, body = common.api_request(
        base,
        None,
        "POST",
        "/api/v1/auth/setup",
        {"display_name": "PerfHarness", "username": "perfharness", "password": password},
    )
    return body["token"], body.get("user", {})


def set_library_root(base: str, token: str, corpus: str, policy: str) -> dict:
    """Replace all library roots with a single root at <corpus>."""
    _, settings = common.api_request(base, token, "GET", "/api/v1/settings/library")
    new_settings = {
        "library_roots": [
            {
                "id": str(uuid.uuid4()),
                "path": os.path.realpath(corpus),
                "label": "perf-corpus",
                "policy": policy,
                "rules": [],
            }
        ],
        "staging_path": settings.get("staging_path", ""),
        "naming_template": settings.get("naming_template", ""),
        "acoustid_api_key": settings.get("acoustid_api_key", ""),
        "enabled": settings.get("enabled", True),
    }
    _, updated = common.api_request(
        base,
        token,
        "PUT",
        "/api/v1/settings/library",
        {"settings": new_settings, "expected_policy_revision": settings["policy_revision"]},
    )
    return updated


def history_run_ids(base: str, token: str) -> set[str]:
    """IDs of all known scan runs (boot-time failures included)."""
    _, history = common.api_request(base, token, "GET", "/api/v1/library/scan-runs")
    runs = history.get("runs") or history.get("items") or []
    return {str(r.get("id") or r.get("run_id")) for r in runs if r.get("id") or r.get("run_id")}


def wait_for_new_run(base: str, token: str, known_ids: set[str], timeout_s: float = 60.0) -> str | None:
    """Wait for the supervisor auto-trigger (trigger=policy_apply) to appear.

    Returns the new run id, or None if no auto-trigger appeared (caller
    falls back to a manual incremental scan and records that).
    """
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        _, current = common.api_request(base, token, "GET", "/api/v1/library/scan-runs/current")
        active = current.get("active") or current.get("queued")
        if active and str(active["id"]) not in known_ids:
            return str(active["id"])
        fresh = history_run_ids(base, token) - known_ids
        if fresh:
            return sorted(fresh)[0]
        time.sleep(0.5)
    return None


def scan_run_detail(base: str, token: str, run_id: str) -> dict:
    _, detail = common.api_request(base, token, "GET", f"/api/v1/library/scan-runs/{run_id}")
    return detail.get("snapshot", {})


def main() -> int:
    parser = argparse.ArgumentParser(description="Run the full perf workload.")
    parser.add_argument("--corpus", required=True, help="Music root to scan")
    parser.add_argument("--workdir", required=True, help="Run dir (created; holds app/ + report)")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--policy", default="local_metadata", choices=["local_metadata", "automatic"])
    parser.add_argument("--port", type=int, default=None, help="Default: free port")
    parser.add_argument("--skip-rescan", action="store_true")
    parser.add_argument("--skip-api", action="store_true")
    parser.add_argument("--search-hit", default="Perf")
    parser.add_argument("--api-requests", type=int, default=50)
    parser.add_argument("--api-warmup", type=int, default=3)
    parser.add_argument("--api-interval", type=float, default=0.045)
    parser.add_argument("--idle-timeout", type=float, default=3600.0)
    args = parser.parse_args()

    common.assert_safe_target(port=args.port, paths=[args.corpus, args.workdir])
    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    appdir = str(workdir / "app")
    cache_dir = os.path.join(appdir, "cache")
    port = args.port if args.port is not None else common.pick_free_port()
    base = f"http://127.0.0.1:{port}"

    report: dict = {
        "meta": common.run_metadata(
            {"seed": args.seed, "corpus": os.path.realpath(args.corpus), "port": port, "policy": args.policy}
        ),
        "steps": {},
    }
    proc = None
    try:
        # 1. Cold boot (kept running for the workload).
        boot_result, proc = boot.measure_boot(
            appdir, port, str(workdir / "boot.log"), keep=True
        )
        if not boot_result.get("ready"):
            raise RuntimeError("instance never became ready (see boot.log)")
        report["steps"]["boot_cold"] = boot_result
        report["steps"]["rss_boot_idle"] = rss_mod.sample_once(proc.pid, "boot_idle")

        # 2. Setup + roots.
        token, _user = setup_admin(base)
        report["steps"]["db_post_setup"] = db_observer.snapshot_sizes(cache_dir)
        known_runs = history_run_ids(base, token)
        updated = set_library_root(base, token, args.corpus, args.policy)
        report["steps"]["roots"] = {
            "policy_revision": updated.get("policy_revision"),
            "reconciliation_required": updated.get("reconciliation_required"),
        }

        # 3. Initial scan: wait for the supervisor auto-trigger to APPEAR
        # (it fires async after policy apply), then drain it. Falls back to
        # a manual incremental if no auto-trigger appears within 60 s.
        auto_id = wait_for_new_run(base, token, known_runs, timeout_s=60.0)
        if auto_id is None:
            initial_result = scan_mod.run_scan(
                base, token, cache_dir, "initial-manual-fallback", kind="incremental"
            )
            initial_result["auto_trigger"] = False
            report["steps"]["scan_initial"] = initial_result
        else:
            t0 = time.perf_counter()
            scan_mod.wait_for_scan_idle(base, token, timeout_s=args.idle_timeout)
            initial_wall = round(time.perf_counter() - t0, 3)
            initial = scan_run_detail(base, token, auto_id)
            run = initial.get("run", {})
            started_at, terminal_at = run.get("started_at"), run.get("terminal_at")
            server_elapsed = (
                round(terminal_at - started_at, 3)
                if isinstance(started_at, (int, float)) and isinstance(terminal_at, (int, float))
                else None
            )
            counters = initial.get("counters", {})
            inspected = counters.get("inspected_count") or counters.get("discovered_count") or 0
            report["steps"]["scan_initial"] = {
                "auto_trigger": True,
                "run_id": auto_id,
                "client_wall_s": initial_wall,
                "server_elapsed_s": server_elapsed,
                "files_per_sec_server": round(inspected / server_elapsed, 2) if server_elapsed else None,
                "run": run,
                "counters": counters,
            }
        report["steps"]["db_post_initial"] = db_observer.snapshot_sizes(cache_dir)

        # 4. Identification drain (local_metadata still enqueues local jobs).
        t0 = time.perf_counter()
        try:
            idle_state = scan_mod.wait_for_library_idle(base, token, timeout_s=args.idle_timeout)
            drain_s: float | None = round(time.perf_counter() - t0, 3)
        except TimeoutError as exc:
            drain_s, idle_state = None, {"warning": str(exc)}
        report["steps"]["ident_drain"] = {"drain_s": drain_s, "activity": idle_state}
        report["steps"]["db_post_drain"] = db_observer.snapshot_sizes(cache_dir)

        # 5. No-op incremental scan.
        noop = scan_mod.run_scan(base, token, cache_dir, "noop-incremental", kind="incremental")
        report["steps"]["scan_noop"] = noop

        # 6. Full re-index scan (optional).
        if not args.skip_rescan:
            rescan = scan_mod.run_scan(base, token, cache_dir, "rescan-files", kind="rescan_files")
            report["steps"]["scan_rescan"] = rescan

        # 7. API latency bench (optional).
        if not args.skip_api:
            bench = api_latency.run_bench(
                base,
                token,
                requests=args.api_requests,
                warmup=args.api_warmup,
                interval_s=args.api_interval,
                search_hit=args.search_hit,
            )
            report["steps"]["api"] = bench

        # 8. Post-workload RSS + DB.
        assert proc is not None
        report["steps"]["rss_post_workload"] = rss_mod.sample_once(proc.pid, "post_workload")
        report["steps"]["db_final"] = {
            "sizes": db_observer.snapshot_sizes(cache_dir),
            "pragmas": db_observer.readonly_pragmas(cache_dir),
        }
    finally:
        if proc is not None and proc.poll() is None:
            report["steps"]["shutdown"] = boot.shutdown_proc(proc)
        common.write_json(workdir / "report.json", report)

    # Budget summary to stdout.
    summary = {
        "report": str(workdir / "report.json"),
        "boot_ready_s": report["steps"].get("boot_cold", {}).get("t_ready_s"),
        "shutdown_s": report["steps"].get("shutdown", {}).get("t_shutdown_s"),
        "rss_boot_kb": report["steps"].get("rss_boot_idle", {}).get("tree_total_rss_kb"),
        "rss_post_kb": report["steps"].get("rss_post_workload", {}).get("tree_total_rss_kb"),
        "initial_files_per_s": report["steps"].get("scan_initial", {}).get("files_per_sec_server"),
        "noop_files_per_s": report["steps"].get("scan_noop", {}).get("files_per_sec_server"),
        "rescan_files_per_s": report["steps"].get("scan_rescan", {}).get("files_per_sec_server"),
        "db_final": report["steps"].get("db_final", {}).get("sizes"),
    }
    api = report["steps"].get("api")
    if api:
        summary["api_p95_ms"] = {name: ep.get("p95_ms") for name, ep in api.items()}
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
