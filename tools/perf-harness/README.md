# DroppedNeedle v3 Stage-1 performance harness

Rerunnable workload/budget harness for the v3 performance budgets
(`.dev-notes/Plans/RustPort/stage0-approvals-1.md` §1, `build-stage-plan.md`
Stage 1). Measures boot, RSS, scan rates, API latency, and DB/WAL behavior
against a **local throwaway instance only**.

## Safety (read first)

- Every entry point binds `127.0.0.1` and picks a **free port** by default.
  Prod ports (`8688`, `8689`) and prod data paths (`/srv/hosting/volumes`,
  `/srv/hosting/storage`, `/music`, `/app`) are **refused** — see
  `common.py:assert_safe_target`. The harness never touches the prod
  container or its volumes.
- Measurement state defaults to `/var/tmp/v3_s1_*` (ext4). `/tmp` here is
  tmpfs: fine for small reports, never for corpora or app dirs.
- The harness boots the backend read-only from the working tree via the
  sanctioned launcher (`maintenance.automatic_upgrade --start-target`).
  It writes no repo files and commits nothing.

## Layout

| File | Purpose |
|---|---|
| `common.py` | Shared helpers: free ports, HTTP+auth, percentiles, RSS/proc, reports, safety |
| `boot.py` | Cold/warm boot-to-ready + graceful-shutdown timing |
| `rss.py` | RSS sampler (launcher + process tree), one-shot or watch |
| `scan.py` | Scan-rate runner (incremental / no-op / rescan_files) + DB/WAL timeline |
| `api_latency.py` | Paced API latency sampler (keep-alive, <=25 req/s, 429-aware) |
| `db_observer.py` | DB/WAL snapshot + read-only PRAGMA introspection (never checkpoints live) |
| `replay.py` | Hatch-3 2x-peak replay: mixed API + ranged-stream load, trip-condition verdict |
| `run_workload.py` | End-to-end orchestrator: boot → setup → roots → scans → API → RSS/DB → shutdown |
| `corpus/generate_corpus.py` | Deterministic synthesized-library generator (byte-copy + mutagen retag, no ffmpeg) |

## Requirements

- Python 3.11+ stdlib only for all measurement scripts.
- `corpus/generate_corpus.py` additionally needs `mutagen` — run it with
  `backend/.venv/bin/python` (mutagen 1.48.1 there). It never needs ffmpeg:
  it byte-copies the committed fixtures and retags the copies.

## Quick start

```bash
# 1. Generate a deterministic corpus (example: 10k tracks, seed 1)
backend/.venv/bin/python tools/perf-harness/corpus/generate_corpus.py \
  --tracks 10000 --seed 1 --out /var/tmp/v3_s1_corpus_10k

# 2. Full workload against a local instance (free port, fresh app dir)
backend/.venv/bin/python tools/perf-harness/run_workload.py \
  --corpus /var/tmp/v3_s1_corpus_10k --workdir /var/tmp/v3_s1_run_10k

# 3. Report lands at <workdir>/report.json; budget numbers print to stdout.
```

Individual stages (all `--help`-documented):

```bash
# Boot timing only (cold boot of a fresh app dir, then clean shutdown)
backend/.venv/bin/python tools/perf-harness/boot.py --appdir /var/tmp/v3_s1_boottest

# API sampler against a running instance (needs base URL + Bearer [REDACTED]
backend/.venv/bin/python tools/perf-harness/api_latency.py \
  --base http://127.0.0.1:PORT --token TOKEN

# DB/WAL snapshot
backend/.venv/bin/python tools/perf-harness/db_observer.py snapshot /path/to/appdir/cache

# RSS sample of a process tree
backend/.venv/bin/python tools/perf-harness/rss.py --pid PID

# Hatch-3 replay against a running instance (60 s, 20 API req/s, 4 streams)
backend/.venv/bin/python tools/perf-harness/replay.py \
  --base http://127.0.0.1:PORT --token TOKEN --pid PID \
  --peak-basis "v2 check: 20/s under the 30/s limiter ceiling, no measured prod peak"
```

## Determinism

- `--seed` (default `1`) drives corpus layout, tag assignment, and template
  choice. File mtimes are fixed (`1700000000 + index`) so reruns are stable.
- Request counts, pacing, warmup, and poll intervals are fixed flags recorded
  in every report. Reports embed `git_sha`, `seed`, hardware notes, and the
  exact command lines used.

## CI-readiness

Scripts are stdlib-only (plus mutagen for the generator), exit non-zero on
failure, write machine-readable JSON to stdout (`--out` for files), and take
no interactive input. Not yet wired into CI - the resolved budgets live in
`BUDGETS.md` (100k-track API p95 + suite-speed ceilings).

## Reference corpora (Stage 0 provenance)

- `ref-18`: committed fixtures `backend/tests/fixtures/library/` (audio only).
- `ref-918`: 50x scaled copy used in `stage0-baseline.md` (not retained as a
  fixture; reproducible via `generate_corpus.py --tracks 918 --seed 7` shape
  or plain copy — see budget doc for equivalence notes).
- `gen-100k`: `generate_corpus.py --tracks 100000 --seed 1` (100k-track
  synthesized library; see `BUDGETS.md` for the resolved API p95 budget).

## Caveats (carried from Stage 0)

Fixture/synthesized files are ~0.3 s silence (1-55 KB): scan rates are
discovery+tag+index bound, not audio-I/O bound. API samples are localhost,
single-user, no concurrency. No streaming/tag-apply/concurrent numbers yet.
