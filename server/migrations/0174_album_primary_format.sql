-- local_albums.primary_format: the album's most common indexed track
-- codec, lowercased, ALAC told apart from AAC (an m4a file with a bit
-- depth); NULL with no indexed track. The format counts and the format
-- filter read it instead of grouping each album's tracks.
--
-- Kept by triggers from local_tracks, never by code. The one refresh,
-- an insert into local_album_primary_format_refresh, works the rule out
-- again for an album; the triggers and the backfill below share it. The
-- rule reads a track's album, availability, file format and whether it
-- has a bit depth, so the update trigger fires only when one of those
-- changed on a track that is or was indexed.

ALTER TABLE local_albums ADD COLUMN primary_format TEXT;

CREATE VIEW local_album_primary_format_refresh (local_album_id) AS SELECT NULL WHERE 0;

CREATE TRIGGER trg_album_primary_format_refresh
INSTEAD OF INSERT ON local_album_primary_format_refresh
BEGIN
    UPDATE local_albums SET primary_format = rule.format
    FROM (SELECT LOWER((
        SELECT CASE WHEN t.file_format = 'm4a' AND t.bit_depth IS NOT NULL
                    THEN 'alac' ELSE t.file_format END AS codec
        FROM local_tracks t
        WHERE t.local_album_id = NEW.local_album_id AND t.availability = 'indexed'
        GROUP BY codec ORDER BY COUNT(*) DESC, codec LIMIT 1)) AS format) rule
    WHERE local_albums.id = NEW.local_album_id
      AND local_albums.primary_format IS NOT rule.format;
END;

-- A track added in the album's primary format can only keep it the most
-- common, so the insert skips the refresh then: a scan adding a large
-- album's files does not count its tracks again for each one.
CREATE TRIGGER trg_album_primary_format_track_insert
AFTER INSERT ON local_tracks
WHEN NEW.availability = 'indexed'
  AND (SELECT primary_format FROM local_albums WHERE id = NEW.local_album_id)
      IS NOT LOWER(CASE WHEN NEW.file_format = 'm4a' AND NEW.bit_depth IS NOT NULL
                        THEN 'alac' ELSE NEW.file_format END)
BEGIN
    INSERT INTO local_album_primary_format_refresh (local_album_id)
    VALUES (NEW.local_album_id);
END;

CREATE TRIGGER trg_album_primary_format_track_delete
AFTER DELETE ON local_tracks
WHEN OLD.availability = 'indexed'
BEGIN
    INSERT INTO local_album_primary_format_refresh (local_album_id)
    VALUES (OLD.local_album_id);
END;

CREATE TRIGGER trg_album_primary_format_track_update
AFTER UPDATE OF local_album_id, availability, file_format, bit_depth ON local_tracks
WHEN (OLD.availability = 'indexed' OR NEW.availability = 'indexed')
  AND (OLD.local_album_id IS NOT NEW.local_album_id
       OR OLD.availability IS NOT NEW.availability
       OR OLD.file_format IS NOT NEW.file_format
       OR (OLD.bit_depth IS NULL) IS NOT (NEW.bit_depth IS NULL))
BEGIN
    INSERT INTO local_album_primary_format_refresh (local_album_id)
    VALUES (OLD.local_album_id);
    INSERT INTO local_album_primary_format_refresh (local_album_id)
    SELECT NEW.local_album_id WHERE NEW.local_album_id IS NOT OLD.local_album_id;
END;

INSERT INTO local_album_primary_format_refresh (local_album_id)
SELECT id FROM local_albums;

PRAGMA user_version = 174;
