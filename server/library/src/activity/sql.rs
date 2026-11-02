//! The SQL of the activity module.

use droppedneedle_kernel::db::time::Millis;

use super::store::QueueSwitch;
use crate::identify::models::JobState;
use crate::jobs::{OperationKind, OperationState};

droppedneedle_kernel::sql! {
    pub(super) IDENTIFICATION_CONTROL = "SELECT state, row_revision FROM library_work_control \
             WHERE queue_kind = 'identification'";
    pub(super) ENSURE_IDENTIFICATION_CONTROL = "INSERT OR IGNORE INTO library_work_control (queue_kind, state) \
         VALUES ('identification', 'running')";
    pub(super) PAUSE_IDENTIFICATION = "UPDATE library_work_control SET state = 'paused', requested_at = ?1, \
             requested_by_user_id = ?2, row_revision = row_revision + 1 \
             WHERE queue_kind = 'identification' AND (?3 IS NULL OR row_revision = ?3) \
             RETURNING row_revision";
    pub(super) RESUME_IDENTIFICATION = "UPDATE library_work_control SET state = 'running', requested_at = NULL, \
             requested_by_user_id = NULL, high_priority_claim_count = 0, \
             row_revision = row_revision + 1 \
             WHERE queue_kind = 'identification' AND (?1 IS NULL OR row_revision = ?1) \
             RETURNING row_revision";
    pub(super) BUMP_IDENTIFICATION_STREAM = "UPDATE library_event_stream_revisions SET value = value + 1 \
         WHERE stream_kind = 'identification'";
    pub(super) JOB_COUNTS_BY_LIVE_STATE = "SELECT CASE WHEN j.state = 'running' AND NOT (j.lease_expires_at > ?1) \
             THEN 'queued' ELSE j.state END AS live_state, COUNT(*) AS total \
             FROM library_identify_jobs j \
             WHERE NOT (j.state = 'failed' \
                 AND COALESCE(j.failure_code, '') IN ('INPUT_MOVED','ALBUM_GONE')) \
             AND NOT (j.state IN ('failed','attention') \
                 AND EXISTS (SELECT 1 FROM library_identify_jobs later \
                     WHERE later.local_album_id = j.local_album_id \
                     AND later.state = 'succeeded' AND later.updated_at > j.updated_at)) \
             GROUP BY live_state";
    pub(super) JOB_TOTALS = "SELECT MIN(created_at) AS started_at, MAX(updated_at) AS updated_at, \
         SUM(CASE WHEN failure_code IS NOT NULL THEN 1 ELSE 0 END) AS deferred, \
         SUM(CASE WHEN (state IN ('queued','deferred') AND not_before_at <= ?1) \
             OR (state = 'running' AND NOT (lease_expires_at > ?1)) THEN 1 ELSE 0 END) AS claimable \
         FROM library_identify_jobs WHERE state IN ('queued','running','deferred')";
    pub(super) NEXT_JOB_PRIORITY = "SELECT priority FROM library_identify_jobs \
             WHERE state IN ('queued','running','deferred') \
             ORDER BY (state = 'running' AND lease_expires_at > ?1) DESC, \
             priority, created_at, rowid LIMIT 1";
    pub(super) PENDING_REVIEW_COUNT = "SELECT COUNT(*) FROM library_identify_reviews WHERE state = 'pending'";
    pub(super) REJECTED_REVIEW_COUNT = "SELECT COUNT(*) FROM library_identify_reviews WHERE state = 'rejected'";
    pub(super) MATCHED_JOB_COUNT = "SELECT COUNT(*) FROM library_identify_jobs j WHERE j.state = 'succeeded' \
         AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
             WHERE r.local_album_id = j.local_album_id AND r.state = 'pending')";
    pub(super) FAILED_JOBS = "SELECT j.id, j.updated_at FROM library_identify_jobs j \
             WHERE j.state IN ('failed','attention') \
             AND NOT (j.state = 'failed' \
                 AND COALESCE(j.failure_code, '') IN ('INPUT_MOVED','ALBUM_GONE')) \
             AND NOT EXISTS (SELECT 1 FROM library_identify_jobs later \
                 WHERE later.local_album_id = j.local_album_id \
                 AND later.state = 'succeeded' AND later.updated_at > j.updated_at) \
             ORDER BY j.updated_at DESC, j.id DESC LIMIT 1";
    pub(super) ACTIVE_OPERATION_COUNT = "SELECT COUNT(*) FROM library_operation_jobs \
         WHERE state IN ('queued','running','paused')";
    pub(super) WAITING_FAILURE_COUNTS = "SELECT failure_code, COUNT(*) AS total FROM library_identify_jobs \
             WHERE state IN ('queued','running','deferred') AND failure_code IS NOT NULL \
             GROUP BY failure_code";
    pub(super) WAITING_JOBS_WITH_FAILURE = "SELECT j.id, j.local_album_id, a.title, ar.display_name, j.failure_code, \
             j.attempts, j.not_before_at, j.updated_at \
             FROM library_identify_jobs j \
             LEFT JOIN local_albums a ON a.id = j.local_album_id \
             LEFT JOIN local_artists ar ON ar.id = a.album_artist_id \
             WHERE j.state IN ('queued','running','deferred') AND j.failure_code IS NOT NULL \
             ORDER BY j.priority, j.created_at, j.rowid LIMIT ?1";
    pub(super) OPERATION_WORK = "SELECT j.id, j.kind, s.purpose, s.phase, \
         j.state, j.expected_work_count, j.completed_count, j.succeeded_count, \
         j.failed_count, j.skipped_count, j.started_at, j.updated_at \
         FROM library_operation_jobs j LEFT JOIN library_repair_snapshots s ON s.job_id = j.id \
         WHERE j.state IN ('queued','running','paused') \
         ORDER BY j.updated_at DESC, j.id DESC LIMIT 20";
    pub(super) STREAM_REVISION = "SELECT value FROM library_event_stream_revisions WHERE stream_kind = ?1";
    pub(super) CATALOG_REVISION = "SELECT COALESCE((SELECT value FROM library_catalog_revision \
             WHERE singleton = 1), 0)";
    pub(super) row ControlRow for [IDENTIFICATION_CONTROL] {
        state: QueueSwitch,
        row_revision: i64,
    }
    pub(super) row StateCount for [JOB_COUNTS_BY_LIVE_STATE] {
        live_state: JobState,
        total: i64,
    }
    pub(super) row JobTotals for [JOB_TOTALS] {
        started_at: Option<Millis>,
        updated_at: Option<Millis>,
        deferred: Option<i64>,
        claimable: Option<i64>,
    }
    pub(super) row FailedJob for [FAILED_JOBS] {
        id: String,
        updated_at: Millis,
    }
    pub(super) row FailureCount for [WAITING_FAILURE_COUNTS] {
        failure_code: String,
        total: i64,
    }
    pub(super) row WaitingJob for [WAITING_JOBS_WITH_FAILURE] {
        id: String,
        local_album_id: Option<String>,
        title: Option<String>,
        display_name: Option<String>,
        failure_code: String,
        attempts: i64,
        not_before_at: Millis,
        updated_at: Millis,
    }
    pub(super) row OperationRow for [OPERATION_WORK] {
        id: String,
        kind: OperationKind,
        purpose: Option<String>,
        phase: Option<String>,
        state: OperationState,
        expected_work_count: i64,
        completed_count: i64,
        succeeded_count: i64,
        failed_count: i64,
        skipped_count: i64,
        started_at: Option<Millis>,
        updated_at: Millis,
    }
}
