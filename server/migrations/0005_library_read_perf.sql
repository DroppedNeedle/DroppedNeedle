-- 0005_library_read_perf.sql - stage-13 fix C: 100k-catalog read budget.
--
-- The six standard reads miss the 10 ms p95 budget on a seeded 100k
-- catalog (stage-13 row 10 + F3): unindexed per-artist aggregation, a
-- full-join album GROUP BY per page, an unsorted track title walk, full
-- scans for stats, and full LIKE scans for text misses. This migration
-- adds the indexes and maintained read models the rewritten reads need:
--
-- - local_albums(album_artist_id): artist totals and per-artist lookups.
-- - local_tracks(availability, title_folded, id): title-order track pages
--   walk the index instead of sorting.
-- - local_tracks(id, availability, local_album_id): covering probe for the
--   batched artist credit aggregation.
-- - local_tracks_fts: contentless trigram FTS5 over the folded text
--   columns, an existence oracle for text misses. A trigram phrase MATCH
--   is exact substring for queries of 3 or more characters; shorter
--   queries keep the LIKE path (see the reads layer).
-- - library_track_format_stats: per-format indexed-track counts and bytes
--   backing library stats and unfiltered track totals.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE INDEX IF NOT EXISTS idx_local_albums_artist_id
    ON local_albums(album_artist_id);

CREATE INDEX IF NOT EXISTS idx_local_tracks_availability_title
    ON local_tracks(availability, title_folded, id);

CREATE INDEX IF NOT EXISTS idx_local_tracks_id_availability_album
    ON local_tracks(id, availability, local_album_id);

CREATE VIRTUAL TABLE IF NOT EXISTS local_tracks_fts USING fts5(
    title_folded,
    artist_name_folded,
    album_title_folded,
    content='',
    tokenize='trigram'
);

-- The FTS rowid mirrors the track rowid (local_tracks has a TEXT primary
-- key, so the rowid is the implicit one). Tracks of every availability
-- stay indexed: the readers apply their own availability filter, and the
-- miss oracle only ever proves text absence.
CREATE TRIGGER IF NOT EXISTS trg_tracks_fts_insert
AFTER INSERT ON local_tracks
BEGIN
    INSERT INTO local_tracks_fts(rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES (NEW.rowid, NEW.title_folded, COALESCE(NEW.artist_name_folded, ''), NEW.album_title_folded);
END;

CREATE TRIGGER IF NOT EXISTS trg_tracks_fts_delete
AFTER DELETE ON local_tracks
BEGIN
    INSERT INTO local_tracks_fts(local_tracks_fts, rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES ('delete', OLD.rowid, OLD.title_folded, COALESCE(OLD.artist_name_folded, ''), OLD.album_title_folded);
END;

CREATE TRIGGER IF NOT EXISTS trg_tracks_fts_update
AFTER UPDATE OF title_folded, artist_name_folded, album_title_folded ON local_tracks
BEGIN
    INSERT INTO local_tracks_fts(local_tracks_fts, rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES ('delete', OLD.rowid, OLD.title_folded, COALESCE(OLD.artist_name_folded, ''), OLD.album_title_folded);
    INSERT INTO local_tracks_fts(rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES (NEW.rowid, NEW.title_folded, COALESCE(NEW.artist_name_folded, ''), NEW.album_title_folded);
END;

CREATE TABLE IF NOT EXISTS library_track_format_stats (
    file_format TEXT PRIMARY KEY,
    indexed_tracks INTEGER NOT NULL DEFAULT 0 CHECK(indexed_tracks >= 0),
    indexed_bytes INTEGER NOT NULL DEFAULT 0 CHECK(indexed_bytes >= 0)
);

-- One row per format ever seen; rows that reach zero stay at zero and the
-- readers filter them, matching GROUP BY semantics exactly.
CREATE TRIGGER IF NOT EXISTS trg_track_stats_insert
AFTER INSERT ON local_tracks
WHEN NEW.availability = 'indexed'
BEGIN
    INSERT INTO library_track_format_stats(file_format, indexed_tracks, indexed_bytes)
    VALUES (NEW.file_format, 1, NEW.file_size_bytes)
    ON CONFLICT(file_format) DO UPDATE SET
        indexed_tracks = indexed_tracks + 1,
        indexed_bytes = indexed_bytes + excluded.indexed_bytes;
END;

CREATE TRIGGER IF NOT EXISTS trg_track_stats_delete
AFTER DELETE ON local_tracks
WHEN OLD.availability = 'indexed'
BEGIN
    UPDATE library_track_format_stats
    SET indexed_tracks = indexed_tracks - 1,
        indexed_bytes = indexed_bytes - OLD.file_size_bytes
    WHERE file_format = OLD.file_format;
END;

CREATE TRIGGER IF NOT EXISTS trg_track_stats_remove
AFTER UPDATE ON local_tracks
WHEN OLD.availability = 'indexed'
 AND (NEW.availability IS NOT 'indexed'
      OR NEW.file_format IS NOT OLD.file_format
      OR NEW.file_size_bytes IS NOT OLD.file_size_bytes)
BEGIN
    UPDATE library_track_format_stats
    SET indexed_tracks = indexed_tracks - 1,
        indexed_bytes = indexed_bytes - OLD.file_size_bytes
    WHERE file_format = OLD.file_format;
END;

CREATE TRIGGER IF NOT EXISTS trg_track_stats_add
AFTER UPDATE ON local_tracks
WHEN NEW.availability = 'indexed'
 AND (OLD.availability IS NOT 'indexed'
      OR NEW.file_format IS NOT OLD.file_format
      OR NEW.file_size_bytes IS NOT OLD.file_size_bytes)
BEGIN
    INSERT INTO library_track_format_stats(file_format, indexed_tracks, indexed_bytes)
    VALUES (NEW.file_format, 1, NEW.file_size_bytes)
    ON CONFLICT(file_format) DO UPDATE SET
        indexed_tracks = indexed_tracks + 1,
        indexed_bytes = indexed_bytes + excluded.indexed_bytes;
END;

-- Backfill over whatever catalog already exists. Fresh databases insert
-- zero rows here; upgrades over a seeded catalog index it once.
INSERT INTO local_tracks_fts(rowid, title_folded, artist_name_folded, album_title_folded)
SELECT rowid, title_folded, COALESCE(artist_name_folded, ''), album_title_folded
FROM local_tracks;

INSERT INTO library_track_format_stats(file_format, indexed_tracks, indexed_bytes)
SELECT file_format, COUNT(*), COALESCE(SUM(file_size_bytes), 0)
FROM local_tracks
WHERE availability = 'indexed'
GROUP BY file_format;

-- Migration high-water mark. The boot assertion compares this against the
-- newest embedded migration and refuses to serve on mismatch.
PRAGMA user_version = 5;
