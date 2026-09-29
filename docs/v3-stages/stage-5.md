# Stage 5 manifest — Metadata providers + enrichment matrix

Status: COMPLETE 2026-09-29. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: provider clients behind traits with the verified policy table
(MB 1/s hard, LB 1/s + headers, AudioDB free 30/min, AcoustID 3/s, CAA
conservative ~1/s + backoff, Last.fm community 5/s + backoff),
per-upstream resilience (idempotency-aware retries, budget,
backoff+jitter, 429/503 + Retry-After/rate-limit headers honored;
401/403/404/400 keep distinct semantics), token buckets,
priority-queue slots (background jobs explicit background priority),
cache-aside with registered prefixes + invalidation, request
coalescing, enrichment aggregation with typed degradation results +
operation/source matrix. Providers: MusicBrainz (+BrainzMash
lifecycle), CAA/coverart stack, AudioDB, Last.fm (per-user, R7),
ListenBrainz, Discogs, iTunes, Preview (Deezer+iTunes), LRCLIB,
Archive, Wikidata, Geocoding, Skiddle, Ticketmaster, YouTube
(quota-file governor), GitHub (update check). Live-version quirk
handlers re-verified and cited.

Preserved behavior: all live-version-cited quirks where
provider-owned; degradation-recording as the error signal;
stale-cache-acceptable vs identity-critical routing; MB-dead fails
identity-critical only.

Intended changes: "non-2xx → 503" replaced by distinct internal
semantics; client-trait surfaces with conformance contract tests;
YouTube links stay standalone per R3.

Acceptance tests: per-provider contract brief (tolerant unknown
fields, required-identity-field decode failure, no default() empties);
limiter briefs per policy row; degradation-matrix briefs (dead
optional source → recorded + None, request succeeds; dead MB on
identity-critical → typed failure); quirk briefs per cited behavior.
E2E: album page enrichment with one provider down (degraded but
complete); lyrics fetch; events lookup.

Destructive ops: none.

Permitted external systems: recorded fixtures + scripted loopback
fakes only; no live network in tests; no quota'd/credentialed
state-changing probes.

Non-goals: acquisition paths using these clients (stage 7); remote
media servers (stage 6); UI (stage 12).

Completion criteria: matrix green; full gate set green; no
live-network default at runtime; review loop clean (zero
blocker/major).

## Completion record

Provider layer is live: 16 clients + shared core (limiters, retry,
cache, singleflight, slots, degradation, matrix) + enrichment
aggregation wired behind the stage-4 port seams, refresh loops spawned
in serve() with shutdown plumbing. 856 tests green (0 failed), full
gate set green (cargo test, clippy -D warnings, fmt --check,
contract-diff gate, cargo deny, cargo audit, container E2E). HTTP
surface unchanged (wiring-only diff; openapi.json byte-identical to
generator output; auth matrix untouched). No live-network default:
lyrics + ListenBrainz flag-gated fail-closed, all other roles
Unconfigured stubs.

Review caught and fixed five majors: ProviderLyrics failures now
record into the degradation context; LiveLrclib gated on a settings
flag with MemoryLyrics fallback; wikidata/YouTube/Archive tolerant
briefs added or repaired. Minors fixed: Last.fm/iTunes/Archive/
Preview/Wikidata honor Retry-After with fallbacks; LRCLIB 5xx mapped
retriable; MB/CAA date parsing delegated to core; BrainzMash 503 typed
as dead mirror; LB Remaining + Pacer priority documented as accepted
gaps; composite/timing-sensitive tests split or de-flaked.

Decisions recorded: two deliberate improvements over v2 kept
(Wikidata 404→None/else→Err instead of None-on-all-non-200; Preview
404→absence on all six legs); BrainzMash double-cooldown fixed by
giving the 429 arm sole ownership of note_cooldown; fixup-time
integrator death (runtime restart) recovered via a finish agent that
verified and completed the partial wiring.

Follow-ups (required before release, not new scope): retry driver
around ProviderLyrics when retry wiring lands; LB proactive
Remaining-merge; Pacer priority + Outcome→retriable bridge with the
first background caller; stage-7 acquisition consumes these clients.

## Accountability

Implementers: s5-core, s5-mb, s5-audio, s5-catalog, s5-events,
s5-enrich (disjoint slices), s5-integrate (died in a runtime restart
after landing wiring + adapters; work verified and kept),
s5-integrate-finish (trait unification, seam wiring, gates), s5-fixups
(review findings).

Reviewers (all different agents from the implementers): s5-review-code
(2 major / 9 minor / 5 nits), s5-review-tests (3 major / 7 minor /
3 nits), s5-review-pass2 (21/21 verified; 1 new minor found and fixed
inline by the orchestrator: BrainzMash double-cooldown).
All blocker/major/minor findings resolved and re-verified; no residuals.
