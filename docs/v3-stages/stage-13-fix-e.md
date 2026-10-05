# Stage 13 fix step E — scan throughput + RSS after SQLite persistence

Status: ACCEPTED 2026-10-04. Branch `v3`. Gate source:
`docs/v3-stages/stage-13-budgets.md` re-verdicts rows 5/6-8:
fix A made 100k reindex 23 s -> 231-327 s and no-op 3 s ->
75-77 s (rows 7/8 now suspect vs the ≥5,000/≥500 files/s
budgets — re-measure in this step), and post-scan RSS still
fails row 5 (282 MB after one 100k scan, growing per scan to
556-695 MB; bulk is unattributed anonymous heap).

## Manifest

Scope: restore scan throughput with batch persistence
(batch commits / prepared-statement reuse / transaction
scoping in the SQLite scan store — profile first, no blind
knobs) so rows 6-8 pass again with margin; cut post-scan
resident memory (heap-profile first: find what holds the
~435 MB anonymous heap across scans and release or bound it)
so row 5 passes at 100k. Also investigate the incidental
count wobble (discovered 100000/99744/99488/99232 across
runs, 512 phantom-missing files): fix if root-caused to the
SQLite port, else record with evidence.

Preserved behavior: fix-A durability (scan → restart →
reads show catalog); verdict semantics; response shapes.

Intended changes: scan store commit path + coordinator
memory handling only; no schema change unless profiling
proves an index is the throughput fix (migration 0006 if so
— this step owns it); no reads/identify/watcher changes.

Acceptance tests: brief-first, minimal: rows 6-8 bench
briefs at 100k back in budget; row-5 RSS brief (post-scan
idle ≤120 MB); restart-durability still green (fix-A briefs
must keep passing); focused runs only (owner crash order).

Destructive ops: scratch state only; never prod.

Permitted external systems: none.

Non-goals: shutdown scan-abort (fix D — land first, this
step starts after; same area, sequential); query perf (fix
C, done).

Completion criteria: rows 5/6/7/8 re-measured PASS;
review loop clean (implementer != reviewer).
