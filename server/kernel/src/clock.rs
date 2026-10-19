//! Wall time for the kernel and the crates above it.
//!
//! The loop supervisor stamps its liveness rows from a [`Clock`], and loops
//! that schedule against the calendar (the events watcher) read it too.
//! Sleeps never go through it: they use tokio time, which tests pause.

#[cfg(any(test, feature = "test-support"))]
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall time, behind a trait because tests pin it.
pub trait Clock: Send + Sync + 'static {
    /// Now, as unix milliseconds.
    fn now_ms(&self) -> i64;

    /// Now, as unix seconds.
    fn now_unix(&self) -> i64 {
        self.now_ms().div_euclid(1000)
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
}

#[cfg(any(test, feature = "test-support"))]
impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}
