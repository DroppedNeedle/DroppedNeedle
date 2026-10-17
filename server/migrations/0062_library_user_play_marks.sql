-- 0062_library_user_play_marks.sql - a user's played and unplayed marks
-- on tracks, set from a Jellyfin app ("mark as played", "mark as
-- unplayed"). Marking an album marks each of its tracks.
--
--   user_id             the user who marked it; the mark goes with the user
--   local_track_id      the track; the mark goes with the track row, and
--                       moves when a missing track continues into another
--   marked_played_at    when it was last marked played, milliseconds since
--                       the epoch; cleared by an unplayed mark
--   marked_unplayed_at  when it was last marked unplayed; the user's plays
--                       of the track up to that second no longer count in
--                       Jellyfin answers. The plays themselves are kept.
--
-- A row holds at least one of the two times. The index serves the cascade
-- and the move of a continued track's marks.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_user_play_marks (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    marked_played_at INTEGER,
    marked_unplayed_at INTEGER,
    PRIMARY KEY (user_id, local_track_id),
    CHECK (marked_played_at IS NOT NULL OR marked_unplayed_at IS NOT NULL)
) STRICT;

CREATE INDEX idx_user_play_marks_track ON library_user_play_marks(local_track_id);

-- Migration high-water mark.
PRAGMA user_version = 62;
