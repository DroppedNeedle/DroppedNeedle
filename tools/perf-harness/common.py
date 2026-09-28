#!/usr/bin/env python3
"""Shared helpers for the Stage-1 perf harness (stdlib only).

Safety: every entry point funnels port/path selection through
assert_safe_target(), which refuses prod ports and prod data paths.
The harness only ever drives a LOCAL throwaway instance on 127.0.0.1.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

# ---------------------------------------------------------------------------
# Paths / constants
# ---------------------------------------------------------------------------

HARNESS_DIR = Path(__file__).resolve().parent
REPO_ROOT = HARNESS_DIR.parent.parent  # tools/perf-harness -> repo root
BACKEND_DIR = REPO_ROOT / "backend"
FIXTURE_DIR = BACKEND_DIR / "tests" / "fixtures" / "library"


def venv_python() -> str:
    """Backend venv interpreter (owns mutagen + app deps)."""
    override = os.environ.get("PERF_VENV_PY")
    if override:
        return override
    candidate = BACKEND_DIR / ".venv" / "bin" / "python"
    if candidate.exists():
        return str(candidate)
    return sys.executable


# ---------------------------------------------------------------------------
# Safety: never touch prod
# ---------------------------------------------------------------------------

REFUSED_PORTS = frozenset({8688, 8689, 80, 443})
REFUSED_PATH_PREFIXES = (
    "/srv/hosting/volumes",
    "/srv/hosting/storage",
    "/music",
    "/app",
)

# Ports 8688/8689 are the prod/dev container ports; 80/443 are the proxy.


def assert_safe_target(port: int | None = None, paths: list[str | Path] | None = None) -> None:
    """Raise SystemExit if a port or path targets prod infrastructure."""
    if port is not None and int(port) in REFUSED_PORTS:
        raise SystemExit(f"refusing prod/reserved port {port}")
    for raw in paths or []:
        resolved = os.path.realpath(str(raw))
        for prefix in REFUSED_PATH_PREFIXES:
            if resolved == prefix or resolved.startswith(prefix.rstrip("/") + "/"):
                raise SystemExit(f"refusing prod data path {raw} (resolves under {prefix})")


def pick_free_port() -> int:
    """Return a currently-free localhost TCP port (bind :0 then release)."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    assert_safe_target(port=port)
    return port


# ---------------------------------------------------------------------------
# HTTP helpers (urllib, no deps)
# ---------------------------------------------------------------------------


class HttpError(RuntimeError):
    def __init__(self, status: int, body: str, path: str):
        super().__init__(f"HTTP {status} for {path}: {body[:300]}")
        self.status = status
        self.body = body
        self.path = path


def api_request(
    base: str,
    token: str | None,
    method: str,
    path: str,
    body: dict | None = None,
    timeout: float = 30.0,
) -> tuple[int, dict]:
    """JSON request against the local instance. Returns (status, parsed body)."""
    data = json.dumps(body).encode() if body is not None else None
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(base + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read().decode()
    except urllib.error.HTTPError as exc:
        raise HttpError(exc.code, exc.read().decode(errors="replace"), path) from exc
    try:
        parsed = json.loads(raw) if raw else {}
    except json.JSONDecodeError as exc:
        raise HttpError(-1, f"non-JSON response: {raw[:200]}", path) from exc
    return 200, parsed


def wait_for_health(base: str, timeout_s: float = 600.0, interval_s: float = 0.05) -> float:
    """Poll GET /health until body reports {"status": "ok"}. Returns elapsed s."""
    deadline = time.perf_counter() + timeout_s
    start = time.perf_counter()
    url = base + "/health"
    while time.perf_counter() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2) as response:
                body = response.read().decode()
            if response.status == 200 and '"ok"' in body and '"upgrading"' not in body:
                return time.perf_counter() - start
        except Exception:
            pass
        time.sleep(interval_s)
    raise TimeoutError(f"{url} never reported status ok within {timeout_s}s")


# ---------------------------------------------------------------------------
# Stats
# ---------------------------------------------------------------------------


def percentile(sorted_values: list[float], pct: float) -> float:
    """Nearest-rank percentile over an already-sorted list (Stage-0 method)."""
    if not sorted_values:
        raise ValueError("empty sample")
    rank = math.ceil(pct / 100 * len(sorted_values))
    index = min(len(sorted_values) - 1, max(0, rank - 1))
    return sorted_values[index]


def summarize_latency_ms(samples_ms: list[float]) -> dict:
    ordered = sorted(samples_ms)
    mean = sum(ordered) / len(ordered)
    return {
        "n": len(ordered),
        "mean_ms": round(mean, 3),
        "min_ms": round(ordered[0], 3),
        "p50_ms": round(percentile(ordered, 50), 3),
        "p95_ms": round(percentile(ordered, 95), 3),
        "p99_ms": round(percentile(ordered, 99), 3),
        "max_ms": round(ordered[-1], 3),
    }


# ---------------------------------------------------------------------------
# /proc RSS sampling
# ---------------------------------------------------------------------------


def proc_rss(pid: int) -> dict:
    """VmRSS/VmHWM/Threads/Name/cmdline for one pid (empty if exited)."""
    out: dict = {}
    try:
        with open(f"/proc/{pid}/status") as handle:
            for line in handle:
                if line.startswith(("VmRSS:", "VmHWM:", "Threads:", "Name:")):
                    key, value = line.split(":", 1)
                    out[key] = value.strip()
    except FileNotFoundError:
        return {}
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as handle:
            out["cmdline"] = handle.read().replace(b"\0", b" ").decode().strip()[:160]
    except FileNotFoundError:
        pass
    return out


def child_pids(pid: int) -> list[int]:
    try:
        with open(f"/proc/{pid}/task/{pid}/children") as handle:
            return [int(x) for x in handle.read().split()]
    except (FileNotFoundError, ValueError):
        return []


def rss_kb(status: dict) -> int | None:
    """Parse 'VmRSS: 169696 kB' -> 169696."""
    raw = status.get("VmRSS", "")
    try:
        return int(raw.split()[0])
    except (IndexError, ValueError):
        return None


def sample_tree(root_pid: int) -> dict:
    """Sample RSS for a root pid plus children and grandchildren."""
    nodes = [{"pid": root_pid, **proc_rss(root_pid)}]
    total_kb = 0
    for child in child_pids(root_pid):
        nodes.append({"pid": child, **proc_rss(child)})
        for grandchild in child_pids(child):
            nodes.append({"pid": grandchild, **proc_rss(grandchild)})
    for node in nodes:
        value = rss_kb(node)
        if value is not None:
            total_kb += value
    return {"nodes": nodes, "tree_total_rss_kb": total_kb}


# ---------------------------------------------------------------------------
# Files / reports
# ---------------------------------------------------------------------------


def file_size(path: str | Path) -> int | None:
    try:
        return os.path.getsize(path)
    except OSError:
        return None


def sha256_file(path: str | Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git_sha() -> str:
    try:
        return (
            subprocess.run(
                ["git", "rev-parse", "--short", "HEAD"],
                cwd=str(REPO_ROOT),
                capture_output=True,
                text=True,
                timeout=10,
            )
            .stdout.strip()
            or "unknown"
        )
    except Exception:
        return "unknown"


def run_metadata(extra: dict | None = None) -> dict:
    meta: dict = {
        "harness": "tools/perf-harness",
        "repo_root": str(REPO_ROOT),
        "git_sha": git_sha(),
        "unix_time": int(time.time()),
        "venv_python": venv_python(),
    }
    if extra:
        meta.update(extra)
    return meta


def write_json(path: str | Path, payload: dict) -> str:
    with open(path, "w") as handle:
        json.dump(payload, handle, indent=2)
        handle.write("\n")
    return str(path)
