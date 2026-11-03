-- 0126_looks_wrong_and_album_needs_check.sql - "Looks wrong" flags, and a
-- held reason of its own for a better copy waiting on an album's match.
--
--   library_album_looks_wrong (new)
--       one row per signed-in user who marked a library album's match as
--       looking wrong. Curators list the albums with a row; a person's
--       choice or confirmation of the album's edition clears them.
--
--   acquire_held_files (rebuilt)
--       its reason CHECK gains album_needs_check: a better copy held
--       because the album it would replace a file on has a match a person
--       should check first. Every row, index and trigger is carried; a row
--       held for that reason before, as owned_on_album with the sentence
--       "This album's match needs checking first.", takes the new reason.
--
--   library_identify_reviews (two columns added)
--       reopen_review_revision and reopen_identity_revision: when a person
--       closes a review with Keep my tags or Decide later, the review's
--       revision and its album's match row revision are noted, and a reopen
--       is allowed only while both are unchanged. NULL everywhere else.
--
-- Nothing references acquire_held_files, so dropping the old table takes
-- no other rows with it.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_album_looks_wrong (
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (local_album_id, user_id)
) STRICT;

CREATE INDEX idx_library_album_looks_wrong_user ON library_album_looks_wrong(user_id);

ALTER TABLE library_identify_reviews ADD COLUMN reopen_review_revision INTEGER;
ALTER TABLE library_identify_reviews ADD COLUMN reopen_identity_revision INTEGER;

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
        'owned_elsewhere', 'owned_on_album', 'album_needs_check',
        'no_album_match', 'album_not_found', 'tags_disagree', 'not_chosen_edition',
        'samples', 'too_many_files', 'interrupted', 'import_failed',
        'held_by_earlier_version', 'format_not_listed', 'target_occupied', 'upgrade_blocked',
        'library_changed', 'library_unavailable', 'naming_script_failed',
        'library_folder_missing', 'library_off', 'local_fault', 'files_missing', 'files_unlisted',
        'outside_downloads_folder', 'size_mismatch', 'corrupt', 'no_audio', 'sample',
        'wrong_edition', 'not_pinned_edition', 'wrong_album', 'quality_rejected',
        'not_an_upgrade', 'wrong_track', 'stalled', 'queue_timeout', 'queue_gave_way',
        'never_started',
        'deadline', 'source_failed', 'no_source', 'download_failed',
        'album_lookup_unavailable', 'recording_unknown', 'no_album_for_recording',
        'no_album_folder', 'no_album_release', 'album_releases_exhausted',
        'track_not_found', 'nothing_allowed', 'no_download_source', 'settings_unreadable',
        'journal_unreadable', 'musicbrainz_unavailable', 'acoustid_unavailable',
        'held_for_review',
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
    reason, reason_detail,
    state, created_at, resolved_at
FROM acquire_held_files;

DROP TABLE acquire_held_files;
ALTER TABLE acquire_held_files_new RENAME TO acquire_held_files;

-- Better copies held on a flagged album before this release carried the
-- reason owned_on_album with this sentence as their detail; they take the
-- reason of their own.
UPDATE acquire_held_files SET reason = 'album_needs_check', reason_detail = NULL
WHERE reason = 'owned_on_album' AND reason_detail = 'This album''s match needs checking first.';

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

-- Migration high-water mark.
PRAGMA user_version = 126;
