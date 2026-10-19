-- 0069_durable_loops.sql - one liveness row per background loop.
--
--   durable_job_registry (dropped)
--       one row per background job with a kind, an unused wakeup channel,
--       a state and heartbeat and update times in REAL unix seconds
--
--   durable_loops (new)
--       one row per loop the supervisor runs: its state, last heartbeat,
--       the code of its last failure, and when the row last changed, in
--       unix milliseconds
--
-- Nothing is carried over. The supervisor clears the table and rebuilds
-- every row each time the server starts, and nothing reads the old rows'
-- history. last_error holds a short fixed code the supervisor writes,
-- never the text of an error.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TABLE durable_job_registry;

CREATE TABLE durable_loops (
    name TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('idle', 'running', 'stopped', 'failed')),
    last_heartbeat_at INTEGER,
    last_error TEXT CHECK (last_error IS NULL OR length(last_error) <= 64),
    updated_at INTEGER NOT NULL
) STRICT;

-- Migration high-water mark.
PRAGMA user_version = 69;
