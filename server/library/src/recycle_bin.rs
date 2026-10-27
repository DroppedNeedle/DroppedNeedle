//! Emptying the recycle bin from its page: a job on the kernel's queue
//! that runs the bin's expiry with no retention, over the copies that
//! reached the bin before the moment the admin read the count. Each
//! attempt adds what it did to the run's row, and the one that finishes
//! does so in the write that finishes the job. A run stopped by a shutdown
//! goes back to the queue without spending an attempt; one that meets bins
//! it can't tell is tried again, and at its attempt limit it fails and the
//! page says it stopped.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::jobs::queue::{
    Enqueued, JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome, PAYLOAD_INVALID, State,
};
use futures_util::future::BoxFuture;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::manage::Publishing;
use crate::publish::expiry::{ExpiryReport, KeptWhy};
use crate::publish::sql;

/// The job that empties the bin.
pub const EMPTY: JobKind = JobKind::new("library.recycle_bin_empty");
/// Its one key: a press while one waits joins it.
const EMPTY_KEY: &str = "recycle-bin";
/// Hashing every copy in a large bin on a network share takes a while.
pub const SPEC: KindSpec = KindSpec::new(EMPTY)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(6 * 3600))
    .backoff(Duration::from_secs(60), Duration::from_secs(1800));
/// A run stopped part way, or could not tell the bins.
const STOPPED: &str = "RECYCLE_BIN_EMPTY_STOPPED";
/// A run failed.
const FAILED: &str = "RECYCLE_BIN_EMPTY_FAILED";
/// Kept copies named in a run's record.
const NAMED_KEPT: usize = 20;

#[derive(Debug, Serialize, Deserialize)]
struct EmptyPayload {
    /// Copies that reached the bin before this, unix ms, go.
    before: i64,
    actor: Option<String>,
}

/// Whether the bin is being emptied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmptyingState {
    /// Nothing running.
    Idle,
    /// A run is waiting or running.
    Emptying,
    /// The last run gave up; what it did is kept, the rest is still in
    /// the bin.
    Stopped,
}

/// A copy a run kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KeptName {
    /// The file's name.
    pub name: String,
    pub why: KeptWhy,
}

/// What the last finished run did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Emptied {
    /// When it finished, unix milliseconds.
    pub at: i64,
    /// Who asked, while the account exists.
    pub by: Option<String>,
    /// Copies deleted, or found already gone.
    pub deleted: u32,
    pub freed_bytes: u64,
    /// Old bin folders no row named, emptied.
    pub older_folders: u32,
    /// Copies kept: changed in the bin, or not where it may delete.
    pub kept: u32,
    /// Up to 20 of them, with why.
    pub kept_files: Vec<KeptName>,
    /// Copies left for later: held by a put-back, or a bin that looked
    /// empty.
    pub waited: u32,
}

/// Queue a run over the copies that reached the bin before `before_ms`.
pub fn request_empty(
    publishing: &Publishing,
    before_ms: i64,
    actor: &str,
) -> Result<Enqueued, JobError> {
    let payload = EmptyPayload {
        before: before_ms,
        actor: Some(actor.to_owned()),
    };
    let enqueued =
        publishing
            .db
            .write_blocking(Lane::Foreground, "recycle_bin.request_empty", |tx| {
                publishing.jobs.enqueue(tx, EMPTY, EMPTY_KEY, &payload)
            })?;
    publishing.jobs.wake();
    Ok(enqueued)
}

/// Whether a run waits, runs, or the last one gave up.
pub async fn state(publishing: &Publishing) -> Result<EmptyingState, DbError> {
    Ok(match publishing.jobs.latest(EMPTY, EMPTY_KEY).await? {
        Some(status) => match status.state {
            State::Queued | State::Running => EmptyingState::Emptying,
            State::Failed => EmptyingState::Stopped,
            State::Done => EmptyingState::Idle,
        },
        None => EmptyingState::Idle,
    })
}

/// What the last finished run did, if one has.
pub fn last_emptied(publishing: &Publishing) -> Result<Option<Emptied>, DbError> {
    type Last = (i64, Option<String>, u32, i64, u32, u32, u32, String);
    let last: Option<Last> = publishing
        .db
        .read_blocking("recycle_bin.last_emptied", |conn| {
            Ok::<_, DbError>(
                conn.prepare_cached(sql::BIN_EMPTIED_LAST.text())?
                    .query_row([], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                        ))
                    })
                    .optional()?,
            )
        })?;
    Ok(last.map(
        |(at, by, deleted, freed, older_folders, kept, waited, names)| Emptied {
            at,
            by,
            deleted,
            freed_bytes: u64::try_from(freed).unwrap_or(0),
            older_folders,
            kept,
            kept_files: serde_json::from_str(&names).unwrap_or_default(),
            waited,
        },
    ))
}

/// The handler the jobs module runs for [`EMPTY`].
pub fn handler(publishing: &Publishing) -> Arc<dyn JobHandler> {
    Arc::new(EmptyHandler(publishing.clone()))
}

struct EmptyHandler(Publishing);

impl JobHandler for EmptyHandler {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let publishing = self.0.clone();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || run(&publishing, &job)).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    tracing::warn!(%error, "emptying the recycle bin stopped");
                    Outcome::Retry {
                        code: FAILED,
                        at: None,
                    }
                }
            }
        })
    }
}

fn run(publishing: &Publishing, job: &JobCtx) -> Outcome {
    let Ok(payload) = job.payload::<EmptyPayload>() else {
        return Outcome::Fail {
            code: PAYLOAD_INVALID,
        };
    };
    let token = job.token().clone();
    let report = match publishing.empty_recycle_bin(payload.before, &|| !token.is_cancelled()) {
        Ok(report) => report,
        Err(error) if error.wrote_nothing() => return Outcome::Busy,
        Err(error) => {
            tracing::warn!(%error, "emptying the recycle bin failed");
            return Outcome::Retry {
                code: FAILED,
                at: None,
            };
        }
    };
    let finished = report.finished;
    match record(publishing, job, &payload, &report) {
        Ok(()) if finished => Outcome::Done,
        Ok(()) => {
            tracing::info!(
                deleted = report.deleted,
                "emptying the recycle bin stopped part way; it runs again"
            );
            if token.is_cancelled() {
                Outcome::Busy
            } else {
                Outcome::Retry {
                    code: STOPPED,
                    at: None,
                }
            }
        }
        Err(error) if error.is_busy() || token.is_cancelled() => Outcome::Busy,
        Err(error) => {
            tracing::warn!(%error, "emptying the recycle bin could not record what it did");
            Outcome::Retry {
                code: FAILED,
                at: None,
            }
        }
    }
}

/// Add what this attempt did to the run's row, kept under the job's id so
/// the attempts of one run add up: deletes, freed bytes and older folders
/// sum; kept and waited are what the latest attempt found, since each one
/// looks at those copies again. A finished attempt finishes the job in the
/// same write.
fn record(
    publishing: &Publishing,
    job: &JobCtx,
    payload: &EmptyPayload,
    report: &ExpiryReport,
) -> Result<(), JobError> {
    let named: Vec<KeptName> = report
        .kept_copies
        .iter()
        .take(NAMED_KEPT)
        .map(|kept| KeptName {
            name: kept
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            why: kept.why,
        })
        .collect();
    let names = serde_json::to_string(&named).map_err(|_| JobError::Payload)?;
    let count = |value: usize| i64::try_from(value).unwrap_or(i64::MAX);
    let id = job.id().to_string();
    let now = publishing.core.clock.now_ms();
    let finished_at = report.finished.then_some(now);
    publishing
        .db
        .write_blocking(Lane::Background, "recycle_bin.emptied", |tx| {
            tx.prepare_cached(sql::BIN_EMPTIED_RECORD.text())?
                .execute(params![
                    id,
                    payload.actor,
                    payload.before,
                    finished_at,
                    count(report.deleted + report.gone),
                    i64::try_from(report.deleted_bytes).unwrap_or(i64::MAX),
                    count(report.legacy_deleted),
                    count(report.changed + report.kept),
                    count(report.waited),
                    names,
                ])?;
            if report.finished {
                job.complete(tx)?;
            }
            Ok(())
        })
}
