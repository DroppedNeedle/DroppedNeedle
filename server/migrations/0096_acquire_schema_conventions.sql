-- 0096_acquire_schema_conventions.sql - the rest of the acquire tables
-- follow the schema conventions.
--
-- Every acquire table the request rebuild (0076) and the held list (0085)
-- left is rebuilt as a STRICT table, rows, indexes and triggers carried:
--
--   * every point in time is integer milliseconds. The download journal,
--     the drop imports, search jobs, the quarantine, personal-mix
--     approvals and seen candidates held REAL seconds; Spotify states,
--     edition marks and follow cursors held integer seconds. A time that
--     does not read as one is NULL where the column allows it; a
--     creation or last-write time falls back to its partner, then now;
--   * a whole-number or flag column holding something else reads NULL,
--     or its default where it has one; flags are checked to be 0 or 1;
--   * download_tasks and search_jobs lose quality_snapshot_hash and
--     quality_snapshot_summary, which nothing has written since v2;
--   * acquire_follow_cursors loses followers, always written '[]';
--   * personal_mix_approvals.state is checked against the approval
--     states (a value outside them asks again: pending), and the reviewer
--     is a key that clears when the account goes;
--   * new keys: an idempotency key, an import decision and an edition
--     mark go with their download; a decision's attempt clears when the
--     attempt goes; a search job goes with its user; a seen candidate goes
--     with its wanted watch. Rows whose parent is already gone are
--     dropped while copying, since the keys are enforced, and a decision
--     naming a missing attempt keeps its row without it.
--
-- Not keys, on purpose: an attempt outlives its download (its cleanup
-- debt must survive the queue row), a drop job outlives its user (the
-- start-up sweep still clears its staging folder), a download keeps the
-- id of a search job pruned under it, and the activity revision of a
-- user is bumped by triggers while that user's rows are deleted.
--
-- Dropping a table drops its triggers, and with keys enforced it first
-- deletes the rows that reference it. So every trigger that names a
-- rebuilt table is dropped first and created again at the end, unchanged,
-- and the held entries of drop items (with their files) are set aside
-- while the drop tables are rebuilt and put back as they were.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Id counters of the AUTOINCREMENT tables rebuilt below. Dropping a table
-- drops its counter, so they are kept here and put back at the end: an id
-- the API handed out is never given to a new row.
CREATE TEMP TABLE acquire_kept_sequences AS
SELECT name, seq FROM sqlite_sequence
WHERE name IN ('download_import_decisions', 'download_quarantine', 'drop_import_items');

DROP TRIGGER download_activity_task_insert;
DROP TRIGGER download_activity_task_landed;
DROP TRIGGER download_activity_task_status;
DROP TRIGGER download_activity_task_search_link;
DROP TRIGGER download_activity_task_owner;
DROP TRIGGER download_activity_task_delete;
DROP TRIGGER download_activity_attempt_insert;
DROP TRIGGER download_activity_attempt_state;
DROP TRIGGER download_activity_attempt_delete;
DROP TRIGGER download_activity_held_entry_insert;
DROP TRIGGER download_activity_held_entry_state;
DROP TRIGGER download_activity_held_entry_delete;
DROP TRIGGER download_activity_held_entry_retry;
DROP TRIGGER download_activity_held_file_insert;
DROP TRIGGER download_activity_held_file_state;

-- Activity revisions.
CREATE TABLE download_activity_global_revision_new (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    revision INTEGER NOT NULL DEFAULT 0
) STRICT;
INSERT INTO download_activity_global_revision_new (singleton, revision)
SELECT singleton, COALESCE(CASE WHEN typeof(revision) IN ('integer', 'real')
    THEN CAST(revision AS INTEGER) END, 0)
FROM download_activity_global_revision WHERE singleton = 1;
DROP TABLE download_activity_global_revision;
ALTER TABLE download_activity_global_revision_new RENAME TO download_activity_global_revision;

CREATE TABLE download_activity_user_revisions_new (
    user_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL DEFAULT 0
) STRICT;
INSERT INTO download_activity_user_revisions_new (user_id, revision)
SELECT user_id, COALESCE(CASE WHEN typeof(revision) IN ('integer', 'real')
    THEN CAST(revision AS INTEGER) END, 0)
FROM download_activity_user_revisions;
DROP TABLE download_activity_user_revisions;
ALTER TABLE download_activity_user_revisions_new RENAME TO download_activity_user_revisions;

-- Downloads.
CREATE TABLE download_tasks_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    request_history_mbid TEXT,
    download_type TEXT NOT NULL DEFAULT 'album',
    release_group_mbid TEXT NOT NULL,
    release_mbid TEXT,
    release_track_mbid TEXT,
    recording_mbid TEXT,
    artist_mbid TEXT,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    track_title TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    year INTEGER,
    track_count INTEGER,
    track_duration_seconds REAL,
    download_client TEXT NOT NULL DEFAULT 'slskd',
    source TEXT NOT NULL DEFAULT 'soulseek',
    origin TEXT NOT NULL DEFAULT 'user',
    source_username TEXT,
    source_directory TEXT,
    search_query TEXT,
    search_job_id TEXT,
    candidate_index INTEGER,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK(status IN ('queued','downloading','processing',
                         'completed','partial','failed','cancelled')),
    preflight_score REAL,
    progress_percent INTEGER NOT NULL DEFAULT 0,
    total_size_bytes INTEGER,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    files_total INTEGER NOT NULL DEFAULT 0,
    files_completed INTEGER NOT NULL DEFAULT 0,
    files_failed INTEGER NOT NULL DEFAULT 0,
    quality_format TEXT,
    quality_bitrate INTEGER,
    quality_sample_rate INTEGER,
    quality_bit_depth INTEGER,
    advertised_queue_depth INTEGER,
    queue_position_start INTEGER,
    queue_position_end INTEGER,
    remote_queued INTEGER NOT NULL DEFAULT 0 CHECK (remote_queued IN (0, 1)),
    preferred_quality_fallback_at INTEGER,
    quality_pool_key TEXT,
    attempt_number INTEGER NOT NULL DEFAULT 0,
    attempt_total INTEGER NOT NULL DEFAULT 0,
    has_next_source INTEGER NOT NULL DEFAULT 0 CHECK (has_next_source IN (0, 1)),
    quality_snapshot_json TEXT,
    quality_preference_step INTEGER,
    quality_certainty TEXT,
    quality_provenance TEXT,
    manual_quality_override INTEGER NOT NULL DEFAULT 0
        CHECK (manual_quality_override IN (0, 1)),
    staging_path TEXT,
    final_path TEXT,
    error_message TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_polled_at INTEGER,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    completed_at INTEGER,
    cancelled_at INTEGER,
    updated_at INTEGER NOT NULL,
    wrong_product_verdict_at INTEGER,
    wrong_product_detail TEXT,
    outcome_code TEXT,
    plan_json TEXT NOT NULL DEFAULT '{}',
    picked_by_person INTEGER NOT NULL DEFAULT 0 CHECK (picked_by_person IN (0, 1))
) STRICT;

INSERT INTO download_tasks_new (rowid, id, user_id, request_history_mbid, download_type,
    release_group_mbid, release_mbid, release_track_mbid, recording_mbid, artist_mbid,
    artist_name, album_title, track_title, track_number, disc_number, year, track_count,
    track_duration_seconds, download_client, source, origin, source_username,
    source_directory, search_query, search_job_id, candidate_index, status, preflight_score,
    progress_percent, total_size_bytes, downloaded_bytes, files_total, files_completed,
    files_failed, quality_format, quality_bitrate, quality_sample_rate, quality_bit_depth,
    advertised_queue_depth, queue_position_start, queue_position_end, remote_queued,
    preferred_quality_fallback_at, quality_pool_key, attempt_number, attempt_total,
    has_next_source, quality_snapshot_json, quality_preference_step, quality_certainty,
    quality_provenance, manual_quality_override, staging_path, final_path, error_message,
    retry_count, last_polled_at, created_at, started_at, completed_at, cancelled_at,
    updated_at, wrong_product_verdict_at, wrong_product_detail, outcome_code, plan_json,
    picked_by_person)
SELECT rowid, id, user_id, request_history_mbid, download_type, release_group_mbid, release_mbid,
    release_track_mbid, recording_mbid, artist_mbid, artist_name, album_title, track_title,
    CASE WHEN typeof(track_number) IN ('integer', 'real') THEN CAST(track_number AS INTEGER) END,
    CASE WHEN typeof(disc_number) IN ('integer', 'real') THEN CAST(disc_number AS INTEGER) END,
    CASE WHEN typeof(year) IN ('integer', 'real') THEN CAST(year AS INTEGER) END,
    CASE WHEN typeof(track_count) IN ('integer', 'real') THEN CAST(track_count AS INTEGER) END,
    CASE WHEN typeof(track_duration_seconds) IN ('integer', 'real')
        THEN track_duration_seconds END,
    download_client, source, origin, source_username, source_directory, search_query,
    search_job_id,
    CASE WHEN typeof(candidate_index) IN ('integer', 'real')
        THEN CAST(candidate_index AS INTEGER) END,
    status,
    CASE WHEN typeof(preflight_score) IN ('integer', 'real') THEN preflight_score END,
    COALESCE(CASE WHEN typeof(progress_percent) IN ('integer', 'real')
        THEN CAST(progress_percent AS INTEGER) END, 0),
    CASE WHEN typeof(total_size_bytes) IN ('integer', 'real')
        THEN CAST(total_size_bytes AS INTEGER) END,
    COALESCE(CASE WHEN typeof(downloaded_bytes) IN ('integer', 'real')
        THEN CAST(downloaded_bytes AS INTEGER) END, 0),
    COALESCE(CASE WHEN typeof(files_total) IN ('integer', 'real')
        THEN CAST(files_total AS INTEGER) END, 0),
    COALESCE(CASE WHEN typeof(files_completed) IN ('integer', 'real')
        THEN CAST(files_completed AS INTEGER) END, 0),
    COALESCE(CASE WHEN typeof(files_failed) IN ('integer', 'real')
        THEN CAST(files_failed AS INTEGER) END, 0),
    quality_format,
    CASE WHEN typeof(quality_bitrate) IN ('integer', 'real')
        THEN CAST(quality_bitrate AS INTEGER) END,
    CASE WHEN typeof(quality_sample_rate) IN ('integer', 'real')
        THEN CAST(quality_sample_rate AS INTEGER) END,
    CASE WHEN typeof(quality_bit_depth) IN ('integer', 'real')
        THEN CAST(quality_bit_depth AS INTEGER) END,
    CASE WHEN typeof(advertised_queue_depth) IN ('integer', 'real')
        THEN CAST(advertised_queue_depth AS INTEGER) END,
    CASE WHEN typeof(queue_position_start) IN ('integer', 'real')
        THEN CAST(queue_position_start AS INTEGER) END,
    CASE WHEN typeof(queue_position_end) IN ('integer', 'real')
        THEN CAST(queue_position_end AS INTEGER) END,
    CASE WHEN typeof(remote_queued) IN ('integer', 'real') AND remote_queued != 0
        THEN 1 ELSE 0 END,
    CAST(round(CASE WHEN typeof(preferred_quality_fallback_at) IN ('integer', 'real')
        THEN preferred_quality_fallback_at * 1000.0
        ELSE (julianday(preferred_quality_fallback_at) - 2440587.5) * 86400000.0 END)
        AS INTEGER),
    quality_pool_key,
    COALESCE(CASE WHEN typeof(attempt_number) IN ('integer', 'real')
        THEN CAST(attempt_number AS INTEGER) END, 0),
    COALESCE(CASE WHEN typeof(attempt_total) IN ('integer', 'real')
        THEN CAST(attempt_total AS INTEGER) END, 0),
    CASE WHEN typeof(has_next_source) IN ('integer', 'real') AND has_next_source != 0
        THEN 1 ELSE 0 END,
    quality_snapshot_json,
    CASE WHEN typeof(quality_preference_step) IN ('integer', 'real')
        THEN CAST(quality_preference_step AS INTEGER) END,
    quality_certainty, quality_provenance,
    CASE WHEN typeof(manual_quality_override) IN ('integer', 'real')
        AND manual_quality_override != 0 THEN 1 ELSE 0 END,
    staging_path, final_path, error_message,
    COALESCE(CASE WHEN typeof(retry_count) IN ('integer', 'real')
        THEN CAST(retry_count AS INTEGER) END, 0),
    CAST(round(CASE WHEN typeof(last_polled_at) IN ('integer', 'real')
        THEN last_polled_at * 1000.0
        ELSE (julianday(last_polled_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
    COALESCE(
        CAST(round(CASE WHEN typeof(created_at) IN ('integer', 'real')
            THEN created_at * 1000.0
            ELSE (julianday(created_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round(CASE WHEN typeof(updated_at) IN ('integer', 'real')
            THEN updated_at * 1000.0
            ELSE (julianday(updated_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    CAST(round(CASE WHEN typeof(started_at) IN ('integer', 'real')
        THEN started_at * 1000.0
        ELSE (julianday(started_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
    CAST(round(CASE WHEN typeof(completed_at) IN ('integer', 'real')
        THEN completed_at * 1000.0
        ELSE (julianday(completed_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
    CAST(round(CASE WHEN typeof(cancelled_at) IN ('integer', 'real')
        THEN cancelled_at * 1000.0
        ELSE (julianday(cancelled_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
    COALESCE(
        CAST(round(CASE WHEN typeof(updated_at) IN ('integer', 'real')
            THEN updated_at * 1000.0
            ELSE (julianday(updated_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round(CASE WHEN typeof(created_at) IN ('integer', 'real')
            THEN created_at * 1000.0
            ELSE (julianday(created_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    CAST(round(CASE WHEN typeof(wrong_product_verdict_at) IN ('integer', 'real')
        THEN wrong_product_verdict_at * 1000.0
        ELSE (julianday(wrong_product_verdict_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
    wrong_product_detail, outcome_code, COALESCE(plan_json, '{}'),
    CASE WHEN typeof(picked_by_person) IN ('integer', 'real') AND picked_by_person != 0
        THEN 1 ELSE 0 END
FROM download_tasks
WHERE EXISTS (SELECT 1 FROM auth_users u WHERE u.id = download_tasks.user_id);

DROP TABLE download_tasks;
ALTER TABLE download_tasks_new RENAME TO download_tasks;

CREATE INDEX idx_download_tasks_status ON download_tasks(status);
CREATE INDEX idx_download_tasks_user ON download_tasks(user_id);
CREATE INDEX idx_download_tasks_rgmbid ON download_tasks(release_group_mbid);
CREATE INDEX idx_download_tasks_type ON download_tasks(download_type);
CREATE INDEX idx_download_tasks_username ON download_tasks(source_username);
CREATE INDEX idx_download_tasks_created ON download_tasks(created_at DESC);
CREATE INDEX idx_download_activity_owner_status
    ON download_tasks(user_id, status);
CREATE INDEX idx_download_activity_landed
    ON download_tasks(release_group_mbid,
        COALESCE(completed_at, updated_at) DESC, user_id)
    WHERE status IN ('completed','partial') AND release_group_mbid != '';

-- The attempt journal.
CREATE TABLE download_attempts_new (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('soulseek','usenet') OR source LIKE 'plugin:%'),
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

INSERT INTO download_attempts_new (id, task_id, source, candidate_index, job_name,
    handle_json, remote_storage, mount_root, workspace_path, materialized_paths_json,
    materialized_fingerprints_json, publisher_bundle_ids_json, legacy_reconciled, state,
    disposition, cleanup_failures, next_retry_at, lease_owner, lease_expires_at, error_code,
    created_at, updated_at, completed_at, row_revision)
SELECT id, task_id, source, CAST(candidate_index AS INTEGER), job_name, handle_json,
    remote_storage, mount_root, workspace_path, materialized_paths_json,
    materialized_fingerprints_json, publisher_bundle_ids_json, legacy_reconciled, state,
    disposition, CAST(cleanup_failures AS INTEGER),
    COALESCE(CAST(round(CASE WHEN typeof(next_retry_at) IN ('integer', 'real')
        THEN next_retry_at * 1000.0
        ELSE (julianday(next_retry_at) - 2440587.5) * 86400000.0 END) AS INTEGER), 0),
    lease_owner,
    CAST(round(CASE WHEN typeof(lease_expires_at) IN ('integer', 'real')
        THEN lease_expires_at * 1000.0
        ELSE (julianday(lease_expires_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
    error_code,
    COALESCE(
        CAST(round(CASE WHEN typeof(created_at) IN ('integer', 'real')
            THEN created_at * 1000.0
            ELSE (julianday(created_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round(CASE WHEN typeof(updated_at) IN ('integer', 'real')
            THEN updated_at * 1000.0
            ELSE (julianday(updated_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    COALESCE(
        CAST(round(CASE WHEN typeof(updated_at) IN ('integer', 'real')
            THEN updated_at * 1000.0
            ELSE (julianday(updated_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round(CASE WHEN typeof(created_at) IN ('integer', 'real')
            THEN created_at * 1000.0
            ELSE (julianday(created_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    CAST(round(CASE WHEN typeof(completed_at) IN ('integer', 'real')
        THEN completed_at * 1000.0
        ELSE (julianday(completed_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
    CAST(row_revision AS INTEGER)
FROM download_attempts;

DROP TABLE download_attempts;
ALTER TABLE download_attempts_new RENAME TO download_attempts;

CREATE INDEX idx_download_attempts_task
    ON download_attempts(task_id, candidate_index, created_at);
CREATE INDEX idx_download_attempts_cleanup
    ON download_attempts(state, next_retry_at, lease_expires_at);
CREATE INDEX idx_download_attempts_job
    ON download_attempts(source, job_name);

-- Idempotency keys. A dispatch claims its key before it writes the task,
-- in the same transaction, so the key is checked at commit.
CREATE TABLE download_idempotency_keys_new (
    key TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES download_tasks(id) ON DELETE CASCADE
        DEFERRABLE INITIALLY DEFERRED,
    operation TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO download_idempotency_keys_new (key, task_id, operation, created_at)
SELECT key, task_id, operation,
    COALESCE(CAST(round(CASE WHEN typeof(created_at) IN ('integer', 'real')
        THEN created_at * 1000.0
        ELSE (julianday(created_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER))
FROM download_idempotency_keys
WHERE task_id IN (SELECT id FROM download_tasks);
DROP TABLE download_idempotency_keys;
ALTER TABLE download_idempotency_keys_new RENAME TO download_idempotency_keys;
CREATE INDEX idx_download_idempotency_task
    ON download_idempotency_keys(task_id);

-- Import decisions.
CREATE TABLE download_import_decisions_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES download_tasks(id) ON DELETE CASCADE,
    attempt_id TEXT REFERENCES download_attempts(id) ON DELETE SET NULL,
    outcome TEXT NOT NULL
        CHECK(outcome IN ('imported','partial','held','rejected','deferred')),
    reason_code TEXT,
    detail TEXT,
    release_mbid TEXT,
    distance REAL,
    files_total INTEGER NOT NULL DEFAULT 0,
    files_imported INTEGER NOT NULL DEFAULT 0,
    files_held INTEGER NOT NULL DEFAULT 0,
    checks_json TEXT NOT NULL DEFAULT '[]',
    missing_positions TEXT NOT NULL DEFAULT '[]',
    decided_at INTEGER NOT NULL
) STRICT;
INSERT INTO download_import_decisions_new (id, task_id, attempt_id, outcome, reason_code,
    detail, release_mbid, distance, files_total, files_imported, files_held, checks_json,
    missing_positions, decided_at)
SELECT id, task_id,
    CASE WHEN attempt_id IN (SELECT id FROM download_attempts) THEN attempt_id END,
    outcome, reason_code, detail, release_mbid,
    CASE WHEN typeof(distance) IN ('integer', 'real') THEN distance END,
    COALESCE(CASE WHEN typeof(files_total) IN ('integer', 'real')
        THEN CAST(files_total AS INTEGER) END, 0),
    COALESCE(CASE WHEN typeof(files_imported) IN ('integer', 'real')
        THEN CAST(files_imported AS INTEGER) END, 0),
    COALESCE(CASE WHEN typeof(files_held) IN ('integer', 'real')
        THEN CAST(files_held AS INTEGER) END, 0),
    checks_json, missing_positions,
    COALESCE(CAST(round(CASE WHEN typeof(decided_at) IN ('integer', 'real')
        THEN decided_at * 1000.0
        ELSE (julianday(decided_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER))
FROM download_import_decisions
WHERE task_id IN (SELECT id FROM download_tasks);
DROP TABLE download_import_decisions;
ALTER TABLE download_import_decisions_new RENAME TO download_import_decisions;
CREATE INDEX idx_download_import_decisions_task
    ON download_import_decisions(task_id, decided_at DESC);
CREATE INDEX idx_download_import_decisions_attempt
    ON download_import_decisions(attempt_id);

-- The quarantine.
CREATE TABLE download_quarantine_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL DEFAULT 'soulseek',
    identity TEXT NOT NULL,
    release_group_mbid TEXT,
    reason TEXT NOT NULL
        CHECK(reason IN ('verify_failed','corrupt','fingerprint_mismatch',
                         'duration_mismatch','download_failed','manual')),
    quarantined_at INTEGER NOT NULL,
    UNIQUE (source, identity, release_group_mbid)
) STRICT;
INSERT INTO download_quarantine_new (id, source, identity, release_group_mbid, reason,
    quarantined_at)
SELECT id, source, identity, release_group_mbid, reason,
    COALESCE(CAST(round(CASE WHEN typeof(quarantined_at) IN ('integer', 'real')
        THEN quarantined_at * 1000.0
        ELSE (julianday(quarantined_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER))
FROM download_quarantine;
DROP TABLE download_quarantine;
ALTER TABLE download_quarantine_new RENAME TO download_quarantine;
CREATE INDEX idx_quarantine_lookup ON download_quarantine(source, identity);
CREATE INDEX idx_quarantine_quarantined_at ON download_quarantine(quarantined_at);

-- Drop imports. The held entries of drop items, and their files, are set
-- aside: dropping the item table deletes them through their keys.
CREATE TEMP TABLE kept_drop_entries AS
SELECT * FROM acquire_held_entries WHERE drop_item_id IS NOT NULL;
CREATE TEMP TABLE kept_drop_files AS
SELECT f.* FROM acquire_held_files f
WHERE f.entry_id IN (SELECT id FROM kept_drop_entries);
CREATE TEMP TABLE kept_drop_items AS SELECT * FROM drop_import_items;

DROP TABLE drop_import_items;

CREATE TABLE drop_import_jobs_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    user_name TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL
        CHECK(status IN ('processing','completed','failed')),
    created_at INTEGER NOT NULL,
    upload_name TEXT NOT NULL,
    staging_dir TEXT NOT NULL,
    error TEXT
) STRICT;
INSERT INTO drop_import_jobs_new (id, user_id, user_name, status, created_at, upload_name,
    staging_dir, error)
SELECT id, user_id, user_name, status,
    COALESCE(CAST(round(CASE WHEN typeof(created_at) IN ('integer', 'real')
        THEN created_at * 1000.0
        ELSE (julianday(created_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    upload_name, staging_dir, error
FROM drop_import_jobs;
DROP TABLE drop_import_jobs;
ALTER TABLE drop_import_jobs_new RENAME TO drop_import_jobs;
CREATE INDEX idx_drop_import_jobs_user
    ON drop_import_jobs(user_id, created_at);

CREATE TABLE drop_import_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL
        REFERENCES drop_import_jobs(id) ON DELETE CASCADE,
    folder_name TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK(status IN ('processing','imported','skipped',
                         'needs_review','failed','discarded')),
    release_group_mbid TEXT,
    album_title TEXT,
    artist_name TEXT,
    files_total INTEGER NOT NULL DEFAULT 0,
    files_imported INTEGER NOT NULL DEFAULT 0,
    detail TEXT,
    staging_paths TEXT NOT NULL DEFAULT '[]',
    updated_at INTEGER NOT NULL,
    reason_code TEXT
) STRICT;
INSERT INTO drop_import_items (id, job_id, folder_name, status, release_group_mbid,
    album_title, artist_name, files_total, files_imported, detail, staging_paths, updated_at,
    reason_code)
SELECT id, job_id, folder_name, status, release_group_mbid, album_title, artist_name,
    COALESCE(CASE WHEN typeof(files_total) IN ('integer', 'real')
        THEN CAST(files_total AS INTEGER) END, 0),
    COALESCE(CASE WHEN typeof(files_imported) IN ('integer', 'real')
        THEN CAST(files_imported AS INTEGER) END, 0),
    detail, staging_paths,
    COALESCE(CAST(round(CASE WHEN typeof(updated_at) IN ('integer', 'real')
        THEN updated_at * 1000.0
        ELSE (julianday(updated_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    reason_code
FROM kept_drop_items
WHERE job_id IN (SELECT id FROM drop_import_jobs);
CREATE INDEX idx_drop_import_items_job
    ON drop_import_items(job_id);

INSERT INTO acquire_held_entries
SELECT * FROM kept_drop_entries
WHERE id NOT IN (SELECT id FROM acquire_held_entries)
  AND drop_item_id IN (SELECT id FROM drop_import_items);
INSERT INTO acquire_held_files
SELECT * FROM kept_drop_files
WHERE id NOT IN (SELECT id FROM acquire_held_files)
  AND entry_id IN (SELECT id FROM acquire_held_entries);
DROP TABLE kept_drop_files;
DROP TABLE kept_drop_entries;
DROP TABLE kept_drop_items;

-- Search jobs.
CREATE TABLE search_jobs_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    year INTEGER,
    track_count INTEGER,
    release_group_mbid TEXT,
    artist_mbid TEXT,
    search_query TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'searching'
        CHECK(status IN ('searching','matched','completed','failed','cancelled')),
    candidates_blob TEXT NOT NULL DEFAULT '[]',
    error_message TEXT,
    quality_snapshot_json TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    updated_at INTEGER NOT NULL
) STRICT;
INSERT INTO search_jobs_new (id, user_id, artist_name, album_title, year, track_count,
    release_group_mbid, artist_mbid, search_query, status, candidates_blob, error_message,
    quality_snapshot_json, created_at, completed_at, updated_at)
SELECT id, user_id, artist_name, album_title,
    CASE WHEN typeof(year) IN ('integer', 'real') THEN CAST(year AS INTEGER) END,
    CASE WHEN typeof(track_count) IN ('integer', 'real') THEN CAST(track_count AS INTEGER) END,
    release_group_mbid, artist_mbid, search_query, status, candidates_blob, error_message,
    quality_snapshot_json,
    COALESCE(
        CAST(round(CASE WHEN typeof(created_at) IN ('integer', 'real')
            THEN created_at * 1000.0
            ELSE (julianday(created_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round(CASE WHEN typeof(updated_at) IN ('integer', 'real')
            THEN updated_at * 1000.0
            ELSE (julianday(updated_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    CAST(round(CASE WHEN typeof(completed_at) IN ('integer', 'real')
        THEN completed_at * 1000.0
        ELSE (julianday(completed_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
    COALESCE(
        CAST(round(CASE WHEN typeof(updated_at) IN ('integer', 'real')
            THEN updated_at * 1000.0
            ELSE (julianday(updated_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round(CASE WHEN typeof(created_at) IN ('integer', 'real')
            THEN created_at * 1000.0
            ELSE (julianday(created_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER))
FROM search_jobs
WHERE EXISTS (SELECT 1 FROM auth_users u WHERE u.id = search_jobs.user_id);
DROP TABLE search_jobs;
ALTER TABLE search_jobs_new RENAME TO search_jobs;
CREATE INDEX idx_search_jobs_user ON search_jobs(user_id);
CREATE INDEX idx_search_jobs_status ON search_jobs(status);
CREATE INDEX idx_search_jobs_rgmbid ON search_jobs(release_group_mbid);

-- Spotify authorize states.
CREATE TABLE spotify_auth_requests_new (
    state        TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    redirect_uri TEXT NOT NULL,
    expires_at   INTEGER NOT NULL
) STRICT;
INSERT INTO spotify_auth_requests_new (state, user_id, redirect_uri, expires_at)
SELECT state, user_id, redirect_uri,
    CAST(round(CASE WHEN typeof(expires_at) IN ('integer', 'real')
        THEN expires_at * 1000.0 ELSE 0 END) AS INTEGER)
FROM spotify_auth_requests
WHERE user_id IN (SELECT id FROM auth_users);
DROP TABLE spotify_auth_requests;
ALTER TABLE spotify_auth_requests_new RENAME TO spotify_auth_requests;
CREATE INDEX idx_spotify_auth_requests_expires
    ON spotify_auth_requests(expires_at);
CREATE INDEX idx_spotify_auth_requests_user_id
    ON spotify_auth_requests(user_id);

-- In-flight edition acquires.
CREATE TABLE acquire_edition_acquires_new (
    release_group_mbid_lower TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES download_tasks(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL
) STRICT;
INSERT INTO acquire_edition_acquires_new (release_group_mbid_lower, task_id, started_at)
SELECT release_group_mbid_lower, task_id,
    CAST(round(CASE WHEN typeof(started_at) IN ('integer', 'real')
        THEN started_at * 1000.0 ELSE 0 END) AS INTEGER)
FROM acquire_edition_acquires
WHERE task_id IN (SELECT id FROM download_tasks);
DROP TABLE acquire_edition_acquires;
ALTER TABLE acquire_edition_acquires_new RENAME TO acquire_edition_acquires;
CREATE INDEX idx_acquire_edition_acquires_task ON acquire_edition_acquires(task_id);

-- Personal-mix approvals.
CREATE TABLE personal_mix_approvals_new (
    user_id TEXT NOT NULL PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK(state IN ('pending','approved','rejected','revoked')),
    requested_at INTEGER NOT NULL,
    reviewed_by_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    reviewed_by_name TEXT,
    reviewed_at INTEGER
) STRICT;
INSERT INTO personal_mix_approvals_new (user_id, state, requested_at, reviewed_by_id,
    reviewed_by_name, reviewed_at)
SELECT user_id,
    CASE WHEN state IN ('pending', 'approved', 'rejected', 'revoked') THEN state
         ELSE 'pending' END,
    COALESCE(CAST(round(CASE WHEN typeof(requested_at) IN ('integer', 'real')
        THEN requested_at * 1000.0
        ELSE (julianday(requested_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    CASE WHEN EXISTS (SELECT 1 FROM auth_users u
                      WHERE u.id = personal_mix_approvals.reviewed_by_id)
        THEN reviewed_by_id END,
    reviewed_by_name,
    CAST(round(CASE WHEN typeof(reviewed_at) IN ('integer', 'real')
        THEN reviewed_at * 1000.0
        ELSE (julianday(reviewed_at) - 2440587.5) * 86400000.0 END) AS INTEGER)
FROM personal_mix_approvals
WHERE EXISTS (SELECT 1 FROM auth_users u WHERE u.id = personal_mix_approvals.user_id);
DROP TABLE personal_mix_approvals;
ALTER TABLE personal_mix_approvals_new RENAME TO personal_mix_approvals;
CREATE INDEX idx_pma_pending
    ON personal_mix_approvals(state) WHERE state = 'pending';
CREATE INDEX idx_pma_reviewed_by ON personal_mix_approvals(reviewed_by_id);

-- Follow cursors and the release groups they know.
CREATE TABLE acquire_follow_cursors_new (
    artist_mbid_lower TEXT PRIMARY KEY,
    baselined INTEGER NOT NULL DEFAULT 0 CHECK(baselined IN (0,1)),
    cursor_date TEXT,
    next_poll_at INTEGER NOT NULL,
    pending TEXT NOT NULL DEFAULT '[]'
) STRICT;
INSERT INTO acquire_follow_cursors_new (artist_mbid_lower, baselined, cursor_date,
    next_poll_at, pending)
SELECT artist_mbid_lower, baselined, cursor_date,
    CAST(round(CASE WHEN typeof(next_poll_at) IN ('integer', 'real')
        THEN next_poll_at * 1000.0 ELSE 0 END) AS INTEGER),
    pending
FROM acquire_follow_cursors;
DROP TABLE acquire_follow_cursors;
ALTER TABLE acquire_follow_cursors_new RENAME TO acquire_follow_cursors;
CREATE INDEX idx_acquire_follow_cursors_due
    ON acquire_follow_cursors(next_poll_at);

CREATE TABLE artist_known_releases_new (
    artist_mbid_lower TEXT NOT NULL,
    rg_mbid_lower TEXT NOT NULL,
    auto_policy_revision INTEGER,
    PRIMARY KEY (artist_mbid_lower, rg_mbid_lower)
) STRICT;
INSERT INTO artist_known_releases_new (artist_mbid_lower, rg_mbid_lower,
    auto_policy_revision)
SELECT artist_mbid_lower, rg_mbid_lower,
    CASE WHEN typeof(auto_policy_revision) IN ('integer', 'real')
        THEN CAST(auto_policy_revision AS INTEGER) END
FROM artist_known_releases;
DROP TABLE artist_known_releases;
ALTER TABLE artist_known_releases_new RENAME TO artist_known_releases;
CREATE INDEX idx_akr_auto_policy
    ON artist_known_releases(artist_mbid_lower, auto_policy_revision)
    WHERE auto_policy_revision IS NOT NULL;

-- Quotas.
CREATE TABLE user_quotas_new (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    request_quota_count INTEGER,
    request_quota_days INTEGER,
    storage_quota_gb INTEGER
) STRICT;
INSERT INTO user_quotas_new (user_id, request_quota_count, request_quota_days,
    storage_quota_gb)
SELECT user_id,
    CASE WHEN typeof(request_quota_count) IN ('integer', 'real')
        THEN CAST(request_quota_count AS INTEGER) END,
    CASE WHEN typeof(request_quota_days) IN ('integer', 'real')
        THEN CAST(request_quota_days AS INTEGER) END,
    CASE WHEN typeof(storage_quota_gb) IN ('integer', 'real')
        THEN CAST(storage_quota_gb AS INTEGER) END
FROM user_quotas
WHERE EXISTS (SELECT 1 FROM auth_users u WHERE u.id = user_quotas.user_id);
DROP TABLE user_quotas;
ALTER TABLE user_quotas_new RENAME TO user_quotas;

-- Candidates a wanted watch has seen.
CREATE TABLE wanted_seen_candidates_new (
    release_group_mbid_lower TEXT NOT NULL
        REFERENCES wanted_watches(release_group_mbid_lower) ON DELETE CASCADE,
    source TEXT NOT NULL,
    identity TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL,
    PRIMARY KEY (release_group_mbid_lower, source, identity)
) STRICT;
INSERT INTO wanted_seen_candidates_new (release_group_mbid_lower, source, identity,
    first_seen_at)
SELECT release_group_mbid_lower, source, identity,
    COALESCE(CAST(round(CASE WHEN typeof(first_seen_at) IN ('integer', 'real')
        THEN first_seen_at * 1000.0
        ELSE (julianday(first_seen_at) - 2440587.5) * 86400000.0 END) AS INTEGER),
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER))
FROM wanted_seen_candidates
WHERE release_group_mbid_lower IN (SELECT release_group_mbid_lower FROM wanted_watches);
DROP TABLE wanted_seen_candidates;
ALTER TABLE wanted_seen_candidates_new RENAME TO wanted_seen_candidates;

-- The activity triggers, as they were.
CREATE TRIGGER download_activity_task_insert
AFTER INSERT ON download_tasks
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_landed
AFTER UPDATE OF release_group_mbid, completed_at, updated_at ON download_tasks
WHEN NEW.status IN ('completed', 'partial')
    AND OLD.status IN ('completed', 'partial')
    AND (OLD.release_group_mbid IS NOT NEW.release_group_mbid
         OR COALESCE(OLD.completed_at, OLD.updated_at)
            IS NOT COALESCE(NEW.completed_at, NEW.updated_at))
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_status
AFTER UPDATE OF status ON download_tasks
WHEN OLD.status IS NOT NEW.status
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_search_link
AFTER UPDATE OF search_job_id, candidate_index ON download_tasks
WHEN OLD.search_job_id IS NOT NEW.search_job_id
    OR OLD.candidate_index IS NOT NEW.candidate_index
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_owner
AFTER UPDATE OF user_id ON download_tasks
WHEN OLD.user_id IS NOT NEW.user_id
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_delete
AFTER DELETE ON download_tasks
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

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

CREATE TRIGGER download_activity_held_entry_insert
AFTER INSERT ON acquire_held_entries
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_entry_state
AFTER UPDATE OF state ON acquire_held_entries
WHEN OLD.state IS NOT NEW.state
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_entry_delete
AFTER DELETE ON acquire_held_entries
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_entry_retry
AFTER UPDATE OF retry_at ON acquire_held_entries
WHEN OLD.retry_at IS NOT NEW.retry_at
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_file_insert
AFTER INSERT ON acquire_held_files
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM acquire_held_entries WHERE id = NEW.entry_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_file_state
AFTER UPDATE OF state, reason ON acquire_held_files
WHEN OLD.state IS NOT NEW.state OR OLD.reason IS NOT NEW.reason
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM acquire_held_entries WHERE id = NEW.entry_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

-- Put the id counters back, never below what the copied rows reached.
UPDATE sqlite_sequence
SET seq = max(seq, (SELECT k.seq FROM temp.acquire_kept_sequences k
                    WHERE k.name = sqlite_sequence.name))
WHERE name IN (SELECT name FROM temp.acquire_kept_sequences);
INSERT INTO sqlite_sequence (name, seq)
SELECT k.name, k.seq FROM temp.acquire_kept_sequences k
WHERE k.name NOT IN (SELECT name FROM sqlite_sequence);
DROP TABLE temp.acquire_kept_sequences;

-- Migration high-water mark.
PRAGMA user_version = 96;
