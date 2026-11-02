//! SQL for the activity read model and the identification pause switch.
//!
//! Everything here reads or writes the application database through a
//! borrowed connection: a pooled read, or the pause switch's foreground
//! write. The pause switch is the `library_work_control`
//! row v2 used; the queue counts read the identification jobs and
//! reviews tables; the change revisions read the library event streams.

use std::collections::BTreeMap;

use droppedneedle_kernel::db::DbError;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::sql::{
    ControlRow, FailedJob, FailureCount, JobTotals, OperationRow, StateCount, WaitingJob,
};
use crate::clock::at_secs;
use crate::identify::models::JobState;
use crate::jobs::{OperationKind, OperationState};

/// Deferred jobs the activity card lists by name (v2 limit).
const DEFERRED_JOB_LIMIT: i64 = 20;

/// Stream rows the activity revisions always carry.
const STREAMS: [&str; 3] = ["scan", "identification", "operation"];

/// What the identification pause switch says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueueSwitch {
    Running,
    Paused,
}

crate::scan::models::spelled!(QueueSwitch {
    Running => "running",
    Paused => "paused",
});

/// The identification pause switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentificationControl {
    /// True while an administrator has identification paused.
    pub paused: bool,
    /// Bumped on every pause and resume; callers send it back to guard
    /// against acting on a stale view.
    pub row_revision: u64,
}

/// Why a pause or resume did not apply.
#[derive(Debug)]
pub enum ControlError {
    /// The switch moved since the caller read it.
    Stale,
    /// The database refused the change, was busy, or is closing.
    Store(DbError),
}

impl From<DbError> for ControlError {
    fn from(error: DbError) -> Self {
        Self::Store(error)
    }
}

impl From<rusqlite::Error> for ControlError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stale => f.write_str("the identification switch moved"),
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

/// One deferred identification job, named for the activity card.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredJob {
    pub job_id: String,
    pub local_album_id: Option<String>,
    pub album_title: Option<String>,
    pub artist_name: Option<String>,
    pub last_failure_code: String,
    pub attempt_count: u64,
    /// Unix seconds before which the job will not run again.
    pub not_before: Option<f64>,
    pub updated_at: f64,
}

/// Aggregate identification queue state. Times are unix seconds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IdentificationSnapshot {
    pub paused: bool,
    pub control_revision: u64,
    pub queued: u64,
    pub running: u64,
    /// Jobs waiting out a backoff after a failed attempt.
    pub backing_off: u64,
    pub succeeded: u64,
    pub failed: u64,
    /// Jobs that ran out of retries or need a person to look.
    pub attention: u64,
    /// Finished jobs whose album was identified without a review.
    pub identified: u64,
    pub needs_review: u64,
    /// Reviews settled by keeping the album's own tags.
    pub kept_local: u64,
    pub started_at: Option<f64>,
    pub updated_at: Option<f64>,
    /// Live jobs carrying a failure from an earlier attempt.
    pub deferred_count: u64,
    pub deferred_reason_counts: BTreeMap<String, u64>,
    pub deferred_jobs: Vec<DeferredJob>,
    /// Live jobs a worker could claim right now.
    pub claimable: u64,
    /// Priority of the job running now, else the next one waiting.
    pub active_priority: Option<i64>,
    /// Latest failed job: its id and when it failed.
    pub failure: Option<(String, f64)>,
    /// Foreground library operations still in flight.
    pub foreground_operations: u64,
}

impl IdentificationSnapshot {
    /// Jobs still to finish: queued, running, or backing off.
    pub fn waiting(&self) -> u64 {
        self.queued + self.running + self.backing_off
    }

    /// Jobs that finished one way or another.
    pub fn completed(&self) -> u64 {
        self.succeeded + self.failed + self.attention
    }
}

fn count(conn: &Connection, sql: &str) -> rusqlite::Result<u64> {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
        .map(|value| value.max(0) as u64)
}

/// Read the pause switch. A database without the row reads as running.
pub fn identification_control(conn: &Connection) -> rusqlite::Result<IdentificationControl> {
    let row = conn
        .query_row(
            crate::activity::sql::IDENTIFICATION_CONTROL.text(),
            [],
            ControlRow::read,
        )
        .optional()?;
    Ok(match row {
        Some(row) => IdentificationControl {
            paused: row.state == QueueSwitch::Paused,
            row_revision: row.row_revision.max(0) as u64,
        },
        None => IdentificationControl {
            paused: false,
            row_revision: 1,
        },
    })
}

/// Pause or resume identification (v2 `pause_identification_queue` and
/// `resume_identification_queue`). With `expected` set, the switch must
/// still be at that revision. Returns the new revision. Runs inside the
/// caller's write.
pub fn set_identification_paused(
    tx: &Connection,
    paused: bool,
    requested_by_user_id: Option<&str>,
    now: f64,
    expected: Option<u64>,
) -> Result<u64, ControlError> {
    tx.execute(
        crate::activity::sql::ENSURE_IDENTIFICATION_CONTROL.text(),
        [],
    )?;
    let expected = expected.map(|revision| revision as i64);
    let revision: Option<i64> = if paused {
        tx.query_row(
            crate::activity::sql::PAUSE_IDENTIFICATION.text(),
            params![crate::clock::ms(now), requested_by_user_id, expected],
            |row| row.get(0),
        )
        .optional()?
    } else {
        tx.query_row(
            crate::activity::sql::RESUME_IDENTIFICATION.text(),
            params![expected],
            |row| row.get(0),
        )
        .optional()?
    };
    let Some(revision) = revision else {
        return Err(ControlError::Stale);
    };
    tx.execute(crate::activity::sql::BUMP_IDENTIFICATION_STREAM.text(), [])?;
    Ok(revision.max(0) as u64)
}

/// Aggregate identification state for the activity feed (v2
/// `get_identification_activity_snapshot`). `now` is unix seconds.
pub fn identification_snapshot(
    conn: &Connection,
    now: f64,
) -> rusqlite::Result<IdentificationSnapshot> {
    let control = identification_control(conn)?;
    let mut snapshot = IdentificationSnapshot {
        paused: control.paused,
        control_revision: control.row_revision,
        ..IdentificationSnapshot::default()
    };
    let now_ms = (now * 1000.0) as i64;
    {
        // A running job whose lease ran out is not running: its worker is
        // gone and the next claim picks it up again, so it counts as queued.
        // A failure the album's later success settled is not counted, the
        // same rule as the latest failure below. Neither is a job that
        // ended because its album changed (files moved mid-attempt, the
        // album gone): nothing failed, and a newer job takes the album.
        let mut stmt = conn.prepare(crate::activity::sql::JOB_COUNTS_BY_LIVE_STATE.text())?;
        for row in stmt.query_map(params![now_ms], StateCount::read)? {
            let row = row?;
            let total = row.total.max(0) as u64;
            match row.live_state {
                JobState::Queued => snapshot.queued = total,
                JobState::Running => snapshot.running = total,
                JobState::Deferred => snapshot.backing_off = total,
                JobState::Succeeded => snapshot.succeeded = total,
                JobState::Failed => snapshot.failed = total,
                JobState::Attention => snapshot.attention = total,
            }
        }
    }
    let totals = conn.query_row(
        crate::activity::sql::JOB_TOTALS.text(),
        params![now_ms],
        JobTotals::read,
    )?;
    snapshot.started_at = totals.started_at.map(at_secs);
    snapshot.updated_at = totals.updated_at.map(at_secs);
    snapshot.deferred_count = totals.deferred.unwrap_or(0).max(0) as u64;
    snapshot.claimable = totals.claimable.unwrap_or(0).max(0) as u64;
    snapshot.active_priority = conn
        .query_row(
            crate::activity::sql::NEXT_JOB_PRIORITY.text(),
            params![now_ms],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    snapshot.needs_review = count(conn, crate::activity::sql::PENDING_REVIEW_COUNT.text())?;
    snapshot.kept_local = count(conn, crate::activity::sql::REJECTED_REVIEW_COUNT.text())?;
    snapshot.identified = count(conn, crate::activity::sql::MATCHED_JOB_COUNT.text())?;
    // A failure the album's own later success settled needs nobody's
    // attention, and a job that ended because its album changed is no
    // failure at all.
    snapshot.failure = conn
        .query_row(
            crate::activity::sql::FAILED_JOBS.text(),
            [],
            FailedJob::read,
        )
        .map(|job| (job.id, at_secs(job.updated_at)))
        .optional()?;
    snapshot.foreground_operations =
        count(conn, crate::activity::sql::ACTIVE_OPERATION_COUNT.text())?;
    {
        let mut stmt = conn.prepare(crate::activity::sql::WAITING_FAILURE_COUNTS.text())?;
        for row in stmt.query_map([], FailureCount::read)? {
            let row = row?;
            snapshot
                .deferred_reason_counts
                .insert(row.failure_code, row.total.max(0) as u64);
        }
    }
    {
        let mut stmt = conn.prepare(crate::activity::sql::WAITING_JOBS_WITH_FAILURE.text())?;
        for row in stmt.query_map(params![DEFERRED_JOB_LIMIT], WaitingJob::read)? {
            let job = row?;
            snapshot.deferred_jobs.push(DeferredJob {
                job_id: job.id,
                local_album_id: job.local_album_id,
                album_title: job.title,
                artist_name: job.display_name,
                last_failure_code: job.failure_code,
                attempt_count: job.attempts.max(0) as u64,
                not_before: (job.not_before_at.0 > 0).then(|| at_secs(job.not_before_at)),
                updated_at: at_secs(job.updated_at),
            });
        }
    }
    Ok(snapshot)
}

/// Change revisions for every library activity stream (v2
/// `get_library_revisions`): `scan`, `identification`, `operation`, and
/// `catalog`. A client compares them to know what to refetch.
/// One library operation job still in flight: a re-identification, an
/// identity repair or preparation, or catalog cleanup.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationWork {
    pub id: String,
    pub kind: OperationKind,
    /// A repair's purpose (`existing_matches`, `management_readiness`,
    /// `catalog_identity_hygiene`).
    pub purpose: Option<String>,
    /// A repair's phase: `dry_run` (checking) or `apply`.
    pub phase: Option<String>,
    pub state: OperationState,
    pub expected: u64,
    pub completed: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub skipped: u64,
    pub started_at: Option<f64>,
    pub updated_at: f64,
}

/// Operation jobs queued, running or paused, newest first (at most 20).
pub fn operation_work(conn: &Connection) -> rusqlite::Result<Vec<OperationWork>> {
    let mut stmt = conn.prepare(crate::activity::sql::OPERATION_WORK.text())?;
    let count = |value: i64| value.max(0) as u64;
    stmt.query_map([], OperationRow::read)?
        .map(|row| {
            row.map(|job| OperationWork {
                id: job.id,
                kind: job.kind,
                purpose: job.purpose,
                phase: job.phase,
                state: job.state,
                expected: count(job.expected_work_count),
                completed: count(job.completed_count),
                succeeded: count(job.succeeded_count),
                failed: count(job.failed_count),
                skipped: count(job.skipped_count),
                started_at: job.started_at.map(at_secs),
                updated_at: at_secs(job.updated_at),
            })
        })
        .collect()
}

pub fn library_revisions(conn: &Connection) -> rusqlite::Result<BTreeMap<String, u64>> {
    let mut revisions = BTreeMap::new();
    for kind in STREAMS {
        let value = conn
            .query_row(
                crate::activity::sql::STREAM_REVISION.text(),
                params![kind],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        revisions.insert(kind.to_owned(), value.max(0) as u64);
    }
    revisions.insert(
        "catalog".to_owned(),
        count(conn, crate::activity::sql::CATALOG_REVISION.text())?,
    );
    Ok(revisions)
}
