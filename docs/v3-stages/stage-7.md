# Stage 7 manifest — Acquisition (requests, downloads, indexers, free music, drop import, wanted, Lidarr import)

Status: COMPLETE 2026-09-29. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: request intake (album/track/batch/edition-acquire + batch-cancel,
active/history/wanted/approvals incl. auto-download + personal-mix flows
+ refresh route, quota + role gating); durable download operations as
SQLite state machines (idempotency keys, attempts, timestamps, terminal
states, startup recovery); slskd + SABnzbd + Newznab + Prowlarr clients
with mock-server contracts + live-quirk handlers; search fan-out jobs;
free-music + drop-import as registered durable operations; quarantine;
held-import retry; orphan/stale reconciliation; watchdog/auto-retry;
request-status-sync; follow-new-release-poll; wanted watcher; background
upgrade sweep; Lidarr read-only import → follows; Spotify import (OAuth
url/callback + playlist routes, `spotify:import` job, settings rows);
acquisition health smoke (Free OR slskd OR Usenet) + per-source release
gates.

Preserved behavior: per-album and per-track acquisition; quality
tiers/recipe/timeouts/quotas/retention from download_policy;
recycle-bin prune; approval semantics (user waits, trusted/admin auto);
conflict/duplicate rules; journal-ownership proofs; DN-folder orphan
deletion (#131).

Intended changes: task sprawl becomes durable state machines +
registered ephemeral loops; drop-import/free-music/events-kick
registered; legacy rebuild/swap machinery gone; R10 endpoints gain UI
callers in stage 12.

Acceptance tests: request-lifecycle + quota/role briefs; durability
briefs (restart resumes without duplicate fetch; orchestrator
failover); per-client contract briefs against mocks; quarantine/orphan
briefs; Lidarr→follow + Spotify + per-source gate briefs. E2E:
request → approve → land; wanted → candidate → auto-download; drop →
quarantine → resolve.

Destructive ops: staging + quarantine writes in sandbox only; orphan
reconcile deletes DN-named folders ONLY when no journal owns them;
never touches the real library.

Permitted external systems: in-repo mock slskd/SAB/Newznab/Prowlarr/
Lidarr servers ONLY. No live contact in tests, ever.

Non-goals: library scan/identify of landed files (stage 8 consumes
landed files); player UI (stage 12).

Completion criteria: matrix green; full gate set green; migration 0002
applies cleanly; review loop clean (zero blocker/major).

## Completion record

Acquisition is live: 6 slices (requests, downloads, slskd, usenet,
flows, imports) unified behind one dispatch/worker/wiring layer, with
migration 0002 (download idempotency keys) as the first migration
since baseline. 1270 tests green (0 failed), full gate set green
(cargo test, clippy -D warnings, fmt --check, contract-diff gate,
cargo deny, cargo audit, container E2E). Auth matrix covers all new
routes; openapi.json + .d.ts regenerated and in sync. Spotify callback
is the only public route (state-token-identified + rate-limited,
matching v2); Lidarr surface is GET-only (zero writes).

Review caught and fixed one durability blocker (crash-retry
double-enqueue → pre-enqueue dedup on deterministic job names) and
ten majors (dispatch idempotency keys, terminal-out store guard,
quarantine wiring, edition-pin-preserving retries, recycle-bin root
guard, secret-redacting Debugs, slskd 429 retry, spawn_blocking for
fs/sqlite, loop Stopped states, batch-approve/reject briefs), plus 23
minors and nits across both reviews.

Decisions recorded: quarantine consult is live for soulseek at
enqueue; the usenet consult is deferred until release identity rides
the handle (failover records job-name rows for audit until then —
documented in quarantine.rs module docs); claimed idempotency keys
release on insert failure so repeats dispatch fresh; local music root
stays provisional until stage 8; integrator needed one orchestrator
nudge to land wiring over seam-perfecting.

Follow-ups (required before release, not new scope): wire the usenet
quarantine consult when handles carry release identity; stage 8
consumes landed files (scan/identify) and supplies real library roots;
stage 12 adds R10 UI callers.

## Accountability

Implementers: s7-requests, s7-downloads (incl. migration 0002),
s7-slskd, s7-usenet, s7-flows, s7-imports (disjoint slices),
s7-integrate (seam unification, wiring, matrix, openapi, journeys),
s7-fixups (review findings).

Reviewers (all different agents from the implementers): s7-review-code
(1 blocker / 9 major / 15 minor + nits), s7-review-tests (1 major /
4 minor / 2 nits), s7-review-pass2 (30/32 verified; 4 remains fixed
inline by the orchestrator: addurl transport scrub on both SAB sites,
usenet-consult doc note, stale recovery doc, dangling-key release).
All blocker/major/minor findings resolved and re-verified; no residuals.
