-- 0042_play_history_user_track.sql - one listener's plays of one track.
--
-- The player lists that sort or filter by a listener's own plays ("last
-- played", "most played", played and unplayed) read every play that
-- listener has. The indexes so far were (user_id, played_at) and one per
-- id column, so each play cost a visit to the table row. This index holds
-- the listener, the track and the time together: those reads, and the
-- play count and last play of the tracks on a page, are answered from the
-- index alone.
--
-- A play can also name an album and no track (a play carried from v2
-- whose track is gone). An album's play count adds those, and they are few
-- beside the plays that name no album at all, so they get a partial index
-- of their own: without it that count reads every play of the listener
-- that names no track, or every play stored under the album.
--
-- The older indexes stay. (user_id, played_at) serves the newest-first
-- history lists, and the single-column ones the merges that move plays
-- from one track, album or artist to another.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE INDEX IF NOT EXISTS idx_play_history_user_track
    ON library_play_history(user_id, local_track_id, played_at);

CREATE INDEX IF NOT EXISTS idx_play_history_user_album_no_track
    ON library_play_history(user_id, local_album_id, played_at)
    WHERE local_track_id IS NULL AND local_album_id IS NOT NULL;

-- Migration high-water mark.
PRAGMA user_version = 42;
