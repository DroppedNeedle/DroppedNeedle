//! Wall-clock helpers shared by the library service and loops.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix time in milliseconds.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_millis() as u64)
        .unwrap_or(0)
}

/// Current unix time in seconds.
pub(crate) fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// Current unix day.
pub(crate) fn today_day() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| (span.as_secs() / 86_400) as i64)
        .unwrap_or(0)
}
