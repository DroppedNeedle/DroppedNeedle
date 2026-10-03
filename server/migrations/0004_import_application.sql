-- 0004_import_application.sql - stage-11 import application record.
--
-- The stage plan reserved the 0002 slot for this; 0002 (stage-7 downloads
-- idempotency) and 0003 (stage-10 plugin tick state) landed first, so the
-- import record takes the next free version. All imported entity tables
-- already exist in the 0001 baseline; this migration adds only the audit
-- trail: one row per committed import, written inside the import
-- transaction (dry runs write nothing, failures roll back).
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS import_runs (
    id            TEXT PRIMARY KEY,
    instance_id   TEXT NOT NULL,
    exported_at   TEXT NOT NULL,
    exit_code     TEXT NOT NULL,
    entity_counts TEXT NOT NULL,
    applied_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_import_runs_instance
    ON import_runs(instance_id);

-- Migration high-water mark. The boot assertion compares this against the
-- newest embedded migration and refuses to serve on mismatch.
PRAGMA user_version = 4;
