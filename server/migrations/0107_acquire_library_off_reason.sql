-- 0107_acquire_library_off_reason.sql - files that wait while the library
-- is switched off say so.
--
-- A download, upload or held file the library refused while its switch was
-- off was stored as library_folder_missing with the detail "the library is
-- switched off", and its sentence blamed a missing folder. It now has its
-- own reason, library_off.
--
--   acquire_held_files (rebuilt)
--       its reason CHECK gains library_off, so it still lists every reason
--       the acquire crate has. A file held as library_folder_missing with
--       the library-off detail becomes library_off, without the detail.
--       Every row, index and trigger is carried.
--   drop_import_items, download_import_decisions, download_tasks
--       the same rows move to library_off: an item whose sentence ends in
--       the library-off detail takes the new sentence, a decision with that
--       detail and a task whose outcome text is the library's "switched
--       off" sentence take the new spelling.
--
-- Nothing references acquire_held_files, so dropping the old table takes
-- no other rows with it.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

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
        'owned_elsewhere', 'owned_on_album',
        'no_album_match', 'album_not_found', 'tags_disagree', 'not_chosen_edition',
        'samples', 'too_many_files', 'interrupted', 'import_failed',
        'held_by_earlier_version', 'target_occupied', 'upgrade_blocked',
        'library_changed', 'library_unavailable', 'naming_script_failed',
        'library_folder_missing', 'library_off', 'local_fault', 'files_missing', 'files_unlisted',
        'outside_downloads_folder', 'size_mismatch', 'corrupt', 'no_audio', 'sample',
        'wrong_edition', 'not_pinned_edition', 'wrong_album', 'quality_rejected',
        'not_an_upgrade', 'wrong_track', 'stalled', 'queue_timeout', 'queue_gave_way',
        'never_started',
        'deadline', 'source_failed', 'no_source', 'download_failed',
        'album_lookup_unavailable', 'recording_unknown', 'no_album_for_recording',
        'no_album_folder', 'no_album_release', 'album_releases_exhausted',
        'track_not_found', 'no_download_source', 'settings_unreadable',
        'journal_unreadable', 'musicbrainz_unavailable', 'held_for_review',
        'tracks_missing', 'already_owned', 'cancelled', 'moved_on')),
    reason_detail TEXT,
    state TEXT NOT NULL CHECK (state IN ('held', 'imported', 'already_owned', 'discarded')),
    created_at INTEGER NOT NULL,
    resolved_at INTEGER
) STRICT;

INSERT INTO acquire_held_files_new (id, entry_id, root, path, original_name, release_mbid,
    release_track_mbid, recording_mbid, disc_number, track_number, track_title, file_format,
    duration_seconds, expected_duration_seconds, evidence_title, evidence_artist,
    evidence_score, source, source_task_id, reason, reason_detail, state, created_at,
    resolved_at)
SELECT id, entry_id, root, path, original_name, release_mbid, release_track_mbid,
    recording_mbid, disc_number, track_number, track_title, file_format, duration_seconds,
    expected_duration_seconds, evidence_title, evidence_artist, evidence_score, source,
    source_task_id,
    CASE WHEN reason = 'library_folder_missing' AND reason_detail = 'the library is switched off'
        THEN 'library_off' ELSE reason END,
    CASE WHEN reason = 'library_folder_missing' AND reason_detail = 'the library is switched off'
        THEN NULL ELSE reason_detail END,
    state, created_at, resolved_at
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

UPDATE drop_import_items
SET reason_code = 'library_off',
    detail = 'The local library is switched off, so the files wait where they are.'
        || substr(detail, length('The library folder isn''t there, so the files wait where they are. the library is switched off') + 1)
WHERE reason_code = 'library_folder_missing'
  AND substr(detail, 1, length('The library folder isn''t there, so the files wait where they are. the library is switched off'))
      = 'The library folder isn''t there, so the files wait where they are. the library is switched off';

UPDATE download_import_decisions
SET reason_code = 'library_off'
WHERE reason_code = 'library_folder_missing' AND detail = 'the library is switched off';

UPDATE download_tasks
SET outcome_code = 'library_off',
    error_message = 'The local library is switched off, so the files wait where they are.'
WHERE outcome_code = 'library_folder_missing'
  AND error_message = 'The local library is switched off, so no files can be changed. Turn it on under Settings > Library.';

-- Migration high-water mark.
PRAGMA user_version = 107;
