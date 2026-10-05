//! Request rate limits for the native API, plus the bounded bucket map the
//! compat limiter shares.
//!
//! Every request takes one token from a bucket keyed by (class, caller).
//! The caller is the signed-in user when the session gate resolved one, and
//! the client IP otherwise, so one client can only exhaust its own budget.
//! Classes come from the table below: exact paths for single endpoints,
//! segment-bounded prefixes for families (`/api/v3/auth/setup` never
//! matches `/api/v3/auth/setup/status`). Covers and streams have their own
//! classes so a grid of artwork or a seeking player cannot starve the API.
//! The numbers keep v2's (default 30/s burst 60, login 2/s burst 5, setup
//! 1/s burst 3, search and discover 10/s burst 20).
//!
//! Login is also limited per username inside the login handler, so a
//! password guesser rotating addresses still meets a wall.
//!
//! Buckets live in a [`BoundedMap`]: idle entries expire after 15 minutes
//! (a full refill takes far less) and the map never holds more than
//! [`MAX_ENTRIES`] keys, evicting the stalest when a new key arrives.
//! Every 429 carries `Retry-After` and the budget headers.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Instant;

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{Extensions, HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};

use super::middleware::{CurrentSession, TrustedProxies};
use crate::client_ip::client_ip;

/// Most keys one map tracks before evicting the stalest.
pub const MAX_ENTRIES: usize = 10_000;
/// Idle lifetime of one entry, seconds.
pub const ENTRY_TTL_SECONDS: f64 = 15.0 * 60.0;

/// Token bucket over an explicit monotonic `now` in seconds, so tests pin
/// behavior without sleeping.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    rate: f64,
    capacity: f64,
    tokens: f64,
    updated_at: f64,
}

impl TokenBucket {
    /// Full bucket stamped at `now`.
    pub fn new(rate: f64, capacity: u32, now: f64) -> Self {
        Self {
            rate,
            capacity: f64::from(capacity),
            tokens: f64::from(capacity),
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

    /// Whole tokens left.
    pub fn remaining(&self) -> u32 {
        self.tokens.floor().max(0.0) as u32
    }

    /// Whole seconds until the next token, at least 1.
    pub fn retry_after(&mut self, now: f64) -> u64 {
        self.refill(now);
        let wait = if self.tokens >= 1.0 {
            0.0
        } else {
            (1.0 - self.tokens) / self.rate
        };
        wait.ceil().max(1.0) as u64
    }

    fn refill(&mut self, now: f64) {
        let elapsed = (now - self.updated_at).max(0.0);
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.updated_at = now;
    }
}

/// String-keyed map with idle expiry and a hard size cap.
#[derive(Debug)]
pub struct BoundedMap<V> {
    entries: HashMap<String, (f64, V)>,
    last_sweep: f64,
}

impl<V> Default for BoundedMap<V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            last_sweep: 0.0,
        }
    }
}

impl<V> BoundedMap<V> {
    /// The entry for `key`, created by `make` when absent, stamped as seen
    /// at `now`. Expired entries are swept at most once a second; when the
    /// map is full a new key evicts the stalest entry.
    pub fn entry(&mut self, key: &str, now: f64, make: impl FnOnce() -> V) -> &mut V {
        if now - self.last_sweep >= 1.0 {
            let cutoff = now - ENTRY_TTL_SECONDS;
            self.entries.retain(|_, (seen, _)| *seen > cutoff);
            self.last_sweep = now;
        }
        if self.entries.len() >= MAX_ENTRIES && !self.entries.contains_key(key) {
            let stalest = self
                .entries
                .iter()
                .min_by(|a, b| a.1.0.total_cmp(&b.1.0))
                .map(|(key, _)| key.clone());
            if let Some(stalest) = stalest {
                self.entries.remove(&stalest);
            }
        }
        let entry = self
            .entries
            .entry(key.to_owned())
            .or_insert_with(|| (now, make()));
        entry.0 = now;
        &mut entry.1
    }

    /// Drop one key.
    pub fn remove(&mut self, key: &str) {
        self.entries.remove(key);
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Keys currently held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no key is held.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One bucket class: refill rate, burst, and a stable name for keys and logs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateClass {
    /// Tokens added per second.
    pub rate_per_sec: f64,
    /// Maximum burst.
    pub capacity: u32,
    /// Stable class name.
    pub name: &'static str,
}

/// How a table row matches a path.
#[derive(Debug, Clone, Copy)]
pub enum PathMatch {
    /// The path equals this one.
    Exact(&'static str),
    /// The path is this one or continues it after a `/`.
    Prefix(&'static str),
}

impl PathMatch {
    fn matches(self, path: &str) -> bool {
        match self {
            Self::Exact(exact) => path == exact,
            Self::Prefix(prefix) => path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/')),
        }
    }
}

/// Class for all `/api/v3/*` traffic without an override.
pub const DEFAULT_CLASS: RateClass = class("default", 30.0, 60);
/// Local login, per client and per username.
pub const LOGIN_CLASS: RateClass = class("login", 2.0, 5);
/// Failed authentications on protected paths, per client address. Charged
/// by the session gate, which answers before the request limiter runs.
pub const AUTH_FAILURE_CLASS: RateClass = class("auth-failure", 1.0, 20);

const fn class(name: &'static str, rate_per_sec: f64, capacity: u32) -> RateClass {
    RateClass {
        rate_per_sec,
        capacity,
        name,
    }
}

/// Override table in match order; the first hit wins.
pub const RATE_CLASSES: &[(PathMatch, RateClass)] = &[
    (PathMatch::Exact("/api/v3/auth/login"), LOGIN_CLASS),
    (
        PathMatch::Exact("/api/v3/auth/setup"),
        class("setup", 1.0, 3),
    ),
    (
        PathMatch::Exact("/api/v3/auth/jellyfin/login"),
        class("jellyfin-login", 2.0, 5),
    ),
    (
        PathMatch::Prefix("/api/v3/auth/plex"),
        class("plex", 5.0, 10),
    ),
    (
        PathMatch::Exact("/api/v3/auth/password-recovery/reset"),
        class("recovery-reset", 2.0, 5),
    ),
    (
        PathMatch::Exact("/api/v3/auth/oidc/exchange"),
        class("oidc-exchange", 2.0, 5),
    ),
    (
        PathMatch::Exact("/api/v3/auth/device-sessions"),
        class("device-sessions", 2.0, 5),
    ),
    (
        PathMatch::Prefix("/api/v3/search"),
        class("search", 10.0, 20),
    ),
    (
        PathMatch::Prefix("/api/v3/discover"),
        class("discover", 10.0, 20),
    ),
    (
        PathMatch::Prefix("/api/v3/covers"),
        class("covers", 30.0, 120),
    ),
    (
        PathMatch::Prefix("/api/v3/stream"),
        class("stream", 10.0, 40),
    ),
];

/// Class for a request path: first matching override, else the default.
pub fn classify(path: &str) -> RateClass {
    RATE_CLASSES
        .iter()
        .find(|(rule, _)| rule.matches(path))
        .map(|(_, class)| *class)
        .unwrap_or(DEFAULT_CLASS)
}

/// Outcome of one acquire attempt.
#[derive(Debug, Clone, Copy)]
pub struct AcquireOutcome {
    /// Whether the request may proceed.
    pub allowed: bool,
    /// Whole tokens left after this attempt.
    pub remaining: u32,
    /// Seconds until one token refills (for `Retry-After` on deny).
    pub retry_after_secs: u64,
}

/// The native limiter: keyed buckets plus the proxy trust used to find the
/// client address.
#[derive(Debug)]
pub struct RateLimiter {
    buckets: Mutex<BoundedMap<TokenBucket>>,
    started: Instant,
    trusted_proxies: TrustedProxies,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    /// Empty limiter trusting loopback proxies only.
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(BoundedMap::default()),
            started: Instant::now(),
            trusted_proxies: TrustedProxies::default(),
        }
    }

    /// Trust the given proxies when reading the client address.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
    }

    /// Take one token for `key` in `class` now.
    pub fn check(&self, class: RateClass, key: &str) -> AcquireOutcome {
        self.check_at(class, key, self.started.elapsed().as_secs_f64())
    }

    /// Take one token for `key` in `class` at `now` (monotonic seconds).
    /// A poisoned lock fails closed.
    pub fn check_at(&self, class: RateClass, key: &str, now: f64) -> AcquireOutcome {
        let Ok(mut buckets) = self.buckets.lock() else {
            return AcquireOutcome {
                allowed: false,
                remaining: 0,
                retry_after_secs: 1,
            };
        };
        let bucket = buckets.entry(&format!("{}|{key}", class.name), now, || {
            TokenBucket::new(class.rate_per_sec, class.capacity, now)
        });
        if bucket.try_acquire(now) {
            AcquireOutcome {
                allowed: true,
                remaining: bucket.remaining(),
                retry_after_secs: 0,
            }
        } else {
            AcquireOutcome {
                allowed: false,
                remaining: 0,
                retry_after_secs: bucket.retry_after(now),
            }
        }
    }

    /// Caller key for a request: the signed-in user when the session gate
    /// resolved one, else the client address.
    pub fn caller_key(&self, request: &Request) -> String {
        if let Some(session) = request.extensions().get::<CurrentSession>() {
            return format!("user:{}", session.user_id);
        }
        ip_key(
            request.extensions(),
            request.headers(),
            &self.trusted_proxies,
        )
    }

    /// Client-address key for handlers that limit by address themselves.
    pub fn ip_key_for(&self, parts: &axum::http::request::Parts) -> String {
        ip_key(&parts.extensions, &parts.headers, &self.trusted_proxies)
    }
}

/// `ip:<addr>` for the client resolved by [`client_ip`], or `ip:unknown`
/// when the server runs without connect info (in-process callers all share
/// that one key).
fn ip_key(extensions: &Extensions, headers: &HeaderMap, trusted: &TrustedProxies) -> String {
    match extensions.get::<ConnectInfo<SocketAddr>>() {
        Some(ConnectInfo(peer)) => format!("ip:{}", client_ip(*peer, headers, trusted)),
        None => "ip:unknown".to_owned(),
    }
}

/// Machine code for exhausted buckets.
pub const RATE_LIMITED: &str = "RATE_LIMITED";

/// Axum middleware over `/api/v3/*`. Mounted inside the session gate so a
/// signed-in caller is keyed by user; non-v3 paths pass through.
pub async fn rate_limit(
    State(limits): State<std::sync::Arc<RateLimiter>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !path.starts_with("/api/v3") {
        return next.run(request).await;
    }
    let class = classify(path);
    let outcome = limits.check(class, &limits.caller_key(&request));
    if !outcome.allowed {
        return rate_limited_response(class, outcome.retry_after_secs);
    }
    let mut response = next.run(request).await;
    stamp_budget_headers(response.headers_mut(), class, outcome.remaining);
    response
}

/// Stamp the budget headers on an admitted response.
pub fn stamp_budget_headers(headers: &mut HeaderMap, class: RateClass, remaining: u32) {
    headers.insert("x-ratelimit-limit", class.capacity.into());
    headers.insert("x-ratelimit-remaining", remaining.into());
}

/// 429 in the shared envelope with `Retry-After` and zeroed budget headers.
pub fn rate_limited_response(class: RateClass, retry_after_secs: u64) -> Response {
    let mut response = crate::error::envelope_response(
        StatusCode::TOO_MANY_REQUESTS,
        RATE_LIMITED,
        "Too many requests",
        None,
    );
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::RETRY_AFTER,
        retry_after_secs.max(1).into(),
    );
    stamp_budget_headers(headers, class, 0);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_match_exact_paths_and_segment_prefixes() {
        assert_eq!(classify("/api/v3/auth/setup").name, "setup");
        assert_eq!(classify("/api/v3/auth/setup/status").name, "default");
        assert_eq!(classify("/api/v3/auth/login").name, "login");
        assert_eq!(classify("/api/v3/auth/plex/poll").name, "plex");
        assert_eq!(classify("/api/v3/auth/plexx").name, "default");
        assert_eq!(classify("/api/v3/covers/release/x").name, "covers");
        assert_eq!(classify("/api/v3/stream/local/x").name, "stream");
        assert_eq!(classify("/api/v3/me").name, "default");
    }

    #[test]
    fn callers_get_separate_buckets() {
        let limiter = RateLimiter::new();
        for _ in 0..5 {
            assert!(limiter.check_at(LOGIN_CLASS, "ip:10.0.0.1", 0.0).allowed);
        }
        let denied = limiter.check_at(LOGIN_CLASS, "ip:10.0.0.1", 0.0);
        assert!(!denied.allowed);
        assert!(denied.retry_after_secs >= 1);
        assert!(limiter.check_at(LOGIN_CLASS, "ip:10.0.0.2", 0.0).allowed);
        assert!(limiter.check_at(LOGIN_CLASS, "ip:10.0.0.1", 3.0).allowed);
    }

    #[test]
    fn bounded_map_expires_idle_keys_and_caps_size() {
        let mut map = BoundedMap::default();
        *map.entry("a", 0.0, || 1) += 1;
        map.entry("b", ENTRY_TTL_SECONDS + 1.0, || 1);
        assert_eq!(map.len(), 1, "idle key expired");
        for n in 0..MAX_ENTRIES + 5 {
            map.entry(
                &n.to_string(),
                ENTRY_TTL_SECONDS + 2.0 + n as f64 * 1e-6,
                || 0,
            );
        }
        assert_eq!(map.len(), MAX_ENTRIES);
    }
}
