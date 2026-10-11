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
-- New columns on library_track_management_state:
--   applied_album_identity_revision, applied_track_identity_revision
--     the album's and the file's match revisions a profile run wrote
--     from, so a new edition (or a changed match) is managed again.
--
-- No down migration: rollback is restoring a pre-upgrade backup.

ALTER TABLE library_track_management_state
    ADD COLUMN applied_album_identity_revision INTEGER;

ALTER TABLE library_track_management_state
    ADD COLUMN applied_track_identity_revision INTEGER;

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
