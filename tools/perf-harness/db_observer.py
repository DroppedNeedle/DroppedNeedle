#!/usr/bin/env python3
"""DB/WAL observer for a local instance cache dir (stdlib only, read-only).

NEVER checkpoints, truncates, or writes the live database. PRAGMA
introspection uses SQLite URI mode=ro; the optional TRUNCATE probe operates
on a temp COPY of the database files, never the live ones.

Usage:
  db_observer.py snapshot <cache_dir> [--out report.json]
  db_observer.py watch <cache_dir> --duration 120 [--interval 0.2]
  db_observer.py copy-probe <cache_dir>   # cold-open a copy, report live frames
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402


def db_paths(cache_dir: str) -> dict:
    db = str(Path(cache_dir) / "library.db")
    return {"db": db, "wal": db + "-wal", "shm": db + "-shm"}


def snapshot_sizes(cache_dir: str) -> dict:
    paths = db_paths(cache_dir)
    return {name: common.file_size(path) for name, path in paths.items()}


def readonly_pragmas(cache_dir: str) -> dict:
    """Read-only PRAGMAs over the live DB (mode=ro connection, no writes)."""
    db = db_paths(cache_dir)["db"]
    if common.file_size(db) is None:
        return {"error": "library.db absent"}
    out: dict = {}
    try:
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
    except sqlite3.Error as exc:
        return {"error": f"open failed: {exc}"}
    try:
        for pragma in (
            "page_size",
            "page_count",
            "freelist_count",
            "journal_mode",
            "synchronous",
            "wal_autocheckpoint",
            "busy_timeout",
            "mmap_size",
        ):
            try:
                out[pragma] = conn.execute(f"PRAGMA {pragma}").fetchone()[0]
            except sqlite3.Error as exc:
                out[pragma] = f"error: {exc}"
        try:
            out["table_count"] = conn.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'"
            ).fetchone()[0]
        except sqlite3.Error as exc:
            out["table_count"] = f"error: {exc}"
    finally:
        conn.close()
    return out


def copy_truncate_probe(cache_dir: str) -> dict:
    """Cold-open a COPY and TRUNCATE-checkpoint it to estimate dead WAL space.

    Stage-0 caveat carries: the copy-open recovery path makes this
    non-definitive about live frames, but a 0-live-frames result strongly
    suggests the retained WAL is dead preallocated space.
    """
    paths = db_paths(cache_dir)
    if common.file_size(paths["db"]) is None:
        return {"error": "library.db absent"}
    with tempfile.TemporaryDirectory(prefix="walprobe_") as tmp:
        for name, src in paths.items():
            if common.file_size(src) is not None:
                shutil.copy2(src, str(Path(tmp) / Path(src).name))
        copy_db = str(Path(tmp) / "library.db")
        conn = sqlite3.connect(copy_db, timeout=30)
        try:
            row = conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
            result = {"checkpoint_busy": row[0], "live_frames": row[1], "checkpointed": row[2]}
        finally:
            conn.close()
        result["wal_after_bytes"] = common.file_size(copy_db + "-wal")
        result["wal_before_bytes"] = common.file_size(paths["wal"])
        return result


def cmd_snapshot(args: argparse.Namespace) -> int:
    common.assert_safe_target(paths=[args.cache_dir])
    payload = {
        "meta": common.run_metadata(),
        "cache_dir": os.path.realpath(args.cache_dir),
        "sizes": snapshot_sizes(args.cache_dir),
        "pragmas": readonly_pragmas(args.cache_dir),
    }
    if args.out:
        common.write_json(args.out, payload)
    print(json.dumps(payload, indent=2))
    return 0


def cmd_watch(args: argparse.Namespace) -> int:
    common.assert_safe_target(paths=[args.cache_dir])
    samples = []
    deadline = time.perf_counter() + args.duration
    start = time.perf_counter()
    while time.perf_counter() < deadline:
        samples.append({"t_s": round(time.perf_counter() - start, 3), **snapshot_sizes(args.cache_dir)})
        time.sleep(args.interval)
    payload = {"meta": common.run_metadata(), "samples": samples}
    if args.out:
        common.write_json(args.out, payload)
    print(json.dumps(payload, indent=2))
    return 0


def cmd_copy_probe(args: argparse.Namespace) -> int:
    common.assert_safe_target(paths=[args.cache_dir])
    payload = {"meta": common.run_metadata(), "probe": copy_truncate_probe(args.cache_dir)}
    if args.out:
        common.write_json(args.out, payload)
    print(json.dumps(payload, indent=2))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Observe DB/WAL of a local cache dir.")
    sub = parser.add_subparsers(dest="cmd", required=True)

    snap = sub.add_parser("snapshot", help="One-shot sizes + read-only PRAGMAs")
    snap.add_argument("cache_dir")
    snap.add_argument("--out", default=None)
    snap.set_defaults(func=cmd_snapshot)

    watch = sub.add_parser("watch", help="Size timeline while other work runs")
    watch.add_argument("cache_dir")
    watch.add_argument("--duration", type=float, default=120.0)
    watch.add_argument("--interval", type=float, default=0.2)
    watch.add_argument("--out", default=None)
    watch.set_defaults(func=cmd_watch)

    probe = sub.add_parser("copy-probe", help="TRUNCATE-checkpoint a temp copy (never live)")
    probe.add_argument("cache_dir")
    probe.add_argument("--out", default=None)
    probe.set_defaults(func=cmd_copy_probe)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
