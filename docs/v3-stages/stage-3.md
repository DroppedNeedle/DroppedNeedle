# Stage 3 manifest — Auth, users, sessions, app passwords

Status: COMPLETE 2026-09-28. Branch `v3`. Plan source:
`.dev-notes/Plans/RustPort/build-stage-plan.md` (local working copy; this
file is the checked-in record).

## Manifest (from the accepted build plan)

Scope: sessions (httpOnly cookie + Bearer [REDACTED] absolute 30-day
lifetimes, sliding refresh rejected, `__Host-` prefix rejected with base-path
+ HTTP-LAN reason, cookie mode returns no token in body, rate-limit classes
for login/setup/Jellyfin/Plex-poll + reset/OIDC-exchange/device-session
rows), roles (user/trusted/admin + curator), companion sessions
(replace-on-remint atomic, companion-cannot-mint 403), profile routes
A:494-501 clean-slate shapes, password recovery, OIDC + Jellyfin login +
user import, unified Plex journey backend (R4), bcrypt→Argon2id hash import
(scheme-tagged, sessions not surviving), per-user Last.fm store (R7),
app-password CRUD, compat-auth contracts byte-locked (Subsonic codes
10/40/43/44/50/70 incl. getAvatar 403-as-text split; Jellyfin 401/login-echo;
account-password rejection on both; no native tokens on compat paths).

Preserved behavior: deny-by-default middleware + public path allowlist;
origin check on cookie mutations; dummy-hash verify uniform 401;
`WWW-Authenticate: Bearer`; 401/403-or-404 matrix on user-owned resources;
setup once-only; recovery single-use; app-password shown once + touch;
30/s+60 default limits; Plex poll rate behavior; OIDC relative Location +
no-store; Spirits of `test_auth_on_every_endpoint.py` as a standing contract
runner.

Intended changes: clean-slate `/api/v3` auth shapes (no v1 shape compat);
sessions own `last_seen_at` writes (throttled); admin-global Last.fm pair
deleted; three Plex flows merged to one journey; session lifetimes absolute;
`__Host-` rejected.

Acceptance tests: session round-trip briefs (cookie/Bearer, origin,
sliding-absent, transport), role-matrix briefs per route, companion
replace-on-remint atomic + cannot-mint 403 brief, OIDC/Jellyfin/Plex fake
journey briefs, bcrypt-import→argon2id brief,
app-password-native-reject/post-import-compat-verify briefs, compat-auth
golden briefs per code, LoginAttempt observability per auth-D6 retained.
E2E: setup → login → list/revoke session → logout-all → re-login; admin
creates user → role transitions → user login → admin deletes user →
sessions dead; app-password create → compat-accept → native-reject →
revoke → dead.

Destructive ops: none outside scratch DBs (auth_tokens excluded from export
by design; no live import runs).

Permitted external systems: none at runtime (IdP handshakes + HIBP are
fakes/seams in tests; no live IdP calls).

Non-goals: sessions UI components (stage 12); compat routers (stage 9);
settings HTTP (stage 10); quota routes A:42-43 (stage 10 user-admin).

Completion criteria: matrix green; login p95 within its budget class (plan
criterion amended: login is work-factor-dominated, not a standard read);
review loop clean.

## Completion record

Native auth is live behind deny-by-default: 43 mounted handlers, 331 tests
green, full gate set green on the orchestrator's own runs (cargo test,
clippy -D warnings, fmt --check, contract-diff gate, cargo deny, cargo
audit, container E2E). Auth-on-every-endpoint matrix cross-checks ApiDoc in
both directions; login p95 ≈300ms against the ≤600ms work-factor class.

Review caught and fixed two security blockers: recovery reset now revokes
all sessions atomically with the hash change, and Plex link/connect polls
are session-gated (the stage0 D2 spec wrongly listed the whole plex prefix
public — amended with attribution). The opportunistic rehash now runs on
the real login path, admin-create faults map correctly, and the D5 bulk
user-import surface (absent tree-wide) was implemented with honest 503s.

Decisions recorded: scheme tags live in `provider_data` JSON (no
migration); production IdP web clients are a follow-up step (Disabled impls
serve contract-correct 503/502 until then); debug CORS is constructor-only
+ debug-builds-only with explicit headers (tower-http rejects credentials +
wildcard); trusted-proxy resolution honors X-Forwarded-* only from trusted
peers; container image owns writable cache/config dirs.

Follow-ups (required before release, not new scope): production IdP HTTP
clients (OIDC discovery/token/userinfo, Jellyfin auth, Plex PIN/account,
Last.fm web — stage-5 provider owns Last.fm); stage-11 handoff (session
death on import clearing test: auth_tokens + recovery/OIDC/Spotify states;
lastfm carve-out; secret re-encryption pipeline); Secure-behind-proxy is
dormant until a prod-wiring step supplies ConnectInfo; unreachable
Authentication→503 fallback arm in federated.rs:668.

## Accountability

Implementers: s3-sessions, s3-users, s3-federated, s3-close (stalled with
zero output, cancelled; split into s3-adapters + s3-routers + orchestrator
app wiring), s3-e2e.

Reviewers (all different agents from the implementers): s3-review-sessions
(0 blocker / 1 major / 6 minor), s3-review-users (1 / 2 / 8),
s3-review-federated (1 / 1 / 9), s3-review-pass2 (0 / 0 / 4 — delivery
envelope stuck, verdict recovered from the session log and confirmed).
Fix-ups: s3-fix-sessions, s3-fix-users, s3-fix-federated, s3-fix-loginbudget,
s3-fix-matrix, s3-fix-allowbrief, s3-fix-residuals.
All blocker/major/minor findings resolved and re-verified; no residuals.
