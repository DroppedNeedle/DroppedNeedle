# Stage 13 escape-hatch evaluation

Status: COMPLETE 2026-10-04. Branch `v3`. No product code changed.

Contract sources: root `AGENTS.md` (live v3 contract),
`.dev-notes/Plans/RustPort/stage0-misc.md` section (4) (the five
architecture escape-hatch triggers with measurable trip conditions),
and `.dev-notes/Plans/RustPort/00-decisions.md` Q9 (rollback is the
last v2 release with its own data dir; the media-mutation rollback
boundary must be specified before beta touches real libraries).

Method: focused `cargo test --test <file>` runs only, per the owner
crash order (no full suites). All runs used committed fixtures and
scratch databases under the system temp dir. No live services, no
prod data, no network. Toolchain: pinned Rust 1.89.0. One temporary
timing probe was created, run, and deleted in the same session; the
tree holds no trace of it (`git status` shows only this file plus
sibling workers' untracked docs).

Verdict summary: all five hatches NO-TRIP. Rollback boundary:
file-mutation irreversibility accepted (beta users must explicitly
accept it before beta touches real libraries).

## Hatch 1 - audio stack

Trigger recap: (a) more than 0 gapless or seek-offset correctness
failures on the golden-track corpus; (b) p99 transcode start over
750 ms for FLAC to 320k MP3 on reference hardware; (c) any
top-20-bytes fleet format undecodable without a new native
dependency.

What the tree actually built: the serve path never decodes through
Symphonia. Transcodes run through an `ffmpeg` sidecar with a
byte-pinned argv contract (`-ss` before `-i`, piped stdout,
`server/src/stream/transcode.rs`), ported from v2. Symphonia is
probe and fingerprint only. So the trigger's "Rust decode path"
premise holds for ingest, not for serving; the serving half of this
hatch evaluates the ffmpeg contract and the byte-range gateway.

(a) NO-TRIP (measured proxy). There is no file literally named a
gapless corpus; the executable proxy is the range-matrix plus
seek-offset briefs, all green in this session:
`stream_gateway` 35/35 (closed/open/suffix ranges, 416 set, HEAD
matrix, `wma_never_streams`), `stream_gateway_engine` 21/21
(including `seek_then_transcode_starts_at_offset`),
`stream_transcode` 25/25 (including
`start_offset_survives_only_on_transcode` and
`opus_argv_seeks_before_input`). Zero failures.

(b) NOT-MEASURABLE-PRE-RELEASE, leaning no-trip. The condition is a
p99 over a 7-day rolling median, which needs production traffic that
does not exist yet. A one-shot pre-release calibration is still
possible and still open: `ffmpeg` is absent from this container, so
not even a single-sample FLAC to MP3 start time could be taken here.
Mitigating design fact: `-ss` before `-i` seeks at the container
level before decode starts, which is the fast-seek construction by
design. Recommended follow-up: run the one-shot calibration on
reference hardware with `ffmpeg` present before beta, and record the
number here.

(c) NO-TRIP (design fact). The recognized set is
`.flac .mp3 .ogg .m4a .aac .wav .opus`
(`server/src/library/scan/walk.rs`, `AUDIO_EXTENSIONS`); every one
probes, decodes, and fingerprints with the stage-0 dependency set
(symphonia plus `symphonia-adapter-libopus`), pinned by
`library_tags` 29/29 green in this session. WMA is unrecognized
everywhere by explicit owner override (00-decisions.md batch-2
record), which is a product call, not a coverage failure. The known
AAC-tag read gap (lofty sees no tags on raw ADTS) is a read-only
restriction accepted in stage 1 with no v2-parity evidence either
way; it does not block decode.

Hatch 1 verdict: NO-TRIP.

## Hatch 2 - SQLite write model

Trigger recap: (a) p99 write-queue wait over 250 ms sustained 24 h
in normal operation; (b) more than 10 `SQLITE_BUSY` errors per day
after retries; (c) checkpoint backpressure engaged over 1 h/day or
checkpoint p99 over 5 s.

All three sub-conditions are sustained-production behavior:
NOT-MEASURABLE-PRE-RELEASE in each case, with the arming
instrumentation present and tested. The factory pins the design the
hatch assumes: 1 writer lane plus 7 readers, `busy_timeout` 5000,
TRUNCATE checkpoint service with observability, bounded lanes that
fail fast to 503 plus Retry-After. Evidence, all re-run green in
this session (`persist_runtime` 22/22):
`boot_migrates_and_applies_pragmas` (pragma set pin),
`configured_busy_timeout_reaches_pool_connections`,
`writer_fairness_burst_eight_then_one`,
`busy_path_returns_503_with_retry_after_and_no_spin`,
`writer_lane_backpressure_fails_fast_when_full`,
`checkpoint_truncate_reclaims_dead_wal`,
`checkpoint_loop_passes_then_stops`,
`runaway_write_aborts_at_hard_budget`.
No condition can trip before production medians exist; the first
reconsider step the hatch names (raise busy-timeout, split hot
stores) is not indicated by anything in the tree.

Hatch 2 verdict: NO-TRIP.

## Hatch 3 - Axum and handler throughput

Trigger recap, at 2x measured peak with a req/s plus
concurrent-stream replay: (a) p99 non-stream API latency over
500 ms with CPU under 70 percent; (b) range-stream start TTFB p99
over 300 ms; (c) any OOM or stall needing a restart.

(a) and (b) are NOT-MEASURABLE-PRE-RELEASE: v3 has no measured peak
yet, so "2x peak" has no input. The stage-1 replay harness exists
(`tools/perf-harness/replay.py`, thresholds coded to the hatch:
500 ms API p99, 300 ms stream TTFB p99, 5xx/stall detection), but it
speaks only `/api/v1` paths today (zero `/api/v3` references across
the harness). It cannot run against v3 until someone ports its
endpoint mix. That is a tooling gap, not a product trip, but it must
close before beta or the hatch is unarmed when peaks first exist.
Recommended follow-up: port the replay mix to `/api/v3` and record
the first 2x-peak run.

(c) proxy evidence, leaning no-trip: the real-app journey
`stage6_concurrent_streams_under_caps` passed in this session
(5.1 s), and the lease briefs pin the anti-stall construction
(32/8 direct plus 2/1 transcode leases, exactly-once release,
429 plus Retry-After on exhaustion). The single-process invariant
holds; nothing in the tree proposes the multi-process topology the
hatch reserves for (c). A true (c) verdict needs the replay run
above.

Hatch 3 verdict: NO-TRIP.

## Hatch 4 - compat matrix feasibility

Trigger recap: (a) more than 3 pinned reference clients fail
acceptance after a good-faith 2-week conformance pass; (b) emulated
surface grows over 25 percent in one release to chase one client;
(c) any compat fix requires breaking a native v3 contract.

(a) is NOT-MEASURABLE-PRE-RELEASE for the real-client half, and
that is structural, not just timing: no client versions are pinned
anywhere. Stage-0 compat section 1.6 states it plainly for Subsonic
("NO pinned versions exist"), and the three Jellyfin clients
(Finamp, Jellify, Manet) are pinned to quirk sets, not versions.
Acceptance today runs through committed self-golden traces
(`self_golden_finamp/jellify/symfonium.trace.json`) plus 203 compat
briefs. Same-day evidence on this tree: the sibling stage-13 gates
worker re-ran the whole matrix green (compat_subsonic 117,
compat_jellyfin 34, compat_journeys 15, compat_shared 21,
compat_adapters 7, compat_wiring 9, auth_compat_goldens 22; see
`docs/v3-stages/stage-13-gates.md`). Real-target certification
against live client builds remains the recorded stage-9 follow-up
and is still open. Note for the record: as long as no versions are
pinned, sub-condition (a) cannot literally trip; either pin versions
at certification time or reword the trigger to count quirk-contract
failures.

(b) NO-TRIP (design fact). v3 is unreleased, so there is no second
release to grow in; within the one release line, stage 9 held the
shim boundary (the `transcoding` extension is served but
deliberately NOT advertised, `server/src/compat/subsonic/mod.rs`;
 transcode extensions stay unadvertised pending certification).

(c) NO-TRIP (design fact). Compat lives outside the native OpenAPI
surface and behind its own app-password auth layer; the stage-9
record shows zero native-contract breaks and the "compat loses any
conflict" rule held through review.

Hatch 4 verdict: NO-TRIP.

## Hatch 5 - scan throughput

Trigger recap, on the reference 50k-track corpus: (a) full cold
scan over 6 h (under about 2.3 files/s end to end); (b)
incremental backlog grows unbounded over 7 days of normal edits;
(c) fingerprint/identify drops over 1 percent of files for
performance, not evidence, reasons.

(a) NO-TRIP (measured, with one wiring caveat). Two facts shape the
reading. First, the production scan path is walk plus tag read plus
probe only (`LoftyTagReader` in `server/src/library/adapters.rs`);
`generate_fingerprint` is implemented, fpcalc-verified, and tested,
but has no production caller and its outcomes table
(`audio_fingerprint_outcomes`) is unwritten. Fingerprint cost is
therefore not in today's end-to-end scan at all. Second, the
`library_scan` rate briefs use null tag/identify seams, so they
measure discovery and indexing only (initial 4790 files/s on the
6-file fixture in this session) and cannot answer (a) alone.

The missing input was measured directly with a temporary release-mode
probe over committed real-audio fixtures (deleted after the run):
17 mixed fixtures (flac/mp3/m4a/ogg/opus/wav/aac) cost 3.86 ms/file
for probe plus tag read plus fingerprint combined; a synthesized
120 s WAV costs 7.5 ms probe plus 167.5 ms fingerprint; a
frame-multiplied 2.8 MB MP3 (container-capped at header duration,
so read as a pessimistic per-byte bound) costs 19.5 ms probe plus
42.5 ms fingerprint. Fingerprint scales about linearly at 1.4 ms
per audio second and caps at the 120 s window. Pessimistic
extrapolation at 1 s/file single-threaded (5x the longest measured
sample) gives 50k files in 13.9 h single-threaded, about 3.5 h over
the 4-worker production pool (`BlockingPool::new(4)` in
`server/src/library/wiring.rs`) on 4 vCPU reference hardware -
inside the 6 h bound. Realistic cost is a fraction of that.
Provider-bound identification (MusicBrainz 1 req/s is a hard upstream
rule) drains through the separate identify queue after scan runs
complete; it was never inside the 6 h scan bound and the tree keeps
it there.

Caveat recorded: when fingerprint generation gets wired into a
scheduled stage, its per-file cost (about 170 ms per long track in
release here) must be budgeted to the background/idle tier the hatch
itself names. Wiring it inline into cold scan without that budget
is the one plausible future trip for this hatch.

(b) NOT-MEASURABLE-PRE-RELEASE (7 days of normal edits cannot be
observed before release). Arming design present and green in this
session: watcher debounce plus batching (`watcher_batching_brief`),
dirty-scope coalescing (`dirty_scopes_brief`), deferred-tag-read
re-offer (`deferred_tag_read_reoffers_next_run`), all inside
`library_scan` 53/53.

(c) NO-TRIP (design fact). No performance shed path exists. The
identify queue uses a bounded owner-signed deferral ladder (30 s
doubling to 7680 s, ten attempts, then terminal), which is
evidence-based retry, never timeout shedding; scan failures are
counted (`failure_accounting_brief`), not dropped; and the
fingerprint stage is unwired, so there is nothing to shed there
either.

Hatch 5 verdict: NO-TRIP.

## Rollback-boundary decision

Contract question (AGENTS.md Rollback boundary, Q9): before beta
may mutate real libraries, either sealed v3 publication
journals/snapshots must remain usable after application rollback, or
beta users explicitly accept that rollback does not reverse file
mutations.

Decision: FILE-MUTATION IRREVERSIBILITY ACCEPTED (pending explicit
beta-user acceptance; that acceptance must be collected before beta
touches real libraries).

Evidence:

- Journals live as rows in the v3 SQLite database
  (`server/src/library/publish/journal.rs`: monotonic
  prepared/staged/published/committed/cleaned state machine with
  compare-and-swap transitions). The v3 schema is versioned
  forward-only with no down-migrations (stage-2 record: rollback
  means restore backup), so a rolled-back binary cannot interpret
  them.
- Snapshots are serde-JSON `BeforeState` blobs under the sandbox
  metadata dir (`blobs/<sha256>`, `snapshots.rs`) with references
  owned by v3 SQLite rows, expiring by `undo_retention_days`
  (per-operation) or held as immutable baselines. Undo planning
  (`undo.rs`) is a live v3-app operation: it pages the source
  operation's snapshots and compares current bytes against the
  published fingerprint, root, and catalog state. There is no
  offline reader.
- The offline tool (`server/src/bin/droppedneedle-tool.rs`) offers
  DB restore only. No journal-replay or snapshot-undo subcommand
  exists.
- Rollback per Q9 is the last v2 release with its own data dir: a
  separate data dir means the v3 journals and snapshot blobs are
  not even present after rollback, and the v2 image could not parse
  v3 state if they were.
- The forward direction is well covered and is not the question:
  `library_publish` 32/32 green in this session, including the
  per-phase crash-injection briefs. The gap is strictly the
  backward direction.

Consequence: restoring v2 recovers software and the v2 DB; files
v3 renamed or retagged stay renamed and retagged. That matches the
contract's second option exactly. Do not present beta rollback as
reversing library mutations.

## Follow-ups (pre-release, not new scope)

1. Hatch 1(b): one-shot transcode-start calibration (FLAC to 320k
   MP3, post-seek first byte) on reference hardware with `ffmpeg`
   present; record the number in this file.
2. Hatch 3: port `tools/perf-harness/replay.py` (and the `scan.py`
   / `api_latency.py` mixes) to `/api/v3`; record the first 2x-peak
   run once access-log peaks exist.
3. Hatch 4(a): run real-target certification against live client
   builds (carried stage-9 follow-up); pin versions or reword the
   trigger to quirk contracts.
4. Hatch 5(a): when fingerprint generation is wired into a
   scheduled stage, budget it to background/idle tier and re-check
   the 6 h bound.
5. Rollback: collect explicit beta-user acceptance of
   file-mutation irreversibility before beta touches real
   libraries.

## Test record (this session, focused runs only)

| File | Result |
|---|---|
| `server/tests/persist_runtime.rs` | 22 passed, 0 failed |
| `server/tests/library_scan.rs` | 53 passed, 0 failed |
| `server/tests/library_tags.rs` | 29 passed, 0 failed |
| `server/tests/library_identify.rs` | 36 passed, 0 failed |
| `server/tests/library_publish.rs` | 32 passed, 0 failed |
| `server/tests/stream_gateway.rs` | 35 passed, 0 failed |
| `server/tests/stream_gateway_engine.rs` | 21 passed, 0 failed |
| `server/tests/stream_transcode.rs` | 25 passed, 0 failed |
| `server/tests/stage6_journey.rs` (`stage6_concurrent_streams_under_caps`) | 1 passed, 0 failed |
| Temporary release-mode timing probe (deleted after) | fixture + WAV + MP3 scaling lines captured |
| Compat matrix (sibling same-day run, `docs/v3-stages/stage-13-gates.md`) | 225 passed, 0 failed |
