//! Native `/api/v3` session core: middleware, transports, origin check, limits.
//!
//! Deny by default: every route is mounted in one posture group (public,
//! user, curator, admin or its own credential), and the session gate
//! covers the user, curator and admin groups. The API document's
//! `security` states each operation's group. The public and own-credential
//! operations are pinned by name in the server's `auth_e2e` tests, so
//! adding one is a reviewed edit there.
//!
//! The session gate over the gated groups, cookie plus Bearer tokens
//! over the same opaque sessions, the cookie-mutation origin check, and the
//! rate-limit class table, plus the live event stream, which rechecks its
//! session while open. Role extractors, session/device management
//! routes, OIDC/Jellyfin/Plex handshakes, and compat auth live in the
//! other auth modules.
//!
//! Mechanism choices, recorded here so they are not reopened:
//!
//! - Cookie sessions are the browser mechanism: `droppedneedle_sid`,
//!   httpOnly, `SameSite=Lax`, `Secure` auto-marked on HTTPS (direct or via
//!   `X-Forwarded-Proto`) and for a sign-in whose `Origin` is an `https`
//!   address saved in the `public_address` setting, never for any other
//!   sign-in, `Path=<base>/api/v3`, 30-day max-age. Bearer tokens are
//!   scoped to device sessions and scripts. Login takes
//!   `transport: "cookie" (default) | "bearer"`; cookie mode returns no token
//!   in the body (the v2 leak is closed), Bearer mode returns the token once and
//!   sets no cookie. Extraction order is Bearer-then-cookie.
//! - Lifetimes are absolute (30 days). Sliding refresh is rejected: absolute
//!   lifetimes are predictable and testable, and they match v2.
//! - The `__Host-` cookie prefix is rejected: it forbids non-`/` paths and
//!   requires `Secure`, both incompatible with base-path serving and plain-HTTP
//!   LAN installs. The base-path-scoped `Path` plus auto-`Secure` is the policy.
//! - Two names per cookie, chosen by the `Secure` mark: a marked cookie is
//!   `__Secure-<name>`, an unmarked one `<name>`, so the https and plain-HTTP
//!   addresses on one host name keep separate sign-ins in a browser. A
//!   request carrying the `__Secure-` name with a value is read by it alone,
//!   valid or not, never falling back to the plain one, which any plain-HTTP
//!   answer for the host name could have set. Sign-out revokes the session
//!   under each name the request carries and clears both
//!   (`cookies::CookieName`).
//! - CSRF defence is `Lax` plus the origin check: any cookie-authenticated
//!   unsafe method (`POST/PUT/PATCH/DELETE`) must carry `Sec-Fetch-Site:
//!   same-origin`, or an `Origin` (or `Referer` fallback) whose scheme, host
//!   and port match the request's or an address saved in the
//!   `public_address` setting, else 403. Bearer requests are exempt. Nothing
//!   a request sends widens the check, and an empty setting is the default.
//! - Tokens are opaque 32-byte values (urlsafe-b64), SHA-256 hex at rest,
//!   constant-time compared, rejected when revoked or expired.
//! - Login failures run a dummy-hash verify and return one uniform message,
//!   so bad-user and bad-password are indistinguishable by status, body, and
//!   hash work. Every 401 carries `WWW-Authenticate: Bearer`.
//! - Production serves same-origin with no CORS middleware; debug builds keep
//!   an explicit localhost-origins allowlist with credentials.
//!
//! `create_app` layers each group in the order the [`middleware`] module
//! documents.

pub mod client_ip;
pub mod cookies;
pub mod cors;
pub mod event_stream;
pub mod extract;
pub mod login;
pub mod middleware;
pub mod origin;
pub mod rate_limit;
pub mod stated;
pub mod store;
pub mod tokens;
