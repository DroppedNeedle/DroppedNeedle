//! Public allowlist for the v3 native API.
//!
//! Every `/api/v3/*` path requires a session except the paths enumerated here,
//! carried over from the v2 list and re-pathed to `/api/v3`. The v2
//! prefix lesson applies: allowlist exact paths, never broad prefixes, except
//! the OIDC sub-journey whose steps share one flow. Nothing outside this list
//! is public; compat routers mount outside the middleware entirely with their
//! own auth.
//!
//! Plex keeps exactly the v2 shape: only the login start and login poll are
//! public (v2 `/auth/plex/pin` + `/auth/plex/poll`). The link and connect
//! polls hand out account Bearer tokens, which a PIN id alone must never unlock, so they stay session-gated
//! like their v2 parents (link under `/me`, settings under `/plex`).

/// Exact public paths under `/api/v3`. Logout is public so a stale client can
/// always clear its cookie. Every entry must be a mounted route (pinned by a
/// test): a public path with no route is a hole waiting for one. Routes
/// that carry their own credential and never need the gate (the Spotify
/// OAuth callback, the wrapped API) mount outside it instead.
pub const PUBLIC_PATHS: &[&str] = &[
    "/api/v3/auth/providers",
    "/api/v3/auth/setup/status",
    "/api/v3/auth/setup",
    "/api/v3/auth/login",
    "/api/v3/auth/password-recovery/reset",
    "/api/v3/auth/logout",
    "/api/v3/auth/jellyfin/login",
    "/api/v3/auth/plex/start",
    "/api/v3/auth/plex/poll/login",
];

/// Public path prefixes (segment-boundary matched): the OIDC
/// authorize/callback/exchange steps share one flow. Plex is not a prefix
/// on purpose: poll/link and poll/connect require a session.
pub const PUBLIC_PREFIXES: &[&str] = &["/api/v3/auth/oidc"];

/// True when the request path (no query string) needs no session.
/// `/health` and root `/openapi.json` never reach the middleware because it
/// layers only over the v3 router; they need no entry here. Debug-only
/// docs/redoc stay unmounted in production rather than allowlisted.
pub fn is_public(path: &str) -> bool {
    if PUBLIC_PATHS.contains(&path) {
        return true;
    }
    PUBLIC_PREFIXES.iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_public_path_is_a_mounted_route() {
        use utoipa::OpenApi as _;
        let doc = crate::docs::ApiDoc::openapi();
        for path in PUBLIC_PATHS {
            assert!(doc.paths.paths.contains_key(*path), "{path} has no route");
        }
        for prefix in PUBLIC_PREFIXES {
            assert!(
                doc.paths.paths.keys().any(|path| path.starts_with(prefix)),
                "{prefix} has no route"
            );
        }
    }

    #[test]
    fn allowlist_is_exact_and_nothing_adjacent() {
        for path in PUBLIC_PATHS {
            assert!(is_public(path), "{path} must be public");
        }
        assert!(is_public("/api/v3/auth/oidc/exchange"));
        assert!(!is_public("/api/v3/auth/plex/poll/link"));
        assert!(!is_public("/api/v3/auth/plex/poll/connect"));
        assert!(!is_public("/api/v3/auth/plex"));
        assert!(!is_public("/api/v3/auth/login/"));
        assert!(!is_public("/api/v3/auth/plex-admin"));
        assert!(!is_public("/api/v3/auth/sessions"));
        assert!(!is_public("/api/v3/me"));
        assert!(!is_public("/api/v3/auth/admin/users"));
    }
}
