//! Compat auth posture: outside the `/api` middleware.
//!
//! The compat shims sit outside the `/api/*` AuthMiddleware, with auth of
//! their own: per-user app
//! passwords only, never account passwords, never native tokens. The
//! credential stores live in `auth::compat_auth` (reused here
//! by the routers, never reinvented); this module holds the shared
//! posture around them: who counts as anonymous, how principals are
//! labeled for rate limiting, and how failures feed backoff.

use super::ratelimit::CompatRateLimits;

/// Whether a raw endpoint name (any case, one `.view` allowed) is a
/// public method in the Subsonic method table.
pub fn subsonic_is_public(raw_endpoint: &str) -> bool {
    let name = crate::subsonic::normalize_endpoint(raw_endpoint);
    crate::subsonic::endpoint(&name).is_some_and(|endpoint| endpoint.public)
}

/// Whether `method` on the Jellyfin route axum matched as `template` skips
/// token auth. Read from the route table, so it can never disagree with the
/// routes themselves.
pub fn jellyfin_is_anonymous(method: &str, template: &str) -> bool {
    crate::jellyfin::is_public(method, template)
}

/// `GET /Items/{id}/Images/{type}[/{index}]`, served without auth.
pub(crate) fn is_image_route(low: &str) -> bool {
    let segs: Vec<&str> = low.split('/').collect();
    if segs.len() != 6 && segs.len() != 7 {
        return false;
    }
    segs[0].is_empty()
        && segs[1] == "jellyfin"
        && segs[2] == "items"
        && !segs[3].is_empty()
        && segs[4] == "images"
        && !segs[5].is_empty()
        && (segs.len() == 6 || !segs[6].is_empty())
}

/// Rate-limit principal label: the authed user
/// when one verified, else the client's key
/// ([`client_key`](super::ratelimit::client_key)). The key must come from
/// the address the trusted-proxy layer established, never a client header.
pub fn principal_label(authed_user_id: Option<&str>, client_key: &str) -> String {
    match authed_user_id {
        Some(id) if !id.is_empty() => format!("user:{id}"),
        _ => client_key.to_owned(),
    }
}

/// Rate-limit label for a revoked app password: its own bucket, keyed by
/// the row, never its owner's or the address's.
pub fn revoked_label(app_password_id: &str) -> String {
    format!("revoked:{app_password_id}")
}

/// Response marker on a 401 that refused a revoked app password. The limits
/// layer leaves such a denial out of the address's lockout; a 401 without
/// it counts.
#[derive(Debug, Clone, Copy)]
pub struct RevokedCredential;

/// Pre-auth gate: is this IP currently locked out from failures?
pub fn auth_locked_out(limits: &mut CompatRateLimits, ip: &str, now: f64) -> Option<u64> {
    limits.auth_failure_retry_after(ip, now)
}

/// Post-denial accounting: feed one failure into backoff. Returns the
/// fresh lockout length when this failure trips it.
pub fn record_auth_denial(limits: &mut CompatRateLimits, ip: &str, now: f64) -> Option<u64> {
    limits.record_auth_failure(ip, None, now)
}

/// [`record_auth_denial`] for a refused credential the caller can name:
/// the parts of what was presented (a scheme tag first). A repeat of the
/// same credential from the same address counts once a minute; a different
/// one, a new guess, always counts. Only a keyed hash of the parts is kept.
pub fn record_credential_denial(
    limits: &mut CompatRateLimits,
    ip: &str,
    presented: &[&[u8]],
    now: f64,
) -> Option<u64> {
    limits.record_auth_failure(ip, Some(presented), now)
}
