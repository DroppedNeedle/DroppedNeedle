//! Library activity: the activity feed and the identification pause
//! switch.
//!
//! [`feed`] builds what `GET /library/activity` answers from the scan
//! runs and the identification queue. [`store`] holds the SQL. This file
//! has the service methods on [`LibrarySetup`]. The `activity.changed`
//! event stream reads the same change revisions from the event hub's own
//! poller.

pub mod feed;
pub mod store;

use self::feed::{FeedInputs, LibraryActivity};
use self::store::{ControlError, IdentificationControl};
use super::clock::now_unix;
use super::scan::models::{Counters, ScanRun};
use super::service::ServiceError;
use super::wiring::LibrarySetup;

/// Answer to a pause or resume of identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentificationControlAnswer {
    /// `running`, `pausing` (paused with a job still finishing) or
    /// `paused`.
    pub state: &'static str,
    pub row_revision: u64,
}

fn store_error(error: rusqlite::Error) -> ServiceError {
    ServiceError::internal(&error)
}

impl LibrarySetup {
    /// The activity feed. Administrators also see which part of the
    /// library each scan covers. Blocking.
    pub fn library_activity(&self, admin: bool) -> Result<LibraryActivity, ServiceError> {
        let with_counters = |run: ScanRun| -> (ScanRun, Counters) {
            match self.coordinator.snapshot(&run.id) {
                Ok((fresh, _, counters)) => (fresh, counters),
                Err(_) => {
                    let counters = run.counters.clone();
                    (run, counters)
                }
            }
        };
        let runs: Vec<(ScanRun, Counters)> = self
            .coordinator
            .current()
            .into_iter()
            .map(with_counters)
            .collect();
        let latest = self
            .coordinator
            .history(1)
            .into_iter()
            .next()
            .map(with_counters);
        let now = now_unix();
        let (identification, revisions) = self
            .scan_store
            .with_connection(|conn| {
                Ok::<_, rusqlite::Error>((
                    store::identification_snapshot(conn, now)?,
                    store::library_revisions(conn)?,
                ))
            })
            .map_err(store_error)?;
        Ok(feed::build(FeedInputs {
            runs: &runs,
            latest_terminal: latest.as_ref(),
            identification: &identification,
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
        let now = now_unix();
        let (revision, snapshot) = self
            .scan_store
            .with_connection(|conn| {
                let requested_by = paused.then_some(user_id);
                let revision =
                    store::set_identification_paused(conn, paused, requested_by, now, expected)?;
                let snapshot = store::identification_snapshot(conn, now)?;
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
                ControlError::Store(error) => store_error(error),
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

    /// True while identification is paused. The identify loop checks this
    /// before it claims each job. An unreadable switch reads as running,
    /// with the error logged: a paused queue that cannot be read should
    /// not freeze identification for good. Blocking.
    pub fn identification_paused(&self) -> bool {
        match self
            .scan_store
            .with_connection(|conn| store::identification_control(conn))
        {
            Ok(IdentificationControl { paused, .. }) => paused,
            Err(error) => {
                tracing::warn!(%error, "cannot read the identification pause switch");
                false
            }
        }
    }
}
