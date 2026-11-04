//! Wall time for the kernel and every crate above it.
//!
//! The one injectable wall clock: the loop supervisor, the jobs table,
//! sessions, playback, discover and contributions all read it, so tests pin
//! it with a `ManualClock` from the test support. Sleeps never go through
//! it: they use tokio time, which tests pause.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Wall time, behind a trait because tests pin it.
pub trait Clock: Send + Sync + 'static {
    /// Now, as unix milliseconds.
    fn now_ms(&self) -> i64;

    /// Now, as unix seconds.
    fn now_unix(&self) -> i64 {
        self.now_ms().div_euclid(1000)
    }

    /// Now, as a system time. The system clock keeps its full resolution,
    /// so float-second stamps taken in quick succession stay in order.
    fn now_system(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(u64::try_from(self.now_ms()).unwrap_or(0))
    }
}

/// The system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|span| i64::try_from(span.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    fn now_system(&self) -> SystemTime {
        SystemTime::now()
    }
}
