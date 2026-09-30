# Stage 8 manifest — Library engine (scan, identify, contributions)

Status: COMPLETE 2026-09-30. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: bounded blocking/CPU pool + filesystem scan
(walk/stat/discovery, dirty scopes, scheduler + filesystem watcher
with poll/batch settings, library-revision-poller, worker
supervision); identification queue (release/track/album-artist
identity, release pins, provider retrieval, audio
fingerprint/lookup, contributions verification worker + submission);
management preview/apply/undo; review operations.

Preserved behavior: all scanning, identification, tag, organization,
and maintenance semantics; identity product rules verbatim (release
pins hint-only; decision_source protection; manual/legacy survive
rescan/move; automatic revisable + retracted on contradiction;
retirement aliases kept); archive safety; track-appearance rules
(no new artists).

Intended changes: un-rolled states → durable machines; hot
read/write paths across pool boundaries; legacy validator + classic
job model retired; cleanup semantics per phase.

Acceptance tests: scan-rate + no-op purity briefs on stage-1 corpora;
preview-seal brief; publisher crash-injection brief per phase;
preservation brief; identity briefs; E2E library suites.

Destructive ops: scan/identify/poller/watcher/organize NEVER write
music files (test-pinned no-op purity); publisher writes ONLY under
sandbox roots.

Permitted external systems: scripted MusicBrainz/AcoustID fakes.
Real-audio corpora, no live network.

Non-goals: compat shims (stage 9); admin batch tooling (stage 10);
frontend (stage 12).

Completion criteria: matrix green; full gate set green; review loop
clean (zero blocker/major).

## Completion record

Library engine is live: 5 slices (scan, tags, identify, publish,
contrib) wired behind one `LibrarySetup` bundle with real library
roots (replacing the stage-6 provisional root in the stream gateway),
loop spawns + startup reconciliation in serve(). 1492 tests green (0
failed), full gate set green (cargo test, clippy -D warnings,
fmt --check, contract-diff gate, cargo deny, cargo audit, container
E2E). Auth matrix covers all 13 new routes (bidirectional); openapi +
.d.ts regenerated and in sync. New deps (lofty, symphonia stack,
rusty-chromaprint) deny-clean; Dockerfile builder gains cmake with
justification + cleanup.

Review caught and fixed three blockers (WMA in scan discovery list,
WMA in planner capability contract, full-decode probe on the async
runtime) and six majors (publish write-lease threading, QuietReconfirm
reachability, dead-gate wiring, absolute-path leak in collision rows,
symlink checks on compensate/cleanup/recovery paths, recovery fsync),
plus 13 minors across both reviews.

Decisions recorded: archive validator + automatic eligibility + poller
loops wired where real paths exist, documented where none do; remote
reads stay direct-only (carried from stage 6); tags slice needed one
orchestrator nudge to stop re-deriving lofty internals and land on the
stage-1 verdicts; two integrators + one fixup agent died in runtime
restarts — all recovered via finish agents that verified partial work.

Follow-ups (required before release, not new scope): stage 9 compat
shims consume the scan catalog + identity stores; stage 12 adds
management UI callers on the frozen preview/apply/undo shapes.

## Accountability

Implementers: s8-scan, s8-tags, s8-identify, s8-publish, s8-contrib
(disjoint slices), s8-integrate (died in a restart after wiring +
openapi + journey), s8-integrate-finish (died in a restart at deny
stage), s8-integrate-finish2 (verified + completed, zero edits needed),
s8-fixups (died in a restart after applying most findings),
s8-fixups-finish (verified + completed 2 remaining gaps).

Reviewers (all different agents from the implementers): s8-review-code
(3 blocker / 6 major / 5 minor / 4 nits), s8-review-tests (8 minor /
4 nits), s8-review-pass2 (22/22 verified; 1 new minor fixed inline by
the orchestrator: publish_tick moved to spawn_blocking).
All blocker/major/minor findings resolved and re-verified; no residuals.
