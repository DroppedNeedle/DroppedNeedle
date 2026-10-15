//! Token-bucket rate limiters with priority waiters.
//!
//! Each provider gets one [`RateLimiter`] built from its verified [`RatePolicy`]
//! row. The bucket refills continuously at `per_second` tokens up to `burst`,
//! and waiters queue by [`RequestPriority`](super::slots::RequestPriority):
//! a user-facing lookup admitted while a background sync waits jumps ahead of
//! it, exactly like v2's heap-ordered waiters. Background jobs pass the
//! background priority explicitly at the call site; nothing here guesses it.
//!
//! The policy rows below encode the verified per-provider table: MusicBrainz
//! 1/s hard, the BrainzMash mirror 10/s, ListenBrainz 1/s, AudioDB 30/min
//! free tier, AcoustID 3/s, Cover Art Archive conservative ~1/s with
//! backoff, Last.fm 5/s with backoff. Two rows differ from v2's running config on purpose: v2 paced
//! ListenBrainz at 2.5/s against a 1/s documented allocation (now fixed to
//! the verified 1/s), and v2's cover-art lane self-throttled at 10/s (now
//! conservative ~1/s per the verified row). Raise a row only by re-verifying
//! against the provider's documented limit.

use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
};

use thiserror::Error;
use tokio::sync::oneshot;

use super::slots::RequestPriority;

/// Floating-point slack for token comparisons, ports v2's `EPSILON`.
const EPSILON: f64 = 1e-9;

/// One provider's verified rate row: sustained rate plus burst capacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RatePolicy {
    /// Sustained tokens per second.
    pub per_second: f64,
    /// Bucket capacity: how many tokens may bunch up at once.
    pub burst: u32,
}

impl RatePolicy {
    /// One verified row.
    #[must_use]
    pub const fn new(per_second: f64, burst: u32) -> Self {
        Self { per_second, burst }
    }
}

/// MusicBrainz: 1 req/s hard, no burst. The capacity of 1 means two callers
/// can never squeeze into the same instant.
pub const MUSICBRAINZ_POLICY: RatePolicy = RatePolicy::new(1.0, 1);
/// BrainzMash, the server-owned MusicBrainz mirror: 10 req/s sustained with
/// no burst (v2 `_BRAINZMASH_RATE_LIMIT`, one in flight).
pub const BRAINZMASH_POLICY: RatePolicy = RatePolicy::new(10.0, 1);
/// ListenBrainz: 1 req/s, no burst.
pub const LISTENBRAINZ_POLICY: RatePolicy = RatePolicy::new(1.0, 1);
/// AudioDB free tier: 30 req/min, paced as 0.5/s with a small burst of 2.
pub const AUDIODB_POLICY: RatePolicy = RatePolicy::new(0.5, 2);
/// AcoustID: 3 req/s with a one-second burst.
pub const ACOUSTID_POLICY: RatePolicy = RatePolicy::new(3.0, 3);
/// Cover Art Archive: no documented allocation, so conservative ~1/s with a
/// burst of 2; 429/503 backoff stays the real governor.
pub const COVERARTARCHIVE_POLICY: RatePolicy = RatePolicy::new(1.0, 2);
/// Last.fm: 5/s community practice with a two-second burst; errors back off.
pub const LASTFM_POLICY: RatePolicy = RatePolicy::new(5.0, 10);

/// Look up one provider's verified row by lowercase name, or `None` for
/// providers without a row (LAN services take no limiter at all).
#[must_use]
pub const fn policy_for(source: &str) -> Option<RatePolicy> {
    // `const fn` with `str` matching keeps this usable in const contexts.
    if source.len() == "musicbrainz".len() && matches_name(source, "musicbrainz") {
        return Some(MUSICBRAINZ_POLICY);
    }
    if source.len() == "brainzmash".len() && matches_name(source, "brainzmash") {
        return Some(BRAINZMASH_POLICY);
    }
    if source.len() == "listenbrainz".len() && matches_name(source, "listenbrainz") {
        return Some(LISTENBRAINZ_POLICY);
    }
    if source.len() == "audiodb".len() && matches_name(source, "audiodb") {
        return Some(AUDIODB_POLICY);
    }
    if source.len() == "acoustid".len() && matches_name(source, "acoustid") {
        return Some(ACOUSTID_POLICY);
    }
    if source.len() == "coverartarchive".len() && matches_name(source, "coverartarchive") {
        return Some(COVERARTARCHIVE_POLICY);
    }
    if source.len() == "lastfm".len() && matches_name(source, "lastfm") {
        return Some(LASTFM_POLICY);
    }
    None
}

const fn matches_name(value: &str, expected: &str) -> bool {
    let left = value.as_bytes();
    let right = expected.as_bytes();
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// Acquiring more tokens than the bucket holds would wait forever.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("cannot acquire {tokens} tokens (capacity: {burst}); the request would wait forever")]
pub struct OverCapacity {
    /// Tokens the caller asked for.
    pub tokens: u32,
    /// The bucket's burst capacity.
    pub burst: u32,
}

/// One queued waiter. Orders as a min-heap on (priority, sequence) so the
/// most urgent, longest-waiting caller grants first.
#[derive(Debug)]
struct Waiter {
    priority: u8,
    sequence: u64,
    tokens: u32,
    grant: Option<oneshot::Sender<()>>,
    cancelled: Arc<AtomicBool>,
}

impl PartialEq for Waiter {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence
    }
}

impl Eq for Waiter {}

impl PartialOrd for Waiter {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Waiter {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: BinaryHeap is a max-heap, we want the smallest first.
        (other.priority, other.sequence).cmp(&(self.priority, self.sequence))
    }
}

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    updated: tokio::time::Instant,
    waiters: BinaryHeap<Waiter>,
    sequence: u64,
}

impl BucketState {
    fn refresh(&mut self, burst: f64, per_second: f64) {
        let now = tokio::time::Instant::now();
        let elapsed = now.duration_since(self.updated).as_secs_f64();
        self.tokens = burst.min(self.tokens + elapsed * per_second);
        self.updated = now;
    }

    /// Grant every grantable head waiter in priority order, dropping
    /// cancelled entries the way v2 drops done futures.
    fn grant_locked(&mut self) {
        while let Some(mut head) = self.waiters.pop() {
            if head.cancelled.load(AtomicOrdering::Acquire) {
                continue;
            }
            if self.tokens + EPSILON < f64::from(head.tokens) {
                self.waiters.push(head);
                return;
            }
            let Some(grant) = head.grant.take() else {
                continue;
            };
            // A dropped receiver means the waiter went away; keep its tokens.
            if grant.send(()).is_err() {
                continue;
            }
            self.tokens -= f64::from(head.tokens);
        }
    }
}

/// Marks the queued waiter cancelled if the `acquire` future drops first,
/// so a cancelled caller never consumes a later grant.
struct CancelOnDrop {
    flag: Arc<AtomicBool>,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.flag.store(true, AtomicOrdering::Release);
    }
}

/// A token-bucket limiter with priority-ordered waiters.
///
/// Cheap to share (`&self` methods only); providers hold one behind an `Arc`
/// inside [`LimiterSet`]. The mutex is a plain std mutex held only across
/// microsecond bookkeeping, never across an await.
#[derive(Debug)]
pub struct RateLimiter {
    policy: RatePolicy,
    state: Mutex<BucketState>,
}

impl RateLimiter {
    /// Build a full bucket from one verified row.
    #[must_use]
    pub fn new(policy: RatePolicy) -> Self {
        Self {
            policy,
            state: Mutex::new(BucketState {
                tokens: f64::from(policy.burst),
                updated: tokio::time::Instant::now(),
                waiters: BinaryHeap::new(),
                sequence: 0,
            }),
        }
    }

    /// The verified row this limiter enforces.
    #[must_use]
    pub const fn policy(&self) -> RatePolicy {
        self.policy
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BucketState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Wait for one token at user priority.
    ///
    /// # Errors
    ///
    /// Returns [`OverCapacity`] when the bucket could never hold the tokens.
    pub async fn acquire(&self) -> Result<(), OverCapacity> {
        self.acquire_with_priority(1, RequestPriority::UserInitiated)
            .await
    }

    /// Wait for `tokens` at an explicit priority.
    ///
    /// Background jobs pass [`RequestPriority::BackgroundSync`] (or
    /// `Opportunistic`) here; the type forces the choice into the open at
    /// every call site instead of defaulting silently.
    ///
    /// # Errors
    ///
    /// Returns [`OverCapacity`] when `tokens` exceeds the burst capacity.
    pub async fn acquire_with_priority(
        &self,
        tokens: u32,
        priority: RequestPriority,
    ) -> Result<(), OverCapacity> {
        if tokens > self.policy.burst {
            return Err(OverCapacity {
                tokens,
                burst: self.policy.burst,
            });
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let (grant, mut notified) = oneshot::channel();
        {
            let mut state = self.lock();
            state.refresh(f64::from(self.policy.burst), self.policy.per_second);
            if state.waiters.is_empty() && state.tokens + EPSILON >= f64::from(tokens) {
                state.tokens -= f64::from(tokens);
                return Ok(());
            }
            let sequence = state.sequence;
            state.sequence += 1;
            state.waiters.push(Waiter {
                priority: priority as u8,
                sequence,
                tokens,
                grant: Some(grant),
                cancelled: Arc::clone(&cancelled),
            });
            state.grant_locked();
        }
        let _cancel = CancelOnDrop { flag: cancelled };
        loop {
            // A latched grant is never lost, so check before parking.
            if notified.try_recv().is_ok() {
                return Ok(());
            }
            let wait = {
                let mut state = self.lock();
                state.refresh(f64::from(self.policy.burst), self.policy.per_second);
                state.grant_locked();
                state.head_wait(f64::from(self.policy.burst), self.policy.per_second)
            };
            tokio::select! {
                biased;
                _ = &mut notified => return Ok(()),
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Take `tokens` without waiting: `true` when granted, `false` when the
    /// bucket is short or another caller already queues.
    pub fn try_acquire(&self, tokens: u32) -> bool {
        let mut state = self.lock();
        state.refresh(f64::from(self.policy.burst), self.policy.per_second);
        if !state.waiters.is_empty() {
            return false;
        }
        if state.tokens + EPSILON >= f64::from(tokens) {
            state.tokens -= f64::from(tokens);
            return true;
        }
        false
    }

    /// Whole tokens currently available.
    #[must_use]
    pub fn remaining(&self) -> u32 {
        let mut state = self.lock();
        state.refresh(f64::from(self.policy.burst), self.policy.per_second);
        state.tokens.max(0.0) as u32
    }

    /// How long until `tokens` are available, rounded up. Zero when the
    /// bucket already covers the request.
    #[must_use]
    pub fn retry_after(&self, tokens: u32) -> std::time::Duration {
        let mut state = self.lock();
        state.refresh(f64::from(self.policy.burst), self.policy.per_second);
        if state.tokens + EPSILON >= f64::from(tokens) {
            return std::time::Duration::ZERO;
        }
        let deficit = f64::from(tokens) - state.tokens;
        let seconds = (deficit / self.policy.per_second).ceil().max(0.0);
        std::time::Duration::from_secs_f64(seconds)
    }

    /// Refill to burst. Tests and provider reconfiguration use this; the
    /// limiter never resets itself in production.
    pub fn reset(&self) {
        let mut state = self.lock();
        state.tokens = f64::from(self.policy.burst);
        state.updated = tokio::time::Instant::now();
    }
}

impl BucketState {
    fn head_wait(&self, _burst: f64, per_second: f64) -> std::time::Duration {
        let Some(head) = self.waiters.peek() else {
            // Our own waiter is queued, so this is unreachable in practice;
            // re-drive quickly rather than stalling if it ever happens.
            return std::time::Duration::from_millis(1);
        };
        let deficit = f64::from(head.tokens) - self.tokens;
        if deficit <= EPSILON {
            return std::time::Duration::from_millis(1);
        }
        let seconds = deficit / per_second;
        let wait = std::time::Duration::from_secs_f64(seconds.max(0.0));
        wait.max(std::time::Duration::from_millis(1))
    }
}

/// The verified provider limiters, built once and shared.
#[derive(Debug)]
pub struct LimiterSet {
    limiters: Vec<(&'static str, RateLimiter)>,
}

impl LimiterSet {
    /// Build one limiter per verified policy row.
    #[must_use]
    pub fn new() -> Self {
        Self {
            limiters: vec![
                ("musicbrainz", RateLimiter::new(MUSICBRAINZ_POLICY)),
                ("brainzmash", RateLimiter::new(BRAINZMASH_POLICY)),
                ("listenbrainz", RateLimiter::new(LISTENBRAINZ_POLICY)),
                ("audiodb", RateLimiter::new(AUDIODB_POLICY)),
                ("acoustid", RateLimiter::new(ACOUSTID_POLICY)),
                ("coverartarchive", RateLimiter::new(COVERARTARCHIVE_POLICY)),
                ("lastfm", RateLimiter::new(LASTFM_POLICY)),
            ],
        }
    }

    /// A set with no rows, so nothing waits. Tests only: production always
    /// paces through [`LimiterSet::new`].
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn unpaced() -> Self {
        Self {
            limiters: Vec::new(),
        }
    }

    /// One provider's limiter, or `None` for providers without a row.
    #[must_use]
    pub fn limiter(&self, source: &str) -> Option<&RateLimiter> {
        self.limiters
            .iter()
            .find(|(name, _)| *name == source)
            .map(|(_, limiter)| limiter)
    }

    /// Lowercase names of every row, in table order.
    #[must_use]
    pub fn sources(&self) -> Vec<&'static str> {
        self.limiters.iter().map(|(name, _)| *name).collect()
    }
}

impl Default for LimiterSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Pacing port behind the audio clients: one token per upstream call. One
/// trait for all four audio clients, so one fake paces every client and the
/// production [`CorePacer`](super::adapters::CorePacer) implements it once
/// against the verified bucket. Slot-lane admission stays at explicit call
/// sites because this seam carries no priority to choose a lane with.
pub trait Pacer: Send + Sync {
    /// Wait until one upstream call may proceed.
    fn acquire(&self) -> impl std::future::Future<Output = ()> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_holds_the_verified_rows() {
        let set = LimiterSet::new();
        assert_eq!(
            set.sources(),
            [
                "musicbrainz",
                "brainzmash",
                "listenbrainz",
                "audiodb",
                "acoustid",
                "coverartarchive",
                "lastfm"
            ]
        );
        assert_eq!(policy_for("musicbrainz"), Some(MUSICBRAINZ_POLICY));
        assert_eq!(policy_for("slskd"), None);
        assert_eq!(policy_for("MUSICBRAINZ"), None);
    }

    #[test]
    fn musicbrainz_row_is_one_per_second_hard() {
        assert_eq!(MUSICBRAINZ_POLICY.per_second, 1.0);
        assert_eq!(MUSICBRAINZ_POLICY.burst, 1);
        assert_eq!(BRAINZMASH_POLICY, RatePolicy::new(10.0, 1));
        assert_eq!(LISTENBRAINZ_POLICY, RatePolicy::new(1.0, 1));
        assert_eq!(AUDIODB_POLICY, RatePolicy::new(0.5, 2));
        assert_eq!(ACOUSTID_POLICY, RatePolicy::new(3.0, 3));
        assert_eq!(COVERARTARCHIVE_POLICY, RatePolicy::new(1.0, 2));
        assert_eq!(LASTFM_POLICY, RatePolicy::new(5.0, 10));
    }

    #[tokio::test]
    async fn over_capacity_fails_instead_of_waiting_forever() {
        let limiter = RateLimiter::new(MUSICBRAINZ_POLICY);
        let error = limiter
            .acquire_with_priority(2, RequestPriority::UserInitiated)
            .await
            .expect_err("2 tokens exceed the burst of 1");
        assert_eq!(
            error,
            OverCapacity {
                tokens: 2,
                burst: 1
            }
        );
    }

    #[tokio::test]
    async fn user_waiter_jumps_ahead_of_background_waiter() {
        use std::sync::Arc;
        use tokio::sync::Mutex as AsyncMutex;

        let limiter = Arc::new(RateLimiter::new(MUSICBRAINZ_POLICY));
        limiter.acquire().await.expect("first token is free");
        assert!(!limiter.try_acquire(1));

        let order = Arc::new(AsyncMutex::new(Vec::new()));
        let background = {
            let limiter = Arc::clone(&limiter);
            let order = Arc::clone(&order);
            tokio::spawn(async move {
                limiter
                    .acquire_with_priority(1, RequestPriority::BackgroundSync)
                    .await
                    .expect("background acquires");
                order.lock().await.push("background");
            })
        };
        // Let the background caller queue first.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let user = {
            let limiter = Arc::clone(&limiter);
            let order = Arc::clone(&order);
            tokio::spawn(async move {
                limiter
                    .acquire_with_priority(1, RequestPriority::UserInitiated)
                    .await
                    .expect("user acquires");
                order.lock().await.push("user");
            })
        };
        background.await.expect("background joins");
        user.await.expect("user joins");
        assert_eq!(*order.lock().await, ["user", "background"]);
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        let limiter = RateLimiter::new(AUDIODB_POLICY);
        assert!(limiter.try_acquire(2));
        // 0.5/s with an empty bucket: one token needs 2s.
        assert_eq!(limiter.retry_after(1), std::time::Duration::from_secs(2));
        limiter.reset();
        assert_eq!(limiter.retry_after(1), std::time::Duration::ZERO);
        assert_eq!(limiter.remaining(), 2);
    }
}
