-- 0066_drop_durable_work_wakeups.sql - drop the wakeup channel table
-- nothing reads.
--
--   durable_work_wakeups
--       one row per wakeup channel (scan, identification, operation,
--       contribution) with request and consume counters
--
-- The channels were never wired: no worker requested or waited on one,
-- so the four rows only ever held their seed values. The job registry
-- (durable_job_registry) stays, rows and all; its wakeup_channel column
-- is always NULL and stays until that table is replaced. No other table,
-- trigger or view refers to this one.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TABLE durable_work_wakeups;

-- Migration high-water mark.
PRAGMA user_version = 66;
