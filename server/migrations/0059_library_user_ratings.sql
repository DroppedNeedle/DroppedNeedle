-- 0059_library_user_ratings.sql - a user's star rating of an artist, an
-- album or a track, set from a Subsonic app.
--
--   user_id    the user who rated it; the rating goes with the user
--   item_kind  artist, album or track
--   item_id    the local artist, album or track id. It names a row of one
--              of three tables, so there is no foreign key, as with
--              favorites. A merge moves the rating to the surviving row.
--   rating     1 to 5; clearing a rating deletes the row
--   rated_at   when it was last set, milliseconds since the epoch
--
-- The index serves the average over every user's rating of an item.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_user_ratings (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    item_kind TEXT NOT NULL CHECK (item_kind IN ('artist', 'album', 'track')),
    item_id TEXT NOT NULL,
    rating INTEGER NOT NULL CHECK (rating BETWEEN 1 AND 5),
    rated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_kind, item_id)
) STRICT;

CREATE INDEX idx_user_ratings_item ON library_user_ratings(item_kind, item_id);

-- Migration high-water mark.
PRAGMA user_version = 59;
