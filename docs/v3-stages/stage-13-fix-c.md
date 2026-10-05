# Stage 13 fix step C — 100k catalog API p95 back in budget

Status: ACCEPTED 2026-10-04. Branch `v3`. Gate source:
`docs/v3-stages/stage-13-budgets.md` row 10 (FAIL) + F3.

## Manifest

Scope: six standard reads miss the 10 ms p95 budget on a
seeded 100k catalog (measured 37-213 ms; worst is
`library_artists` full-set totals aggregation at ~213 ms,
222 ms for the totals SQL in isolation). Fix query shape:
indexes, aggregate pushdown, and/or maintained totals — the
v2 anti-pattern family (full projections, unindexed counts)
called out in stage 1. All six endpoints must land ≤10 ms
p95 in the stage-1 bench shape (localhost, keep-alive, n=50
after warmup); search/login rows must not regress.

Preserved behavior: response shapes, filter semantics,
totals values, sort orders; no API contract change.

Intended changes: SQL + index changes in the reads layer;
migration `0005_*` (this step owns the number — fix A adds
no migration); totals-maintenance triggers/caches only if
indexes alone cannot close the gap, with invalidation proven
by briefs.

Acceptance tests: brief-first, minimal: per-endpoint p95
briefs on a seeded 100k catalog in the repo's own bench
shape; totals-correctness briefs if totals move; focused
runs only (owner crash order).

Destructive ops: scratch DBs only; never prod.

Permitted external systems: none.

Non-goals: scan persistence (fix A); shutdown drain (fix
B); new endpoints.

Completion criteria: row-10 re-measurement passes all six
endpoints; review loop clean (implementer != reviewer).
