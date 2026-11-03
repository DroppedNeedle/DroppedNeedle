-- 0134_kernel_loop_states.sql - a loop row's state lists the states the
-- supervisor writes.
--
-- durable_loops.state drops idle, which nothing writes since every loop runs
-- on the supervisor. The rows are cleared and written again at every start,
-- so the table is made again empty rather than copied: no row is lost that
-- the next start would not have cleared.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TABLE durable_loops;
CREATE TABLE durable_loops (
    name TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('running', 'stopped', 'failed')),
    last_heartbeat_at INTEGER,
    last_error TEXT CHECK (last_error IS NULL OR length(last_error) <= 64),
    updated_at INTEGER NOT NULL
) STRICT;

-- Migration high-water mark.
PRAGMA user_version = 134;
