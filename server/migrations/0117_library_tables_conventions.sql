-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0117_library_tables_conventions.sql - the rest of the library's tables,
-- except the catalog parents, follow the schema conventions.
--
-- Each table is rebuilt as a STRICT table, rowids, indexes and triggers
-- carried (the rebuild recipe in kernel/src/schema/boot.rs). What each row
-- becomes:
--
--   * a time in REAL seconds becomes integer milliseconds, rounded. One too
--     far off to fit is held at the limit, and a required one that is no
--     number reads as 0, so it has passed. Undo records and sealed JSON keep
--     their times in seconds: the code rounds them to the millisecond where
--     it compares them with a row or writes them back;
--   * library_identify_jobs and library_identify_reviews lose the _ms suffix:
--     not_before_at, lease_expires_at, created_at, updated_at. The remap
--     queue's not_before and the fingerprint outcomes' retry_after become
--     not_before_at and retry_at;
--   * audio_fingerprint_outcomes becomes library_audio_fingerprint_outcomes;
--   * library_identify_jobs.requested_by_user_id,
--     library_identify_reviews.resolved_by_user_id and
--     library_edition_remap_queue.chosen_by_user_id gain a set-null key, with
--     an index. An id of no account becomes NULL;
--   * library_scan_run_scopes.discovery_state is checked against its enum. A
--     value outside it reads as partially_read, which never counts as walked;
--   * a RUN_END_PENDING row of library_scan_failures becomes the run's
--     next_end_pass, the first pass its note names that is in the list, with
--     the waits and the retry time of a pass put off. A note on a run with no
--     end time, or naming no pass in the list, is dropped: the library-wide
--     passes run again at the end of the next scan;
--   * a put-back whose before_json does not parse holds '{}', so the index on
--     what it undid can be built. It could not be matched to a removal before;
--   * library_album_release_pins and library_reference_tombstones go. An
--     import reads edition pins from its bundle, and nothing reads tombstones:
--     a playlist entry keeps its own names.
--
-- Rows with a key of the wrong type (a NULL or BLOB text key, a non-integer
-- ordinal) cannot be named by any read and are not copied. Rows that point at
-- one with a BLOB go with it, or lose the pointer where it is optional. A
-- whole number held as a fraction is cut down and kept at 1 or more where its
-- CHECK asks for that; when that makes a sealed edition's version or position
-- clash with another, the first row stays. A count that is no number starts at
-- 1. Rows no writer could make (a BLOB and text with the same bytes on one
-- unique key, say) may refuse the upgrade; a refusal is clean and changes
-- nothing.
--
-- library_artist_reconciliation_dismissals.dismissed_by_user_id and
-- library_custom_edition_manifests.sealed_by_user_id still restrict their
-- user, as the account deletion path expects today.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Orphans are compared by table, row and parent, not by foreign key number: a
-- rebuild that adds a key renumbers the table's others, and an old orphan must
-- not refuse the upgrade.
--
-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE library_rebuild_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_117 INTEGER CONSTRAINT guard_stamp_below_117 CHECK (stamp_below_117 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO library_rebuild_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO library_rebuild_guard (stamp_below_117)
    SELECT (SELECT user_version FROM pragma_user_version) < 117;

CREATE TEMP TABLE library_rebuild_orphans AS
    SELECT CASE "table" WHEN 'audio_fingerprint_outcomes'
               THEN 'library_audio_fingerprint_outcomes' ELSE "table" END AS child,
           rowid AS row, parent
    FROM pragma_foreign_key_check;
CREATE INDEX temp.library_rebuild_orphans_at
    ON library_rebuild_orphans(child, row, parent);

-- Children outside the rebuilt tables of parents that are not copied.
UPDATE local_album_external_identities SET attempt_id = NULL WHERE typeof(attempt_id) = 'blob';
UPDATE local_artist_external_identities SET attempt_id = NULL WHERE typeof(attempt_id) = 'blob';
UPDATE local_track_external_identities SET attempt_id = NULL WHERE typeof(attempt_id) = 'blob';

DROP TABLE library_album_release_pins;
DROP TABLE library_reference_tombstones;

CREATE TABLE library_scan_runs_new (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('incremental','rescan_files','policy_reconcile')),
    trigger TEXT NOT NULL CHECK(trigger IN ('manual','automatic','subsonic','startup_resume','policy_apply')),
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    state TEXT NOT NULL CHECK(state IN ('queued','discovering','indexing','reconciling','pausing','paused','stopping','completed','cancelled','superseded_policy_changed','failed')),
    phase TEXT NOT NULL CHECK(phase IN ('queued','discovering','indexing','reconciling')),
    resume_phase TEXT CHECK(resume_phase IN ('queued','discovering','indexing','reconciling')),
    requested_control TEXT NOT NULL DEFAULT 'none' CHECK(requested_control IN ('none','pause','stop')),
    aggregate_scope TEXT NOT NULL,
    total_count INTEGER NOT NULL DEFAULT 0,
    discovered_count INTEGER NOT NULL DEFAULT 0,
    inspected_count INTEGER NOT NULL DEFAULT 0,
    new_count INTEGER NOT NULL DEFAULT 0,
    changed_count INTEGER NOT NULL DEFAULT 0,
    indexed_count INTEGER NOT NULL DEFAULT 0,
    unchanged_count INTEGER NOT NULL DEFAULT 0,
    excluded_count INTEGER NOT NULL DEFAULT 0,
    missing_count INTEGER NOT NULL DEFAULT 0,
    errored_count INTEGER NOT NULL DEFAULT 0,
    identification_enqueued_count INTEGER NOT NULL DEFAULT 0,
    coalesced_request_count INTEGER NOT NULL DEFAULT 0,
    queued_at INTEGER NOT NULL,
    started_at INTEGER,
    updated_at INTEGER NOT NULL,
    terminal_at INTEGER,
    heartbeat_at INTEGER,
    terminal_code TEXT,
    terminal_summary TEXT,
    phase_started_at INTEGER,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
    stop_requested_at INTEGER,
    pause_requested_at INTEGER,
    control_latency_ms INTEGER,
    next_end_pass TEXT CHECK(next_end_pass IN ('scan_end_join_across_scans','scan_end_carry_to_first_half','scan_end_join_replaced','scan_end_keep_departures','scan_end_place_scanned','scan_end_continue_tracks','scan_end_continue_moved','scan_end_converge_artists','scan_end_hold_replacements','scan_end_offer_held_back','scan_end_held_scopes')),
    end_pass_waits_used INTEGER NOT NULL DEFAULT 0 CHECK(end_pass_waits_used >= 0),
    end_pass_retry_at INTEGER,
    inventory_cleanup_pending INTEGER NOT NULL DEFAULT 0
        CHECK(inventory_cleanup_pending IN (0,1)),
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    event_revision INTEGER NOT NULL DEFAULT 0 CHECK(event_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;
INSERT INTO library_scan_runs_new (rowid, id, kind, trigger, requested_by_user_id, state, phase, resume_phase, requested_control,
    aggregate_scope, total_count, discovered_count, inspected_count, new_count,
    changed_count, indexed_count, unchanged_count, excluded_count, missing_count,
    errored_count, identification_enqueued_count, coalesced_request_count, queued_at,
    started_at, updated_at, terminal_at, heartbeat_at, terminal_code, terminal_summary,
    phase_started_at, phase_timings_json, stop_requested_at, pause_requested_at,
    control_latency_ms, inventory_cleanup_pending, row_revision, event_revision,
    next_end_pass, end_pass_waits_used, end_pass_retry_at)
SELECT s.rowid, CAST(id AS TEXT), CAST(kind AS TEXT), CAST(trigger AS TEXT),
    CAST(requested_by_user_id AS TEXT), CAST(state AS TEXT), CAST(phase AS TEXT),
    CAST(resume_phase AS TEXT), CAST(requested_control AS TEXT),
    CAST(aggregate_scope AS TEXT), CAST(total_count AS INTEGER),
    CAST(discovered_count AS INTEGER), CAST(inspected_count AS INTEGER),
    CAST(new_count AS INTEGER), CAST(changed_count AS INTEGER),
    CAST(indexed_count AS INTEGER), CAST(unchanged_count AS INTEGER),
    CAST(excluded_count AS INTEGER), CAST(missing_count AS INTEGER),
    CAST(errored_count AS INTEGER), CAST(identification_enqueued_count AS INTEGER),
    CAST(coalesced_request_count AS INTEGER),
    COALESCE(CAST(round(queued_at * 1000) AS INTEGER), 0),
    CAST(round(started_at * 1000) AS INTEGER),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0),
    CAST(round(terminal_at * 1000) AS INTEGER),
    CAST(round(heartbeat_at * 1000) AS INTEGER), CAST(terminal_code AS TEXT),
    CAST(terminal_summary AS TEXT), CAST(round(phase_started_at * 1000) AS INTEGER),
    CAST(phase_timings_json AS TEXT), CAST(round(stop_requested_at * 1000) AS INTEGER),
    CAST(round(pause_requested_at * 1000) AS INTEGER), CAST(control_latency_ms AS INTEGER),
    CAST(inventory_cleanup_pending AS INTEGER), CAST(row_revision AS INTEGER),
    CAST(event_revision AS INTEGER),
    CASE WHEN s.terminal_at IS NOT NULL THEN (SELECT CASE WHEN instr(' ' || f.relative_path || ' ', ' scan_end_join_across_scans ') > 0 THEN 'scan_end_join_across_scans' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_carry_to_first_half ') > 0 THEN 'scan_end_carry_to_first_half' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_join_replaced ') > 0 THEN 'scan_end_join_replaced' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_keep_departures ') > 0 THEN 'scan_end_keep_departures' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_place_scanned ') > 0 THEN 'scan_end_place_scanned' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_continue_tracks ') > 0 THEN 'scan_end_continue_tracks' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_continue_moved ') > 0 THEN 'scan_end_continue_moved' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_converge_artists ') > 0 THEN 'scan_end_converge_artists' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_hold_replacements ') > 0 THEN 'scan_end_hold_replacements' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_offer_held_back ') > 0 THEN 'scan_end_offer_held_back' WHEN instr(' ' || f.relative_path || ' ', ' scan_end_held_scopes ') > 0 THEN 'scan_end_held_scopes' END FROM library_scan_failures f WHERE f.run_id = s.id AND f.failure_code = 'RUN_END_PENDING' LIMIT 1) END,
    COALESCE((SELECT MAX(0, CAST(substr(d.note, 9) AS INTEGER)) FROM (SELECT (SELECT f.failure_detail FROM library_scan_failures f WHERE f.run_id = s.id AND f.failure_code = 'RUN_END_PENDING' LIMIT 1) AS note) d WHERE d.note LIKE 'put off % %'), 0),
    (SELECT CAST(round(CAST(substr(d.note, 9 + instr(substr(d.note, 9), ' ')) AS REAL) * 1000) AS INTEGER) FROM (SELECT (SELECT f.failure_detail FROM library_scan_failures f WHERE f.run_id = s.id AND f.failure_code = 'RUN_END_PENDING' LIMIT 1) AS note) d WHERE d.note LIKE 'put off % %')
FROM library_scan_runs s WHERE typeof(id) = 'text';
DROP TABLE library_scan_runs;
ALTER TABLE library_scan_runs_new RENAME TO library_scan_runs;
CREATE INDEX idx_scan_runs_state ON library_scan_runs(state, queued_at);
CREATE UNIQUE INDEX idx_scan_runs_single_active
ON library_scan_runs((1))
WHERE state IN ('discovering','indexing','reconciling','pausing','paused','stopping');
CREATE UNIQUE INDEX idx_scan_runs_single_queued
ON library_scan_runs((1)) WHERE state = 'queued';
CREATE INDEX idx_library_scan_runs_requested_by_user_id
    ON library_scan_runs(requested_by_user_id);

CREATE TABLE library_scan_run_scopes_new (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    scope_sequence INTEGER NOT NULL,
    root_id TEXT NOT NULL,
    scope_id TEXT,
    relative_path TEXT NOT NULL,
    root_path TEXT,
    effective_policy TEXT NOT NULL CHECK(effective_policy IN ('local_metadata','automatic','excluded')),
    policy_revision TEXT NOT NULL,
    estimated_count INTEGER,
    discovered_count INTEGER NOT NULL DEFAULT 0,
    discovery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK(discovery_state IN ('pending','completed','partially_read','unavailable')),
    discovery_generation INTEGER NOT NULL DEFAULT 1,
    reconciliation_state TEXT NOT NULL DEFAULT 'pending',
    reconciliation_cursor TEXT,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
    error_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807), walk_revision INTEGER,
    PRIMARY KEY(run_id, scope_sequence),
    UNIQUE(run_id, root_id, relative_path)
) STRICT;
INSERT INTO library_scan_run_scopes_new (rowid, run_id, scope_sequence, root_id, scope_id, relative_path, root_path, effective_policy,
    policy_revision, estimated_count, discovered_count, discovery_state,
    discovery_generation, reconciliation_state, reconciliation_cursor, phase_timings_json,
    error_code, row_revision, walk_revision)
SELECT s.rowid, CAST(run_id AS TEXT), COALESCE(CAST(scope_sequence AS INTEGER), 0),
    CAST(root_id AS TEXT), CAST(scope_id AS TEXT), CAST(relative_path AS TEXT),
    CAST(root_path AS TEXT), CAST(effective_policy AS TEXT), CAST(policy_revision AS TEXT),
    CAST(estimated_count AS INTEGER), CAST(discovered_count AS INTEGER),
    CASE WHEN discovery_state IN ('pending','completed','partially_read','unavailable') THEN discovery_state ELSE 'partially_read' END,
    CAST(discovery_generation AS INTEGER), CAST(reconciliation_state AS TEXT),
    CAST(reconciliation_cursor AS TEXT), CAST(phase_timings_json AS TEXT),
    CAST(error_code AS TEXT), CAST(row_revision AS INTEGER),
    CAST(walk_revision AS INTEGER)
FROM library_scan_run_scopes s WHERE typeof(run_id) = 'text' AND typeof(scope_sequence) = 'integer';
DROP TABLE library_scan_run_scopes;
ALTER TABLE library_scan_run_scopes_new RENAME TO library_scan_run_scopes;

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
    processing_state TEXT NOT NULL DEFAULT 'pending',
    checkpoint TEXT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    failure_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(run_id, root_id, relative_path)
) STRICT;
INSERT INTO library_scan_inventory_new (rowid, run_id, root_id, relative_path, scope_relative_path, discovery_generation,
    absolute_path, file_size_bytes, file_mtime_ns, stat_revision, policy_revision,
    effective_policy, comparison_result, processing_state, checkpoint, local_track_id,
    failure_code, row_revision)
SELECT s.rowid, CAST(run_id AS TEXT), CAST(root_id AS TEXT), CAST(relative_path AS TEXT),
    CAST(scope_relative_path AS TEXT), CAST(discovery_generation AS INTEGER),
    CAST(absolute_path AS TEXT), COALESCE(CAST(file_size_bytes AS INTEGER), 0),
    COALESCE(CAST(file_mtime_ns AS INTEGER), 0), CAST(stat_revision AS TEXT),
    CAST(policy_revision AS TEXT), CAST(effective_policy AS TEXT),
    CAST(comparison_result AS TEXT), CAST(processing_state AS TEXT),
    CAST(checkpoint AS TEXT), CAST(local_track_id AS TEXT), CAST(failure_code AS TEXT),
    CAST(row_revision AS INTEGER)
FROM library_scan_inventory s WHERE typeof(run_id) = 'text' AND typeof(root_id) = 'text' AND typeof(relative_path) = 'text';
DROP TABLE library_scan_inventory;
ALTER TABLE library_scan_inventory_new RENAME TO library_scan_inventory;
CREATE INDEX idx_scan_inventory_processing ON library_scan_inventory(run_id, processing_state, root_id, relative_path);
CREATE INDEX idx_scan_inventory_management_candidates ON library_scan_inventory(run_id, processing_state, comparison_result, local_track_id);
CREATE INDEX idx_scan_inventory_track ON library_scan_inventory(local_track_id);

CREATE TABLE library_scan_failures_new (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    failure_code TEXT NOT NULL,
    failure_detail TEXT NOT NULL DEFAULT '',
    phase TEXT NOT NULL CHECK(phase IN ('discovering','indexing','reconciling')),
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY(run_id, root_id, relative_path, phase, failure_code)
) STRICT;
INSERT INTO library_scan_failures_new (rowid, run_id, root_id, relative_path, failure_code, failure_detail, phase, recorded_at)
SELECT s.rowid, CAST(run_id AS TEXT), CAST(root_id AS TEXT), CAST(relative_path AS TEXT),
    CAST(failure_code AS TEXT), CAST(failure_detail AS TEXT), CAST(phase AS TEXT),
    COALESCE(CAST(round(recorded_at * 1000) AS INTEGER), 0)
FROM library_scan_failures s WHERE typeof(run_id) = 'text' AND typeof(root_id) = 'text' AND typeof(relative_path) = 'text' AND typeof(failure_code) = 'text' AND typeof(phase) = 'text' AND failure_code IS NOT 'RUN_END_PENDING';
DROP TABLE library_scan_failures;
ALTER TABLE library_scan_failures_new RENAME TO library_scan_failures;
CREATE INDEX idx_scan_failures_run ON library_scan_failures(run_id);
CREATE INDEX idx_scan_failures_guard ON library_scan_failures(run_id, root_id)
    WHERE failure_code = 'MASS_MISSING_GUARD';

CREATE TABLE library_scan_album_moves_new (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    local_track_id TEXT NOT NULL,
    previous_album_id TEXT NOT NULL,
    PRIMARY KEY (run_id, root_id, relative_path)
) STRICT;
INSERT INTO library_scan_album_moves_new (rowid, run_id, root_id, relative_path, local_track_id, previous_album_id)
SELECT s.rowid, CAST(run_id AS TEXT), CAST(root_id AS TEXT), CAST(relative_path AS TEXT),
    CAST(local_track_id AS TEXT), CAST(previous_album_id AS TEXT)
FROM library_scan_album_moves s WHERE typeof(run_id) = 'text' AND typeof(root_id) = 'text' AND typeof(relative_path) = 'text';
DROP TABLE library_scan_album_moves;
ALTER TABLE library_scan_album_moves_new RENAME TO library_scan_album_moves;
CREATE INDEX idx_library_scan_album_moves_from
    ON library_scan_album_moves(run_id, previous_album_id);

CREATE TABLE library_identify_jobs_new (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('automatic','manual','historical')),
    priority INTEGER NOT NULL,
    state TEXT NOT NULL
        CHECK(state IN ('queued','running','deferred','succeeded','failed','attention')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    not_before_at INTEGER NOT NULL DEFAULT 0,
    lease_expires_at INTEGER,
    input_revision TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    failure_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_identify_jobs_new (rowid, id, local_album_id, kind, priority, state, attempts, not_before_at, lease_expires_at,
    input_revision, idempotency_key, requested_by_user_id, failure_code, created_at,
    updated_at)
SELECT s.rowid, CAST(id AS TEXT), CAST(local_album_id AS TEXT), CAST(kind AS TEXT),
    COALESCE(CAST(priority AS INTEGER), 0), CAST(state AS TEXT), CAST(attempts AS INTEGER),
    COALESCE(CAST(not_before_ms AS INTEGER), 0), CAST(lease_expires_ms AS INTEGER),
    CAST(input_revision AS TEXT), CAST(idempotency_key AS TEXT),
    (SELECT u.id FROM auth_users u WHERE u.id = s.requested_by_user_id),
    CAST(failure_code AS TEXT), COALESCE(CAST(created_ms AS INTEGER), 0),
    COALESCE(CAST(updated_ms AS INTEGER), 0)
FROM library_identify_jobs s WHERE typeof(id) = 'text';
DROP TABLE library_identify_jobs;
ALTER TABLE library_identify_jobs_new RENAME TO library_identify_jobs;
CREATE UNIQUE INDEX idx_identify_jobs_live_key
    ON library_identify_jobs(idempotency_key)
    WHERE state IN ('queued','running','deferred');
CREATE INDEX idx_identify_jobs_claim
    ON library_identify_jobs(state, priority, not_before_at);
CREATE INDEX idx_identify_jobs_album
    ON library_identify_jobs(local_album_id);
CREATE INDEX idx_library_identify_jobs_requested_by_user_id
    ON library_identify_jobs(requested_by_user_id);

CREATE TABLE library_identify_reviews_new (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    reason_code TEXT NOT NULL,
    candidates_json TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK(state IN ('pending','approved','rejected','dismissed','excluded')),
    resolved_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    selected_candidate_key TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
, row_revision INTEGER NOT NULL DEFAULT 1) STRICT;
INSERT INTO library_identify_reviews_new (rowid, id, local_album_id, reason_code, candidates_json, state, resolved_by_user_id,
    selected_candidate_key, created_at, updated_at, row_revision)
SELECT s.rowid, CAST(id AS TEXT), CAST(local_album_id AS TEXT), CAST(reason_code AS TEXT),
    CAST(candidates_json AS TEXT), CAST(state AS TEXT),
    (SELECT u.id FROM auth_users u WHERE u.id = s.resolved_by_user_id),
    CAST(selected_candidate_key AS TEXT), COALESCE(CAST(created_ms AS INTEGER), 0),
    COALESCE(CAST(updated_ms AS INTEGER), 0), CAST(row_revision AS INTEGER)
FROM library_identify_reviews s WHERE typeof(id) = 'text';
DROP TABLE library_identify_reviews;
ALTER TABLE library_identify_reviews_new RENAME TO library_identify_reviews;
CREATE INDEX idx_identify_reviews_album
    ON library_identify_reviews(local_album_id, state);
CREATE INDEX idx_identify_reviews_state
    ON library_identify_reviews(state, updated_at DESC);
CREATE TRIGGER library_identify_reviews_revision
AFTER UPDATE OF state, candidates_json, reason_code, local_album_id
ON library_identify_reviews
FOR EACH ROW
WHEN OLD.state IS NOT NEW.state
    OR OLD.candidates_json IS NOT NEW.candidates_json
    OR OLD.reason_code IS NOT NEW.reason_code
    OR OLD.local_album_id IS NOT NEW.local_album_id
BEGIN
    UPDATE library_identify_reviews
    SET row_revision = OLD.row_revision + 1
    WHERE id = NEW.id;
END;
CREATE INDEX idx_library_identify_reviews_resolved_by_user_id
    ON library_identify_reviews(resolved_by_user_id);

CREATE TABLE library_review_exclusions_new (
    review_id TEXT NOT NULL REFERENCES library_identify_reviews(id) ON DELETE CASCADE,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    PRIMARY KEY (review_id, local_track_id)
) STRICT;
INSERT INTO library_review_exclusions_new (rowid, review_id, local_track_id)
SELECT s.rowid, CAST(review_id AS TEXT), CAST(local_track_id AS TEXT)
FROM library_review_exclusions s WHERE typeof(review_id) = 'text' AND typeof(local_track_id) = 'text';
DROP TABLE library_review_exclusions;
ALTER TABLE library_review_exclusions_new RENAME TO library_review_exclusions;
CREATE INDEX idx_library_review_exclusions_local_track_id
    ON library_review_exclusions(local_track_id);

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
    state TEXT NOT NULL,
    terminal_reason_code TEXT NOT NULL,
    selected_candidate_key TEXT,
    candidate_count INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
    degradation_flags_json TEXT NOT NULL DEFAULT '[]',
    started_at INTEGER NOT NULL,
    completed_at INTEGER NOT NULL,
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
) STRICT;
INSERT INTO library_identification_attempts_new (rowid, id, local_album_id, local_track_id, trigger, requested_by_user_id, input_tag_revision,
    input_policy_revision, input_file_revision, input_identity_revision, matcher_version,
    state, terminal_reason_code, selected_candidate_key, candidate_count,
    degradation_flags_json, started_at, completed_at)
SELECT s.rowid, CAST(id AS TEXT), CAST(local_album_id AS TEXT), CAST(local_track_id AS TEXT),
    CAST(trigger AS TEXT), CAST(requested_by_user_id AS TEXT),
    CAST(input_tag_revision AS TEXT), CAST(input_policy_revision AS TEXT),
    CAST(input_file_revision AS TEXT), CAST(input_identity_revision AS TEXT),
    CAST(matcher_version AS TEXT), CAST(state AS TEXT), CAST(terminal_reason_code AS TEXT),
    CAST(selected_candidate_key AS TEXT), CAST(candidate_count AS INTEGER),
    CAST(degradation_flags_json AS TEXT),
    COALESCE(CAST(round(started_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(completed_at * 1000) AS INTEGER), 0)
FROM library_identification_attempts s WHERE typeof(id) = 'text';
DROP TABLE library_identification_attempts;
ALTER TABLE library_identification_attempts_new RENAME TO library_identification_attempts;
CREATE INDEX idx_identification_attempt_subject_album ON library_identification_attempts(local_album_id, completed_at);
CREATE INDEX idx_identification_attempt_subject_track ON library_identification_attempts(local_track_id, completed_at);
CREATE INDEX idx_library_identification_attempts_requested_by_user_id
    ON library_identification_attempts(requested_by_user_id);
CREATE TRIGGER trg_library_identification_attempts_immutable
BEFORE UPDATE ON library_identification_attempts
BEGIN SELECT RAISE(ABORT, 'identification attempts are immutable'); END;

CREATE TABLE library_reidentification_snapshots_new (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    expected_album_revision INTEGER NOT NULL,
    expected_input_revision TEXT NOT NULL,
    expected_identity_revision TEXT NOT NULL DEFAULT '',
    one_off_local_metadata INTEGER NOT NULL DEFAULT 0 CHECK(one_off_local_metadata IN (0,1)),
    requested_release_mbid TEXT,
    selected_candidate_key TEXT,
    result_json TEXT,
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_reidentification_snapshots_new (rowid, job_id, local_album_id, expected_album_revision, expected_input_revision,
    expected_identity_revision, one_off_local_metadata, requested_release_mbid,
    selected_candidate_key, result_json, created_at)
SELECT s.rowid, CAST(job_id AS TEXT), CAST(local_album_id AS TEXT),
    COALESCE(CAST(expected_album_revision AS INTEGER), 0),
    CAST(expected_input_revision AS TEXT), CAST(expected_identity_revision AS TEXT),
    CAST(one_off_local_metadata AS INTEGER), CAST(requested_release_mbid AS TEXT),
    CAST(selected_candidate_key AS TEXT), CAST(result_json AS TEXT),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0)
FROM library_reidentification_snapshots s WHERE typeof(job_id) = 'text';
DROP TABLE library_reidentification_snapshots;
ALTER TABLE library_reidentification_snapshots_new RENAME TO library_reidentification_snapshots;
CREATE INDEX idx_library_reidentification_snapshots_local_album_id
    ON library_reidentification_snapshots(local_album_id);

CREATE TABLE library_automatic_edition_undo_new (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL UNIQUE REFERENCES local_albums(id) ON DELETE CASCADE,
    job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE SET NULL,
    evidence_id TEXT,
    prior_identity_json TEXT,
    prior_track_identities_json TEXT NOT NULL DEFAULT '[]',
    expected_post_album_revision INTEGER NOT NULL
        CHECK(expected_post_album_revision BETWEEN 1 AND 9223372036854775807),
    expected_post_identity_revision INTEGER NOT NULL
        CHECK(expected_post_identity_revision BETWEEN 1 AND 9223372036854775807),
    reason_code TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    consumed_at INTEGER,
    consumed_action_id TEXT
) STRICT;
INSERT INTO library_automatic_edition_undo_new (rowid, id, local_album_id, job_id, evidence_id, prior_identity_json,
    prior_track_identities_json, expected_post_album_revision,
    expected_post_identity_revision, reason_code, created_at, consumed_at,
    consumed_action_id)
SELECT s.rowid, CAST(id AS TEXT), CAST(local_album_id AS TEXT), CAST(job_id AS TEXT),
    CAST(evidence_id AS TEXT), CAST(prior_identity_json AS TEXT),
    CAST(prior_track_identities_json AS TEXT),
    COALESCE(CAST(expected_post_album_revision AS INTEGER), 0),
    COALESCE(CAST(expected_post_identity_revision AS INTEGER), 0),
    CAST(reason_code AS TEXT), COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0),
    CAST(round(consumed_at * 1000) AS INTEGER), CAST(consumed_action_id AS TEXT)
FROM library_automatic_edition_undo s WHERE typeof(id) = 'text';
DROP TABLE library_automatic_edition_undo;
ALTER TABLE library_automatic_edition_undo_new RENAME TO library_automatic_edition_undo;
CREATE INDEX idx_library_automatic_edition_undo_job_id
    ON library_automatic_edition_undo(job_id);

CREATE TABLE library_album_match_state_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('unconfirmed', 'unmatched')),
    reason_code TEXT NOT NULL,
    release_mbid TEXT,
    candidates_json TEXT NOT NULL DEFAULT '[]',
    updated_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_album_match_state_new (rowid, local_album_id, state, reason_code, release_mbid, candidates_json, updated_at)
SELECT s.rowid, CAST(local_album_id AS TEXT), CAST(state AS TEXT), CAST(reason_code AS TEXT),
    CAST(release_mbid AS TEXT), CAST(candidates_json AS TEXT),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0)
FROM library_album_match_state s WHERE typeof(local_album_id) = 'text';
DROP TABLE library_album_match_state;
ALTER TABLE library_album_match_state_new RENAME TO library_album_match_state;
CREATE INDEX idx_album_match_state_state
    ON library_album_match_state(state, updated_at DESC);

CREATE TABLE library_custom_edition_manifests_new (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL CHECK(version > 0),
    release_group_mbid TEXT NOT NULL,
    album_title TEXT NOT NULL,
    album_artist_name TEXT NOT NULL,
    artist_mbid TEXT,
    album_metadata_json TEXT NOT NULL DEFAULT '{}',
    source_album_revision INTEGER NOT NULL CHECK(source_album_revision > 0),
    source_identity_revision INTEGER CHECK(source_identity_revision IS NULL OR source_identity_revision > 0),
    input_revision TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    selected_candidate_key TEXT,
    sealed_by_user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE RESTRICT,
    sealed_at INTEGER NOT NULL,
    UNIQUE(local_album_id, version),
    UNIQUE(local_album_id, content_hash)
) STRICT;
INSERT OR IGNORE INTO library_custom_edition_manifests_new (rowid, id, local_album_id, version, release_group_mbid, album_title, album_artist_name,
    artist_mbid, album_metadata_json, source_album_revision, source_identity_revision,
    input_revision, content_hash, selected_candidate_key, sealed_by_user_id, sealed_at)
SELECT s.rowid, CAST(id AS TEXT), CAST(local_album_id AS TEXT),
    MAX(1, COALESCE(CAST(version AS INTEGER), 0)), CAST(release_group_mbid AS TEXT),
    CAST(album_title AS TEXT), CAST(album_artist_name AS TEXT), CAST(artist_mbid AS TEXT),
    CAST(album_metadata_json AS TEXT),
    MAX(1, COALESCE(CAST(source_album_revision AS INTEGER), 0)),
    MAX(1, CAST(source_identity_revision AS INTEGER)), CAST(input_revision AS TEXT),
    CAST(content_hash AS TEXT), CAST(selected_candidate_key AS TEXT),
    CAST(sealed_by_user_id AS TEXT), COALESCE(CAST(round(sealed_at * 1000) AS INTEGER), 0)
FROM library_custom_edition_manifests s WHERE typeof(id) = 'text';
DROP TABLE library_custom_edition_manifests;
ALTER TABLE library_custom_edition_manifests_new RENAME TO library_custom_edition_manifests;
CREATE INDEX idx_custom_edition_album
ON library_custom_edition_manifests(local_album_id, version DESC);
CREATE INDEX idx_library_custom_edition_manifests_sealed_by_user_id
    ON library_custom_edition_manifests(sealed_by_user_id);
CREATE TRIGGER trg_custom_edition_manifest_immutable_update
BEFORE UPDATE ON library_custom_edition_manifests
BEGIN
    SELECT RAISE(ABORT, 'custom edition manifests are immutable');
END;
CREATE TRIGGER trg_custom_edition_manifest_immutable_delete
BEFORE DELETE ON library_custom_edition_manifests
BEGIN
    SELECT RAISE(ABORT, 'custom edition manifests are immutable');
END;

CREATE TABLE library_custom_edition_tracks_new (
    manifest_id TEXT NOT NULL REFERENCES library_custom_edition_manifests(id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    source_track_revision INTEGER NOT NULL CHECK(source_track_revision > 0),
    source_identity_revision INTEGER CHECK(source_identity_revision IS NULL OR source_identity_revision > 0),
    stat_revision TEXT NOT NULL,
    tag_revision TEXT NOT NULL,
    title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    album_artist_name TEXT NOT NULL,
    disc_number INTEGER NOT NULL CHECK(disc_number > 0),
    track_number INTEGER NOT NULL CHECK(track_number > 0),
    recording_mbid TEXT,
    artist_mbid TEXT,
    album_artist_mbid TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    file_format TEXT NOT NULL DEFAULT '',
    duration_seconds REAL,
    PRIMARY KEY(manifest_id, ordinal),
    UNIQUE(manifest_id, local_track_id),
    UNIQUE(manifest_id, disc_number, track_number)
) STRICT;
INSERT OR IGNORE INTO library_custom_edition_tracks_new (rowid, manifest_id, ordinal, local_track_id, source_track_revision, source_identity_revision,
    stat_revision, tag_revision, title, artist_name, album_title, album_artist_name,
    disc_number, track_number, recording_mbid, artist_mbid, album_artist_mbid,
    metadata_json, file_format, duration_seconds)
SELECT s.rowid, CAST(manifest_id AS TEXT), COALESCE(CAST(ordinal AS INTEGER), 0),
    CAST(local_track_id AS TEXT),
    MAX(1, COALESCE(CAST(source_track_revision AS INTEGER), 0)),
    MAX(1, CAST(source_identity_revision AS INTEGER)), CAST(stat_revision AS TEXT),
    CAST(tag_revision AS TEXT), CAST(title AS TEXT), CAST(artist_name AS TEXT),
    CAST(album_title AS TEXT), CAST(album_artist_name AS TEXT),
    MAX(1, COALESCE(CAST(disc_number AS INTEGER), 0)),
    MAX(1, COALESCE(CAST(track_number AS INTEGER), 0)), CAST(recording_mbid AS TEXT),
    CAST(artist_mbid AS TEXT), CAST(album_artist_mbid AS TEXT),
    CAST(metadata_json AS TEXT), CAST(file_format AS TEXT), CAST(duration_seconds AS REAL)
FROM library_custom_edition_tracks s WHERE typeof(manifest_id) = 'text' AND typeof(ordinal) = 'integer' AND manifest_id IN (SELECT id FROM library_custom_edition_manifests);
DROP TABLE library_custom_edition_tracks;
ALTER TABLE library_custom_edition_tracks_new RENAME TO library_custom_edition_tracks;
CREATE INDEX idx_custom_edition_track_local
ON library_custom_edition_tracks(local_track_id);
CREATE TRIGGER trg_custom_edition_tracks_immutable_update
BEFORE UPDATE ON library_custom_edition_tracks
BEGIN
    SELECT RAISE(ABORT, 'custom edition tracks are immutable');
END;
CREATE TRIGGER trg_custom_edition_tracks_immutable_delete
BEFORE DELETE ON library_custom_edition_tracks
BEGIN
    SELECT RAISE(ABORT, 'custom edition tracks are immutable');
END;

CREATE TABLE library_custom_edition_active_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    manifest_id TEXT NOT NULL UNIQUE REFERENCES library_custom_edition_manifests(id) ON DELETE RESTRICT,
    activated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision > 0)
) STRICT;
INSERT INTO library_custom_edition_active_new (rowid, local_album_id, manifest_id, activated_at, row_revision)
SELECT s.rowid, CAST(local_album_id AS TEXT), CAST(manifest_id AS TEXT),
    COALESCE(CAST(round(activated_at * 1000) AS INTEGER), 0),
    MAX(1, CAST(row_revision AS INTEGER))
FROM library_custom_edition_active s WHERE typeof(local_album_id) = 'text' AND manifest_id IN (SELECT id FROM library_custom_edition_manifests);
DROP TABLE library_custom_edition_active;
ALTER TABLE library_custom_edition_active_new RENAME TO library_custom_edition_active;

CREATE TABLE library_edition_choice_undo_new (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL UNIQUE REFERENCES local_albums(id) ON DELETE CASCADE,
    action_id TEXT NOT NULL,
    prior_identity_json TEXT,
    prior_tracks_json TEXT NOT NULL DEFAULT '[]',
    prior_match_state_json TEXT,
    expected_identity_revision INTEGER NOT NULL,
    created_at INTEGER NOT NULL
, prior_hold_json TEXT) STRICT;
INSERT INTO library_edition_choice_undo_new (rowid, id, local_album_id, action_id, prior_identity_json, prior_tracks_json,
    prior_match_state_json, expected_identity_revision, created_at, prior_hold_json)
SELECT s.rowid, CAST(id AS TEXT), CAST(local_album_id AS TEXT), CAST(action_id AS TEXT),
    CAST(prior_identity_json AS TEXT), CAST(prior_tracks_json AS TEXT),
    CAST(prior_match_state_json AS TEXT),
    COALESCE(CAST(expected_identity_revision AS INTEGER), 0),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0), CAST(prior_hold_json AS TEXT)
FROM library_edition_choice_undo s WHERE typeof(id) = 'text';
DROP TABLE library_edition_choice_undo;
ALTER TABLE library_edition_choice_undo_new RENAME TO library_edition_choice_undo;

CREATE TABLE library_edition_remap_queue_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    release_mbid TEXT NOT NULL,
    chosen_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    queued_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    not_before_at INTEGER NOT NULL DEFAULT 0,
    last_code TEXT,
    -- 'album_pin': a person chose this album's edition; 'group_pin': the
    -- album got the choice from a release-group pin covering every copy;
    -- 'place_only': the album has its edition, only its files are placed.
    origin TEXT NOT NULL DEFAULT 'album_pin'
        CHECK(origin IN ('album_pin', 'group_pin', 'place_only'))
) STRICT;
INSERT INTO library_edition_remap_queue_new (rowid, local_album_id, release_mbid, chosen_by_user_id, queued_at, attempts, not_before_at,
    last_code, origin)
SELECT s.rowid, CAST(local_album_id AS TEXT), CAST(release_mbid AS TEXT),
    (SELECT u.id FROM auth_users u WHERE u.id = s.chosen_by_user_id),
    COALESCE(CAST(round(queued_at * 1000) AS INTEGER), 0), CAST(attempts AS INTEGER),
    COALESCE(CAST(round(not_before * 1000) AS INTEGER), 0), CAST(last_code AS TEXT),
    CAST(origin AS TEXT)
FROM library_edition_remap_queue s WHERE typeof(local_album_id) = 'text';
DROP TABLE library_edition_remap_queue;
ALTER TABLE library_edition_remap_queue_new RENAME TO library_edition_remap_queue;
CREATE INDEX idx_library_edition_remap_queue_chosen_by_user_id
    ON library_edition_remap_queue(chosen_by_user_id);

CREATE TABLE library_contribution_drafts_new (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    created_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    updated_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    state TEXT NOT NULL CHECK(state IN (
        'draft', 'ready', 'seeded', 'verifying', 'linked',
        'needs_review', 'stale', 'cancelled'
    )),
    album_row_revision INTEGER NOT NULL,
    input_revision TEXT NOT NULL,
    local_snapshot_json TEXT NOT NULL,
    resolved_draft_json TEXT NOT NULL,
    source_selection_json TEXT NOT NULL,
    provider_snapshot_expires_at INTEGER,
    duplicate_result_json TEXT,
    duplicate_checked_at INTEGER,
    duplicate_input_revision TEXT,
    result_release_mbid TEXT,
    result_source TEXT CHECK(result_source IN ('callback', 'manual') OR result_source IS NULL),
    result_received_at INTEGER,
    seed_snapshot_json TEXT,
    seed_hash TEXT,
    seeded_at INTEGER,
    terminal_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
, last_verification_failure TEXT) STRICT;
INSERT INTO library_contribution_drafts_new (rowid, id, local_album_id, created_by_user_id, updated_by_user_id, state, album_row_revision,
    input_revision, local_snapshot_json, resolved_draft_json, source_selection_json,
    provider_snapshot_expires_at, duplicate_result_json, duplicate_checked_at,
    duplicate_input_revision, result_release_mbid, result_source, result_received_at,
    seed_snapshot_json, seed_hash, seeded_at, terminal_at, created_at, updated_at,
    row_revision, last_verification_failure)
SELECT s.rowid, CAST(id AS TEXT), CAST(local_album_id AS TEXT), CAST(created_by_user_id AS TEXT),
    CAST(updated_by_user_id AS TEXT), CAST(state AS TEXT),
    COALESCE(CAST(album_row_revision AS INTEGER), 0), CAST(input_revision AS TEXT),
    CAST(local_snapshot_json AS TEXT), CAST(resolved_draft_json AS TEXT),
    CAST(source_selection_json AS TEXT),
    CAST(round(provider_snapshot_expires_at * 1000) AS INTEGER),
    CAST(duplicate_result_json AS TEXT),
    CAST(round(duplicate_checked_at * 1000) AS INTEGER),
    CAST(duplicate_input_revision AS TEXT), CAST(result_release_mbid AS TEXT),
    CAST(result_source AS TEXT), CAST(round(result_received_at * 1000) AS INTEGER),
    CAST(seed_snapshot_json AS TEXT), CAST(seed_hash AS TEXT),
    CAST(round(seeded_at * 1000) AS INTEGER), CAST(round(terminal_at * 1000) AS INTEGER),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0), CAST(row_revision AS INTEGER),
    CAST(last_verification_failure AS TEXT)
FROM library_contribution_drafts s WHERE typeof(id) = 'text';
DROP TABLE library_contribution_drafts;
ALTER TABLE library_contribution_drafts_new RENAME TO library_contribution_drafts;
CREATE UNIQUE INDEX idx_contribution_active_album
ON library_contribution_drafts(local_album_id)
WHERE state NOT IN ('linked', 'cancelled', 'stale');
CREATE INDEX idx_contribution_album_updated
ON library_contribution_drafts(local_album_id, updated_at DESC);
CREATE INDEX idx_library_contribution_drafts_updated_by_user_id
    ON library_contribution_drafts(updated_by_user_id);
CREATE INDEX idx_library_contribution_drafts_created_by_user_id
    ON library_contribution_drafts(created_by_user_id);

CREATE TABLE library_contribution_callback_tokens_new (
    token_hash TEXT PRIMARY KEY,
    contribution_id TEXT NOT NULL
        REFERENCES library_contribution_drafts(id) ON DELETE CASCADE,
    requested_by_user_id TEXT NOT NULL
        REFERENCES auth_users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER,
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_contribution_callback_tokens_new (rowid, token_hash, contribution_id, requested_by_user_id, expires_at, consumed_at, created_at)
SELECT s.rowid, CAST(token_hash AS TEXT), CAST(contribution_id AS TEXT),
    CAST(requested_by_user_id AS TEXT),
    COALESCE(CAST(round(expires_at * 1000) AS INTEGER), 0),
    CAST(round(consumed_at * 1000) AS INTEGER),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0)
FROM library_contribution_callback_tokens s WHERE typeof(token_hash) = 'text' AND typeof(contribution_id) = 'text';
DROP TABLE library_contribution_callback_tokens;
ALTER TABLE library_contribution_callback_tokens_new RENAME TO library_contribution_callback_tokens;
CREATE INDEX idx_contribution_callback_expiry
ON library_contribution_callback_tokens(expires_at, consumed_at);
CREATE INDEX idx_library_contribution_callback_tokens_requested_by_user_id
    ON library_contribution_callback_tokens(requested_by_user_id);
CREATE INDEX idx_library_contribution_callback_tokens_contribution_id
    ON library_contribution_callback_tokens(contribution_id);

CREATE TABLE library_repair_materialization_new (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    pinned_catalog_revision INTEGER NOT NULL
        CHECK(pinned_catalog_revision BETWEEN 0 AND 9223372036854775807),
    eligibility_version TEXT NOT NULL CHECK(length(trim(eligibility_version)) > 0),
    purpose TEXT NOT NULL CHECK(length(trim(purpose)) > 0),
    staging_cursor TEXT,
    staged_ordinal INTEGER NOT NULL DEFAULT -1 CHECK(staged_ordinal >= -1),
    staged_count INTEGER NOT NULL DEFAULT 0 CHECK(staged_count >= 0),
    sealed INTEGER NOT NULL DEFAULT 0 CHECK(sealed IN (0,1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_repair_materialization_new (rowid, job_id, pinned_catalog_revision, eligibility_version, purpose, staging_cursor,
    staged_ordinal, staged_count, sealed, created_at, updated_at)
SELECT s.rowid, CAST(job_id AS TEXT), COALESCE(CAST(pinned_catalog_revision AS INTEGER), 0),
    CAST(eligibility_version AS TEXT), CAST(purpose AS TEXT), CAST(staging_cursor AS TEXT),
    CAST(staged_ordinal AS INTEGER), CAST(staged_count AS INTEGER),
    CAST(sealed AS INTEGER), COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0)
FROM library_repair_materialization s WHERE typeof(job_id) = 'text';
DROP TABLE library_repair_materialization;
ALTER TABLE library_repair_materialization_new RENAME TO library_repair_materialization;

CREATE TABLE library_repair_snapshots_new (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    scope_json TEXT NOT NULL,
    source_matcher_version TEXT,
    target_matcher_version TEXT NOT NULL,
    phase TEXT NOT NULL DEFAULT 'dry_run' CHECK(phase IN ('dry_run','apply')),
    result_json TEXT,
    created_at INTEGER NOT NULL
, purpose TEXT
    CHECK (purpose IN ('existing_matches', 'management_readiness',
                       'catalog_identity_hygiene', 'play_history_filing'))) STRICT;
INSERT INTO library_repair_snapshots_new (rowid, job_id, scope_json, source_matcher_version, target_matcher_version, phase, result_json,
    created_at, purpose)
SELECT s.rowid, CAST(job_id AS TEXT), CAST(scope_json AS TEXT), CAST(source_matcher_version AS TEXT),
    CAST(target_matcher_version AS TEXT), CAST(phase AS TEXT), CAST(result_json AS TEXT),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0), CAST(purpose AS TEXT)
FROM library_repair_snapshots s WHERE typeof(job_id) = 'text';
DROP TABLE library_repair_snapshots;
ALTER TABLE library_repair_snapshots_new RENAME TO library_repair_snapshots;

CREATE TABLE library_identity_repair_findings_new (
    id TEXT NOT NULL PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    expected_album_revision INTEGER NOT NULL,
    expected_identity_revision INTEGER,
    finding_code TEXT NOT NULL,
    confidence TEXT NOT NULL,
    reason_code TEXT NOT NULL DEFAULT '',
    apply_eligible INTEGER NOT NULL DEFAULT 0 CHECK(apply_eligible IN (0,1)),
    apply_result TEXT,
    suggested_release_mbid TEXT,
    suggested_release_group_mbid TEXT,
    suggested_edition_json TEXT NOT NULL DEFAULT '{}',
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','applied','skipped','stale')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(job_id, local_album_id, finding_code)
) STRICT;
INSERT INTO library_identity_repair_findings_new (rowid, id, job_id, local_album_id, expected_album_revision, expected_identity_revision,
    finding_code, confidence, reason_code, apply_eligible, apply_result,
    suggested_release_mbid, suggested_release_group_mbid, suggested_edition_json, state,
    created_at, updated_at, row_revision)
SELECT s.rowid, CAST(id AS TEXT), CAST(job_id AS TEXT), CAST(local_album_id AS TEXT),
    COALESCE(CAST(expected_album_revision AS INTEGER), 0),
    CAST(expected_identity_revision AS INTEGER), CAST(finding_code AS TEXT),
    CAST(confidence AS TEXT), CAST(reason_code AS TEXT), CAST(apply_eligible AS INTEGER),
    CAST(apply_result AS TEXT), CAST(suggested_release_mbid AS TEXT),
    CAST(suggested_release_group_mbid AS TEXT), CAST(suggested_edition_json AS TEXT),
    CAST(state AS TEXT), COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0), CAST(row_revision AS INTEGER)
FROM library_identity_repair_findings s WHERE typeof(id) = 'text';
DROP TABLE library_identity_repair_findings;
ALTER TABLE library_identity_repair_findings_new RENAME TO library_identity_repair_findings;
CREATE INDEX idx_repair_findings_cursor
    ON library_identity_repair_findings(job_id, finding_code, updated_at DESC, id DESC);
CREATE INDEX idx_repair_findings_album
    ON library_identity_repair_findings(local_album_id);

CREATE TABLE library_catalog_actions_new (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT UNIQUE,
    actor_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    action_kind TEXT NOT NULL,
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
INSERT INTO library_catalog_actions_new (rowid, id, idempotency_key, actor_user_id, action_kind, local_artist_id, local_album_id,
    local_track_id, operation_job_id, before_json, after_json, reason_code, created_at)
SELECT s.rowid, CAST(id AS TEXT), CAST(idempotency_key AS TEXT), CAST(actor_user_id AS TEXT),
    CAST(action_kind AS TEXT), CAST(local_artist_id AS TEXT), CAST(local_album_id AS TEXT),
    CAST(local_track_id AS TEXT), CAST(operation_job_id AS TEXT),
    CASE WHEN action_kind = 'files_put_back' AND NOT json_valid(before_json) THEN '{}' ELSE CAST(before_json AS TEXT) END,
    CAST(after_json AS TEXT), CAST(reason_code AS TEXT),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0)
FROM library_catalog_actions s WHERE typeof(id) = 'text';
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
-- What a put-back undid, for the lookups that ask whether a removal is spent.
CREATE INDEX idx_library_catalog_actions_undoes
    ON library_catalog_actions(json_extract(before_json, '$.undoes'))
    WHERE action_kind = 'files_put_back';

CREATE TABLE library_artist_reconciliation_dismissals_new (
    left_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    right_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    left_artist_revision INTEGER NOT NULL
        CHECK(left_artist_revision BETWEEN 1 AND 9223372036854775807),
    right_artist_revision INTEGER NOT NULL
        CHECK(right_artist_revision BETWEEN 1 AND 9223372036854775807),
    dismissed_by_user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE RESTRICT,
    reason_code TEXT NOT NULL DEFAULT 'MARKED_DISTINCT',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(left_artist_id, right_artist_id),
    CHECK(left_artist_id < right_artist_id)
) STRICT;
INSERT INTO library_artist_reconciliation_dismissals_new (rowid, left_artist_id, right_artist_id, left_artist_revision, right_artist_revision,
    dismissed_by_user_id, reason_code, created_at, updated_at, row_revision)
SELECT s.rowid, CAST(left_artist_id AS TEXT), CAST(right_artist_id AS TEXT),
    COALESCE(CAST(left_artist_revision AS INTEGER), 0),
    COALESCE(CAST(right_artist_revision AS INTEGER), 0),
    CAST(dismissed_by_user_id AS TEXT), CAST(reason_code AS TEXT),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0), CAST(row_revision AS INTEGER)
FROM library_artist_reconciliation_dismissals s WHERE typeof(left_artist_id) = 'text' AND typeof(right_artist_id) = 'text';
DROP TABLE library_artist_reconciliation_dismissals;
ALTER TABLE library_artist_reconciliation_dismissals_new RENAME TO library_artist_reconciliation_dismissals;
CREATE INDEX idx_artist_reconciliation_dismissal_user
ON library_artist_reconciliation_dismissals(dismissed_by_user_id, updated_at DESC);
CREATE INDEX idx_library_artist_reconciliation_dismissals_right_artist_id
    ON library_artist_reconciliation_dismissals(right_artist_id);

CREATE TABLE local_artist_merge_candidates_new (
    id TEXT PRIMARY KEY,
    left_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    right_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    reason_code TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','resolved','dismissed')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    CHECK(left_artist_id != right_artist_id),
    UNIQUE(left_artist_id, right_artist_id, reason_code)
) STRICT;
INSERT INTO local_artist_merge_candidates_new (rowid, id, left_artist_id, right_artist_id, reason_code, state, created_at, updated_at,
    row_revision)
SELECT s.rowid, CAST(id AS TEXT), CAST(left_artist_id AS TEXT), CAST(right_artist_id AS TEXT),
    CAST(reason_code AS TEXT), CAST(state AS TEXT),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0), CAST(row_revision AS INTEGER)
FROM local_artist_merge_candidates s WHERE typeof(id) = 'text';
DROP TABLE local_artist_merge_candidates;
ALTER TABLE local_artist_merge_candidates_new RENAME TO local_artist_merge_candidates;
CREATE INDEX idx_local_artist_merge_candidates_right_artist_id
    ON local_artist_merge_candidates(right_artist_id);

CREATE TABLE library_audio_fingerprint_outcomes_new (
    id TEXT PRIMARY KEY,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    stat_revision TEXT NOT NULL,
    fingerprinter_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('matched','no_match','failed','disabled','skipped','deferred')),
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
INSERT INTO library_audio_fingerprint_outcomes_new (rowid, id, local_track_id, stat_revision, fingerprinter_version, state, fingerprint,
    duration_seconds, recording_mbid, release_group_ids_json, partial_decode, score,
    failure_code, attempt_count, first_attempt_at, last_attempt_at, retry_at, row_revision)
SELECT s.rowid, CAST(id AS TEXT), CAST(local_track_id AS TEXT), CAST(stat_revision AS TEXT),
    CAST(fingerprinter_version AS TEXT), CAST(state AS TEXT), CAST(fingerprint AS TEXT),
    CAST(duration_seconds AS REAL), CAST(recording_mbid AS TEXT),
    CAST(release_group_ids_json AS TEXT), CAST(partial_decode AS INTEGER),
    CAST(score AS REAL), CAST(failure_code AS TEXT),
    MAX(1, COALESCE(CAST(attempt_count AS INTEGER), 0)),
    COALESCE(CAST(round(first_attempt_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(last_attempt_at * 1000) AS INTEGER), 0),
    CAST(round(retry_after * 1000) AS INTEGER), CAST(row_revision AS INTEGER)
FROM audio_fingerprint_outcomes s WHERE typeof(id) = 'text';
DROP TABLE audio_fingerprint_outcomes;
ALTER TABLE library_audio_fingerprint_outcomes_new RENAME TO library_audio_fingerprint_outcomes;

CREATE TABLE library_track_management_identity_new (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    applied_album_identity_revision INTEGER,
    applied_track_identity_revision INTEGER
) STRICT;
INSERT INTO library_track_management_identity_new (rowid, local_track_id, applied_album_identity_revision, applied_track_identity_revision)
SELECT s.rowid, CAST(local_track_id AS TEXT), CAST(applied_album_identity_revision AS INTEGER),
    CAST(applied_track_identity_revision AS INTEGER)
FROM library_track_management_identity s WHERE typeof(local_track_id) = 'text';
DROP TABLE library_track_management_identity;
ALTER TABLE library_track_management_identity_new RENAME TO library_track_management_identity;

CREATE TABLE library_track_management_state_new (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE RESTRICT,
    baseline_id TEXT REFERENCES library_management_baselines(id) ON DELETE RESTRICT,
    applied_profile_id TEXT,
    applied_profile_revision TEXT,
    applied_projection_hash TEXT,
    applied_naming_script_revision TEXT,
    applied_override_revision TEXT,
    last_operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
    managed_root_id TEXT,
    managed_path_revision TEXT,
    last_managed_at INTEGER,
    last_outcome TEXT,
    last_reason_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
, written_sha256 TEXT) STRICT;
INSERT INTO library_track_management_state_new (rowid, local_track_id, baseline_id, applied_profile_id, applied_profile_revision,
    applied_projection_hash, applied_naming_script_revision, applied_override_revision,
    last_operation_job_id, managed_root_id, managed_path_revision, last_managed_at,
    last_outcome, last_reason_code, row_revision, written_sha256)
SELECT s.rowid, CAST(local_track_id AS TEXT), CAST(baseline_id AS TEXT),
    CAST(applied_profile_id AS TEXT), CAST(applied_profile_revision AS TEXT),
    CAST(applied_projection_hash AS TEXT), CAST(applied_naming_script_revision AS TEXT),
    CAST(applied_override_revision AS TEXT), CAST(last_operation_job_id AS TEXT),
    CAST(managed_root_id AS TEXT), CAST(managed_path_revision AS TEXT),
    CAST(round(last_managed_at * 1000) AS INTEGER), CAST(last_outcome AS TEXT),
    CAST(last_reason_code AS TEXT), CAST(row_revision AS INTEGER),
    CAST(written_sha256 AS TEXT)
FROM library_track_management_state s WHERE typeof(local_track_id) = 'text';
DROP TABLE library_track_management_state;
ALTER TABLE library_track_management_state_new RENAME TO library_track_management_state;
CREATE INDEX idx_library_track_management_state_last_operation_job_id
    ON library_track_management_state(last_operation_job_id);
CREATE INDEX idx_library_track_management_state_baseline_id
    ON library_track_management_state(baseline_id);

INSERT INTO library_rebuild_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3) AS now
    WHERE now.n > (SELECT COUNT(*) FROM library_rebuild_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent));

DROP TABLE library_rebuild_guard;
DROP TABLE library_rebuild_orphans;

-- Migration high-water mark.
PRAGMA user_version = 117;
COMMIT;
PRAGMA foreign_keys=ON;
