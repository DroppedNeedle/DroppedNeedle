#!/usr/bin/env python3
"""Scan-rate runner against a local instance (stdlib only).

Triggers a scan run (incremental | rescan_files), polls
GET /api/v1/library/scan-runs/{id} to a terminal state, and reports
server-side phase timings (terminal_at - started_at, phases sum exactly)
plus DB/WAL size timelines sampled during the run.

If a supervisor auto-triggered run is already active, it is OBSERVED instead
of double-triggering (disposition "observed_auto_run").

Idle gate: callers should drain identification work first — poll
GET /api/v1/library/activity until quiet (see wait_for_library_idle).

Usage:
  scan.py --base http://127.0.0.1:PORT --token TOKEN --cache-dir <appdir>/cache \
      --tag incremental-900 [--kind incremental] [--out report.json]
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.parse
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402
import db_observer  # noqa: E402

TERMINAL_STATES = {"completed", "cancelled", "superseded_policy_changed", "failed"}


def wait_for_library_idle(base: str, token: str, timeout_s: float = 1800.0, interval_s: float = 2.0) -> dict:
    """Poll /library/activity until no work remains. Returns last body.

    Real shape is LibraryActivityResponse {items, work_items, revisions}:
    idle means both lists are empty. Two consecutive quiet polls are
    required so post-scan identification work that enqueues async is not
    missed by a single lucky sample.
    """
    deadline = time.perf_counter() + timeout_s
    last: dict = {}
    quiet_polls = 0
    while time.perf_counter() < deadline:
        _, last = common.api_request(base, token, "GET", "/api/v1/library/activity")
        if not last.get("items") and not last.get("work_items"):
            quiet_polls += 1
            if quiet_polls >= 2:
                return last
        else:
            quiet_polls = 0
        time.sleep(interval_s)
    raise TimeoutError(f"library never idle within {timeout_s}s: {last}")


def wait_for_scan_idle(base: str, token: str, timeout_s: float = 1800.0, interval_s: float = 0.5) -> None:
    """Wait until /scan-runs/current reports no active run."""
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        _, current = common.api_request(base, token, "GET", "/api/v1/library/scan-runs/current")
        if not (current.get("active") or current.get("queued")):
            return
        time.sleep(interval_s)
    raise TimeoutError("scan runs never drained")


def run_scan(
    base: str,
    token: str,
    cache_dir: str,
    tag: str,
    kind: str = "incremental",
    poll_interval_s: float = 0.2,
    timeout_s: float = 3600.0,
) -> dict:
    common.assert_safe_target(paths=[cache_dir])
    parts = urllib.parse.urlparse(base)
    if (parts.hostname or "") != "127.0.0.1":
        raise SystemExit("scan only targets local 127.0.0.1 instances")
    common.assert_safe_target(port=parts.port or 80)  # bare host still dials port 80
    pre_sizes = db_observer.snapshot_sizes(cache_dir)

    _, settings = common.api_request(base, token, "GET", "/api/v1/settings/library")
    _, current = common.api_request(base, token, "GET", "/api/v1/library/scan-runs/current")
    active = current.get("active") or current.get("queued")

    t_request = time.perf_counter()
    if active:
        run_id = active["id"]
        disposition = "observed_auto_run"
        auto_triggered = True
    else:
        _, created = common.api_request(
            base,
            token,
            "POST",
            "/api/v1/library/scan-runs",
            {"kind": kind, "scope_ids": [], "expected_policy_revision": settings["policy_revision"]},
        )
        run_id = created["run_id"]
        disposition = created.get("disposition", "")
        auto_triggered = False

    timeline: list[dict] = []
    final: dict | None = None
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        _, detail = common.api_request(base, token, "GET", f"/api/v1/library/scan-runs/{run_id}")
        snap = detail["snapshot"]
        run = snap["run"]
        timeline.append(
            {
                "t_s": round(time.perf_counter() - t_request, 3),
                "state": run["state"],
                "phase": run.get("phase"),
                **db_observer.snapshot_sizes(cache_dir),
                "counters": dict(snap.get("counters", {})),
            }
        )
        if run["state"] in TERMINAL_STATES:
            final = snap
            break
        time.sleep(poll_interval_s)
    client_elapsed = time.perf_counter() - t_request
    if final is None:
        return {"tag": tag, "run_id": run_id, "error": "timeout", "timeline_tail": timeline[-5:]}

    run = final["run"]
    counters = final.get("counters", {})
    inspected = counters.get("inspected_count") or counters.get("discovered_count") or 0
    started_at = run.get("started_at")
    terminal_at = run.get("terminal_at")
    server_elapsed = (
        round(terminal_at - started_at, 3)
        if isinstance(started_at, (int, float)) and isinstance(terminal_at, (int, float))
        else None
    )
    return {
        "tag": tag,
        "requested_kind": kind,
        "run_id": run_id,
        "auto_triggered": auto_triggered,
        "disposition": disposition,
        "final_state": run["state"],
        "terminal_code": run.get("terminal_code"),
        "phase_timings": run.get("phase_timings", {}),
        "counters": counters,
        "server_started_at": started_at,
        "server_terminal_at": terminal_at,
        "client_elapsed_s": round(client_elapsed, 3),
        "server_elapsed_s": server_elapsed,
        "files_per_sec_client": round(inspected / client_elapsed, 2) if client_elapsed > 0 else None,
        "files_per_sec_server": round(inspected / server_elapsed, 2) if server_elapsed else None,
        "pre_sizes": pre_sizes,
        "post_sizes": db_observer.snapshot_sizes(cache_dir),
        "wal_peak_bytes": max((s.get("wal") or 0) for s in timeline) if timeline else 0,
        "samples": len(timeline),
        "timeline": timeline,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Run a timed library scan.")
    parser.add_argument("--base", required=True, help="e.g. http://127.0.0.1:PORT")
    parser.add_argument("--token", required=True, help="Bearer [REDACTED] for setup admin")
    parser.add_argument("--cache-dir", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--kind", default="incremental", choices=["incremental", "rescan_files"])
    parser.add_argument("--out", default=None)
    parser.add_argument("--poll-interval", type=float, default=0.2)
    parser.add_argument("--timeout", type=float, default=3600.0)
    args = parser.parse_args()

    result = run_scan(
        args.base, args.token, args.cache_dir, args.tag,
        kind=args.kind, poll_interval_s=args.poll_interval, timeout_s=args.timeout,
    )
    result["meta"] = common.run_metadata({"kind": args.kind})
    if args.out:
        common.write_json(args.out, result)
    else:
        # Keep stdout parseable: full timeline only goes to --out files.
        short = dict(result)
        short.pop("timeline", None)
        print(json.dumps(short, indent=2))
        return 0 if result.get("final_state") == "completed" else 2
    short = dict(result)
    short.pop("timeline", None)
    print(json.dumps(short, indent=2))
    return 0 if result.get("final_state") == "completed" else 2


if __name__ == "__main__":
    sys.exit(main())
