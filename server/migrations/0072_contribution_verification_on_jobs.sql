-- 0072_contribution_verification_on_jobs.sql - contribution verification
-- runs on the kernel's jobs table.
--
--   library_contribution_drafts (column added)
--       last_verification_failure: the code of the last verification that
--       ended without a link, which the needs-review reason reads
--
--   durable_jobs (rows carried)
--       every queued or running verification becomes one queued
--       'library.contribution_verify' job keyed by its contribution, with
--       its attempts, due time and last code. A running row was held by a
--       stopped process, so it is queued again at once.
--
--   library_contribution_verification_jobs (dropped, with its indexes)
--
-- Times move from REAL unix seconds to integer unix milliseconds. The old
-- table allowed one queued or running row per contribution, so no key
-- gets two queued jobs.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_contribution_drafts ADD COLUMN last_verification_failure TEXT;

UPDATE library_contribution_drafts
SET last_verification_failure = (
    SELECT j.last_failure_code
    FROM library_contribution_verification_jobs j
    WHERE j.contribution_id = library_contribution_drafts.id
      AND j.state IN ('needs_review', 'failed', 'cancelled')
    ORDER BY j.updated_at DESC, j.id DESC
    LIMIT 1
);

INSERT INTO durable_jobs (kind, idempotency_key, state, attempts, not_before_at, payload_json,
                          last_error, created_at, updated_at)
SELECT 'library.contribution_verify',
       contribution_id,
       'queued',
       attempt_count,
       CAST(round(not_before * 1000) AS INTEGER),
       '{}',
       substr(last_failure_code, 1, 64),
       CAST(round(created_at * 1000) AS INTEGER),
       CAST(round(updated_at * 1000) AS INTEGER)
FROM library_contribution_verification_jobs
WHERE state IN ('queued', 'running');

DROP TABLE library_contribution_verification_jobs;

-- Migration high-water mark.
PRAGMA user_version = 72;
