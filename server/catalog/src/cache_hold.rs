//! How long an answer may be cached when a read behind it failed.
//!
//! A section built past a failed read looks like "nothing there" or a
//! short list. Caching it for the normal lifetime would keep an outage, a
//! rate limit or a refusal on screen long after the provider is back. A
//! failure that could pass is held only for the wait the provider named,
//! and not at all when it named none. A deterministic failure (absence, a
//! rejected request) is a real answer and keeps the normal lifetime.

use std::time::Duration;

use droppedneedle_providers::ProviderError;

/// The longest an answer may be cached after the failures noted so far.
/// `None` until a passing failure is noted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheHold(Option<Duration>);

impl CacheHold {
    /// Note a provider failure the answer was built past.
    pub fn provider(&mut self, error: &ProviderError) {
        if error.is_retriable() {
            self.limit(error.retry_after().unwrap_or(Duration::ZERO));
        }
    }

    /// Note a local failure (a store read): nothing says when it passes,
    /// so the answer is not cached.
    pub fn local(&mut self) {
        self.limit(Duration::ZERO);
    }

    /// Fold in the failures another part of the answer noted.
    pub fn merge(&mut self, other: Self) {
        if let Some(wait) = other.0 {
            self.limit(wait);
        }
    }

    /// The lifetime to cache the answer for: `ttl`, cut to the shortest
    /// wait noted. Zero means do not cache.
    #[must_use]
    pub fn cap(self, ttl: Duration) -> Duration {
        self.0.map_or(ttl, |wait| ttl.min(wait))
    }

    fn limit(&mut self, wait: Duration) {
        self.0 = Some(self.0.map_or(wait, |held| held.min(wait)));
    }
}
