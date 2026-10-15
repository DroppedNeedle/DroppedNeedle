//! Pacing shared by every jobs loop: interval, jitter, and recovery.
//!
//! Each loop owns a [`Schedule`]: how long to wait between cycles, how much
//! random spread to add so restarts do not synchronize fleets of timers, how
//! long to hold off after boot, and how backoff grows while cycles keep
//! failing. The floor ([`Schedule::min_delay`]) is the no-hot-loop guarantee:
//! even a zero interval with instant failures still waits out the floor, so a
//! broken seam burns log lines, never CPU.
//!
//! Randomness is a tiny self-contained splitmix64. The tree carries no RNG
//! dependency and a loop timer does not need cryptographic randomness; tests
//! seed it fixed and production seeds it from the clock.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Smallest gap the scheduler ever returns. Loops clamp their configured
/// floor up to at least this so a zeroed config cannot spin.
pub const ABSOLUTE_MIN_DELAY: Duration = Duration::from_millis(50);

/// How far backoff shifts compound before the cap takes over. Ten doublings
/// turn a 1 s base into ~17 minutes, past any sane cap.
const MAX_BACKOFF_SHIFT: u32 = 10;

/// Pacing for one background loop.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    interval: Duration,
    jitter_max: Duration,
    initial_delay: Duration,
    min_delay: Duration,
    backoff_base: Duration,
    backoff_cap: Duration,
}

impl Schedule {
    /// A plain fixed cadence: no jitter, no boot delay, a 1 s backoff base
    /// capped at 5 minutes, and the absolute floor as the minimum gap.
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            jitter_max: Duration::ZERO,
            initial_delay: Duration::ZERO,
            min_delay: ABSOLUTE_MIN_DELAY,
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(5 * 60),
        }
    }

    /// Spread each cycle by up to this much, drawn uniformly per cycle.
    pub fn with_jitter(mut self, jitter_max: Duration) -> Self {
        self.jitter_max = jitter_max;
        self
    }

    /// Hold-off after boot before the first cycle.
    pub fn with_initial_delay(mut self, initial_delay: Duration) -> Self {
        self.initial_delay = initial_delay;
        self
    }

    /// Smallest gap between cycles, whatever the interval says. Clamped up
    /// to [`ABSOLUTE_MIN_DELAY`] so zero stays safe.
    pub fn with_min_delay(mut self, min_delay: Duration) -> Self {
        self.min_delay = min_delay.max(ABSOLUTE_MIN_DELAY);
        self
    }

    /// Recovery pacing: while cycles fail in a row, each wait grows by
    /// `base * 2^failures` on top of interval plus jitter, capped at `cap`.
    /// One clean cycle resets the count to zero.
    pub fn with_backoff(mut self, base: Duration, cap: Duration) -> Self {
        self.backoff_base = base;
        self.backoff_cap = cap;
        self
    }

    /// Boot hold-off for this schedule.
    pub fn initial_delay(&self) -> Duration {
        self.initial_delay
    }

    /// Delay before the next cycle after `failures` consecutive failures.
    /// Interval plus a fresh jitter draw plus doubled backoff, never below
    /// the floor.
    pub fn next_delay(&self, rng: &mut SplitMix64, failures: u32) -> Duration {
        let jitter = rng.below_duration(self.jitter_max);
        let backoff = if failures == 0 {
            Duration::ZERO
        } else {
            let shift = failures.min(MAX_BACKOFF_SHIFT + 1);
            let grown = self
                .backoff_base
                .checked_mul(1_u32 << shift.min(MAX_BACKOFF_SHIFT))
                .unwrap_or(self.backoff_cap);
            grown.min(self.backoff_cap)
        };
        self.interval
            .saturating_add(jitter)
            .saturating_add(backoff)
            .max(self.min_delay)
    }
}

/// Minimal splitmix64 for jitter draws. Deterministic per seed so tests can
/// pin exact sequences; production seeds from the wall clock.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    /// Fixed seed for tests and benches.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Seed from the current wall clock. Two processes starting in the same
    /// nanosecond still diverge on the first draw per loop, which is enough
    /// spread for timer jitter.
    pub fn seed_from_time() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|span| span.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    /// Next raw output.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    /// Uniform draw below `bound`. Zero bound draws zero.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        // Rejection-free modulo bias is irrelevant for timer jitter.
        self.next_u64() % bound
    }

    /// Uniform draw in `[0, max]`, millisecond resolution.
    pub fn below_duration(&mut self, max: Duration) -> Duration {
        Duration::from_millis(
            self.below(max.as_millis().saturating_add(1).min(u64::MAX as u128) as u64),
        )
    }
}

/// Sleep unless the stop fires first. Returns true when the loop must exit.
/// A stop that fired before this call returns at once; shutdown never waits
/// out a sleep.
pub async fn sleep_or_stop(delay: Duration, stop: &tokio::sync::Notify) -> bool {
    tokio::select! {
        biased;
        _ = stop.notified() => true,
        _ = tokio::time::sleep(delay) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_schedule_returns_interval_without_jitter() {
        let schedule = Schedule::new(Duration::from_secs(30));
        let mut rng = SplitMix64::new(7);
        assert_eq!(schedule.next_delay(&mut rng, 0), Duration::from_secs(30));
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let schedule = Schedule::new(Duration::from_secs(60)).with_jitter(Duration::from_secs(5));
        let mut rng = SplitMix64::new(1);
        for _ in 0..500 {
            let delay = schedule.next_delay(&mut rng, 0);
            assert!(delay >= Duration::from_secs(60));
            assert!(delay <= Duration::from_secs(65) + Duration::from_millis(1));
        }
    }

    #[test]
    fn backoff_doubles_then_caps() {
        let schedule = Schedule::new(Duration::ZERO)
            .with_backoff(Duration::from_secs(1), Duration::from_secs(8));
        let mut rng = SplitMix64::new(3);
        let delays: Vec<Duration> = (1..=6).map(|f| schedule.next_delay(&mut rng, f)).collect();
        assert_eq!(delays[0], Duration::from_secs(2));
        assert_eq!(delays[1], Duration::from_secs(4));
        assert_eq!(delays[2], Duration::from_secs(8));
        assert_eq!(delays[3], Duration::from_secs(8));
        assert_eq!(delays[5], Duration::from_secs(8));
    }

    #[test]
    fn zero_interval_still_waits_out_the_floor() {
        let schedule = Schedule::new(Duration::ZERO).with_min_delay(Duration::ZERO);
        let mut rng = SplitMix64::new(9);
        assert_eq!(schedule.next_delay(&mut rng, 0), ABSOLUTE_MIN_DELAY);
        // Failures stack backoff on top, capped, never spinning.
        assert_eq!(
            schedule.next_delay(&mut rng, 100),
            Duration::from_secs(5 * 60)
        );
    }

    #[test]
    fn same_seed_replays_same_draws() {
        let mut first = SplitMix64::new(42);
        let mut second = SplitMix64::new(42);
        for _ in 0..32 {
            assert_eq!(first.next_u64(), second.next_u64());
        }
    }
}
