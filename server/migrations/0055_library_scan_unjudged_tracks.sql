-- 0055_library_scan_unjudged_tracks.sql - files first indexed in a held
-- folder whose hold lifted before their album could be judged as a retag.
--
-- A scan that holds a folder back keeps automatic management off the files
-- that appeared in it since, and each scan's end checks whether their album
-- replaced an album whose files went: if so, it is left unmanaged as the
-- person's retag. That check needs the album's settled match. When the hold
-- lifts while the match is still a best guess, missing or waiting for
-- review, the album's held files are kept here, and automatic management
-- makes the same check before it first writes the album, on the edition it
-- is about to write. One row per track:
--
--   local_track_id  the file's track row; the row goes with it
--   held_at         when the hold it was found under began, the earliest
--                   when it sat under several
--
-- A row is written at the scan end that lifts the hold, for an album that
-- is not left unmanaged and has no file the app has a record of. The rows
-- of an album go when automatic management first looks at it to write it,
-- whatever the check decides. Keyed by track, so a file merged into another
-- album takes its row along. Times are milliseconds.
--
-- Nothing is carried over: holds lifted before this version are gone.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_scan_unjudged_tracks (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    held_at INTEGER NOT NULL
) STRICT;

-- Migration high-water mark.
PRAGMA user_version = 55;
