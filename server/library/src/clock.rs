//! The units the library's tables keep, read from the kernel clock passed
//! in: unix seconds as a float and the unix day.

use std::time::UNIX_EPOCH;

use droppedneedle_kernel::clock::Clock;

/// Unix seconds and days on any kernel clock.
pub(crate) trait LibraryClock {
    /// Now, as unix seconds at the clock's full resolution.
    fn now_secs(&self) -> f64;

    /// Now, as unix milliseconds in the unsigned form job stamps use.
    fn now_ms_u64(&self) -> u64;

    /// Today, as days since the unix epoch.
    fn today_day(&self) -> i64;
}

impl<C: Clock + ?Sized> LibraryClock for C {
    fn now_secs(&self) -> f64 {
        self.now_system()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |span| span.as_secs_f64())
    }

    fn now_ms_u64(&self) -> u64 {
        u64::try_from(self.now_ms()).unwrap_or(0)
    }

    fn today_day(&self) -> i64 {
        self.now_ms().div_euclid(86_400_000)
    }
}

/// Unix seconds as unix milliseconds, for a change that stamps a
/// millisecond column with the same time as its other rows.
pub(crate) fn ms(secs: f64) -> i64 {
    (secs * 1000.0).round() as i64
}
