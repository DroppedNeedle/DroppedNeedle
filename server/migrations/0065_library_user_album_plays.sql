-- 0065_library_user_album_plays.sql - each listener's play values per
-- album, and whether an album has a track that can be streamed, kept so
-- the album pages sorted or filtered by play history read them by key
-- instead of working them out from every play and mark of the listener.
--
--   local_albums.has_indexed_track
--       1 exactly when a track on the album is indexed.
--
--   library_user_album_plays
--       one row per listener and album whose play count is above 0, with
--       the album's values as Jellyfin answers give them:
--         plays           the play count: each track's counted plays (its
--                         plays later than its unplayed mark, in whole
--                         seconds, or all of them with no unplayed mark;
--                         1 when it has none and a played mark), plus the
--                         plays filed under the album with no track
--         last_played_at  the latest of those plays and played marks,
--                         whole seconds times 1000; NULL when no counted
--                         play has a readable time and there is no played
--                         mark
--         played          1 when the album has an indexed track and every
--                         indexed track on it has a count above 0
--       user_id has no foreign key: plays carry none, and plays exist for
--       user ids with no account (the v2 import).
--
-- No code writes either. The triggers below keep them, inside the
-- transaction of every write to library_play_history,
-- library_user_play_marks and local_tracks, whoever makes it. The rule
-- lives once, in the refresh behind the view
-- library_user_album_plays_refresh: inserting (user, album) into the view
-- works that pair out again from the source rows. The backfill and every
-- trigger go through it. A play counts for the album its track is on now;
-- the album stored on a play that names a track is not read.
--
-- The followed tables are never written with REPLACE, which removes rows
-- without firing their delete triggers. A later migration that rebuilds
-- one of them drops these triggers with it and must create them again
-- (the schema test counts the triggers). Changing the rule needs a
-- migration that recreates the refresh trigger and runs the backfill
-- again.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE local_albums ADD COLUMN has_indexed_track INTEGER NOT NULL DEFAULT 0
    CHECK (has_indexed_track IN (0, 1));

CREATE TABLE library_user_album_plays (
    user_id TEXT NOT NULL,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    plays INTEGER NOT NULL CHECK (plays > 0),
    last_played_at INTEGER,
    played INTEGER NOT NULL CHECK (played IN (0, 1)),
    PRIMARY KEY (user_id, local_album_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_user_album_plays_album ON library_user_album_plays(local_album_id);
CREATE INDEX idx_user_album_plays_last_played
    ON library_user_album_plays(user_id, last_played_at, local_album_id);
CREATE INDEX idx_user_album_plays_count
    ON library_user_album_plays(user_id, plays, local_album_id);

CREATE VIEW library_user_album_plays_refresh (user_id, local_album_id) AS
    SELECT NULL, NULL WHERE 0;

-- The one refresh. A pair with no album (a mark whose track was just
-- deleted, a play that names neither) or an album that is not there does
-- nothing.
CREATE TRIGGER trg_user_album_plays_refresh
INSTEAD OF INSERT ON library_user_album_plays_refresh
WHEN NEW.local_album_id IS NOT NULL
 AND EXISTS (SELECT 1 FROM local_albums WHERE id = NEW.local_album_id)
BEGIN
    DELETE FROM library_user_album_plays
    WHERE user_id = NEW.user_id AND local_album_id = NEW.local_album_id;
    INSERT INTO library_user_album_plays (user_id, local_album_id, plays, last_played_at, played)
    SELECT NEW.user_id, NEW.local_album_id, SUM(plays), MAX(last) * 1000,
           SUM(indexed) > 0 AND SUM(indexed AND plays = 0) = 0
    FROM (
        SELECT indexed,
               CASE WHEN counted > 0 THEN counted
                    WHEN marked IS NOT NULL THEN 1 ELSE 0 END AS plays,
               CASE WHEN counted > 0 THEN COALESCE(latest, marked) ELSE marked END AS last
        FROM (
            SELECT t.availability = 'indexed' AS indexed,
                   m.marked_played_at / 1000 AS marked,
                   (SELECT COUNT(*)
                    FROM library_play_history h INDEXED BY idx_play_history_user_track
                    WHERE h.user_id = NEW.user_id AND h.local_track_id = t.id
                      AND (m.marked_unplayed_at IS NULL
                           OR unixepoch(h.played_at) > m.marked_unplayed_at / 1000)) AS counted,
                   (SELECT MAX(unixepoch(h.played_at))
                    FROM library_play_history h INDEXED BY idx_play_history_user_track
                    WHERE h.user_id = NEW.user_id AND h.local_track_id = t.id
                      AND (m.marked_unplayed_at IS NULL
                           OR unixepoch(h.played_at) > m.marked_unplayed_at / 1000)) AS latest
            FROM local_tracks t
            LEFT JOIN library_user_play_marks m
                ON m.user_id = NEW.user_id AND m.local_track_id = t.id
            WHERE t.local_album_id = NEW.local_album_id
        )
        UNION ALL
        SELECT 0, COUNT(*), MAX(unixepoch(h.played_at))
        FROM library_play_history h INDEXED BY idx_play_history_user_album_no_track
        WHERE h.user_id = NEW.user_id AND h.local_track_id IS NULL
          AND h.local_album_id = NEW.local_album_id AND h.local_album_id IS NOT NULL
    )
    HAVING SUM(plays) > 0;
END;

-- Backfill, through the same refresh.
UPDATE local_albums SET has_indexed_track = 1
WHERE EXISTS (SELECT 1 FROM local_tracks t
              WHERE t.local_album_id = local_albums.id AND t.availability = 'indexed');

INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
SELECT h.user_id, t.local_album_id
FROM library_play_history h JOIN local_tracks t ON t.id = h.local_track_id
UNION
SELECT user_id, local_album_id FROM library_play_history
WHERE local_track_id IS NULL AND local_album_id IS NOT NULL
UNION
SELECT m.user_id, t.local_album_id
FROM library_user_play_marks m JOIN local_tracks t ON t.id = m.local_track_id;

-- Plays. A play's album is its track's album now when it names a track,
-- else the album it is filed under. A play that names neither counts for
-- no album.
CREATE TRIGGER trg_user_album_plays_play_insert
AFTER INSERT ON library_play_history
WHEN NEW.local_track_id IS NOT NULL OR NEW.local_album_id IS NOT NULL
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (NEW.user_id, CASE WHEN NEW.local_track_id IS NULL THEN NEW.local_album_id
        ELSE (SELECT local_album_id FROM local_tracks WHERE id = NEW.local_track_id) END);
END;

CREATE TRIGGER trg_user_album_plays_play_delete
AFTER DELETE ON library_play_history
WHEN OLD.local_track_id IS NOT NULL OR OLD.local_album_id IS NOT NULL
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (OLD.user_id, CASE WHEN OLD.local_track_id IS NULL THEN OLD.local_album_id
        ELSE (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id) END);
END;

-- The album stored on a play that names a track is not read, so setting
-- it (the import's link step, a merge's retarget) refreshes nothing.
CREATE TRIGGER trg_user_album_plays_play_update
AFTER UPDATE OF user_id, local_track_id, local_album_id, played_at ON library_play_history
WHEN OLD.user_id IS NOT NEW.user_id
  OR OLD.local_track_id IS NOT NEW.local_track_id
  OR OLD.played_at IS NOT NEW.played_at
  OR ((OLD.local_track_id IS NULL OR NEW.local_track_id IS NULL)
      AND OLD.local_album_id IS NOT NEW.local_album_id)
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT OLD.user_id, CASE WHEN OLD.local_track_id IS NULL THEN OLD.local_album_id
        ELSE (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id) END
    UNION
    SELECT NEW.user_id, CASE WHEN NEW.local_track_id IS NULL THEN NEW.local_album_id
        ELSE (SELECT local_album_id FROM local_tracks WHERE id = NEW.local_track_id) END;
END;

-- Marks, for the album of the mark's track. A mark cascading off a
-- deleted track finds no album; the track's own trigger refreshes it.
CREATE TRIGGER trg_user_album_plays_mark_insert
AFTER INSERT ON library_user_play_marks
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (NEW.user_id, (SELECT local_album_id FROM local_tracks WHERE id = NEW.local_track_id));
END;

CREATE TRIGGER trg_user_album_plays_mark_delete
AFTER DELETE ON library_user_play_marks
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (OLD.user_id, (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id));
END;

CREATE TRIGGER trg_user_album_plays_mark_update
AFTER UPDATE ON library_user_play_marks
WHEN OLD.user_id IS NOT NEW.user_id
  OR OLD.local_track_id IS NOT NEW.local_track_id
  OR OLD.marked_played_at IS NOT NEW.marked_played_at
  OR OLD.marked_unplayed_at IS NOT NEW.marked_unplayed_at
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT OLD.user_id, (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id)
    UNION
    SELECT NEW.user_id, (SELECT local_album_id FROM local_tracks WHERE id = NEW.local_track_id);
END;

-- Tracks. Only the listeners with a row on an album can have a count
-- there: a new, moved or changed track gives no one a play. A track with
-- plays cannot be deleted; its marks cascade and fire their own trigger.
CREATE TRIGGER trg_user_album_plays_track_insert
AFTER INSERT ON local_tracks
WHEN NEW.availability = 'indexed'
BEGIN
    UPDATE local_albums SET has_indexed_track = 1
    WHERE id = NEW.local_album_id AND has_indexed_track = 0;
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT user_id, local_album_id FROM library_user_album_plays
    WHERE local_album_id = NEW.local_album_id;
END;

CREATE TRIGGER trg_user_album_plays_track_delete
AFTER DELETE ON local_tracks
BEGIN
    UPDATE local_albums SET has_indexed_track = 0
    WHERE id = OLD.local_album_id AND has_indexed_track = 1
      AND NOT EXISTS (SELECT 1 FROM local_tracks t
                      WHERE t.local_album_id = OLD.local_album_id AND t.availability = 'indexed');
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT user_id, local_album_id FROM library_user_album_plays
    WHERE local_album_id = OLD.local_album_id;
END;

-- A move or a crossing of 'indexed' either way. The column is written
-- only where it changes.
CREATE TRIGGER trg_user_album_plays_track_update
AFTER UPDATE OF availability, local_album_id ON local_tracks
WHEN OLD.local_album_id IS NOT NEW.local_album_id
  OR (OLD.availability = 'indexed') IS NOT (NEW.availability = 'indexed')
BEGIN
    UPDATE local_albums SET has_indexed_track = NOT has_indexed_track
    WHERE id IN (OLD.local_album_id, NEW.local_album_id)
      AND has_indexed_track IS NOT EXISTS (SELECT 1 FROM local_tracks t
          WHERE t.local_album_id = local_albums.id AND t.availability = 'indexed');
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT u.user_id, x.id
    FROM (SELECT DISTINCT user_id FROM library_user_album_plays
          WHERE local_album_id IN (OLD.local_album_id, NEW.local_album_id)) u
    CROSS JOIN (SELECT OLD.local_album_id AS id UNION SELECT NEW.local_album_id) x;
END;

-- Migration high-water mark.
PRAGMA user_version = 65;
