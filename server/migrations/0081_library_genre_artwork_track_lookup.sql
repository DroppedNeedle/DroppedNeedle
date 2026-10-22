-- 0081_library_genre_artwork_track_lookup.sql - the genre artwork triggers
-- read an album's tracks through the album's index.
--
-- The four triggers that bump the genre artwork revisions of an album's
-- genres (an album renamed, re-credited or retired, and its artwork added,
-- changed or removed) look up the album's indexed tracks. Before the
-- database has planner statistics, SQLite can pick
-- idx_local_tracks_album_order_folded (availability first) for that lookup
-- and walk every indexed track in the library for each album written, so
-- a write over thousands of albums took minutes on a new install. Each
-- trigger is recreated unchanged except that the lookup names
-- idx_local_tracks_album_availability.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TRIGGER IF EXISTS trg_genre_artwork_artwork_insert;
CREATE TRIGGER trg_genre_artwork_artwork_insert
AFTER INSERT ON local_album_artwork
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

DROP TRIGGER IF EXISTS trg_genre_artwork_artwork_update;
CREATE TRIGGER trg_genre_artwork_artwork_update
AFTER UPDATE ON local_album_artwork
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

DROP TRIGGER IF EXISTS trg_genre_artwork_artwork_delete;
CREATE TRIGGER trg_genre_artwork_artwork_delete
AFTER DELETE ON local_album_artwork
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = OLD.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

DROP TRIGGER IF EXISTS trg_genre_artwork_album_update;
CREATE TRIGGER trg_genre_artwork_album_update
AFTER UPDATE OF title, album_artist_id, album_artist_name, retired_into_album_id
ON local_albums
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

-- Migration high-water mark.
PRAGMA user_version = 81;
