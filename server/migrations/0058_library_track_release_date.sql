-- 0058_library_track_release_date.sql - a track's release date, read from
-- its file's tags beside the other edition hints.
--
--   local_track_edition_tags.release_date
--       `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, or NULL when the file names no
--       release date
--
-- Subsonic albums send it as `releaseDate` when every track of the album
-- has the same one. Existing rows read NULL: a scan fills the value as a
-- file changes, and "Rescan files" fills it for every file.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE local_track_edition_tags ADD COLUMN release_date TEXT;

-- Migration high-water mark.
PRAGMA user_version = 58;
