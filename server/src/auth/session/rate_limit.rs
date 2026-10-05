//! Rate-limit classes: default bucket plus per-path overrides.
//!
//! v2 shape kept: one shared token bucket per class, prefix-matched in table
//! order, 429s carrying `Retry-After` plus `X-RateLimit-Limit/Remaining`.
//! Default stays 30/s+60. v2 overrides re-pathed (`auth/login`,
//! `auth/setup`, `auth/jellyfin/login`, `auth/plex/*` for the unified journey)
//! plus the three new strict rows the spec orders at login-class strictness:
//! `auth/password-recovery/reset`, `auth/oidc/exchange`, `auth/device-sessions`.
//! (v2's search/discover/covers overrides belong to those endpoints.)

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::error::{ErrorBody, ErrorEnvelope};

/// One bucket class: refill rate, burst capacity, stable name for tests/logs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateClass {
    /// Tokens added per second.
    pub rate_per_sec: f64,
    /// Maximum burst.
    pub capacity: u32,
    /// Stable class name.
    pub name: &'static str,
}

/// Default class for all `/api/v3/*` traffic without an override.
pub const DEFAULT_CLASS: RateClass = RateClass {
    rate_per_sec: 30.0,
    capacity: 60,
    name: "default",
};

/// Override table in match order: (path prefix, class). First prefix hit wins.
pub const RATE_CLASSES: &[(&str, RateClass)] = &[
    (
        "/api/v3/auth/login",
        RateClass {
            rate_per_sec: 2.0,
            capacity: 5,
            name: "login",
        },
    ),
    (
        "/api/v3/auth/setup",
        RateClass {
            rate_per_sec: 1.0,
            capacity: 3,
            name: "setup",
        },
    ),
    (
        "/api/v3/auth/jellyfin/login",
        RateClass {
            rate_per_sec: 2.0,
            capacity: 5,
            name: "jellyfin-login",
        },
    ),
    (
        "/api/v3/auth/plex",
        RateClass {
            rate_per_sec: 5.0,
            capacity: 10,
            name: "plex",
        },
    ),
    (
        "/api/v3/auth/password-recovery/reset",
        RateClass {
            rate_per_sec: 2.0,
            capacity: 5,
            name: "recovery-reset",
        },
    ),
    (
        "/api/v3/auth/oidc/exchange",
        RateClass {
            rate_per_sec: 2.0,
            capacity: 5,
            name: "oidc-exchange",
        },
    ),
    (
        "/api/v3/auth/device-sessions",
        RateClass {
            rate_per_sec: 2.0,
            capacity: 5,
            name: "device-sessions",
        },
    ),
];

/// Class for a request path: first matching override, else the default.
pub fn classify(path: &str) -> RateClass {
    RATE_CLASSES
        .iter()
        .find(|(prefix, _)| path.starts_with(prefix))
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

/// One token bucket. Time is injected so tests never sleep.
pub struct TokenBucket {
    class: RateClass,
    state: Mutex<BucketState>,
}

struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Full bucket of one class, refilling from `now`.
    pub fn new(class: RateClass, now: Instant) -> Self {
        Self {
            class,
            state: Mutex::new(BucketState {
                tokens: class.capacity as f64,
                last_refill: now,
            }),
        }
    }

    /// Class served by this bucket.
    pub fn class(&self) -> RateClass {
        self.class
    }

    /// Try to take one token at `now`. Lock failure fails closed.
    pub fn try_acquire_at(&self, now: Instant) -> AcquireOutcome {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return AcquireOutcome {
                    allowed: false,
                    remaining: 0,
                    retry_after_secs: 1,
                };
            }
        };
        let elapsed = now
            .saturating_duration_since(state.last_refill)
            .as_secs_f64();
        state.tokens =
            (state.tokens + elapsed * self.class.rate_per_sec).min(self.class.capacity as f64);
        state.last_refill = now;
        if state.tokens >= 1.0 {
            state.tokens -= 1.0;
            AcquireOutcome {
                allowed: true,
                remaining: state.tokens.floor() as u32,
                retry_after_secs: 0,
            }
        } else {
            let deficit = 1.0 - state.tokens;
            AcquireOutcome {
                allowed: false,
                remaining: 0,
                retry_after_secs: (deficit / self.class.rate_per_sec).ceil().max(1.0) as u64,
            }
        }
    }
}

/// Shared limiter state: one bucket per override plus the default.
pub struct RateLimiterSet {
    default: TokenBucket,
    overrides: Vec<(&'static str, TokenBucket)>,
}

impl RateLimiterSet {
    /// Fresh buckets, all full.
    pub fn new(now: Instant) -> Self {
        Self {
            default: TokenBucket::new(DEFAULT_CLASS, now),
            overrides: RATE_CLASSES
                .iter()
                .map(|(prefix, class)| (*prefix, TokenBucket::new(*class, now)))
                .collect(),
        }
    }

    /// Bucket serving `path` and its class (first override hit, else default).
    pub fn bucket_for(&self, path: &str) -> (&TokenBucket, RateClass) {
        for (prefix, bucket) in &self.overrides {
            if path.starts_with(prefix) {
                return (bucket, bucket.class());
            }
        }
        (&self.default, DEFAULT_CLASS)
    }
}

/// Machine code for exhausted buckets.
pub const RATE_LIMITED: &str = "RATE_LIMITED";

/// Axum middleware: pre-auth backstop and aggregate cap over `/api/v3/*`.
/// Non-v3 paths pass through untouched.
pub async fn rate_limit(
    State(limits): State<Arc<RateLimiterSet>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if !path.starts_with("/api/v3") {
        return next.run(request).await;
    }
    let (bucket, class) = limits.bucket_for(&path);
    let outcome = bucket.try_acquire_at(Instant::now());
    if !outcome.allowed {
        return rate_limited_response(class, outcome.retry_after_secs);
    }
    let mut response = next.run(request).await;
    stamp_budget_headers(response.headers_mut(), class, outcome.remaining);
    response
}

/// Stamp the budget headers on an admitted response.
pub fn stamp_budget_headers(headers: &mut HeaderMap, class: RateClass, remaining: u32) {
    if let (Ok(limit), Ok(left)) = (
        class.capacity.to_string().parse(),
        remaining.to_string().parse(),
    ) {
        headers.insert("x-ratelimit-limit", limit);
        headers.insert("x-ratelimit-remaining", left);
    }
}

/// 429 response with `Retry-After` and zeroed budget headers. The body is
/// the shared error envelope (no local duplicate).
pub fn rate_limited_response(class: RateClass, retry_after_secs: u64) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: RATE_LIMITED.to_owned(),
            message: "Too many requests".to_owned(),
            details: None,
        },
    };
    let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = retry_after_secs.max(1).to_string().parse() {
        headers.insert(axum::http::header::RETRY_AFTER, value);
    }
    stamp_budget_headers(headers, class, 0);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_match_v2_numbers_plus_login_class_new_rows() {
        assert_eq!(classify("/api/v3/me").name, "default");
        let login = classify("/api/v3/auth/login");
        assert_eq!((login.rate_per_sec, login.capacity), (2.0, 5));
        let setup = classify("/api/v3/auth/setup");
        assert_eq!((setup.rate_per_sec, setup.capacity), (1.0, 3));
        let plex = classify("/api/v3/auth/plex/poll");
        assert_eq!((plex.rate_per_sec, plex.capacity), (5.0, 10));
        for path in [
            "/api/v3/auth/password-recovery/reset",
            "/api/v3/auth/oidc/exchange",
            "/api/v3/auth/device-sessions",
        ] {
            let class = classify(path);
            assert_eq!((class.rate_per_sec, class.capacity), (2.0, 5), "{path}");
        }
    }

    #[test]
    fn bucket_exhausts_then_refills_without_sleeping() {
        let start = Instant::now();
        let bucket = TokenBucket::new(classify("/api/v3/auth/login"), start);
        for _ in 0..5 {
            assert!(bucket.try_acquire_at(start).allowed);
        }
        let denied = bucket.try_acquire_at(start);
        assert!(!denied.allowed);
        assert!(denied.retry_after_secs >= 1);
        let later = start + std::time::Duration::from_secs(3);
        assert!(bucket.try_acquire_at(later).allowed);
    }
}
