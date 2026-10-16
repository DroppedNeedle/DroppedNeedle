-- 0061_library_track_lyrics.sql - which tracks carry lyrics in their files.
--
-- local_track_lyrics holds one row per track whose file carries lyrics: a
-- `.lrc` file beside it with the same name, or a lyrics tag. A track
-- without lyrics has no row. The lyrics sweep writes the rows; it only
-- reads music files.
--
--   local_track_id  the track; the row goes with it
--   source          'sidecar' for the `.lrc` file, 'tag' for the tag
--
-- local_album_lyrics_checks remembers what the sweep last looked at for
-- an album, so albums whose files and folders did not change are skipped.
--
--   local_album_id  the album; the row goes with it
--   signature       a hash of its tracks' ids and stat revisions and its
--                   folders' modification times
--   checked_at      when, in milliseconds
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE local_track_lyrics (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK (source IN ('sidecar', 'tag'))
) STRICT;

CREATE TABLE local_album_lyrics_checks (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    signature TEXT NOT NULL,
    checked_at INTEGER NOT NULL
) STRICT;

-- Migration high-water mark.
PRAGMA user_version = 61;
