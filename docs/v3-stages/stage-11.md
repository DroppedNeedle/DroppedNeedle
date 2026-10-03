# Stage 11 manifest — v2→v3 export/import + dev tooling

Status: COMPLETE 2026-10-03. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: versioned export envelope (`format_version: 1`,
users/settings/follows/approvals, semantic keys, sealed secrets
under argon2id+xchacha20poly1305 with operator passphrase,
reserved sections ignored-with-warning); exporter (v2 key decrypt
incl. legacy-plaintext passthrough, refuses missing key file);
standalone validator (error/warning taxonomy); importer (parse →
unlock → settings replace → users/providers → re-encrypt (except
recovery-code hashes, stored verbatim) → follows/approvals →
atomic commit → post-import rebuild; R8 one-shot
`sync_frequency → scan_frequency` import reading v2 config.json
directly, never the export stream; conflict + deleted-ID rules;
idempotent re-import); machine-readable report
(`OK`/`OK_WITH_DROPS`/failures, per-entity counts,
semantic-keyed items, counts-only secrets); dry-run parity;
migration 0004 = import application (plan said 0002; drift
recorded); dev-only tooling route for covers-debug (R11, never in
prod API).

Preserved behavior: Q2 preserved set exactly (accounts,
settings, follows; scan state/history/queues/jobs dropped); Q14
hash import; app-password survival (revoked rows exported
revoked:true); root-ID stability; `instance_id` verbatim;
indexers order; whole-config replace with `settings_defaulted`
reporting.

Intended changes: new tooling surface (CLI:
export/validate/import/dry-run/restore); v2 plaintext-at-rest
bugs closed at seal time (AudioDB/plugin); dropped-section
presence is a validator error (D-lists enforced).

Acceptance tests: migration-0004 upgrade brief (fresh + over
migrated-0003); envelope-briefs (version reject, required keys,
reserved ignore); validator briefs per error/warning rule;
conflict briefs (provider-binding, email/username nulling,
follow OR/min/max merge, approval-state most-permissive merge,
reviewer nulling); deleted-ID briefs (fail-closed refusal, never
invented); idempotency brief (re-import ⇒ all skipped_identical
on clean imports, zero DB writes always); atomicity brief
(failure ⇒ rolled back, incl. kill-mid-import recovery);
dry-run parity brief (counts equal real import);
passphrase-failure brief (zero writes); missing-v2-key refusal
brief. E2E: live v2 fixture dir → export → validator → dry-run
→ import → login with v2 password (rehashes) → follows intact →
compat client re-authenticates with surviving app password.

Destructive ops: importer writes ONLY to scratch v3 DBs in
tests; atomic commit verified by kill-mid-import recovery test.

Permitted external systems: none (file + DB only).

Non-goals: cutover execution on the real instance (stage 13);
UI for import (CLI + report JSON suffice).

Completion criteria: full pipeline green on a fixture v2
instance carrying every entity + conflict + deleted-ID case;
report machine-validated; review loop clean.

## Completion record

Export/import is live: `server/src/export/` (envelope, seal,
v2 reader with Fernet semantics matching v2's crypto.py),
`server/src/import/` (validator, pipeline, R8 carry, report),
`droppedneedle-tool` CLI (export/validate/import/dry-run/
restore), migration 0004, offline DB restore (stage-10
follow-up, CLI-only), and a dev-only covers-debug route
provably impossible in prod (flag + cfg mount + debug-only CLI
arg). 2021 tests green (0 failed), full gate set green (cargo
test, clippy -D warnings, fmt --check, contract-diff gate,
cargo deny, cargo audit, container E2E). Fixture v2 instance
carries every entity + conflict + deleted-ID case; the pipeline
E2E proves login/rehash, follows intact, and compat re-auth.

Review found 1 major + 8 minors + 5 nits (code) and 5 majors +
10 minors + 5 nits (tests), all fixed: fail-closed deleted-ID
decision recorded (spec §§2.4/8 contradicted; refusal +
operator repair won), all 15 validator codes pinned,
rejected-state merge + reviewer-name orphan decided and
briefed, mega-tests split one-behavior-per-brief, drop
counters covered, migration + wording drifts corrected.

Decisions recorded: re-import "zero writes" is DB-only
(config.json bytes rewrite on sealed secrets); dry-run applies
migrations + lock probe before deciding (documented);
`requested_at` min-merge is convergent (one spec line);
duplicate `code_hash` stays a mid-transaction internal error
(friendlier validator rule declined to keep the atomicity
brief stable).

Follow-ups (required before release, not new scope): stage 13
executes cutover on the real instance using this pipeline.

## Accountability

Implementers: s11-export, s11-import, s11-tooling (disjoint
slices), s11-integrate (one envelope source of truth + CLI
wiring + E2E proof), s11-fixups (died in a restart after most
findings), s11-fixups-finish (verified + closed 3 remaining
gaps + 1 doc correction).

Reviewers (all different agents from the implementers):
s11-review-code (pass with findings: 1 major / 8 minor /
5 nits), s11-review-tests (conditional pass: 5 major /
10 minor / 5 nits), s11-review-pass2-code (pass, 0 open
blockers/majors), s11-review-pass2-tests (pass, 0 open
blockers/majors).
All blocker/major/minor findings resolved and re-verified; no residuals.
