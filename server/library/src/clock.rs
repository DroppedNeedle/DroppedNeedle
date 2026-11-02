//! The units the library's tables keep, read from the kernel clock passed
//! in: unix seconds as a float and the unix day.

use std::time::UNIX_EPOCH;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;

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

/// One day in milliseconds. A column that keeps a whole day holds that day's
/// first millisecond.
pub(crate) const DAY_MS: i64 = 86_400_000;

/// Unix milliseconds as unix seconds, the unit the services work in.
pub(crate) fn secs(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

/// About the earliest unix seconds that round to `ms`, to compare a time kept
/// to the millisecond with one still kept in seconds. A time at or after the
/// kept one reads as at or after it, but for a float step at the very edge of
/// the half millisecond.
pub(crate) fn earliest_secs(ms: i64) -> f64 {
    (ms as f64 - 0.5) / 1000.0
}

/// A stored `_at` column, as unix seconds.
pub(crate) fn secs_at(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<f64> {
    Ok(secs(row.get::<_, Millis>(index)?.0))
}

/// A stored nullable `_at` column, as unix seconds.
pub(crate) fn opt_secs_at(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<f64>> {
    Ok(row.get::<_, Option<Millis>>(index)?.map(|at| secs(at.0)))
}
