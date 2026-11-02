//! Durable re-identification state.
//!
//! A re-identification keeps a snapshot in
//! `library_reidentification_snapshots`: the album, input, and identity
//! revisions it was started against and whether a Local metadata lookup
//! was confirmed. Its worker hands the album to the identification queue
//! and finishes, so no re-identification waits on a choice.
//!
//! The functions here are plain SQL on a caller's connection or
//! transaction; the service owns the transactions.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::identify::queue::album_input_revision;
use crate::identity::DecisionSource;
use crate::identity::reads::{album_identity_row, identity_hash};
use crate::jobs::OperationJob;
use crate::jobs::OperationKind;
use crate::jobs::OperationState;
use crate::jobs::models::OperationError;
use crate::jobs::store::job_after;
use crate::jobs::{self, NewJob, NewWork, Tally, WorkState};
use crate::management::profile::root_assignment;
use crate::reasons;
use crate::reidentify::models::Evaluation;
use crate::reidentify::sql::SnapshotRow;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::LibraryManagement;

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
        crate::reidentify::sql::SNAPSHOT.text(),
        params![job_id],
        SnapshotRow::read,
    )
    .optional()
    .map(|row| {
        row.map(|row| Snapshot {
            local_album_id: row.local_album_id,
            expected_album_revision: row.expected_album_revision,
            expected_input_revision: row.expected_input_revision,
            expected_identity_revision: row.expected_identity_revision,
            requested_release_mbid: row.requested_release_mbid,
            selected_candidate_key: row.selected_candidate_key,
            one_off_local_metadata: row.one_off_local_metadata,
            evaluation: row
                .result_json
                .and_then(|raw| match serde_json::from_str(&raw) {
                    Ok(evaluation) => Some(evaluation),
                    Err(error) => {
                        tracing::warn!(%error, "unreadable re-identification evaluation");
                        None
                    }
                }),
        })
    })
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
            crate::reidentify::sql::LIVE_ALBUM_REVISION.text(),
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(album_revision) = album_revision else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(crate::reidentify::sql::ALBUM_APPLIED_POLICIES.text())?;
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
pub(crate) fn retry_gate(
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
        row.decision_source
            .is_some_and(DecisionSource::is_persons_choice)
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
    let mut stmt = conn.prepare(crate::reidentify::sql::LOCAL_METADATA_MANAGED.text())?;
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

/// The job key a re-identification request's idempotency key stands for.
fn reidentification_key(new: &NewReidentification) -> Option<String> {
    new.idempotency_key
        .as_ref()
        .map(|key| format!("{key}:reidentification"))
}

/// The job an earlier request with the same idempotency key started, if
/// any. A key that started a re-identification of another album is
/// refused.
pub fn replayed_reidentification(
    conn: &Connection,
    new: &NewReidentification,
) -> Result<Option<OperationJob>, OperationError> {
    let Some(key) = reidentification_key(new) else {
        return Ok(None);
    };
    let Some(existing) = jobs::job_by_key(conn, &key)? else {
        return Ok(None);
    };
    let same_album =
        snapshot(conn, &existing.id)?.is_some_and(|snap| snap.local_album_id == new.local_album_id);
    if !same_album {
        return Err(OperationError::Conflict(reasons::CONTROL_KEY_REUSED));
    }
    Ok(Some(existing))
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
    if let Some(existing) = replayed_reidentification(tx, new)? {
        return Ok(existing);
    }
    let key = reidentification_key(new);
    let revisions = retry_gate(
        tx,
        management,
        &new.local_album_id,
        new.one_off_local_metadata,
    )?;
    if key.is_none()
        && let Some(existing) =
            live_reidentification(tx, &new.local_album_id, new.one_off_local_metadata)?
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
        crate::reidentify::sql::CREATE_REIDENTIFICATION.text(),
        params![
            new.job_id,
            new.local_album_id,
            revisions.album_revision,
            revisions.input_revision,
            revisions.identity_revision,
            new.one_off_local_metadata,
            None::<String>,
            crate::clock::ms(now),
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

/// The album's re-identification that is queued or running, if any. One
/// started without the Local metadata confirmation is not handed to a
/// request that carries it, since it would refuse the lookup that request
/// confirmed.
fn live_reidentification(
    conn: &Connection,
    album_id: &str,
    one_off_local_metadata: bool,
) -> rusqlite::Result<Option<OperationJob>> {
    let id: Option<String> = conn
        .query_row(
            crate::reidentify::sql::LIVE_REIDENTIFICATION.text(),
            params![
                OperationKind::ExplicitReidentification,
                album_id,
                OperationState::Queued,
                OperationState::Running,
                one_off_local_metadata,
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
