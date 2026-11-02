-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0115_library_jobs_management_publish_conventions.sql - the library's job,
-- management and publish tables follow the schema conventions.
--
-- Each table is rebuilt as a STRICT table, rowids, indexes and triggers
-- carried (the rebuild recipe in kernel/src/schema/boot.rs). Jobs and
-- publish rows are parents of tables in the library, so the file runs
-- outside sqlx's transaction with foreign keys off, and refuses, changing
-- nothing, when the copies leave any child pointing at a missing parent that
-- did not before.
--
-- What each row becomes:
--
--   * a time in REAL seconds becomes integer milliseconds, rounded. A value
--     that is no number reads as 0, so it has passed;
--   * a *_day column becomes the *_at column of that day's first millisecond,
--     so a comparison by day answers as before. A day too far off to fit is
--     held at the limit, and an expiry that is no number never expires, as it
--     read before, so no copy is purged early;
--   * library_publish_operations keeps created_at where it has one; a row
--     from before that column gets its created_day;
--   * library_management_automatic_queue.not_before becomes not_before_at;
--   * library_publish_journal.state and kind are checked against their enums.
--     A row of another kind or state cannot be read or acted on, so it
--     becomes needs_attention, which touches no file and shows it to an admin,
--     and a kind outside the list reads as sidecar;
--   * library_publish_journal.catalog_revision goes: nothing reads it;
--   * library_management_exclusions.excluded_by_user_id is nullable and set
--     to null with its user, so that user can be deleted and the album stays
--     excluded;
--   * a job's snapshot cascades from its job and its link to another job is
--     set to null. Nothing deletes job rows today;
--   * library_publish_resolutions.resolved_by_user_id and
--     library_management_baseline_purges.actor_user_id gain a set-null key,
--     with an index. An id of no account becomes NULL.
--
-- Rows with a key of the wrong type (a NULL or BLOB text key, a non-integer
-- ordinal) cannot be named by any read, and are not copied. Rows that point
-- at one with a BLOB go with it, or lose the pointer where it is optional, so
-- no such row can refuse the upgrade.
--
-- library_publish_created_folders and library_publish_outputs already follow
-- the conventions and stay as they are.
--
-- Not keyed, on purpose: journal, snapshot and baseline track ids, which
-- outlive their track; journal and snapshot bundle ids, which may name a
-- removal's record rather than an operation; blob hashes, whose bytes may
-- have moved to the undo folder; the media refresh job ids, which the
-- kernel's queue prunes.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE library_rebuild_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_115 INTEGER CONSTRAINT guard_stamp_below_115 CHECK (stamp_below_115 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO library_rebuild_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO library_rebuild_guard (stamp_below_115)
    SELECT (SELECT user_version FROM pragma_user_version) < 115;

CREATE TEMP TABLE library_rebuild_orphans AS
    SELECT "table" AS child, rowid AS row, parent, fkid FROM pragma_foreign_key_check;

-- Children outside the rebuilt tables of parents that are not copied.
DELETE FROM library_reidentification_snapshots WHERE typeof(job_id) = 'blob';
DELETE FROM library_repair_snapshots WHERE typeof(job_id) = 'blob';
DELETE FROM library_repair_materialization WHERE typeof(job_id) = 'blob';
UPDATE library_track_management_state SET last_operation_job_id = NULL
    WHERE typeof(last_operation_job_id) = 'blob';
UPDATE library_track_management_state SET baseline_id = NULL
    WHERE typeof(baseline_id) = 'blob';
UPDATE library_catalog_actions SET operation_job_id = NULL
    WHERE typeof(operation_job_id) = 'blob';
UPDATE library_automatic_edition_undo SET job_id = NULL WHERE typeof(job_id) = 'blob';

-- Everything that names a rebuilt table goes first: a rename re-reads every
-- trigger and view, and a dropped table would fail it. All come back below,
-- word for word.
DROP TRIGGER trg_library_publish_journal_fence_insert;
DROP TRIGGER trg_library_publish_journal_fence_state;
DROP TRIGGER trg_library_publish_outputs_landed;
DROP TRIGGER trg_library_publish_resolution_fence;
DROP TRIGGER trg_library_publish_outputs_refresh;
DROP VIEW library_publish_outputs_rule;

CREATE TABLE library_operation_jobs_new (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('bulk_review_apply','repair','explicit_reidentification','library_management')),
    state TEXT NOT NULL CHECK(state IN ('queued','running','paused','ready','succeeded','failed','cancelled','stopped')),
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    input_catalog_revision INTEGER CHECK(input_catalog_revision BETWEEN 0 AND 9223372036854775807),
    expected_work_count INTEGER NOT NULL DEFAULT 0 CHECK(expected_work_count >= 0),
    completed_count INTEGER NOT NULL DEFAULT 0 CHECK(completed_count >= 0),
    succeeded_count INTEGER NOT NULL DEFAULT 0 CHECK(succeeded_count >= 0),
    failed_count INTEGER NOT NULL DEFAULT 0 CHECK(failed_count >= 0),
    skipped_count INTEGER NOT NULL DEFAULT 0 CHECK(skipped_count >= 0),
    control_request TEXT NOT NULL DEFAULT 'none' CHECK(control_request IN ('none','pause','stop')),
    terminal_code TEXT,
    idempotency_key TEXT UNIQUE,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    heartbeat_at INTEGER,
    next_attempt_at INTEGER,
    reidentification_attempt_count INTEGER NOT NULL DEFAULT 0
        CHECK(reidentification_attempt_count >= 0),
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    phase_started_at INTEGER,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL,
    terminal_at INTEGER,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    event_revision INTEGER NOT NULL DEFAULT 0 CHECK(event_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;
INSERT INTO library_operation_jobs_new (rowid, id, kind, state, requested_by_user_id,
    input_catalog_revision, expected_work_count, completed_count, succeeded_count, failed_count,
    skipped_count, control_request, terminal_code, idempotency_key, lease_owner, lease_expires_at,
    heartbeat_at, next_attempt_at, reidentification_attempt_count, created_at, started_at,
    phase_started_at, phase_timings_json, updated_at, terminal_at, row_revision, event_revision)
SELECT rowid, id, kind, state, CAST(requested_by_user_id AS TEXT),
    CAST(input_catalog_revision AS INTEGER), CAST(expected_work_count AS INTEGER),
    CAST(completed_count AS INTEGER), CAST(succeeded_count AS INTEGER),
    CAST(failed_count AS INTEGER), CAST(skipped_count AS INTEGER), control_request,
    CAST(terminal_code AS TEXT), CAST(idempotency_key AS TEXT), CAST(lease_owner AS TEXT),
    CAST(round(lease_expires_at * 1000) AS INTEGER), CAST(round(heartbeat_at * 1000) AS INTEGER),
    CAST(round(next_attempt_at * 1000) AS INTEGER), CAST(reidentification_attempt_count AS INTEGER),
    CAST(round(created_at * 1000) AS INTEGER), CAST(round(started_at * 1000) AS INTEGER),
    CAST(round(phase_started_at * 1000) AS INTEGER), CAST(phase_timings_json AS TEXT),
    CAST(round(updated_at * 1000) AS INTEGER), CAST(round(terminal_at * 1000) AS INTEGER),
    CAST(row_revision AS INTEGER), CAST(event_revision AS INTEGER)
FROM library_operation_jobs WHERE typeof(id) = 'text';
DROP TABLE library_operation_jobs;
ALTER TABLE library_operation_jobs_new RENAME TO library_operation_jobs;
CREATE INDEX idx_operation_jobs_claim ON library_operation_jobs(state, created_at);
CREATE INDEX idx_operation_jobs_lease ON library_operation_jobs(state, lease_expires_at);
CREATE INDEX idx_library_operation_jobs_requested_by_user_id
    ON library_operation_jobs(requested_by_user_id);

CREATE TABLE library_operation_work_new (
    job_id TEXT NOT NULL REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    expected_subject_revision INTEGER NOT NULL,
    expected_input_revision TEXT NOT NULL,
    action TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','running','succeeded','failed','skipped')),
    checkpoint_json TEXT,
    result_json TEXT,
    failure_code TEXT,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    bundle_id TEXT,
    PRIMARY KEY(job_id, ordinal),
    UNIQUE(job_id, idempotency_key),
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
) STRICT;
INSERT INTO library_operation_work_new (rowid, job_id, ordinal, local_album_id, local_track_id,
    expected_subject_revision, expected_input_revision, action, idempotency_key, state,
    checkpoint_json, result_json, failure_code, updated_at, row_revision, bundle_id)
SELECT rowid, CAST(job_id AS TEXT), ordinal, CAST(local_album_id AS TEXT),
    CAST(local_track_id AS TEXT), CAST(expected_subject_revision AS INTEGER),
    CAST(expected_input_revision AS TEXT), CAST(action AS TEXT), CAST(idempotency_key AS TEXT),
    state, CAST(checkpoint_json AS TEXT), CAST(result_json AS TEXT), CAST(failure_code AS TEXT),
    CAST(round(updated_at * 1000) AS INTEGER), CAST(row_revision AS INTEGER),
    CAST(bundle_id AS TEXT)
FROM library_operation_work WHERE typeof(ordinal) = 'integer' AND typeof(job_id) = 'text'
    AND typeof(local_album_id) <> 'blob' AND typeof(local_track_id) <> 'blob';
DROP TABLE library_operation_work;
ALTER TABLE library_operation_work_new RENAME TO library_operation_work;
CREATE INDEX idx_operation_work_claim ON library_operation_work(job_id, state, ordinal);
CREATE INDEX idx_library_operation_work_local_track_id
    ON library_operation_work(local_track_id);
CREATE INDEX idx_library_operation_work_local_album_id
    ON library_operation_work(local_album_id);
CREATE INDEX idx_library_operation_work_bundle_id
    ON library_operation_work(bundle_id) WHERE bundle_id IS NOT NULL;

CREATE TABLE library_operation_control_idempotency_new (
    idempotency_key TEXT PRIMARY KEY CHECK(length(trim(idempotency_key)) > 0),
    job_id TEXT NOT NULL REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    control TEXT NOT NULL CHECK(control IN ('pause','resume','stop')),
    requested_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_operation_control_idempotency_new (rowid, idempotency_key, job_id, control,
    requested_at)
SELECT rowid, idempotency_key, CAST(job_id AS TEXT), control,
    CAST(round(requested_at * 1000) AS INTEGER)
FROM library_operation_control_idempotency
WHERE typeof(idempotency_key) = 'text' AND typeof(job_id) = 'text';
DROP TABLE library_operation_control_idempotency;
ALTER TABLE library_operation_control_idempotency_new
    RENAME TO library_operation_control_idempotency;
CREATE INDEX idx_library_operation_control_idempotency_job_id
    ON library_operation_control_idempotency(job_id);

CREATE TABLE library_work_control_new (
    queue_kind TEXT PRIMARY KEY CHECK(queue_kind = 'identification'),
    state TEXT NOT NULL CHECK(state IN ('running','paused')),
    requested_at INTEGER,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    high_priority_claim_count INTEGER NOT NULL DEFAULT 0 CHECK(high_priority_claim_count >= 0),
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;
INSERT INTO library_work_control_new (rowid, queue_kind, state, requested_at,
    requested_by_user_id, high_priority_claim_count, row_revision)
SELECT rowid, queue_kind, state, CAST(round(requested_at * 1000) AS INTEGER),
    CAST(requested_by_user_id AS TEXT), CAST(high_priority_claim_count AS INTEGER),
    CAST(row_revision AS INTEGER)
FROM library_work_control WHERE typeof(queue_kind) = 'text';
DROP TABLE library_work_control;
ALTER TABLE library_work_control_new RENAME TO library_work_control;
CREATE INDEX idx_library_work_control_requested_by_user_id
    ON library_work_control(requested_by_user_id);

CREATE TABLE library_management_automatic_queue_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    queued_at INTEGER NOT NULL,
    not_before_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    last_reason_code TEXT,
    retag_followed_at INTEGER
) STRICT;
INSERT INTO library_management_automatic_queue_new (rowid, local_album_id, queued_at,
    not_before_at, attempts, last_reason_code, retag_followed_at)
SELECT rowid, local_album_id, CAST(round(queued_at * 1000) AS INTEGER),
    CAST(round(not_before * 1000) AS INTEGER), CAST(attempts AS INTEGER),
    CAST(last_reason_code AS TEXT), CAST(retag_followed_at AS INTEGER)
FROM library_management_automatic_queue WHERE typeof(local_album_id) = 'text';
DROP TABLE library_management_automatic_queue;
ALTER TABLE library_management_automatic_queue_new RENAME TO library_management_automatic_queue;
CREATE INDEX idx_library_management_automatic_queue_due
    ON library_management_automatic_queue(not_before_at);

CREATE TABLE library_management_baseline_purges_new (
    idempotency_key TEXT PRIMARY KEY CHECK(length(trim(idempotency_key)) > 0),
    impact_token TEXT NOT NULL CHECK(length(trim(impact_token)) > 0),
    actor_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    purged_baseline_count INTEGER NOT NULL CHECK(purged_baseline_count >= 0),
    detached_reference_count INTEGER NOT NULL CHECK(detached_reference_count >= 0),
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_management_baseline_purges_new (rowid, idempotency_key, impact_token,
    actor_user_id, purged_baseline_count, detached_reference_count, created_at)
SELECT rowid, idempotency_key, CAST(impact_token AS TEXT),
    (SELECT u.id FROM auth_users u WHERE u.id = actor_user_id),
    CAST(purged_baseline_count AS INTEGER), CAST(detached_reference_count AS INTEGER),
    CAST(round(created_at * 1000) AS INTEGER)
FROM library_management_baseline_purges WHERE typeof(idempotency_key) = 'text';
DROP TABLE library_management_baseline_purges;
ALTER TABLE library_management_baseline_purges_new RENAME TO library_management_baseline_purges;
CREATE INDEX idx_library_management_baseline_purges_actor_user_id
    ON library_management_baseline_purges(actor_user_id);

-- v2's ledger of the blobs baselines name, and its baselines: the import
-- writes them, and they are kept byte for byte.
CREATE TABLE library_management_blobs_new (
    sha256 TEXT PRIMARY KEY
        CHECK(length(sha256) = 64 AND sha256 = lower(sha256)
              AND sha256 NOT GLOB '*[^0-9a-f]*'),
    kind TEXT NOT NULL CHECK(kind IN (
        'tag_snapshot','image','sidecar_manifest','metadata_document'
    )),
    byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
    relative_path TEXT NOT NULL CHECK(length(trim(relative_path)) > 0),
    media_metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;
INSERT INTO library_management_blobs_new (rowid, sha256, kind, byte_length, relative_path,
    media_metadata_json, created_at, row_revision)
SELECT rowid, sha256, kind, CAST(byte_length AS INTEGER), CAST(relative_path AS TEXT),
    CAST(media_metadata_json AS TEXT), CAST(round(created_at * 1000) AS INTEGER),
    CAST(row_revision AS INTEGER)
FROM library_management_blobs WHERE typeof(sha256) = 'text';
DROP TABLE library_management_blobs;
ALTER TABLE library_management_blobs_new RENAME TO library_management_blobs;

CREATE TABLE library_management_baselines_new (
    id TEXT PRIMARY KEY,
    local_track_id TEXT NOT NULL UNIQUE REFERENCES local_tracks(id) ON DELETE RESTRICT,
    original_root_id TEXT NOT NULL CHECK(length(trim(original_root_id)) > 0),
    original_relative_path TEXT NOT NULL CHECK(length(trim(original_relative_path)) > 0),
    format TEXT NOT NULL CHECK(length(trim(format)) > 0),
    adapter_version TEXT NOT NULL CHECK(length(trim(adapter_version)) > 0),
    semantic_snapshot_blob_sha256 TEXT NOT NULL
        REFERENCES library_management_blobs(sha256) ON DELETE RESTRICT,
    image_snapshot_json TEXT NOT NULL DEFAULT '[]',
    ancillary_snapshot_json TEXT NOT NULL DEFAULT '[]',
    file_mtime_ns INTEGER,
    file_mode INTEGER,
    stat_revision TEXT NOT NULL,
    tag_revision TEXT NOT NULL,
    identity_revision INTEGER
        CHECK(identity_revision IS NULL OR identity_revision BETWEEN 1 AND 9223372036854775807),
    created_at INTEGER NOT NULL,
    restore_status TEXT NOT NULL DEFAULT 'available' CHECK(restore_status IN (
        'available','restoring','restored','stale','purged'
    )),
    last_verified_at INTEGER,
    catalog_document_json TEXT,
    catalog_document_hash TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;
INSERT INTO library_management_baselines_new (rowid, id, local_track_id, original_root_id,
    original_relative_path, format, adapter_version, semantic_snapshot_blob_sha256,
    image_snapshot_json, ancillary_snapshot_json, file_mtime_ns, file_mode, stat_revision,
    tag_revision, identity_revision, created_at, restore_status, last_verified_at,
    catalog_document_json, catalog_document_hash, row_revision)
SELECT rowid, id, CAST(local_track_id AS TEXT), CAST(original_root_id AS TEXT),
    CAST(original_relative_path AS TEXT), CAST(format AS TEXT), CAST(adapter_version AS TEXT),
    CAST(semantic_snapshot_blob_sha256 AS TEXT), CAST(image_snapshot_json AS TEXT),
    CAST(ancillary_snapshot_json AS TEXT), CAST(file_mtime_ns AS INTEGER),
    CAST(file_mode AS INTEGER), CAST(stat_revision AS TEXT), CAST(tag_revision AS TEXT),
    CAST(identity_revision AS INTEGER), CAST(round(created_at * 1000) AS INTEGER), restore_status,
    CAST(round(last_verified_at * 1000) AS INTEGER), CAST(catalog_document_json AS TEXT),
    CAST(catalog_document_hash AS TEXT), CAST(row_revision AS INTEGER)
FROM library_management_baselines WHERE typeof(id) = 'text'
    AND typeof(local_track_id) = 'text' AND typeof(semantic_snapshot_blob_sha256) = 'text';
DROP TABLE library_management_baselines;
ALTER TABLE library_management_baselines_new RENAME TO library_management_baselines;
CREATE INDEX idx_management_baseline_track
ON library_management_baselines(local_track_id);
CREATE INDEX idx_library_management_baselines_semantic_snapshot_blob_sha256
    ON library_management_baselines(semantic_snapshot_blob_sha256);

CREATE TABLE library_management_exclusions_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    reason TEXT NOT NULL,
    excluded_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    excluded_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision > 0)
) STRICT;
INSERT INTO library_management_exclusions_new (rowid, local_album_id, reason, excluded_by_user_id,
    excluded_at, row_revision)
SELECT rowid, local_album_id, CAST(reason AS TEXT),
    (SELECT u.id FROM auth_users u WHERE u.id = excluded_by_user_id),
    CAST(round(excluded_at * 1000) AS INTEGER), CAST(row_revision AS INTEGER)
FROM library_management_exclusions WHERE typeof(local_album_id) = 'text';
DROP TABLE library_management_exclusions;
ALTER TABLE library_management_exclusions_new RENAME TO library_management_exclusions;
CREATE INDEX idx_library_management_exclusions_excluded_by_user_id
    ON library_management_exclusions(excluded_by_user_id);

CREATE TABLE library_management_job_snapshots_new (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK(mode IN (
        'preview','apply','automatic_apply','undo','baseline_restore','duplicate_resolution'
    )),
    origin TEXT NOT NULL CHECK(origin IN (
        'manual','acquisition','drop_import','scan_discovered'
    )),
    phase TEXT NOT NULL CHECK(phase IN (
        'planning','ready','applying','undoing','restoring','complete'
    )),
    selection_json TEXT NOT NULL,
    profile_revision TEXT NOT NULL,
    settings_revision TEXT NOT NULL,
    proposed_settings_revision TEXT,
    naming_revision TEXT NOT NULL,
    policy_revision TEXT NOT NULL,
    catalog_revision INTEGER NOT NULL
        CHECK(catalog_revision BETWEEN 0 AND 9223372036854775807),
    profile_snapshot_json TEXT NOT NULL,
    preview_token_hash TEXT,
    preview_created_at INTEGER,
    preview_expires_at INTEGER,
    apply_idempotency_key TEXT,
    target_root_id TEXT,
    linked_operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE SET NULL,
    intent_json TEXT NOT NULL DEFAULT '{}',
    summary_json TEXT NOT NULL DEFAULT '{}',
    warnings_json TEXT NOT NULL DEFAULT '[]',
    staging_cursor TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    jellyfin_refresh_job_id INTEGER,
    plex_refresh_job_id INTEGER,
    navidrome_refresh_job_id INTEGER
) STRICT;
INSERT INTO library_management_job_snapshots_new (rowid, job_id, mode, origin, phase,
    selection_json, profile_revision, settings_revision, proposed_settings_revision,
    naming_revision, policy_revision, catalog_revision, profile_snapshot_json, preview_token_hash,
    preview_created_at, preview_expires_at, apply_idempotency_key, target_root_id,
    linked_operation_job_id, intent_json, summary_json, warnings_json, staging_cursor, created_at,
    updated_at, row_revision, jellyfin_refresh_job_id, plex_refresh_job_id,
    navidrome_refresh_job_id)
SELECT rowid, job_id, mode, origin, phase, CAST(selection_json AS TEXT),
    CAST(profile_revision AS TEXT), CAST(settings_revision AS TEXT),
    CAST(proposed_settings_revision AS TEXT), CAST(naming_revision AS TEXT),
    CAST(policy_revision AS TEXT), CAST(catalog_revision AS INTEGER),
    CAST(profile_snapshot_json AS TEXT), CAST(preview_token_hash AS TEXT),
    CAST(round(preview_created_at * 1000) AS INTEGER),
    CAST(round(preview_expires_at * 1000) AS INTEGER), CAST(apply_idempotency_key AS TEXT),
    CAST(target_root_id AS TEXT),
    CASE WHEN typeof(linked_operation_job_id) = 'text' THEN linked_operation_job_id END,
    CAST(intent_json AS TEXT), CAST(summary_json AS TEXT), CAST(warnings_json AS TEXT),
    CAST(staging_cursor AS TEXT), CAST(round(created_at * 1000) AS INTEGER),
    CAST(round(updated_at * 1000) AS INTEGER), CAST(row_revision AS INTEGER),
    CAST(jellyfin_refresh_job_id AS INTEGER), CAST(plex_refresh_job_id AS INTEGER),
    CAST(navidrome_refresh_job_id AS INTEGER)
FROM library_management_job_snapshots WHERE typeof(job_id) = 'text';
DROP TABLE library_management_job_snapshots;
ALTER TABLE library_management_job_snapshots_new RENAME TO library_management_job_snapshots;
CREATE UNIQUE INDEX idx_management_apply_idempotency
ON library_management_job_snapshots(apply_idempotency_key)
WHERE apply_idempotency_key IS NOT NULL;
CREATE INDEX idx_library_management_job_snapshots_linked_operation_job_id
    ON library_management_job_snapshots(linked_operation_job_id);

CREATE TABLE library_management_metadata_snapshots_new (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL CHECK(length(trim(provider)) > 0),
    entity_kind TEXT NOT NULL CHECK(length(trim(entity_kind)) > 0),
    entity_id TEXT NOT NULL CHECK(length(trim(entity_id)) > 0),
    input_hash TEXT NOT NULL
        CHECK(length(input_hash) = 64 AND input_hash = lower(input_hash)
              AND input_hash NOT GLOB '*[^0-9a-f]*'),
    canonical_payload_json TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL
        CHECK(length(payload_sha256) = 64 AND payload_sha256 = lower(payload_sha256)
              AND payload_sha256 NOT GLOB '*[^0-9a-f]*'),
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER,
    provider_version_notes TEXT,
    UNIQUE(provider, entity_kind, entity_id, input_hash, payload_sha256)
) STRICT;
INSERT INTO library_management_metadata_snapshots_new (rowid, id, provider, entity_kind, entity_id,
    input_hash, canonical_payload_json, payload_sha256, fetched_at, expires_at,
    provider_version_notes)
SELECT rowid, id, CAST(provider AS TEXT), CAST(entity_kind AS TEXT), CAST(entity_id AS TEXT),
    CAST(input_hash AS TEXT), CAST(canonical_payload_json AS TEXT), CAST(payload_sha256 AS TEXT),
    CAST(round(fetched_at * 1000) AS INTEGER), CAST(round(expires_at * 1000) AS INTEGER),
    CAST(provider_version_notes AS TEXT)
FROM library_management_metadata_snapshots WHERE typeof(id) = 'text';
DROP TABLE library_management_metadata_snapshots;
ALTER TABLE library_management_metadata_snapshots_new
    RENAME TO library_management_metadata_snapshots;
CREATE INDEX idx_management_metadata_lookup
ON library_management_metadata_snapshots(provider, entity_kind, entity_id, input_hash);
CREATE INDEX idx_management_metadata_expiry
ON library_management_metadata_snapshots(expires_at, id);
CREATE TRIGGER trg_management_metadata_snapshot_immutable
BEFORE UPDATE ON library_management_metadata_snapshots
BEGIN
    SELECT RAISE(ABORT, 'management metadata snapshots are immutable');
END;

CREATE TABLE library_management_overrides_new (
    id TEXT PRIMARY KEY,
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('album','track')),
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    field_name TEXT NOT NULL CHECK(length(trim(field_name)) > 0),
    value_json TEXT NOT NULL,
    mode TEXT NOT NULL CHECK(mode IN ('replace','preserve','clear')),
    actor_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    reason TEXT,
    subject_revision INTEGER NOT NULL
        CHECK(subject_revision BETWEEN 1 AND 9223372036854775807),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    CHECK(
        (subject_kind = 'album' AND local_album_id IS NOT NULL AND local_track_id IS NULL)
        OR
        (subject_kind = 'track' AND local_track_id IS NOT NULL AND local_album_id IS NULL)
    )
) STRICT;
INSERT INTO library_management_overrides_new (rowid, id, subject_kind, local_album_id,
    local_track_id, field_name, value_json, mode, actor_user_id, reason, subject_revision,
    created_at, updated_at, row_revision)
SELECT rowid, id, subject_kind, CAST(local_album_id AS TEXT), CAST(local_track_id AS TEXT),
    CAST(field_name AS TEXT), CAST(value_json AS TEXT), mode, CAST(actor_user_id AS TEXT),
    CAST(reason AS TEXT), CAST(subject_revision AS INTEGER),
    CAST(round(created_at * 1000) AS INTEGER), CAST(round(updated_at * 1000) AS INTEGER),
    CAST(row_revision AS INTEGER)
FROM library_management_overrides WHERE typeof(id) = 'text'
    AND typeof(local_album_id) <> 'blob' AND typeof(local_track_id) <> 'blob';
DROP TABLE library_management_overrides;
ALTER TABLE library_management_overrides_new RENAME TO library_management_overrides;
CREATE UNIQUE INDEX idx_management_override_album_field
ON library_management_overrides(local_album_id, field_name)
WHERE subject_kind = 'album';
CREATE UNIQUE INDEX idx_management_override_track_field
ON library_management_overrides(local_track_id, field_name)
WHERE subject_kind = 'track';
CREATE INDEX idx_library_management_overrides_actor_user_id
    ON library_management_overrides(actor_user_id);
CREATE INDEX idx_library_management_overrides_local_track_id
    ON library_management_overrides(local_track_id);
CREATE INDEX idx_library_management_overrides_local_album_id
    ON library_management_overrides(local_album_id);

CREATE TABLE library_management_plan_items_new (
    job_id TEXT NOT NULL
        REFERENCES library_management_job_snapshots(job_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    bundle_ordinal INTEGER NOT NULL CHECK(bundle_ordinal >= 0),
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    expected_album_revision INTEGER CHECK(
        expected_album_revision IS NULL
        OR expected_album_revision BETWEEN 1 AND 9223372036854775807
    ),
    expected_track_revision INTEGER CHECK(
        expected_track_revision IS NULL
        OR expected_track_revision BETWEEN 1 AND 9223372036854775807
    ),
    expected_identity_revision INTEGER CHECK(
        expected_identity_revision IS NULL
        OR expected_identity_revision BETWEEN 1 AND 9223372036854775807
    ),
    expected_album_identity_revision INTEGER CHECK(
        expected_album_identity_revision IS NULL
        OR expected_album_identity_revision BETWEEN 1 AND 9223372036854775807
    ),
    expected_override_revision TEXT,
    expected_catalog_revision INTEGER NOT NULL
        CHECK(expected_catalog_revision BETWEEN 0 AND 9223372036854775807),
    expected_policy_revision TEXT NOT NULL,
    expected_profile_revision TEXT NOT NULL,
    expected_root_id TEXT NOT NULL,
    expected_relative_path TEXT NOT NULL,
    expected_stat_revision TEXT NOT NULL,
    expected_tag_revision TEXT NOT NULL,
    expected_file_fingerprint TEXT NOT NULL,
    source_path_identity TEXT NOT NULL,
    destination_root_id TEXT,
    destination_relative_path TEXT,
    destination_collision_key TEXT,
    desired_document_json TEXT NOT NULL,
    desired_document_hash TEXT NOT NULL
        CHECK(length(desired_document_hash) = 64
              AND desired_document_hash = lower(desired_document_hash)
              AND desired_document_hash NOT GLOB '*[^0-9a-f]*'),
    catalog_document_json TEXT,
    catalog_document_hash TEXT,
    artwork_choices_json TEXT NOT NULL DEFAULT '[]',
    diff_json TEXT NOT NULL DEFAULT '{}',
    capability_json TEXT NOT NULL DEFAULT '{}',
    collision_json TEXT NOT NULL DEFAULT '[]',
    eligibility TEXT NOT NULL CHECK(eligibility IN ('eligible','warning','blocked','stale')),
    reason_code TEXT,
    estimated_temporary_bytes INTEGER NOT NULL DEFAULT 0
        CHECK(estimated_temporary_bytes >= 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(job_id, ordinal),
    CHECK(local_album_id IS NOT NULL OR local_track_id IS NOT NULL)
) STRICT;
INSERT INTO library_management_plan_items_new (rowid, job_id, ordinal, bundle_ordinal,
    local_album_id, local_track_id, expected_album_revision, expected_track_revision,
    expected_identity_revision, expected_album_identity_revision, expected_override_revision,
    expected_catalog_revision, expected_policy_revision, expected_profile_revision,
    expected_root_id, expected_relative_path, expected_stat_revision, expected_tag_revision,
    expected_file_fingerprint, source_path_identity, destination_root_id,
    destination_relative_path, destination_collision_key, desired_document_json,
    desired_document_hash, catalog_document_json, catalog_document_hash, artwork_choices_json,
    diff_json, capability_json, collision_json, eligibility, reason_code,
    estimated_temporary_bytes, created_at)
SELECT rowid, CAST(job_id AS TEXT), ordinal, CAST(bundle_ordinal AS INTEGER),
    CAST(local_album_id AS TEXT), CAST(local_track_id AS TEXT),
    CAST(expected_album_revision AS INTEGER), CAST(expected_track_revision AS INTEGER),
    CAST(expected_identity_revision AS INTEGER), CAST(expected_album_identity_revision AS INTEGER),
    CAST(expected_override_revision AS TEXT), CAST(expected_catalog_revision AS INTEGER),
    CAST(expected_policy_revision AS TEXT), CAST(expected_profile_revision AS TEXT),
    CAST(expected_root_id AS TEXT), CAST(expected_relative_path AS TEXT),
    CAST(expected_stat_revision AS TEXT), CAST(expected_tag_revision AS TEXT),
    CAST(expected_file_fingerprint AS TEXT), CAST(source_path_identity AS TEXT),
    CAST(destination_root_id AS TEXT), CAST(destination_relative_path AS TEXT),
    CAST(destination_collision_key AS TEXT), CAST(desired_document_json AS TEXT),
    CAST(desired_document_hash AS TEXT), CAST(catalog_document_json AS TEXT),
    CAST(catalog_document_hash AS TEXT), CAST(artwork_choices_json AS TEXT),
    CAST(diff_json AS TEXT), CAST(capability_json AS TEXT), CAST(collision_json AS TEXT),
    eligibility, CAST(reason_code AS TEXT), CAST(estimated_temporary_bytes AS INTEGER),
    CAST(round(created_at * 1000) AS INTEGER)
FROM library_management_plan_items WHERE typeof(ordinal) = 'integer' AND typeof(job_id) = 'text'
    AND typeof(local_album_id) <> 'blob' AND typeof(local_track_id) <> 'blob';
DROP TABLE library_management_plan_items;
ALTER TABLE library_management_plan_items_new RENAME TO library_management_plan_items;
CREATE INDEX idx_management_plan_cursor
ON library_management_plan_items(job_id, ordinal);
CREATE INDEX idx_management_plan_eligibility
ON library_management_plan_items(job_id, eligibility, ordinal);
CREATE INDEX idx_management_plan_track
ON library_management_plan_items(job_id, local_track_id);
CREATE INDEX idx_management_plan_destination
    ON library_management_plan_items(
        job_id, destination_root_id, destination_collision_key, ordinal);
CREATE INDEX idx_library_management_plan_items_local_track_id
    ON library_management_plan_items(local_track_id);
CREATE INDEX idx_library_management_plan_items_local_album_id
    ON library_management_plan_items(local_album_id);

CREATE TABLE library_publish_operations_new (
    bundle_id TEXT PRIMARY KEY,
    bundle_json TEXT NOT NULL,
    applied_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_publish_operations_new (rowid, bundle_id, bundle_json, applied_by_user_id,
    created_at)
SELECT rowid, bundle_id, CAST(bundle_json AS TEXT), CAST(applied_by_user_id AS TEXT),
    CASE WHEN CAST(created_at AS INTEGER) > 0 THEN CAST(created_at AS INTEGER)
        ELSE max(min(CAST(created_day AS INTEGER), 100000000000), -100000000000) * 86400000 END
FROM library_publish_operations WHERE typeof(bundle_id) = 'text';
DROP TABLE library_publish_operations;
ALTER TABLE library_publish_operations_new RENAME TO library_publish_operations;
CREATE INDEX idx_library_publish_operations_applied_by_user_id
    ON library_publish_operations(applied_by_user_id);

CREATE TABLE library_publish_journal_new (
    id TEXT PRIMARY KEY,
    bundle_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN (
        'audio','sidecar','artwork','output','delete','remove','put_back','add'
    )),
    source_root TEXT,
    source_rel TEXT,
    dest_root TEXT NOT NULL,
    dest_rel TEXT NOT NULL,
    staged TEXT NOT NULL,
    backup TEXT,
    source_sha256 TEXT,
    staged_sha256 TEXT NOT NULL,
    track_id TEXT,
    state TEXT NOT NULL CHECK(state IN (
        'prepared','staged','published','committed','cleaned','compensated','needs_attention',
        'cleanup_pending'
    )),
    seq INTEGER NOT NULL DEFAULT 0,
    fence_revision INTEGER,
    commit_as TEXT CHECK (commit_as IN ('managed', 'imported'))
) STRICT;
INSERT INTO library_publish_journal_new (rowid, id, bundle_id, kind, source_root, source_rel,
    dest_root, dest_rel, staged, backup, source_sha256, staged_sha256, track_id, state, seq,
    fence_revision, commit_as)
SELECT rowid, id, CAST(bundle_id AS TEXT),
    CASE WHEN kind IN ('audio','sidecar','artwork','output','delete','remove','put_back','add')
        THEN kind ELSE 'sidecar' END,
    CAST(source_root AS TEXT), CAST(source_rel AS TEXT), CAST(dest_root AS TEXT),
    CAST(dest_rel AS TEXT), CAST(staged AS TEXT), CAST(backup AS TEXT),
    CAST(source_sha256 AS TEXT), CAST(staged_sha256 AS TEXT), CAST(track_id AS TEXT),
    CASE WHEN kind IN ('audio','sidecar','artwork','output','delete','remove','put_back','add')
            AND state IN ('prepared','staged','published','committed','cleaned','compensated',
                'needs_attention','cleanup_pending')
        THEN state ELSE 'needs_attention' END,
    CAST(seq AS INTEGER), CAST(fence_revision AS INTEGER), commit_as
FROM library_publish_journal WHERE typeof(id) = 'text';
DROP TABLE library_publish_journal;
ALTER TABLE library_publish_journal_new RENAME TO library_publish_journal;
CREATE INDEX idx_library_publish_journal_bundle
    ON library_publish_journal(bundle_id);
CREATE INDEX idx_library_publish_journal_state
    ON library_publish_journal(state);
CREATE INDEX idx_library_publish_journal_track
    ON library_publish_journal(track_id) WHERE track_id IS NOT NULL;
CREATE INDEX idx_library_publish_journal_fence
    ON library_publish_journal(fence_revision) WHERE fence_revision IS NOT NULL;

CREATE TABLE library_publish_replacements_new (
    journal_id TEXT PRIMARY KEY REFERENCES library_publish_journal(id) ON DELETE CASCADE,
    replaced_rel TEXT NOT NULL,
    replaced_sha256 TEXT NOT NULL,
    replaced_track_id TEXT,
    recycle_to TEXT NOT NULL,
    recycled_at INTEGER,
    expired_at INTEGER,
    binned_stat_json TEXT
        CHECK (binned_stat_json IS NULL OR json_valid(binned_stat_json))
) STRICT;
INSERT INTO library_publish_replacements_new (rowid, journal_id, replaced_rel, replaced_sha256,
    replaced_track_id, recycle_to, recycled_at, expired_at, binned_stat_json)
SELECT rowid, journal_id, CAST(replaced_rel AS TEXT), CAST(replaced_sha256 AS TEXT),
    CAST(replaced_track_id AS TEXT), CAST(recycle_to AS TEXT),
    CAST(round(recycled_at * 1000) AS INTEGER), CAST(expired_at AS INTEGER),
    CAST(binned_stat_json AS TEXT)
FROM library_publish_replacements WHERE typeof(journal_id) = 'text';
DROP TABLE library_publish_replacements;
ALTER TABLE library_publish_replacements_new RENAME TO library_publish_replacements;
CREATE INDEX idx_library_publish_replacements_due
    ON library_publish_replacements(recycled_at, journal_id) WHERE expired_at IS NULL;
CREATE INDEX idx_library_publish_replacements_track
    ON library_publish_replacements(replaced_track_id)
    WHERE replaced_track_id IS NOT NULL;

CREATE TABLE library_publish_resolutions_new (
    bundle_id TEXT PRIMARY KEY,
    resolved_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    verified_files INTEGER NOT NULL CHECK(verified_files >= 0),
    resolved_at INTEGER NOT NULL,
    as_before_files INTEGER NOT NULL DEFAULT 0 CHECK (as_before_files >= 0),
    empty_files INTEGER NOT NULL DEFAULT 0 CHECK (empty_files >= 0)
) STRICT;
INSERT INTO library_publish_resolutions_new (rowid, bundle_id, resolved_by_user_id, verified_files,
    resolved_at, as_before_files, empty_files)
SELECT rowid, bundle_id, (SELECT u.id FROM auth_users u WHERE u.id = resolved_by_user_id),
    CAST(verified_files AS INTEGER), CAST(round(resolved_at * 1000) AS INTEGER),
    CAST(as_before_files AS INTEGER), CAST(empty_files AS INTEGER)
FROM library_publish_resolutions WHERE typeof(bundle_id) = 'text';
DROP TABLE library_publish_resolutions;
ALTER TABLE library_publish_resolutions_new RENAME TO library_publish_resolutions;
CREATE INDEX idx_library_publish_resolutions_resolved_by_user_id
    ON library_publish_resolutions(resolved_by_user_id);

CREATE TABLE library_publish_snapshots_new (
    id TEXT PRIMARY KEY,
    bundle_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    blob_sha256 TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_publish_snapshots_new (rowid, id, bundle_id, track_id, blob_sha256, created_at,
    expires_at)
SELECT rowid, id, CAST(bundle_id AS TEXT), CAST(track_id AS TEXT), CAST(blob_sha256 AS TEXT),
    max(min(CAST(created_day AS INTEGER), 100000000000), -100000000000) * 86400000,
    CASE WHEN typeof(expires_day) IN ('integer', 'real') THEN max(min(CAST(expires_day AS INTEGER), 100000000000), -100000000000) * 86400000
        ELSE 100000000000 * 86400000 END
FROM library_publish_snapshots WHERE typeof(id) = 'text';
DROP TABLE library_publish_snapshots;
ALTER TABLE library_publish_snapshots_new RENAME TO library_publish_snapshots;
CREATE INDEX idx_library_publish_snapshots_bundle
    ON library_publish_snapshots(bundle_id);

CREATE TABLE library_publish_baselines_new (
    track_id TEXT PRIMARY KEY,
    blob_sha256 TEXT NOT NULL,
    original_root TEXT NOT NULL,
    original_rel TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_publish_baselines_new (rowid, track_id, blob_sha256, original_root,
    original_rel, created_at)
SELECT rowid, track_id, CAST(blob_sha256 AS TEXT), CAST(original_root AS TEXT),
    CAST(original_rel AS TEXT), max(min(CAST(created_day AS INTEGER), 100000000000), -100000000000) * 86400000
FROM library_publish_baselines WHERE typeof(track_id) = 'text';
DROP TABLE library_publish_baselines;
ALTER TABLE library_publish_baselines_new RENAME TO library_publish_baselines;

CREATE TABLE library_publish_blobs_new (
    sha256 TEXT PRIMARY KEY,
    bytes BLOB NOT NULL
) STRICT;
INSERT INTO library_publish_blobs_new (rowid, sha256, bytes)
SELECT rowid, sha256, CAST(bytes AS BLOB)
FROM library_publish_blobs WHERE typeof(sha256) = 'text';
DROP TABLE library_publish_blobs;
ALTER TABLE library_publish_blobs_new RENAME TO library_publish_blobs;

CREATE TABLE library_publish_blob_refs_new (
    sha256 TEXT NOT NULL,
    owner_kind TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    PRIMARY KEY (sha256, owner_kind, owner_id)
) STRICT;
INSERT INTO library_publish_blob_refs_new (rowid, sha256, owner_kind, owner_id)
SELECT rowid, sha256, owner_kind, owner_id
FROM library_publish_blob_refs
WHERE typeof(sha256) = 'text' AND typeof(owner_kind) = 'text' AND typeof(owner_id) = 'text';
DROP TABLE library_publish_blob_refs;
ALTER TABLE library_publish_blob_refs_new RENAME TO library_publish_blob_refs;

CREATE TRIGGER trg_library_publish_journal_fence_insert
AFTER INSERT ON library_publish_journal
WHEN NEW.track_id IS NOT NULL OR NEW.kind = 'remove'
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE id = NEW.id;
END;
CREATE TRIGGER trg_library_publish_journal_fence_state
AFTER UPDATE OF state ON library_publish_journal
WHEN NEW.state IS NOT OLD.state AND (NEW.track_id IS NOT NULL OR NEW.kind = 'remove')
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE id = NEW.id;
END;
CREATE TRIGGER trg_library_publish_resolution_fence
AFTER INSERT ON library_publish_resolutions
WHEN EXISTS (
    SELECT 1 FROM library_publish_journal
    WHERE bundle_id = NEW.bundle_id
      AND (track_id IS NOT NULL OR kind = 'remove')
)
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE bundle_id = NEW.bundle_id
          AND (track_id IS NOT NULL OR kind = 'remove');
END;

CREATE VIEW library_publish_outputs_rule
    (journal_id, bundle_id, item_index, output_index, track_id, dest_root, dest_rel,
     sha256, size, kind) AS
SELECT j.id, j.bundle_id, i.key, x.key, json_extract(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.track_id'),
       j.dest_root, j.dest_rel, j.staged_sha256, json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.size'),
       json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.kind')
FROM library_publish_journal j
JOIN library_publish_operations o ON o.bundle_id = j.bundle_id
JOIN json_each(CASE WHEN json_valid(o.bundle_json) THEN o.bundle_json ELSE '{}' END,
               '$.items') i
JOIN json_each(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.outputs') x
WHERE j.kind = 'output'
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.dest_root') = j.dest_root
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.dest_rel') = j.dest_rel
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.sha256') = j.staged_sha256
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.kind') IN ('artwork', 'lyrics')
  AND typeof(json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.size')) = 'integer'
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.size') >= 0
  AND typeof(json_extract(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.track_id')) = 'text'
  AND EXISTS (SELECT 1 FROM library_publish_journal k
              WHERE k.bundle_id = j.bundle_id
                AND k.state IN ('committed', 'cleanup_pending', 'cleaned'));
CREATE TRIGGER trg_library_publish_outputs_refresh
INSTEAD OF INSERT ON library_publish_outputs_refresh
WHEN NEW.journal_id IS NOT NULL
BEGIN
    INSERT INTO library_publish_outputs
        (journal_id, bundle_id, item_index, output_index, track_id, dest_root, dest_rel,
         sha256, size, kind)
    SELECT journal_id, bundle_id, item_index, output_index, track_id, dest_root, dest_rel,
           sha256, size, kind
    FROM library_publish_outputs_rule WHERE journal_id = NEW.journal_id
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER trg_library_publish_outputs_landed
AFTER UPDATE OF state ON library_publish_journal
WHEN NEW.kind = 'output'
 AND NEW.state IN ('committed', 'cleanup_pending', 'cleaned')
 AND OLD.state NOT IN ('committed', 'cleanup_pending', 'cleaned')
BEGIN
    INSERT INTO library_publish_outputs_refresh (journal_id) VALUES (NEW.id);
END;

INSERT INTO library_rebuild_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, fkid, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3, 4) AS now
    WHERE now.n > (SELECT COUNT(*) FROM library_rebuild_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent AND before.fkid = now.fkid));

DROP TABLE library_rebuild_guard;
DROP TABLE library_rebuild_orphans;

-- Migration high-water mark.
PRAGMA user_version = 115;
COMMIT;
PRAGMA foreign_keys=ON;
