# Stage 9 manifest — Compat APIs (Subsonic + Jellyfin, contract-first)

Status: COMPLETE 2026-10-02. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: Subsonic pinned 1.16.1 (endpoint x fields x auth table,
XML+JSON+JSONP, error codes, binary-vs-envelope split, ID prefixes,
match-all search, size buckets, playlist/favorite/rating/scrobble/queue/
bookmark/info/avatar/lyrics/genre/user/scan/discovery semantics);
Jellyfin matrixed clients only (Finamp/Jellify/Manet quirk contracts;
PascalCase, real codes, anon audio + api_key-embedded DirectStreamUrl,
both FavoriteItems dialects, playback sessions, playlists); shared
streaming engine reuse from stage 6 (byte-exact); golden-trace corpus;
kill switches default off; compat rate limits + auth backoff; CORS `*`
creds-off; case-insensitive paths; access-log redaction; D5
single-folder; signed-param transcode endpoints; setRating validated
no-op; playqueue cap + stale-remap; getOpenSubsonicExtensions public;
disabled-Subsonic code-0 vs disabled-Jellyfin 404; transcode-hint
fields; download filename sanitization; _plugin_user token rule on
anon routes.

Preserved behavior: every quirk in stage0-compat.md sections 1-2
(Symfonium/Arpeggi/gonic match-all, Feishin bitrate-0 + lowercase
paths, Finamp 15 non-null fields + login objects, Jellify bare-array
Latest + headerless audio, Manet Filters-first + DateCreated "O"
format); Subsonic auth-required binary vs Jellyfin anon-audio split;
getAvatar 403-as-text; PlayedItems marker-only; QuickConnect literal
false; no `/jellyfin/socket`.

Intended changes: v2's compat layer reimplemented on the stage-6
engine; `transcoding`-advertised divergence resolved to one documented
choice; transcode extensions stay unadvertised pending real-target
certification (hatch-4 armed).

Acceptance tests: golden brief per matrix row (wire bytes + header
sidecar with exact/regex/ignore modes); auth-matrix briefs (Subsonic
10/40/43/44/50/70 incl. binary-enveloped-50 vs text-403; Jellyfin
401/login-echo/anon set); streaming-semantics briefs (shared with
stage 6, both protocols); kill-switch briefs (disabled means no
method enumeration); redaction brief. E2E per matrixed client
journey: browse, stream, favorite, playlist, scrobble-report
(Symfonium-shape via Subsonic; Finamp-shape via Jellyfin; Jellify
Latest to headerless play).

Destructive ops: none (compat never mutates the library beyond
favorites/playlists/presence projection, all fixture-scoped in
tests).

Permitted external systems: golden-trace capture against reference
servers recorded once; tests run against committed goldens + fakes
only.

Non-goals: non-matrixed clients (Swiftfin/Infuse/web/Kodi/Roku/DLNA/
QuickConnect/video); native API changes (compat loses any conflict).

Completion criteria: full matrix green; hatch-4 evaluated (at most 3
pinned-client failures after conformance pass, surface growth at
most 25%, zero native-contract breaks); review loop clean.

## Completion record

Compat APIs are live: 4 slices (subsonic, jellyfin, shared,
journeys) wired outside `/api` session auth behind their own
app-password layer, reusing the stage-6 streaming engine. 1699
tests green (0 failed), of which 203 are new compat briefs
(subsonic, jellyfin, shared auth matrix, journeys, adapters,
wiring) plus fixture goldens. Full gate set green (cargo test,
clippy -D warnings, fmt --check, contract-diff gate, cargo deny,
cargo audit, container E2E). Compat stays out of OpenAPI (all
paths remain `/api/*` or `/health`); kill switches default off;
access-log redaction wired and briefed.

Review caught and fixed 9 majors (transcode-offset byte
correctness, bitrate-direct-as-transcode, media-path lockout
bypass, golden-is-error-envelope, adapter-coverage gap, HEAD+Range
contradiction, doc/corpus split, trace-without-headers, unwired
redaction) plus 18 minors and 6 nits across both reviews, each
with a dedicated test.

Decisions recorded: compat keeps its own auth-matrix briefs
instead of duplicating them into auth_e2e; tower moves to main
deps, regex to dev-deps; pass-2 verified every finding fixed in
code and covered by a test (one harmless nit left open).

Follow-ups (required before release, not new scope): hatch-4
pinned-client certification against real targets; stage 12 adds
compat admin UI callers on the frozen shapes.

## Accountability

Implementers: s9-subsonic, s9-jellyfin, s9-shared, s9-journeys
(disjoint slices), s9-integrate (died in a restart after partial
wiring), s9-integrate-finish (verified + completed wiring),
s9-fixups (died in a restart after substantial work),
s9-fixups-finish (died in a restart at the fmt gate; tree verified
directly by the orchestrator instead, plus 2 residual clippy lints
fixed inline: leak.rs to_owned, journeys doc-list blank line).

Reviewers (all different agents from the implementers):
s9-review-code (3 major / 10 minor / 3 nits), s9-review-tests
(6 major / 8 minor / 6 nits), s9-review-pass2-code (pass, 0 open
majors), s9-review-pass2-tests (pass, 0 open majors).
All blocker/major/minor findings resolved and re-verified; no residuals.
