//! Durable operation state over the 0001 tables.
//!
//! A job is a `library_operation_jobs` row with one row per work item in
//! `library_operation_work`, both written through [`crate::jobs`]. A
//! re-identification also keeps a snapshot in
//! `library_reidentification_snapshots`: the album, input, and identity
//! revisions it was started against and whether a Local metadata lookup
//! was confirmed. Its worker hands the album to the identification queue
//! and finishes, so no re-identification waits on a choice.
//!
//! The functions here are plain SQL on a caller's connection or
//! transaction; [`super::service`] owns the transactions. The catalog
//! decisions a person makes on an album live in [`super::decisions`].

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::control::{ControlPlan, Requeue, plan};
use super::models::{
    Control, Evaluation, OperationError, OperationJob, OperationKind, OperationState, WorkResult,
};
use super::reasons;
use crate::identify::queue::album_input_revision;
use crate::identity::DecisionSource;
use crate::identity::reads::{album_identity_row, identity_hash};
use crate::jobs::{self, ControlChange, NewJob, NewWork, Tally, WorkState};
use crate::management::profile::root_assignment;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::LibraryManagement;

/// Work results a job detail shows; the rest are counted, not listed.
const RESULTS_SHOWN: usize = 100;
pub(super) const PROVIDER: &str = "musicbrainz";

/// The job after a write that must have found it.
pub(crate) fn job_after(conn: &Connection, job_id: &str) -> Result<OperationJob, OperationError> {
    jobs::job(conn, job_id)?.ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))
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

/// A re-identification's sealed inputs, and the evaluation a job made
/// before re-identifications were handed to identification.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub local_album_id: String,
    pub expected_album_revision: i64,
    pub expected_input_revision: String,
    pub expected_identity_revision: String,
    pub requested_release_mbid: Option<String>,
    pub selected_candidate_key: Option<String>,
    pub one_off_local_metadata: bool,
    pub evaluation: Option<Evaluation>,
}

pub fn snapshot(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<Snapshot>> {
    conn.query_row(
        "SELECT local_album_id, expected_album_revision, expected_input_revision, \
         expected_identity_revision, requested_release_mbid, selected_candidate_key, \
         result_json, one_off_local_metadata FROM library_reidentification_snapshots \
         WHERE job_id = ?1",
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
                one_off_local_metadata: row.get(7)?,
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
        identity_revision: identity_hash(conn, album_id)?,
        policies,
    }))
}

// ---------------------------------------------------------------------------
// Creating a re-identification.
// ---------------------------------------------------------------------------

/// Whether the album may be identified again on a person's request, and
/// its revisions when it may. Checked when the request is stored and
/// again when the worker hands the album to identification.
pub(super) fn retry_gate(
    conn: &Connection,
    management: &Live<LibraryManagement>,
    album_id: &str,
    one_off_local_metadata: bool,
) -> Result<AlbumRevisions, OperationError> {
    let revisions = album_revisions(conn, album_id)?
        .ok_or(OperationError::NotFound(reasons::ALBUM_NOT_FOUND))?;
    if revisions.policies.iter().any(|policy| policy == "excluded") {
        return Err(OperationError::Conflict(reasons::ALBUM_EXCLUDED));
    }
    if revisions
        .policies
        .iter()
        .any(|policy| policy == "local_metadata")
    {
        // A lookup's result could reach the files through management, in
        // a folder that keeps its own tags.
        if local_metadata_managed(conn, management, album_id)? {
            return Err(OperationError::Conflict(reasons::LOCAL_METADATA_MANAGED));
        }
        if !one_off_local_metadata {
            return Err(OperationError::Conflict(
                reasons::LOCAL_METADATA_NEEDS_CONFIRMATION,
            ));
        }
    }
    // Identification keeps an edition someone chose or imported, so a
    // retry would change nothing.
    if album_identity_row(conn, album_id)?.is_some_and(|row| {
        row.decision_source.as_deref().is_some_and(|raw| {
            DecisionSource::parse(raw).is_some_and(DecisionSource::is_persons_choice)
        })
    }) {
        return Err(OperationError::Conflict(reasons::EDITION_IS_CHOSEN));
    }
    Ok(revisions)
}

/// True when a folder holding the album's Local metadata files has
/// Library Management turned on.
fn local_metadata_managed(
    conn: &Connection,
    management: &Live<LibraryManagement>,
    album_id: &str,
) -> Result<bool, OperationError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT root_id FROM local_tracks WHERE local_album_id = ?1 \
         AND availability = 'indexed' AND applied_policy = 'local_metadata'",
    )?;
    let roots = stmt
        .query_map(params![album_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let settings = management
        .get()
        .map_err(|error| OperationError::Store(error.to_string()))?;
    Ok(roots
        .iter()
        .any(|root| root_assignment(&settings, root).enabled))
}

/// A re-identification ready to store.
#[derive(Debug, Clone)]
pub struct NewReidentification {
    pub job_id: String,
    pub requested_by_user_id: String,
    /// The caller's key. Without one, a re-identification of the album
    /// that has not run yet is handed back instead of a new one.
    pub idempotency_key: Option<String>,
    pub local_album_id: String,
    pub one_off_local_metadata: bool,
}

/// Store a re-identification, or hand back the job an earlier request
/// with the same idempotency key created, or (without a key) one of the
/// album's that is still to run.
pub fn create_reidentification(
    tx: &Transaction<'_>,
    management: &Live<LibraryManagement>,
    new: &NewReidentification,
    now: f64,
) -> Result<OperationJob, OperationError> {
    let key = new
        .idempotency_key
        .as_ref()
        .map(|key| format!("{key}:reidentification"));
    if let Some(key) = &key
        && let Some(existing) = jobs::job_by_key(tx, key)?
    {
        return Ok(existing);
    }
    let revisions = retry_gate(
        tx,
        management,
        &new.local_album_id,
        new.one_off_local_metadata,
    )?;
    if key.is_none()
        && let Some(existing) = live_reidentification(tx, &new.local_album_id)?
    {
        return Ok(existing);
    }
    jobs::insert(
        tx,
        &NewJob {
            id: &new.job_id,
            kind: OperationKind::ExplicitReidentification,
            requested_by: Some(&new.requested_by_user_id),
            idempotency_key: key.as_deref(),
            catalog_revision: None,
            expected_work_count: 1,
            now,
        },
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
            None::<String>,
            now,
        ],
    )?;
    jobs::insert_work(
        tx,
        &NewWork {
            job_id: &new.job_id,
            ordinal: 0,
            local_album_id: Some(&new.local_album_id),
            local_track_id: None,
            expected_subject_revision: revisions.album_revision,
            expected_input_revision: &revisions.input_revision,
            action: "reidentify",
            idempotency_key: &format!("{}:{}", new.job_id, new.local_album_id),
            now,
        },
    )?;
    job_after(tx, &new.job_id)
}

/// The album's re-identification that is queued or running, if any.
fn live_reidentification(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<OperationJob>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM library_operation_jobs \
             WHERE kind = ?1 AND state IN (?3, ?4) AND id IN \
             (SELECT job_id FROM library_reidentification_snapshots WHERE local_album_id = ?2) \
             ORDER BY created_at LIMIT 1",
            params![
                OperationKind::ExplicitReidentification,
                album_id,
                OperationState::Queued,
                OperationState::Running,
            ],
            |row| row.get(0),
        )
        .optional()?;
    match id {
        Some(id) => jobs::job(conn, &id),
        None => Ok(None),
    }
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
    let current =
        jobs::job(tx, job_id)?.ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))?;
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
    let change = match plan(&current, control) {
        ControlPlan::Unchanged => return Ok(current),
        ControlPlan::Request(request) => ControlChange::Request(request),
        ControlPlan::StopNow => ControlChange::StopNow,
        ControlPlan::Requeue(Requeue::Restart) => {
            tx.execute(
                "UPDATE library_reidentification_snapshots SET result_json = NULL, \
                 selected_candidate_key = NULL WHERE job_id = ?1",
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

// ---------------------------------------------------------------------------
// The worker side.
// ---------------------------------------------------------------------------

/// End a running job with no evaluation (the album went away or moved).
pub fn fail(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    code: &str,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    if jobs::holds(tx, job_id, worker)? {
        jobs::finish_running_work(tx, job_id, WorkState::Failed, Some(code), None, now)?;
        jobs::finish(
            tx,
            job_id,
            worker,
            OperationState::Failed,
            code,
            Tally::AllFailed,
            now,
        )?;
    }
    jobs::job(tx, job_id)
}

/// End a running re-identification whose album went to the
/// identification queue in the same transaction.
pub fn hand_over(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    if jobs::holds(tx, job_id, worker)? {
        jobs::finish_running_work(
            tx,
            job_id,
            WorkState::Succeeded,
            None,
            Some(r#"{"outcome":"handed_to_identification"}"#),
            now,
        )?;
        jobs::finish(
            tx,
            job_id,
            worker,
            OperationState::Succeeded,
            "HANDED_TO_IDENTIFICATION",
            Tally::AllSucceeded,
            now,
        )?;
    }
    jobs::job(tx, job_id)
}
