//! A scan run's stop flag and the scan worker's answer to a busy database.
//!
//! The coordinator holds one [`RunStop`] per drive. A pause or stop request
//! for the run trips it, and so does the shutdown signal. The drive keeps
//! its checkpoint between windows; the flag is what ends a long read, a
//! chunked write or a wait on a busy database early.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use droppedneedle_kernel::db::CancelFlag;

use super::store::ScanStoreError;

/// First wait after a busy answer; it doubles up to [`BUSY_WAIT_MAX`].
const BUSY_WAIT_FIRST: Duration = Duration::from_millis(100);
/// Longest wait between two tries of a busy write.
const BUSY_WAIT_MAX: Duration = Duration::from_secs(5);
/// How often a wait looks at the stop flag.
const STOP_POLL: Duration = Duration::from_millis(50);

/// One drive's stop flag. Clones share it.
#[derive(Clone, Default)]
pub struct RunStop {
    flag: CancelFlag,
    tripped: Arc<AtomicBool>,
}

impl RunStop {
    /// A flag nothing trips: boot recovery, sweeps and tests.
    pub fn never() -> Self {
        Self::default()
    }

    /// Ask the drive to stop: a long read or a chunked write ends at its
    /// next boundary and a busy write stops waiting.
    pub fn trip(&self) {
        self.tripped.store(true, Ordering::Release);
        self.flag.cancel();
    }

    /// True once [`Self::trip`] ran.
    pub fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::Acquire)
    }

    /// The flag the database checks.
    pub fn flag(&self) -> &CancelFlag {
        &self.flag
    }
}

/// Run one database call of the scan worker. `Busy` means the file lock
/// was held at `BEGIN` and nothing landed, so the same call runs again
/// after a [`Backoff`] pause, until it lands or the run is asked to stop;
/// then it answers `Stopped`. Every other answer is returned as it is.
pub fn retry_busy<T>(
    stop: &RunStop,
    what: &'static str,
    mut call: impl FnMut() -> Result<T, ScanStoreError>,
) -> Result<T, ScanStoreError> {
    let mut backoff = Backoff::new();
    loop {
        match call() {
            Err(ScanStoreError::Busy { .. }) => backoff.pause(stop, what)?,
            other => return other,
        }
    }
}

/// The wait between two tries of a write the database was too busy for:
/// 100 ms with jitter, doubling to 5 s.
pub struct Backoff {
    wait: Duration,
}

impl Backoff {
    pub fn new() -> Self {
        Self {
            wait: BUSY_WAIT_FIRST,
        }
    }

    /// Log the retry and wait, or answer `Stopped` once the run is asked
    /// to stop.
    pub fn pause(&mut self, stop: &RunStop, what: &'static str) -> Result<(), ScanStoreError> {
        if stop.is_tripped() {
            return Err(ScanStoreError::Stopped);
        }
        let pause = jittered(self.wait);
        tracing::warn!(
            what,
            wait_ms = pause.as_millis() as u64,
            "the database is busy; the scan tries the write again"
        );
        if !sleep_unless_stopped(stop, pause) {
            return Err(ScanStoreError::Stopped);
        }
        self.wait = (self.wait * 2).min(BUSY_WAIT_MAX);
        Ok(())
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Between half and all of `wait`.
fn jittered(wait: Duration) -> Duration {
    let mut bytes = [0u8; 4];
    let fraction = match getrandom::fill(&mut bytes) {
        Ok(()) => f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX),
        Err(_) => 1.0,
    };
    wait.mul_f64(0.5 + fraction / 2.0)
}

/// Sleep for `pause`, or less when the stop flag trips. False when it did.
fn sleep_unless_stopped(stop: &RunStop, pause: Duration) -> bool {
    let until = Instant::now() + pause;
    loop {
        if stop.is_tripped() {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(STOP_POLL));
    }
}
