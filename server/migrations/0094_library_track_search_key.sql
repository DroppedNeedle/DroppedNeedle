-- A stable integer key on tracks for the search index and the player's
-- natural and shuffled orders.
--
-- The search index was keyed on the implicit rowid of local_tracks, which
-- has a TEXT primary key. Copying the table into a rebuilt one (the way a
-- table is reshaped) numbers the rows afresh, and the contentless index
-- could not be rebuilt from content, so a search then named the wrong
-- tracks. search_key is an ordinary column: it is copied with the row and
-- never renumbered.
--
-- Existing tracks take their current rowid, so every order read from it
-- stays as it was. A new track takes one more than the largest key.

ALTER TABLE local_tracks ADD COLUMN search_key INTEGER;

UPDATE local_tracks SET search_key = rowid;

CREATE UNIQUE INDEX idx_local_tracks_search_key ON local_tracks(search_key);

-- The streamable walk in key order: the player's natural order and seeded
-- shuffles read the key from here, as they read the rowid before.
DROP INDEX idx_local_tracks_availability;
CREATE INDEX idx_local_tracks_availability
    ON local_tracks(availability, missing_since, search_key);

DROP TRIGGER trg_tracks_fts_insert;
DROP TRIGGER trg_tracks_fts_delete;
DROP TRIGGER trg_tracks_fts_update;
DROP TABLE local_tracks_fts;

-- External content over local_tracks, keyed on search_key, so the index
-- can be rebuilt from the table. The triggers keep it in step; NULL and
-- empty text index the same (nothing).
CREATE VIRTUAL TABLE local_tracks_fts USING fts5(
    title_folded,
    artist_name_folded,
    album_title_folded,
    content='local_tracks',
    content_rowid='search_key',
    tokenize='trigram'
);

INSERT INTO local_tracks_fts(local_tracks_fts) VALUES ('rebuild');

-- Sets the key of a new track when the insert gave none, then indexes it.
CREATE TRIGGER trg_tracks_fts_insert
AFTER INSERT ON local_tracks
BEGIN
    UPDATE local_tracks
    SET search_key = (SELECT COALESCE(MAX(search_key), 0) + 1 FROM local_tracks)
    WHERE rowid = NEW.rowid AND search_key IS NULL;
    INSERT INTO local_tracks_fts(rowid, title_folded, artist_name_folded, album_title_folded)
    SELECT search_key, NEW.title_folded, COALESCE(NEW.artist_name_folded, ''),
        NEW.album_title_folded
    FROM local_tracks WHERE rowid = NEW.rowid;
END;

CREATE TRIGGER trg_tracks_fts_delete
AFTER DELETE ON local_tracks
BEGIN
    INSERT INTO local_tracks_fts(local_tracks_fts, rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES ('delete', OLD.search_key, OLD.title_folded, COALESCE(OLD.artist_name_folded, ''), OLD.album_title_folded);
END;

CREATE TRIGGER trg_tracks_fts_update
AFTER UPDATE OF title_folded, artist_name_folded, album_title_folded ON local_tracks
WHEN OLD.title_folded IS NOT NEW.title_folded
  OR OLD.artist_name_folded IS NOT NEW.artist_name_folded
  OR OLD.album_title_folded IS NOT NEW.album_title_folded
BEGIN
    INSERT INTO local_tracks_fts(local_tracks_fts, rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES ('delete', OLD.search_key, OLD.title_folded, COALESCE(OLD.artist_name_folded, ''), OLD.album_title_folded);
    INSERT INTO local_tracks_fts(rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES (NEW.search_key, NEW.title_folded, COALESCE(NEW.artist_name_folded, ''), NEW.album_title_folded);
END;

-- Migration high-water mark.
PRAGMA user_version = 94;
