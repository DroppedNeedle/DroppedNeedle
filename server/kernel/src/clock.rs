//! Wall time for the kernel and every crate above it.
//!
//! The one injectable wall clock: the loop supervisor, the jobs table,
//! sessions, playback, discover and contributions all read it, so tests pin
//! it with a [`ManualClock`]. Sleeps never go through it: they use tokio
//! time, which tests pause.

#[cfg(any(test, feature = "test-support"))]
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
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

/// A clock that moves only when a test sets it.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default, Clone)]
pub struct ManualClock {
    now_ms: Arc<AtomicI64>,
}

#[cfg(any(test, feature = "test-support"))]
impl ManualClock {
    /// A clock reading `now_ms`.
    pub fn at(now_ms: i64) -> Self {
        Self {
            now_ms: Arc::new(AtomicI64::new(now_ms)),
        }
    }

    /// Set the time.
    pub fn set(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Move the time by `by_ms`.
    pub fn advance(&self, by_ms: i64) {
        self.now_ms.fetch_add(by_ms, Ordering::SeqCst);
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}
