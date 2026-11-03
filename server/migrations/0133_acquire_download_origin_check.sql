-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0133_acquire_download_origin_check.sql - a download's origin lists the
-- dispatch origins.
--
-- download_tasks is rebuilt with its origin checked against user, approval,
-- retry, wanted, upgrade, edition and follow, rowids, indexes and triggers
-- carried. It is the parent of the idempotency keys, the import decisions and
-- the edition marks, so the file follows the rebuild recipe in
-- kernel/src/schema/boot.rs. An origin written before the rename,
-- edition_conversion, becomes edition; any other value outside the list
-- becomes user, the column's default, which counts against the quota as a
-- fresh ask does (the v2 importer maps an origin the same way). Every other
-- column is copied as it is.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE download_origin_133_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_133 INTEGER CONSTRAINT guard_stamp_below_133 CHECK (stamp_below_133 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO download_origin_133_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO download_origin_133_guard (stamp_below_133)
    SELECT (SELECT user_version FROM pragma_user_version) < 133;

CREATE TEMP TABLE download_origin_133_orphans AS
    SELECT "table" AS child, rowid AS row, parent FROM pragma_foreign_key_check;
CREATE INDEX temp.download_origin_133_orphans_at
    ON download_origin_133_orphans(child, row, parent);

-- The attempts' activity triggers name the table; its own go with it.
DROP TRIGGER download_activity_attempt_insert;
DROP TRIGGER download_activity_attempt_state;
DROP TRIGGER download_activity_attempt_delete;

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
    origin TEXT NOT NULL DEFAULT 'user'
        CHECK(origin IN ('user','approval','retry','wanted','upgrade','edition','follow')),
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
SELECT rowid, id, user_id, request_history_mbid, download_type,
    release_group_mbid, release_mbid, release_track_mbid, recording_mbid, artist_mbid,
    artist_name, album_title, track_title, track_number, disc_number, year, track_count,
    track_duration_seconds, download_client, source,
    CASE WHEN origin IN ('user', 'approval', 'retry', 'wanted', 'upgrade', 'edition', 'follow')
            THEN origin
        WHEN origin = 'edition_conversion' THEN 'edition'
        ELSE 'user' END,
    source_username,
    source_directory, search_query, search_job_id, candidate_index, status, preflight_score,
    progress_percent, total_size_bytes, downloaded_bytes, files_total, files_completed,
    files_failed, quality_format, quality_bitrate, quality_sample_rate, quality_bit_depth,
    advertised_queue_depth, queue_position_start, queue_position_end, remote_queued,
    preferred_quality_fallback_at, quality_pool_key, attempt_number, attempt_total,
    has_next_source, quality_snapshot_json, quality_preference_step, quality_certainty,
    quality_provenance, manual_quality_override, staging_path, final_path, error_message,
    retry_count, last_polled_at, created_at, started_at, completed_at, cancelled_at,
    updated_at, wrong_product_verdict_at, wrong_product_detail, outcome_code, plan_json,
    picked_by_person
FROM download_tasks;
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

INSERT INTO download_origin_133_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3) AS now
    WHERE now.n > (SELECT COUNT(*) FROM download_origin_133_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent));

DROP TABLE download_origin_133_guard;
DROP TABLE download_origin_133_orphans;

-- Migration high-water mark.
PRAGMA user_version = 133;
COMMIT;
PRAGMA foreign_keys=ON;
