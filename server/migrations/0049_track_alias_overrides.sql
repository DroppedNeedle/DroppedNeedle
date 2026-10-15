-- 0049_track_alias_overrides.sql - an alias says whether field overrides
-- follow it.
--
-- A track that continues into another (0047) hands over everything people
-- made on it, field overrides included: both rows are the same track of
-- the same edition. A track replaced by this edition's version of it is a
-- different recording, so its field overrides (title, ISRC, recording id,
-- credits) describe only its own file. Its plays, favorites, playlist
-- entries, bookmarks and play queues move; its overrides stay on it:
--
--   moves_overrides  1: field overrides move to the track the alias names
--                    0: they stay on the old row (a replaced version)
--
-- Every alias written before this migration continues a track into the
-- same track, so it keeps 1.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE local_track_aliases
    ADD COLUMN moves_overrides INTEGER NOT NULL DEFAULT 1 CHECK (moves_overrides IN (0, 1));

-- Migration high-water mark.
PRAGMA user_version = 49;
