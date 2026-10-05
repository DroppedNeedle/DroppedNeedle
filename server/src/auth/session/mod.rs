//! Native `/api/v3` session core: middleware, transports, origin check, limits.
//!
//! Deny-by-default middleware over `/api/v3/*`, cookie plus Bearer tokens
//! over the same opaque sessions, the cookie-mutation origin check, and the
//! rate-limit class table. Role extractors, session/device management
//! routes, OIDC/Jellyfin/Plex handshakes, and compat auth live in the
//! sibling modules.
//!
//! Mechanism choices, recorded here so they are not reopened:
//!
//! - Cookie sessions are the browser mechanism: `droppedneedle_session`,
//!   httpOnly, `SameSite=Lax`, `Secure` auto-marked on HTTPS (direct or via
//!   `X-Forwarded-Proto`), `Path=<base>/api/v3`, 30-day max-age. Bearer tokens are
//!   scoped to device sessions and scripts. Login takes
//!   `transport: "cookie" (default) | "bearer"`; cookie mode returns no token
//!   in the body (the v2 leak is closed), Bearer mode returns the token once and
//!   sets no cookie. Extraction order is Bearer-then-cookie.
//! - Lifetimes are absolute (30 days). Sliding refresh is rejected: absolute
//!   lifetimes are predictable and testable, and they match v2.
//! - The `__Host-` cookie prefix is rejected: it forbids non-`/` paths and
//!   requires `Secure`, both incompatible with base-path serving and plain-HTTP
//!   LAN installs. The base-path-scoped `Path` plus auto-`Secure` is the policy.
//! - CSRF defence is `Lax` plus the origin check: any cookie-authenticated
//!   unsafe method (`POST/PUT/PATCH/DELETE`) must present `Origin` (or `Referer`
//!   fallback) matching the request host, else 403. Bearer requests are exempt.
//! - Tokens are opaque 32-byte values (urlsafe-b64), SHA-256 hex at rest,
//!   constant-time compared, rejected when revoked or expired.
//! - Login failures run a dummy-hash verify and return one uniform message,
//!   so bad-user and bad-password are indistinguishable by status, body, and
//!   hash work. Every 401 carries `WWW-Authenticate: Bearer`.
//! - Production serves same-origin with no CORS middleware; debug builds keep
//!   an explicit localhost-origins allowlist with credentials.
//!
//! `create_app` mounts the v3 router with the layers in the order the
//! [`middleware`] module documents.

pub mod allowlist;
pub mod cookies;
pub mod cors;
pub mod extract;
pub mod login;
pub mod middleware;
pub mod origin;
pub mod rate_limit;
pub mod store;
pub mod tokens;
