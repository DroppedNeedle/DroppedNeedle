# Stage 13 fix step A — persist the scan catalog to SQLite

Status: ACCEPTED 2026-10-04. Branch `v3`. Gate source:
`docs/v3-stages/stage-13-budgets.md` finding F1 + rows 5/10 context.

## Manifest

Scope: replace the runtime `MemoryScanStore` with a SQLite-backed
`ScanStore` implementation that persists runs, scopes, inventory,
failures, and the track catalog (`local_tracks`, `local_albums`,
`local_artists`, join tables) into the existing migration-0001
schema (`library_scan_*`, `local_*`, identity/genre/join tables),
and wire it into the production runtime in place of the memory
store. Rescan semantics (classify by exact/legacy revision,
unchanged/excluded/expired verdicts) must behave identically;
reads (`local_tracks.availability = 'indexed'`) must show scanned
content; a restart must keep the catalog.

Preserved behavior: `ScanStore` trait semantics and dispositions;
coordinator/walker untouched; `MemoryScanStore` stays for unit
tests (it is the honest in-process fake, not dead code).

Intended changes: new `SqliteScanStore` (+ catalog-commit path
mapping indexed files to `local_*` rows); runtime wiring swaps
the store; no migration file (all tables exist in 0001) — fix C
owns `0005_*`.

Acceptance tests: brief-first, minimal: persist-then-restart
scan shows catalog rows via the reads layer; rescan verdicts
unchanged for untouched files; failure records durable; focused
`cargo test` slices only (owner crash order).

Destructive ops: scratch DBs/dirs only; never prod.

Permitted external systems: none.

Non-goals: identify-queue durability (recorded follow-up);
query-shape perf (fix C); shutdown drain (fix B).

Completion criteria: scan → restart → reads show the catalog;
review loop clean (implementer != reviewer).
