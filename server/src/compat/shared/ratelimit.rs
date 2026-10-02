//! Bounded, identity-aware rate limiting for the compat APIs.
//!
//! Ports `backend/api/compat/common/ratelimit.py`: a public-IP bucket
//! (5/s, burst 20), per-principal browse (30/s, burst 120) and mutation
//! (5/s, burst 20) buckets, and per-IP auth-failure backoff (5 failures
//! in 60s locks out for 10s, doubling to 5min). Media paths are
//! limiter-exempt; every 429 carries `Retry-After`.
//!
//! Time is an explicit `now` (monotonic seconds) so tests pin behavior
//! without sleeping; production passes its monotonic clock.

use std::collections::{HashMap, VecDeque};

/// Max tracked principals / IPs (v2 `_MAX_PRINCIPALS` / `_MAX_IPS`).
pub const MAX_ENTRIES: usize = 10_000;
/// Idle entry lifetime, seconds (v2 `_ENTRY_TTL_SECONDS`).
pub const ENTRY_TTL_SECONDS: f64 = 15.0 * 60.0;
/// Failures inside the window that trigger a lockout.
pub const AUTH_FAILURE_LIMIT: usize = 5;
/// Window those failures are counted in, seconds.
pub const AUTH_FAILURE_WINDOW_SECONDS: f64 = 60.0;
/// First lockout length, seconds.
pub const AUTH_INITIAL_COOLDOWN_SECONDS: f64 = 10.0;
/// Longest lockout, seconds.
pub const AUTH_MAX_COOLDOWN_SECONDS: f64 = 5.0 * 60.0;

/// Token bucket (v2 `TokenBucketRateLimiter` behavior).
#[derive(Debug, Clone)]
pub struct TokenBucket {
    rate: f64,
    capacity: f64,
    tokens: f64,
    updated_at: f64,
}

impl TokenBucket {
    /// Fresh full bucket stamped at `now`.
    pub fn new(rate: f64, capacity: usize, now: f64) -> Self {
        Self {
            rate,
            capacity: capacity as f64,
            tokens: capacity as f64,
            updated_at: now,
        }
    }

    /// Take one token if available.
    pub fn try_acquire(&mut self, now: f64) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Seconds until the next token (v2 `retry_after`).
    pub fn retry_after(&mut self, now: f64) -> f64 {
        self.refill(now);
        if self.tokens >= 1.0 {
            0.0
        } else {
            (1.0 - self.tokens) / self.rate
        }
    }

    fn refill(&mut self, now: f64) {
        let elapsed = (now - self.updated_at).max(0.0);
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.updated_at = now;
    }
}

#[derive(Debug, Clone, Default)]
struct AuthFailureState {
    events: VecDeque<f64>,
    blocked_until: f64,
    strikes: u32,
}

#[derive(Debug, Clone, Copy)]
struct BucketSpec {
    rate: f64,
    capacity: usize,
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
    table: &mut HashMap<String, (f64, TokenBucket)>,
    key: &str,
    spec: BucketSpec,
    now: f64,
) -> Option<u64> {
    evict_expired(table, now);
    if table.len() >= MAX_ENTRIES && !table.contains_key(key) {
        evict_oldest(table);
    }
    let entry = table
        .entry(key.to_owned())
        .or_insert_with(|| (now, TokenBucket::new(spec.rate, spec.capacity, now)));
    entry.0 = now;
    if entry.1.try_acquire(now) {
        None
    } else {
        Some(entry.1.retry_after(now).max(1.0) as u64)
    }
}

fn evict_expired<V>(table: &mut HashMap<String, (f64, V)>, now: f64) {
    let cutoff = now - ENTRY_TTL_SECONDS;
    table.retain(|_, (seen, _)| *seen > cutoff);
}

fn evict_oldest<V>(table: &mut HashMap<String, (f64, V)>) {
    if let Some(oldest) = table
        .iter()
        .min_by(|a, b| {
            a.1.0
                .partial_cmp(&b.1.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(key, _)| key.clone())
    {
        table.remove(&oldest);
    }
}

/// The three bucket families plus auth-failure state (v2
/// `CompatRateLimitState`).
#[derive(Debug, Default)]
pub struct CompatRateLimits {
    public_by_ip: HashMap<String, (f64, TokenBucket)>,
    browse_by_principal: HashMap<String, (f64, TokenBucket)>,
    mutation_by_principal: HashMap<String, (f64, TokenBucket)>,
    auth_failures_by_ip: HashMap<String, (f64, AuthFailureState)>,
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
        let spec = if mutation { MUTATION_SPEC } else { BROWSE_SPEC };
        let table = if mutation {
            &mut self.mutation_by_principal
        } else {
            &mut self.browse_by_principal
        };
        acquire(table, principal, spec, now)
    }

    /// Active lockout remaining for an IP, if any (v2 rounds up).
    pub fn auth_failure_retry_after(&mut self, ip: &str, now: f64) -> Option<u64> {
        evict_expired(&mut self.auth_failures_by_ip, now);
        let state = self
            .auth_failures_by_ip
            .entry(ip.to_owned())
            .or_insert((now, AuthFailureState::default()));
        state.0 = now;
        let remaining = state.1.blocked_until - now;
        if remaining > 0.0 {
            Some((remaining + 0.999).max(1.0) as u64)
        } else {
            None
        }
    }

    /// Record one auth failure; returns the fresh lockout length when
    /// this failure trips it (v2 `record_auth_failure`).
    pub fn record_auth_failure(&mut self, ip: &str, now: f64) -> Option<u64> {
        evict_expired(&mut self.auth_failures_by_ip, now);
        if self.auth_failures_by_ip.len() >= MAX_ENTRIES
            && !self.auth_failures_by_ip.contains_key(ip)
        {
            evict_oldest(&mut self.auth_failures_by_ip);
        }
        let state = self
            .auth_failures_by_ip
            .entry(ip.to_owned())
            .or_insert((now, AuthFailureState::default()));
        state.0 = now;
        let cutoff = now - AUTH_FAILURE_WINDOW_SECONDS;
        while state.1.events.front().is_some_and(|first| *first <= cutoff) {
            state.1.events.pop_front();
        }
        state.1.events.push_back(now);
        if state.1.events.len() < AUTH_FAILURE_LIMIT {
            return None;
        }
        state.1.events.clear();
        state.1.strikes += 1;
        let cooldown = (AUTH_INITIAL_COOLDOWN_SECONDS
            * f64::from(1u32 << state.1.strikes.saturating_sub(1).min(30)))
        .min(AUTH_MAX_COOLDOWN_SECONDS);
        state.1.blocked_until = now + cooldown;
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

/// Media paths skip limiting entirely (v2 `is_media_request`):
/// Subsonic `stream`/`download`, Jellyfin `/audio/*`. Matched
/// case-insensitively on the canonical path.
pub fn is_media_request(path: &str) -> bool {
    let low = path.to_lowercase();
    if let Some(rest) = low.strip_prefix("/subsonic/rest/") {
        let endpoint = rest.rsplit('/').next().unwrap_or(rest);
        let endpoint = endpoint.strip_suffix(".view").unwrap_or(endpoint);
        return matches!(endpoint, "stream" | "download");
    }
    low.starts_with("/jellyfin/audio/")
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
