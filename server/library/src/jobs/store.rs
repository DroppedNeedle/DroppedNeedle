//! Reading a job's results and applying an administrator's controls. Plain
//! SQL on a caller's transaction; the service owns the transactions.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::jobs::OperationJob;
use crate::jobs::control::ControlPlan;
use crate::jobs::control::Requeue;
use crate::jobs::control::plan;
use crate::jobs::models::Control;
use crate::jobs::models::OperationError;
use crate::jobs::models::WorkResult;
use crate::jobs::sql::{ControlKey, WorkResultRow};
use crate::jobs::{self, ControlChange};
use crate::reasons;

/// Work results a job detail shows; the rest are counted, not listed.
const RESULTS_SHOWN: usize = 100;

/// The job after a write that must have found it.
pub(crate) fn job_after(conn: &Connection, job_id: &str) -> Result<OperationJob, OperationError> {
    jobs::job(conn, job_id)?.ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))
}

/// The first work results of a job, and whether more exist.
pub fn work_results(conn: &Connection, job_id: &str) -> rusqlite::Result<(Vec<WorkResult>, bool)> {
    let mut stmt = conn.prepare(crate::jobs::sql::WORK_RESULTS.text())?;
    let rows = stmt.query_map(
        params![job_id, (RESULTS_SHOWN + 1) as i64],
        WorkResultRow::read,
    )?;
    let mut results = rows
        .map(|row| {
            row.map(|row| WorkResult {
                ordinal: row.ordinal,
                local_album_id: row.local_album_id,
                local_track_id: row.local_track_id,
                action: row.action,
                state: row.state,
                failure_code: row.failure_code,
                result: row
                    .result_json
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_else(|| serde_json::json!({})),
                bundle_id: row.bundle_id,
            })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = results.len() > RESULTS_SHOWN;
    results.truncate(RESULTS_SHOWN);
    Ok((results, truncated))
}

/// Apply one control under the caller's revision. A repeated idempotency
/// key answers with the job as it is now.
pub fn request_control(
    tx: &Transaction<'_>,
    job_id: &str,
    control: Control,
    expected_row_revision: i64,
    idempotency_key: Option<&str>,
    now: f64,
) -> Result<OperationJob, OperationError> {
    let current =
        jobs::job(tx, job_id)?.ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))?;
    if let Some(key) = idempotency_key {
        if key.trim().is_empty() {
            return Err(OperationError::Invalid(reasons::CONTROL_KEY_EMPTY));
        }
        let prior = tx
            .query_row(
                crate::jobs::sql::CONTROL_OF_IDEMPOTENCY_KEY.text(),
                params![key],
                ControlKey::read,
            )
            .optional()?;
        if let Some(prior) = prior {
            if prior.job_id != job_id || prior.control != control.as_str() {
                return Err(OperationError::Conflict(reasons::CONTROL_KEY_REUSED));
            }
            return Ok(current);
        }
    }
    if current.row_revision != expected_row_revision {
        return Err(OperationError::Conflict(reasons::OPERATION_CHANGED));
    }
    if let Some(key) = idempotency_key {
        tx.execute(
            crate::jobs::sql::RECORD_CONTROL_REQUEST.text(),
            params![key, job_id, control.as_str(), crate::clock::ms(now)],
        )?;
    }
    let change = match plan(&current, control) {
        ControlPlan::Unchanged => return Ok(current),
        ControlPlan::Request(request) => ControlChange::Request(request),
        ControlPlan::StopNow => ControlChange::StopNow,
        ControlPlan::Requeue(Requeue::Restart) => {
            tx.execute(
                crate::jobs::sql::CLEAR_REIDENTIFICATION_RESULT.text(),
                params![job_id],
            )?;
            ControlChange::Restart
        }
        ControlPlan::Requeue(Requeue::RetryFailed) => ControlChange::RetryFailed,
        ControlPlan::Requeue(Requeue::Continue) => ControlChange::Continue,
    };
    if !jobs::control(tx, job_id, change, expected_row_revision, now)? {
        return Err(OperationError::Conflict(reasons::OPERATION_CHANGED));
    }
    job_after(tx, job_id)
}
