-- 0047_local_track_aliases.sql - the old ids of tracks that continue into
-- another track.
--
-- A file retagged and renamed in one go, or one of two copies of a track
-- deleted, leaves its track row missing while another row of the same
-- album holds the same MusicBrainz release track. What people made on the
-- missing row (plays, favorites, playlist entries, bookmarks, saved play
-- queues, field overrides) moves to the other row, and the old id stays
-- here so links and ids saved by player apps still open the track:
--
--   alias           the missing track's id
--   local_track_id  the track it continues into; never itself an alias
--   created_at      when the alias was made, in milliseconds
--
-- The alias belongs to the old row and goes with it; the row it names
-- must outlive it. A row is deleted when the old track's file comes back.
--
-- Migration 0041 dropped an older, empty table of the same name that no
-- code filled.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE local_track_aliases (
    alias TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    created_at INTEGER NOT NULL,
    CHECK (alias <> local_track_id)
) STRICT;

CREATE INDEX idx_local_track_aliases_track ON local_track_aliases(local_track_id);

-- Migration high-water mark.
PRAGMA user_version = 47;
