#!/usr/bin/env python3
"""Paced API latency sampler against a local instance (stdlib only).

Sequential GETs per endpoint over a keep-alive connection, after warmup.
Sustains ~22 req/s by default: the v2 API has global + per-user 30/s token
buckets (capacity 60 each), and unpaced benching measures the 429 limiter
instead of the handlers (Stage-0 finding). Any 429 aborts the run with
guidance instead of recording polluted numbers.

Default endpoint set (Stage-0 provenance; search-hit query configurable):
  library_albums  GET /api/v1/library/albums?page=1&page_size=50
  library_artists GET /api/v1/library/artists?limit=50
  library_tracks  GET /api/v1/library/tracks?limit=48
  library_stats   GET /api/v1/library/stats
  local_albums    GET /api/v1/local/albums?limit=50
  local_search_miss GET /api/v1/local/search?q=<rare>
  local_search_hit  GET /api/v1/local/search?q=<hit>

Usage:
  api_latency.py --base http://127.0.0.1:PORT --token TOKEN [--out report.json]
      [--requests 50] [--warmup 3] [--interval 0.045] [--search-hit Track]
"""

from __future__ import annotations

import argparse
import http.client
import json
import sys
import time
import urllib.parse
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402


def build_endpoints(search_hit: str, search_miss: str) -> list[tuple[str, str]]:
    return [
        ("library_albums", "/api/v1/library/albums?page=1&page_size=50"),
        ("library_artists", "/api/v1/library/artists?limit=50"),
        ("library_tracks", "/api/v1/library/tracks?limit=48"),
        ("library_stats", "/api/v1/library/stats"),
        ("local_albums", "/api/v1/local/albums?limit=50"),
        ("local_search_miss", "/api/v1/local/search?q=" + urllib.parse.quote(search_miss)),
        ("local_search_hit", "/api/v1/local/search?q=" + urllib.parse.quote(search_hit)),
    ]


def bench_endpoint(
    host: str, port: int, path: str, token: str, requests: int, warmup: int, interval_s: float
) -> dict:
    conn = http.client.HTTPConnection(host, port, timeout=30)
    conn.connect()
    headers = {"Authorization": f"Bearer {token}"}
    try:
        for _ in range(warmup):
            conn.request("GET", path, headers=headers)
            response = conn.getresponse()
            response.read()
            if response.status == 429:
                raise RuntimeError("429 during warmup: limiter engaged, lower the rate")
        latencies: list[float] = []
        statuses: dict[int, int] = {}
        body_bytes = 0
        for _ in range(requests):
            start = time.perf_counter()
            conn.request("GET", path, headers=headers)
            response = conn.getresponse()
            body = response.read()
            latencies.append((time.perf_counter() - start) * 1000)
            statuses[response.status] = statuses.get(response.status, 0) + 1
            body_bytes += len(body)
            if response.status == 429:
                raise RuntimeError(
                    f"HTTP 429 on {path}: rate limiter engaged, numbers would be "
                    "polluted — rerun with a larger --interval"
                )
            time.sleep(interval_s)
    finally:
        conn.close()
    summary = common.summarize_latency_ms(latencies)
    summary.update({"statuses": {str(k): v for k, v in statuses.items()}, "avg_body_bytes": body_bytes // requests})
    return summary


def run_bench(
    base: str,
    token: str,
    requests: int = 50,
    warmup: int = 3,
    interval_s: float = 0.045,
    search_hit: str = "Track",
    search_miss: str = "zzz-no-such-term-zzz",
) -> dict:
    parts = urllib.parse.urlparse(base)
    if (parts.hostname or "") != "127.0.0.1":
        raise SystemExit("api_latency only targets local 127.0.0.1 instances")
    host, port = parts.hostname or "127.0.0.1", parts.port or 80
    common.assert_safe_target(port=port)  # judge the port we actually dial
    out: dict = {}
    for name, path in build_endpoints(search_hit, search_miss):
        out[name] = {"path": path, **bench_endpoint(host, port, path, token, requests, warmup, interval_s)}
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description="Sample API latency (paced, keep-alive).")
    parser.add_argument("--base", required=True)
    parser.add_argument("--token", required=True)
    parser.add_argument("--requests", type=int, default=50)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--interval", type=float, default=0.045, help="Pacing seconds (~22 req/s)")
    parser.add_argument("--search-hit", default="Track")
    parser.add_argument("--search-miss", default="zzz-no-such-term-zzz")
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    results = run_bench(
        args.base, args.token, requests=args.requests, warmup=args.warmup,
        interval_s=args.interval, search_hit=args.search_hit, search_miss=args.search_miss,
    )
    payload = {
        "meta": common.run_metadata(
            {"requests": args.requests, "warmup": args.warmup, "interval_s": args.interval}
        ),
        "endpoints": results,
    }
    if args.out:
        common.write_json(args.out, payload)
    print(json.dumps(payload, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
