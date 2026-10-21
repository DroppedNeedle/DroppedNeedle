//! SQLite stores behind the requests surface.
//!
//! [`RequestStore`] keeps the request ledger in `request_history`, its
//! listeners in `request_history_requesters` and per-user hides in
//! `request_history_dismissals`. [`WantedStore`] keeps `wanted_watches`,
//! shared by the wanted view and the watcher loop. [`PersonalMixStore`] and
//! [`EditionStore`] keep the mix approvals and the in-flight edition
//! acquires; the follow approvals live with the follows. Reads use the
//! reader pool; every mutation is one writer-lane transaction, so each
//! compare-and-swap reads and writes the row under the write lock.
//!
//! Album rows key on the lowercased release-group MBID and track rows on
//! `track:` plus the lowercased recording MBID (the v2 key scheme). Rows
//! carry epoch seconds; the request, watch and approval tables store
//! integer milliseconds, converted only by [`ms_from_secs`] and
//! [`secs_from_ms`].

mod editions;
mod mixes;
mod requests;
mod wanted;

pub use editions::EditionStore;
pub use mixes::PersonalMixStore;
pub use requests::RequestStore;
pub use wanted::{WantedStore, WatchChange};
pub(crate) use wanted::{rearm_after_removal, stop_after_removal};

use super::error::RequestsError;
use droppedneedle_kernel::db::{DbError, map_sqlx_busy};

/// Map a writer-lane failure: busy stays retryable, the rest is a fault.
pub(crate) fn lane_error(operation: &'static str, error: DbError) -> RequestsError {
    match error {
        DbError::Busy { .. } | DbError::LaneClosed => RequestsError::Busy {
            operation: operation.to_owned(),
        },
        other => RequestsError::internal(&format_args!("{operation}: {other}")),
    }
}

/// Map a reader-pool failure the same way.
pub(crate) fn read_error(operation: &'static str, error: sqlx::Error) -> RequestsError {
    lane_error(operation, map_sqlx_busy(operation, error))
}

/// Personal-mix approval states (`personal_mix_approvals` has no CHECK yet).
pub(super) const APPROVAL_PENDING: &str = "pending";
pub(super) const APPROVAL_APPROVED: &str = "approved";
pub(super) const APPROVAL_REVOKED: &str = "revoked";

/// Epoch seconds as stored: request, watch and approval times are integer
/// milliseconds.
pub(crate) fn ms_from_secs(seconds: u64) -> i64 {
    i64::try_from(seconds)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000)
}

/// Fractional epoch seconds (a library removal's time) as stored.
pub(super) fn ms_from_secs_f64(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

/// A stored millisecond time as epoch seconds, clamping negatives to zero.
pub(crate) fn secs_from_ms(millis: i64) -> u64 {
    u64::try_from(millis / 1000).unwrap_or(0)
}

/// Personal-mix times are still REAL epoch seconds.
pub(super) fn epoch_from_real(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}
