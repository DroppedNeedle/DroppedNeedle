# Stage 12 manifest — frontend migration to /api/v3

Status: COMPLETE 2026-10-04. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: clean-slate frontend on native `/api/v3` (Q1): generated
transport (openapi-fetch client + endpoint registry + registry
scan), query migration in two slices (core domains, then flows),
missing UIs per R6/R9/R10, profile seam cut to v3, IndexedDB
cache buster so v2 cache can never poison v3 reads, CI E2E flows
against the v3 backend, and a catalog-gap step wiring the
surfaces the slices missed (album detail, SearchSuggestions,
plus four backend gap routes: task reimport, library
album-match, random, discovery).

Preserved behavior: every v2 UI surface keeps working against
v3 with identical user-visible behavior; RequestCard and the
request/approve flows survive; per-user Last.fm (R7) stays
per-user; playback goes through one gateway.

Intended changes: v1 API client and v1 query modules removed
(wire-not-delete inside the migration: dead v1 pages deleted,
live v3 modules wired); old integration-coverage spec replaced
by contract-coverage (195 routes) + registry scan; persisted
cache keys gain the userId segment (cross-user leak closed);
single TanStack surface per R10.

Acceptance tests: contract-coverage spec (every native route
has a query or an explicit exemption row); persistedCache +
userKeySegment specs; RequestCard + SearchSuggestions specs;
6 CI E2E flows green; full server + client vitest projects
green; svelte-check 0 errors; eslint clean.

Destructive ops: none against live data (frontend-only
migration; backend gap routes additive).

Permitted external systems: none (scratch backends in tests).

Non-goals: visual redesign (behavior migration only); v2
Python removal happens at cutover (stage 13).

Completion criteria: no frontend import touches /api/v1;
full gate set green; review loop clean.

## Completion record

The frontend runs on `/api/v3` end to end: `lib/api/v3/`
transport, migrated queries under `lib/queries/`, R6/R9/R10
UIs, profile seam on v3, and 6 CI E2E flows proving
login→browse→request→approve→download→play against the Rust
backend. Backend gained four additive gap routes (task
reimport, album match, random, discovery) with auth-matrix
rows (reimport Admin, rest User). Zero `importOriginal`
remains in specs: the browser suite hung on those mocks, so
all 38 were converted to plain-object fakes (authStore fake
keeps LAST_USER_ID_KEY).

Gates green: backend 2048 tests + clippy -D warnings + fmt
--check + openapi check + cargo deny + cargo audit +
container E2E (backend tree unchanged since that gate);
frontend svelte-check 0 errors, eslint clean, server project
1593 passed, client project 1136 passed (169 files). One
transient single-test failure appeared in one client run and
passed on every rerun; recorded as flaky, not waived.

Review found 1 blocker + 6 majors (code) and 2 blockers +
3 majors (tests) at pass 1 (509 svelte-check errors, dead v1
pages, removed RequestCard, dual playback gateway, 13-fail
DownloadQueries, 0/6 E2E), all fixed across four fixups plus
the catalog-gap step; pass-2 residuals (album detail +
SearchSuggestions unwired, 2 red suites) fixed and closed by
a final verifier pass with zero open findings.

Decisions recorded: wire-not-delete for v3 modules (dead v1
pages deleted, live modules wired); R10 single TanStack
surface; plain-object mock pattern with `fakeClient` naming
(the eslint queryClient rule also matches mock factories);
SvelteSet for reactive sets; `_` prefix for unused vars.

Follow-ups (required before release, not new scope): stage 13
runs the parity gate and cutover on the real instance.

## Accountability

Implementers: s12-transport, s12-queries-core,
s12-queries-flows, s12-uis (disjoint slices), s12-integrate
(profile seam + E2E flows), s12-fixups-types,
s12-fixups-wiring, s12-fixups-restore, s12-fixups-e2e
(pass-1 findings), s12-gap-catalog (died in a restart) +
s12-gap-finish (catalog-gap step), s12-fixups-residual
(pass-2 residuals), s12-fixups-clientmocks (died in a
restart) + s12-fixups-clientmocks-finish (38 plain-object
mock conversions, full client suite green).

Reviewers (all different agents from the implementers):
s12-review-code (fail: 1 blocker / 6 major),
s12-review-tests (fail: 2 blocker / 3 major),
s12-review-pass2-code, s12-review-pass2-tests,
s12-verify-final (pass, 0 open findings).
All blocker/major/minor findings resolved and re-verified; no residuals.
