# Stage 13 release gates — re-run record

Status: ALL GATES PASS, re-run 2026-10-04 against the current `v3` tree.
Runner: stage-13 release-gates worker (read-only on product code: no fixes,
no edits outside this file).

Contract sources: root `AGENTS.md` (live v3 contract),
`.dev-notes/Plans/RustPort/stage0-compat.md` (compat matrix),
`.dev-notes/Plans/RustPort/00-decisions.md` Q5 (per-source release gates:
slskd, SABnzbd, Newznab, Lidarr import - the OR-readiness check is only a
smoke signal and never a release verdict).

Method: focused `cargo test --test <file>` runs only, per the owner crash
order (no full suites). Every run below used the in-repo loopback mocks and
test doubles (the Rust equivalents of the old mock ASGI apps): no real
external services, no real credentials, scratch databases and throwaway
directories only. The live `droppedneedle` container, prod ports, and prod
data were never touched. Toolchain: pinned Rust 1.89.0.

## 1. Per-source gates (Q5)

| # | Gate | Test file | Result | Exit |
|---|---|---|---|---|
| 1 | slskd end to end | `server/tests/acquire_slskd.rs` | 35 passed, 0 failed | 0 |
| 2a | SABnzbd end to end | `server/tests/acquire_usenet.rs` (full file) | 29 passed, 0 failed | 0 |
| 2b | SABnzbd slice | same file, filter `sab_` | 15 passed, 14 filtered out | 0 |
| 3 | Newznab slice | same file, filter `newznab` | 7 passed, 22 filtered out | 0 |
| 4a | Lidarr import + health | `server/tests/acquire_imports.rs` (full file) | 38 passed, 0 failed | 0 |
| 4b | Lidarr slice | same file, filter `lidarr` | 11 passed, 27 filtered out | 0 |
| 5 | Source-path journeys | `server/tests/acquire_journey.rs` | 6 passed, 0 failed | 0 |

Verdicts:

- slskd: PASS. Wire-shape briefs (lossless bitRate-absent, PascalCase
  enqueue, searchTimeout ms), 429 retry, semaphore serialization, search
  ladder fallback, and correlation briefs all green against `MockSlskd` on
  loopback.
- SABnzbd: PASS. Suffix params, addurl fallback, multipart add, status walk,
  history filter, abort/discard ownership, remap, materialization, and
  redaction briefs all green against `SabnzbdMock` over 127.0.0.1.
- Newznab: PASS. Caps, query strategy, music 202 fallback, error forms, item
  parsing, query ladder, and fan-out briefs all green against `NewznabMock`.
- Lidarr import: PASS. Monitored-artists-become-follows plus the health
  smoke with independent per-source gates all green against loopback mocks.
- End-to-end paths: PASS. Request to approve to land, wanted watch to
  auto-download, drop to quarantine to resolve, and follow-toggle journeys
  run through the real app on scratch databases.

## 2. Compat matrix re-run

| Test file | Result | Exit |
|---|---|---|
| `server/tests/compat_subsonic.rs` | 117 passed, 0 failed | 0 |
| `server/tests/compat_jellyfin.rs` | 34 passed, 0 failed | 0 |
| `server/tests/compat_journeys.rs` | 15 passed, 0 failed | 0 |
| `server/tests/compat_shared.rs` | 21 passed, 0 failed | 0 |
| `server/tests/compat_adapters.rs` | 7 passed, 0 failed | 0 |
| `server/tests/compat_wiring.rs` | 9 passed, 0 failed | 0 |
| `server/tests/auth_compat_goldens.rs` | 22 passed, 0 failed | 0 |

Verdict: PASS (225 passed, 0 failed). Subsonic and Jellyfin goldens match
the pinned reference expectations in `server/tests/fixtures/compat/`
(nothing re-blessed: `COMPAT_BLESS` was never set), and the
Symfonium/Finamp/Jellify client-shaped journeys stay green through the real
compat routers.

## 3. Crash/recovery re-run

| Test file | Result | Exit |
|---|---|---|
| `server/tests/library_publish.rs` | 32 passed, 0 failed | 0 |
| `server/tests/acquire_downloads.rs` | 37 passed, 0 failed | 0 |
| `server/tests/import_safety.rs` | 11 passed, 0 failed | 0 |
| `server/tests/acquire_download_tasks.rs` | 3 passed, 0 failed | 0 |

Named-behavior confirmations (filtered re-runs, each 1 passed, exit 0):

- Kill mid write, publisher: `crash_at_every_phase_resumes_without_half_state`
  (31 filtered out).
- Kill before commit, import: `kill_before_commit_recovers_clean`
  (10 filtered out).
- Restart mid download: `restart_mid_download_resumes_without_duplicate_fetch`
  (36 filtered out).
- Startup reconciliation: `orphan_reconcile_removes_only_proven_debris`
  (36 filtered out).
- Journal dispatch: covered by the 3 `acquire_download_tasks` tests
  (`dispatch_reads_progress_and_guard_from_journal` among them).

Verdict: PASS (83 passed, 0 failed). Crash-injection at every publisher
phase resumes with no half state, restarts resume without duplicate fetches,
orphan reconciliation removes only proven debris, and journal-backed
dispatch reads progress and guards correctly.

## Notes

- No gate failed, so no gate-driven fix step is needed from this re-run.
- Product code was not modified. The only tree change observed during the
  run was an untracked file from another worker
  (`server/examples/rehearsal_fixture.rs`); it was left untouched and plays
  no part in these results.
- Pre-existing untracked `frontend/test-results/` was also left alone.
