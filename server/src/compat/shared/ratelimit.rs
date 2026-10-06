//! Bounded, identity-aware rate limiting for the compat APIs.
//!
//! Ports v2's compat rate limiter: a public-IP bucket
//! (5/s, burst 20) for unauthenticated routes only (login, public probes),
//! per-principal browse (30/s, burst 120) and mutation (5/s, burst 20)
//! buckets for signed-in callers, and per-IP auth-failure backoff (5
//! failures in 60s locks out for 10s, doubling to 5min). Media and artwork
//! skip the buckets (a cover grid is dozens of requests at once); every 429
//! carries `Retry-After`. Buckets and the bounded TTL maps are the native
//! limiter's ([`crate::auth::session::rate_limit`]).
//!
//! Time is an explicit `now` (monotonic seconds) so tests pin behavior
//! without sleeping; production passes its monotonic clock.

use std::collections::VecDeque;

use crate::auth::session::rate_limit::{BoundedMap, TokenBucket};

/// Failures inside the window that trigger a lockout.
pub const AUTH_FAILURE_LIMIT: usize = 5;
/// Window those failures are counted in, seconds.
pub const AUTH_FAILURE_WINDOW_SECONDS: f64 = 60.0;
/// First lockout length, seconds.
pub const AUTH_INITIAL_COOLDOWN_SECONDS: f64 = 10.0;
/// Longest lockout, seconds.
pub const AUTH_MAX_COOLDOWN_SECONDS: f64 = 5.0 * 60.0;

#[derive(Debug, Clone, Default)]
struct AuthFailureState {
    events: VecDeque<f64>,
    blocked_until: f64,
    strikes: u32,
}

#[derive(Debug, Clone, Copy)]
struct BucketSpec {
    rate: f64,
    capacity: u32,
}

const PUBLIC_SPEC: BucketSpec = BucketSpec {
    rate: 5.0,
    capacity: 20,
};
const BROWSE_SPEC: BucketSpec = BucketSpec {
    rate: 30.0,
    capacity: 120,
};
const MUTATION_SPEC: BucketSpec = BucketSpec {
    rate: 5.0,
    capacity: 20,
};

fn acquire(
    table: &mut BoundedMap<TokenBucket>,
    key: &str,
    spec: BucketSpec,
    now: f64,
) -> Option<u64> {
    let bucket = table.entry(key, now, || TokenBucket::new(spec.rate, spec.capacity, now));
    if bucket.try_acquire(now) {
        None
    } else {
        Some(bucket.retry_after(now))
    }
}

/// The three bucket families plus auth-failure state (v2
/// `CompatRateLimitState`).
#[derive(Debug, Default)]
pub struct CompatRateLimits {
    public_by_ip: BoundedMap<TokenBucket>,
    browse_by_principal: BoundedMap<TokenBucket>,
    mutation_by_principal: BoundedMap<TokenBucket>,
    auth_failures_by_ip: BoundedMap<AuthFailureState>,
}

impl CompatRateLimits {
    /// Empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Public-IP check; `Some(retry_after)` means reject with 429.
    pub fn public_retry_after(&mut self, ip: &str, now: f64) -> Option<u64> {
        acquire(&mut self.public_by_ip, ip, PUBLIC_SPEC, now)
    }

    /// Per-principal check; `mutation` selects the tighter bucket.
    pub fn principal_retry_after(
        &mut self,
        principal: &str,
        mutation: bool,
        now: f64,
    ) -> Option<u64> {
        let (table, spec) = if mutation {
            (&mut self.mutation_by_principal, MUTATION_SPEC)
        } else {
            (&mut self.browse_by_principal, BROWSE_SPEC)
        };
        acquire(table, principal, spec, now)
    }

    /// Active lockout remaining for an IP, if any (v2 rounds up).
    pub fn auth_failure_retry_after(&mut self, ip: &str, now: f64) -> Option<u64> {
        let state = self
            .auth_failures_by_ip
            .entry(ip, now, AuthFailureState::default);
        let remaining = state.blocked_until - now;
        if remaining > 0.0 {
            Some((remaining + 0.999).max(1.0) as u64)
        } else {
            None
        }
    }

    /// Record one auth failure; returns the fresh lockout length when
    /// this failure trips it (v2 `record_auth_failure`).
    pub fn record_auth_failure(&mut self, ip: &str, now: f64) -> Option<u64> {
        let state = self
            .auth_failures_by_ip
            .entry(ip, now, AuthFailureState::default);
        let cutoff = now - AUTH_FAILURE_WINDOW_SECONDS;
        while state.events.front().is_some_and(|first| *first <= cutoff) {
            state.events.pop_front();
        }
        state.events.push_back(now);
        if state.events.len() < AUTH_FAILURE_LIMIT {
            return None;
        }
        state.events.clear();
        state.strikes += 1;
        let cooldown = (AUTH_INITIAL_COOLDOWN_SECONDS
            * f64::from(1u32 << state.strikes.saturating_sub(1).min(30)))
        .min(AUTH_MAX_COOLDOWN_SECONDS);
        state.blocked_until = now + cooldown;
        Some(cooldown as u64)
    }

    /// Drop a principal's buckets (v2 `clear_principal`).
    pub fn clear_principal(&mut self, principal: &str) {
        self.browse_by_principal.remove(principal);
        self.mutation_by_principal.remove(principal);
    }

    /// Empty all state (tests).
    pub fn reset(&mut self) {
        self.public_by_ip.clear();
        self.browse_by_principal.clear();
        self.mutation_by_principal.clear();
        self.auth_failures_by_ip.clear();
    }
}

/// Normalized Subsonic endpoint of a `/subsonic/rest/...` path.
fn subsonic_endpoint(low: &str) -> Option<&str> {
    let rest = low.strip_prefix("/subsonic/rest/")?;
    let endpoint = rest.rsplit('/').next().unwrap_or(rest);
    Some(endpoint.strip_suffix(".view").unwrap_or(endpoint))
}

/// Media paths skip the token buckets (v2 `is_media_request`): Subsonic
/// `stream`/`download`, Jellyfin `/audio/*` and `/items/{id}/file`.
/// Matched case-insensitively on the canonical path.
pub fn is_media_request(path: &str) -> bool {
    let low = path.to_lowercase();
    if let Some(endpoint) = subsonic_endpoint(&low) {
        return matches!(endpoint, "stream" | "download");
    }
    if low.starts_with("/jellyfin/audio/") {
        return true;
    }
    let segs: Vec<&str> = low.split('/').collect();
    matches!(segs.as_slice(), ["", "jellyfin", "items", id, "file"] if !id.is_empty())
}

/// Artwork paths skip the token buckets too: Subsonic `getCoverArt` and
/// `getAvatar`, Jellyfin `/items/{id}/images/*`. A client paints a grid of
/// covers at once, so a request budget would only produce blank tiles.
pub fn is_artwork_request(path: &str) -> bool {
    let low = path.to_lowercase();
    if let Some(endpoint) = subsonic_endpoint(&low) {
        return matches!(endpoint, "getcoverart" | "getavatar");
    }
    super::auth::is_image_route(&low)
}

/// Mutation classification (v2 `is_mutation_request`): DELETE/PATCH/PUT
/// always; POST except the login and PlaybackInfo reads.
pub fn is_mutation_request(method: &str, path: &str) -> bool {
    if matches!(
        method.to_ascii_uppercase().as_str(),
        "DELETE" | "PATCH" | "PUT"
    ) {
        return true;
    }
    if !method.eq_ignore_ascii_case("POST") {
        return false;
    }
    let low = path.to_lowercase();
    !low.ends_with("/authenticatebyname") && !low.contains("/playbackinfo")
}
