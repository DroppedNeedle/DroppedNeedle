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
//! `track:` plus the lowercased recording MBID. Rows
//! carry epoch seconds; the request, watch and approval tables store
//! integer milliseconds, converted only by [`ms_from_secs`] and
//! [`secs_from_ms`].

mod editions;
mod mixes;
mod requests;
mod wanted;

pub use editions::EditionStore;
pub use mixes::PersonalMixStore;
pub(crate) use requests::HistoryOrder;
pub use requests::RequestStore;
pub use wanted::{WantedStore, WatchChange, WatchCheck};
pub(crate) use wanted::{rearm_after_removal, stop_after_removal};

pub(super) use crate::db::ms_from_secs_f64;

use super::error::RequestsError;
use crate::db::AcquireDb;
use droppedneedle_kernel::db::DbError;

/// Map a database failure, from the writer lane or the reader pool: busy
/// stays retryable, the rest (a row that does not decode included) is a
/// fault.
pub(crate) fn lane_error(operation: &'static str, error: DbError) -> RequestsError {
    match error {
        DbError::Busy { .. } | DbError::LaneClosed => RequestsError::Busy {
            operation: operation.to_owned(),
        },
        other => RequestsError::internal(&format_args!("{operation}: {other}")),
    }
}

/// One pooled read, its failure mapped as [`lane_error`] maps a write's.
pub(crate) async fn read<R, F>(
    db: &AcquireDb,
    operation: &'static str,
    op: F,
) -> Result<R, RequestsError>
where
    F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<R> + Send + 'static,
    R: Send + 'static,
{
    db.db()
        .read(operation, move |conn| op(conn).map_err(DbError::from))
        .await
        .map_err(|error| lane_error(operation, error))
}

/// A stored value outside its closed set, as a decode failure of column
/// `index`.
pub(crate) fn unknown_value(index: usize, column: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        format!("{column}: unknown value {value}").into(),
    )
}

/// Epoch seconds as stored: request, watch and approval times are integer
/// milliseconds.
pub(crate) fn ms_from_secs(seconds: u64) -> i64 {
    i64::try_from(seconds)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000)
}

/// A stored millisecond time as epoch seconds, clamping negatives to zero.
pub(crate) fn secs_from_ms(millis: i64) -> u64 {
    u64::try_from(millis / 1000).unwrap_or(0)
}
