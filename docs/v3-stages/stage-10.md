# Stage 10 manifest — Settings, admin, users, jobs, plugins, system, events, wrapped

Status: COMPLETE 2026-10-03. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: remaining `/api/v3` settings surface (preferences incl.
section-prefs, scan schedule/watcher, advanced allowlist semantics,
all connection sections with verify endpoints, download
policy/clients, wanted, source priority, indexers, prowlarr,
lidarr-import config, events, wrapped, version/update-check,
security, OIDC, connect-apps, library roots+policies, management
profiles, musicbrainz/brainzmash lifecycle, scrobble,
lastfm_settings per-user, primary-source, free-music, get-it,
plugins enable+settings); admin UX HTTP routes (backup
list/run/restore-report/pre-upgrade-auto-backup N=5, cache
stats/clear, queue/provider stats, user admin incl. quotas,
checkpoint observability on health); job-registry mechanism +
stage-10-owned loops only (registry + WAL checkpoint, presence,
precache, plugin ticks on store, events-kick registration fix,
personal-mix refresh loop, Navidrome playlist-sync route+loop,
events watcher); plugins host (install/update/uninstall/ext
proxy/panel.js, secret-flagged values encrypted); R9
ListenBrainz/scrobble settings backend.

Preserved behavior: whole-config replace import semantics (stage
11 executes); D15 transient hints recomputable; D16 `_internal`
cursors derivable; per-section verify-then-save; settings-save
side effects (checkpoint/provider rebuild, event-kick).

Intended changes: dropped sections rejected by validator
(sync/local-files/home/_legacy_lidarr + vestigial mirrors); mask
normalization enforced; plugin tick durability redesigned (state
in the database); dead `settingsLocalFiles{,Verify}` builders
deleted with D3; A:324 path-mapping drop named.

Acceptance tests: per-section round-trip + verify brief;
dropped-section rejection brief; mask briefs (incoming mask keeps
ciphertext; raw getter decrypts); job-registry briefs (each loop:
interval, jitter, recovery, no-hot-loop, virtual-time); plugin
tick durability brief (state survives restart via store);
backup-UX briefs. E2E: admin edits indexer, verify, save,
provider rebuilds; admin runs backup, lists, restores to empty
dir; plugin install, tick fires, persisted state visible.

Destructive ops: settings writes fixture-scoped; prune jobs
(store/retention only) run against sandbox data with retention
pins test-verified.

Permitted external systems: none in tests (verify endpoints hit
fakes/mocks); read-only recorded probes only when re-verifying a
quirk.

Non-goals: export/import file format execution (stage 11);
settings UI + R9 UI (stage 12).

Completion criteria: every kept section round-trips; every
background loop registered, cancellation-responsive, and
recovery-tested; review loop clean.

## Completion record

Settings, admin, jobs, and plugins are live: 4 slices wired
behind `SettingsSetup`/`JobsSetup`/`PluginsSetup` bundles with
the settings HTTP router mounted, verify endpoints per
connection section, save fan-out (cache invalidation + events
kick), and owned loops spawned in serve(). 1878 tests green (0
failed), full gate set green (cargo test, clippy -D warnings,
fmt --check, contract-diff gate, cargo deny, cargo audit,
container E2E). Migration 0003 adds durable plugin tick state;
scrobble prefs + ListenBrainz links use the stage-2 tables.
OpenAPI snapshot + TypeScript regenerated and in sync.

Pass-1 failed on both reviews and was right to: the settings
slice landed without its HTTP surface, without verify
endpoints, and without tests. The fixup round built the missing
router (admin-gated, dropped sections 410), all verify
endpoints, masked-save echoes, the advanced/library service
layer, MusicBrainz save effects, validated section-prefs saves,
and the full settings brief suite — plus the platform fixes
(playlist-sync admin gate, durable tick store, SQLite scrobble
stores, precache trigger, plugin shadow deletion). 3 blockers +
9 majors (code) and 4 blockers + 1 major (tests) fixed, each
with a dedicated test.

Decisions recorded: actual DB restore is a stage-11 offline CLI
by design, so backup UX scope is run→list→report; connection
sections without cached reads need no invalidation (clients
build per request); precache trigger is a post-sync hook.

Follow-ups (required before release, not new scope): stage 11
executes whole-config replace import; stage 12 adds settings UI
+ R9 UI callers on the frozen shapes.

## Accountability

Implementers: s10-settings, s10-admin, s10-jobs, s10-plugins
(disjoint slices), s10-integrate (wiring + openapi + journeys),
s10-fixups-settings (missing HTTP surface + verify + masks +
service gaps + settings briefs), s10-fixups-platform
(durability + gates + triggers + shadow deletion).

Reviewers (all different agents from the implementers):
s10-review-code (FAIL pass-1: 3 blocker / 9 major / 7 minor /
2 nits), s10-review-tests (FAIL pass-1: 4 blocker / 1 major /
3 minor / 2 nits), s10-review-pass2-code (pass, 0 open
blockers/majors), s10-review-pass2-tests (pass, 0 open
blockers/majors; 1 new minor fixed inline by the orchestrator:
live fan-out dispatch brief).
All blocker/major/minor findings resolved and re-verified; no residuals.
