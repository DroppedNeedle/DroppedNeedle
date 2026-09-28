#!/usr/bin/env python3
"""Boot/shutdown timing via the sanctioned launcher path (stdlib only).

Spawns: backend/.venv/bin/python -m maintenance.automatic_upgrade
--start-target with ROOT_APP_DIR=<appdir> PORT=<port> BIND_HOST=127.0.0.1.

Ready = GET /health returns 200 with body {"status": "ok"} (during upgrade
the launcher serves {"status": "upgrading"} on the same path).

Direct `uvicorn target_main:app` on a fresh ROOT_APP_DIR is an UNSUPPORTED
boot (fail-closes without the migration marker) — this script only uses the
launcher, matching Stage 0.

Usage:
  boot.py --appdir <dir> [--port <p>|--free-port] [--keep] [--out report.json]

Safety: refuses prod ports/paths (see common.assert_safe_target).
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402


def measure_boot(
    appdir: str,
    port: int,
    logpath: str,
    keep: bool = False,
    ready_timeout_s: float = 600.0,
) -> tuple[dict, subprocess.Popen | None]:
    """Boot the instance, wait for ready, sample RSS. Returns (result, proc).

    When keep=True the process is left running and returned; otherwise it is
    shut down cleanly with SIGTERM and shutdown timing is recorded.
    """
    common.assert_safe_target(port=port, paths=[appdir, logpath])
    os.makedirs(appdir, exist_ok=True)

    env = dict(os.environ)
    env["ROOT_APP_DIR"] = os.path.realpath(appdir)
    env["PORT"] = str(port)
    env["BIND_HOST"] = "127.0.0.1"
    cmd = [common.venv_python(), "-m", "maintenance.automatic_upgrade", "--start-target"]
    base = f"http://127.0.0.1:{port}"

    started = time.perf_counter()
    with open(logpath, "w") as logf:
        proc = subprocess.Popen(
            cmd, cwd=str(common.BACKEND_DIR), env=env, stdout=logf, stderr=subprocess.STDOUT
        )
        try:
            ready_s = common.wait_for_health(base, timeout_s=ready_timeout_s)
        except TimeoutError:
            result = {
                "ready": False,
                "t_ready_s": None,
                "launcher_returncode": proc.poll(),
                "log": logpath,
            }
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=45)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=10)
            return result, None

        # Settle delay (Stage-0 convention) before the RSS sample.
        time.sleep(2.0)
        tree = common.sample_tree(proc.pid)
        result = {
            "ready": True,
            "cmd": cmd,
            "cwd": str(common.BACKEND_DIR),
            "env": {"ROOT_APP_DIR": env["ROOT_APP_DIR"], "PORT": str(port), "BIND_HOST": "127.0.0.1"},
            "t_ready_s": round(ready_s, 3),
            "t_rss_sample_s": round(time.perf_counter() - started, 3),
            "rss": tree,
            "log": logpath,
            "launcher_pid": proc.pid,
        }
        if keep:
            return result, proc

        stop_started = time.perf_counter()
        proc.terminate()
        try:
            proc.wait(timeout=45)
            result["shutdown"] = "sigterm_clean"
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)
            result["shutdown"] = "killed_after_45s_sigterm"
        result["t_shutdown_s"] = round(time.perf_counter() - stop_started, 3)
        result["launcher_returncode"] = proc.returncode
        return result, None


def shutdown_proc(proc: subprocess.Popen, timeout_s: float = 45.0) -> dict:
    """Gracefully stop a kept process. Returns shutdown timing."""
    if proc.poll() is not None:
        return {"shutdown": "already_exited", "t_shutdown_s": 0.0, "returncode": proc.returncode}
    started = time.perf_counter()
    proc.terminate()
    try:
        proc.wait(timeout=timeout_s)
        outcome = "sigterm_clean"
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=10)
        outcome = "killed_after_sigterm_timeout"
    return {
        "shutdown": outcome,
        "t_shutdown_s": round(time.perf_counter() - started, 3),
        "returncode": proc.returncode,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Measure boot/shutdown timing.")
    parser.add_argument("--appdir", required=True, help="ROOT_APP_DIR for this run")
    parser.add_argument("--port", type=int, default=None, help="Port (refuses prod ports)")
    parser.add_argument("--log", default=None, help="Server log path (default <appdir>/boot.log)")
    parser.add_argument("--keep", action="store_true", help="Leave the instance running")
    parser.add_argument("--out", default=None, help="Write JSON report to this path")
    parser.add_argument("--ready-timeout", type=float, default=600.0)
    args = parser.parse_args()

    port = args.port if args.port is not None else common.pick_free_port()
    logpath = args.log or os.path.join(args.appdir, "boot.log")
    result, proc = measure_boot(args.appdir, port, logpath, keep=args.keep, ready_timeout_s=args.ready_timeout)
    result["meta"] = common.run_metadata({"port": port})
    if proc is not None:
        result["note"] = f"instance kept running as pid {proc.pid}; stop it yourself"
    if args.out:
        common.write_json(args.out, result)
    print(json.dumps(result, indent=2))
    return 0 if result.get("ready") else 1


if __name__ == "__main__":
    sys.exit(main())
