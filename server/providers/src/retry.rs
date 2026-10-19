//! Idempotency-aware retry with backoff, jitter, and a budget.
//!
//! [`execute`] runs one logical provider call. Non-idempotent operations
//! (anything that writes: scrobbles, contributions, ratings) run exactly
//! once and never retry, because a retry could double-apply the write.
//! Idempotent reads retry on [`ProviderError::is_retriable`] failures with
//! exponential backoff and full-ish jitter, porting v2's `with_retry`:
//! `min(base * 2^(attempt-1), max) * (0.5 + random())`.
//!
//! Two v2 behaviors carry over on purpose:
//!
//! - A `Retry-After` carried on the failure (429/503) replaces the computed
//!   backoff for that sleep instead of adding to it.
//! - The optional budget caps the whole logical call: when the next sleep
//!   would cross it, the last failure returns immediately instead of
//!   sleeping past the caller's deadline.
//!
//! Time and sleep are tokio's, so tests drive retries on paused time and
//! never wait on the wall.

use std::{
    collections::hash_map::RandomState,
    future::Future,
    hash::{BuildHasher as _, Hasher as _},
    time::Duration,
};

use tokio::time::Instant;

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

/// Run one logical provider call with retry.
///
/// `idempotent` gates retrying entirely: writes pass `false` and run exactly
/// once. Reads pass `true` and retry retriable failures until attempts or
/// budget run out. A `Retry-After` on the failure overrides that sleep's
/// backoff; it is still subject to the budget.
///
/// Returns the first success, or the last failure when nothing succeeded.
pub async fn execute<T, F, Fut>(
    policy: &RetryPolicy,
    idempotent: bool,
    mut operation: F,
) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>> + Send,
{
    let started = Instant::now();
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
                        apply_jitter(backoff, jitter_seed(started, attempt))
                    } else {
                        backoff
                    }
                });
                if delay.is_zero() {
                    // A zero Retry-After still paces attempts apart.
                    delay = Duration::from_millis(1);
                }
                if let Some(budget) = policy.budget {
                    let remaining = budget.saturating_sub(started.elapsed());
                    if delay >= remaining {
                        return Err(failure);
                    }
                }
                tokio::time::sleep(delay).await;
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

fn jitter_seed(started: Instant, attempt: u32) -> u64 {
    started.elapsed().as_nanos() as u64 ^ (u64::from(attempt) << 32) ^ 0x9E37_79B9_7F4A_7C15
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn transport() -> ProviderError {
        ProviderError::Transport {
            provider: "musicbrainz",
            message: "reset".to_owned(),
        }
    }

    /// When each attempt started, on paused tokio time.
    #[derive(Default)]
    struct Calls(Mutex<Vec<Instant>>);

    impl Calls {
        /// Record an attempt and return its 1-based number.
        fn record(&self) -> usize {
            let mut starts = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
            starts.push(Instant::now());
            starts.len()
        }

        fn count(&self) -> usize {
            self.0
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .len()
        }

        /// The sleeps between attempts, in order.
        fn sleeps(&self) -> Vec<Duration> {
            let starts = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
            starts.windows(2).map(|pair| pair[1] - pair[0]).collect()
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

    #[tokio::test(start_paused = true)]
    async fn flaky_read_succeeds_after_scripted_backoffs() {
        let policy = RetryPolicy::default_policy().without_jitter();
        let calls = Calls::default();
        let outcome = execute(&policy, true, || {
            let call = calls.record();
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
        assert_eq!(calls.count(), 3);
        assert_eq!(
            calls.sleeps(),
            [Duration::from_secs(1), Duration::from_secs(2)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn writes_run_exactly_once() {
        let policy = RetryPolicy::default_policy().without_jitter();
        let calls = Calls::default();
        let outcome = execute(&policy, false, || {
            calls.record();
            async { Err::<(), _>(transport()) }
        })
        .await;
        assert!(outcome.is_err());
        assert_eq!(calls.count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn non_retriable_failures_return_immediately() {
        let policy = RetryPolicy::default_policy().without_jitter();
        let calls = Calls::default();
        let denied = ProviderError::Forbidden { provider: "lastfm" };
        let outcome = execute(&policy, true, || {
            calls.record();
            let denied = denied.clone();
            async move { Err::<(), _>(denied) }
        })
        .await;
        assert_eq!(outcome, Err(denied));
        assert_eq!(calls.count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_replaces_the_backoff() {
        let policy =
            RetryPolicy::new(3, Duration::from_secs(1), Duration::from_secs(2)).without_jitter();
        let calls = Calls::default();
        let outcome = execute(&policy, true, || {
            let call = calls.record();
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
        assert_eq!(calls.sleeps(), [Duration::from_secs(30)]);
    }

    #[tokio::test(start_paused = true)]
    async fn budget_stops_before_a_sleep_that_crosses_it() {
        let policy = RetryPolicy::new(5, Duration::from_secs(5), Duration::from_secs(5))
            .without_jitter()
            .with_budget(Duration::from_secs(6));
        let calls = Calls::default();
        let outcome = execute(&policy, true, || {
            calls.record();
            async { Err::<(), _>(transport()) }
        })
        .await;
        assert!(outcome.is_err());
        // First sleep (5s) fits; the second would cross the 6s budget.
        assert_eq!(calls.count(), 2);
        assert_eq!(calls.sleeps(), [Duration::from_secs(5)]);
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_attempts_return_the_last_failure() {
        let policy = RetryPolicy::new(2, Duration::from_millis(10), Duration::from_millis(10))
            .without_jitter();
        let calls = Calls::default();
        let outcome = execute(&policy, true, || {
            calls.record();
            async { Err::<(), _>(transport()) }
        })
        .await;
        assert_eq!(outcome, Err(transport()));
        assert_eq!(calls.sleeps(), [Duration::from_millis(10)]);
    }

    #[tokio::test(start_paused = true)]
    async fn jitter_stays_within_half_to_one_and_a_half() {
        let policy = RetryPolicy::new(40, Duration::from_secs(4), Duration::from_secs(4));
        let calls = Calls::default();
        let outcome = execute(&policy, true, || {
            calls.record();
            async { Err::<(), _>(transport()) }
        })
        .await;
        assert!(outcome.is_err());
        assert_eq!(calls.count(), 40);
        for sleep in calls.sleeps() {
            assert!(
                sleep >= Duration::from_secs(2) && sleep <= Duration::from_secs(6),
                "jittered {sleep:?} escaped 0.5x..1.5x of 4s"
            );
        }
    }
}
