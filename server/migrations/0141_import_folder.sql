-- no-transaction
PRAGMA foreign_keys=OFF;

BEGIN IMMEDIATE;

-- The import folder: work that belongs to no user. A drop import job gains
-- its source (an upload, or the import folder) and may have no owner; a
-- held entry may come from the import folder, with no owner and no expiry;
-- a held file may sit in the import folder, and may be held as unreadable;
-- an import-folder file keeps its size and modification time as listed, so
-- one that changed since is never acted on as if it were the same file.
--
-- drop_import_jobs and acquire_held_entries are pointed at by other
-- tables, so this file rebuilds them with foreign keys off, by the recipe
-- in kernel/src/db/migrate.rs. Ids, indexes and every other row stay as
-- they were. The held triggers come back guarded: an entry with no owner
-- bumps only the global download revision, since the per-user revision
-- table refuses a NULL user.

CREATE TEMP TABLE import_folder_guard_keys (
    ok INTEGER CONSTRAINT guard_foreign_keys_off CHECK (ok = 1)
);
INSERT INTO import_folder_guard_keys
SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;

CREATE TEMP TABLE import_folder_guard_version (
    ok INTEGER CONSTRAINT guard_not_applied_yet CHECK (ok = 1)
);
INSERT INTO import_folder_guard_version
SELECT (SELECT user_version FROM pragma_user_version) < 141;

CREATE TEMP TABLE import_folder_orphans (
    child TEXT,
    child_rowid INTEGER,
    parent TEXT
);
INSERT INTO import_folder_orphans (child, child_rowid, parent)
SELECT "table", "rowid", parent FROM pragma_foreign_key_check;
CREATE INDEX import_folder_orphans_key
    ON import_folder_orphans (child, child_rowid, parent);

CREATE TABLE drop_import_jobs_new (
    id TEXT PRIMARY KEY,
    user_id TEXT,
    user_name TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL
        CHECK(status IN ('processing','completed','failed')),
    created_at INTEGER NOT NULL,
    upload_name TEXT NOT NULL,
    staging_dir TEXT NOT NULL,
    error TEXT,
    source TEXT NOT NULL DEFAULT 'upload' CHECK (source IN ('upload', 'import_folder')),
    CHECK ((source = 'upload') = (user_id IS NOT NULL))
) STRICT;

INSERT INTO drop_import_jobs_new (
    rowid, id, user_id, user_name, status, created_at, upload_name, staging_dir, error, source)
SELECT
    rowid, id, user_id, user_name, status, created_at, upload_name, staging_dir, error, 'upload'
FROM drop_import_jobs;

DROP TABLE drop_import_jobs;

ALTER TABLE drop_import_jobs_new RENAME TO drop_import_jobs;

CREATE INDEX idx_drop_import_jobs_user
    ON drop_import_jobs(user_id, created_at);

-- The held files' triggers read acquire_held_entries: they go before it is
-- dropped and come back below.
DROP TRIGGER download_activity_held_file_insert;
DROP TRIGGER download_activity_held_file_state;

CREATE TABLE acquire_held_entries_new (
    id INTEGER PRIMARY KEY,
    user_id TEXT REFERENCES auth_users(id) ON DELETE CASCADE,
    origin TEXT NOT NULL CHECK (origin IN ('download', 'upload', 'import_folder')),
    group_key TEXT NOT NULL,
    release_group_mbid TEXT,
    recording_mbid TEXT,
    drop_item_id INTEGER REFERENCES drop_import_items(id) ON DELETE CASCADE,
    artist_name TEXT,
    album_title TEXT,
    state TEXT NOT NULL CHECK (state IN ('held', 'placing', 'resolved', 'expired')),
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    resolved_at INTEGER,
    retry_tries INTEGER NOT NULL DEFAULT 0,
    retry_at INTEGER,
    CHECK ((origin = 'import_folder') = (user_id IS NULL)),
    CHECK ((origin = 'import_folder') = (expires_at IS NULL))
) STRICT;

INSERT INTO acquire_held_entries_new (
    id, user_id, origin, group_key, release_group_mbid, recording_mbid, drop_item_id,
    artist_name, album_title, state, created_at, expires_at, resolved_at, retry_tries,
    retry_at)
SELECT
    id, user_id, origin, group_key, release_group_mbid, recording_mbid, drop_item_id,
    artist_name, album_title, state, created_at, expires_at, resolved_at, retry_tries,
    retry_at
FROM acquire_held_entries;

DROP TABLE acquire_held_entries;

ALTER TABLE acquire_held_entries_new RENAME TO acquire_held_entries;

CREATE INDEX idx_acquire_held_entries_drop_item ON acquire_held_entries(drop_item_id);

CREATE INDEX idx_acquire_held_entries_expiry ON acquire_held_entries(state, expires_at);

CREATE INDEX idx_acquire_held_entries_group
    ON acquire_held_entries(release_group_mbid, state);

CREATE UNIQUE INDEX idx_acquire_held_entries_open
    ON acquire_held_entries(user_id, group_key) WHERE state IN ('held', 'placing');

CREATE INDEX idx_acquire_held_entries_retry ON acquire_held_entries(state, retry_at);

CREATE INDEX idx_acquire_held_entries_user ON acquire_held_entries(user_id, state);

CREATE TABLE acquire_held_files_new (
    id INTEGER PRIMARY KEY,
    entry_id INTEGER NOT NULL REFERENCES acquire_held_entries(id) ON DELETE CASCADE,
    root TEXT NOT NULL CHECK (root IN ('held', 'drop', 'import')),
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
        'no_release', 'no_tracks', 'corrupt', 'version_lower_quality',
        'no_better_than_yours', 'owned_elsewhere', 'owned_on_album', 'album_needs_check',
        'no_album_match', 'album_not_found', 'tags_disagree', 'not_chosen_edition',
        'samples', 'too_many_files', 'interrupted', 'import_failed',
        'held_by_earlier_version', 'format_not_listed', 'target_occupied',
        'upgrade_blocked', 'library_changed', 'library_unavailable',
        'naming_script_failed', 'library_folder_missing', 'library_off', 'local_fault',
        'musicbrainz_unavailable', 'acoustid_unavailable')),
    reason_detail TEXT,
    state TEXT NOT NULL CHECK (state IN ('held', 'imported', 'already_owned', 'discarded')),
    created_at INTEGER NOT NULL,
    resolved_at INTEGER,
    file_size INTEGER,
    file_modified_at INTEGER
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
    source_task_id, reason, reason_detail, state, created_at, resolved_at
FROM acquire_held_files;

DROP TABLE acquire_held_files;

ALTER TABLE acquire_held_files_new RENAME TO acquire_held_files;

CREATE INDEX idx_acquire_held_files_entry ON acquire_held_files(entry_id, state);

CREATE INDEX idx_acquire_held_files_task ON acquire_held_files(source_task_id, state);

CREATE TRIGGER download_activity_held_entry_insert
AFTER INSERT ON acquire_held_entries
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT NEW.user_id, 1 WHERE NEW.user_id IS NOT NULL
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_entry_state
AFTER UPDATE OF state ON acquire_held_entries
WHEN OLD.state IS NOT NEW.state
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT NEW.user_id, 1 WHERE NEW.user_id IS NOT NULL
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_entry_delete
AFTER DELETE ON acquire_held_entries
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT OLD.user_id, 1 WHERE OLD.user_id IS NOT NULL
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_entry_retry
AFTER UPDATE OF retry_at ON acquire_held_entries
WHEN OLD.retry_at IS NOT NEW.retry_at
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT NEW.user_id, 1 WHERE NEW.user_id IS NOT NULL
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_file_insert
AFTER INSERT ON acquire_held_files
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM acquire_held_entries
    WHERE id = NEW.entry_id AND user_id IS NOT NULL
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_file_state
AFTER UPDATE OF state, reason ON acquire_held_files
WHEN OLD.state IS NOT NEW.state OR OLD.reason IS NOT NEW.reason
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM acquire_held_entries
    WHERE id = NEW.entry_id AND user_id IS NOT NULL
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TEMP TABLE import_folder_guard_orphans (
    ok INTEGER CONSTRAINT guard_no_new_orphans CHECK (ok = 1)
);
INSERT INTO import_folder_guard_orphans
SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, "rowid" AS child_rowid, parent, COUNT(*) AS found
        FROM pragma_foreign_key_check
        GROUP BY 1, 2, 3
    ) AS now
    WHERE now.found > (
        SELECT COUNT(*) FROM import_folder_orphans saved
        WHERE saved.child = now.child
          AND saved.child_rowid IS now.child_rowid
          AND saved.parent = now.parent
    )
);

DROP TABLE import_folder_guard_keys;
DROP TABLE import_folder_guard_version;
DROP TABLE import_folder_orphans;
DROP TABLE import_folder_guard_orphans;

PRAGMA user_version = 141;

COMMIT;

PRAGMA foreign_keys=ON;
