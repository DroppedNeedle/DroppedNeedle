# Stage 6 manifest — Remote sources (R1) + streaming gateway (R2) + playback reporting

Status: COMPLETE 2026-09-29. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: ONE remote-source adapter unifying Jellyfin/Navidrome/Plex browse
(hub/albums/artists/tracks/search/recent/favorites/genres/playlists/
info/lyrics/top/similar/sessions/history/images/covers/match),
per-user connections + credential store (re-encrypted), Navidrome
folder preferences; ONE source-keyed stream gateway unifying
local/JF/ND/Plex (GET/HEAD + start/progress/stop/scrobble/now-playing
reporting); transcode `decide()` policy + ffmpeg invocation contract
(mp3/opus, `-ss` before `-i`, piped stdout, identity encoding); shared
streaming byte contract (single-range 206 incl. suffix/open, 416 +
`bytes */N` otherwise, HEAD headers-only, per-extension content
types, concurrency leases 32/8 direct + 2/1 transcode, 429 +
Retry-After); native scrobble + now-playing presence; JF/ND/Plex MBID
warmup loops owned here.

Preserved behavior: `decide()` rules in order (explicit-request
trigger, server max as ceiling, 0/unset handling); ffmpeg argv +
response headers; byte-exact 200/206/416/HEAD; lease release exactly
once incl. cancellation; plugin-stream fallback ordering; scrobble
>90%-or-within-1s rule; Navidrome 0.62.0 single-folder probe
behaviors.

Intended changes: ~86 parallel browse endpoints collapse to one
adapter surface (R1); ~17 stream endpoints collapse to one gateway
(R2); unified shapes under `/api/v3`; rows A:424 (ND cover) + A:476
(Plex playlist-thumb) folded under adapter covers/images; WMA never
streams; compat §1.5 WMA content-type row dropped.

Acceptance tests: adapter-parity briefs per source; gateway briefs
(range matrix, 416 set, HEAD, content types, identity, lease 429);
decide-policy briefs; cancel-brief; E2E connect → browse → play →
seek → stop (reported), transcode with estimate, concurrent streams
under caps.

Destructive ops: none (no library mutation; transcodes piped).

Permitted external systems: in-repo mock servers for JF/ND/Plex; no
live media servers in tests.

Non-goals: compat streaming shims (stage 9 reuses this engine);
download acquisition (stage 7); player UI (stage 12).

Completion criteria: matrix green; full gate set green; review loop
clean (zero blocker/major).

## Completion record

Remote sources + streaming are live: unified adapter over three
in-repo mock servers, source-keyed gateway with the byte-exact
contract, ffmpeg policy + execution, playback reporting with live
now-playing registry, MBID warmup loops spawned with shutdown
plumbing. 1060 tests green (0 failed), full gate set green (cargo
test, clippy -D warnings, fmt --check, contract-diff gate, cargo deny,
cargo audit, container E2E). Auth matrix covers all new routes
including HEAD; openapi.json + .d.ts regenerated and in sync.

Review caught and fixed two journey blockers (transcode-estimate
round-trip + concurrent-streams-under-caps journeys added) and one
major (HEAD auth coverage + documented_routes gap closed), plus
minors: Jellyfin 403→Auth, folder echo when down, upstream
content-type fallback, HEAD length mirroring, secret Debug redaction,
symlink sandbox hardening, PermissionDenied mapping, ND offset + Plex
section pins, HEAD 416, identity asserts on all range forms,
mega-test splits, exact journey totals.

Decisions recorded: gateway engine written inline by the orchestrator
after three slice attempts died on provider stream stalls (leases +
gateway + 17 engine briefs, all green); engine implements the routes'
StreamEngine trait spelling in place (no type moves); remote reads are
direct-only (ffmpeg takes a local path; server-side transcode stays a
remotes-adapter concern); local root is provisional `<root>/music`
until stage 8 owns real roots; direct lease covers open+read only
(whole-bytes seam); stage-4 static now-playing snapshot retired and
its dead plumbing + test removed (not kept as contract).

Follow-ups (required before release, not new scope): stage 8 supplies
real library roots to the gateway; stage 9 reuses this engine for
compat streaming shims; stage 12 migrates R2/R1 player call sites to
the frozen stream shapes.

## Accountability

Implementers: s6-adapter, s6-gateway (stalled, zero files), s6-transcode
(stalled, zero files), s6-playback, s6-gateway-2 (stalled, zero files),
s6-transcode-2, s6-gweng (stalled, zero files), s6-gwroutes,
orchestrator (gateway engine: leases.rs + gateway.rs + mod.rs +
transcode.rs out_format accessor + 17 engine briefs), s6-integrate
(wiring, RemoteReader bridge, warmup spawn, matrix, openapi, journey),
s6-fixups (review findings).

Reviewers (all different agents from the implementers): s6-review-code
(0 blocker / 0 major / 7 minor / 3 nits), s6-review-tests (2 blocker /
1 major / 7 minor / 4 nits), s6-review-pass2 (17/17 verified; 3 new
non-blocking minors fixed inline by the orchestrator: two stale
wiring docs, one Debug-redaction pinning test).
All blocker/major/minor findings resolved and re-verified; no residuals.
