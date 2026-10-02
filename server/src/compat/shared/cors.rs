//! Compat-scoped CORS: wildcard origin, credentials off.
//!
//! Ports `backend/api/compat/common/cors.py` verbatim. `*` with creds off
//! is safe here because compat auth is an explicit token or secret, never
//! an ambient cookie. Applies to `/subsonic` + `/jellyfin` only, and
//! OPTIONS preflights short-circuit 204 before auth (they carry no
//! credentials and must not 401).

use super::path_case;

/// Path prefixes this CORS policy covers.
pub const PREFIXES: [&str; 2] = ["/subsonic", "/jellyfin"];

/// Header set stamped on every compat response (v2 `_CORS_HEADERS`).
pub const HEADERS: [(&str, &str); 5] = [
    ("Access-Control-Allow-Origin", "*"),
    (
        "Access-Control-Allow-Methods",
        "GET, POST, DELETE, HEAD, OPTIONS",
    ),
    (
        "Access-Control-Allow-Headers",
        "Authorization, X-Emby-Token, X-MediaBrowser-Token, X-Emby-Authorization, Content-Type, Range",
    ),
    (
        "Access-Control-Expose-Headers",
        "Content-Range, Accept-Ranges, Content-Length",
    ),
    ("Access-Control-Max-Age", "600"),
];

/// Preflight short-circuit status (v2 returns 204, pre-auth).
pub const PREFLIGHT_STATUS: u16 = 204;

/// Whether a request path falls under compat CORS (exact registered
/// casing; edge routing uses the case-insensitive
/// [`path_case::is_compat_path`] instead, since `/SUBSONIC/...` must
/// redispatch and preflight exactly like `/subsonic/...`).
pub fn is_compat_path(path: &str) -> bool {
    PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

/// Whether this request is a preflight to short-circuit (OPTIONS on a
/// compat path, answered before auth). The path match is
/// case-insensitive: a preflight to `/SUBSONIC/...` must 204, not fall
/// through to the native 404.
pub fn is_preflight(method: &str, path: &str) -> bool {
    method.eq_ignore_ascii_case("OPTIONS") && path_case::is_compat_path(path)
}

/// Credentials are never allowed on compat CORS (no
/// `Access-Control-Allow-Credentials` header is ever emitted).
pub const ALLOWS_CREDENTIALS: bool = false;
