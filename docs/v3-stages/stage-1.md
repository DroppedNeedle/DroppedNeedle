# Stage 1 manifest — Skeleton, CI, Docker, contract pipeline, must-pass spikes, fixture capture

Status: COMPLETE 2026-09-28. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: Rust workspace layout (domain/application/ports/adapters; explicit
AppState + constructor injection + trait fakes), Axum boot
with `/health`, `rust-toolchain.toml`, Dockerfile + compose (single-process
invariant), CI (test, clippy,
fmt, audit, license check, generated-TS diff), utoipa → OpenAPI →
openapi-typescript pipeline skeleton (utoipa-lag hatch armed: pin Axum /
ts-rs fallback), error-envelope + 5xx-leak middleware,
tracing + correlation IDs, outbound HTTP factory + provider-policy table
skeleton, workload/budget harness on reference fixtures (+ hatch-3 2x-peak
replay harness), large-corpus fixture
(100k-track API budget), v2 behavior-fixture capture before deletion (real-audio fixtures via
independent tooling only, never tagger round-trip);

Preserved behavior: `/health` semantics; error envelope
`{"error": {code, message, details}}` with SCREAMING_SNAKE codes; v2 rate-limit
numbers as initial provider-policy rows (re-verified table from the live
contract); all v2 guard-test concepts earmarked as brief inputs.

Intended changes: new workspace (no ported logic yet); stage-1 must-pass
spikes, all blocking their dependent stages: (a) fpcalc A/B of
rusty-chromaprint (both presets + base64) on ≥10 real tracks, preset picked
and AcoustID match behavior documented; (b) Opus decoder spike
(symplica-adapter-libopus first choice) green or escape hatch tripped;
(c) lofty save-wrapper design (pre-save unencodable scan incl. TXXX:WORK,
post-save re-read verify, unknown-frame accounting via native tags) +
upstream WORK→TXXX issue/PR filed; (d) ADTS decode-based duration fallback + AAC-tag read gap
accept-or-mitigate (guardrail check — no v2-parity evidence either way);
(e) WMA cut implemented as unrecognized extension (no ASF code, no fixture).
Capture v2 fixtures: scan corpora + decisions, identity decisions, compat
traffic (self-golden per compat §4.1), acquisition mock transcripts,
publisher failure scenarios. Resolve the 100k-track API p95 budget TBD.

Acceptance tests (minimal briefs): boot-ready brief; health-shape brief;
envelope-shape brief; 5xx-leak brief (fixed strings + correlation ID, no
paths/hosts); TS-diff gate brief (drifted generated file fails CI);
must-pass spike briefs (fpcalc-equivalence, Opus decode, save-wrapper
sentinel preservation incl. CUSTOM_KEEP, ADTS duration). E2E: fresh
container boot → healthy → clean SIGTERM shutdown.

Destructive ops: none (no migrations, no file mutation; spike crates live in
/tmp, never the repo).

Permitted external systems: none at runtime; recorded read-only metadata
probes only for fixture capture (per network policy). No live
SABnzbd/slskd/Lidarr/Newznab probes.

Non-goals: any `/api/v3` business endpoint; persistence schema (stage 2);
audio pipeline integration (lands with library engine); compat shims.

Completion criteria: CI green on every gate; image boots cold ≤1.5 s / warm
≤1.0 s / RSS ≤80 MB idle (first budget check); all five must-pass items
closed or hatch-tripped with a recorded decision; v2 fixtures committed;
100k-track budget set; suite-speed budget recorded (slow suite = bug);
review loop clean.

## Completion record

All five must-pass items closed, no hatch tripped:

- (a) fpcalc A/B: PASS. rusty-chromaprint 0.3.0 bit-exact vs fpcalc 1.5.1
  on identical 11025 Hz mono PCM (15/15 tracks, both presets); preset pick
  test2; base64 must be URL_SAFE_NO_PAD; v3 pipeline prescribed (symphonia
  decode, f64 downmix, rubato cubic-256 to 11025, truncate to the 120 s
  window). Live AcoustID lookup deferred to the key holder (one command,
  recorded in the spike report); transcode calibration shows our prints sit
  1-2 orders of magnitude inside the match band.
- (b) Opus decode: PASS. symphonia-adapter-libopus 0.3.0 decodes the
  fixture to exactly 28800 s16 samples (0.3000 s, mutagen oracle),
  deterministic; synth sine round-trip sample-exact. v3 build image needs
  cmake + a C toolchain for the bundled libopus.
- (c) lofty save-wrapper: PASS. Prototype wrapper (native in-place edits,
  pre-save unencodable scan, temp-copy save, post-save re-read verify
  against a byte-level frame inventory, ID3 version pin) preserves
  TXXX:WORK, unknown frames, multi-values, description case, vendor
  string, pictures, and CUSTOM_KEEP on MP3/FLAC/OGG/Opus/M4A (mutagen
  oracle 83/83 green); refuses loudly on mixed v2.3, empty Vorbis values,
  and any out-of-edit delta. Upstream issue filed:
  https://github.com/Serial-ATA/lofty-rs/issues/732 (OPEN, verified).
- (d) ADTS duration: proven. symphonia container duration 10.07x wrong;
  demux-count fallback (packets x 1024 / rate) matches full decode
  exactly at a fraction of the cost. AAC-tag read gap accepted as
  read-only restriction for now (no v2-parity evidence either way).
- (e) WMA cut: implemented. `.wma` is not a recognized extension
  (`server/src/media.rs`); no ASF code, no fixture, brief pinned.

Budgets resolved (durable record: `tools/perf-harness/BUDGETS.md`):
100k-track API p95 split into standard reads ≤10 ms and full-text search
with results ≤250 ms; suite-speed ceilings recorded (backend warm ≤120 s,
cold ≤600 s, frontend-server ≤120 s, frontend-client ≤300 s unmeasured,
full gate set ≤15 min wall, parallel CI assumed).

First budget check (reference hardware, skeleton floor not prediction):
cold boot 0.02-0.03 s, warm boot 0.02-0.03 s, idle RSS 1.6-1.8 MiB,
SIGTERM shutdown 0.30-0.34 s exit 0. Full gate set green on the
orchestrator's own runs: cargo test (23 passed), clippy -D warnings,
fmt --check, contract-diff gate, cargo audit, cargo deny, container E2E.

Full spike reports (local working copies, durable disk):
`.dev-notes/Plans/RustPort/stage1-spikes/`.

## Accountability

Implementers (a runtime restart killed 4 of the first 6 workers; finish
agents verified and completed their partial work): s1-skeleton /
s1-skeleton-finish, s1-fpcalc-ab / s1-fpcalc-finish, s1-opus-decode,
s1-lofty-wrapper / s1-lofty-finish, s1-fixture-capture,
s1-perf-harness / s1-harness-finish.

Reviewers (all different agents from the implementers): s1-review-skeleton
(0 blocker / 1 major / 4 minor), s1-review-data (0 / 3 / 4),
s1-review-spikes (0 / 0 / 6), s1-review-pass2 (0 / 0 / 1).
Fix-ups: s1-fix-skeleton, s1-fix-data, s1-fix-guardport.
All blocker/major findings resolved and re-verified; residual minors
recorded below. One pass-1 minor declined with evidence (nested
`server/.gitignore` already excludes `target/`; `git add -n` stages 27
files, verified by orchestrator and pass-2 reviewer).

Residual minors recorded (fixed ones noted): spike-report prose nits
(fpcalc format-count wording, ladispute bits_eq 4th decimal, lofty case
count 13 vs 14, opus em-dashes — polish only, /tmp-local); real-music
Opus decode untested by the A/B (stage 8 must add a real-music Opus
fixture); AcoustID confirmation command needs the submit-ready rusty b64
string emitted at stage 8; check.sh diff display (fixed); middleware 503
preservation (fixed + briefed); percentile docstring (fixed); replay.py
+x (fixed); SESSION placeholder dupe (fixed); caps_dead naming (fixed);
guard default-port bypass (fixed + probed).

Stage-8 follow-ups (not new steps, inputs to the library-engine stage):
symphonia MP3 decoder defect on one file (PCM correlation 0.18 vs
ffmpeg; root-cause before trusting symphonia MP3 blindly); widen the
ADTS corpus beyond the single .aac fixture; OGG/Opus packet-payload
audio comparison; MP4/APE native empty-value probes; WAV+RIFF INFO
write path; AAC tag story; one live AcoustID confirmation lookup by
the key holder.
