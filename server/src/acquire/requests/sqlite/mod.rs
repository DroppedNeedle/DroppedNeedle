//! SQLite stores behind the requests surface.
//!
//! [`RequestStore`] keeps the request ledger in `request_history`, its
//! listeners in `request_history_requesters` and per-user hides in
//! `request_history_dismissals`. [`WantedStore`] keeps `wanted_watches`,
//! shared by the wanted view and the watcher loop. [`FollowApprovalStore`],
//! [`PersonalMixStore`] and [`EditionStore`] keep the approval queues and
//! the in-flight edition acquires. Reads use the reader pool; every
//! mutation is one writer-lane transaction, so each compare-and-swap reads
//! and writes the row under the write lock.
//!
//! Album rows key on the lowercased release-group MBID and track rows on
//! `track:` plus the lowercased recording MBID (the v2 key scheme). Times
//! are epoch seconds.

mod approvals;
mod editions;
mod mixes;
mod requests;
mod wanted;

pub use approvals::FollowApprovalStore;
pub use editions::EditionStore;
pub use mixes::PersonalMixStore;
pub use requests::RequestStore;
pub use wanted::{WantedStore, WatchChange};

use super::error::RequestsError;
use crate::db::{DbError, map_sqlx_busy};

/// Map a writer-lane failure: busy stays retryable, the rest is a fault.
fn lane_error(operation: &'static str, error: DbError) -> RequestsError {
    match error {
        DbError::Busy { .. } | DbError::LaneClosed => RequestsError::Busy {
            operation: operation.to_owned(),
        },
        other => RequestsError::internal(&format_args!("{operation}: {other}")),
    }
}

/// Map a reader-pool failure the same way.
fn read_error(operation: &'static str, error: sqlx::Error) -> RequestsError {
    lane_error(operation, map_sqlx_busy(operation, error))
}

/// Approval states.
pub(super) const APPROVAL_PENDING: &str = "pending";
pub(super) const APPROVAL_APPROVED: &str = "approved";
pub(super) const APPROVAL_REVOKED: &str = "revoked";

/// v2 stores watch times as REAL epoch seconds.
pub(super) fn epoch_from_real(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}
