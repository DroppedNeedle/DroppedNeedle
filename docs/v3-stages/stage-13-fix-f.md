# Stage 13 fix step F — post-scan RSS floor (allocator + tag churn + caches)

Status: ACCEPTED 2026-10-04. Branch `v3`. Gate source:
`docs/v3-stages/stage-13-budgets.md` row 5 (still FAIL after
fix E: 208-246 MB vs ≤120 MB budget).

## Manifest

Scope: fix E proved the scan-store/coordinator floor is
74 MB and the remaining ~135-170 MB sits in tag-parse
churn (thread-arena fragmented freelists ~100 MB+),
TrackAlbumMap/identify-queue/watcher-baseline/sqlx live
caches (~50 MB), and sqlx mmap (~25-30 MB). This step owns
tags + adapters + db-factory: global-allocator choice
(mimalloc or arena tuning — measured, not fashion),
tag-buffer reuse in the tag reader, sqlx pool mmap/cache
diet during scans, watcher-baseline streaming/bounding.
Target: post-100k-scan idle RSS ≤120 MB with rows 6/7/8
still in budget and fix-A durability intact.

Preserved behavior: scan verdicts, catalog contents,
identify semantics, read results; no API change; no
behavior change beyond memory footprint.

Intended changes: allocator + tag reader + adapter maps +
pool config only. Do NOT touch scan/sqlite_store.rs,
scan/coordinator.rs, scan/walk.rs (fixes A/D/E, reviewed
or under review), reads/, identify/, or migrations
(no new migration expected; allocator is a Cargo dep).

Acceptance tests: brief-first, minimal: row-5 RSS brief
(post-100k-scan idle ≤120 MB, production binary, same
driver shape); rows 6/7/8 + fix-A durability briefs must
keep passing; focused runs only (owner crash order).

Destructive ops: scratch state only; never prod.

Permitted external systems: none.

Non-goals: further scan-store tuning (fix E, done);
shutdown/reads/identify changes.

Completion criteria: row 5 re-measured PASS; review loop
clean (implementer != reviewer). If measurement proves
the budget unreachable without architecture change, STOP
and report with evidence (budget revisions are owner
decisions — do not park a FAIL silently, do not water
down the budget yourself).

## Handoff evidence note (fix-E fixups, 2026-10-04)

The composition in Scope above (74 MB floor, ~100 MB+
arenas, ~50 MB caches, ~25-30 MB mmap) is UNVERIFIED:
no maps rollup, malloc_info dump, or method note from
fix E exists in `/var/tmp/v3_s13*` or the repo (searched
2026-10-04; `/var/tmp/tagchurn*` is this step's own fresh
allocator probe work, not fix-E evidence). The row-5
FAIL verdict itself is honest (fix-E review re-verified
208-246 MB post-scan RSS on final code), but the split
is attribution without artifacts. Measure the floor and
the split yourself before trusting them.
