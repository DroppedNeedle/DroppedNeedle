//! The import job: one [`KIND`] run, keyed by the person, reads the next
//! month file of their upload, keeps its listens and queues the next run,
//! until the file is read; then the import is current and the file is
//! deleted.
//!
//! A run is safe to repeat: a listen is kept once, and a month counts as
//! read only in the write that also completes the run, which checks the
//! import has not moved on. A busy database puts the run back without
//! spending an attempt. A file that cannot be opened marks the import
//! failed; a month file that cannot be read is passed over with a log line.
//! Every run first sweeps the files no import names any more.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};

use super::archive::Export;
use super::store::{HistoryStore, WriteError};
use crate::playback::sqlite::off_worker;

/// The import job kind.
pub const KIND: JobKind = JobKind::new("listens.import");

/// Tries a run gets before the import is marked failed.
const MAX_ATTEMPTS: u32 = 5;

/// How import runs go: one month file each, so a run is short; done rows
/// are kept an hour, since a long history leaves one a month.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(MAX_ATTEMPTS)
    .run_timeout(Duration::from_secs(5 * 60))
    .keep_done(Duration::from_secs(3600));

/// An import job's payload: everything a run needs is in the person's row.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct ImportJob {}

/// The one payload every import job carries.
pub(crate) const IMPORT_JOB: ImportJob = ImportJob {};

/// Runs [`KIND`] jobs over the one history store.
pub struct ImportJobs {
    store: Arc<HistoryStore>,
}

impl ImportJobs {
    /// The handler over `store`.
    pub fn new(store: Arc<HistoryStore>) -> Self {
        Self { store }
    }
}

impl JobHandler for ImportJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let store = Arc::clone(&self.store);
        Box::pin(async move { run(&store, job).await })
    }
}

async fn run(store: &HistoryStore, job: JobCtx) -> Outcome {
    let user_id = job.key().to_owned();
    store.sweep().await;
    let reading = match store.reading(&user_id).await {
        Ok(Some(reading)) => reading,
        // Forgotten, replaced by a newer upload, already read, or the
        // account is gone; the sweep takes a file left behind.
        Ok(None) => return Outcome::Done,
        Err(DbError::Busy { .. }) => return Outcome::Busy,
        Err(error) if job.attempt() >= MAX_ATTEMPTS => {
            tracing::warn!(%error, "listening history import not read; the import failed");
            return failed(store, &user_id, None, job).await;
        }
        Err(error) => {
            tracing::warn!(%error, "listening history import not read; trying again");
            return retry();
        }
    };
    let path = store.archive(&user_id, &reading.import_id);
    let position = reading.entries_read;
    let opened = off_worker(move || {
        let mut export = Export::open(&path)?;
        let months = export.months().len();
        Ok::<_, super::archive::ArchiveError>((months, export.read(position)))
    });
    let (months, month) = match opened {
        Ok(read) => read,
        Err(error) => {
            tracing::warn!(%error, "a listening history file could not be read");
            return failed(store, &user_id, Some(&reading.import_id), job).await;
        }
    };
    let last = position + 1 >= months;
    let listens = match month {
        Some(month) => {
            if let Some(why) = month.passed_over {
                tracing::warn!(position, why, "a month of listening history passed over");
            }
            if month.skipped > 0 {
                tracing::info!(
                    position,
                    skipped = month.skipped,
                    "lines of listening history that were not listens passed over"
                );
            }
            month.listens
        }
        None => Vec::new(),
    };
    let attempt = job.attempt();
    let finish = job.clone();
    match store
        .write_month(&user_id, &reading, listens, last, move |tx| {
            finish.complete(tx)
        })
        .await
    {
        Ok(added) => {
            tracing::debug!(position, months, added, "a month of listening history kept");
            if last {
                store.remove_file(&user_id, &reading.import_id).await;
                tracing::info!(months, "a listening history import finished");
            }
            Outcome::Done
        }
        // A forget, a newer upload or another run moved the import on.
        Err(WriteError::MovedOn | WriteError::Job(JobError::LeaseLost)) => Outcome::Done,
        Err(error) if error.is_busy() => Outcome::Busy,
        Err(error) if attempt >= MAX_ATTEMPTS => {
            tracing::warn!(%error, "listening history not kept; the import failed");
            failed(store, &user_id, Some(&reading.import_id), job).await
        }
        Err(error) => {
            tracing::warn!(%error, "listening history not kept; trying again");
            retry()
        }
    }
}

/// Mark the import of `import_id` (whichever file, with none) failed,
/// delete the person's files no import reads any more, and end the run.
async fn failed(
    store: &HistoryStore,
    user_id: &str,
    import_id: Option<&str>,
    job: JobCtx,
) -> Outcome {
    match store
        .fail(user_id, import_id, move |tx| job.complete(tx))
        .await
    {
        Ok(()) | Err(WriteError::MovedOn | WriteError::Job(JobError::LeaseLost)) => {
            store.remove_unread(user_id).await;
            Outcome::Done
        }
        Err(error) if error.is_busy() => Outcome::Busy,
        Err(error) => {
            tracing::warn!(%error, "a failed listening history import not recorded");
            retry()
        }
    }
}

fn retry() -> Outcome {
    Outcome::Retry {
        code: "HISTORY_IMPORT_FAILED",
        at: None,
    }
}
