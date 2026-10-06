//! Durable operation state over the 0001 tables.
//!
//! Jobs live in `library_operation_jobs` with one row per work item in
//! `library_operation_work`. A re-identification also keeps a snapshot in
//! `library_reidentification_snapshots`: the album, input, and identity
//! revisions it was started against, the release an administrator named,
//! and (in `result_json`) the evaluation with its candidates. Every write
//! takes the job's `row_revision` forward, and every decision an
//! administrator makes checks the revisions it was shown first.
//!
//! The functions here are plain SQL on a caller's connection or
//! transaction; [`super::service`] owns the transactions. The decisions an
//! administrator makes on a finished evaluation live in
//! [`super::decisions`].

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::control::{ControlPlan, Requeue, plan};
use super::models::{
    Control, ControlRequest, Evaluation, OperationError, OperationJob, OperationKind,
    OperationState, WorkResult,
};
use super::reasons;
use crate::library::identify::sqlite::album_input_revision;

/// How long a worker holds a claimed job before recovery requeues it.
pub const LEASE_SECS: f64 = 60.0;
/// Work results a job detail shows; the rest are counted, not listed.
const RESULTS_SHOWN: usize = 100;
pub(super) const PROVIDER: &str = "musicbrainz";

const JOB_COLUMNS: &str = "id, kind, state, requested_by_user_id, expected_work_count, \
     completed_count, succeeded_count, failed_count, skipped_count, control_request, \
     terminal_code, reidentification_attempt_count, row_revision, event_revision, created_at, \
     updated_at";

fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationJob> {
    Ok(OperationJob {
        id: row.get(0)?,
        kind: row.get(1)?,
        state: OperationState::parse(&row.get::<_, String>(2)?).unwrap_or(OperationState::Failed),
        requested_by_user_id: row.get(3)?,
        expected_work_count: row.get(4)?,
        completed_count: row.get(5)?,
        succeeded_count: row.get(6)?,
        failed_count: row.get(7)?,
        skipped_count: row.get(8)?,
        control_request: ControlRequest::parse(&row.get::<_, String>(9)?),
        terminal_code: row.get(10)?,
        reidentification_attempt_count: row.get(11)?,
        row_revision: row.get(12)?,
        event_revision: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

pub fn job(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<OperationJob>> {
    conn.query_row(
        &format!("SELECT {JOB_COLUMNS} FROM library_operation_jobs WHERE id = ?1"),
        params![job_id],
        map_job,
    )
    .optional()
}

fn job_by_key(conn: &Connection, key: &str) -> rusqlite::Result<Option<OperationJob>> {
    conn.query_row(
        &format!("SELECT {JOB_COLUMNS} FROM library_operation_jobs WHERE idempotency_key = ?1"),
        params![key],
        map_job,
    )
    .optional()
}

/// The job after a write that must have found it.
pub(super) fn job_after(conn: &Connection, job_id: &str) -> Result<OperationJob, OperationError> {
    job(conn, job_id)?.ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))
}

/// The first work results of a job, and whether more exist.
pub fn work_results(conn: &Connection, job_id: &str) -> rusqlite::Result<(Vec<WorkResult>, bool)> {
    let mut stmt = conn.prepare(
        "SELECT ordinal, local_album_id, local_track_id, action, state, failure_code, \
         result_json FROM library_operation_work WHERE job_id = ?1 ORDER BY ordinal LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![job_id, (RESULTS_SHOWN + 1) as i64], |row| {
        let raw: Option<String> = row.get(6)?;
        Ok(WorkResult {
            ordinal: row.get(0)?,
            local_album_id: row.get(1)?,
            local_track_id: row.get(2)?,
            action: row.get(3)?,
            state: row.get(4)?,
            failure_code: row.get(5)?,
            result: raw
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_else(|| serde_json::json!({})),
        })
    })?;
    let mut results = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = results.len() > RESULTS_SHOWN;
    results.truncate(RESULTS_SHOWN);
    Ok((results, truncated))
}

/// A re-identification's sealed inputs and its evaluation, once made.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub local_album_id: String,
    pub expected_album_revision: i64,
    pub expected_input_revision: String,
    pub expected_identity_revision: String,
    pub requested_release_mbid: Option<String>,
    pub selected_candidate_key: Option<String>,
    pub evaluation: Option<Evaluation>,
}

pub fn snapshot(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<Snapshot>> {
    conn.query_row(
        "SELECT local_album_id, expected_album_revision, expected_input_revision, \
         expected_identity_revision, requested_release_mbid, selected_candidate_key, \
         result_json FROM library_reidentification_snapshots WHERE job_id = ?1",
        params![job_id],
        |row| {
            let raw: Option<String> = row.get(6)?;
            Ok(Snapshot {
                local_album_id: row.get(0)?,
                expected_album_revision: row.get(1)?,
                expected_input_revision: row.get(2)?,
                expected_identity_revision: row.get(3)?,
                requested_release_mbid: row.get(4)?,
                selected_candidate_key: row.get(5)?,
                evaluation: raw.and_then(|raw| match serde_json::from_str(&raw) {
                    Ok(evaluation) => Some(evaluation),
                    Err(error) => {
                        tracing::warn!(%error, "unreadable re-identification evaluation");
                        None
                    }
                }),
            })
        },
    )
    .optional()
}

// ---------------------------------------------------------------------------
// Album revisions.
// ---------------------------------------------------------------------------

/// What an album looks like right now, for the compare-and-swap checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumRevisions {
    pub album_revision: i64,
    pub input_revision: String,
    pub identity_revision: String,
    /// Policies the album's indexed files were scanned under.
    pub policies: Vec<String>,
}

/// Revisions of a live album with indexed files; `None` otherwise.
pub fn album_revisions(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<AlbumRevisions>> {
    let album_revision: Option<i64> = conn
        .query_row(
            "SELECT row_revision FROM local_albums WHERE id = ?1 \
             AND retired_into_album_id IS NULL",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(album_revision) = album_revision else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT DISTINCT applied_policy FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY applied_policy",
    )?;
    let policies = stmt
        .query_map(params![album_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if policies.is_empty() {
        return Ok(None);
    }
    Ok(Some(AlbumRevisions {
        album_revision,
        input_revision: album_input_revision(conn, album_id)?,
        identity_revision: identity_revision(conn, album_id)?,
        policies,
    }))
}

/// A hash over the album's identity row and its indexed tracks' identity
/// rows: any seal, retraction, or curator decision moves it.
fn identity_revision(conn: &Connection, album_id: &str) -> rusqlite::Result<String> {
    let mut hasher = Sha256::new();
    let album: Option<(String, Option<String>, String, i64)> = conn
        .query_row(
            "SELECT release_group_mbid, release_mbid, decision_source, row_revision \
             FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
            params![album_id, PROVIDER],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((group, release, source, revision)) = album {
        hasher.update(
            format!(
                "album\0{group}\0{}\0{source}\0{revision}\n",
                release.unwrap_or_default()
            )
            .as_bytes(),
        );
    }
    let mut stmt = conn.prepare(
        "SELECT t.id, i.recording_mbid, i.release_mbid, i.release_track_mbid, \
         i.medium_position, i.release_track_position, i.decision_source, i.row_revision \
         FROM local_tracks t JOIN local_track_external_identities i \
         ON i.local_track_id = t.id AND i.provider = ?2 \
         WHERE t.local_album_id = ?1 AND t.availability = 'indexed' ORDER BY t.id",
    )?;
    let mut rows = stmt.query(params![album_id, PROVIDER])?;
    while let Some(row) = rows.next()? {
        let line = format!(
            "track\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\n",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            row.get::<_, Option<i64>>(4)?.unwrap_or_default(),
            row.get::<_, Option<i64>>(5)?.unwrap_or_default(),
            row.get::<_, String>(6)?,
            row.get::<_, i64>(7)?,
        );
        hasher.update(line.as_bytes());
    }
    Ok(hasher
        .finalize()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

// ---------------------------------------------------------------------------
// Creating a re-identification.
// ---------------------------------------------------------------------------

/// A re-identification ready to store.
#[derive(Debug, Clone)]
pub struct NewReidentification {
    pub job_id: String,
    pub requested_by_user_id: String,
    /// The caller's key; without one, the album's input revision keys the
    /// request, so asking twice for unchanged files returns one job.
    pub idempotency_key: Option<String>,
    pub local_album_id: String,
    pub one_off_local_metadata: bool,
    pub release_mbid: Option<String>,
    pub expected_album_revision: Option<i64>,
    pub expected_input_revision: Option<String>,
}

/// Store a re-identification, or hand back the job an earlier request
/// with the same idempotency key created.
pub fn create_reidentification(
    tx: &Transaction<'_>,
    new: &NewReidentification,
    now: f64,
) -> Result<OperationJob, OperationError> {
    let release_part = new.release_mbid.as_deref().unwrap_or("automatic");
    let caller_key = new
        .idempotency_key
        .as_ref()
        .map(|key| format!("{key}:release:{release_part}"));
    if let Some(key) = &caller_key
        && let Some(existing) = job_by_key(tx, key)?
    {
        return Ok(existing);
    }
    let revisions = album_revisions(tx, &new.local_album_id)?
        .ok_or(OperationError::NotFound(reasons::ALBUM_NOT_FOUND))?;
    let key = match caller_key {
        Some(key) => key,
        None => {
            let key = format!(
                "explicit_reidentification:{}:{}:release:{release_part}",
                new.local_album_id, revisions.input_revision
            );
            if let Some(existing) = job_by_key(tx, &key)? {
                return Ok(existing);
            }
            key
        }
    };
    if new
        .expected_album_revision
        .is_some_and(|expected| expected != revisions.album_revision)
    {
        return Err(OperationError::Conflict(reasons::ALBUM_CHANGED));
    }
    if new
        .expected_input_revision
        .as_deref()
        .is_some_and(|expected| expected != revisions.input_revision)
    {
        return Err(OperationError::Conflict(reasons::ALBUM_FILES_CHANGED));
    }
    if revisions.policies.iter().any(|policy| policy == "excluded") {
        return Err(OperationError::Conflict(reasons::ALBUM_EXCLUDED));
    }
    if revisions
        .policies
        .iter()
        .any(|policy| policy == "local_metadata")
        && !new.one_off_local_metadata
    {
        return Err(OperationError::Conflict(
            reasons::LOCAL_METADATA_NEEDS_CONFIRMATION,
        ));
    }
    tx.execute(
        "INSERT INTO library_operation_jobs (id, kind, state, requested_by_user_id, \
         expected_work_count, idempotency_key, created_at, updated_at) \
         VALUES (?1, ?2, 'queued', ?3, 1, ?4, ?5, ?5)",
        params![
            new.job_id,
            OperationKind::ExplicitReidentification.as_str(),
            new.requested_by_user_id,
            key,
            now,
        ],
    )?;
    tx.execute(
        "INSERT INTO library_reidentification_snapshots (job_id, local_album_id, \
         expected_album_revision, expected_input_revision, expected_identity_revision, \
         one_off_local_metadata, requested_release_mbid, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            new.job_id,
            new.local_album_id,
            revisions.album_revision,
            revisions.input_revision,
            revisions.identity_revision,
            new.one_off_local_metadata,
            new.release_mbid,
            now,
        ],
    )?;
    tx.execute(
        "INSERT INTO library_operation_work (job_id, ordinal, local_album_id, \
         expected_subject_revision, expected_input_revision, action, idempotency_key, \
         updated_at) VALUES (?1, 0, ?2, ?3, ?4, 'reidentify', ?5, ?6)",
        params![
            new.job_id,
            new.local_album_id,
            revisions.album_revision,
            revisions.input_revision,
            format!("{}:{}", new.job_id, new.local_album_id),
            now,
        ],
    )?;
    job_after(tx, &new.job_id)
}

// ---------------------------------------------------------------------------
// Controls.
// ---------------------------------------------------------------------------

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
    let current = job(tx, job_id)?.ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))?;
    if let Some(key) = idempotency_key {
        if key.trim().is_empty() {
            return Err(OperationError::Invalid(reasons::CONTROL_KEY_EMPTY));
        }
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT job_id, control FROM library_operation_control_idempotency \
                 WHERE idempotency_key = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((prior_job, prior_control)) = prior {
            if prior_job != job_id || prior_control != control.as_str() {
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
            "INSERT INTO library_operation_control_idempotency \
             (idempotency_key, job_id, control, requested_at) VALUES (?1, ?2, ?3, ?4)",
            params![key, job_id, control.as_str(), now],
        )?;
    }
    let assignments = match plan(&current, control) {
        ControlPlan::Unchanged => return Ok(current),
        ControlPlan::Request(request) => format!("control_request = '{}'", request.as_str()),
        ControlPlan::StopNow => format!(
            "state = 'stopped', control_request = 'none', terminal_code = 'STOPPED', \
             terminal_at = {now}, lease_owner = NULL, lease_expires_at = NULL, \
             heartbeat_at = NULL"
        ),
        ControlPlan::Requeue(Requeue::Restart) => {
            tx.execute(
                "UPDATE library_operation_work SET state = 'pending', failure_code = NULL, \
                 result_json = NULL, updated_at = ?2, row_revision = row_revision + 1 \
                 WHERE job_id = ?1",
                params![job_id, now],
            )?;
            tx.execute(
                "UPDATE library_reidentification_snapshots SET result_json = NULL, \
                 selected_candidate_key = NULL WHERE job_id = ?1",
                params![job_id],
            )?;
            "state = 'queued', control_request = 'none', terminal_code = NULL, \
             terminal_at = NULL, next_attempt_at = NULL, completed_count = 0, \
             succeeded_count = 0, failed_count = 0, skipped_count = 0"
                .to_owned()
        }
        ControlPlan::Requeue(Requeue::RetryFailed) => {
            tx.execute(
                "UPDATE library_operation_work SET state = 'pending', failure_code = NULL, \
                 result_json = NULL, updated_at = ?2, row_revision = row_revision + 1 \
                 WHERE job_id = ?1 AND state IN ('failed','running')",
                params![job_id, now],
            )?;
            "state = 'queued', control_request = 'none', terminal_code = NULL, \
             terminal_at = NULL, next_attempt_at = NULL, reidentification_attempt_count = 0, \
             completed_count = 0, failed_count = 0"
                .to_owned()
        }
        ControlPlan::Requeue(Requeue::Continue) => "state = 'queued', control_request = 'none', \
             terminal_code = NULL, terminal_at = NULL"
            .to_owned(),
    };
    let changed = tx.execute(
        &format!(
            "UPDATE library_operation_jobs SET {assignments}, updated_at = ?1, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = ?2 AND row_revision = ?3"
        ),
        params![now, job_id, expected_row_revision],
    )?;
    if changed != 1 {
        return Err(OperationError::Conflict(reasons::OPERATION_CHANGED));
    }
    job_after(tx, job_id)
}

// ---------------------------------------------------------------------------
// The worker side.
// ---------------------------------------------------------------------------

/// Put jobs whose worker vanished (its lease ran out) back in the queue.
pub fn recover_expired(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE library_operation_work SET state = 'pending', updated_at = ?1, \
         row_revision = row_revision + 1 WHERE state = 'running' AND job_id IN \
         (SELECT id FROM library_operation_jobs WHERE state = 'running' \
         AND lease_expires_at < ?1)",
        params![now],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'queued', lease_owner = NULL, \
         lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?1, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE state = 'running' AND lease_expires_at < ?1",
        params![now],
    )
}

/// Claim the next due re-identification for `worker`.
pub fn claim_reidentification(
    tx: &Transaction<'_>,
    worker: &str,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    let candidate: Option<String> = tx
        .query_row(
            "SELECT id FROM library_operation_jobs WHERE state = 'queued' AND kind = ?1 \
             AND (next_attempt_at IS NULL OR next_attempt_at <= ?2) \
             ORDER BY updated_at, created_at, id LIMIT 1",
            params![OperationKind::ExplicitReidentification.as_str(), now],
            |row| row.get(0),
        )
        .optional()?;
    let Some(job_id) = candidate else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'running', \
         started_at = COALESCE(started_at, ?1), lease_owner = ?2, lease_expires_at = ?3, \
         heartbeat_at = ?1, updated_at = ?1, next_attempt_at = NULL, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?4 AND state = 'queued'",
        params![now, worker, now + LEASE_SECS, job_id],
    )?;
    job(tx, &job_id)
}

/// Claim the job's next pending work item: `(ordinal, row_revision)`.
pub fn claim_work(tx: &Transaction<'_>, job_id: &str, now: f64) -> rusqlite::Result<Option<i64>> {
    let ordinal: Option<i64> = tx
        .query_row(
            "SELECT ordinal FROM library_operation_work WHERE job_id = ?1 \
             AND state = 'pending' ORDER BY ordinal LIMIT 1",
            params![job_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(ordinal) = ordinal {
        tx.execute(
            "UPDATE library_operation_work SET state = 'running', updated_at = ?3, \
             row_revision = row_revision + 1 WHERE job_id = ?1 AND ordinal = ?2",
            params![job_id, ordinal, now],
        )?;
    }
    Ok(ordinal)
}

/// Honor a pause or stop the worker has not seen yet. `Some(job)` when the
/// job left `running`; the running work item goes back to pending.
pub fn checkpoint(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    let request: Option<String> = tx
        .query_row(
            "SELECT control_request FROM library_operation_jobs WHERE id = ?1 \
             AND state = 'running' AND lease_owner = ?2",
            params![job_id, worker],
            |row| row.get(0),
        )
        .optional()?;
    let (state, code) = match request.as_deref().map(ControlRequest::parse) {
        Some(ControlRequest::Pause) => ("paused", "PAUSED"),
        Some(ControlRequest::Stop) => ("stopped", "STOPPED"),
        // Still running and nobody asked, or no longer ours.
        Some(ControlRequest::None) => return Ok(None),
        None => return job(tx, job_id),
    };
    tx.execute(
        "UPDATE library_operation_jobs SET state = ?2, control_request = 'none', \
         terminal_code = COALESCE(terminal_code, ?3), lease_owner = NULL, \
         lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?4, \
         terminal_at = CASE WHEN ?2 = 'stopped' THEN ?4 ELSE terminal_at END, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?1",
        params![job_id, state, code, now],
    )?;
    tx.execute(
        "UPDATE library_operation_work SET state = 'pending', updated_at = ?2, \
         row_revision = row_revision + 1 WHERE job_id = ?1 AND state = 'running'",
        params![job_id, now],
    )?;
    job(tx, job_id)
}

/// True while `worker` holds the running job's lease.
fn holds(tx: &Transaction<'_>, job_id: &str, worker: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_operation_jobs WHERE id = ?1 \
         AND state = 'running' AND lease_owner = ?2)",
        params![job_id, worker],
        |row| row.get(0),
    )
}

/// End a running job with no evaluation (the album went away or moved).
pub fn fail(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    code: &str,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    if !holds(tx, job_id, worker)? {
        return job(tx, job_id);
    }
    tx.execute(
        "UPDATE library_operation_work SET state = 'failed', failure_code = ?2, \
         updated_at = ?3, row_revision = row_revision + 1 \
         WHERE job_id = ?1 AND state = 'running'",
        params![job_id, code, now],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'failed', terminal_code = ?3, \
         terminal_at = ?4, updated_at = ?4, completed_count = expected_work_count, \
         failed_count = expected_work_count, lease_owner = NULL, lease_expires_at = NULL, \
         heartbeat_at = NULL, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 \
         WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
        params![job_id, worker, code, now],
    )?;
    job(tx, job_id)
}

/// A scan started while the job ran: hand the work back without counting
/// an attempt, so the job evaluates again once the scan is done.
pub fn requeue_for_scan(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    if !holds(tx, job_id, worker)? {
        return job(tx, job_id);
    }
    tx.execute(
        "UPDATE library_operation_work SET state = 'pending', updated_at = ?2, \
         row_revision = row_revision + 1 WHERE job_id = ?1 AND state = 'running'",
        params![job_id, now],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'queued', lease_owner = NULL, \
         lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?3, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
        params![job_id, worker, now],
    )?;
    job(tx, job_id)
}

/// MusicBrainz was down: hand the work back and try again at `retry_at`.
pub fn defer(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    reason: &str,
    retry_at: f64,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    if !holds(tx, job_id, worker)? {
        return job(tx, job_id);
    }
    tx.execute(
        "UPDATE library_operation_work SET state = 'pending', failure_code = ?2, \
         updated_at = ?3, row_revision = row_revision + 1 \
         WHERE job_id = ?1 AND state = 'running'",
        params![job_id, reason, now],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'queued', lease_owner = NULL, \
         lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?3, \
         next_attempt_at = ?4, \
         reidentification_attempt_count = reidentification_attempt_count + 1, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
        params![job_id, worker, now, retry_at],
    )?;
    job(tx, job_id)
}

/// The work result a re-identification records.
#[derive(Serialize)]
struct WorkOutcome<'a> {
    outcome: &'a str,
    reason_code: &'a str,
    candidate_keys: Vec<&'a str>,
}

/// Record an evaluation. With candidates the job waits on a choice
/// (`ready`); without, it is done. The album must still be what the job
/// was started against, or the evaluation is thrown away as stale.
pub fn finish_evaluation(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    evaluation: &Evaluation,
    now: f64,
) -> Result<Option<OperationJob>, OperationError> {
    if !holds(tx, job_id, worker)? {
        // Stopped, or the lease ran out and another worker took over.
        return Ok(job(tx, job_id)?);
    }
    // A pause or stop that arrived during recall wins over the result.
    if let Some(halted) = checkpoint(tx, job_id, worker, now)? {
        return Ok(Some(halted));
    }
    let Some(snap) = snapshot(tx, job_id)? else {
        return Ok(fail(tx, job_id, worker, "MISSING_SNAPSHOT", now)?);
    };
    if !matches_snapshot(tx, &snap)? {
        return Ok(fail(tx, job_id, worker, "STALE_INPUT", now)?);
    }
    let unavailable = evaluation.candidates.is_empty()
        && evaluation.reason_code == super::control::RESUMABLE_FAILURE;
    let work = serde_json::to_string(&WorkOutcome {
        outcome: &evaluation.outcome,
        reason_code: &evaluation.reason_code,
        candidate_keys: evaluation
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_key.as_str())
            .collect(),
    })
    .map_err(|error| OperationError::Store(error.to_string()))?;
    let stored = serde_json::to_string(evaluation)
        .map_err(|error| OperationError::Store(error.to_string()))?;
    tx.execute(
        "UPDATE library_operation_work SET state = ?2, result_json = ?3, failure_code = ?4, \
         updated_at = ?5, row_revision = row_revision + 1 \
         WHERE job_id = ?1 AND state = 'running'",
        params![
            job_id,
            if unavailable { "failed" } else { "succeeded" },
            work,
            unavailable.then_some(evaluation.reason_code.as_str()),
            now,
        ],
    )?;
    tx.execute(
        "UPDATE library_reidentification_snapshots SET result_json = ?2 WHERE job_id = ?1",
        params![job_id, stored],
    )?;
    let (state, code) = if unavailable {
        ("failed", evaluation.reason_code.as_str())
    } else if evaluation.candidates.is_empty() {
        ("succeeded", evaluation.reason_code.as_str())
    } else {
        ("ready", "CANDIDATES_READY")
    };
    tx.execute(
        "UPDATE library_operation_jobs SET state = ?3, terminal_code = ?4, \
         completed_count = 1, succeeded_count = ?5, failed_count = ?6, lease_owner = NULL, \
         lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?7, \
         terminal_at = CASE WHEN ?3 IN ('succeeded','failed') THEN ?7 ELSE NULL END, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
        params![
            job_id,
            worker,
            state,
            code,
            i64::from(!unavailable),
            i64::from(unavailable),
            now,
        ],
    )?;
    Ok(job(tx, job_id)?)
}

/// True while the album still has the revisions the snapshot sealed.
pub(super) fn matches_snapshot(conn: &Connection, snap: &Snapshot) -> rusqlite::Result<bool> {
    Ok(
        album_revisions(conn, &snap.local_album_id)?.is_some_and(|now| {
            now.album_revision == snap.expected_album_revision
                && now.input_revision == snap.expected_input_revision
                && now.identity_revision == snap.expected_identity_revision
        }),
    )
}
