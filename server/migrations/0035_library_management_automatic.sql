-- 0035_library_management_automatic.sql - automatic management and
-- resolved publish bundles.
--
-- New tables:
--   library_management_automatic_queue  albums automatic management should
--                                       look at: one row per album, added
--                                       when an import lands or
--                                       identification settles. The worker
--                                       starts a run once the album is
--                                       ready, waits (with a growing delay)
--                                       while it is not, and drops the row
--                                       when there is nothing to do.
--   library_publish_resolutions         publish bundles an administrator
--                                       closed after checking their files.
--                                       The journal rows keep their
--                                       needs_attention state and evidence;
--                                       this only records that someone
--                                       looked and the files were in place.
--
--   library_track_management_identity  per file, the album's and the
--                                       file's match revisions the last
--                                       profile run wrote from, so a new
--                                       edition (or a changed match) is
--                                       managed again. A table of its own
--                                       rather than new columns, so this
--                                       file is safe to run twice.
--   library_publish_replacements        publish journals that replace the
--                                       file standing at their destination
--                                       (a duplicate resolution that keeps
--                                       the new copy): the replaced file's
--                                       bytes, its track, and where in the
--                                       recycle bin it goes once the
--                                       bundle commits. Until then it waits
--                                       under the journal's backup.
--
-- Every statement is IF NOT EXISTS: running the file twice is harmless.
--
-- No down migration: rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS library_track_management_identity (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    applied_album_identity_revision INTEGER,
    applied_track_identity_revision INTEGER
);

CREATE TABLE IF NOT EXISTS library_publish_replacements (
    journal_id TEXT PRIMARY KEY REFERENCES library_publish_journal(id) ON DELETE CASCADE,
    replaced_rel TEXT NOT NULL,
    replaced_sha256 TEXT NOT NULL,
    replaced_track_id TEXT,
    recycle_to TEXT NOT NULL,
    recycled_at REAL
);

CREATE TABLE IF NOT EXISTS library_management_automatic_queue (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    queued_at REAL NOT NULL,
    not_before REAL NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    last_reason_code TEXT
);

CREATE INDEX IF NOT EXISTS idx_library_management_automatic_queue_due
    ON library_management_automatic_queue(not_before);

CREATE TABLE IF NOT EXISTS library_publish_resolutions (
    bundle_id TEXT PRIMARY KEY,
    resolved_by_user_id TEXT,
    verified_files INTEGER NOT NULL CHECK(verified_files >= 0),
    resolved_at REAL NOT NULL
);

-- Migration high-water mark.
PRAGMA user_version = 35;
