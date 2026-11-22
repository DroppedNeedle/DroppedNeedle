-- no-transaction
PRAGMA foreign_keys=OFF;

BEGIN IMMEDIATE;

-- Deezer, through Deemix, becomes a download source: an attempt may be
-- stored under the source `deezer`.
--
-- download_import_decisions points at download_attempts, so this file
-- rebuilds download_attempts with foreign keys off, by the recipe in
-- kernel/src/db/migrate.rs. Every row keeps its id and rowid, and the
-- indexes and triggers the drop takes with it come back as they were.

CREATE TEMP TABLE download_attempts_deezer_guard_keys (
    ok INTEGER CONSTRAINT guard_foreign_keys_off CHECK (ok = 1)
);
INSERT INTO download_attempts_deezer_guard_keys
SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;

CREATE TEMP TABLE download_attempts_deezer_guard_version (
    ok INTEGER CONSTRAINT guard_not_applied_yet CHECK (ok = 1)
);
INSERT INTO download_attempts_deezer_guard_version
SELECT (SELECT user_version FROM pragma_user_version) < 156;

CREATE TEMP TABLE download_attempts_deezer_orphans (
    child TEXT,
    child_rowid INTEGER,
    parent TEXT
);
INSERT INTO download_attempts_deezer_orphans (child, child_rowid, parent)
SELECT "table", "rowid", parent FROM pragma_foreign_key_check;
CREATE INDEX download_attempts_deezer_orphans_key
    ON download_attempts_deezer_orphans (child, child_rowid, parent);

CREATE TABLE download_attempts_new (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('soulseek','usenet','deezer') OR source LIKE 'plugin:%'),
    candidate_index INTEGER NOT NULL CHECK(candidate_index >= 0),
    job_name TEXT NOT NULL DEFAULT '',
    handle_json TEXT NOT NULL,
    remote_storage TEXT,
    mount_root TEXT,
    workspace_path TEXT,
    materialized_paths_json TEXT NOT NULL DEFAULT '[]',
    materialized_fingerprints_json TEXT NOT NULL DEFAULT '{}',
    publisher_bundle_ids_json TEXT NOT NULL DEFAULT '[]',
    legacy_reconciled INTEGER NOT NULL DEFAULT 0 CHECK(legacy_reconciled IN (0,1)),
    state TEXT NOT NULL CHECK(state IN (
        'acquiring','in_use','cleanup_pending','workspace_removed','complete',
        'preserved','needs_attention'
    )),
    disposition TEXT NOT NULL DEFAULT 'undecided'
        CHECK(disposition IN ('undecided','discard','preserve')),
    cleanup_failures INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_failures >= 0),
    next_retry_at INTEGER NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;

INSERT INTO download_attempts_new (
    rowid, id, task_id, source, candidate_index, job_name, handle_json, remote_storage,
    mount_root, workspace_path, materialized_paths_json, materialized_fingerprints_json,
    publisher_bundle_ids_json, legacy_reconciled, state, disposition, cleanup_failures,
    next_retry_at, lease_owner, lease_expires_at, error_code, created_at, updated_at,
    completed_at, row_revision)
SELECT
    rowid, id, task_id, source, candidate_index, job_name, handle_json, remote_storage,
    mount_root, workspace_path, materialized_paths_json, materialized_fingerprints_json,
    publisher_bundle_ids_json, legacy_reconciled, state, disposition, cleanup_failures,
    next_retry_at, lease_owner, lease_expires_at, error_code, created_at, updated_at,
    completed_at, row_revision
FROM download_attempts;

DROP TABLE download_attempts;

ALTER TABLE download_attempts_new RENAME TO download_attempts;

CREATE INDEX idx_download_attempts_cleanup
    ON download_attempts(state, next_retry_at, lease_expires_at);

CREATE INDEX idx_download_attempts_job
    ON download_attempts(source, job_name);

CREATE INDEX idx_download_attempts_task
    ON download_attempts(task_id, candidate_index, created_at);

CREATE TRIGGER download_activity_attempt_insert
AFTER INSERT ON download_attempts
WHEN EXISTS (SELECT 1 FROM download_tasks WHERE id = NEW.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = NEW.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_attempt_state
AFTER UPDATE OF state ON download_attempts
WHEN OLD.state IS NOT NEW.state
    AND EXISTS (SELECT 1 FROM download_tasks WHERE id = NEW.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = NEW.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_attempt_delete
AFTER DELETE ON download_attempts
WHEN EXISTS (SELECT 1 FROM download_tasks WHERE id = OLD.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = OLD.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TEMP TABLE download_attempts_deezer_guard_orphans (
    ok INTEGER CONSTRAINT guard_no_new_orphans CHECK (ok = 1)
);
INSERT INTO download_attempts_deezer_guard_orphans
SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, "rowid" AS child_rowid, parent, COUNT(*) AS found
        FROM pragma_foreign_key_check
        GROUP BY 1, 2, 3
    ) AS now
    WHERE now.found > (
        SELECT COUNT(*) FROM download_attempts_deezer_orphans saved
        WHERE saved.child = now.child
          AND saved.child_rowid IS now.child_rowid
          AND saved.parent = now.parent
    )
);

DROP TABLE download_attempts_deezer_guard_keys;
DROP TABLE download_attempts_deezer_guard_version;
DROP TABLE download_attempts_deezer_orphans;
DROP TABLE download_attempts_deezer_guard_orphans;

PRAGMA user_version = 156;

COMMIT;

PRAGMA foreign_keys=ON;
