-- 0085_acquire_held_entries.sql - one list of files that need a decision.
--
-- Every file kept out of the library for a person, a download's or an
-- upload's, is one row of acquire_held_files under one entry of
-- acquire_held_entries. An entry is one download or one upload: a
-- 53-track album held from two candidates is one entry with 53 files.
--
--   acquire_held_entries (new)
--       group_key: what the entry is about, written by one rule:
--       upload:<drop item>, track:<recording> for a track download,
--       album:<release group> for an album download, task:<task> for a
--       download held before any release was matched. MBIDs lower-case.
--       One open entry ('held' or 'placing') per user and key. An upload's
--       entry points at its drop item and goes with it. expires_at is when
--       the held copies are removed: 90 days after the entry was made, or
--       after its newest file arrived.
--   acquire_held_files (new)
--       one held copy, its path relative to the app's `held` folder (a
--       download's copy) or `drop` folder (an upload), the track it was
--       paired to and why it is held. Its sentence and suggested action
--       are worked out from the reason when read.
--   held_imports (dropped)
--       its held rows move into the tables above. Rows already imported
--       or discarded go: nothing reads them. The retry columns nothing
--       wrote are not carried.
--   drop_import_items in needs_review
--       each becomes an upload entry with its staged files inside its job's
--       folder, named by their path below it. The item keeps its status and
--       history; one with no such file left is settled as discarded.
--
-- Times carried from REAL seconds become integer milliseconds. A carried
-- entry expires no sooner than 90 days after this upgrade, so nothing is
-- removed before it has been shown with its date. A stored reason that is
-- not one a file is held for reads as held_by_earlier_version.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE acquire_held_entries (
    id INTEGER PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    origin TEXT NOT NULL CHECK (origin IN ('download', 'upload')),
    group_key TEXT NOT NULL,
    release_group_mbid TEXT,
    recording_mbid TEXT,
    drop_item_id INTEGER REFERENCES drop_import_items(id) ON DELETE CASCADE,
    artist_name TEXT,
    album_title TEXT,
    state TEXT NOT NULL CHECK (state IN ('held', 'placing', 'resolved', 'expired')),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    resolved_at INTEGER
) STRICT;

CREATE UNIQUE INDEX idx_acquire_held_entries_open
    ON acquire_held_entries(user_id, group_key) WHERE state IN ('held', 'placing');
CREATE INDEX idx_acquire_held_entries_user ON acquire_held_entries(user_id, state);
CREATE INDEX idx_acquire_held_entries_drop_item ON acquire_held_entries(drop_item_id);
CREATE INDEX idx_acquire_held_entries_group
    ON acquire_held_entries(release_group_mbid, state);
CREATE INDEX idx_acquire_held_entries_expiry ON acquire_held_entries(state, expires_at);

CREATE TABLE acquire_held_files (
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
        'no_album_match', 'album_not_found', 'tags_disagree', 'not_chosen_edition',
        'samples', 'too_many_files', 'interrupted', 'import_failed',
        'held_by_earlier_version', 'target_occupied', 'upgrade_blocked',
        'library_changed', 'library_unavailable', 'naming_script_failed',
        'library_folder_missing', 'local_fault', 'files_missing', 'files_unlisted',
        'outside_downloads_folder', 'size_mismatch', 'corrupt', 'no_audio', 'sample',
        'wrong_edition', 'not_pinned_edition', 'wrong_album', 'quality_rejected',
        'not_an_upgrade', 'wrong_track', 'stalled', 'queue_timeout', 'never_started',
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

CREATE INDEX idx_acquire_held_files_entry ON acquire_held_files(entry_id, state);
CREATE INDEX idx_acquire_held_files_task ON acquire_held_files(source_task_id, state);

-- The queue's activity revision moves when a held entry or file changes,
-- as it did for held_imports.
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

-- The upgrade's time, the 90 days every carried entry is kept for at
-- least, and the reasons a file is held for: those a person decides on
-- and those the library side caused.
CREATE TEMP TABLE held_upgrade AS
SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) AS now_ms,
       CAST(90 AS INTEGER) * 86400000 AS keep_ms,
       json_array(
           'weak_match', 'no_artist_evidence', 'mostly_unmatched', 'tag_mismatch',
           'fingerprint_mismatch', 'fingerprint_unconfirmed', 'album_version_only',
           'no_release', 'no_tracks', 'version_lower_quality', 'no_better_than_yours',
           'no_album_match', 'album_not_found', 'tags_disagree', 'not_chosen_edition',
           'samples', 'too_many_files', 'interrupted', 'import_failed',
           'held_by_earlier_version', 'target_occupied', 'upgrade_blocked',
           'library_changed', 'library_unavailable', 'naming_script_failed',
           'library_folder_missing', 'local_fault') AS codes;

-- Held downloads, each with the key of the entry it joins.
CREATE TEMP TABLE held_carried AS
SELECT h.*,
       CASE
           WHEN t.download_type = 'track' AND COALESCE(h.recording_mbid, '') != ''
               THEN 'track:' || lower(h.recording_mbid)
           WHEN COALESCE(h.release_group_mbid, '') != ''
               THEN 'album:' || lower(h.release_group_mbid)
           ELSE 'task:' || COALESCE(h.source_task_id, 'held-' || h.id)
       END AS carried_key,
       CAST(h.created_at * 1000 AS INTEGER) AS created_ms
FROM held_imports h
LEFT JOIN download_tasks t ON t.id = h.source_task_id
WHERE h.status = 'held' AND h.user_id IN (SELECT id FROM auth_users);

INSERT INTO acquire_held_entries (user_id, origin, group_key, release_group_mbid,
    recording_mbid, artist_name, album_title, state, created_at, expires_at)
SELECT user_id, 'download', carried_key,
       lower(MAX(NULLIF(release_group_mbid, ''))),
       CASE WHEN carried_key LIKE 'track:%' THEN substr(carried_key, 7) END,
       MAX(artist_name), MAX(album_title), 'held', MIN(created_ms),
       MAX(MAX(created_ms), (SELECT now_ms FROM held_upgrade))
           + (SELECT keep_ms FROM held_upgrade)
FROM held_carried
GROUP BY user_id, carried_key;

INSERT INTO acquire_held_files (entry_id, root, path, original_name, release_mbid,
    release_track_mbid, recording_mbid, disc_number, track_number, track_title,
    file_format, duration_seconds, expected_duration_seconds, evidence_title,
    evidence_artist, evidence_score, source, source_task_id, reason, reason_detail,
    state, created_at)
SELECT e.id, 'held',
       -- The held folder is flat: the copy's own name.
       substr(c.held_path, length(rtrim(c.held_path, replace(c.held_path, '/', ''))) + 1),
       COALESCE(NULLIF(c.original_filename, ''),
           substr(c.held_path, length(rtrim(c.held_path, replace(c.held_path, '/', ''))) + 1)),
       c.release_mbid, c.release_track_mbid, lower(c.recording_mbid), c.disc_number,
       c.track_number, c.track_title, c.file_format, c.duration_seconds,
       c.expected_duration_seconds, c.evidence_title, c.evidence_artist,
       c.evidence_score, c.source, c.source_task_id,
       CASE WHEN EXISTS (SELECT 1 FROM json_each((SELECT codes FROM held_upgrade)) k
               WHERE k.value = c.reason)
           THEN c.reason ELSE 'held_by_earlier_version' END,
       c.reason_detail, 'held', c.created_ms
FROM held_carried c
JOIN acquire_held_entries e
    ON e.user_id = c.user_id AND e.group_key = c.carried_key AND e.state = 'held';

-- Uploads waiting for a person: each staged file inside its job's
-- folder, by its path below that folder. A path anywhere else is not
-- carried, so nothing outside the job's own files can be named.
CREATE TEMP TABLE held_staged AS
SELECT i.id AS item_id, j.id AS job_id,
       substr(p.value, length(j.staging_dir) + 2) AS below
FROM drop_import_items i
JOIN drop_import_jobs j ON j.id = i.job_id
JOIN json_each(CASE WHEN json_valid(i.staging_paths) THEN i.staging_paths ELSE '[]' END) p
WHERE i.status = 'needs_review' AND typeof(p.value) = 'text'
  AND substr(p.value, 1, length(j.staging_dir) + 1) = j.staging_dir || '/'
  AND length(p.value) > length(j.staging_dir) + 1
  AND instr('/' || substr(p.value, length(j.staging_dir) + 2) || '/', '/../') = 0
  AND instr('/' || substr(p.value, length(j.staging_dir) + 2) || '/', '/./') = 0;

-- One entry per drop item with a file to show.
INSERT INTO acquire_held_entries (user_id, origin, group_key, release_group_mbid,
    drop_item_id, artist_name, album_title, state, created_at, expires_at)
SELECT j.user_id, 'upload', 'upload:' || i.id, lower(NULLIF(i.release_group_mbid, '')),
       i.id, i.artist_name, i.album_title, 'held',
       CAST(j.created_at * 1000 AS INTEGER),
       MAX(CAST(j.created_at * 1000 AS INTEGER), (SELECT now_ms FROM held_upgrade))
           + (SELECT keep_ms FROM held_upgrade)
FROM drop_import_items i
JOIN drop_import_jobs j ON j.id = i.job_id
WHERE i.status = 'needs_review' AND j.user_id IN (SELECT id FROM auth_users)
  AND i.id IN (SELECT item_id FROM held_staged);

INSERT INTO acquire_held_files (entry_id, root, path, original_name, source, reason,
    reason_detail, state, created_at)
SELECT e.id, 'drop', s.job_id || '/' || s.below, s.below, 'upload',
       CASE WHEN i.reason_code = 'musicbrainz_unavailable'
               OR EXISTS (SELECT 1 FROM json_each((SELECT codes FROM held_upgrade)) k
                   WHERE k.value = i.reason_code)
           THEN i.reason_code ELSE 'held_by_earlier_version' END,
       NULL, 'held', e.created_at
FROM held_staged s
JOIN acquire_held_entries e ON e.drop_item_id = s.item_id AND e.origin = 'upload'
JOIN drop_import_items i ON i.id = s.item_id;

-- An item waiting for review with no file left to show has nothing to
-- decide: it is settled, so no empty entry sits in the list.
UPDATE drop_import_items
SET status = 'discarded', detail = 'No files were left to review.', reason_code = NULL,
    staging_paths = '[]'
WHERE status = 'needs_review'
  AND id NOT IN (SELECT drop_item_id FROM acquire_held_entries
                 WHERE drop_item_id IS NOT NULL);

DROP TABLE held_staged;
DROP TABLE held_carried;
DROP TABLE held_upgrade;
DROP TABLE held_imports;

-- Migration high-water mark.
PRAGMA user_version = 85;
