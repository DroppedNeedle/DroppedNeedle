-- no-transaction
PRAGMA foreign_keys=OFF;

BEGIN IMMEDIATE;

-- A play goes with the account that made it: library_play_history.user_id
-- gains a foreign key to auth_users that cascades, so deleting an account
-- deletes its plays. The play and mark delete triggers refresh
-- library_user_album_plays only while the account is there, and a trigger
-- on auth_users removes a deleted account's rows in the same write.
--
-- Plays left by accounts deleted before this upgrade go first, through the
-- old table so its delete trigger keeps library_user_album_plays right,
-- and then any library_user_album_plays row whose account is gone. The
-- table is then rebuilt with foreign keys off, by the recipe in
-- kernel/src/db/migrate.rs. Every other play keeps its id and rowid, and
-- the indexes and triggers the drop takes with it come back as they were.

CREATE TEMP TABLE play_history_user_guard_keys (
    ok INTEGER CONSTRAINT guard_foreign_keys_off CHECK (ok = 1)
);
INSERT INTO play_history_user_guard_keys
SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;

CREATE TEMP TABLE play_history_user_guard_version (
    ok INTEGER CONSTRAINT guard_not_applied_yet CHECK (ok = 1)
);
INSERT INTO play_history_user_guard_version
SELECT (SELECT user_version FROM pragma_user_version) < 158;

DELETE FROM library_play_history
WHERE NOT EXISTS (
    SELECT 1 FROM auth_users WHERE auth_users.id = library_play_history.user_id
);

DELETE FROM library_user_album_plays
WHERE NOT EXISTS (
    SELECT 1 FROM auth_users WHERE auth_users.id = library_user_album_plays.user_id
);

CREATE TEMP TABLE play_history_user_orphans (
    child TEXT,
    child_rowid INTEGER,
    parent TEXT
);
INSERT INTO play_history_user_orphans (child, child_rowid, parent)
SELECT "table", "rowid", parent FROM pragma_foreign_key_check;
CREATE INDEX play_history_user_orphans_key
    ON play_history_user_orphans (child, child_rowid, parent);

CREATE TABLE library_play_history_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
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

INSERT INTO library_play_history_new (
    rowid, id, user_id, local_track_id, local_album_id, local_artist_id, track_name,
    artist_name, album_name, recording_mbid, release_group_mbid, duration_ms, source,
    played_at)
SELECT
    rowid, id, user_id, local_track_id, local_album_id, local_artist_id, track_name,
    artist_name, album_name, recording_mbid, release_group_mbid, duration_ms, source,
    played_at
FROM library_play_history;

-- The album plays refresh reads library_play_history: it goes before the
-- table is dropped and comes back below.
DROP TRIGGER trg_user_album_plays_refresh;

DROP TABLE library_play_history;

ALTER TABLE library_play_history_new RENAME TO library_play_history;

CREATE INDEX idx_play_history_user_album_no_track
    ON library_play_history(user_id, local_album_id, played_at)
    WHERE local_track_id IS NULL AND local_album_id IS NOT NULL;

CREATE INDEX idx_play_history_user_track
    ON library_play_history(user_id, local_track_id, played_at);

CREATE INDEX idx_target_history_album ON library_play_history(local_album_id);

CREATE INDEX idx_target_history_artist ON library_play_history(local_artist_id);

CREATE INDEX idx_target_history_track ON library_play_history(local_track_id);

CREATE INDEX idx_target_history_user_played ON library_play_history(user_id, played_at DESC);

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
WHEN (OLD.local_track_id IS NOT NULL OR OLD.local_album_id IS NOT NULL)
 AND EXISTS (SELECT 1 FROM auth_users WHERE id = OLD.user_id)
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (OLD.user_id, CASE WHEN OLD.local_track_id IS NULL THEN OLD.local_album_id
        ELSE (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id) END);
END;

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

DROP TRIGGER trg_user_album_plays_mark_delete;

CREATE TRIGGER trg_user_album_plays_mark_delete
AFTER DELETE ON catalog_user_play_marks
WHEN EXISTS (SELECT 1 FROM auth_users WHERE id = OLD.user_id)
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (OLD.user_id, (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id));
END;

-- The read model's rows of a removed account go with it. Its plays and
-- marks go by cascade, after the account row, so their delete triggers
-- skip the refresh: one refresh per play would keep an account with a long
-- history from being deleted within a request's write budget.
CREATE TRIGGER trg_user_album_plays_account_delete
AFTER DELETE ON auth_users
BEGIN
    DELETE FROM library_user_album_plays WHERE user_id = OLD.id;
END;

CREATE TEMP TABLE play_history_user_guard_orphans (
    ok INTEGER CONSTRAINT guard_no_new_orphans CHECK (ok = 1)
);
INSERT INTO play_history_user_guard_orphans
SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, "rowid" AS child_rowid, parent, COUNT(*) AS found
        FROM pragma_foreign_key_check
        GROUP BY 1, 2, 3
    ) AS now
    WHERE now.found > (
        SELECT COUNT(*) FROM play_history_user_orphans saved
        WHERE saved.child = now.child
          AND saved.child_rowid IS now.child_rowid
          AND saved.parent = now.parent
    )
);

DROP TABLE play_history_user_guard_keys;
DROP TABLE play_history_user_guard_version;
DROP TABLE play_history_user_orphans;
DROP TABLE play_history_user_guard_orphans;

PRAGMA user_version = 158;

COMMIT;

PRAGMA foreign_keys=ON;
