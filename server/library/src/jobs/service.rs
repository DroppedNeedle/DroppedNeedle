//! The operations service: what the HTTP handlers and the worker loop call.
//! Its methods sit with the feature they belong to, in `edition`,
//! `reidentify`, `reviews` and `catalog_actions`.
//!
//! Reads and writes go through the shared database's blocking calls, so
//! every method runs off the async workers (handlers use `spawn_blocking`;
//! the loop drives its tick on a blocking thread). Requests write on the
//! foreground lane, the worker on the background lane.

use std::sync::Arc;

use rusqlite::Connection;

use crate::clock::LibraryClock as _;
use crate::identify::service::IdentifyService;
use crate::identify::sqlite::SqliteIdentifyStore;
use crate::jobs::OperationJob;
use crate::jobs::models::Control;
use crate::jobs::models::OperationDetail;
use crate::jobs::models::OperationError;
use crate::jobs::{self, OperationKind};
use crate::reasons;
use crate::reidentify::store;
use crate::scan::watcher::WorkWakeups;
use crate::settings::LibrarySettings;
use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::LibraryManagement;

/// Operation jobs over the library's identify store and service.
#[derive(Clone)]
pub struct Operations {
    pub(crate) db: Db,
    pub(crate) store: Arc<SqliteIdentifyStore>,
    pub(crate) identify: Arc<IdentifyService>,
    pub(crate) ids: Arc<dyn IdGenerator>,
    pub(crate) management: Live<LibraryManagement>,
    pub(crate) wakeups: WorkWakeups,
    pub(crate) clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    /// Signs bulk review previews with the server's data key.
    pub(crate) signer: LibrarySettings,
}

impl Operations {
    pub fn new(
        db: Db,
        store: Arc<SqliteIdentifyStore>,
        identify: Arc<IdentifyService>,
        ids: Arc<dyn IdGenerator>,
        settings: &LibrarySettings,
        wakeups: WorkWakeups,
        clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    ) -> Self {
        Self {
            db,
            store,
            identify,
            ids,
            management: settings.management.clone(),
            wakeups,
            clock,
            signer: settings.clone(),
        }
    }

    /// Pause, resume, or stop a job. Blocking.
    pub fn control(
        &self,
        job_id: &str,
        control: Control,
        expected_row_revision: i64,
        idempotency_key: Option<&str>,
    ) -> Result<OperationJob, OperationError> {
        let job = self
            .db
            .write_blocking(Lane::Foreground, "operations.control", |tx| {
                crate::jobs::store::request_control(
                    tx,
                    job_id,
                    control,
                    expected_row_revision,
                    idempotency_key,
                    self.clock.now_secs(),
                )
            })?;
        if let Some(kind) = OperationKind::parse(&job.kind) {
            jobs::wake(&self.wakeups, kind);
        }
        Ok(job)
    }
}

/// One job with its results and, for a re-identification, its candidates,
/// read on `conn`. Callers that read other rows too do it in the same read,
/// so every row comes from one commit.
pub(crate) fn job_detail(
    conn: &Connection,
    job_id: &str,
) -> Result<OperationDetail, OperationError> {
    let job =
        jobs::job(conn, job_id)?.ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))?;
    let (results, results_truncated) = crate::jobs::store::work_results(conn, job_id)?;
    let (candidates, selected_candidate_key) = if job.is_reidentification() {
        match store::snapshot(conn, job_id)? {
            Some(snap) => (
                snap.evaluation.map(|e| e.candidates).unwrap_or_default(),
                snap.selected_candidate_key,
            ),
            None => (Vec::new(), None),
        }
    } else {
        (Vec::new(), None)
    };
    Ok(OperationDetail {
        job,
        results,
        results_truncated,
        candidates,
        selected_candidate_key,
    })
}
