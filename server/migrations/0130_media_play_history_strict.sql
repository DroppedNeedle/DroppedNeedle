-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0130_media_play_history_strict.sql - the play history follows the schema
-- conventions.
--
-- library_play_history is rebuilt as a STRICT table, rowids, indexes and its
-- three triggers carried (the rebuild recipe in kernel/src/schema/boot.rs). No
-- table references it. What each row becomes:
--
--   * played_at is integer milliseconds. ISO text converts to the instant
--     SQLite read it as before (every read went through unixepoch), so the
--     whole seconds the play rules compare are unchanged. A number, or text
--     that is only a number, is unix seconds, as the v2 importer reads one.
--     A time that does not read becomes 0 and the row stays: it is still a
--     play;
--   * duration_ms is a whole number: a fraction is rounded, and anything that
--     is no number reads NULL, as an unknown length;
--   * a row with no id cannot be named by any read and is not copied.
--
-- The album play values (library_user_album_plays) are kept by a refresh that
-- reads played_at, so the refresh is made again reading milliseconds, its rule
-- unchanged, and the backfill runs again over every play and mark.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE play_history_130_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_130 INTEGER CONSTRAINT guard_stamp_below_130 CHECK (stamp_below_130 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO play_history_130_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO play_history_130_guard (stamp_below_130)
    SELECT (SELECT user_version FROM pragma_user_version) < 130;

CREATE TEMP TABLE play_history_130_orphans AS
    SELECT "table" AS child, rowid AS row, parent FROM pragma_foreign_key_check;
CREATE INDEX temp.play_history_130_orphans_at
    ON play_history_130_orphans(child, row, parent);

-- The refresh names the table; the table's own triggers go with it.
DROP TRIGGER trg_user_album_plays_refresh;

CREATE TABLE library_play_history_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    track_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_name TEXT,
    recording_mbid TEXT,
    release_group_mbid TEXT,
    duration_ms INTEGER,
    source TEXT,
    played_at INTEGER NOT NULL
) STRICT;
INSERT INTO library_play_history_new (rowid, id, user_id, local_track_id, local_album_id,
    local_artist_id, track_name, artist_name, album_name, recording_mbid, release_group_mbid,
    duration_ms, source, played_at)
SELECT rowid, CAST(id AS TEXT), CAST(user_id AS TEXT), CAST(local_track_id AS TEXT),
    CAST(local_album_id AS TEXT), CAST(local_artist_id AS TEXT), CAST(track_name AS TEXT),
    CAST(artist_name AS TEXT), CAST(album_name AS TEXT), CAST(recording_mbid AS TEXT),
    CAST(release_group_mbid AS TEXT),
    CASE WHEN typeof(duration_ms) IN ('integer', 'real')
        THEN CAST(round(duration_ms) AS INTEGER) END,
    CAST(source AS TEXT),
    COALESCE(CASE WHEN typeof(played_at) IN ('integer', 'real')
            THEN CAST(round(played_at * 1000) AS INTEGER)
        WHEN played_at GLOB '[0-9]*' AND played_at NOT GLOB '*[^0-9.]*'
            THEN CAST(round(CAST(played_at AS REAL) * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(played_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM library_play_history
WHERE id IS NOT NULL;
DROP TABLE library_play_history;
ALTER TABLE library_play_history_new RENAME TO library_play_history;

CREATE INDEX idx_target_history_user_played ON library_play_history(user_id, played_at DESC);
CREATE INDEX idx_target_history_track ON library_play_history(local_track_id);
CREATE INDEX idx_target_history_album ON library_play_history(local_album_id);
CREATE INDEX idx_target_history_artist ON library_play_history(local_artist_id);
CREATE INDEX idx_play_history_user_track
    ON library_play_history(user_id, local_track_id, played_at);
CREATE INDEX idx_play_history_user_album_no_track
    ON library_play_history(user_id, local_album_id, played_at)
    WHERE local_track_id IS NULL AND local_album_id IS NOT NULL;

-- The one refresh, as before, with each play's time read as its whole seconds.
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
                           OR h.played_at / 1000 > m.marked_unplayed_at / 1000)) AS counted,
                   (SELECT MAX(h.played_at / 1000)
                    FROM library_play_history h INDEXED BY idx_play_history_user_track
                    WHERE h.user_id = NEW.user_id AND h.local_track_id = t.id
                      AND (m.marked_unplayed_at IS NULL
                           OR h.played_at / 1000 > m.marked_unplayed_at / 1000)) AS latest
            FROM local_tracks t
            LEFT JOIN catalog_user_play_marks m
                ON m.user_id = NEW.user_id AND m.local_track_id = t.id
            WHERE t.local_album_id = NEW.local_album_id
        )
        UNION ALL
        SELECT 0, COUNT(*), MAX(h.played_at / 1000)
        FROM library_play_history h INDEXED BY idx_play_history_user_album_no_track
        WHERE h.user_id = NEW.user_id AND h.local_track_id IS NULL
          AND h.local_album_id = NEW.local_album_id AND h.local_album_id IS NOT NULL
    )
    HAVING SUM(plays) > 0;
END;

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

-- The backfill, over every pair a play or a mark names.
DELETE FROM library_user_album_plays;
INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
SELECT h.user_id, t.local_album_id
FROM library_play_history h JOIN local_tracks t ON t.id = h.local_track_id
UNION
SELECT user_id, local_album_id FROM library_play_history
WHERE local_track_id IS NULL AND local_album_id IS NOT NULL
UNION
SELECT m.user_id, t.local_album_id
FROM catalog_user_play_marks m JOIN local_tracks t ON t.id = m.local_track_id;

INSERT INTO play_history_130_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3) AS now
    WHERE now.n > (SELECT COUNT(*) FROM play_history_130_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent));

DROP TABLE play_history_130_guard;
DROP TABLE play_history_130_orphans;

-- Migration high-water mark.
PRAGMA user_version = 130;
COMMIT;
PRAGMA foreign_keys=ON;
