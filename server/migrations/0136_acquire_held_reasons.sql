-- acquire_held_files.reason lists only the reasons a held file can carry
-- (Reason::HELD in acquire/src/reason.rs), not every download reason, so a
-- reason only a download task carries needs no schema change. No table
-- points at acquire_held_files, so it is rebuilt inside the runner's
-- transaction; its ids (the rowid), indexes and triggers come back as they
-- were.

-- No writer stores a reason outside the list. A row that has one anyway
-- keeps its file and reads as held by an earlier version, as the v2 import
-- reads a reason it does not know: the reason is a label, and the start
-- never stops on it.

CREATE TABLE acquire_held_files_new (
    id INTEGER PRIMARY KEY,
    entry_id INTEGER NOT NULL REFERENCES acquire_held_entries(id) ON DELETE CASCADE,
    root TEXT NOT NULL CHECK (root IN ('held', 'drop')),
    path TEXT NOT NULL,
    original_name TEXT NOT NULL,
    release_mbid TEXT,
    release_track_mbid TEXT,
    recording_mbid TEXT,
    disc_number INTEGER,
    track_number INTEGER,
    track_title TEXT,
    file_format TEXT,
    duration_seconds REAL,
    expected_duration_seconds REAL,
    evidence_title TEXT,
    evidence_artist TEXT,
    evidence_score REAL,
    source TEXT NOT NULL,
    source_task_id TEXT,
    reason TEXT NOT NULL CHECK (reason IN (
        'weak_match', 'no_artist_evidence', 'mostly_unmatched', 'tag_mismatch',
        'fingerprint_mismatch', 'fingerprint_unconfirmed', 'album_version_only',
        'no_release', 'no_tracks', 'version_lower_quality', 'no_better_than_yours',
        'owned_elsewhere', 'owned_on_album', 'album_needs_check', 'no_album_match',
        'album_not_found', 'tags_disagree', 'not_chosen_edition', 'samples',
        'too_many_files', 'interrupted', 'import_failed', 'held_by_earlier_version',
        'format_not_listed', 'target_occupied', 'upgrade_blocked', 'library_changed',
        'library_unavailable', 'naming_script_failed', 'library_folder_missing',
        'library_off', 'local_fault', 'musicbrainz_unavailable', 'acoustid_unavailable')),
    reason_detail TEXT,
    state TEXT NOT NULL CHECK (state IN ('held', 'imported', 'already_owned', 'discarded')),
    created_at INTEGER NOT NULL,
    resolved_at INTEGER
) STRICT;

INSERT INTO acquire_held_files_new (
    id, entry_id, root, path, original_name, release_mbid, release_track_mbid,
    recording_mbid, disc_number, track_number, track_title, file_format, duration_seconds,
    expected_duration_seconds, evidence_title, evidence_artist, evidence_score, source,
    source_task_id, reason, reason_detail, state, created_at, resolved_at)
SELECT
    id, entry_id, root, path, original_name, release_mbid, release_track_mbid,
    recording_mbid, disc_number, track_number, track_title, file_format, duration_seconds,
    expected_duration_seconds, evidence_title, evidence_artist, evidence_score, source,
    source_task_id,
    CASE WHEN reason IN ('weak_match', 'no_artist_evidence', 'mostly_unmatched', 'tag_mismatch',
        'fingerprint_mismatch', 'fingerprint_unconfirmed', 'album_version_only',
        'no_release', 'no_tracks', 'version_lower_quality', 'no_better_than_yours',
        'owned_elsewhere', 'owned_on_album', 'album_needs_check', 'no_album_match',
        'album_not_found', 'tags_disagree', 'not_chosen_edition', 'samples',
        'too_many_files', 'interrupted', 'import_failed', 'held_by_earlier_version',
        'format_not_listed', 'target_occupied', 'upgrade_blocked', 'library_changed',
        'library_unavailable', 'naming_script_failed', 'library_folder_missing',
        'library_off', 'local_fault', 'musicbrainz_unavailable', 'acoustid_unavailable')
        THEN reason ELSE 'held_by_earlier_version' END,
    reason_detail, state, created_at, resolved_at
FROM acquire_held_files;

DROP TABLE acquire_held_files;

ALTER TABLE acquire_held_files_new RENAME TO acquire_held_files;

CREATE INDEX idx_acquire_held_files_entry ON acquire_held_files(entry_id, state);

CREATE INDEX idx_acquire_held_files_task ON acquire_held_files(source_task_id, state);

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

PRAGMA user_version = 136;
