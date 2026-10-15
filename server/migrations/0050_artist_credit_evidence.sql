-- 0050_artist_credit_evidence.sql - the MusicBrainz artist id each credit's
-- file gave, so an artist renamed in the tags keeps its row.
--
--   local_track_artists.artist_mbid, local_album_artists.artist_mbid
--       the credit's MusicBrainz artist id when the file lists its artist
--       names and ids one for one (ARTISTS with MUSICBRAINZ_ARTISTID,
--       ALBUMARTISTS with MUSICBRAINZ_ALBUMARTISTID), lowercased; NULL
--       otherwise. Written by the scan with every credit row.
--   local_artists.converged_mbid
--       the id that proved a row retired by the scan's automatic merge to
--       be the artist it was merged into; NULL for every other row.
--
-- Existing credits start with NULL: a file's ids are recorded the next
-- time the scan reads it.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE local_track_artists
    ADD COLUMN artist_mbid TEXT
    CHECK (artist_mbid IS NULL OR (length(artist_mbid) = 36 AND artist_mbid = lower(artist_mbid)));

ALTER TABLE local_album_artists
    ADD COLUMN artist_mbid TEXT
    CHECK (artist_mbid IS NULL OR (length(artist_mbid) = 36 AND artist_mbid = lower(artist_mbid)));

ALTER TABLE local_artists
    ADD COLUMN converged_mbid TEXT
    CHECK (converged_mbid IS NULL OR (length(converged_mbid) = 36
        AND converged_mbid = lower(converged_mbid)));

-- The rows an id was credited to, and whether a row holds another id.
CREATE INDEX idx_local_track_artists_mbid
    ON local_track_artists(artist_mbid, local_artist_id) WHERE artist_mbid IS NOT NULL;
CREATE INDEX idx_local_track_artists_artist_mbid
    ON local_track_artists(local_artist_id, artist_mbid) WHERE artist_mbid IS NOT NULL;
CREATE INDEX idx_local_album_artists_mbid
    ON local_album_artists(artist_mbid, local_artist_id) WHERE artist_mbid IS NOT NULL;
CREATE INDEX idx_local_album_artists_artist_mbid
    ON local_album_artists(local_artist_id, artist_mbid) WHERE artist_mbid IS NOT NULL;

-- Migration high-water mark.
PRAGMA user_version = 50;
