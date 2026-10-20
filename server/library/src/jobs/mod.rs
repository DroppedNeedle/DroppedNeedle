//! Library operation jobs: how a `library_operation_jobs` row and its
//! `library_operation_work` items are queued, claimed, kept alive, put
//! back, halted, finished and recovered. Management, re-identification
//! and repairs keep their own domain rows beside a job and write the job
//! and work rows only through here.
//!
//! The lease is the kernel's lease core (claim under a fresh owner,
//! heartbeat, release, recover) over this table's own statements, so a
//! worker that lost its lease changes nothing. Recovery belongs to each
//! kind's runner: its claim first requeues the jobs of its kind whose lease
//! ran out, and its loop's first turn requeues every job of its kind an
//! earlier process left running. Work items running under a requeued job
//! go back to pending with it.
//!
//! Every function here is plain SQL on the caller's transaction; the
//! services own the transactions. Times are unix seconds. A caller that
//! queues a job wakes its kind's loop with [`wake`] once the write is in.

pub(crate) mod sql;
mod states;

use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::queue::JobError;
use droppedneedle_kernel::jobs::queue::lease::{self, LeaseStatements};
use rusqlite::{Connection, OptionalExtension as _, Transaction, named_params, params};

use crate::scan::watcher::WorkWakeups;

pub use states::{ControlRequest, ManagementPhase, OperationKind, OperationState, WorkState};

/// How long a claim holds a job before recovery may requeue it.
pub const LEASE_SECS: f64 = 60.0;

/// The wakeup the management loop listens on.
pub const MANAGEMENT_WAKEUP: &str = "library-management";
/// The wakeup the operations loop (re-identification) listens on.
pub const OPERATIONS_WAKEUP: &str = "library-operations";
/// The wakeup the repairs loop listens on.
pub const REPAIRS_WAKEUP: &str = "library-repairs";

/// This table's lease statements, for the kernel's lease core.
const STATEMENTS: LeaseStatements = LeaseStatements {
    claim: sql::CLAIM,
    heartbeat: sql::HEARTBEAT,
    release: sql::RELEASE,
    recover_expired: sql::RECOVER_EXPIRED,
    recover_at_start: sql::RECOVER_AT_START,
};

/// One `library_operation_jobs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationJob {
    pub id: String,
    /// The stored kind, kept as text so a row from a newer kind still reads.
    pub kind: String,
    pub state: OperationState,
    pub requested_by_user_id: Option<String>,
    pub expected_work_count: i64,
    pub completed_count: i64,
    pub succeeded_count: i64,
    pub failed_count: i64,
    pub skipped_count: i64,
    pub control_request: ControlRequest,
    pub terminal_code: Option<String>,
    pub reidentification_attempt_count: i64,
    pub row_revision: i64,
    pub event_revision: i64,
    pub created_at: f64,
    pub updated_at: f64,
}

impl OperationJob {
    pub fn is_reidentification(&self) -> bool {
        self.kind == OperationKind::ExplicitReidentification.as_str()
    }
}

fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationJob> {
    Ok(OperationJob {
        id: row.get(0)?,
        kind: row.get(1)?,
        state: row.get(2)?,
        requested_by_user_id: row.get(3)?,
        expected_work_count: row.get(4)?,
        completed_count: row.get(5)?,
        succeeded_count: row.get(6)?,
        failed_count: row.get(7)?,
        skipped_count: row.get(8)?,
        control_request: row.get(9)?,
        terminal_code: row.get(10)?,
        reidentification_attempt_count: row.get(11)?,
        row_revision: row.get(12)?,
        event_revision: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

/// Wake the loop that runs jobs of `kind`.
pub fn wake(wakeups: &WorkWakeups, kind: OperationKind) {
    wakeups.notify(match kind {
        OperationKind::LibraryManagement => MANAGEMENT_WAKEUP,
        OperationKind::Repair => REPAIRS_WAKEUP,
        OperationKind::BulkReviewApply | OperationKind::ExplicitReidentification => {
            OPERATIONS_WAKEUP
        }
    });
}

// ---------------------------------------------------------------------------
// Reading and queueing.
// ---------------------------------------------------------------------------

/// One job, or `None` when the id names none.
pub fn job(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<OperationJob>> {
    conn.prepare_cached(sql::JOB_BY_ID.text())?
        .query_row(params![job_id], map_job)
        .optional()
}

/// The job an idempotency key made, if any.
pub fn job_by_key(conn: &Connection, key: &str) -> rusqlite::Result<Option<OperationJob>> {
    conn.prepare_cached(sql::JOB_BY_KEY.text())?
        .query_row(params![key], map_job)
        .optional()
}

/// A job to queue.
pub struct NewJob<'a> {
    pub id: &'a str,
    pub kind: OperationKind,
    pub requested_by: Option<&'a str>,
    pub idempotency_key: Option<&'a str>,
    /// The catalog revision the job was made against, when it pins one.
    pub catalog_revision: Option<i64>,
    pub expected_work_count: i64,
    pub now: f64,
}

/// Queue a new job.
pub fn insert(tx: &Transaction<'_>, new: &NewJob<'_>) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::INSERT_JOB.text())?
        .execute(named_params! {
            ":id": new.id,
            ":kind": new.kind,
            ":requested_by": new.requested_by,
            ":catalog_revision": new.catalog_revision,
            ":work_count": new.expected_work_count,
            ":key": new.idempotency_key,
            ":now": new.now,
        })
        .map(drop)
}

/// Record a job of `kind` that is already done with `code`, under
/// idempotency key `key`: a marker that something happened once. Nothing
/// when the key is taken.
pub fn insert_done(
    tx: &Transaction<'_>,
    id: &str,
    kind: OperationKind,
    code: &str,
    key: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::INSERT_DONE.text())?
        .execute(named_params! {
            ":id": id,
            ":kind": kind,
            ":code": code,
            ":key": key,
            ":now": now,
        })
        .map(drop)
}

/// A work item to add to a job.
pub struct NewWork<'a> {
    pub job_id: &'a str,
    pub ordinal: i64,
    pub local_album_id: Option<&'a str>,
    pub local_track_id: Option<&'a str>,
    pub expected_subject_revision: i64,
    pub expected_input_revision: &'a str,
    pub action: &'a str,
    pub idempotency_key: &'a str,
    pub now: f64,
}

/// Add one pending work item.
pub fn insert_work(tx: &Transaction<'_>, new: &NewWork<'_>) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::INSERT_WORK.text())?
        .execute(named_params! {
            ":job": new.job_id,
            ":ordinal": new.ordinal,
            ":album": new.local_album_id,
            ":track": new.local_track_id,
            ":subject_revision": new.expected_subject_revision,
            ":input_revision": new.expected_input_revision,
            ":action": new.action,
            ":key": new.idempotency_key,
            ":now": new.now,
        })
        .map(drop)
}

/// Queue a job again with fresh counts for `work_count` items.
pub fn queue_work(
    tx: &Transaction<'_>,
    job_id: &str,
    work_count: i64,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::QUEUE_WORK.text())?
        .execute(named_params! { ":id": job_id, ":work_count": work_count, ":now": now })
        .map(drop)
}

// ---------------------------------------------------------------------------
// The lease.
// ---------------------------------------------------------------------------

fn millis(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

fn lease_until(now: f64) -> i64 {
    millis(now + LEASE_SECS)
}

/// The lease core's error as the database error it carries. The core
/// raises nothing else on these calls.
fn db_error(error: JobError) -> DbError {
    match error {
        JobError::Db(error) => error,
        other => DbError::from(rusqlite::Error::ToSqlConversionFailure(Box::new(other))),
    }
}

fn rowid(tx: &Transaction<'_>, job_id: &str) -> rusqlite::Result<Option<i64>> {
    tx.prepare_cached(sql::ROWID.text())?
        .query_row(params![job_id], |row| row.get(0))
        .optional()
}

/// Requeue the jobs of `kind` whose lease ran out, with their running work.
pub fn recover_expired(
    tx: &Transaction<'_>,
    kind: OperationKind,
    now: f64,
) -> Result<u64, DbError> {
    // The work and the job compare against the same rounded time, so a
    // job requeued here never leaves its work running.
    let now_ms = millis(now);
    tx.prepare_cached(sql::WORK_RECOVER_EXPIRED.text())?
        .execute(named_params! { ":kind": kind, ":now": now_ms as f64 / 1000.0 })?;
    lease::recover_expired(tx, &STATEMENTS, kind.as_str(), now_ms).map_err(db_error)
}

/// Requeue every running job of `kind`, with its running work: at start,
/// each was held by a process that stopped. Only safe because the server
/// lock keeps a second server off the data directory.
pub fn recover_at_start(
    tx: &Transaction<'_>,
    kind: OperationKind,
    now: f64,
) -> Result<u64, DbError> {
    tx.prepare_cached(sql::WORK_RECOVER_AT_START.text())?
        .execute(named_params! { ":kind": kind, ":now": now })?;
    lease::recover_at_start(tx, &STATEMENTS, kind.as_str(), millis(now)).map_err(db_error)
}

/// Requeue the jobs of `kind` whose lease ran out, then claim the next due
/// one for `owner`.
pub fn claim(
    tx: &Transaction<'_>,
    kind: OperationKind,
    owner: &str,
    now: f64,
) -> Result<Option<OperationJob>, DbError> {
    recover_expired(tx, kind, now)?;
    let claimed = lease::claim(
        tx,
        &STATEMENTS,
        kind.as_str(),
        owner,
        millis(now),
        lease_until(now),
    )
    .map_err(db_error)?;
    let Some(rowid) = claimed else {
        return Ok(None);
    };
    Ok(tx
        .prepare_cached(sql::JOB_BY_ROWID.text())?
        .query_row(params![rowid], map_job)
        .optional()?)
}

/// Push `owner`'s lease on a running job forward. False when it no longer
/// holds the job (stopped, or recovered and taken by another claim).
pub fn heartbeat(
    tx: &Transaction<'_>,
    job_id: &str,
    owner: &str,
    now: f64,
) -> Result<bool, DbError> {
    let Some(rowid) = rowid(tx, job_id)? else {
        return Ok(false);
    };
    lease::heartbeat(tx, &STATEMENTS, rowid, owner, millis(now), lease_until(now)).map_err(db_error)
}

/// Hand `owner`'s running job back to the queue, its running work back to
/// pending, without counting anything: a scan started, the database was
/// busy, or the server is stopping. False when it no longer held the job.
pub fn release(tx: &Transaction<'_>, job_id: &str, owner: &str, now: f64) -> Result<bool, DbError> {
    if !holds(tx, job_id, owner)? {
        return Ok(false);
    }
    let Some(rowid) = rowid(tx, job_id)? else {
        return Ok(false);
    };
    work_back_to_pending(tx, job_id, None, now)?;
    lease::release(tx, &STATEMENTS, rowid, owner, millis(now)).map_err(db_error)
}

/// As [`release`], but no claim takes the job again before `retry_at`,
/// and its running work keeps `reason` as why it waits. False when
/// `owner` no longer held the job.
pub fn release_until(
    tx: &Transaction<'_>,
    job_id: &str,
    owner: &str,
    retry_at: f64,
    reason: Option<&str>,
    now: f64,
) -> Result<bool, DbError> {
    if !holds(tx, job_id, owner)? {
        return Ok(false);
    }
    let Some(rowid) = rowid(tx, job_id)? else {
        return Ok(false);
    };
    work_back_to_pending(tx, job_id, reason, now)?;
    if !lease::release(tx, &STATEMENTS, rowid, owner, millis(now)).map_err(db_error)? {
        return Ok(false);
    }
    tx.prepare_cached(sql::RETRY_AT.text())?
        .execute(named_params! { ":id": job_id, ":at": retry_at })?;
    Ok(true)
}

/// True while `owner` holds the running job.
pub fn holds(conn: &Connection, job_id: &str, owner: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(sql::HOLDS.text())?
        .query_row(named_params! { ":id": job_id, ":owner": owner }, |row| {
            row.get(0)
        })
}

// ---------------------------------------------------------------------------
// Halting and finishing.
// ---------------------------------------------------------------------------

/// Honour a pause or stop the worker has not seen yet. `Some(job)` when
/// the job left `running` (or `owner` no longer holds it); its running
/// work goes back to pending.
pub fn checkpoint(
    tx: &Transaction<'_>,
    job_id: &str,
    owner: &str,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    let request: Option<ControlRequest> = tx
        .prepare_cached(sql::HELD_CONTROL.text())?
        .query_row(named_params! { ":id": job_id, ":owner": owner }, |row| {
            row.get(0)
        })
        .optional()?;
    let (state, code) = match request {
        Some(ControlRequest::Pause) => (OperationState::Paused, "PAUSED"),
        Some(ControlRequest::Stop) => (OperationState::Stopped, "STOPPED"),
        Some(ControlRequest::None) => return Ok(None),
        None => return job(tx, job_id),
    };
    tx.prepare_cached(sql::HALT.text())?
        .execute(named_params! { ":id": job_id, ":state": state, ":code": code, ":now": now })?;
    work_back_to_pending(tx, job_id, None, now)?;
    job(tx, job_id)
}

fn work_back_to_pending(
    tx: &Transaction<'_>,
    job_id: &str,
    reason: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::WORK_BACK_TO_PENDING.text())?
        .execute(named_params! { ":job": job_id, ":code": reason, ":now": now })
        .map(drop)
}

/// What the end of a job does to its counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tally {
    /// The counts stand as the work left them.
    AsCounted,
    /// Every expected item counts as failed.
    AllFailed,
    /// Every expected item counts as succeeded.
    AllSucceeded,
}

impl Tally {
    fn code(self) -> i64 {
        match self {
            Self::AsCounted => 0,
            Self::AllFailed => 1,
            Self::AllSucceeded => 2,
        }
    }
}

/// End `owner`'s running job in `state` with `code`. False when it no
/// longer held the job.
pub fn finish(
    tx: &Transaction<'_>,
    job_id: &str,
    owner: &str,
    state: OperationState,
    code: &str,
    tally: Tally,
    now: f64,
) -> rusqlite::Result<bool> {
    let changed = tx
        .prepare_cached(sql::FINISH.text())?
        .execute(named_params! {
            ":id": job_id,
            ":owner": owner,
            ":state": state,
            ":code": code,
            ":tally": tally.code(),
            ":now": now,
        })?;
    Ok(changed == 1)
}

/// `owner`'s running job planned its work and waits for a person. False
/// when it no longer held the job.
pub fn park_ready(
    tx: &Transaction<'_>,
    job_id: &str,
    owner: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let changed = tx
        .prepare_cached(sql::PARK_READY.text())?
        .execute(named_params! { ":id": job_id, ":owner": owner, ":now": now })?;
    Ok(changed == 1)
}

/// `owner`'s running job checked its work and waits for a person, its
/// counts kept and `code` saying why. False when it no longer held the
/// job.
pub fn report_ready(
    tx: &Transaction<'_>,
    job_id: &str,
    owner: &str,
    code: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let changed = tx
        .prepare_cached(sql::REPORT_READY.text())?
        .execute(named_params! { ":id": job_id, ":owner": owner, ":code": code, ":now": now })?;
    Ok(changed == 1)
}

/// Set a ready job aside with `code`, its work never done. False when it
/// was not ready.
pub fn cancel_ready(
    tx: &Transaction<'_>,
    job_id: &str,
    code: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let changed = tx
        .prepare_cached(sql::CANCEL_READY.text())?
        .execute(named_params! { ":id": job_id, ":code": code, ":now": now })?;
    Ok(changed == 1)
}

/// End a ready job that has nothing to write, as done with `code`.
pub fn complete_ready(
    tx: &Transaction<'_>,
    job_id: &str,
    code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::COMPLETE_READY.text())?
        .execute(named_params! { ":id": job_id, ":code": code, ":now": now })
        .map(drop)
}

/// Set a management preview aside, under the revision the caller saw:
/// only one still planning or waiting to be applied. False when it was
/// not that.
pub fn discard_preview(
    tx: &Transaction<'_>,
    job_id: &str,
    expected_revision: i64,
    now: f64,
) -> rusqlite::Result<bool> {
    let changed = tx
        .prepare_cached(sql::DISCARD_PREVIEW.text())?
        .execute(named_params! {
            ":id": job_id,
            ":code": "DISCARDED",
            ":revision": expected_revision,
            ":now": now,
        })?;
    Ok(changed == 1)
}

/// What an administrator's control does to a job's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlChange {
    /// Ask the running worker to pause or stop at its next checkpoint.
    Request(ControlRequest),
    /// Stop now: nothing is running the job.
    StopNow,
    /// Queue it again from the start, every work item pending.
    Restart,
    /// Queue it again for its failed work.
    RetryFailed,
    /// Queue it again where it left off.
    Continue,
}

/// Apply a control under the revision the caller saw. False when the job
/// changed since.
pub fn control(
    tx: &Transaction<'_>,
    job_id: &str,
    change: ControlChange,
    expected_revision: i64,
    now: f64,
) -> rusqlite::Result<bool> {
    let statement = match change {
        ControlChange::Request(request) => {
            let changed =
                tx.prepare_cached(sql::CONTROL_REQUEST.text())?
                    .execute(named_params! {
                        ":id": job_id,
                        ":request": request,
                        ":revision": expected_revision,
                        ":now": now,
                    })?;
            return Ok(changed == 1);
        }
        ControlChange::StopNow => sql::CONTROL_STOP_NOW,
        ControlChange::Restart => {
            tx.prepare_cached(sql::WORK_RESET_ALL.text())?
                .execute(named_params! { ":job": job_id, ":now": now })?;
            sql::CONTROL_RESTART
        }
        ControlChange::RetryFailed => {
            tx.prepare_cached(sql::WORK_RESET_FAILED.text())?
                .execute(named_params! { ":job": job_id, ":now": now })?;
            sql::CONTROL_RETRY_FAILED
        }
        ControlChange::Continue => sql::CONTROL_CONTINUE,
    };
    let changed = tx
        .prepare_cached(statement.text())?
        .execute(named_params! { ":id": job_id, ":revision": expected_revision, ":now": now })?;
    Ok(changed == 1)
}

// ---------------------------------------------------------------------------
// Work items.
// ---------------------------------------------------------------------------

/// Start the job's next pending work item: its ordinal.
pub fn claim_work(tx: &Transaction<'_>, job_id: &str, now: f64) -> rusqlite::Result<Option<i64>> {
    let ordinal: Option<i64> = tx
        .prepare_cached(sql::NEXT_WORK.text())?
        .query_row(named_params! { ":job": job_id }, |row| row.get(0))
        .optional()?;
    if let Some(ordinal) = ordinal {
        tx.prepare_cached(sql::START_WORK.text())?
            .execute(named_params! { ":job": job_id, ":ordinal": ordinal, ":now": now })?;
    }
    Ok(ordinal)
}

/// Record one work item's outcome and count it on its job.
pub fn finish_work(
    tx: &Transaction<'_>,
    job_id: &str,
    ordinal: i64,
    state: WorkState,
    failure_code: Option<&str>,
    result_json: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::FINISH_WORK.text())?
        .execute(named_params! {
            ":job": job_id,
            ":ordinal": ordinal,
            ":state": state,
            ":code": failure_code,
            ":result": result_json,
            ":now": now,
        })?;
    tx.prepare_cached(sql::COUNT_WORK.text())?
        .execute(named_params! { ":job": job_id, ":state": state, ":now": now })
        .map(drop)
}

/// Record the outcome of the job's running work. A `None` result keeps
/// what the item recorded so far.
pub fn finish_running_work(
    tx: &Transaction<'_>,
    job_id: &str,
    state: WorkState,
    failure_code: Option<&str>,
    result_json: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::FINISH_RUNNING_WORK.text())?
        .execute(named_params! {
            ":job": job_id,
            ":state": state,
            ":code": failure_code,
            ":result": result_json,
            ":now": now,
        })
        .map(drop)
}

/// The publish bundle a work item writes under, if it has one yet.
pub fn work_bundle(
    conn: &Connection,
    job_id: &str,
    ordinal: i64,
) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(sql::WORK_BUNDLE.text())?
        .query_row(
            named_params! { ":job": job_id, ":ordinal": ordinal },
            |row| row.get(0),
        )
        .optional()
        .map(Option::flatten)
}

/// Record the publish bundle a work item is about to write under.
pub fn set_work_bundle(
    conn: &Connection,
    job_id: &str,
    ordinal: i64,
    bundle_id: &str,
) -> rusqlite::Result<()> {
    conn.prepare_cached(sql::SET_WORK_BUNDLE.text())?
        .execute(named_params! { ":job": job_id, ":ordinal": ordinal, ":bundle": bundle_id })
        .map(drop)
}

/// The work item (job id, ordinal) that wrote under `bundle_id`.
pub fn work_of_bundle(
    conn: &Connection,
    bundle_id: &str,
) -> rusqlite::Result<Option<(String, i64)>> {
    conn.prepare_cached(sql::WORK_OF_BUNDLE.text())?
        .query_row(named_params! { ":bundle": bundle_id }, |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()
}

/// A condition on `job` (a `library_operation_jobs` row) that holds while
/// the job still has work coming on its albums: queued, running, paused,
/// or ready and not past its expiry. A ready preview past its expiry can
/// never be applied, so it holds nothing back; one with no expiry stored
/// still does. A dry run for turning automatic management on is never
/// applied either, so it holds nothing back while ready. `now` is the SQL
/// parameter holding the current time in seconds.
pub fn live_job_sql(now: &str) -> String {
    format!(
        "(job.state IN ('queued','running','paused') OR (job.state = 'ready' \
         AND NOT EXISTS(SELECT 1 FROM library_management_job_snapshots expiry \
         WHERE expiry.job_id = job.id AND (expiry.preview_expires_at <= {now} \
         OR expiry.proposed_settings_revision IS NOT NULL))))"
    )
}

#[cfg(test)]
mod tests {
    use droppedneedle_kernel::db::scratch::ScratchDir;
    use droppedneedle_kernel::db::{DbConfig, open_runtime};
    use droppedneedle_kernel::schema::template::copy_to;

    /// The job table's lease statements keep the promises the kernel's
    /// lease core relies on: a stale owner changes nothing, a lease that
    /// ran out is requeued for its kind, and every held job is requeued at
    /// start.
    #[tokio::test]
    async fn the_job_table_keeps_the_lease_promises() {
        let dir = ScratchDir::new("library-jobs-lease").unwrap();
        let path = dir.join("library.db");
        copy_to(&path).unwrap();
        let runtime = open_runtime(&DbConfig::new(&path)).await.unwrap();
        let kind = super::OperationKind::ExplicitReidentification.as_str();
        super::lease::check_conformance(runtime.db(), super::STATEMENTS, kind, move |tx| {
            tx.execute(
                "INSERT INTO library_operation_jobs (id, kind, state, created_at, updated_at) \
                 VALUES ('job', ?1, 'queued', 0, 0)",
                [kind],
            )?;
            tx.query_row(
                "SELECT rowid FROM library_operation_jobs WHERE id = 'job'",
                [],
                |row| row.get(0),
            )
        })
        .await
        .unwrap();
        runtime.shutdown().await;
    }
}
