//! Library activity: the activity feed and the identification pause
//! switch.
//!
//! [`feed`] builds what `GET /library/activity` answers from the scan
//! runs and the identification queue. [`store`] holds the SQL. This file
//! has the [`Activity`] service. The `activity.changed`
//! event stream reads the same change revisions from the event hub's own
//! poller.

pub mod feed;
pub mod store;

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::writer::Lane;
use droppedneedle_kernel::db::{Db, DbError};

use self::feed::{FeedInputs, LibraryActivity};
use self::store::{ControlError, IdentificationControl};
use super::clock::LibraryClock as _;
use super::scan::models::{Counters, ScanRun};
use super::scan::sqlite_store::SqliteScanStore;
use super::scan::store::ScanStoreError;
use super::scan::watcher::WorkWakeups;
use super::scans::store_error;
use super::service::ServiceError;
use super::wiring::ScanCoordinator;

/// Answer to a pause or resume of identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentificationControlAnswer {
    /// `running`, `pausing` (paused with a job still finishing) or
    /// `paused`.
    pub state: &'static str,
    pub row_revision: u64,
}

/// The activity feed and the identification pause switch.
#[derive(Clone)]
pub struct Activity {
    pub(crate) coordinator: Arc<ScanCoordinator>,
    pub(crate) scan_store: Arc<SqliteScanStore>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) wakeups: WorkWakeups,
}

impl Activity {
    /// The activity feed. Administrators also see which part of the
    /// library each scan covers. Blocking.
    pub fn library_activity(&self, admin: bool) -> Result<LibraryActivity, ServiceError> {
        let with_counters = |run: ScanRun| -> (ScanRun, Counters) {
            match self.coordinator.snapshot(&run.id) {
                Ok((fresh, _, counters)) => (fresh, counters),
                Err(_) => {
                    let counters = run.counters;
                    (run, counters)
                }
            }
        };
        let runs: Vec<(ScanRun, Counters)> = self
            .coordinator
            .current_runs()
            .map_err(store_error)?
            .into_iter()
            .map(with_counters)
            .collect();
        let latest = self
            .coordinator
            .history(1)
            .map_err(store_error)?
            .into_iter()
            .next()
            .map(with_counters);
        let now = self.clock.now_secs();
        // One snapshot, so the counts and the revisions agree.
        let (identification, revisions, operations) = self
            .scan_store
            .db()
            .read_blocking("library_activity", |conn| {
                Ok::<_, ScanStoreError>((
                    store::identification_snapshot(conn, now)?,
                    store::library_revisions(conn)?,
                    if admin {
                        store::operation_work(conn)?
                    } else {
                        Vec::new()
                    },
                ))
            })
            .map_err(store_error)?;
        Ok(feed::build(FeedInputs {
            runs: &runs,
            latest_terminal: latest.as_ref(),
            identification: &identification,
            operations: &operations,
            revisions,
            admin,
            // v3 keeps no live MusicBrainz health signal the library can
            // read, so the card never claims the provider is down.
            provider_unavailable: false,
            now,
        }))
    }

    /// Pause (`paused = true`) or resume identification. A job already
    /// running finishes; nothing new starts until resume. `expected` is
    /// the switch revision the caller last saw. Blocking.
    pub fn set_identification_paused(
        &self,
        paused: bool,
        user_id: &str,
        expected: Option<u64>,
    ) -> Result<IdentificationControlAnswer, ServiceError> {
        let now = self.clock.now_secs();
        let (revision, snapshot) = self
            .scan_store
            .db()
            .write_blocking(Lane::Foreground, "identification_pause", |tx| {
                let requested_by = paused.then_some(user_id);
                let revision =
                    store::set_identification_paused(tx, paused, requested_by, now, expected)?;
                let snapshot = store::identification_snapshot(tx, now)?;
                Ok::<_, ControlError>((revision, snapshot))
            })
            .map_err(|error| match error {
                ControlError::Stale => ServiceError::Conflict {
                    message: if paused {
                        "Identification controls changed before Pause was requested.".to_owned()
                    } else {
                        "Identification controls changed before Resume was requested.".to_owned()
                    },
                },
                ControlError::Store(error) => store_error(error.into()),
            })?;
        if !paused {
            self.wakeups.notify("identification");
        }
        let state = match (snapshot.paused, snapshot.running > 0) {
            (true, true) => "pausing",
            (true, false) => "paused",
            (false, _) => "running",
        };
        Ok(IdentificationControlAnswer {
            state,
            row_revision: revision,
        })
    }
}

/// True while identification is paused. The identify loop checks this
/// before each claim to stop early; the claim itself also refuses while
/// paused, so this is only a fast path. An unreadable switch reads as
/// running, with the error logged. Blocking.
pub(crate) fn identification_paused(db: &Db) -> bool {
    match db.read_blocking("identification_switch", |conn| {
        Ok::<_, DbError>(store::identification_control(conn)?)
    }) {
        Ok(IdentificationControl { paused, .. }) => paused,
        Err(error) => {
            tracing::warn!(%error, "cannot read the identification pause switch");
            false
        }
    }
}
