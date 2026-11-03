-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0129_library_state_checks.sql - the library's last tables reach the schema
-- conventions: every state column lists exactly the values its enum writes.
--
-- Rebuilt as STRICT tables, rowids, indexes and triggers carried (the rebuild
-- recipe in kernel/src/schema/boot.rs; library_identification_attempts is the
-- parent of the three external identity tables). What each row becomes:
--
--   * library_catalog_revision and library_event_stream_revisions become
--     STRICT. A counter that is no number reads as 0, and a stream with no
--     name, which no read can name, is not copied;
--   * library_audio_fingerprint_outcomes.state lists matched, no_match and
--     deferred. Nothing reads the state; failed, disabled and skipped were
--     lookups that never answered, and become deferred;
--   * local_album_artwork.source lists embedded, folder and manual. A
--     cover_cache or provider row is a cover no reader can serve and the scan
--     cannot decode. It is deleted (its genre art revisions move, as on any
--     delete) and its album's art check is reset, keeping the art version it
--     reached, so the next scan looks for that album's art again;
--   * library_identification_attempts.state lists identified and
--     needs_review. Nothing reads it; any other value did not end identified
--     and becomes needs_review;
--   * library_publish_blob_refs.owner_kind lists operation, preview, baseline
--     and v2_baseline, the only kinds ever written. A reference holds undo
--     bytes, so one of any other kind refuses the upgrade rather than be
--     guessed at;
--   * library_scan_inventory.processing_state lists pending, indexed,
--     skipped, deferred and failed. Any other value becomes pending, the
--     column's default, so a run that resumes reads that file again;
--   * library_catalog_actions.action_kind lists the kinds the library writes.
--     A row of any other kind (edition_extras_recycled, edition_extras_restored
--     and explicit_reidentification from builds before the release) is named by
--     no read, and is deleted;
--   * library_scan_run_scopes loses reconciliation_state, which nothing
--     writes: every row holds its default.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE library_checks_129_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_129 INTEGER CONSTRAINT guard_stamp_below_129 CHECK (stamp_below_129 = 1),
    blob_owners_known INTEGER CONSTRAINT guard_blob_owners_known CHECK (blob_owners_known = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO library_checks_129_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO library_checks_129_guard (stamp_below_129)
    SELECT (SELECT user_version FROM pragma_user_version) < 129;
INSERT INTO library_checks_129_guard (blob_owners_known)
    SELECT NOT EXISTS (SELECT 1 FROM library_publish_blob_refs
        WHERE owner_kind NOT IN ('operation', 'preview', 'baseline', 'v2_baseline'));

CREATE TEMP TABLE library_checks_129_orphans AS
    SELECT "table" AS child, rowid AS row, parent FROM pragma_foreign_key_check;
CREATE INDEX temp.library_checks_129_orphans_at
    ON library_checks_129_orphans(child, row, parent);

-- Revision counters ------------------------------------------------------------

CREATE TABLE library_catalog_revision_new (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    value INTEGER NOT NULL DEFAULT 0
        CHECK(value BETWEEN 0 AND 9223372036854775807)
) STRICT;
INSERT INTO library_catalog_revision_new (singleton, value)
SELECT singleton, CASE WHEN typeof(value) IN ('integer', 'real') AND value >= 0
    THEN CAST(value AS INTEGER) ELSE 0 END
FROM library_catalog_revision WHERE singleton = 1;
DROP TABLE library_catalog_revision;
ALTER TABLE library_catalog_revision_new RENAME TO library_catalog_revision;

CREATE TABLE library_event_stream_revisions_new (
    stream_kind TEXT PRIMARY KEY NOT NULL
        CHECK(stream_kind IN ('scan','identification','operation')),
    value INTEGER NOT NULL CHECK(value BETWEEN 0 AND 9223372036854775807)
) STRICT;
INSERT INTO library_event_stream_revisions_new (rowid, stream_kind, value)
SELECT rowid, stream_kind, CASE WHEN typeof(value) IN ('integer', 'real') AND value >= 0
    THEN CAST(value AS INTEGER) ELSE 0 END
FROM library_event_stream_revisions WHERE stream_kind IS NOT NULL;
DROP TABLE library_event_stream_revisions;
ALTER TABLE library_event_stream_revisions_new RENAME TO library_event_stream_revisions;

-- Fingerprints -----------------------------------------------------------------

CREATE TABLE library_audio_fingerprint_outcomes_new (
    id TEXT PRIMARY KEY,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    stat_revision TEXT NOT NULL,
    fingerprinter_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('matched','no_match','deferred')),
    fingerprint TEXT,
    duration_seconds REAL,
    recording_mbid TEXT,
    release_group_ids_json TEXT NOT NULL DEFAULT '[]',
    partial_decode INTEGER NOT NULL DEFAULT 0 CHECK(partial_decode IN (0,1)),
    score REAL,
    failure_code TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 1 CHECK(attempt_count >= 1),
    first_attempt_at INTEGER NOT NULL,
    last_attempt_at INTEGER NOT NULL,
    retry_at INTEGER,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(local_track_id, stat_revision, fingerprinter_version)
) STRICT;
INSERT INTO library_audio_fingerprint_outcomes_new (rowid, id, local_track_id, stat_revision,
    fingerprinter_version, state, fingerprint, duration_seconds, recording_mbid,
    release_group_ids_json, partial_decode, score, failure_code, attempt_count,
    first_attempt_at, last_attempt_at, retry_at, row_revision)
SELECT rowid, id, local_track_id, stat_revision, fingerprinter_version,
    CASE WHEN state IN ('matched', 'no_match', 'deferred') THEN state ELSE 'deferred' END,
    fingerprint, duration_seconds, recording_mbid, release_group_ids_json, partial_decode,
    score, failure_code, attempt_count, first_attempt_at, last_attempt_at, retry_at,
    row_revision
FROM library_audio_fingerprint_outcomes;
DROP TABLE library_audio_fingerprint_outcomes;
ALTER TABLE library_audio_fingerprint_outcomes_new RENAME TO library_audio_fingerprint_outcomes;

-- Album art --------------------------------------------------------------------

INSERT INTO local_album_artwork_checks (local_album_id, signature, checked_at, art_version)
SELECT local_album_id, '', 0, version FROM local_album_artwork
WHERE source NOT IN ('embedded', 'folder', 'manual')
    AND local_album_id IN (SELECT id FROM local_albums)
ON CONFLICT(local_album_id) DO UPDATE SET signature = '',
    art_version = MAX(art_version, excluded.art_version);
DELETE FROM local_album_artwork WHERE source NOT IN ('embedded', 'folder', 'manual');

CREATE TABLE local_album_artwork_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    cover_url TEXT,
    source TEXT NOT NULL CHECK(source IN ('embedded','folder','manual')),
    source_locator TEXT,
    content_hash TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK(version BETWEEN 1 AND 9223372036854775807),
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;
INSERT INTO local_album_artwork_new (rowid, local_album_id, cover_url, source, source_locator,
    content_hash, version, updated_at, row_revision)
SELECT rowid, local_album_id, cover_url, source, source_locator, content_hash, version,
    updated_at, row_revision
FROM local_album_artwork;
DROP TABLE local_album_artwork;
ALTER TABLE local_album_artwork_new RENAME TO local_album_artwork;

CREATE TRIGGER trg_genre_artwork_artwork_delete
AFTER DELETE ON local_album_artwork
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = OLD.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_artwork_insert
AFTER INSERT ON local_album_artwork
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_artwork_update
AFTER UPDATE ON local_album_artwork
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

-- Identification attempts ------------------------------------------------------

CREATE TABLE library_identification_attempts_new (
    id TEXT PRIMARY KEY,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    trigger TEXT NOT NULL,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    input_tag_revision TEXT NOT NULL,
    input_policy_revision TEXT NOT NULL,
    input_file_revision TEXT NOT NULL,
    input_identity_revision TEXT NOT NULL DEFAULT '',
    matcher_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('identified','needs_review')),
    terminal_reason_code TEXT NOT NULL,
    selected_candidate_key TEXT,
    candidate_count INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
    degradation_flags_json TEXT NOT NULL DEFAULT '[]',
    started_at INTEGER NOT NULL,
    completed_at INTEGER NOT NULL,
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
) STRICT;
INSERT INTO library_identification_attempts_new (rowid, id, local_album_id, local_track_id,
    trigger, requested_by_user_id, input_tag_revision, input_policy_revision,
    input_file_revision, input_identity_revision, matcher_version, state,
    terminal_reason_code, selected_candidate_key, candidate_count, degradation_flags_json,
    started_at, completed_at)
SELECT rowid, id, local_album_id, local_track_id, trigger, requested_by_user_id,
    input_tag_revision, input_policy_revision, input_file_revision, input_identity_revision,
    matcher_version,
    CASE WHEN state IN ('identified', 'needs_review') THEN state ELSE 'needs_review' END,
    terminal_reason_code, selected_candidate_key, candidate_count, degradation_flags_json,
    started_at, completed_at
FROM library_identification_attempts;
DROP TABLE library_identification_attempts;
ALTER TABLE library_identification_attempts_new RENAME TO library_identification_attempts;

CREATE INDEX idx_identification_attempt_subject_album
    ON library_identification_attempts(local_album_id, completed_at);
CREATE INDEX idx_identification_attempt_subject_track
    ON library_identification_attempts(local_track_id, completed_at);
CREATE INDEX idx_library_identification_attempts_requested_by_user_id
    ON library_identification_attempts(requested_by_user_id);

CREATE TRIGGER trg_library_identification_attempts_immutable
BEFORE UPDATE ON library_identification_attempts
BEGIN SELECT RAISE(ABORT, 'identification attempts are immutable'); END;

-- Publish blob references ------------------------------------------------------

CREATE TABLE library_publish_blob_refs_new (
    sha256 TEXT NOT NULL,
    owner_kind TEXT NOT NULL
        CHECK(owner_kind IN ('operation','preview','baseline','v2_baseline')),
    owner_id TEXT NOT NULL,
    PRIMARY KEY (sha256, owner_kind, owner_id)
) STRICT;
INSERT INTO library_publish_blob_refs_new (rowid, sha256, owner_kind, owner_id)
SELECT rowid, sha256, owner_kind, owner_id FROM library_publish_blob_refs;
DROP TABLE library_publish_blob_refs;
ALTER TABLE library_publish_blob_refs_new RENAME TO library_publish_blob_refs;

-- Scan inventory and scopes ----------------------------------------------------

CREATE TABLE library_scan_inventory_new (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    scope_relative_path TEXT NOT NULL DEFAULT '.',
    discovery_generation INTEGER NOT NULL DEFAULT 1,
    absolute_path TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL,
    file_mtime_ns INTEGER NOT NULL,
    stat_revision TEXT NOT NULL,
    policy_revision TEXT NOT NULL,
    effective_policy TEXT NOT NULL CHECK(effective_policy IN ('local_metadata','automatic','excluded')),
    comparison_result TEXT NOT NULL CHECK(comparison_result IN ('new','changed','unchanged','excluded','candidate_missing')),
    processing_state TEXT NOT NULL DEFAULT 'pending'
        CHECK(processing_state IN ('pending','indexed','skipped','deferred','failed')),
    checkpoint TEXT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    failure_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(run_id, root_id, relative_path)
) STRICT;
INSERT INTO library_scan_inventory_new (rowid, run_id, root_id, relative_path,
    scope_relative_path, discovery_generation, absolute_path, file_size_bytes, file_mtime_ns,
    stat_revision, policy_revision, effective_policy, comparison_result, processing_state,
    checkpoint, local_track_id, failure_code, row_revision)
SELECT rowid, run_id, root_id, relative_path, scope_relative_path, discovery_generation,
    absolute_path, file_size_bytes, file_mtime_ns, stat_revision, policy_revision,
    effective_policy, comparison_result,
    CASE WHEN processing_state IN ('pending', 'indexed', 'skipped', 'deferred', 'failed')
        THEN processing_state ELSE 'pending' END,
    checkpoint, local_track_id, failure_code, row_revision
FROM library_scan_inventory;
DROP TABLE library_scan_inventory;
ALTER TABLE library_scan_inventory_new RENAME TO library_scan_inventory;

CREATE INDEX idx_scan_inventory_processing
    ON library_scan_inventory(run_id, processing_state, root_id, relative_path);
CREATE INDEX idx_scan_inventory_management_candidates
    ON library_scan_inventory(run_id, processing_state, comparison_result, local_track_id);
CREATE INDEX idx_scan_inventory_track ON library_scan_inventory(local_track_id);

ALTER TABLE library_scan_run_scopes DROP COLUMN reconciliation_state;

-- Catalog actions --------------------------------------------------------------

CREATE TABLE library_catalog_actions_new (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT UNIQUE,
    actor_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    action_kind TEXT NOT NULL CHECK(action_kind IN ('accept_management_track_mappings',
        'album_artist', 'album_artist_undone', 'artist_link_moved', 'artist_link_removed',
        'artist_link_set', 'automatic_exact_edition', 'automatic_match_sealed',
        'automatic_match_withdrawn', 'continue_tracks', 'custom_edition', 'edition_choice',
        'edition_choice_cleared', 'edition_choice_undone', 'edition_confirmed',
        'files_put_back', 'files_recycled', 'fold_undone', 'follow_merged_artist_id',
        'leave_unmanaged', 'management_hand_back_given_up', 'management_handed_back',
        'management_reenabled', 'management_stepped_back', 'merge', 'merge_album',
        'merge_artist', 'move', 'new_copy_at_path', 'other_music_at_path', 'remove_track',
        'repair_detach', 'reset', 'retag_dismissal_ended', 'retag_first_half',
        'review_approve', 'review_detach', 'review_exclude', 'review_reopened',
        'review_restore', 'split', 'track_match_undone', 'track_matched',
        'undo_automatic_edition')),
    local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
    before_json TEXT NOT NULL,
    after_json TEXT NOT NULL,
    reason_code TEXT,
    created_at INTEGER NOT NULL,
    CHECK(local_artist_id IS NOT NULL OR local_album_id IS NOT NULL OR local_track_id IS NOT NULL)
) STRICT;
INSERT INTO library_catalog_actions_new (rowid, id, idempotency_key, actor_user_id,
    action_kind, local_artist_id, local_album_id, local_track_id, operation_job_id,
    before_json, after_json, reason_code, created_at)
SELECT rowid, id, idempotency_key, actor_user_id, action_kind, local_artist_id,
    local_album_id, local_track_id, operation_job_id, before_json, after_json, reason_code,
    created_at
FROM library_catalog_actions
WHERE action_kind IN ('accept_management_track_mappings',
    'album_artist', 'album_artist_undone', 'artist_link_moved', 'artist_link_removed',
    'artist_link_set', 'automatic_exact_edition', 'automatic_match_sealed',
    'automatic_match_withdrawn', 'continue_tracks', 'custom_edition', 'edition_choice',
    'edition_choice_cleared', 'edition_choice_undone', 'edition_confirmed',
    'files_put_back', 'files_recycled', 'fold_undone', 'follow_merged_artist_id',
    'leave_unmanaged', 'management_hand_back_given_up', 'management_handed_back',
    'management_reenabled', 'management_stepped_back', 'merge', 'merge_album',
    'merge_artist', 'move', 'new_copy_at_path', 'other_music_at_path', 'remove_track',
    'repair_detach', 'reset', 'retag_dismissal_ended', 'retag_first_half',
    'review_approve', 'review_detach', 'review_exclude', 'review_reopened',
    'review_restore', 'split', 'track_match_undone', 'track_matched',
    'undo_automatic_edition');
DROP TABLE library_catalog_actions;
ALTER TABLE library_catalog_actions_new RENAME TO library_catalog_actions;

CREATE INDEX idx_library_catalog_actions_album_kind
    ON library_catalog_actions(local_album_id, action_kind);
CREATE INDEX idx_library_catalog_actions_operation_job_id
    ON library_catalog_actions(operation_job_id);
CREATE INDEX idx_library_catalog_actions_local_track_id
    ON library_catalog_actions(local_track_id);
CREATE INDEX idx_library_catalog_actions_local_artist_id
    ON library_catalog_actions(local_artist_id);
CREATE INDEX idx_library_catalog_actions_actor_user_id
    ON library_catalog_actions(actor_user_id);
CREATE INDEX idx_library_catalog_actions_undoes
    ON library_catalog_actions(json_extract(before_json, '$.undoes'))
    WHERE action_kind = 'files_put_back';

INSERT INTO library_checks_129_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3) AS now
    WHERE now.n > (SELECT COUNT(*) FROM library_checks_129_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent));

DROP TABLE library_checks_129_guard;
DROP TABLE library_checks_129_orphans;

-- Migration high-water mark.
PRAGMA user_version = 129;
COMMIT;
PRAGMA foreign_keys=ON;
