-- 0070_durable_jobs.sql - the kernel's one jobs table.
--
--   durable_jobs (new)
--       one row per job: its kind, the idempotency key that dedupes its
--       enqueues, its state, the claims counted so far, when it may next
--       run, the lease of the worker holding it, a JSON payload of ids and
--       plain values, the code of its last failure, and when it was
--       enqueued, last changed and finished, in unix milliseconds
--
-- A key has at most one queued row; a done or failed row never blocks a
-- new one. kind takes no value list because the areas above the kernel
-- add kinds. last_error holds a short fixed code, never the text of an
-- error. Nothing is carried over: the table is new.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE durable_jobs (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 64),
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'done', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_at INTEGER NOT NULL,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json) AND length(payload_json) <= 16384),
    last_error TEXT CHECK (last_error IS NULL OR length(last_error) <= 64),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    finished_at INTEGER,
    CHECK (state <> 'running' OR (lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK ((state IN ('done', 'failed')) = (finished_at IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX durable_jobs_one_queued ON durable_jobs (kind, idempotency_key)
    WHERE state = 'queued';
CREATE INDEX durable_jobs_claim ON durable_jobs (kind, state, not_before_at, id);
CREATE INDEX durable_jobs_by_key ON durable_jobs (kind, idempotency_key, id);

-- Migration high-water mark.
PRAGMA user_version = 70;
