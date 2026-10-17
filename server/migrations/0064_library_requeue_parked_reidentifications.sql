-- 0064_library_requeue_parked_reidentifications.sql - re-identifications
-- that wait on a choice go back to the queue.
--
-- A retried review used to score the album's candidates and park its job
-- as `ready` until someone picked one, and no screen offers that pick any
-- more. The worker now hands the album to the identification queue
-- instead, so each parked job is put back in the queue to be handed over:
--
--   library_operation_work               back to `pending`, its result and
--                                        failure cleared
--   library_reidentification_snapshots   the stale candidates and pick
--                                        cleared
--   library_operation_jobs               back to `queued` with no ending,
--                                        control or counts
--
-- Only `explicit_reidentification` jobs in `ready` change. Repair reports
-- waiting for Apply, and paused, stopped or failed jobs, stay as they are.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

UPDATE library_operation_work
SET state = 'pending',
    result_json = NULL,
    failure_code = NULL,
    row_revision = row_revision + 1
WHERE job_id IN (
    SELECT id FROM library_operation_jobs
    WHERE kind = 'explicit_reidentification' AND state = 'ready'
);

UPDATE library_reidentification_snapshots
SET result_json = NULL,
    selected_candidate_key = NULL
WHERE job_id IN (
    SELECT id FROM library_operation_jobs
    WHERE kind = 'explicit_reidentification' AND state = 'ready'
);

UPDATE library_operation_jobs
SET state = 'queued',
    terminal_code = NULL,
    terminal_at = NULL,
    next_attempt_at = NULL,
    control_request = 'none',
    completed_count = 0,
    succeeded_count = 0,
    failed_count = 0,
    skipped_count = 0,
    reidentification_attempt_count = 0,
    row_revision = row_revision + 1,
    event_revision = event_revision + 1,
    updated_at = (julianday('now') - 2440587.5) * 86400.0
WHERE kind = 'explicit_reidentification' AND state = 'ready';

-- Migration high-water mark.
PRAGMA user_version = 64;
