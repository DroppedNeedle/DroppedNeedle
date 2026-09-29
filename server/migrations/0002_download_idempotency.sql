-- 0002_download_idempotency.sql - stage-7 downloads slice: idempotency keys.
--
-- INTEGRATOR: this file is owned by the stage-7 downloads slice. If another
-- slice also landed a 0002, renumber this file (and its user_version stamp)
-- to the next free version; the table below is the only new object.
--
-- v2 reached idempotency through guarded commands (expected_candidate_index
-- CAS on try_next_source, generation CAS on request relink) rather than a
-- key table. v3 keeps those CAS guards and adds one small table so a repeat
-- enqueue or retry after a crash is a cheap key lookup instead of a second
-- fetch against the download client.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS download_idempotency_keys (
    key TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    created_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_download_idempotency_task
    ON download_idempotency_keys(task_id);

-- Migration high-water mark. The boot assertion compares this against the
-- newest embedded migration and refuses to serve on mismatch.
PRAGMA user_version = 2;
