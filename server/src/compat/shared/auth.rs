//! Compat auth posture: OUTSIDE the `/api` middleware.
//!
//! v2 mounts the compat shims in `target_application.py` outside the
//! `/api/*` AuthMiddleware, with auth of their own: per-user app
//! passwords ONLY, never account passwords, never native tokens. The
//! credential stores live in stage 3 (`auth::compat_auth`, reused here
//! by the routers, never reinvented); this module holds the shared
//! posture around them: who counts as anonymous, how principals are
//! labeled for rate limiting, and how failures feed backoff. (The v2
//! `_plugin_user` fallback needs no helper here: anonymous routes carry
//! no plugin seam in this slice, so there is nothing to verify a fallback
//! token against.)

use super::ratelimit::CompatRateLimits;

/// Subsonic public endpoint (normalized: casefolded, one `.view`
/// stripped). The ONLY endpoint that skips auth; binary
/// stream/download/cover included, everything else needs it (v2
/// `_PUBLIC`, stage0 section 1.3).
pub const SUBSONIC_PUBLIC_ENDPOINT: &str = "getopensubsonicextensions";

/// Normalize an endpoint the way v2 `_dispatch` does: casefold, strip
/// ONE trailing `.view`.
pub fn normalize_subsonic_endpoint(raw: &str) -> String {
    let folded = raw.to_lowercase();
    folded.strip_suffix(".view").unwrap_or(&folded).to_owned()
}

/// Whether a raw endpoint name is the public one.
pub fn subsonic_is_public(raw_endpoint: &str) -> bool {
    normalize_subsonic_endpoint(raw_endpoint) == SUBSONIC_PUBLIC_ENDPOINT
}

/// Jellyfin routes that skip token auth (v2 `auth=False` call sites):
/// public system info, QuickConnect flag, logout, login, item images,
/// and anonymous audio (real Jellyfin audio routes carry no
/// `[Authorize]`; Jellify/Finamp/Manet fetch headerless). Case-insensitive
/// on the canonical path; `{id}` segments match any single segment.
pub fn jellyfin_is_anonymous(method: &str, path: &str) -> bool {
    let method = method.to_ascii_uppercase();
    let low = path.to_lowercase();
    let m = method.as_str();
    match m {
        "GET" => {
            low == "/jellyfin/system/info/public"
                || low == "/jellyfin/quickconnect/enabled"
                || is_image_route(&low)
                || is_audio_route(&low, false)
        }
        "HEAD" => is_audio_route(&low, true),
        "POST" => low == "/jellyfin/sessions/logout" || low == "/jellyfin/users/authenticatebyname",
        _ => false,
    }
}

/// `GET /Items/{id}/Images/{type}[/{index}]` (v2 `_image`, anon).
fn is_image_route(low: &str) -> bool {
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

/// `GET /Audio/{id}/universal`, `GET /Audio/{id}/stream[.ext]`
/// (v2 `_universal` / `_audio_stream`, anon).
fn is_audio_route(low: &str, head: bool) -> bool {
    let _ = head;
    let segs: Vec<&str> = low.split('/').collect();
    if segs.len() != 5 || !segs[0].is_empty() || segs[1] != "jellyfin" || segs[2] != "audio" {
        return false;
    }
    if segs[3].is_empty() {
        return false;
    }
    let leaf = segs[4];
    leaf == "universal" || leaf == "stream" || leaf.starts_with("stream.")
}

/// Rate-limit principal label (v2 `_media_principal`): the authed user
/// when one verified, else `ip:<trusted-ip>`. The IP must be the one
/// the trusted-proxy layer established, never a client header (v2
/// `trusted_client_ip`).
pub fn principal_label(authed_user_id: Option<&str>, trusted_ip: &str) -> String {
    match authed_user_id {
        Some(id) if !id.is_empty() => format!("user:{id}"),
        _ => format!("ip:{trusted_ip}"),
    }
}

/// Pre-auth gate: is this IP currently locked out from failures?
pub fn auth_locked_out(limits: &mut CompatRateLimits, ip: &str, now: f64) -> Option<u64> {
    limits.auth_failure_retry_after(ip, now)
}

/// Post-denial accounting: feed one failure into backoff. Returns the
/// fresh lockout length when this failure trips it.
pub fn record_auth_denial(limits: &mut CompatRateLimits, ip: &str, now: f64) -> Option<u64> {
    limits.record_auth_failure(ip, now)
}
