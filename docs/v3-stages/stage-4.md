# Stage 4 manifest — Native read APIs

Status: COMPLETE 2026-09-29. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: clean-slate `/api/v3` read surface in five slices — library
(albums/artists/tracks/lyrics/stats/recently-added/genres plus
local-library browse), unified search (search/suggest/per-bucket
drill-down plus one enrich-batch POST), discover (discover/queue/radio/
batches/home shelves/now-playing), collections (playlists/favorites/
follows/approvals/edition pins), platform (covers/version/wrapped).
95 new ops (46 → 141 total), all inside the session gate except the
key-authed wrapped share routes.

Preserved behavior: deny-by-default sessions; error envelope
`{"error": {code, message, details}}` with SCREAMING_SNAKE codes; fixed
generic 5xx bodies with request id only; 401/403-or-404 matrix on every
gated route; release pins steer the edition-search hint and never become
catalog identity evidence; no WMA anywhere.

Intended changes: clean-slate v3 read shapes (no v1 shape compat); query
rejections render the envelope via ValidQuery wrappers; provider-backed
reads run scripted fakes behind port seams until stage 5; discover/home
refresh loops built but explicitly deferred to stage 5 (static scripted
ports need no refresh yet).

Acceptance tests: brief-first suites per slice (`reads_library`,
`reads_search`, `reads_discover`, `reads_collections`,
`reads_platform`); auth matrix extended to all 95 new (method, path)
pairs in both directions; trusted-tier E2E (edition-pin admitted,
approvals still 403); stateful playlist lifecycle journey through the
real app (POST → add → GET → DELETE → GET 404); contract-diff gate over
openapi.json + openapi.d.ts.

Destructive ops: none (reads only; scratch DBs in tests).

Permitted external systems: none at runtime (provider fakes/seams in
tests; no live provider calls).

Non-goals: real metadata providers (stage 5); compat routers (stage 9);
write/settings/admin surfaces beyond collections (stage 10); frontend
migration (stage 12).

Completion criteria: matrix green; full gate set green; review loop
clean (zero blocker/major).

## Completion record

Native reads are live behind deny-by-default: 95 mounted ops, 511 tests
green (0 failed), full gate set green (cargo test, clippy -D warnings,
fmt --check, contract-diff gate, cargo deny, cargo audit, container
E2E). Auth matrix covers all 95 reads pairs with bidirectional
fail-by-name; posture passes assert no 5xx; openapi.json is
byte-identical to generator output.

Review caught and fixed six majors: 13 bare Axum Query sites now use
ValidQuery so malformed queries render the envelope; hardcoded joke
video id removed from unconfigured YouTube search; fabricated Deezer
provider attribution removed from previews (empty until stage 5);
trusted-tier E2E added for the curator middle tier; stateful playlist
journey added through the real app. Minors fixed: 400 codes aligned on
INVALID_INPUT, utoipa error responses added, discover session extractor
aligned, stage-5 boundary headers in all reads suites, stale
unmounted-doc headers corrected, kitchen-sink tests split per route.

Decisions recorded: refresh loops deferred to stage 5 with an explicit
note in main.rs + refresh.rs (documented, not dropped); wrapped share
routes live outside the session gate with key-only auth plus matrix
rows; FakeContent fixture shelves accepted as the documented stage-4
stand-in (only false provider claims removed).

Follow-ups (required before release, not new scope): stage 5 wires real
providers behind the ports seams and spawns the refresh loops; stage-9
compat routers reuse these read handlers.

## Accountability

Implementers: s4-library, s4-search, s4-discover, s4-collections,
s4-platform (disjoint slices), s4-integrate (lib/app/state/docs/Cargo
wiring, openapi regen, matrix extension), s4-fixups (review findings).

Reviewers (all different agents from the implementers): s4-review-code
(4 major / 3 minor / 3 nits), s4-review-tests (2 major / 4 minor /
1 nit), s4-review-pass2 (11/13 verified; 2 residual minors fixed inline
by the orchestrator: stale src headers, one test split).
All blocker/major/minor findings resolved and re-verified; no residuals.
