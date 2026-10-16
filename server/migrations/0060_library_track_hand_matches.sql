-- 0060_library_track_hand_matches.sql - files a person matched by hand to
-- a track of their album's edition.
--
-- On the album's compare table a curator can put a file the edition did
-- not place on an edition track no file holds. The match itself is the
-- file's track identity, written as the person's (`manual`); this table
-- keeps what the undo needs and what the edition plan shows as "matched by
-- you". One row per file:
--
--   local_track_id       the file's track row; the row goes with it
--   release_mbid         the edition, as the album's identity spelled it
--   release_track_mbid   the edition track the file was put on
--   prior_identity_json  the file's track identity before the match, as
--                        JSON; NULL when it had none
--   matched_by_user_id   who matched it; NULL once that account is gone
--   matched_at           when, in milliseconds
--
-- A row is live only while the match stands: the file's track identity is
-- still `manual` and names this release and release track. Choosing
-- another edition, "Let DroppedNeedle choose" or "Leave unmanaged" change
-- that identity, and the row then stands for nothing. A new match of the
-- same file replaces its row; undoing the match deletes it.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_track_hand_matches (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    release_mbid TEXT NOT NULL,
    release_track_mbid TEXT NOT NULL,
    prior_identity_json TEXT,
    matched_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    matched_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_library_track_hand_matches_user
    ON library_track_hand_matches(matched_by_user_id);

-- Migration high-water mark.
PRAGMA user_version = 60;
