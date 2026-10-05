# Stage 13 budget verdicts — measured on reference hardware

Status: MEASURED 2026-10-04 against the current `v3` tree. No product
code touched: this step measures only. FAIL rows below are inputs to
gate-driven fix steps, not fixed here.

Contract sources: root `AGENTS.md` (live v3 contract),
`.dev-notes/Plans/RustPort/stage0-approvals-1.md` section 1 (budget
rows), `.dev-notes/Plans/RustPort/stage1-spikes/v3_s1_budget.md` and
`tools/perf-harness/BUDGETS.md` (stage-1 resolved 100k-track API p95:
standard reads ≤10 ms, full-text search with results ≤250 ms, login
≤600 ms).

Method: release binary (`server/target/release/droppedneedle`, built
this run with the repo-pinned toolchain plus `~/.local/cmake/bin` on
PATH for the `opusic-sys` build), scratch app dirs and free 127.0.0.1
ports only. Outbound provider traffic blackholed to a dead proxy so no
junk lookups hit live services (identify fails fast locally; the 1/s
MusicBrainz gate still paces every attempt, same as production).
Corpora are copies of the retained stage-1 fixtures; the audit
originals re-verify clean (200/200 sampled file hashes each, plus the
recorded `111aef1a...` manifest hash). The live `droppedneedle`
container, prod ports, and prod data were never touched.

Machine: Ryzen 5 7640HS, 12 GB RAM, NVMe ext4, Debian 13. Reports and
scripts: `/var/tmp/v3_s13/` (`driver.py`, `seed.py`, `*.json`).

## Verdict table

One row per budget. "Measured" is the headline number; details and n
sizes follow.

| # | Budget row | Budget | Measured | Verdict |
|---|---|---|---|---|
| 1 | Cold boot to ready | ≤1.5 s | 0.11 s | PASS |
| 2 | Warm boot to ready | ≤1.0 s | 0.06 s (also 0.06 s on a 145 MB DB) | PASS |
| 3 | Shutdown (SIGTERM) | ≤0.5 s, clean every time | 0.03 s when quiet; hangs past 120 s (SIGKILL) mid identify drain | FAIL |
| 4 | Idle RSS, process total | ≤80 MB | 41-46 MB (fresh and 145 MB DB) | PASS |
| 5 | Post-workload idle RSS | ≤120 MB | 103-118 MB at ≤918 tracks; 641 MB after 100k scan; 164-168 MB after 100k reads | FAIL at 100k |
| 6 | Initial index rate | ≥500 files/s | 5,497/s (918), 4,597/s (100k) | PASS |
| 7 | No-op rescan rate | ≥5,000 files/s | 183,600/s (918), 34,025/s (100k) | PASS |
| 8 | Re-index rate | ≥500 files/s | 4,310/s (918), 4,404/s (100k) | PASS |
| 9 | API p95, ~1k-track catalog | ≤10 ms | 3.1-3.9 ms on all 8 endpoints | PASS |
| 10 | API p95, 100k standard reads | ≤10 ms | 37-213 ms, 6 of 6 fail | FAIL |
| 11 | API p95, 100k search with results | ≤250 ms | 115 ms local, 57-59 ms unified | PASS |
| 12 | Login p95 | ≤600 ms | ~20 ms (all runs) | PASS |
| 13 | DB steady-state growth | ≤5 KB/track | 1.39 KB/track marginal at 100k | PASS |
| 14 | WAL bounded, no dead retention | Bounded + stated policy | ≤1.4 MB live (64 MB HWM); clean shutdown truncates to 0 bytes | PASS |

Score: 11 pass, 3 fail (rows 3, 5, 10) plus the structural finding
F1 that conditions rows 9-13.

## What the numbers rest on

Three facts shape every row below, so they come first.

F1. Scans do not persist. The scan pipeline reads tags and indexes
into memory stores only; no code in the tree writes `local_tracks` or
`local_albums` (only tests do). After a 100k-file scan the catalog
tables hold exactly the two migration seed artists. Scan rates (rows
6-8) are therefore discovery plus tag plus memory-index rates, and
the API/DB rows (9-13) were measured on SQL-seeded catalogs whose
column set mirrors the repo's own `reads_library.rs` seeder, with
real tag strings from the corpus manifests. A seeded row leaves
probe-detail columns NULL that a real scan would fill, so DB
size-per-track understates reality somewhat; the budget holds with
3.6x headroom anyway.

F2. Shutdown hangs mid identify drain. `identify_tick` drains every
due job with no shutdown check, and each attempt takes a 1/s
MusicBrainz gate slot. The 100k scan enqueues 10,000 album jobs, so a
SIGTERM landing mid-drain waits out the drain: the log shows
"identify attempt finished" continuing at exactly 1/s for the full
120 s past "shutdown signal received" until SIGKILL. Quiet shutdowns
take 0.03 s. This reproduces with live providers too (the gate paces
real calls the same way), so it is not a measurement artifact.

F3. 100k reads miss the budget on query shape, not noise. Re-runs
reproduce within 5%. The worst case, `library_artists` at 213 ms, is
a full-set totals aggregation: the totals SQL alone takes 222 ms in
isolation against the scratch DB. Per-row aggregates and full-set
counts are the same family as the v2 anti-patterns stage 1 called
out (full projections, unindexed counts).

## Row details

Boot and shutdown (rows 1-3). Cold boot on a fresh app dir: 0.114 s
to `/health` 200. Warm reboot of the same dir: 0.060 s. Boot on the
seeded 145 MB catalog: 0.059-0.060 s, so boot does not scale with DB
size. Every run in this step corroborates (a dozen boots, all
0.06-0.11 s). SIGTERM on a quiet instance (n=9) exits 0 in
0.031-0.032 s every time. SIGTERM during the post-scan identify drain
never exited
within 60 s (918 tracks) or 120 s (918 and 100k); all three needed
SIGKILL. Row 3 FAILS on "clean, every time".

Memory (rows 4-5). Idle RSS (whole process tree, `/proc` VmRSS): 46
MB cold, 41-42 MB warm, 42 MB on the 145 MB DB. Post-workload: 113
MB after a 20-file scan plus bench with the drain finished; 118 MB
after 918 files with the drain still running; 641 MB after the 100k
scan (633 MB already right after the scans, before the login
bench, so logins are not the driver); 164-168 MB after a reads-only
bench on the seeded 100k catalog (two runs); 103 MB after the same on
918 tracks. The 100k scan inventory costs about 6 KB/file resident.
Row 5 FAILS at 100k on both the scan and the reads-only workload.

Scan rates (rows 6-8). Server-side started-to-terminal per run; the
first index of a new root runs automatically (Hook B `policy_apply`
run), which is the measured initial. 918 files: initial 918/0.167 s
= 5,497/s; no-op 918/0.005 s = 183,600/s; re-index after touching
every mtime 918/0.213 s = 4,310/s. 100k files: initial
100,000/21.75 s = 4,597/s; no-op 100,000/2.94 s = 34,025/s;
re-index 100,000/22.70 s = 4,404/s. Zero errors on all six runs.
Client-measured walls agree at 100k (23.2 s re-index, 3.0 s no-op);
at 918 the client wall is poll-quantum dominated, hence the
server-side figures. All PASS with wide margins, under the F1
caveat that "index" lands in memory only.

API latency (rows 9-12). Stage-1 bench shape throughout: localhost,
keep-alive, n=50 per endpoint after 3 warmup, paced ~22/s, any 429
aborts the run (login is the exception: sequential, 429s retried,
never recorded; about 20 of 70 attempts hit the limiter each run).
v3 analogues of the stage-1 seven endpoints plus unified search.

Seeded 918 (92 albums, real format split), p95: albums 3.27 ms,
artists 3.40 ms, tracks 3.45 ms, stats 3.21 ms, local albums 3.09
ms, search miss 3.36 ms, local search hit 3.90 ms, unified search
hit 3.27 ms. Row 9 PASSES.

Seeded 100k (10,000 albums, 500 artists), p95 with re-run in
parentheses: albums 38.96 (38.88) ms, artists 213.32 (213.56) ms,
tracks 53.78 (50.98) ms, stats 74.79 (74.81) ms, local albums 38.39
(37.17) ms, search miss 64.28 (63.85) ms, local search hit 115.03
(115.10) ms, unified search hit 57.13 (58.94) ms. The six standard
reads (lists, stats, empty search) all miss the 10 ms budget,
some by an order of magnitude: row 10 FAILS. Both search-with-results
endpoints hold the 250 ms budget with margin: row 11 PASSES. Login p95
sits at 19-21 ms in all six login benches (n=50 each): row 12
PASSES.

DB growth (row 13). Fresh setup DB is 2,592,768 bytes. Seeded 100k
is 145,055,744 bytes, so marginal growth is (145,055,744 -
2,592,768)/100,000 = 1,425 bytes/track = 1.39 KB/track. The 918
seed gives 1.36 KB/track, consistent. Row 13 PASSES. No per-run
inventory retention exists to repeat the v2 5.64 KB blowup (there is
no scan inventory table content at all, per F1).

WAL (row 14). Live `-wal` peaked at 1.36 MB across all runs against
the 64 MB backpressure high water: bounded in practice. The stated
policy is PASSIVE every 30 s, live TRUNCATE reclaim only when quiet
plus an hour since the last reclaim, and TRUNCATE at clean shutdown.
The shutdown reclaim is verified working: `-wal` and `-shm` go to 0
bytes on every clean shutdown including after the 100k bench, with
"shutdown checkpoint reclaimed the WAL file" in the log and
`/health` reporting `active_bytes: 0`. The hour-gated live reclaim
never fired (no run lasts an hour), and the SIGKILL shutdowns
necessarily retain the WAL; both follow from the stated policy plus
F2 rather than contradicting it. Row 14 PASSES with F2 recorded as
the path that skips the reclaim.

## Incidental observations (not budget rows)

- The server writes a `.droppedneedle-management-meta/publish.db`
  sidecar into each music root on registration. That is not a music
  file so it breaks no stated rule, but it means library roots gain
  a hidden directory as a side effect of adding them.
- The identify queue is memory resident: a restart clears pending
  jobs. That made the quiet-shutdown measurements possible and is
  worth a durability look in a fix step.
- Fixture caveat carried from stage 0/1: corpus files are 1-55 KB
  silences, so scan rates are discovery-plus-parse bound, not audio
  I/O bound. No concurrency, streaming, or tag-apply numbers yet,
  same scope as stage 1.

## Reproduction

```bash
export PATH="$HOME/.local/cmake/bin:$PATH"   # opusic-sys needs cmake
cargo build --release --manifest-path server/Cargo.toml
cp -a /var/tmp/v3_s1_corpus_918 /var/tmp/v3_s13/corpus_918
cp -a /var/tmp/v3_s1_corpus_100k /var/tmp/v3_s13/corpus_100k
python3 /var/tmp/v3_s13/driver.py boot --appdir /var/tmp/v3_s13/boot \
  --port 18901 --out /var/tmp/v3_s13/boot.json
python3 /var/tmp/v3_s13/driver.py run --corpus /var/tmp/v3_s13/corpus_100k \
  --workdir /var/tmp/v3_s13/run_100k --port 18905 --tag 100k \
  --out /var/tmp/v3_s13/run_100k.json --shutdown-timeout 120
# seeded-catalog benches: boot+setup an appdir, stop it, then
python3 /var/tmp/v3_s13/seed.py --corpus /var/tmp/v3_s13/corpus_100k \
  --db /var/tmp/v3_s13/app_seed100k/cache/library.db
python3 /var/tmp/v3_s13/driver.py bench --appdir /var/tmp/v3_s13/app_seed100k \
  --port 18913 --tag seed100k --out /var/tmp/v3_s13/bench_100k.json
```

Every command binds scratch ports, refuses prod ports/paths, and
blackholes outbound provider traffic. No step writes product code or
touches the live container.

## Re-verdicts after fixes A/B/C (2026-10-04)

Status: RE-MEASURED 2026-10-04 against the current `v3` tree with
fixes A (SQLite scan store), B (shutdown-aware identify drain), and
C (read-path indexes, migration 0005) landed but uncommitted. No
product code touched by this step. Only the three FAIL rows were
re-measured, with the same `driver.py`/`seed.py` tooling, corpora,
and bench shape as above; the release binary was rebuilt first
(binary mtime newer than every fix source). Scratch ports
18921-18933, dead-proxy blackhole, live container untouched. The
seeded 100k catalog was rebuilt fresh so fix C's indexes apply
(175.8 MB now vs 145 MB before, same seed method). Reports:
`/var/tmp/v3_s13/remeasure/`.

Row 3: Shutdown (SIGTERM), budget ≤0.5 s clean every time — FAIL
(mechanism changed). The original failure is gone: SIGTERM with a
provably active identify drain now exits 0 in 0.032 s (918 tracks),
0.032 s (918 rerun), and 0.064 s (100k, drain markers 1 -> 4 over
3 s before the signal), with zero identify attempts finishing after
the signal in all three probes. Fix B works. But both full-workload
100k runs shut down slowly and cleanly: 17.3 s and 22.4 s, exit 0.
The timestamped log attributes the 22.4 s fully: a filesystem-watcher
rescan was in flight at SIGTERM and shutdown ran it to completion
(signal 15:55:31, scan completed 15:55:54, checkpoint 30 ms, drain
abort instant with 0 post-signal attempts). The scan was
self-triggered: the server wrote its own `publish.db` sidecar inside
the watched corpus root mid-bench and the watcher queued a 100k
rescan ~80 s later. Shutdown never asks scans to stop
(`stop_requested_at` is NULL on the waited-out run). So the drain
hang is fixed but the row as written ("every time") still fails:
any SIGTERM landing during a scan waits it out, and fix A made
scans 10-25x slower (reindex 23 s -> 231-327 s, no-op 3 s -> 75-77 s
at 100k), widening the window. This is new finding F4 below, not
F2.

Row 5: Post-workload idle RSS, budget ≤120 MB — FAIL. After the
100k scan workload: 556 MB (run A, right after scans), 620 MB (run
A, post workload), 585 MB and 695 MB (run B rerun). A single 100k
scan alone leaves 282 MB, so RSS grows per scan (282 -> 556-585
across three). A maps rollup mid-run shows ~435 of 445 MB as
anonymous heap, not mmap or page cache; the identify queue (one
small job per album) plus the track/album map account for only tens
of MB, so the bulk is unattributed and needs a heap profile to fix
with confidence. After 100k reads on the seeded catalog: 119.6,
119.7, and 119.9 MB across three runs — a pass, but by 0.1-0.4 MB.
One of four reads runs spiked to 185.7 MB with all endpoints
slower; a same-day info-level rerun came back at 119.9 MB, ruling
out log level, so the spike reads as shared-box contention (parallel
workers were building on this 12 GB box). Reported, not hidden.

Row 10: API p95 on seeded 100k standard reads, budget ≤10 ms —
PASS. Three runs agree within 5% (n=50 after warmup, same paced
shape):

| Endpoint | r1 p95 | r3 p95 | r4 p95 |
|---|---|---|---|
| library_albums | 3.37 | 3.96 | 3.84 |
| library_artists | 9.04 | 8.81 | 8.96 |
| library_tracks | 2.70 | 2.84 | 2.84 |
| library_stats | 2.14 | 2.28 | 2.21 |
| local_albums | 3.40 | 3.54 | 3.66 |
| local_search_miss | 4.20 | 4.81 | 4.41 |

All six pass in all three runs (fix C closed the 37-213 ms gap),
but `library_artists` holds only ~1 ms of margin, so it bears
watching. The fourth run spiked artists to 16.18 ms in the same
contention window as the row-5 spike above; the other two runs at
both log levels agree with r1, so this is scored a pass with the
outlier on record. No regressions elsewhere: search with results
10.9-12.1 / 57-64 ms (row 11 intact), login p95 19.7-19.9 ms (row
12 intact).

F4. Shutdown waits out in-flight scans (new). Nothing in the
shutdown path requests a scan stop; a scan in flight at SIGTERM runs
to completion first (measured 17.3 / 22.4 s at 100k). The watcher
self-trigger (own sidecar write inside a watched root queuing a full
rescan) makes this easy to hit. Needs its own fix step: scan abort
or stop-request on shutdown, plus keeping the server's own sidecar
writes from retriggering the watcher.

Incidental: per-scan discovered counts wobble (100000 / 99744 /
99488 / 99232 across runs with an untouched corpus), and one no-op
scan reported 512 missing files that the next reindex found as new.
Scan correctness is outside these three rows, but a fix step owning
that area should look.
