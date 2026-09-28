#!/usr/bin/env python3
"""Hatch-3 2x-peak replay harness (stdlib only).

Replays a mixed load against a LOCAL instance: paced non-stream API reads
plus concurrent ranged stream starts. Evaluates the stage0-misc hatch-3
trip conditions:

  3a. p99 non-stream API latency >500 ms with CPU <70% (queuing, not compute)
  3b. range-stream start TTFB p99 >300 ms
  3c. any OOM/stall requiring restart (5xx / connection failures / dead at end)

"2x peak" is defined at use time: pass --api-rate/--streams as double the
measured peak and record the basis in --peak-basis (embedded in the report).
Stage-1 note: v2 has no measured prod peak, so v2 verification runs use modest
rates under the 30/s limiter ceiling and record that as the basis. The same
script replays true 2x-peak against v3 once access-log peaks exist.

Stream targets: pass --stream-id values (local file ids) or let the script
auto-pick the first N via local albums -> album tracks.

Usage:
  replay.py --base http://127.0.0.1:PORT --token TOKEN --duration 60 \
      --api-rate 20 --streams 4 --pid <launcher-pid> [--out report.json]

Safety: localhost only, refuses prod ports/paths. Counts HTTP 429s
separately: on v2 a nonzero 429 count means the replay exceeded the
limiter and the API numbers measure the queue, not the handlers.
"""

from __future__ import annotations

import argparse
import http.client
import json
import sys
import threading
import time
import urllib.parse
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402

# Hatch-3 trip thresholds (stage0-misc section 4).
TRIP_API_P99_MS = 500.0
TRIP_API_CPU_PCT = 70.0
TRIP_STREAM_TTFB_P99_MS = 300.0

API_MIX = [
    "/api/v1/library/albums?page=1&page_size=50",
    "/api/v1/library/artists?limit=50",
    "/api/v1/library/tracks?limit=48",
    "/api/v1/library/stats",
    "/api/v1/local/albums?limit=50",
]


def read_cpu_times() -> tuple[int, int]:
    """(total, idle) jiffies from /proc/stat."""
    with open("/proc/stat") as handle:
        parts = handle.readline().split()
    values = [int(x) for x in parts[1:]]
    return sum(values), values[3] + values[4]


def system_cpu_pct(stop: threading.Event, interval_s: float, out: list[float]) -> None:
    """Sample system CPU usage until stop is set."""
    prev_total, prev_idle = read_cpu_times()
    while not stop.wait(interval_s):
        total, idle = read_cpu_times()
        delta_total = total - prev_total
        out.append(
            round(100 * (1 - (idle - prev_idle) / delta_total), 1) if delta_total > 0 else 0.0
        )
        prev_total, prev_idle = total, idle


def _proc_jiffies(pid: int) -> int | None:
    """utime+stime jiffies for one pid (None if unreadable)."""
    try:
        with open(f"/proc/{pid}/stat") as handle:
            fields = handle.read().split()
        # Field 2 (comm) can contain spaces inside parens; anchor on the last ")".
        raw = " ".join(fields)
        after_comm = raw.rsplit(")", 1)[1].split()
        # utime/stime are fields 14/15 overall, i.e. indexes 11/12 past comm.
        return int(after_comm[11]) + int(after_comm[12])
    except (FileNotFoundError, ValueError, IndexError):
        return None


def tree_cpu_pct(pid: int, prev: tuple[int, int]) -> tuple[float | None, tuple[int, int]]:
    """CPU percent for a pid TREE (root + children + grandchildren).

    The launcher pid alone is idle; the worker children burn the CPU, so
    sampling just --pid would sit at 0. Returns (pct_or_None, now).
    """
    try:
        with open("/proc/stat") as handle:
            total_now = sum(int(x) for x in handle.readline().split()[1:])
    except (FileNotFoundError, ValueError):
        return None, prev
    tree_now = 0
    seen = False
    pids = [pid, *common.child_pids(pid)]
    for child in list(pids):
        pids.extend(common.child_pids(child))
    for candidate in pids:
        jiffies = _proc_jiffies(candidate)
        if jiffies is not None:
            tree_now += jiffies
            seen = True
    if not seen:
        return None, prev
    prev_proc, prev_total = prev
    d_proc = tree_now - prev_proc
    d_total = total_now - prev_total
    pct = round(100 * d_proc / d_total, 1) if d_total > 0 else None
    return pct, (tree_now, total_now)


def api_worker(
    host: str,
    port: int,
    token: str,
    deadline: float,
    interval_s: float,
    latencies_ms: list,
    errors: list,
    lock: threading.Lock,
) -> None:
    """Paced GETs across the endpoint mix until the deadline."""
    conn = http.client.HTTPConnection(host, port, timeout=30)
    try:
        conn.connect()
    except Exception as exc:  # noqa: BLE001
        with lock:
            errors.append(f"api-connect: {exc}")
        return
    headers = {"Authorization": f"Bearer {token}"}
    index = 0
    try:
        while time.perf_counter() < deadline:
            path = API_MIX[index % len(API_MIX)]
            index += 1
            start = time.perf_counter()
            try:
                conn.request("GET", path, headers=headers)
                response = conn.getresponse()
                response.read()
                elapsed_ms = (time.perf_counter() - start) * 1000
                with lock:
                    if response.status == 429:
                        errors.append("api-429")
                    elif response.status >= 500:
                        errors.append(f"api-{response.status} {path}")
                    else:
                        latencies_ms.append(elapsed_ms)
            except Exception as exc:  # noqa: BLE001
                with lock:
                    errors.append(f"api-exc {type(exc).__name__}: {exc}")
                try:
                    conn.close()
                    conn.connect()
                except Exception:  # noqa: BLE001
                    pass
            time.sleep(interval_s)
    finally:
        conn.close()


def stream_worker(
    host: str,
    port: int,
    token: str,
    stream_id: str,
    deadline: float,
    interval_s: float,
    ttfb_ms: list,
    total_ms: list,
    errors: list,
    lock: threading.Lock,
) -> None:
    """Loop ranged stream starts; record TTFB (headers) and full-read time."""
    path = f"/api/v1/stream/local/{urllib.parse.quote(stream_id)}"
    conn = http.client.HTTPConnection(host, port, timeout=30)
    try:
        conn.connect()
    except Exception as exc:  # noqa: BLE001
        with lock:
            errors.append(f"stream-connect: {exc}")
        return
    headers = {"Authorization": f"Bearer {token}", "Range": "bytes=0-65535"}
    try:
        while time.perf_counter() < deadline:
            start = time.perf_counter()
            try:
                conn.request("GET", path, headers=headers)
                response = conn.getresponse()
                first_byte_ms = (time.perf_counter() - start) * 1000
                response.read()
                full_ms = (time.perf_counter() - start) * 1000
                with lock:
                    if response.status == 429:
                        errors.append("stream-429")
                    elif response.status >= 500 or response.status not in (200, 206, 429):
                        errors.append(f"stream-{response.status} {stream_id}")
                    else:
                        ttfb_ms.append(first_byte_ms)
                        total_ms.append(full_ms)
            except Exception as exc:  # noqa: BLE001
                with lock:
                    errors.append(f"stream-exc {type(exc).__name__}: {exc}")
                try:
                    conn.close()
                    conn.connect()
                except Exception:  # noqa: BLE001
                    pass
            time.sleep(interval_s)
    finally:
        conn.close()


def resolve_stream_ids(base: str, token: str, count: int) -> list[str]:
    """Pick streamable local file ids via local albums -> album tracks.

    The native catalog track id is NOT a streamable id; only
    LocalTrackInfo.track_file_id feeds /stream/local/{file_id}.
    """
    _, albums = common.api_request(base, token, "GET", "/api/v1/local/albums?limit=50")
    ids: list[str] = []
    for album in albums.get("items", []):
        if len(ids) >= count:
            break
        mbid = album.get("musicbrainz_id")
        if not mbid:
            continue
        _, tracks = common.api_request(
            base, token, "GET", f"/api/v1/local/albums/{urllib.parse.quote(mbid)}/tracks"
        )
        items = tracks if isinstance(tracks, list) else tracks.get("items", [])
        for track in items:
            if track.get("track_file_id"):
                ids.append(str(track["track_file_id"]))
            if len(ids) >= count:
                break
    if len(ids) < count:
        raise SystemExit(f"only {len(ids)} streamable ids found, need {count}")
    return ids[:count]


def evaluate(
    api: dict, stream_ttfb: dict, hard_errors: list[str], alive_at_end: bool, cpu_max: float | None
) -> dict:
    """Evaluate hatch-3 trip conditions. Returns {tripped, conditions}."""
    api_p99 = api.get("p99_ms")
    ttfb_p99 = stream_ttfb.get("p99_ms")
    cond_a = (
        api_p99 is not None
        and api_p99 > TRIP_API_P99_MS
        and cpu_max is not None
        and cpu_max < TRIP_API_CPU_PCT
    )
    cond_b = ttfb_p99 is not None and ttfb_p99 > TRIP_STREAM_TTFB_P99_MS
    cond_c = bool(hard_errors) or not alive_at_end
    return {
        "tripped": bool(cond_a or cond_b or cond_c),
        "3a_api_p99_over_500ms_with_cpu_under_70": cond_a,
        "3b_stream_ttfb_p99_over_300ms": cond_b,
        "3c_errors_or_stall": cond_c,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Hatch-3 2x-peak replay.")
    parser.add_argument("--base", required=True)
    parser.add_argument("--token", required=True)
    parser.add_argument("--duration", type=float, default=60.0, help="Replay seconds")
    parser.add_argument("--api-rate", type=float, default=20.0, help="API req/s total")
    parser.add_argument("--api-concurrency", type=int, default=2, help="API loader threads")
    parser.add_argument("--streams", type=int, default=4, help="Concurrent stream loops")
    parser.add_argument("--stream-interval", type=float, default=0.0, help="Pacing per stream loop (v2 checks need this to stay under the 30/s limiter)")
    parser.add_argument("--stream-id", action="append", default=[], help="Repeatable local file id; default: auto-pick")
    parser.add_argument("--pid", type=int, default=None, help="Instance pid for CPU sampling")
    parser.add_argument("--peak-basis", default="", help="What '2x peak' doubles; recorded verbatim")
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    parts = urllib.parse.urlparse(args.base)
    if (parts.hostname or "") != "127.0.0.1":
        raise SystemExit("replay only targets local 127.0.0.1 instances")
    common.assert_safe_target(port=parts.port)
    host, port = parts.hostname or "127.0.0.1", parts.port or 80

    stream_ids = list(args.stream_id) or resolve_stream_ids(args.base, args.token, args.streams)
    while len(stream_ids) < args.streams:
        stream_ids.append(stream_ids[len(stream_ids) % len(stream_ids)])

    latencies: list = []
    ttfb: list = []
    totals: list = []
    errors: list = []
    lock = threading.Lock()
    cpu_samples: list[float] = []
    stop_cpu = threading.Event()
    cpu_thread = threading.Thread(target=system_cpu_pct, args=(stop_cpu, 1.0, cpu_samples))

    per_thread_interval = args.api_concurrency / args.api_rate if args.api_rate > 0 else 0.05
    deadline = time.perf_counter() + args.duration
    proc_prev: tuple[int, int] | None = None
    proc_samples: list[float] = []
    if args.pid is not None:
        _, proc_prev = tree_cpu_pct(args.pid, (0, 0))  # baseline sample, pct discarded

    threads = []
    cpu_thread.start()
    for _ in range(args.api_concurrency):
        threads.append(
            threading.Thread(
                target=api_worker,
                args=(host, port, args.token, deadline, per_thread_interval, latencies, errors, lock),
            )
        )
    for sid in stream_ids[: args.streams]:
        threads.append(
            threading.Thread(
                target=stream_worker,
                args=(host, port, args.token, sid, deadline, args.stream_interval, ttfb, totals, errors, lock),
            )
        )
    for thread in threads:
        thread.start()
    while any(thread.is_alive() for thread in threads):
        time.sleep(0.5)
        if args.pid is not None and proc_prev is not None:
            pct, proc_prev = tree_cpu_pct(args.pid, proc_prev)
            if pct is not None:
                proc_samples.append(pct)
    for thread in threads:
        thread.join()
    stop_cpu.set()
    cpu_thread.join()

    try:
        common.wait_for_health(args.base, timeout_s=10.0, interval_s=0.5)
        alive = True
    except TimeoutError:
        alive = False

    limiter_hits = sum(1 for e in errors if e.endswith("429"))
    hard_errors = sorted({e for e in errors if not e.endswith("429")})
    api_summary = common.summarize_latency_ms(latencies) if latencies else {"n": 0}
    ttfb_summary = common.summarize_latency_ms(ttfb) if ttfb else {"n": 0}
    total_summary = common.summarize_latency_ms(totals) if totals else {"n": 0}
    cpu_max = max(cpu_samples) if cpu_samples else None

    report = {
        "meta": common.run_metadata(
            {
                "duration_s": args.duration,
                "api_rate": args.api_rate,
                "api_concurrency": args.api_concurrency,
                "streams": args.streams,
                "stream_interval_s": args.stream_interval,
                "peak_basis": args.peak_basis,
            }
        ),
        "api": api_summary,
        "stream_ttfb": ttfb_summary,
        "stream_total": total_summary,
        "cpu_system_max": cpu_max,
        "cpu_system_samples": cpu_samples,
        "cpu_tree_samples": proc_samples,
        "limiter_429_hits": limiter_hits,
        "hard_errors": hard_errors,
        "alive_at_end": alive,
        "hatch3": evaluate(api_summary, ttfb_summary, hard_errors, alive, cpu_max),
    }
    if args.out:
        common.write_json(args.out, report)
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
