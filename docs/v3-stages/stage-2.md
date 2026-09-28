# Stage 2 manifest — Persistence foundation, migrations, config/secrets core

Status: COMPLETE 2026-09-28. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: `sqlx::migrate!` framework; migration 0001 = consolidated v2-final
schema with ONE owner per table (field-by-field diff of the 11 dual-owner
tables; D13 legacy download/swap + D14 legacy library tables excluded at
baseline); SQLite runtime factory per S0-SQLITE (pool 1 writer + 7 readers,
pragma set, checkout guard, FK ON, checkpoint service with TRUNCATE reclaim +
observability, writer two-lane queue foreground-burst-8, tx duration limits,
no-await-inside-write-tx,
local-filesystem-only boot check, online-backup mechanism +
checkpoint-observability recording (HTTP routes land in stage 10); durable-worker
fabric spec (wakeup channels scan/identification/operation/contribution, job
registry, DurableWorkWakeups) consumed by stages 7/8/10;
typed config sections + env tier + exact-match mask-sentinel normalization
(D12; AudioDB/plugin secrets encrypted at rest; jellyfin/LB/youtube mask gaps
closed).

Preserved behavior: single-file WAL discipline; GH-293 checkpoint calibration
(30 s cadence, 60 s reader bound, 64/16 MiB waters, background-only
backpressure); `fold()` search semantics; writer fairness; PriorityWriteLock
burst-8; v1 backup verified-then-rotate UX; YouTube quota JSON file semantics.

Intended changes: ratchets replaced by versioned forward-only SQL (no
down-migrations; rollback = restore backup); FK enforcement ON with
export/import gate (`foreign_key_check` before cutover); checkpoint
observability exported (latest record on admin health); `last_seen_at`
throttled session writes (auth-owned, persisted here); D1 (no lidarr
tombstone/bytecode), D6/D7/D8 (vestigial/dual Settings + jellyfin_url mirror
gone), D9 hygiene.

Acceptance tests: migration idempotency brief (fresh + re-run +
`foreign_key_check` + `integrity_check`); boot-assertion brief (user_version
mismatch refuses to serve); checkpoint-TRUNCATE brief (dead WAL reclaimed,
shutdown leaves no WAL); writer-fairness brief; busy-path brief (503 +
Retry-After, no spin); backup/restore round-trip brief incl. empty-target
refusal; mask-sentinel exact-match brief per secret; local-filesystem refusal
brief. E2E: fresh boot → write workload → backup → restore into empty dir →
verify.

Destructive ops: none on user data (scratch DBs only). Migration 0001
defines an empty-data baseline; no live v2 DB is touched.

Permitted external systems: none.

Non-goals: business tables' service logic; export/import of v2 data
(stage 11 owns the importer; 0002 lands there); settings HTTP surface
(stage 10); restore CLI (stage 11).

Completion criteria: full chain greenfield + re-run clean; WAL bounded under
scan-simulation writes (budget check); DB growth baseline measured toward
≤5 KB/track; review loop clean.

## Completion record

Migration 0001 is a single forward-only file: 181 tables, 181 indexes, 43
triggers, `user_version = 1`. All stage-2 briefs pass (120 tests total in
the workspace, up from 23). Full gate set green on the orchestrator's own
runs: cargo test, clippy -D warnings, fmt --check, contract-diff gate,
cargo deny, cargo audit (one lockfile-only rsa advisory ignored with a
recorded reason), container E2E.

Decisions and deviations recorded during review:

- The dual-owner count is 12, not 11: the trace's own list expands to 12
  and a full backend scan confirms exactly those 12 (plus 2 test-only
  dupes). All 12 merged correctly (7 identical collapses, 4 store-wins on
  auth_users FKs, 1 discovery+native merge).
- Review caught two live drops and both were restored: `local_tracks`
  was missing the `tag_album_*` ratchet columns, and `DownloadPolicy`
  was missing live `flac_mp3_only` (default true + recipe save refusal
  restored; the v2 cross-check is live, not a no-op).
- The writer lane owns one dedicated rusqlite connection while the sqlx
  pool carries 7 readers: still honestly 1W+7R, and it makes
  awaiting inside a write tx structurally unrepresentable. rusqlite
  alongside sqlx is justified (online backup API, fold() registration,
  progress-handler abort); pragma sets match exactly.
- The FS check is a deny-list (corrected lustre magic plus FUSE, FUSEBLK,
  GFS2, OCFS2); container-local overlayfs passes by design.
- Backup steps wait 10 ms instead of v1's 250 ms: the 250 ms was the
  Python library default, not an owner calibration.
- Crypto: ChaCha20-Poly1305, fresh nonce per encryption, `v3:` envelope,
  0600 + fsync key file, decrypt fails closed, no legacy passthrough
  outside the stage-11 importer. v2's silent key-loss corruption becomes
  a loud failure.
- The Last.fm admin-global deletion stands: R7 is an explicit owner
  decision that necessarily overrides the export generic 1:1 line for
  that section. Stage-11 fallout is flagged in code.
- Lanes are bounded (128 per lane, full fails fast to 503 + Retry-After);
  the read pool pins query_only after boot migrations.

Follow-ups owned by later stages (not new steps): checkpoint-loop boot
wiring + pre-upgrade-backup hook (stage 10); AppConfig wiring of the
remaining 19 deployment vars (consumer stages); stage-11 carve-outs
(lastfm, indexer pairing, sync_frequency one-shot); v2 DB growth
baseline (3.85 KB/track post-initial) is the number to beat toward the
≤5 KB/track budget.

## Accountability

Implementers (one runtime attempt died of an infra idle-timeout with
zero writes and was respawned): s2-schema, s2-runtime / s2-runtime-retry,
s2-config.

Reviewers (all different agents from the implementers): s2-review-schema
(1 blocker / 0 major / 1 minor), s2-review-runtime (0 / 2 / 12),
s2-review-config (1 / 1 / 5), s2-review-pass2 (CLEAN).
Fix-ups: s2-fix-schema, s2-fix-runtime-retry (first attempt also died of
an infra idle-timeout with zero writes), s2-fix-config, s2-fix-fold.
All blocker/major/minor findings resolved and re-verified; no residuals.
