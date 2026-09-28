#!/usr/bin/env python3
"""RSS sampler for a local instance process tree (stdlib only, read-only).

Samples VmRSS/VmHWM/Threads from /proc for a root pid plus its children and
grandchildren (launcher supervisor + uvicorn worker layout).

Usage:
  rss.py --pid <pid> [--label idle|post-workload] [--out report.json]
  rss.py --pid <pid> --watch --interval 2 --duration 60  # timeline to stdout
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402


def sample_once(pid: int, label: str = "") -> dict:
    tree = common.sample_tree(pid)
    return {
        "label": label or "sample",
        "pid": pid,
        "t": round(time.time(), 3),
        **tree,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Sample RSS of a process tree.")
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--label", default="sample")
    parser.add_argument("--out", default=None)
    parser.add_argument("--watch", action="store_true")
    parser.add_argument("--interval", type=float, default=2.0)
    parser.add_argument("--duration", type=float, default=60.0)
    args = parser.parse_args()

    if args.watch:
        samples = []
        deadline = time.perf_counter() + args.duration
        while time.perf_counter() < deadline:
            samples.append(sample_once(args.pid, args.label))
            time.sleep(args.interval)
        payload = {"meta": common.run_metadata(), "samples": samples}
    else:
        payload = {"meta": common.run_metadata(), **sample_once(args.pid, args.label)}
    if args.out:
        common.write_json(args.out, payload)
    print(json.dumps(payload, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
