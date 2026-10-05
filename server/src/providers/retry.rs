//! Idempotency-aware retry with backoff, jitter, and a budget.
//!
//! [`execute`] runs one logical provider call. Non-idempotent operations
//! (anything that writes: scrobbles, contributions, ratings) run exactly
//! once and never retry, because a retry could double-apply the write.
//! Idempotent reads retry on [`ProviderError::is_retriable`] failures with
//! exponential backoff and full-ish jitter, porting v2's `with_retry`:
//! `min(base * 2^(attempt-1), max) * (0.5 + random())`.
//!
//! Two v2 behaviors carry over deliberately:
//!
//! - A `Retry-After` carried on the failure (429/503) replaces the computed
//!   backoff for that sleep instead of adding to it.
//! - The optional budget caps the whole logical call: when the next sleep
//!   would cross it, the last failure returns immediately instead of
//!   sleeping past the caller's deadline.
//!
//! Time and sleep come from a [`Clock`] so tests drive retries with
//! [`ManualClock`] (no wall-clock waits) while production uses [`TokioClock`].

use std::{
    collections::hash_map::RandomState,
    future::Future,
    hash::{BuildHasher as _, Hasher as _},
    time::Duration,
};

#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex;

use super::error::ProviderError;

/// How one logical call retries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    /// Attempts before giving up, at least 1.
    pub max_attempts: u32,
    /// Backoff after the first failure, before jitter.
    pub base_delay: Duration,
    /// Backoff never exceeds this, before jitter.
    pub max_delay: Duration,
    /// Multiply each computed backoff by a 0.5..1.5 jitter factor.
    pub jitter: bool,
    /// Wall-clock cap for the whole logical call, sleeps included. `None`
    /// means attempts alone bound the call.
    pub budget: Option<Duration>,
}

impl RetryPolicy {
    /// Three attempts, 1s base doubling to a 10s cap, jittered, no budget.
    #[must_use]
    pub const fn new(max_attempts: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_attempts: if max_attempts < 1 { 1 } else { max_attempts },
            base_delay,
            max_delay,
            jitter: true,
            budget: None,
        }
    }

    /// Ports v2's `with_retry` defaults.
    #[must_use]
    pub const fn default_policy() -> Self {
        Self::new(3, Duration::from_secs(1), Duration::from_secs(10))
    }

    /// Disable jitter for deterministic tests.
    #[must_use]
    pub const fn without_jitter(mut self) -> Self {
        self.jitter = false;
        self
    }

    /// Cap the whole logical call at `budget` from its start.
    #[must_use]
    pub const fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = Some(budget);
        self
    }

    /// The computed backoff before attempt `attempt` (1-based) fails over to
    /// the next attempt, without jitter and without any `Retry-After`.
    #[must_use]
    pub fn backoff_for_attempt(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(31);
        let grown = self.base_delay.saturating_mul(1 << shift);
        grown.min(self.max_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::default_policy()
    }
}

/// Time and sleep for retries. Production uses [`TokioClock`]; tests use
/// [`ManualClock`], whose clock only moves through `sleep`, so retry tests
/// never wait on the wall.
pub trait Clock: Send + Sync {
    /// A timestamp marking the logical call's start.
    type Mark: Copy + Send;
    /// The current time.
    fn now(&self) -> Self::Mark;
    /// Time elapsed since a mark.
    fn elapsed(&self, since: Self::Mark) -> Duration;
    /// Wait out one backoff.
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

/// Real time and real sleep.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioClock;

impl Clock for TokioClock {
    type Mark = tokio::time::Instant;

    fn now(&self) -> Self::Mark {
        tokio::time::Instant::now()
    }

    fn elapsed(&self, since: Self::Mark) -> Duration {
        self.now().saturating_duration_since(since)
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// A scripted clock for tests: `sleep` records the delay and advances the
/// fake clock instead of waiting.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct ManualClock {
    state: Mutex<ManualState>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
struct ManualState {
    now: Duration,
    sleeps: Vec<Duration>,
}

#[cfg(any(test, feature = "test-support"))]
impl ManualClock {
    /// A clock starting at zero with no recorded sleeps.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every delay passed to `sleep`, in order.
    #[must_use]
    pub fn sleeps(&self) -> Vec<Duration> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .sleeps
            .clone()
    }

    /// Move the clock without recording a sleep (for budget tests where the
    /// operation itself burns time).
    pub fn advance(&self, duration: Duration) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .now += duration;
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Clock for ManualClock {
    type Mark = Duration;

    fn now(&self) -> Self::Mark {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .now
    }

    fn elapsed(&self, since: Self::Mark) -> Duration {
        self.now().saturating_sub(since)
    }

    async fn sleep(&self, duration: Duration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.now += duration;
        state.sleeps.push(duration);
    }
}

/// Run one logical provider call with retry.
///
/// `idempotent` gates retrying entirely: writes pass `false` and run exactly
/// once. Reads pass `true` and retry retriable failures until attempts or
/// budget run out. A `Retry-After` on the failure overrides that sleep's
/// backoff; it is still subject to the budget.
///
/// Returns the first success, or the last failure when nothing succeeded.
pub async fn execute<C, T, F, Fut>(
    policy: &RetryPolicy,
    clock: &C,
    idempotent: bool,
    mut operation: F,
) -> Result<T, ProviderError>
where
    C: Clock,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>> + Send,
{
    let started = clock.now();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match operation().await {
            Ok(value) => return Ok(value),
            Err(failure) => {
                if !idempotent || !failure.is_retriable() || attempt >= policy.max_attempts {
                    return Err(failure);
                }
                let mut delay = failure.retry_after().unwrap_or_else(|| {
                    let backoff = policy.backoff_for_attempt(attempt);
                    if policy.jitter {
                        apply_jitter(backoff, jitter_seed(clock, started, attempt))
                    } else {
                        backoff
                    }
                });
                if delay.is_zero() {
                    // A zero Retry-After still paces attempts apart.
                    delay = Duration::from_millis(1);
                }
                if let Some(budget) = policy.budget {
                    let remaining = budget.saturating_sub(clock.elapsed(started));
                    if delay >= remaining {
                        return Err(failure);
                    }
                }
                clock.sleep(delay).await;
            }
        }
    }
}

/// Jitter factor in 0.5..1.5, ports v2's `delay * (0.5 + random())`.
fn apply_jitter(delay: Duration, seed: u64) -> Duration {
    let factor = 0.5 + unit_from_seed(seed);
    delay.mul_f64(factor)
}

fn unit_from_seed(seed: u64) -> f64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(seed);
    let bits = hasher.finish() >> 11;
    (bits as f64) / ((1u64 << 53) as f64)
}

fn jitter_seed<C: Clock>(clock: &C, started: C::Mark, attempt: u32) -> u64 {
    clock.elapsed(started).as_nanos() as u64 ^ (u64::from(attempt) << 32) ^ 0x9E37_79B9_7F4A_7C15
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn transport() -> ProviderError {
        ProviderError::Transport {
            provider: "musicbrainz",
            message: "reset".to_owned(),
        }
    }

    #[test]
    fn backoff_doubles_to_the_cap() {
        let policy = RetryPolicy::new(5, Duration::from_secs(1), Duration::from_secs(3));
        assert_eq!(policy.backoff_for_attempt(1), Duration::from_secs(1));
        assert_eq!(policy.backoff_for_attempt(2), Duration::from_secs(2));
        assert_eq!(policy.backoff_for_attempt(3), Duration::from_secs(3));
        assert_eq!(policy.backoff_for_attempt(9), Duration::from_secs(3));
        assert_eq!(
            RetryPolicy::new(0, Duration::ZERO, Duration::ZERO).max_attempts,
            1
        );
    }

    #[tokio::test]
    async fn flaky_read_succeeds_after_scripted_backoffs() {
        let policy = RetryPolicy::default_policy().without_jitter();
        let clock = ManualClock::new();
        let calls = AtomicUsize::new(0);
        let outcome = execute(&policy, &clock, true, || {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if call < 3 {
                    Err(transport())
                } else {
                    Ok("found")
                }
            }
        })
        .await;
        assert_eq!(outcome, Ok("found"));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            clock.sleeps(),
            [Duration::from_secs(1), Duration::from_secs(2)]
        );
    }

    #[tokio::test]
    async fn writes_run_exactly_once() {
        let policy = RetryPolicy::default_policy().without_jitter();
        let clock = ManualClock::new();
        let calls = AtomicUsize::new(0);
        let outcome = execute(&policy, &clock, false, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(transport()) }
        })
        .await;
        assert!(outcome.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(clock.sleeps().is_empty());
    }

    #[tokio::test]
    async fn non_retriable_failures_return_immediately() {
        let policy = RetryPolicy::default_policy().without_jitter();
        let clock = ManualClock::new();
        let calls = AtomicUsize::new(0);
        let denied = ProviderError::Forbidden { provider: "lastfm" };
        let outcome = execute(&policy, &clock, true, || {
            calls.fetch_add(1, Ordering::SeqCst);
            let denied = denied.clone();
            async move { Err::<(), _>(denied) }
        })
        .await;
        assert_eq!(outcome, Err(denied));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(clock.sleeps().is_empty());
    }

    #[tokio::test]
    async fn retry_after_replaces_the_backoff() {
        let policy =
            RetryPolicy::new(3, Duration::from_secs(1), Duration::from_secs(2)).without_jitter();
        let clock = ManualClock::new();
        let calls = AtomicUsize::new(0);
        let outcome = execute(&policy, &clock, true, || {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if call == 1 {
                    Err(ProviderError::RateLimited {
                        provider: "musicbrainz",
                        retry_after: Some(Duration::from_secs(30)),
                    })
                } else {
                    Ok(())
                }
            }
        })
        .await;
        assert_eq!(outcome, Ok(()));
        // The server's 30s won over the computed 1s backoff.
        assert_eq!(clock.sleeps(), [Duration::from_secs(30)]);
    }

    #[tokio::test]
    async fn budget_stops_before_a_sleep_that_crosses_it() {
        let policy = RetryPolicy::new(5, Duration::from_secs(5), Duration::from_secs(5))
            .without_jitter()
            .with_budget(Duration::from_secs(6));
        let clock = ManualClock::new();
        let calls = AtomicUsize::new(0);
        let outcome = execute(&policy, &clock, true, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(transport()) }
        })
        .await;
        assert!(outcome.is_err());
        // First sleep (5s) fits; the second would cross the 6s budget.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(clock.sleeps(), [Duration::from_secs(5)]);
    }

    #[tokio::test]
    async fn exhausted_attempts_return_the_last_failure() {
        let policy = RetryPolicy::new(2, Duration::from_millis(10), Duration::from_millis(10))
            .without_jitter();
        let clock = ManualClock::new();
        let outcome = execute(&policy, &clock, true, || async {
            Err::<(), _>(transport())
        })
        .await;
        assert_eq!(outcome, Err(transport()));
        assert_eq!(clock.sleeps(), [Duration::from_millis(10)]);
    }

    #[tokio::test]
    async fn jitter_stays_within_half_to_one_and_a_half() {
        let policy = RetryPolicy::new(40, Duration::from_secs(4), Duration::from_secs(4));
        let clock = ManualClock::new();
        let outcome = execute(&policy, &clock, true, || async {
            Err::<(), _>(transport())
        })
        .await;
        assert!(outcome.is_err());
        for sleep in clock.sleeps() {
            assert!(
                sleep >= Duration::from_secs(2) && sleep <= Duration::from_secs(6),
                "jittered {sleep:?} escaped 0.5x..1.5x of 4s"
            );
        }
    }
}
